use super::{
    correction_warning_json, ensure_output_is_available, f32_samples, report_json, HarnessError,
    HarnessResult,
};
use crate::source_verification::{
    sha256_file, verify_declared_sources, SourceDeclaration, VerifiedSource,
};
use eqforbeginner_dsp_core::phase4::{
    run_phase4_offline, MeasuredStereoPosition, MeasuredStereoResponseSet, Phase4OfflineConfig,
    Phase4OfflineResult, SafeRedesignChannel, StereoChannel, TimedCombinedImpulse,
};
use eqforbeginner_dsp_core::sub_integration::{
    analyze_separated_paths, CombinedResponse, SeparatedPathAnalysis,
};
use eqforbeginner_export::{inspect_wav, write_stereo_wav, StereoFir};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::Builder;

// Historic on-disk format id, predates the product rename; not user-visible branding
// — do not rename. It is compared against the persisted fixture
// `measurments/derived/phase4-offline-measurements.json`.
const FIXTURE_SCHEMA: &str = "similarrew-phase4-offline-measurements-v1";
const PROJECT_SCHEMA: &str = "eqforbeginner-phase4-offline-project-v2";
const MODEL_GATE_MAX_RMSE_DB: f64 = 1.0;
const EXPECTED_RESPONSE_IDS: [&str; 6] = [
    "combined-left-xo90",
    "combined-right-xo90",
    "main-left-xo90",
    "main-right-xo90",
    "sub-xo90-a",
    "sub-xo90-b",
];

#[derive(Debug, Clone, PartialEq)]
pub struct GeneratedPhase4Analysis {
    pub output_directory: PathBuf,
    pub project_file: PathBuf,
    pub filter_wav: PathBuf,
    pub readme: PathBuf,
    pub numerical_passed: bool,
    pub verification_state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Phase4Fixture {
    schema_version: String,
    extraction: ExtractionMetadata,
    assumptions: Assumptions,
    frequency_grid_hz: Vec<f64>,
    responses: BTreeMap<String, ResponseFixture>,
    combined_impulses: BTreeMap<String, ImpulseFixture>,
    design_inputs: DesignInputs,
    separated_references: Vec<SeparatedReferenceFixture>,
    post_fir_measurements: Vec<serde_json::Value>,
    limitations: Vec<String>,
    required_evidence_missing: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionMetadata {
    extractor_version: String,
    dependency: String,
    source_rew_build: String,
    preferred_future_path: String,
    extracted_analysis_range_hz: [f64; 2],
    source_sweep_range_hz: [f64; 2],
    level_alignment_applied: bool,
    timeline_shift_applied: bool,
    maximum_source_bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Assumptions {
    declaration_date: String,
    source: String,
    verified: bool,
    system: String,
    crossover_hz: f64,
    main_delay_ms: f64,
    main_delay_status: String,
    polarity: String,
    sub_level: String,
    playback_volume: String,
    microphone_gain: String,
    post_fir_measurement_deferred_until: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseFixture {
    source_path: String,
    source_sha256: String,
    source_bytes: u64,
    measurement_title: String,
    measurement_notes: String,
    rew_version: String,
    sample_rate_hz: u32,
    magnitude_db_spl: Vec<f64>,
    unwrapped_phase_degrees: Vec<f64>,
    calibration: CalibrationFixture,
    quality: QualityFixture,
    timing: TimingFixture,
    source_valid_range_hz: [f64; 2],
    filter_set_when_measured_present: bool,
    role: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalibrationFixture {
    embedded_calibration_applied: bool,
    calibration_limit_applied: bool,
    microphone_serial: u64,
    microphone_calibration_file: String,
    spl_calibration_offset_db: Option<f64>,
    frequency_range_hz: [f64; 2],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityFixture {
    signal_to_noise_db: Option<f64>,
    signal_to_distortion_db: Option<f64>,
    signal_dbfs: Option<f64>,
    noise_and_distortion_dbfs: Option<f64>,
    known_warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimingFixture {
    used_acoustic_reference: bool,
    reference_stimulus: String,
    measurement_delay_ms: f64,
    timing_offset_ms: f64,
    cumulative_start_time_offset_ms: f64,
    clock_adjustment_ppm: f64,
    original_peak_time_ms: f64,
    timeline_policy: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImpulseFixture {
    response_id: String,
    sample_interval_s: f64,
    start_time_s: f64,
    timeline_shift_applied: bool,
    samples: Vec<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DesignInputs {
    position_id: String,
    position_weight: f64,
    left_combined_response_id: String,
    right_combined_response_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeparatedReferenceFixture {
    id: String,
    crossover_hz: f64,
    channel: String,
    main_response_id: String,
    sub_response_id: String,
    combined_response_id: String,
}

#[derive(Debug)]
struct SeparatedDiagnostic {
    id: String,
    channel: String,
    sub_repeat: String,
    analysis: SeparatedPathAnalysis,
    model_gate_passed: bool,
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::Invalid(message.into())
}

fn approximately(left: f64, right: f64, tolerance: f64) -> bool {
    left.is_finite() && right.is_finite() && (left - right).abs() <= tolerance
}

fn finite_optional(value: Option<f64>) -> bool {
    value.is_none_or(f64::is_finite)
}

fn expected_role(id: &str) -> Option<&'static str> {
    if id.starts_with("combined-") {
        Some("combined")
    } else if id.starts_with("main-") {
        Some("main-only")
    } else if id.starts_with("sub-") {
        Some("sub-only")
    } else {
        None
    }
}

fn response<'a>(fixture: &'a Phase4Fixture, id: &str) -> HarnessResult<&'a ResponseFixture> {
    fixture
        .responses
        .get(id)
        .ok_or_else(|| invalid(format!("fixture response `{id}` does not exist")))
}

fn validate_extraction(fixture: &Phase4Fixture) -> HarnessResult<()> {
    let extraction = &fixture.extraction;
    if !extraction.extractor_version.ends_with("grid-origin")
        || extraction.dependency.trim().is_empty()
        || extraction.source_rew_build != "5.31.3"
        || extraction.preferred_future_path.trim().is_empty()
        || extraction.level_alignment_applied
        || extraction.timeline_shift_applied
        || extraction.maximum_source_bytes < 1_000_000
        || extraction.extracted_analysis_range_hz != [20.0, 3_000.0]
        || extraction.source_sweep_range_hz != [20.0, 20_000.0]
    {
        return Err(invalid(
            "Phase 4 extraction metadata is incompatible or declares level/timeline alteration",
        ));
    }
    Ok(())
}

fn validate_assumptions(assumptions: &Assumptions) -> HarnessResult<()> {
    let unchanged = "unchanged-value-not-recorded";
    if assumptions.source != "user-declared"
        || assumptions.verified
        || assumptions.system != "2.1 single subwoofer"
        || !approximately(assumptions.crossover_hz, 90.0, 1.0e-9)
        || !approximately(assumptions.main_delay_ms, 0.83, 1.0e-9)
        || assumptions.main_delay_status != "assumed-optimal-not-app-optimized"
        || assumptions.polarity != unchanged
        || assumptions.sub_level != unchanged
        || assumptions.playback_volume != unchanged
        || assumptions.microphone_gain != unchanged
        || assumptions.declaration_date.trim().is_empty()
        || assumptions
            .post_fir_measurement_deferred_until
            .trim()
            .is_empty()
    {
        return Err(invalid(
            "Phase 4 user-declared assumptions changed or were incorrectly marked verified",
        ));
    }
    Ok(())
}

fn validate_frequency_grid(grid: &[f64]) -> HarnessResult<()> {
    if grid.len() < 100
        || grid.iter().any(|value| !value.is_finite())
        || grid
            .windows(2)
            .any(|pair| pair[1] <= pair[0] || pair[1] - pair[0] > 0.5)
        || !(20.0..=20.2).contains(&grid[0])
        || !(2_999.5..=3_000.0).contains(grid.last().expect("grid checked nonempty"))
    {
        return Err(invalid(
            "Phase 4 frequency grid must be the finite, increasing 20-3000 Hz REW grid",
        ));
    }
    Ok(())
}

fn validate_response_metadata(fixture: &Phase4Fixture) -> HarnessResult<()> {
    let actual_ids: BTreeSet<&str> = fixture.responses.keys().map(String::as_str).collect();
    let expected_ids: BTreeSet<&str> = EXPECTED_RESPONSE_IDS.into_iter().collect();
    if actual_ids != expected_ids {
        return Err(invalid(
            "Phase 4 fixture must contain exactly two combined, two main-only and two sub-only responses",
        ));
    }

    let mut microphone_serial = None;
    let mut calibration_file: Option<&str> = None;
    let mut spl_offset = None;
    for (id, measured) in &fixture.responses {
        if measured.sample_rate_hz != 48_000
            || measured.rew_version != fixture.extraction.source_rew_build
            || measured.magnitude_db_spl.len() != fixture.frequency_grid_hz.len()
            || measured.unwrapped_phase_degrees.len() != fixture.frequency_grid_hz.len()
            || measured
                .magnitude_db_spl
                .iter()
                .chain(&measured.unwrapped_phase_degrees)
                .any(|value| !value.is_finite())
            || measured.measurement_title.trim().is_empty()
            || measured.measurement_notes.trim().is_empty()
            || measured.role != expected_role(id).unwrap_or_default()
            || measured.source_valid_range_hz[0] > 20.2
            || measured.source_valid_range_hz[1] < 19_999.0
            || measured.filter_set_when_measured_present
        {
            return Err(invalid(format!(
                "measurement `{id}` has incompatible response, role, sample-rate or source-range metadata"
            )));
        }
        let calibration = &measured.calibration;
        if !calibration.embedded_calibration_applied
            || !calibration.calibration_limit_applied
            || calibration.frequency_range_hz[0] > 20.0
            || calibration.frequency_range_hz[1] < 20_000.0
            || calibration.microphone_calibration_file.trim().is_empty()
            || !finite_optional(calibration.spl_calibration_offset_db)
        {
            return Err(invalid(format!(
                "measurement `{id}` lacks compatible microphone calibration evidence"
            )));
        }
        if microphone_serial
            .replace(calibration.microphone_serial)
            .is_some_and(|previous| previous != calibration.microphone_serial)
            || calibration_file
                .replace(&calibration.microphone_calibration_file)
                .is_some_and(|previous| previous != calibration.microphone_calibration_file)
            || spl_offset
                .replace(calibration.spl_calibration_offset_db)
                .is_some_and(|previous| previous != calibration.spl_calibration_offset_db)
        {
            return Err(invalid(
                "Phase 4 responses do not share one microphone calibration identity and SPL offset",
            ));
        }

        let quality = &measured.quality;
        if !finite_optional(quality.signal_to_noise_db)
            || !finite_optional(quality.signal_to_distortion_db)
            || !finite_optional(quality.signal_dbfs)
            || !finite_optional(quality.noise_and_distortion_dbfs)
            || quality.signal_dbfs.is_some_and(|level| level >= -1.0)
            || quality
                .noise_and_distortion_dbfs
                .is_some_and(|level| level >= -1.0)
            || quality.signal_to_noise_db.is_none_or(|snr| snr < 20.0)
        {
            return Err(invalid(format!(
                "measurement `{id}` fails the offline clipping/SNR metadata gate"
            )));
        }
        if id.starts_with("combined-") && quality.signal_to_noise_db.is_none_or(|snr| snr < 30.0) {
            return Err(invalid(format!(
                "combined measurement `{id}` has less than 30 dB SNR"
            )));
        }
        if id.starts_with("sub-")
            && (quality
                .signal_to_distortion_db
                .is_some_and(|value| value < 20.0)
                != !quality.known_warnings.is_empty())
        {
            return Err(invalid(format!(
                "sub measurement `{id}` must explicitly retain its low-distortion-evidence warning"
            )));
        }

        let timing = &measured.timing;
        let timing_values = [
            timing.measurement_delay_ms,
            timing.timing_offset_ms,
            timing.cumulative_start_time_offset_ms,
            timing.clock_adjustment_ppm,
            timing.original_peak_time_ms,
        ];
        if !timing.used_acoustic_reference
            || timing.reference_stimulus.trim().is_empty()
            || timing.timeline_policy != "raw REW timeline retained; no peak-to-zero shift applied"
            || timing_values.iter().any(|value| !value.is_finite())
            || timing.clock_adjustment_ppm.abs() > 100.0
        {
            return Err(invalid(format!(
                "measurement `{id}` lacks a valid retained acoustic timing reference"
            )));
        }
    }
    Ok(())
}

fn validate_impulses(fixture: &Phase4Fixture) -> HarnessResult<()> {
    let expected: BTreeSet<&str> = ["combined-left-xo90", "combined-right-xo90"]
        .into_iter()
        .collect();
    let actual: BTreeSet<&str> = fixture
        .combined_impulses
        .keys()
        .map(String::as_str)
        .collect();
    if actual != expected {
        return Err(invalid(
            "Phase 4 fixture must retain exactly the L/R combined raw IR timelines",
        ));
    }
    for (id, impulse) in &fixture.combined_impulses {
        if impulse.response_id != *id
            || impulse.timeline_shift_applied
            || !approximately(impulse.sample_interval_s, 1.0 / 48_000.0, 1.0e-12)
            || !impulse.start_time_s.is_finite()
            || impulse.samples.is_empty()
            || impulse.samples.len() > 262_144
            || impulse.samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(invalid(format!(
                "combined impulse `{id}` has an invalid or shifted 48 kHz timeline"
            )));
        }
    }
    Ok(())
}

fn validate_fixture(fixture: &Phase4Fixture) -> HarnessResult<()> {
    if fixture.schema_version != FIXTURE_SCHEMA {
        return Err(invalid(format!(
            "unsupported Phase 4 fixture schema `{}`",
            fixture.schema_version
        )));
    }
    validate_extraction(fixture)?;
    validate_assumptions(&fixture.assumptions)?;
    validate_frequency_grid(&fixture.frequency_grid_hz)?;
    validate_response_metadata(fixture)?;
    validate_impulses(fixture)?;
    if !fixture.post_fir_measurements.is_empty() {
        return Err(invalid(
            "this offline command accepts no post-FIR measurement; use the future closed-loop validator instead",
        ));
    }
    if fixture.limitations.is_empty()
        || fixture.required_evidence_missing.len() < 5
        || fixture
            .limitations
            .iter()
            .any(|item| item.trim().is_empty())
        || fixture
            .required_evidence_missing
            .iter()
            .any(|item| item.trim().is_empty())
    {
        return Err(invalid(
            "Phase 4 fixture must retain limitations and all missing closed-loop evidence",
        ));
    }
    if fixture.design_inputs.position_id != "P0"
        || !approximately(fixture.design_inputs.position_weight, 2.0, 1.0e-12)
        || fixture.design_inputs.left_combined_response_id != "combined-left-xo90"
        || fixture.design_inputs.right_combined_response_id != "combined-right-xo90"
    {
        return Err(invalid("Phase 4 design-input mapping changed"));
    }
    Ok(())
}

fn verify_sources(
    fixture: &Phase4Fixture,
    source_root: &Path,
) -> HarnessResult<Vec<VerifiedSource>> {
    verify_declared_sources(
        source_root,
        fixture
            .responses
            .iter()
            .map(|(id, response)| SourceDeclaration {
                id,
                relative_path: &response.source_path,
                sha256: &response.source_sha256,
                bytes: response.source_bytes,
            }),
    )
}

fn combined_response(frequencies_hz: &[f64], response: &ResponseFixture) -> CombinedResponse {
    CombinedResponse {
        frequencies_hz: frequencies_hz.to_vec(),
        magnitude_db: response.magnitude_db_spl.clone(),
        phase_rad: Some(
            response
                .unwrapped_phase_degrees
                .iter()
                .map(|phase| phase.to_radians())
                .collect(),
        ),
        timing: None,
    }
}

fn separated_diagnostics(fixture: &Phase4Fixture) -> HarnessResult<Vec<SeparatedDiagnostic>> {
    let expected_ids: BTreeSet<&str> = [
        "xo90-left-sub-a",
        "xo90-left-sub-b",
        "xo90-right-sub-a",
        "xo90-right-sub-b",
    ]
    .into_iter()
    .collect();
    let actual_ids: BTreeSet<&str> = fixture
        .separated_references
        .iter()
        .map(|reference| reference.id.as_str())
        .collect();
    if actual_ids != expected_ids || fixture.separated_references.len() != 4 {
        return Err(invalid(
            "Phase 4 fixture must contain exactly four L/R x Sub-A/B separated-path references",
        ));
    }

    let mut diagnostics = Vec::with_capacity(4);
    for reference in &fixture.separated_references {
        if !approximately(reference.crossover_hz, 90.0, 1.0e-9)
            || !matches!(reference.channel.as_str(), "left" | "right")
        {
            return Err(invalid(format!(
                "separated reference `{}` has incompatible hardware metadata",
                reference.id
            )));
        }
        let expected_main = format!("main-{}-xo90", reference.channel);
        let expected_combined = format!("combined-{}-xo90", reference.channel);
        if reference.main_response_id != expected_main
            || reference.combined_response_id != expected_combined
            || !matches!(
                reference.sub_response_id.as_str(),
                "sub-xo90-a" | "sub-xo90-b"
            )
        {
            return Err(invalid(format!(
                "separated reference `{}` maps the wrong response roles",
                reference.id
            )));
        }
        let analysis = analyze_separated_paths(
            &combined_response(
                &fixture.frequency_grid_hz,
                response(fixture, &reference.main_response_id)?,
            ),
            &combined_response(
                &fixture.frequency_grid_hz,
                response(fixture, &reference.sub_response_id)?,
            ),
            &combined_response(
                &fixture.frequency_grid_hz,
                response(fixture, &reference.combined_response_id)?,
            ),
            reference.crossover_hz,
        )?;
        let model_gate_passed = analysis.complex_sum_magnitude_rmse_db <= MODEL_GATE_MAX_RMSE_DB;
        diagnostics.push(SeparatedDiagnostic {
            id: reference.id.clone(),
            channel: reference.channel.clone(),
            sub_repeat: reference.sub_response_id.clone(),
            analysis,
            model_gate_passed,
        });
    }
    if diagnostics
        .iter()
        .any(|diagnostic| !diagnostic.model_gate_passed)
    {
        return Err(invalid(format!(
            "separated-path consistency gate exceeded {MODEL_GATE_MAX_RMSE_DB:.1} dB"
        )));
    }
    Ok(diagnostics)
}

fn phase4_input(fixture: &Phase4Fixture) -> HarnessResult<MeasuredStereoResponseSet> {
    let left_id = &fixture.design_inputs.left_combined_response_id;
    let right_id = &fixture.design_inputs.right_combined_response_id;
    let left_response = response(fixture, left_id)?;
    let right_response = response(fixture, right_id)?;
    let left_ir = fixture
        .combined_impulses
        .get(left_id)
        .ok_or_else(|| invalid("left combined IR is missing"))?;
    let right_ir = fixture
        .combined_impulses
        .get(right_id)
        .ok_or_else(|| invalid("right combined IR is missing"))?;
    Ok(MeasuredStereoResponseSet {
        sample_rate_hz: 48_000,
        frequencies_hz: fixture.frequency_grid_hz.clone(),
        positions: vec![MeasuredStereoPosition {
            id: fixture.design_inputs.position_id.clone(),
            weight: fixture.design_inputs.position_weight,
            left_magnitude_db: left_response.magnitude_db_spl.clone(),
            right_magnitude_db: right_response.magnitude_db_spl.clone(),
            left_timed_combined_ir: Some(TimedCombinedImpulse {
                samples: left_ir.samples.clone(),
                start_time_seconds: left_ir.start_time_s,
                arrival_time_seconds: Some(left_response.timing.original_peak_time_ms / 1_000.0),
            }),
            right_timed_combined_ir: Some(TimedCombinedImpulse {
                samples: right_ir.samples.clone(),
                start_time_seconds: right_ir.start_time_s,
                arrival_time_seconds: Some(right_response.timing.original_peak_time_ms / 1_000.0),
            }),
        }],
    })
}

fn safe_redesign_json(channel: &SafeRedesignChannel) -> serde_json::Value {
    json!({
        "applied": channel.applied,
        "original_requested_maximum_attenuation_db": channel.original_requested_maximum_attenuation_db,
        "original_limit_db": channel.original_limit_db,
        "redesign_target_maximum_attenuation_db": channel.redesign_target_maximum_attenuation_db,
        "redesign_strength": channel.redesign_strength,
        "redesigned_maximum_attenuation_db": channel.redesigned_maximum_attenuation_db,
        "resolved": channel.resolved,
    })
}

fn separated_json(diagnostic: &SeparatedDiagnostic) -> serde_json::Value {
    json!({
        "id": diagnostic.id,
        "channel": diagnostic.channel,
        "sub_repeat": diagnostic.sub_repeat,
        "band": {
            "lower_hz": diagnostic.analysis.band.lower_hz,
            "upper_hz": diagnostic.analysis.band.upper_hz,
            "bin_count": diagnostic.analysis.band.bin_count,
        },
        "complex_sum_magnitude_rmse_db": diagnostic.analysis.complex_sum_magnitude_rmse_db,
        "cancellation_loss_rms_db": diagnostic.analysis.cancellation_loss_rms_db,
        "cancellation_loss_p95_db": diagnostic.analysis.cancellation_loss_p95_db,
        "cancellation_loss_worst_db": diagnostic.analysis.cancellation_loss_worst_db,
        "model_gate_maximum_rmse_db": MODEL_GATE_MAX_RMSE_DB,
        "model_gate_passed": diagnostic.model_gate_passed,
    })
}

fn source_json(source: &VerifiedSource) -> serde_json::Value {
    json!({
        "id": source.id,
        "relative_path": source.relative_path,
        "sha256": source.sha256,
        "bytes": source.bytes,
        "hash_and_size_verified": true,
    })
}

fn write_filter_design_csv(output: &Path, result: &Phase4OfflineResult) -> HarnessResult<()> {
    if result.design_frequencies_hz.len() != result.measured_grid_stereo_design.left_gain_db.len()
        || result.design_frequencies_hz.len() != result.left_design.protected_dip.len()
        || result.design_frequencies_hz.len() != result.right_design.protected_dip.len()
        || result.design_frequencies_hz.len() != result.left_design.spatial_dip_support.len()
        || result.design_frequencies_hz.len() != result.right_design.spatial_dip_support.len()
        || result.design_frequencies_hz.len() != result.left_design.boost_eligible.len()
        || result.design_frequencies_hz.len() != result.right_design.boost_eligible.len()
    {
        return Err(invalid("Phase 4 design output grids differ"));
    }
    let mut csv = String::from(
        "frequency_hz,left_gain_db,right_gain_db,common_gain_db,channel_specific_mix,left_protected_dip,right_protected_dip,left_spatial_support,right_spatial_support,left_spatial_dip_support,right_spatial_dip_support,left_boost_eligible,right_boost_eligible\n",
    );
    for index in 0..result.design_frequencies_hz.len() {
        writeln!(
            csv,
            "{:.9},{:.9},{:.9},{:.9},{:.9},{},{},{:.9},{:.9},{:.9},{:.9},{},{}",
            result.design_frequencies_hz[index],
            result.measured_grid_stereo_design.left_gain_db[index],
            result.measured_grid_stereo_design.right_gain_db[index],
            result.measured_grid_stereo_design.common_gain_db[index],
            result.measured_grid_stereo_design.channel_specific_mix[index],
            result.left_design.protected_dip[index],
            result.right_design.protected_dip[index],
            result.left_design.spatial_support[index],
            result.right_design.spatial_support[index],
            result.left_design.spatial_dip_support[index],
            result.right_design.spatial_dip_support[index],
            result.left_design.boost_eligible[index],
            result.right_design.boost_eligible[index],
        )
        .map_err(|error| invalid(error.to_string()))?;
    }
    fs::write(output, csv)?;
    Ok(())
}

fn write_prediction_csv(output: &Path, result: &Phase4OfflineResult) -> HarnessResult<()> {
    let left = &result.left_validation;
    let right = &result.right_validation;
    if left.frequencies_hz != right.frequencies_hz {
        return Err(invalid("Phase 4 L/R validation grids differ"));
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
        .map_err(|error| invalid(error.to_string()))?;
    }
    fs::write(output, csv)?;
    Ok(())
}

fn trial_readme(result: &Phase4OfflineResult, fixture: &Phase4Fixture) -> String {
    format!(
        "EQforBeginner Phase 4 offline measured-response trial\r\n\
         ===================================================\r\n\r\n\
         State: {}\r\n\
         Numerical prediction gates passed: {}\r\n\
         Hardware verification: UNVERIFIED\r\n\r\n\
         This 48 kHz minimum-phase FIR was designed from the measured L+sub and\r\n\
         R+sub baseline responses. It was NOT played through the real system and\r\n\
         remeasured. Do not describe it as closed-loop verified and do not use it\r\n\
         as a final Roon export. No Roon ZIP is produced by this command.\r\n\r\n\
         Assumed hardware state: 90 Hz crossover, 0.83 ms main delay (user-declared\r\n\
         optimal), unchanged polarity/sub level/playback volume/microphone gain.\r\n\
         Only central position P0 is available.\r\n\r\n\
         Target: B&K-style house curve v1 (independent product curve)\r\n\
         Correction: 20-500 Hz, unity taper through 650 Hz; repeated broad\r\n\
         shallow dips may receive at most +3 dB, while deep/narrow dips remain\r\n\
         protected\r\n\
         Algorithm: {}\r\n\
         FIR: {} taps, stereo float32 WAV, 48 kHz\r\n\r\n\
         Left raw/predicted RMSE: {:.3}/{:.3} dB\r\n\
         Right raw/predicted RMSE: {:.3}/{:.3} dB\r\n\
         Left raw/predicted broad-peak RMSE: {:.3}/{:.3} dB\r\n\
         Right raw/predicted broad-peak RMSE: {:.3}/{:.3} dB\r\n\r\n\
         Recommended headroom: unavailable. A room-IR sample maximum is not a\r\n\
         playback-signal true peak. Headroom remains null until real-path testing.\r\n\r\n\
         Missing evidence:\r\n{}",
        result.verification_state.as_str(),
        result.numerical_passed,
        result.algorithm_version,
        result.fir_fft_size,
        result.left_validation.metrics.raw_rmse_db,
        result.left_validation.metrics.predicted_rmse_db,
        result.right_validation.metrics.raw_rmse_db,
        result.right_validation.metrics.predicted_rmse_db,
        result.left_validation.metrics.raw_peak_rmse_db,
        result.left_validation.metrics.predicted_peak_rmse_db,
        result.right_validation.metrics.raw_peak_rmse_db,
        result.right_validation.metrics.predicted_peak_rmse_db,
        fixture
            .required_evidence_missing
            .iter()
            .map(|item| format!("         - {item}\r\n"))
            .collect::<String>(),
    )
}

fn project_json(
    fixture: &Phase4Fixture,
    verified_sources: &[VerifiedSource],
    diagnostics: &[SeparatedDiagnostic],
    result: &Phase4OfflineResult,
    filter_metadata: &eqforbeginner_export::FilterFile,
    filter_sha256: &str,
    filter_bytes: u64,
) -> serde_json::Value {
    let config = Phase4OfflineConfig::default();
    json!({
        "schema_version": PROJECT_SCHEMA,
        "app_version": env!("CARGO_PKG_VERSION"),
        "dsp_algorithm_version": result.algorithm_version,
        "verification_state": result.verification_state.as_str(),
        "hardware_verification": "unverified",
        "closed_loop_passed": null,
        "export_eligible": false,
        "recommended_headroom_db": null,
        "recommended_headroom_reason": "requires an actual FIR-applied playback-path measurement and true-peak check",
        "numerical_prediction_passed": result.numerical_passed,
        "system_mode": "2.1-single-subwoofer",
        "sample_rate_hz": result.sample_rate_hz,
        "assumptions": fixture.assumptions,
        "measurement_sources": verified_sources.iter().map(source_json).collect::<Vec<_>>(),
        "source_fixture": {
            "schema_version": fixture.schema_version,
            "extractor_version": fixture.extraction.extractor_version,
            "source_rew_build": fixture.extraction.source_rew_build,
            "level_alignment_applied": fixture.extraction.level_alignment_applied,
            "timeline_shift_applied": fixture.extraction.timeline_shift_applied,
        },
        "settings": {
            "target": {
                "name": config.target.name(),
                "version": config.target.version(),
                "left_alignment_db": result.left_design.target_alignment_db,
                "right_alignment_db": result.right_design.target_alignment_db,
            },
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
                "required_max_peak_error_improvement_fraction": config.validation.required_max_peak_error_improvement_fraction,
                "required_overall_rmse_improvement_fraction": config.validation.required_overall_rmse_improvement_fraction,
                "minimum_actionable_rmse_db": config.validation.minimum_actionable_rmse_db,
                "maximum_positive_correction_db": config.validation.maximum_positive_correction_db,
                "maximum_attenuation_db": config.validation.maximum_attenuation_db,
                "maximum_protected_dip_attenuation_db": config.maximum_protected_dip_attenuation_db,
                "maximum_protected_dip_boost_db": config.maximum_protected_dip_boost_db,
            },
            "fir_fft_size": config.fir_fft_size,
            "safe_redesign_maximum_attenuation_db": config.safe_redesign_maximum_attenuation_db,
        },
        "results": {
            "meaning": "measured-baseline response replay and numerical prediction only; not real-path verification",
            "left": report_json(&result.left_validation),
            "right": report_json(&result.right_validation),
            "protected_dips": {
                "allowed_attenuation_db": result.protected_dip_validation.allowed_attenuation_db,
                "allowed_boost_db": result.protected_dip_validation.allowed_boost_db,
                "left_maximum_attenuation_db": result.protected_dip_validation.left_maximum_attenuation_db,
                "right_maximum_attenuation_db": result.protected_dip_validation.right_maximum_attenuation_db,
                "left_maximum_boost_db": result.protected_dip_validation.left_maximum_boost_db,
                "right_maximum_boost_db": result.protected_dip_validation.right_maximum_boost_db,
                "left_passed": result.protected_dip_validation.left_passed,
                "right_passed": result.protected_dip_validation.right_passed,
            },
            "safe_redesign": {
                "left": safe_redesign_json(&result.safe_redesign.left),
                "right": safe_redesign_json(&result.safe_redesign.right),
            },
            "separated_path_consistency": diagnostics.iter().map(separated_json).collect::<Vec<_>>(),
            "filter": filter_metadata,
            "filter_integrity": {
                "sha256": filter_sha256,
                "bytes": filter_bytes,
            },
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
            "time_domain_diagnostics": result.time_domain_diagnostics.iter().map(|diagnostic| json!({
                "position_id": diagnostic.position_id,
                "channel": match diagnostic.channel { StereoChannel::Left => "left", StereoChannel::Right => "right" },
                "input_start_time_seconds": diagnostic.input_start_time_seconds,
                "predicted_start_time_seconds": diagnostic.predicted_start_time_seconds,
                "input_sample_count": diagnostic.input_sample_count,
                "predicted_sample_count": diagnostic.predicted_sample_count,
                "maximum_absolute_ir_sample": diagnostic.maximum_absolute_ir_sample,
                "meaning": "room-IR convolution diagnostic only; not clipping or true-peak evidence",
            })).collect::<Vec<_>>(),
            "correction_warnings": {
                "left": result.left_design.warnings.iter().map(correction_warning_json).collect::<Vec<_>>(),
                "right": result.right_design.warnings.iter().map(correction_warning_json).collect::<Vec<_>>(),
            },
        },
        "limitations": fixture.limitations,
        "required_evidence_missing": fixture.required_evidence_missing,
        "forbidden_promotions_without_new_measurements": [
            "closed-loop verified",
            "recommended headroom",
            "final Roon export",
        ],
    })
}

fn generate_in(
    fixture: &Phase4Fixture,
    verified_sources: &[VerifiedSource],
    diagnostics: &[SeparatedDiagnostic],
    output: &Path,
) -> HarnessResult<GeneratedPhase4Analysis> {
    let result = run_phase4_offline(&phase4_input(fixture)?, &Phase4OfflineConfig::default())?;
    fs::create_dir_all(output.join("filter"))?;
    let filter_wav = output.join("filter/EQforBeginner_48000_Phase4_Trial.wav");
    let filter_metadata = write_stereo_wav(
        &filter_wav,
        &StereoFir {
            sample_rate: result.sample_rate_hz,
            left: f32_samples(&result.left_fir.taps)?,
            right: f32_samples(&result.right_fir.taps)?,
        },
    )?;
    let (filter_sha256, filter_bytes) = sha256_file(&filter_wav)?;
    write_filter_design_csv(&output.join("filter-design.csv"), &result)?;
    write_prediction_csv(&output.join("predicted-response.csv"), &result)?;
    let target_rows = Phase4OfflineConfig::default()
        .target
        .knots()
        .iter()
        .map(|knot| format!("{} {}", knot.frequency_hz, knot.level_db))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        output.join("target.txt"),
        format!("# B&K-style house curve | bk-style-v1-log-f-linear-db\n{target_rows}\n"),
    )?;
    let readme = output.join("README.txt");
    fs::write(&readme, trial_readme(&result, fixture))?;
    let project_file = output.join("project.json");
    fs::write(
        &project_file,
        serde_json::to_vec_pretty(&project_json(
            fixture,
            verified_sources,
            diagnostics,
            &result,
            &filter_metadata,
            &filter_sha256,
            filter_bytes,
        ))?,
    )?;
    Ok(GeneratedPhase4Analysis {
        output_directory: output.to_path_buf(),
        project_file,
        filter_wav,
        readme,
        numerical_passed: result.numerical_passed,
        verification_state: result.verification_state.as_str().into(),
    })
}

fn verify_staged(generated: &GeneratedPhase4Analysis) -> HarnessResult<()> {
    for required in [
        generated.project_file.clone(),
        generated.filter_wav.clone(),
        generated.readme.clone(),
        generated.output_directory.join("filter-design.csv"),
        generated.output_directory.join("predicted-response.csv"),
        generated.output_directory.join("target.txt"),
    ] {
        if !required.is_file() {
            return Err(invalid(format!(
                "staged Phase 4 artifact `{}` is missing",
                required.display()
            )));
        }
    }
    let wav = inspect_wav(&generated.filter_wav)?;
    if wav.sample_rate != 48_000
        || wav.channels != 2
        || wav.bits_per_sample != 32
        || wav.sample_format != "ieee-float"
    {
        return Err(invalid("staged Phase 4 FIR is not stereo 48 kHz float32"));
    }
    if fs::read_dir(&generated.output_directory)?
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "zip")
        })
    {
        return Err(invalid("offline Phase 4 must not emit a Roon ZIP"));
    }
    let project: serde_json::Value = serde_json::from_slice(&fs::read(&generated.project_file)?)?;
    let (filter_sha256, filter_bytes) = sha256_file(&generated.filter_wav)?;
    if project["verification_state"] != "predicted-only-measured"
        || project["hardware_verification"] != "unverified"
        || !project["closed_loop_passed"].is_null()
        || project["export_eligible"] != false
        || !project["recommended_headroom_db"].is_null()
        || project["measurement_sources"].as_array().map(Vec::len) != Some(6)
        || project["results"]["filter_integrity"]["sha256"] != filter_sha256
        || project["results"]["filter_integrity"]["bytes"] != filter_bytes
    {
        return Err(invalid(
            "staged Phase 4 project attempted to promote an offline prediction or lost source evidence",
        ));
    }
    Ok(())
}

/// Replay the trusted measured Phase 4 fixture without overwriting user data.
///
/// The returned state can only be `predicted-only-measured`. This function never
/// creates a Roon ZIP and cannot authorize a recommended headroom.
pub fn analyze_phase4_offline(
    dataset: &Path,
    source_root: &Path,
    output: &Path,
) -> HarnessResult<GeneratedPhase4Analysis> {
    if output.file_name().is_none() {
        return Err(invalid(format!(
            "output path `{}` must name a directory below its parent",
            output.display()
        )));
    }
    let fixture: Phase4Fixture = serde_json::from_slice(&fs::read(dataset)?)?;
    validate_fixture(&fixture)?;
    let verified_sources = verify_sources(&fixture, source_root)?;
    let diagnostics = separated_diagnostics(&fixture)?;

    let output_parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output_existed = ensure_output_is_available(output)?;
    fs::create_dir_all(output_parent)?;
    let staging = Builder::new()
        .prefix(".eqforbeginner-phase4-stage-")
        .tempdir_in(output_parent)?;
    let staged = generate_in(&fixture, &verified_sources, &diagnostics, staging.path())?;
    verify_staged(&staged)?;

    let output_still_exists = ensure_output_is_available(output)?;
    if output_still_exists != output_existed {
        return Err(invalid(format!(
            "output path `{}` changed during Phase 4 generation; refusing to commit",
            output.display()
        )));
    }
    match fs::rename(staging.path(), output) {
        Ok(()) => {}
        Err(first_error) if output_existed => {
            if !ensure_output_is_available(output)? {
                return Err(invalid(format!(
                    "output path `{}` disappeared during Phase 4 generation",
                    output.display()
                )));
            }
            fs::remove_dir(output)?;
            if let Err(commit_error) = fs::rename(staging.path(), output) {
                let restore_error = fs::create_dir(output).err();
                return Err(invalid(match restore_error {
                    Some(restore_error) => format!(
                        "could not commit Phase 4 output ({commit_error}); could not restore empty directory ({restore_error})"
                    ),
                    None => format!(
                        "could not commit Phase 4 output after empty-directory replacement failed ({first_error}; {commit_error})"
                    ),
                }));
            }
        }
        Err(error) => return Err(error.into()),
    }
    let _committed_staging_path = staging.keep();
    Ok(GeneratedPhase4Analysis {
        output_directory: output.to_path_buf(),
        project_file: output.join("project.json"),
        filter_wav: output.join("filter/EQforBeginner_48000_Phase4_Trial.wav"),
        readme: output.join("README.txt"),
        numerical_passed: staged.numerical_passed,
        verification_state: staged.verification_state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
    fn measured_fixture_stays_predicted_only_and_emits_no_zip() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if skip_without(
            &repository.join("measurments/derived/phase4-offline-measurements.json"),
            "the local Phase 4 measurement fixture",
        ) {
            return;
        }
        let directory = tempdir().expect("temporary directory");
        let output = directory.path().join("phase4");
        let generated = analyze_phase4_offline(
            &repository.join("measurments/derived/phase4-offline-measurements.json"),
            &repository.join("measurments/phase4"),
            &output,
        )
        .expect("analyze measured Phase 4 fixture");
        assert_eq!(generated.verification_state, "predicted-only-measured");
        let project: serde_json::Value = serde_json::from_slice(
            &fs::read(&generated.project_file).expect("read Phase 4 project"),
        )
        .expect("parse Phase 4 project");
        assert_eq!(project["hardware_verification"], "unverified");
        assert!(project["closed_loop_passed"].is_null());
        assert_eq!(project["export_eligible"], false);
        assert!(project["recommended_headroom_db"].is_null());
        assert_eq!(
            project["measurement_sources"].as_array().map(Vec::len),
            Some(6)
        );
        assert!(!output.join("EQforBeginner_Roon.zip").exists());
    }

    #[test]
    fn offline_fixture_rejects_post_fir_evidence_and_verified_assumptions() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dataset = repository.join("measurments/derived/phase4-offline-measurements.json");
        if skip_without(&dataset, "the local Phase 4 measurement fixture") {
            return;
        }
        let mut fixture: Phase4Fixture =
            serde_json::from_slice(&fs::read(dataset).expect("read measured fixture"))
                .expect("parse measured fixture");

        fixture
            .post_fir_measurements
            .push(json!({"id": "not-admissible"}));
        let error = validate_fixture(&fixture).expect_err("offline path must reject post-FIR data");
        assert!(error
            .to_string()
            .contains("accepts no post-FIR measurement"));

        fixture.post_fir_measurements.clear();
        fixture.assumptions.verified = true;
        let error = validate_fixture(&fixture).expect_err("declaration is not measured evidence");
        assert!(error.to_string().contains("incorrectly marked verified"));
    }
}
