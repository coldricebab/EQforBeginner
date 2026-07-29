#![recursion_limit = "256"]
use eqforbeginner_dsp_core::correction::CorrectionWarning;
use eqforbeginner_dsp_core::fixture::SyntheticRoomFixture;
use eqforbeginner_dsp_core::pipeline::{run_phase1, Phase1Config};
use eqforbeginner_dsp_core::validation::{PredictionMetrics, ValidationIssue, ValidationReport};
use eqforbeginner_dsp_core::{DspError, ALGORITHM_VERSION};
use eqforbeginner_export::{
    create_roon_zip, inspect_wav, validate_roon_zip, write_stereo_wav, ExportError, StereoFir,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tempfile::Builder;

mod phase4_analysis;
mod phase6_export;
mod source_verification;
mod sub_analysis;
pub use phase4_analysis::{analyze_phase4_offline, GeneratedPhase4Analysis};
pub use phase6_export::{
    generate_phase6_reference_package, prepare_phase6_measured_preview, GeneratedPhase6Export,
};
pub use sub_analysis::{analyze_sub_dataset, GeneratedSubAnalysis};

#[derive(Debug)]
pub enum HarnessError {
    Dsp(DspError),
    Export(ExportError),
    Io(std::io::Error),
    Json(serde_json::Error),
    Invalid(String),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dsp(error) => write!(f, "DSP error: {error}"),
            Self::Export(error) => write!(f, "export error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Json(error) => write!(f, "JSON error: {error}"),
            Self::Invalid(message) => write!(f, "invalid result: {message}"),
        }
    }
}

impl std::error::Error for HarnessError {}

impl From<DspError> for HarnessError {
    fn from(value: DspError) -> Self {
        Self::Dsp(value)
    }
}

impl From<ExportError> for HarnessError {
    fn from(value: ExportError) -> Self {
        Self::Export(value)
    }
}

impl From<std::io::Error> for HarnessError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for HarnessError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub type HarnessResult<T> = Result<T, HarnessError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedExample {
    pub output_directory: PathBuf,
    pub project_file: PathBuf,
    pub filter_wav: PathBuf,
    pub roon_zip: PathBuf,
}

fn f32_samples(values: &[f64]) -> HarnessResult<Vec<f32>> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let sample = *value as f32;
            if sample.is_finite() {
                Ok(sample)
            } else {
                Err(HarnessError::Invalid(format!(
                    "sample {index} cannot be represented as finite f32"
                )))
            }
        })
        .collect()
}

fn metrics_json(metrics: &PredictionMetrics) -> serde_json::Value {
    json!({
        "raw_rmse_db": metrics.raw_rmse_db,
        "predicted_rmse_db": metrics.predicted_rmse_db,
        "raw_peak_rmse_db": metrics.raw_peak_rmse_db,
        "predicted_peak_rmse_db": metrics.predicted_peak_rmse_db,
        "raw_max_peak_error_db": metrics.raw_max_peak_error_db,
        "predicted_max_peak_error_db": metrics.predicted_max_peak_error_db,
        "maximum_correction_gain_db": metrics.maximum_correction_gain_db,
        "maximum_correction_attenuation_db": metrics.maximum_correction_attenuation_db,
        "predicted_impulse_peak": metrics.predicted_impulse_peak,
    })
}

fn report_json(report: &ValidationReport) -> serde_json::Value {
    json!({
        "passed": report.passed,
        "issues": report.issues.iter().map(validation_issue_json).collect::<Vec<_>>(),
        "metrics": metrics_json(&report.metrics),
    })
}

fn correction_warning_json(warning: &CorrectionWarning) -> serde_json::Value {
    match warning {
        CorrectionWarning::AttenuationLimitReached {
            requested_db,
            limit_db,
        } => json!({
            "code": "attenuation-limit-reached",
            "requested_db": requested_db,
            "limit_db": limit_db,
        }),
        CorrectionWarning::BoostLimitReached {
            requested_db,
            limit_db,
        } => json!({
            "code": "boost-limit-reached",
            "requested_db": requested_db,
            "limit_db": limit_db,
        }),
        CorrectionWarning::SinglePositionOnly => json!({
            "code": "single-position-only",
        }),
    }
}

fn validation_issue_json(issue: &ValidationIssue) -> serde_json::Value {
    match issue {
        ValidationIssue::NonFiniteOutput => json!({
            "code": "non-finite-output",
        }),
        ValidationIssue::PositiveCorrectionGain {
            maximum_db,
            allowed_db,
        } => json!({
            "code": "positive-correction-gain",
            "maximum_db": maximum_db,
            "allowed_db": allowed_db,
        }),
        ValidationIssue::ExcessiveAttenuation {
            maximum_db,
            allowed_db,
        } => json!({
            "code": "excessive-attenuation",
            "maximum_db": maximum_db,
            "allowed_db": allowed_db,
        }),
        ValidationIssue::PeakRmseDidNotImprove {
            raw_db,
            predicted_db,
        } => json!({
            "code": "peak-rmse-did-not-improve",
            "raw_db": raw_db,
            "predicted_db": predicted_db,
        }),
        ValidationIssue::MaximumPeakDidNotImprove {
            raw_db,
            predicted_db,
        } => json!({
            "code": "maximum-peak-did-not-improve",
            "raw_db": raw_db,
            "predicted_db": predicted_db,
        }),
        ValidationIssue::OverallRmseDidNotImprove {
            raw_db,
            predicted_db,
        } => json!({
            "code": "overall-rmse-did-not-improve",
            "raw_db": raw_db,
            "predicted_db": predicted_db,
        }),
    }
}

fn write_prediction_csv(
    output: &Path,
    left: &ValidationReport,
    right: &ValidationReport,
) -> HarnessResult<()> {
    if left.frequencies_hz != right.frequencies_hz {
        return Err(HarnessError::Invalid(
            "left/right validation frequency grids differ".into(),
        ));
    }
    let mut csv = String::from(
        "frequency_hz,left_raw_db,left_predicted_db,left_target_db,right_raw_db,right_predicted_db,right_target_db\n",
    );
    for index in 0..left.frequencies_hz.len() {
        writeln!(
            csv,
            "{:.9},{:.9},{:.9},{:.9},{:.9},{:.9},{:.9}",
            left.frequencies_hz[index],
            left.raw_spatial_average_db[index],
            left.predicted_spatial_average_db[index],
            left.aligned_target_db[index],
            right.raw_spatial_average_db[index],
            right.predicted_spatial_average_db[index],
            right.aligned_target_db[index],
        )
        .map_err(|error| HarnessError::Invalid(error.to_string()))?;
    }
    fs::write(output, csv)?;
    Ok(())
}

fn package_readme(left: &ValidationReport, right: &ValidationReport) -> String {
    format!(
        "EQforBeginner Phase 1 synthetic trial\r\n\
         ====================================\r\n\r\n\
         This archive contains a 48 kHz stereo IEEE-float WAV produced from a\r\n\
         deterministic synthetic room fixture. It validates package structure and\r\n\
         predicted DSP only. It is NOT measured for your room and is NOT a completed\r\n\
         correction filter.\r\n\r\n\
         Algorithm: {ALGORITHM_VERSION}\r\n\
         Target: B&K-style v1 (independent product curve)\r\n\
         Band: 20-500 Hz, unity taper through 650 Hz\r\n\
         Left raw/predicted peak RMSE: {:.3}/{:.3} dB\r\n\
         Right raw/predicted peak RMSE: {:.3}/{:.3} dB\r\n\r\n\
         Recommended headroom: unavailable until a filter is verified with a real\r\n\
         playback-path measurement and true-peak check. No fixed value is invented.\r\n\r\n\
         Future verified package application:\r\n\
         1. On Roon for Mac or Windows, disable any existing convolution so only one is active.\r\n\
         2. Open the zone volume control, MUSE, then Convolution and Browse to the ZIP.\r\n\
         3. Apply the package-specific recommended headroom.\r\n\
         4. Enable Roon's clipping indicator and increase attenuation if it turns red.\r\n",
        left.metrics.raw_peak_rmse_db,
        left.metrics.predicted_peak_rmse_db,
        right.metrics.raw_peak_rmse_db,
        right.metrics.predicted_peak_rmse_db,
    )
}

fn ensure_output_is_available(output: &Path) -> HarnessResult<bool> {
    match fs::symlink_metadata(output) {
        Ok(metadata) if !metadata.is_dir() => Err(HarnessError::Invalid(format!(
            "output path `{}` already exists and is not a directory; refusing to overwrite it",
            output.display()
        ))),
        Ok(_) => {
            let mut entries = fs::read_dir(output)?;
            if entries.next().transpose()?.is_some() {
                return Err(HarnessError::Invalid(format!(
                    "output directory `{}` is not empty; refusing to overwrite existing data",
                    output.display()
                )));
            }
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn verify_staged_example(generated: &GeneratedExample) -> HarnessResult<()> {
    let required_files = [
        generated.output_directory.join("README.txt"),
        generated.output_directory.join("filter-design.csv"),
        generated.output_directory.join("predicted-response.csv"),
        generated.output_directory.join("target.txt"),
        generated.project_file.clone(),
        generated.filter_wav.clone(),
        generated.roon_zip.clone(),
    ];
    for path in required_files {
        if !path.is_file() {
            return Err(HarnessError::Invalid(format!(
                "staged artifact `{}` is missing",
                path.display()
            )));
        }
    }
    for index in 0..6 {
        inspect_wav(
            &generated
                .output_directory
                .join(format!("input/P{index}_synthetic_room_ir.wav")),
        )?;
    }
    let filter = inspect_wav(&generated.filter_wav)?;
    if filter.sample_rate != 48_000 {
        return Err(HarnessError::Invalid(format!(
            "staged filter sample rate is {}, expected 48000",
            filter.sample_rate
        )));
    }
    validate_roon_zip(&generated.roon_zip, &BTreeSet::from([48_000]))?;

    let project: serde_json::Value = serde_json::from_slice(&fs::read(&generated.project_file)?)?;
    if project["verification_state"] != "predicted-only-synthetic"
        || project["results"]["passed"] != true
    {
        return Err(HarnessError::Invalid(
            "staged project lost its synthetic declaration or passed validation state".into(),
        ));
    }
    Ok(())
}

fn generate_example_in(output: &Path) -> HarnessResult<GeneratedExample> {
    let fixture = SyntheticRoomFixture::phase1_48k()?;
    let config = Phase1Config::default();
    let result = run_phase1(&fixture, &config)?;
    if !result.passed {
        return Err(HarnessError::Invalid(format!(
            "automatic validation failed: left={:?}, right={:?}",
            result.left_validation.issues, result.right_validation.issues
        )));
    }

    let input_directory = output.join("input");
    let export_directory = output.join("export");
    fs::create_dir_all(&input_directory)?;
    fs::create_dir_all(&export_directory)?;

    let mut positions = Vec::with_capacity(fixture.position_labels.len());
    for index in 0..fixture.position_labels.len() {
        let file_name = format!("P{index}_synthetic_room_ir.wav");
        let path = input_directory.join(&file_name);
        write_stereo_wav(
            &path,
            &StereoFir {
                sample_rate: fixture.sample_rate_hz,
                left: f32_samples(&fixture.left_impulses[index])?,
                right: f32_samples(&fixture.right_impulses[index])?,
            },
        )?;
        positions.push(json!({
            "id": format!("P{index}"),
            "label": fixture.position_labels[index],
            "weight": result.position_weights[index],
            "file": format!("input/{file_name}"),
            "channel_mapping": {"left_ir": 0, "right_ir": 1},
            "status": "accepted",
            "source": "deterministic-synthetic-fixture",
        }));
    }

    let filter_wav = export_directory.join("EQforBeginner_48000_stereo.wav");
    let filter_metadata = write_stereo_wav(
        &filter_wav,
        &StereoFir {
            sample_rate: result.sample_rate_hz,
            left: f32_samples(&result.left_fir.taps)?,
            right: f32_samples(&result.right_fir.taps)?,
        },
    )?;
    let readme = package_readme(&result.left_validation, &result.right_validation);
    fs::write(output.join("README.txt"), &readme)?;
    let roon_zip = export_directory.join("EQforBeginner_Phase1_Trial_Roon.zip");
    let package_manifest = create_roon_zip(
        &roon_zip,
        std::slice::from_ref(&filter_wav),
        ALGORITHM_VERSION,
        &readme,
    )?;

    write_prediction_csv(
        &output.join("predicted-response.csv"),
        &result.left_validation,
        &result.right_validation,
    )?;
    let mut design_csv = String::from(
        "frequency_hz,left_gain_db,right_gain_db,common_gain_db,left_protected_dip,right_protected_dip,left_spatial_dip_support,right_spatial_dip_support,left_boost_eligible,right_boost_eligible\n",
    );
    for index in 0..result.frequencies_hz.len() {
        writeln!(
            design_csv,
            "{:.9},{:.9},{:.9},{:.9},{},{},{:.9},{:.9},{},{}",
            result.frequencies_hz[index],
            result.stereo_design.left_gain_db[index],
            result.stereo_design.right_gain_db[index],
            result.stereo_design.common_gain_db[index],
            result.left_design.protected_dip[index],
            result.right_design.protected_dip[index],
            result.left_design.spatial_dip_support[index],
            result.right_design.spatial_dip_support[index],
            result.left_design.boost_eligible[index],
            result.right_design.boost_eligible[index],
        )
        .map_err(|error| HarnessError::Invalid(error.to_string()))?;
    }
    fs::write(output.join("filter-design.csv"), design_csv)?;

    let target_rows = config
        .target
        .knots()
        .iter()
        .map(|knot| format!("{} {}", knot.frequency_hz, knot.level_db))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        output.join("target.txt"),
        format!(
            "# {} | {}\n{target_rows}\n",
            config.target.name(),
            config.target.version()
        ),
    )?;

    let project_file = output.join("project.json");
    let project = json!({
        "schema_version": "eqforbeginner-project-v1-draft",
        "app_version": env!("CARGO_PKG_VERSION"),
        "dsp_algorithm_version": ALGORITHM_VERSION,
        "verification_state": "predicted-only-synthetic",
        "system_mode": "2.1-synthetic-regression",
        "sample_rate_hz": result.sample_rate_hz,
        "fixture": {
            "id": "phase1-six-position-room-v1",
            "fft_size": fixture.fft_size,
            "channels": ["left", "right"],
            "timeline": "synthetic-declared; not acoustic timing verification",
        },
        "positions": positions,
        "excluded_measurements": [],
        "settings": {
            "correction": {
                "correction_low_hz": config.correction.correction_low_hz,
                "correction_full_end_hz": config.correction.correction_full_end_hz,
                "taper_end_hz": config.correction.taper_end_hz,
                "maximum_attenuation_db": config.correction.maximum_attenuation_db,
                "maximum_boost_db": config.correction.maximum_boost_db,
                "smoothing_fwhm_octaves": config.correction.smoothing_fwhm_octaves,
                "deep_dip_threshold_db": config.correction.deep_dip_threshold_db,
                "dip_protection_half_width_octaves": config.correction.dip_protection_half_width_octaves,
                "spatial_lower_quantile": config.correction.spatial_lower_quantile,
                "minimum_supported_peak_db": config.correction.minimum_supported_peak_db,
                "minimum_supported_dip_db": config.correction.minimum_supported_dip_db,
                "boost_smoothing_fwhm_octaves": config.correction.boost_smoothing_fwhm_octaves,
                "minimum_boost_width_octaves": config.correction.minimum_boost_width_octaves,
                "target_reference_low_hz": config.correction.target_reference_low_hz,
                "target_reference_high_hz": config.correction.target_reference_high_hz,
            },
            "stereo_blend": {
                "common_below_hz": config.stereo_blend.common_below_hz,
                "channel_specific_above_hz": config.stereo_blend.channel_specific_above_hz,
            },
            "validation": {
                "required_peak_rmse_improvement_fraction": config.validation.required_peak_rmse_improvement_fraction,
                "required_overall_rmse_improvement_fraction": config.validation.required_overall_rmse_improvement_fraction,
                "required_max_peak_error_improvement_fraction": config.validation.required_max_peak_error_improvement_fraction,
                "minimum_actionable_rmse_db": config.validation.minimum_actionable_rmse_db,
                "maximum_positive_correction_db": config.validation.maximum_positive_correction_db,
                "maximum_attenuation_db": config.validation.maximum_attenuation_db,
            },
        },
        "target": {
            "name": config.target.name(),
            "version": config.target.version(),
            "file": "target.txt",
            "knots": config.target.knots().iter().map(|knot| json!({
                "frequency_hz": knot.frequency_hz,
                "level_db": knot.level_db,
            })).collect::<Vec<_>>(),
            "left_alignment_db": result.left_design.target_alignment_db,
            "right_alignment_db": result.right_design.target_alignment_db,
        },
        "results": {
            "passed": result.passed,
            "meaning": "offline predicted validation only; not real-room verification",
            "left": report_json(&result.left_validation),
            "right": report_json(&result.right_validation),
            "filter": filter_metadata,
            "fir_design": {
                "left": {
                    "filter_length_taps": result.left_fir.taps.len(),
                    "design_grid_bins": result.left_fir.design_gain_db.len(),
                    "safety_normalization_db": result.left_fir.safety_normalization_db,
                },
                "right": {
                    "filter_length_taps": result.right_fir.taps.len(),
                    "design_grid_bins": result.right_fir.design_gain_db.len(),
                    "safety_normalization_db": result.right_fir.safety_normalization_db,
                },
            },
            "warnings": {
                "left_correction": result.left_design.warnings.iter().map(correction_warning_json).collect::<Vec<_>>(),
                "right_correction": result.right_design.warnings.iter().map(correction_warning_json).collect::<Vec<_>>(),
                "left_validation_issues": result.left_validation.issues.iter().map(validation_issue_json).collect::<Vec<_>>(),
                "right_validation_issues": result.right_validation.issues.iter().map(validation_issue_json).collect::<Vec<_>>(),
            },
            "roon_trial_package": package_manifest,
            "recommended_headroom_db": null,
            "recommended_headroom_reason": "requires real playback-path true-peak verification",
        },
    });
    fs::write(&project_file, serde_json::to_vec_pretty(&project)?)?;

    Ok(GeneratedExample {
        output_directory: output.to_path_buf(),
        project_file,
        filter_wav,
        roon_zip,
    })
}

/// Generate the reproducible Phase 1 example without overwriting existing data.
///
/// All artifacts are written and validated in a temporary sibling directory,
/// then committed with a same-filesystem directory rename. An existing empty
/// output directory is accepted; files and non-empty directories are rejected
/// before generation begins.
pub fn generate_example(output: &Path) -> HarnessResult<GeneratedExample> {
    if output.file_name().is_none() {
        return Err(HarnessError::Invalid(format!(
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
        .prefix(".eqforbeginner-phase1-stage-")
        .tempdir_in(output_parent)?;
    let staged = generate_example_in(staging.path())?;
    verify_staged_example(&staged)?;

    let output_still_exists = ensure_output_is_available(output)?;
    if output_still_exists != output_existed {
        return Err(HarnessError::Invalid(format!(
            "output path `{}` changed while artifacts were being generated; refusing to commit",
            output.display()
        )));
    }

    match fs::rename(staging.path(), output) {
        Ok(()) => {}
        Err(first_error) if output_existed => {
            // Windows cannot replace even an empty directory with rename. Recheck
            // immediately before the portable fallback, and restore the empty
            // directory if the final rename fails.
            if !ensure_output_is_available(output)? {
                return Err(HarnessError::Invalid(format!(
                    "output path `{}` disappeared while artifacts were being committed",
                    output.display()
                )));
            }
            fs::remove_dir(output)?;
            if let Err(commit_error) = fs::rename(staging.path(), output) {
                let restore_error = fs::create_dir(output).err();
                return Err(HarnessError::Invalid(match restore_error {
                    Some(restore_error) => format!(
                        "could not commit staged output ({commit_error}); also could not restore the original empty directory ({restore_error})"
                    ),
                    None => format!(
                        "could not commit staged output after the platform rejected replacing an empty directory ({first_error}; {commit_error})"
                    ),
                }));
            }
        }
        Err(error) => return Err(error.into()),
    }
    let _committed_staging_path = staging.keep();

    Ok(GeneratedExample {
        output_directory: output.to_path_buf(),
        project_file: output.join("project.json"),
        filter_wav: output.join("export/EQforBeginner_48000_stereo.wav"),
        roon_zip: output.join("export/EQforBeginner_Phase1_Trial_Roon.zip"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generator_creates_validated_reproducible_slice() {
        let directory = tempdir().expect("temporary directory");
        let output = directory.path().join("phase1");
        fs::create_dir(&output).expect("create empty output directory");
        let generated = generate_example(&output).expect("generate example");
        assert!(generated.project_file.is_file());
        assert!(generated.filter_wav.is_file());
        assert!(generated.roon_zip.is_file());
        assert!(output.join("input/P0_synthetic_room_ir.wav").is_file());
        assert!(output.join("predicted-response.csv").is_file());

        let project: serde_json::Value =
            serde_json::from_slice(&fs::read(generated.project_file).expect("read project"))
                .expect("parse project");
        assert_eq!(project["results"]["passed"], true);
        assert_eq!(project["verification_state"], "predicted-only-synthetic");
        assert!(project["results"]["recommended_headroom_db"].is_null());
        assert_eq!(
            project["settings"]["correction"]
                .as_object()
                .expect("correction settings")
                .len(),
            15
        );
        assert_eq!(
            project["settings"]["correction"]["maximum_boost_db"],
            eqforbeginner_dsp_core::correction::MAXIMUM_SUPPORTED_BOOST_DB
        );
        assert_eq!(
            project["settings"]["stereo_blend"]
                .as_object()
                .expect("stereo settings")
                .len(),
            2
        );
        assert_eq!(
            project["settings"]["validation"]
                .as_object()
                .expect("validation settings")
                .len(),
            6
        );
        assert!(project["results"]["warnings"]["left_correction"].is_array());
        assert!(project["results"]["warnings"]["left_validation_issues"].is_array());
        assert!(
            project["results"]["fir_design"]["left"]["filter_length_taps"]
                .as_u64()
                .is_some_and(|length| length > 0)
        );
        assert!(
            project["results"]["fir_design"]["left"]["safety_normalization_db"]
                .as_f64()
                .is_some()
        );

        let entries = fs::read_dir(directory.path())
            .expect("read parent")
            .collect::<Result<Vec<_>, _>>()
            .expect("parent entries");
        assert_eq!(entries.len(), 1, "staging directory must not remain");
    }

    #[test]
    fn generator_refuses_nonempty_directory_without_changing_it() {
        let directory = tempdir().expect("temporary directory");
        let output = directory.path().join("phase1");
        fs::create_dir(&output).expect("create output");
        let sentinel = output.join("keep-me.bin");
        fs::write(&sentinel, b"user data").expect("write sentinel");

        let error = generate_example(&output).expect_err("must reject nonempty directory");

        assert!(error.to_string().contains("not empty"));
        assert_eq!(fs::read(&sentinel).expect("read sentinel"), b"user data");
        let output_entries = fs::read_dir(&output)
            .expect("read output")
            .collect::<Result<Vec<_>, _>>()
            .expect("output entries");
        assert_eq!(output_entries.len(), 1);
        let parent_entries = fs::read_dir(directory.path())
            .expect("read parent")
            .collect::<Result<Vec<_>, _>>()
            .expect("parent entries");
        assert_eq!(
            parent_entries.len(),
            1,
            "refusal must not create staging data"
        );
    }

    #[test]
    fn generator_refuses_existing_file_without_changing_it() {
        let directory = tempdir().expect("temporary directory");
        let output = directory.path().join("phase1");
        fs::write(&output, b"existing file").expect("write existing file");

        let error = generate_example(&output).expect_err("must reject existing file");

        assert!(error.to_string().contains("not a directory"));
        assert_eq!(
            fs::read(&output).expect("read existing file"),
            b"existing file"
        );
        let parent_entries = fs::read_dir(directory.path())
            .expect("read parent")
            .collect::<Result<Vec<_>, _>>()
            .expect("parent entries");
        assert_eq!(
            parent_entries.len(),
            1,
            "refusal must not create staging data"
        );
    }
}
