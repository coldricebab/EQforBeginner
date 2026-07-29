use super::{ensure_output_is_available, f32_samples, HarnessError, HarnessResult};
use crate::source_verification::{sha256_bytes, sha256_file};
use eqforbeginner_dsp_core::phase6::{
    design_native_rate_filters, reproduce_phase4_48k_source, Phase6Config, Phase6DesignIntent,
    Phase6NativeResult, PHASE6_ALGORITHM_VERSION, ROON_NATIVE_SAMPLE_RATES,
};
use eqforbeginner_export::{
    create_roon_six_rate_zip, inspect_wav, read_stereo_wav_bytes, validate_roon_six_rate_zip,
    write_stereo_wav, StereoFir,
};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::Builder;

const PROJECT_SCHEMA: &str = "eqforbeginner-phase6-export-project-v1";
const PHASE4_PROJECT_SCHEMA: &str = "eqforbeginner-phase4-offline-project-v2";
const MAX_PROJECT_BYTES: u64 = 2 * 1_048_576;
const MAX_DESIGN_BYTES: u64 = 4 * 1_048_576;
const MAX_SOURCE_WAV_BYTES: u64 = 16 * 1_048_576;
const LEGACY_DESIGN_HEADER: &str = "frequency_hz,left_gain_db,right_gain_db,common_gain_db,channel_specific_mix,left_protected_dip,right_protected_dip,left_spatial_support,right_spatial_support";
const DESIGN_HEADER: &str = "frequency_hz,left_gain_db,right_gain_db,common_gain_db,channel_specific_mix,left_protected_dip,right_protected_dip,left_spatial_support,right_spatial_support,left_spatial_dip_support,right_spatial_dip_support,left_boost_eligible,right_boost_eligible";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPhase6Export {
    pub output_directory: PathBuf,
    pub project_file: PathBuf,
    pub readme: PathBuf,
    pub filter_wavs: Vec<PathBuf>,
    pub roon_zip: Option<PathBuf>,
    pub cross_rate_passed: bool,
    pub export_eligible: bool,
    pub verification_state: String,
}

#[derive(Debug)]
struct MeasuredEvidence {
    project_path: PathBuf,
    project_sha256: String,
    project_bytes: u64,
    design_path: PathBuf,
    design_sha256: String,
    design_bytes: u64,
    source_wav_path: PathBuf,
    source_wav_sha256: String,
    source_wav_bytes: u64,
    source_fir: StereoFir,
}

#[derive(Debug)]
struct SourceResponseBinding {
    left_maximum_f32_sample_difference: f64,
    right_maximum_f32_sample_difference: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerationKind {
    MeasuredPreview,
    SyntheticReference,
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::Invalid(message.into())
}

fn read_regular_file(path: &Path, maximum_bytes: u64, label: &str) -> HarnessResult<Vec<u8>> {
    let link_metadata = fs::symlink_metadata(path)?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
        return Err(invalid(format!(
            "{label} must be a regular non-symlink file"
        )));
    }
    if link_metadata.len() > maximum_bytes {
        return Err(invalid(format!(
            "{label} exceeds the {maximum_bytes}-byte input limit"
        )));
    }
    Ok(fs::read(path)?)
}

fn json_bool(project: &serde_json::Value, pointer: &str) -> HarnessResult<bool> {
    project
        .pointer(pointer)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            invalid(format!(
                "Phase 4 project field `{pointer}` is absent or invalid"
            ))
        })
}

fn json_f64(project: &serde_json::Value, pointer: &str) -> HarnessResult<f64> {
    project
        .pointer(pointer)
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| {
            invalid(format!(
                "Phase 4 project field `{pointer}` is absent or invalid"
            ))
        })
}

fn json_u64(project: &serde_json::Value, pointer: &str) -> HarnessResult<u64> {
    project
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            invalid(format!(
                "Phase 4 project field `{pointer}` is absent or invalid"
            ))
        })
}

fn json_str<'a>(project: &'a serde_json::Value, pointer: &str) -> HarnessResult<&'a str> {
    project
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            invalid(format!(
                "Phase 4 project field `{pointer}` is absent or invalid"
            ))
        })
}

fn parse_design_csv(bytes: &[u8]) -> HarnessResult<Phase6DesignIntent> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| invalid("Phase 4 filter-design.csv must be valid UTF-8"))?;
    let mut lines = text.lines();
    let header = lines.next();
    let column_count = match header {
        Some(DESIGN_HEADER) => 13,
        Some(LEGACY_DESIGN_HEADER) => 9,
        _ => return Err(invalid("Phase 4 filter-design.csv header is incompatible")),
    };
    let mut frequencies_hz = Vec::new();
    let mut left_gain_db = Vec::new();
    let mut right_gain_db = Vec::new();
    for (offset, line) in lines.enumerate() {
        let line_number = offset + 2;
        if line.trim().is_empty() {
            return Err(invalid(format!(
                "filter-design.csv line {line_number} is unexpectedly empty"
            )));
        }
        let columns: Vec<&str> = line.split(',').collect();
        if columns.len() != column_count {
            return Err(invalid(format!(
                "filter-design.csv line {line_number} must contain exactly {column_count} columns"
            )));
        }
        let parse = |column: usize, name: &str| -> HarnessResult<f64> {
            columns[column]
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or_else(|| {
                    invalid(format!(
                        "filter-design.csv line {line_number} has invalid {name}"
                    ))
                })
        };
        let frequency = parse(0, "frequency")?;
        let left = parse(1, "left gain")?;
        let right = parse(2, "right gain")?;
        // Parse every numeric field so corrupted unused diagnostics cannot hide.
        for (column, name) in [
            (3, "common gain"),
            (4, "channel mix"),
            (7, "left support"),
            (8, "right support"),
        ] {
            let _ = parse(column, name)?;
        }
        if !matches!(columns[5], "true" | "false") || !matches!(columns[6], "true" | "false") {
            return Err(invalid(format!(
                "filter-design.csv line {line_number} has an invalid protected-dip flag"
            )));
        }
        if column_count == 13 {
            let _ = parse(9, "left dip support")?;
            let _ = parse(10, "right dip support")?;
            if !matches!(columns[11], "true" | "false") || !matches!(columns[12], "true" | "false")
            {
                return Err(invalid(format!(
                    "filter-design.csv line {line_number} has an invalid boost-eligible flag"
                )));
            }
        }
        frequencies_hz.push(frequency);
        left_gain_db.push(left);
        right_gain_db.push(right);
        if frequencies_hz.len() > 100_000 {
            return Err(invalid("filter-design.csv exceeds 100000 design rows"));
        }
    }
    Ok(Phase6DesignIntent {
        frequencies_hz,
        left_gain_db,
        right_gain_db,
        correction_low_hz: 20.0,
        taper_end_hz: 650.0,
    })
}

fn load_measured_evidence(
    phase4_project: &Path,
    design_csv: &Path,
    phase4_wav: &Path,
) -> HarnessResult<(Phase6DesignIntent, MeasuredEvidence)> {
    let project_bytes = read_regular_file(phase4_project, MAX_PROJECT_BYTES, "Phase 4 project")?;
    let design_bytes = read_regular_file(design_csv, MAX_DESIGN_BYTES, "Phase 4 design CSV")?;
    let wav_bytes = read_regular_file(phase4_wav, MAX_SOURCE_WAV_BYTES, "Phase 4 FIR WAV")?;

    let project_parent = fs::canonicalize(
        phase4_project
            .parent()
            .ok_or_else(|| invalid("Phase 4 project must have a parent directory"))?,
    )?;
    let design_parent = fs::canonicalize(
        design_csv
            .parent()
            .ok_or_else(|| invalid("Phase 4 design CSV must have a parent directory"))?,
    )?;
    let wav_parent = fs::canonicalize(
        phase4_wav
            .parent()
            .ok_or_else(|| invalid("Phase 4 FIR must have a parent directory"))?,
    )?;
    if design_parent != project_parent || wav_parent != project_parent.join("filter") {
        return Err(invalid(
            "Phase 4 project, design CSV and filter must retain the generated sibling layout",
        ));
    }

    let project: serde_json::Value = serde_json::from_slice(&project_bytes)?;
    let phase4_algorithm = json_str(&project, "/dsp_algorithm_version")?;
    if json_str(&project, "/schema_version")? != PHASE4_PROJECT_SCHEMA
        || !matches!(
            phase4_algorithm,
            "phase4-response-replay-v1" | "phase4-response-replay-v2"
        )
        || json_str(&project, "/verification_state")? != "predicted-only-measured"
        || json_str(&project, "/hardware_verification")? != "unverified"
        || json_bool(&project, "/export_eligible")?
        || !json_bool(&project, "/numerical_prediction_passed")?
        || !project["closed_loop_passed"].is_null()
        || !project["recommended_headroom_db"].is_null()
        || json_u64(&project, "/sample_rate_hz")? != 48_000
        || json_u64(&project, "/settings/fir_fft_size")? != 16_384
    {
        return Err(invalid(
            "Phase 4 evidence is not the expected unverified predicted-only state",
        ));
    }
    let missing = project["required_evidence_missing"]
        .as_array()
        .ok_or_else(|| invalid("Phase 4 required-evidence list is absent"))?;
    if missing.is_empty()
        || project["measurement_sources"].as_array().map(Vec::len) != Some(6)
        || project["forbidden_promotions_without_new_measurements"]
            .as_array()
            .is_none_or(|values| !values.iter().any(|value| value == "final Roon export"))
    {
        return Err(invalid("Phase 4 project lost its evidence boundary"));
    }

    let correction_low_hz = json_f64(&project, "/settings/correction/correction_low_hz")?;
    let taper_end_hz = json_f64(&project, "/settings/correction/taper_end_hz")?;
    if (correction_low_hz - 20.0).abs() > 1.0e-9 || (taper_end_hz - 650.0).abs() > 1.0e-9 {
        return Err(invalid("Phase 4 correction band changed from 20..650 Hz"));
    }
    let expected_wav_name = json_str(&project, "/results/filter/file")?;
    if phase4_wav.file_name().and_then(|name| name.to_str()) != Some(expected_wav_name) {
        return Err(invalid(
            "Phase 4 FIR file name does not match project metadata",
        ));
    }
    let wav_sha256 = sha256_bytes(&wav_bytes);
    let wav_size = wav_bytes.len() as u64;
    if json_str(&project, "/results/filter_integrity/sha256")? != wav_sha256
        || json_u64(&project, "/results/filter_integrity/bytes")? != wav_size
    {
        return Err(invalid(
            "Phase 4 FIR hash or byte length does not match its project",
        ));
    }
    let source_fir = read_stereo_wav_bytes(expected_wav_name, &wav_bytes)?;
    if source_fir.sample_rate != 48_000
        || source_fir.left.len() != 16_384
        || source_fir.right.len() != 16_384
    {
        return Err(invalid("Phase 4 FIR layout is incompatible with Phase 6"));
    }
    let mut intent = parse_design_csv(&design_bytes)?;
    intent.correction_low_hz = correction_low_hz;
    intent.taper_end_hz = taper_end_hz;
    Ok((
        intent,
        MeasuredEvidence {
            project_path: phase4_project.to_path_buf(),
            project_sha256: sha256_bytes(&project_bytes),
            project_bytes: project_bytes.len() as u64,
            design_path: design_csv.to_path_buf(),
            design_sha256: sha256_bytes(&design_bytes),
            design_bytes: design_bytes.len() as u64,
            source_wav_path: phase4_wav.to_path_buf(),
            source_wav_sha256: wav_sha256,
            source_wav_bytes: wav_size,
            source_fir,
        },
    ))
}

fn synthetic_reference_intent() -> Phase6DesignIntent {
    let frequencies_hz: Vec<f64> = (0..1_024)
        .map(|index| 20.0 * (650.0_f64 / 20.0).powf(index as f64 / 1_023.0))
        .collect();
    let gain_for = |frequency: f64, channel_shift: f64| {
        let peak_40 =
            -4.0 * (-0.5 * ((frequency / (40.0 + channel_shift)).log2() / 0.20).powi(2)).exp();
        let peak_66 =
            -5.5 * (-0.5 * ((frequency / (66.0 + channel_shift)).log2() / 0.22).powi(2)).exp();
        let broad_dip_support =
            2.0 * (-0.5 * ((frequency / (125.0 + channel_shift)).log2() / 0.30).powi(2)).exp();
        let taper = if frequency <= 500.0 {
            1.0
        } else {
            0.5 * (1.0 + (std::f64::consts::PI * (frequency - 500.0) / 150.0).cos())
        };
        (peak_40 + peak_66 + broad_dip_support) * taper
    };
    Phase6DesignIntent {
        left_gain_db: frequencies_hz
            .iter()
            .map(|frequency| gain_for(*frequency, 0.0))
            .collect(),
        right_gain_db: frequencies_hz
            .iter()
            .map(|frequency| gain_for(*frequency, 1.0))
            .collect(),
        frequencies_hz,
        correction_low_hz: 20.0,
        taper_end_hz: 650.0,
    }
}

fn compare_regenerated_48k(
    intent: &Phase6DesignIntent,
    evidence: &MeasuredEvidence,
) -> HarnessResult<SourceResponseBinding> {
    let (regenerated_left, regenerated_right) = reproduce_phase4_48k_source(intent)?;
    if regenerated_left.taps.len() != evidence.source_fir.left.len()
        || regenerated_right.taps.len() != evidence.source_fir.right.len()
    {
        return Err(invalid("Phase 4 source reproduction length changed"));
    }
    let left_maximum_f32_sample_difference = regenerated_left
        .taps
        .iter()
        .zip(&evidence.source_fir.left)
        .map(|(regenerated, source)| ((*regenerated as f32) - source).abs() as f64)
        .fold(0.0, f64::max);
    let right_maximum_f32_sample_difference = regenerated_right
        .taps
        .iter()
        .zip(&evidence.source_fir.right)
        .map(|(regenerated, source)| ((*regenerated as f32) - source).abs() as f64)
        .fold(0.0, f64::max);
    if left_maximum_f32_sample_difference > 1.0e-6 || right_maximum_f32_sample_difference > 1.0e-6 {
        return Err(invalid(format!(
            "Phase 6 design CSV does not reproduce the Phase 4 source FIR (L/R max f32 sample difference {left_maximum_f32_sample_difference:.3e}/{right_maximum_f32_sample_difference:.3e})",
        )));
    }
    Ok(SourceResponseBinding {
        left_maximum_f32_sample_difference,
        right_maximum_f32_sample_difference,
    })
}

fn comparison_json(result: &Phase6NativeResult) -> Vec<serde_json::Value> {
    result
        .comparisons
        .iter()
        .map(|comparison| {
            json!({
                "sample_rate_hz": comparison.sample_rate_hz,
                "reference_sample_rate_hz": comparison.reference_sample_rate_hz,
                "passed": comparison.passed,
                "left": {
                    "maximum_magnitude_difference_db": comparison.left.maximum_magnitude_difference_db,
                    "maximum_magnitude_difference_frequency_hz": comparison.left.maximum_magnitude_difference_frequency_hz,
                    "maximum_relative_group_delay_difference_ms": comparison.left.maximum_relative_group_delay_difference_ms,
                    "maximum_relative_group_delay_difference_frequency_hz": comparison.left.maximum_relative_group_delay_difference_frequency_hz,
                },
                "right": {
                    "maximum_magnitude_difference_db": comparison.right.maximum_magnitude_difference_db,
                    "maximum_magnitude_difference_frequency_hz": comparison.right.maximum_magnitude_difference_frequency_hz,
                    "maximum_relative_group_delay_difference_ms": comparison.right.maximum_relative_group_delay_difference_ms,
                    "maximum_relative_group_delay_difference_frequency_hz": comparison.right.maximum_relative_group_delay_difference_frequency_hz,
                },
            })
        })
        .collect()
}

fn phase6_readme(kind: GenerationKind, result: &Phase6NativeResult) -> String {
    let metrics = result
        .comparisons
        .iter()
        .map(|comparison| {
            format!(
                "{} Hz: magnitude L/R {:.6}/{:.6} dB, relative GD L/R {:.6}/{:.6} ms, pass={}\r\n",
                comparison.sample_rate_hz,
                comparison.left.maximum_magnitude_difference_db,
                comparison.right.maximum_magnitude_difference_db,
                comparison.left.maximum_relative_group_delay_difference_ms,
                comparison.right.maximum_relative_group_delay_difference_ms,
                comparison.passed,
            )
        })
        .collect::<String>();
    match kind {
        GenerationKind::MeasuredPreview => format!(
            "EQforBeginner Phase 6 measured-project developer preview\r\n\
             =====================================================\r\n\r\n\
             UNVERIFIED PREVIEW — DO NOT LOAD THESE WAV FILES INTO ROON.\r\n\r\n\
             The six FIRs were redesigned independently on native sample-rate\r\n\
             grids. The source project is predicted-only and has no FIR-applied\r\n\
             real-path remeasurement or true-peak evidence. Export eligibility\r\n\
             therefore remains false and no ZIP was created. A common safety\r\n\
             attenuation aligns level across rates; it is not a headroom value.\r\n\r\n\
             Cross-rate numerical validation (magnitude 20-20000 Hz, relative\r\n\
             group delay 20-650 Hz):\r\n{metrics}\r\n\
             Required before final export: perform 48 kHz FIR-applied L/R\r\n\
             remeasurement, validate channel mapping and clipping, and calculate\r\n\
             signal true peak/headroom.\r\n"
        ),
        GenerationKind::SyntheticReference => format!(
            "EQforBeginner Phase 6 synthetic structural reference\r\n\
             ==================================================\r\n\r\n\
             SYNTHETIC TEST DATA — DO NOT USE FOR LISTENING OR ROOM CORRECTION.\r\n\r\n\
             This ZIP exists only to exercise Roon package structure. In a real\r\n\
             verified export, first disable any existing convolution, select one\r\n\
             EQforBeginner ZIP in Roon MUSE Convolution, enable the measured\r\n\
             recommended Headroom Management value, start at low volume, and\r\n\
             check Roon's clipping indicator and Signal Path.\r\n\r\n\
             Cross-rate numerical validation (magnitude 20-20000 Hz, relative\r\n\
             group delay 20-650 Hz):\r\n{metrics}"
        ),
    }
}

fn generate_in(
    output: &Path,
    kind: GenerationKind,
    intent: &Phase6DesignIntent,
    evidence: Option<&MeasuredEvidence>,
) -> HarnessResult<GeneratedPhase6Export> {
    let config = Phase6Config::default();
    let result = design_native_rate_filters(intent, &config)?;
    if !result.cross_rate_passed {
        return Err(invalid(format!(
            "Phase 6 cross-rate validation failed: {:?}",
            result.comparisons
        )));
    }
    let regeneration_difference = evidence
        .map(|evidence| compare_regenerated_48k(intent, evidence))
        .transpose()?;
    let filter_directory = output.join("filters");
    fs::create_dir_all(&filter_directory)?;
    let mut filter_wavs = Vec::new();
    let mut filter_records = Vec::new();
    for filter in &result.filters {
        let name = match kind {
            GenerationKind::MeasuredPreview => format!(
                "EQforBeginner_{}_UNVERIFIED_PREVIEW.wav",
                filter.sample_rate_hz
            ),
            GenerationKind::SyntheticReference => {
                format!("EQforBeginner_{}_stereo.wav", filter.sample_rate_hz)
            }
        };
        let path = filter_directory.join(&name);
        let metadata = write_stereo_wav(
            &path,
            &StereoFir {
                sample_rate: filter.sample_rate_hz,
                left: f32_samples(&filter.left_fir.taps)?,
                right: f32_samples(&filter.right_fir.taps)?,
            },
        )?;
        let (sha256, bytes) = sha256_file(&path)?;
        filter_records.push(json!({
            "metadata": metadata,
            "sha256": sha256,
            "bytes": bytes,
            "native_fft_size": filter.fft_size,
            "native_design_grid_bins": filter.native_frequencies_hz.len(),
            "redesigned_from_physical_frequency_intent": true,
            "common_safety_normalization_db": result.common_safety_normalization_db,
            "left_additional_common_safety_attenuation_db": filter.left_additional_common_safety_attenuation_db,
            "right_additional_common_safety_attenuation_db": filter.right_additional_common_safety_attenuation_db,
            "safety_gain_is_not_recommended_headroom": true,
        }));
        filter_wavs.push(path);
    }
    let readme_text = phase6_readme(kind, &result);
    let readme = output.join("README.txt");
    fs::write(&readme, &readme_text)?;
    let roon_zip = if kind == GenerationKind::SyntheticReference {
        let path = output.join("EQforBeginner_Phase6_SYNTHETIC_REFERENCE_NOT_FOR_PLAYBACK.zip");
        create_roon_six_rate_zip(&path, &filter_wavs, PHASE6_ALGORITHM_VERSION, &readme_text)?;
        Some(path)
    } else {
        None
    };
    let verification_state = match kind {
        GenerationKind::MeasuredPreview => "predicted-only-measured-native-preview",
        GenerationKind::SyntheticReference => "synthetic-structural-reference",
    };
    let project_file = output.join("project.json");
    let mut project_bytes = serde_json::to_vec_pretty(&json!({
        "schema_version": PROJECT_SCHEMA,
        "app_version": env!("CARGO_PKG_VERSION"),
        "dsp_algorithm_version": result.algorithm_version,
        "verification_state": verification_state,
        "package_kind": match kind { GenerationKind::MeasuredPreview => "measured-developer-preview", GenerationKind::SyntheticReference => "synthetic-structural-reference" },
        "hardware_verification": false,
        "closed_loop_passed": null,
        "export_eligible": false,
        "recommended_headroom_db": null,
        "roon_zip_created": roon_zip.is_some(),
        "roon_zip": roon_zip.as_ref().and_then(|path| path.file_name()).and_then(|name| name.to_str()),
        "source_evidence": evidence.map(|evidence| json!({
            "phase4_project": {"path": evidence.project_path, "sha256": evidence.project_sha256, "bytes": evidence.project_bytes},
            "design_csv": {"path": evidence.design_path, "sha256": evidence.design_sha256, "bytes": evidence.design_bytes},
            "phase4_wav": {"path": evidence.source_wav_path, "sha256": evidence.source_wav_sha256, "bytes": evidence.source_wav_bytes},
            "regenerated_48k_source_response_binding": regeneration_difference.as_ref().map(|binding| json!({
                "legacy_phase4_fft_size": 16384,
                "maximum_f32_sample_difference": {"left": binding.left_maximum_f32_sample_difference, "right": binding.right_maximum_f32_sample_difference},
                "tap_reproduction_within_tolerance_required": true,
                "maximum_f32_sample_difference_allowed": 1.0e-6,
                "phase6_tap_identity_expected": false,
                "reason": "the CSV first reproduces the legacy Phase 4 FIR; Phase 6 then redesigns that bound physical-frequency intent on common-duration native grids",
                "passed": true,
            })),
        })),
        "settings": {
            "sample_rates_hz": ROON_NATIVE_SAMPLE_RATES,
            "magnitude_compare_band_hz": [config.magnitude_compare_low_hz, config.magnitude_compare_high_hz],
            "group_delay_compare_band_hz": [config.group_delay_compare_low_hz, config.group_delay_compare_high_hz],
            "maximum_magnitude_difference_db": config.maximum_magnitude_difference_db,
            "maximum_relative_group_delay_difference_ms": config.maximum_relative_group_delay_difference_ms,
            "comparison_points": config.comparison_points,
        },
        "results": {
            "cross_rate_passed": result.cross_rate_passed,
            "comparisons": comparison_json(&result),
            "filters": filter_records,
        },
        "required_evidence_missing": match kind {
            GenerationKind::MeasuredPreview => vec![
                "actual 48 kHz FIR-applied L+sub and R+sub remeasurement",
                "actual-path clipping, sample-drop and channel-map checks",
                "verification-signal true peak and calculated headroom",
            ],
            GenerationKind::SyntheticReference => vec!["synthetic reference is never user export evidence"],
        },
        "forbidden_promotions_without_new_measurements": [
            "closed-loop verified", "recommended headroom", "final Roon export"
        ],
    }))?;
    project_bytes.push(b'\n');
    fs::write(&project_file, project_bytes)?;
    Ok(GeneratedPhase6Export {
        output_directory: output.to_path_buf(),
        project_file,
        readme,
        filter_wavs,
        roon_zip,
        cross_rate_passed: true,
        export_eligible: false,
        verification_state: verification_state.into(),
    })
}

fn verify_staged(generated: &GeneratedPhase6Export, kind: GenerationKind) -> HarnessResult<()> {
    if !generated.project_file.is_file()
        || !generated.readme.is_file()
        || generated.filter_wavs.len() != 6
        || !generated.cross_rate_passed
        || generated.export_eligible
    {
        return Err(invalid(
            "Phase 6 staged artifact set is incomplete or promoted",
        ));
    }
    let found_rates: Vec<u32> = generated
        .filter_wavs
        .iter()
        .map(|path| inspect_wav(path).map(|metadata| metadata.sample_rate))
        .collect::<Result<_, _>>()?;
    if found_rates != ROON_NATIVE_SAMPLE_RATES {
        return Err(invalid("Phase 6 staged WAV sample-rate mapping changed"));
    }
    let project: serde_json::Value = serde_json::from_slice(&fs::read(&generated.project_file)?)?;
    if project["schema_version"] != PROJECT_SCHEMA
        || project["export_eligible"] != false
        || project["hardware_verification"] != false
        || !project["closed_loop_passed"].is_null()
        || !project["recommended_headroom_db"].is_null()
        || project["results"]["cross_rate_passed"] != true
    {
        return Err(invalid("Phase 6 staged project lost its safety boundary"));
    }
    match (kind, &generated.roon_zip) {
        (GenerationKind::MeasuredPreview, None) => {
            if fs::read_dir(&generated.output_directory)?
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "zip")
                })
            {
                return Err(invalid("measured Phase 6 preview must not contain a ZIP"));
            }
        }
        (GenerationKind::SyntheticReference, Some(zip)) => {
            validate_roon_six_rate_zip(zip)?;
        }
        _ => return Err(invalid("Phase 6 package kind and ZIP state disagree")),
    }
    Ok(())
}

fn commit_generated(
    output: &Path,
    kind: GenerationKind,
    intent: &Phase6DesignIntent,
    evidence: Option<&MeasuredEvidence>,
) -> HarnessResult<GeneratedPhase6Export> {
    if output.file_name().is_none() {
        return Err(invalid(format!(
            "output path `{}` must name a directory below its parent",
            output.display()
        )));
    }
    let output_parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output_existed = ensure_output_is_available(output)?;
    fs::create_dir_all(output_parent)?;
    let staging = Builder::new()
        .prefix(".eqforbeginner-phase6-stage-")
        .tempdir_in(output_parent)?;
    let staged = generate_in(staging.path(), kind, intent, evidence)?;
    verify_staged(&staged, kind)?;
    let output_still_exists = ensure_output_is_available(output)?;
    if output_still_exists != output_existed {
        return Err(invalid(format!(
            "output path `{}` changed during Phase 6 generation",
            output.display()
        )));
    }
    match fs::rename(staging.path(), output) {
        Ok(()) => {}
        Err(first_error) if output_existed => {
            if !ensure_output_is_available(output)? {
                return Err(invalid("Phase 6 output changed before commit"));
            }
            fs::remove_dir(output)?;
            if let Err(commit_error) = fs::rename(staging.path(), output) {
                let restore_error = fs::create_dir(output).err();
                return Err(invalid(match restore_error {
                    Some(restore_error) => format!(
                        "could not commit Phase 6 output ({commit_error}); could not restore empty directory ({restore_error})"
                    ),
                    None => format!(
                        "could not commit Phase 6 output after empty-directory replacement failed ({first_error}; {commit_error})"
                    ),
                }));
            }
        }
        Err(error) => return Err(error.into()),
    }
    let _committed = staging.keep();
    let filter_wavs = ROON_NATIVE_SAMPLE_RATES
        .iter()
        .map(|rate| {
            output.join(match kind {
                GenerationKind::MeasuredPreview => {
                    format!("filters/EQforBeginner_{rate}_UNVERIFIED_PREVIEW.wav")
                }
                GenerationKind::SyntheticReference => {
                    format!("filters/EQforBeginner_{rate}_stereo.wav")
                }
            })
        })
        .collect();
    Ok(GeneratedPhase6Export {
        output_directory: output.to_path_buf(),
        project_file: output.join("project.json"),
        readme: output.join("README.txt"),
        filter_wavs,
        roon_zip: (kind == GenerationKind::SyntheticReference)
            .then(|| output.join("EQforBeginner_Phase6_SYNTHETIC_REFERENCE_NOT_FOR_PLAYBACK.zip")),
        cross_rate_passed: true,
        export_eligible: false,
        verification_state: match kind {
            GenerationKind::MeasuredPreview => "predicted-only-measured-native-preview",
            GenerationKind::SyntheticReference => "synthetic-structural-reference",
        }
        .into(),
    })
}

/// Redesign six native-rate FIRs for developer inspection. A measured Phase 4
/// project is deliberately unable to produce a ZIP until real verification and
/// true-peak evidence exist.
pub fn prepare_phase6_measured_preview(
    phase4_project: &Path,
    design_csv: &Path,
    phase4_wav: &Path,
    output: &Path,
) -> HarnessResult<GeneratedPhase6Export> {
    let (intent, evidence) = load_measured_evidence(phase4_project, design_csv, phase4_wav)?;
    commit_generated(
        output,
        GenerationKind::MeasuredPreview,
        &intent,
        Some(&evidence),
    )
}

/// Generate a deterministic six-rate structural package from synthetic design
/// data. It is a format regression artifact, never user verification evidence.
pub fn generate_phase6_reference_package(output: &Path) -> HarnessResult<GeneratedPhase6Export> {
    commit_generated(
        output,
        GenerationKind::SyntheticReference,
        &synthetic_reference_intent(),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local-only measurement fixtures live outside the repository (see
    /// `docs/measurement-protocol.md`). Without them this test has nothing to
    /// assert, so it reports the skip loudly instead of passing silently.
    fn skip_without(path: &Path, what: &str) -> bool {
        if path.exists() {
            return false;
        }
        eprintln!("SKIPPED: {what} is absent at {}", path.display());
        true
    }

    #[test]
    fn measured_project_produces_six_preview_wavs_but_no_zip() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let phase4 = repository.join("examples/phase4-offline-measured");
        if skip_without(
            &phase4.join("project.json"),
            "the generated Phase 4 example project",
        ) {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("phase6-measured");
        let generated = prepare_phase6_measured_preview(
            &phase4.join("project.json"),
            &phase4.join("filter-design.csv"),
            &phase4.join("filter/EQforBeginner_48000_Phase4_Trial.wav"),
            &output,
        )
        .unwrap();
        assert_eq!(generated.filter_wavs.len(), 6);
        assert!(generated.roon_zip.is_none());
        assert!(!generated.export_eligible);
        assert!(!fs::read_dir(output)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "zip")));
    }

    #[test]
    fn tampered_project_cannot_authorize_or_generate_output() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let phase4 = repository.join("examples/phase4-offline-measured");
        if skip_without(
            &phase4.join("project.json"),
            "the generated Phase 4 example project",
        ) {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let copied = directory.path().join("phase4");
        fs::create_dir_all(copied.join("filter")).unwrap();
        fs::copy(
            phase4.join("filter-design.csv"),
            copied.join("filter-design.csv"),
        )
        .unwrap();
        fs::copy(
            phase4.join("filter/EQforBeginner_48000_Phase4_Trial.wav"),
            copied.join("filter/EQforBeginner_48000_Phase4_Trial.wav"),
        )
        .unwrap();
        let tampered = copied.join("project.json");
        let mut project: serde_json::Value =
            serde_json::from_slice(&fs::read(phase4.join("project.json")).unwrap()).unwrap();
        project["export_eligible"] = json!(true);
        fs::write(&tampered, serde_json::to_vec(&project).unwrap()).unwrap();
        let output = directory.path().join("output");
        let error = prepare_phase6_measured_preview(
            &tampered,
            &copied.join("filter-design.csv"),
            &copied.join("filter/EQforBeginner_48000_Phase4_Trial.wav"),
            &output,
        )
        .unwrap_err();
        assert!(error.to_string().contains("predicted-only"));
        assert!(!output.exists());
    }

    #[test]
    fn synthetic_reference_has_exact_six_rate_structural_zip() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("reference");
        let generated = generate_phase6_reference_package(&output).unwrap();
        let zip = generated.roon_zip.expect("reference ZIP");
        validate_roon_six_rate_zip(&zip).unwrap();
        assert!(!generated.export_eligible);
    }

    #[test]
    fn design_parser_accepts_bounded_boost_and_rejects_over_limit_or_malformed_rows() {
        let bounded = format!(
            "{DESIGN_HEADER}\n20,3,0,0,0,false,false,1,1,1,1,true,false\n500,0,0,0,0,false,false,1,1,0,0,false,false\n650,0,0,0,0,false,false,1,1,0,0,false,false\n"
        );
        let intent = parse_design_csv(bounded.as_bytes()).unwrap();
        assert!(design_native_rate_filters(&intent, &Phase6Config::default()).is_ok());
        let over_limit = format!(
            "{DESIGN_HEADER}\n20,3.1,0,0,0,false,false,1,1,1,1,true,false\n500,0,0,0,0,false,false,1,1,0,0,false,false\n650,0,0,0,0,false,false,1,1,0,0,false,false\n"
        );
        let intent = parse_design_csv(over_limit.as_bytes()).unwrap();
        assert!(design_native_rate_filters(&intent, &Phase6Config::default()).is_err());
        assert!(parse_design_csv(format!("{DESIGN_HEADER}\n20,0,0\n").as_bytes()).is_err());
    }
}
