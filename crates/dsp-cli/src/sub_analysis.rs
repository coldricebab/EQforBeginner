use super::{ensure_output_is_available, HarnessError, HarnessResult};
use crate::source_verification::{verify_declared_sources, SourceDeclaration, VerifiedSource};
use eqforbeginner_dsp_core::sub_integration::{
    analyze_separated_paths, rank_candidates, CandidateSettings, Channel, CombinedResponse,
    Polarity, PositionObservation, RankingConfig, SeparatedPathAnalysis, SubIntegrationCandidate,
    SubIntegrationReport, SUB_INTEGRATION_ALGORITHM_VERSION,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::f64::consts::PI;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::Builder;

// Historic on-disk format id, predates the product rename; not user-visible branding
// — do not rename. It is compared against the persisted fixture
// `measurments/derived/phase3-responses.json`.
const FIXTURE_SCHEMA: &str = "similarrew-phase3-measurements-v1";
const MODEL_GATE_MAX_RMSE_DB: f64 = 1.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedSubAnalysis {
    pub output_directory: PathBuf,
    pub ranking_json: PathBuf,
    pub ranking_csv: PathBuf,
    pub readme: PathBuf,
    pub best_candidate_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MeasurementFixture {
    schema_version: String,
    extraction: ExtractionMetadata,
    frequency_grid_hz: Vec<f64>,
    responses: BTreeMap<String, ResponseFixture>,
    candidates: Vec<CandidateFixture>,
    separated_references: Vec<SeparatedReferenceFixture>,
    repeatability_groups: Vec<RepeatabilityGroupFixture>,
    limitations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionMetadata {
    extractor_version: String,
    dependency: String,
    preferred_future_path: String,
    source_rew_build: String,
    frequency_range_hz: [f64; 2],
    level_alignment_applied: bool,
    timeline_shift_applied: bool,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalibrationFixture {
    embedded_calibration_applied: bool,
    calibration_limit_applied: bool,
    microphone_serial: u64,
    microphone_calibration_file: String,
    spl_calibration_offset_db: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityFixture {
    signal_to_noise_db: Option<f64>,
    signal_to_distortion_db: Option<f64>,
    signal_dbfs: Option<f64>,
    noise_and_distortion_dbfs: Option<f64>,
    #[serde(default)]
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
struct CandidateFixture {
    id: String,
    hardware: HardwareFixture,
    positions: Vec<PositionFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HardwareFixture {
    crossover_hz: f64,
    main_delay_ms: Option<f64>,
    sub_level_db: Option<f64>,
    polarity_inverted: Option<bool>,
    source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PositionFixture {
    id: String,
    weight: f64,
    left_response_id: String,
    right_response_id: String,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepeatabilityGroupFixture {
    id: String,
    response_ids: Vec<String>,
}

#[derive(Debug)]
struct RepeatabilityMetrics {
    id: String,
    response_ids: Vec<String>,
    raw_bin_rmse_db_40_160_hz: f64,
    smoothed_1_12_octave_rmse_db_40_160_hz: f64,
    measurement_delay_difference_ms: f64,
}

#[derive(Debug)]
struct SeparatedDiagnostic {
    id: String,
    channel: String,
    measured_state: SeparatedPathAnalysis,
    inverted_sub_counterfactual: SeparatedPathAnalysis,
    model_gate_passed: bool,
}

#[derive(Debug)]
struct ShapeDiagnostic {
    candidate_id: String,
    shape_rmse_about_median_db_40_160_hz: f64,
    span_db_40_160_hz: f64,
    left_right_rmse_db_40_160_hz: f64,
    span_db_50_140_hz: f64,
    energy_average_db_near_83_5_hz: f64,
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::Invalid(message.into())
}

fn response<'a>(
    fixture: &'a MeasurementFixture,
    response_id: &str,
) -> HarnessResult<&'a ResponseFixture> {
    fixture
        .responses
        .get(response_id)
        .ok_or_else(|| invalid(format!("fixture response `{response_id}` does not exist")))
}

fn core_response(
    frequencies_hz: &[f64],
    fixture: &ResponseFixture,
) -> HarnessResult<CombinedResponse> {
    if fixture.sample_rate_hz != 48_000 {
        return Err(invalid(format!(
            "measurement `{}` has sample rate {}, expected 48000",
            fixture.measurement_title, fixture.sample_rate_hz
        )));
    }
    if fixture.magnitude_db_spl.len() != frequencies_hz.len()
        || fixture.unwrapped_phase_degrees.len() != frequencies_hz.len()
    {
        return Err(invalid(format!(
            "measurement `{}` response arrays do not match the common grid",
            fixture.measurement_title
        )));
    }
    Ok(CombinedResponse {
        frequencies_hz: frequencies_hz.to_vec(),
        magnitude_db: fixture.magnitude_db_spl.clone(),
        phase_rad: Some(
            fixture
                .unwrapped_phase_degrees
                .iter()
                .map(|phase| phase.to_radians())
                .collect(),
        ),
        // The crossover candidates were not repeated at the same hardware
        // setting. Arrival metadata is retained in the fixture, but is not
        // promoted to repeatability evidence or awarded a zero penalty.
        timing: None,
    })
}

fn core_candidates(fixture: &MeasurementFixture) -> HarnessResult<Vec<SubIntegrationCandidate>> {
    let mut result = Vec::with_capacity(fixture.candidates.len());
    for candidate in &fixture.candidates {
        let mut positions = Vec::with_capacity(candidate.positions.len());
        for position in &candidate.positions {
            positions.push(PositionObservation {
                id: position.id.clone(),
                weight: position.weight,
                left_combined: core_response(
                    &fixture.frequency_grid_hz,
                    response(fixture, &position.left_response_id)?,
                )?,
                right_combined: core_response(
                    &fixture.frequency_grid_hz,
                    response(fixture, &position.right_response_id)?,
                )?,
            });
        }
        result.push(SubIntegrationCandidate {
            id: candidate.id.clone(),
            settings: CandidateSettings {
                crossover_hz: candidate.hardware.crossover_hz,
                main_delay_ms: candidate.hardware.main_delay_ms,
                sub_level_db: candidate.hardware.sub_level_db,
                polarity: candidate.hardware.polarity_inverted.map(|inverted| {
                    if inverted {
                        Polarity::Inverted
                    } else {
                        Polarity::Normal
                    }
                }),
            },
            positions,
        });
    }
    Ok(result)
}

fn verify_sources(
    fixture: &MeasurementFixture,
    source_root: &Path,
) -> HarnessResult<Vec<VerifiedSource>> {
    verify_declared_sources(
        source_root,
        fixture
            .responses
            .iter()
            .map(|(response_id, response)| SourceDeclaration {
                id: response_id,
                relative_path: &response.source_path,
                sha256: &response.source_sha256,
                bytes: response.source_bytes,
            }),
    )
}

fn band_indices(frequencies_hz: &[f64], low_hz: f64, high_hz: f64) -> Vec<usize> {
    frequencies_hz
        .iter()
        .enumerate()
        .filter_map(|(index, frequency)| {
            (*frequency >= low_hz && *frequency <= high_hz).then_some(index)
        })
        .collect()
}

fn rms(values: impl Iterator<Item = f64>) -> HarnessResult<f64> {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return Err(invalid("RMSE input must be finite and nonempty"));
    }
    Ok((values.iter().map(|value| value * value).sum::<f64>() / values.len() as f64).sqrt())
}

fn gaussian_log_smooth(
    frequencies_hz: &[f64],
    values: &[f64],
    fwhm_octaves: f64,
) -> HarnessResult<Vec<f64>> {
    if frequencies_hz.len() != values.len()
        || frequencies_hz.is_empty()
        || !fwhm_octaves.is_finite()
        || fwhm_octaves <= 0.0
    {
        return Err(invalid("invalid log-smoothing input"));
    }
    let sigma = fwhm_octaves / (2.0 * (2.0 * std::f64::consts::LN_2).sqrt());
    let radius_ratio = 2.0_f64.powf(3.0 * sigma);
    let mut result = Vec::with_capacity(values.len());
    for (target_index, target_hz) in frequencies_hz.iter().copied().enumerate() {
        if !target_hz.is_finite() || target_hz <= 0.0 {
            return Err(invalid("log-smoothing frequencies must be positive"));
        }
        let first =
            frequencies_hz.partition_point(|frequency| *frequency < target_hz / radius_ratio);
        let end =
            frequencies_hz.partition_point(|frequency| *frequency <= target_hz * radius_ratio);
        let mut sum = 0.0;
        let mut weight_sum = 0.0;
        for index in first..end {
            let distance = (frequencies_hz[index] / target_hz).log2();
            let weight = (-0.5 * (distance / sigma).powi(2)).exp();
            sum += weight * values[index];
            weight_sum += weight;
        }
        let value = sum / weight_sum;
        if !value.is_finite() {
            return Err(invalid(format!(
                "log smoothing produced a non-finite value at {target_index}"
            )));
        }
        result.push(value);
    }
    Ok(result)
}

fn repeatability_metrics(fixture: &MeasurementFixture) -> HarnessResult<Vec<RepeatabilityMetrics>> {
    let indices = band_indices(&fixture.frequency_grid_hz, 40.0, 160.0);
    if indices.is_empty() {
        return Err(invalid("fixture lacks 40-160 Hz repeatability bins"));
    }
    let mut result = Vec::with_capacity(fixture.repeatability_groups.len());
    for group in &fixture.repeatability_groups {
        if group.response_ids.len() != 2 {
            return Err(invalid(format!(
                "repeatability group `{}` must contain exactly two responses",
                group.id
            )));
        }
        let first = response(fixture, &group.response_ids[0])?;
        let second = response(fixture, &group.response_ids[1])?;
        let first_smoothed = gaussian_log_smooth(
            &fixture.frequency_grid_hz,
            &first.magnitude_db_spl,
            1.0 / 12.0,
        )?;
        let second_smoothed = gaussian_log_smooth(
            &fixture.frequency_grid_hz,
            &second.magnitude_db_spl,
            1.0 / 12.0,
        )?;
        result.push(RepeatabilityMetrics {
            id: group.id.clone(),
            response_ids: group.response_ids.clone(),
            raw_bin_rmse_db_40_160_hz: rms(indices
                .iter()
                .map(|index| first.magnitude_db_spl[*index] - second.magnitude_db_spl[*index]))?,
            smoothed_1_12_octave_rmse_db_40_160_hz: rms(indices
                .iter()
                .map(|index| first_smoothed[*index] - second_smoothed[*index]))?,
            measurement_delay_difference_ms: (first.timing.measurement_delay_ms
                - second.timing.measurement_delay_ms)
                .abs(),
        });
    }
    Ok(result)
}

fn separated_diagnostics(fixture: &MeasurementFixture) -> HarnessResult<Vec<SeparatedDiagnostic>> {
    let mut result = Vec::with_capacity(fixture.separated_references.len());
    for reference in &fixture.separated_references {
        let main = core_response(
            &fixture.frequency_grid_hz,
            response(fixture, &reference.main_response_id)?,
        )?;
        let sub = core_response(
            &fixture.frequency_grid_hz,
            response(fixture, &reference.sub_response_id)?,
        )?;
        let combined = core_response(
            &fixture.frequency_grid_hz,
            response(fixture, &reference.combined_response_id)?,
        )?;
        let measured_state =
            analyze_separated_paths(&main, &sub, &combined, reference.crossover_hz)?;
        let mut inverted_sub = sub;
        for phase in inverted_sub
            .phase_rad
            .as_mut()
            .expect("fixture conversion always supplies phase")
        {
            *phase += PI;
        }
        let inverted_sub_counterfactual =
            analyze_separated_paths(&main, &inverted_sub, &combined, reference.crossover_hz)?;
        result.push(SeparatedDiagnostic {
            id: reference.id.clone(),
            channel: reference.channel.clone(),
            model_gate_passed: measured_state.complex_sum_magnitude_rmse_db
                <= MODEL_GATE_MAX_RMSE_DB,
            measured_state,
            inverted_sub_counterfactual,
        });
    }
    Ok(result)
}

fn energy_average_db(left_db: f64, right_db: f64) -> f64 {
    10.0 * ((10.0_f64.powf(left_db / 10.0) + 10.0_f64.powf(right_db / 10.0)) / 2.0).log10()
}

fn median(values: &[f64]) -> HarnessResult<f64> {
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return Err(invalid("median input must be finite and nonempty"));
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Ok(if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    })
}

fn shape_diagnostics(fixture: &MeasurementFixture) -> HarnessResult<Vec<ShapeDiagnostic>> {
    let indices_40_160 = band_indices(&fixture.frequency_grid_hz, 40.0, 160.0);
    let indices_50_140 = band_indices(&fixture.frequency_grid_hz, 50.0, 140.0);
    let near_83_5 = fixture
        .frequency_grid_hz
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| (*left - 83.5).abs().total_cmp(&(*right - 83.5).abs()))
        .map(|(index, _)| index)
        .ok_or_else(|| invalid("empty frequency grid"))?;
    let mut result = Vec::with_capacity(fixture.candidates.len());
    for candidate in &fixture.candidates {
        if candidate.positions.len() != 1 {
            return Err(invalid(
                "human-readable P0 shape diagnostics require one position per candidate",
            ));
        }
        let position = &candidate.positions[0];
        let left = response(fixture, &position.left_response_id)?;
        let right = response(fixture, &position.right_response_id)?;
        let left_smoothed = gaussian_log_smooth(
            &fixture.frequency_grid_hz,
            &left.magnitude_db_spl,
            1.0 / 12.0,
        )?;
        let right_smoothed = gaussian_log_smooth(
            &fixture.frequency_grid_hz,
            &right.magnitude_db_spl,
            1.0 / 12.0,
        )?;
        let energy = |index: usize| energy_average_db(left_smoothed[index], right_smoothed[index]);
        let band_energy = indices_40_160
            .iter()
            .map(|index| energy(*index))
            .collect::<Vec<_>>();
        let reference = median(&band_energy)?;
        let minimum = band_energy.iter().copied().fold(f64::INFINITY, f64::min);
        let maximum = band_energy
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        let band_50_140 = indices_50_140
            .iter()
            .map(|index| energy(*index))
            .collect::<Vec<_>>();
        result.push(ShapeDiagnostic {
            candidate_id: candidate.id.clone(),
            shape_rmse_about_median_db_40_160_hz: rms(band_energy
                .iter()
                .map(|value| value - reference))?,
            span_db_40_160_hz: maximum - minimum,
            left_right_rmse_db_40_160_hz: rms(indices_40_160
                .iter()
                .map(|index| left_smoothed[*index] - right_smoothed[*index]))?,
            span_db_50_140_hz: band_50_140
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max)
                - band_50_140.iter().copied().fold(f64::INFINITY, f64::min),
            energy_average_db_near_83_5_hz: energy(near_83_5),
        });
    }
    Ok(result)
}

fn separated_json(analysis: &SeparatedPathAnalysis) -> serde_json::Value {
    json!({
        "band": {
            "lower_hz": analysis.band.lower_hz,
            "upper_hz": analysis.band.upper_hz,
            "bin_count": analysis.band.bin_count,
        },
        "complex_sum_magnitude_rmse_db": analysis.complex_sum_magnitude_rmse_db,
        "cancellation_loss_rms_db": analysis.cancellation_loss_rms_db,
        "cancellation_loss_p95_db": analysis.cancellation_loss_p95_db,
        "cancellation_loss_worst_db": analysis.cancellation_loss_worst_db,
    })
}

fn report_json(
    fixture: &MeasurementFixture,
    config: &RankingConfig,
    report: &SubIntegrationReport,
    sources: &[VerifiedSource],
    repeats: &[RepeatabilityMetrics],
    separated: &[SeparatedDiagnostic],
    shapes: &[ShapeDiagnostic],
) -> serde_json::Value {
    let source_quality_warnings = fixture
        .responses
        .iter()
        .flat_map(|(response_id, response)| {
            response
                .quality
                .known_warnings
                .iter()
                .map(move |warning| json!({"response_id": response_id, "warning": warning}))
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": "eqforbeginner-phase3-ranking-v1",
        "algorithm_version": SUB_INTEGRATION_ALGORITHM_VERSION,
        "algorithm_settings": {
            "comparison_band_rule": "0.5 * minimum candidate crossover through 2.0 * maximum candidate crossover",
            "magnitude_smoothing_fwhm_octaves": config.magnitude_smoothing_octaves,
            "group_delay_smoothing_fwhm_octaves": config.group_delay_smoothing_octaves,
            "deficit_p95_weight": config.deficit_p95_weight,
            "deficit_worst_weight": config.deficit_worst_weight,
            "worst_seat_weight": config.worst_seat_weight,
            "spatial_spread_weight": config.spatial_spread_weight,
            "phase_weight_db_per_ms": config.phase_weight_db_per_ms,
            "timing_weight_db_per_ms": config.timing_weight_db_per_ms,
            "delay_regularization_db_per_ms": config.delay_regularization_db_per_ms,
            "level_regularization_db_per_db": config.level_regularization_db_per_db,
            "anchor_warning_threshold_db": config.anchor_warning_threshold_db,
        },
        "verification_state": "provisional-measured-candidate-ranking",
        "hardware_changed_by_app": false,
        "needs_confirmation": report.needs_confirmation,
        "confidence": if report.spatial_evidence_available { "measured-multiposition" } else { "limited-single-position" },
        "proof_scope": "Ranks only the measured 70/80/90 Hz crossover candidates; delay, polarity and sub level were not varied or optimized.",
        "best_candidate_id": report.rankings.first().map(|candidate| candidate.id.as_str()),
        "scoring_band": {
            "lower_hz": report.band.lower_hz,
            "upper_hz": report.band.upper_hz,
            "bin_count": report.band.bin_count,
        },
        "evidence": {
            "spatial_available": report.spatial_evidence_available,
            "phase_available": report.phase_evidence_available,
            "same-setting_timing_repeatability_available": report.timing_evidence_available,
            "anchor_level_spread_db": report.anchor_level_spread_db,
            "automatic_level_alignment_applied": fixture.extraction.level_alignment_applied,
            "timeline_shift_applied": fixture.extraction.timeline_shift_applied,
        },
        "rankings": report.rankings.iter().map(|candidate| json!({
            "rank": candidate.rank,
            "id": candidate.id,
            "settings": {
                "crossover_hz": candidate.settings.crossover_hz,
                "main_delay_ms": candidate.settings.main_delay_ms,
                "sub_level_db": candidate.settings.sub_level_db,
                "polarity": candidate.settings.polarity.map(|polarity| match polarity {
                    Polarity::Normal => "normal",
                    Polarity::Inverted => "inverted",
                }),
            },
            "metrics": {
                "deficit_rms_db": candidate.metrics.deficit_rms_db,
                "deficit_p95_db": candidate.metrics.deficit_p95_db,
                "deficit_worst_db": candidate.metrics.deficit_worst_db,
                "worst_seat_rms_db": candidate.metrics.worst_seat_rms_db,
                "spatial_spread_rms_db": candidate.metrics.spatial_spread_rms_db,
                "phase_irregularity_rms_ms": candidate.metrics.phase_irregularity_rms_ms,
                "timing_repeatability_rms_ms": candidate.metrics.timing_repeatability_rms_ms,
                "delay_regularization_db": candidate.metrics.delay_regularization_db,
                "level_regularization_db": candidate.metrics.level_regularization_db,
                "total_score": candidate.metrics.total_score,
            },
            "observations": candidate.observations.iter().map(|observation| json!({
                "position_id": observation.position_id,
                "channel": match observation.channel { Channel::Left => "left", Channel::Right => "right" },
                "rms_db": observation.rms_db,
                "p95_db": observation.p95_db,
                "worst_db": observation.worst_db,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "declared_candidate_hardware": fixture.candidates.iter().map(|candidate| json!({
            "id": candidate.id,
            "source": candidate.hardware.source,
            "crossover_hz": candidate.hardware.crossover_hz,
            "main_delay_ms": candidate.hardware.main_delay_ms,
            "sub_level_db": candidate.hardware.sub_level_db,
            "polarity_inverted": candidate.hardware.polarity_inverted,
        })).collect::<Vec<_>>(),
        "shape_diagnostics_not_used_as_score": shapes.iter().map(|shape| json!({
            "candidate_id": shape.candidate_id,
            "smoothing": "1/12-octave Gaussian on log-frequency axis",
            "shape_rmse_about_band_median_db_40_160_hz": shape.shape_rmse_about_median_db_40_160_hz,
            "span_db_40_160_hz": shape.span_db_40_160_hz,
            "left_right_rmse_db_40_160_hz": shape.left_right_rmse_db_40_160_hz,
            "span_db_50_140_hz": shape.span_db_50_140_hz,
            "energy_average_db_near_83_5_hz": shape.energy_average_db_near_83_5_hz,
        })).collect::<Vec<_>>(),
        "separated_path_diagnostics_not_used_as_rank_score": separated.iter().map(|diagnostic| json!({
            "id": diagnostic.id,
            "channel": diagnostic.channel,
            "model_gate_max_rmse_db": MODEL_GATE_MAX_RMSE_DB,
            "model_gate_passed": diagnostic.model_gate_passed,
            "measured_state": separated_json(&diagnostic.measured_state),
            "inverted_sub_counterfactual": {
                "complex_sum_magnitude_rmse_db": diagnostic.inverted_sub_counterfactual.complex_sum_magnitude_rmse_db,
                "note": "Only the complex-sum prediction changes; cancellation-loss metrics require an actually measured inverted combined response."
            },
        })).collect::<Vec<_>>(),
        "repeatability": repeats.iter().map(|repeat| json!({
            "id": repeat.id,
            "response_ids": repeat.response_ids,
            "raw_bin_rmse_db_40_160_hz": repeat.raw_bin_rmse_db_40_160_hz,
            "smoothed_1_12_octave_rmse_db_40_160_hz": repeat.smoothed_1_12_octave_rmse_db_40_160_hz,
            "measurement_delay_difference_ms": repeat.measurement_delay_difference_ms,
        })).collect::<Vec<_>>(),
        "source_verification": {
            "all_hashes_verified": true,
            "source_root_file_count": sources.len(),
            "files": sources.iter().map(|source| json!({
                "response_id": source.id,
                "path": source.relative_path,
                "sha256": source.sha256,
                "bytes": source.bytes,
            })).collect::<Vec<_>>(),
            "rew_build": fixture.extraction.source_rew_build,
            "extractor_version": fixture.extraction.extractor_version,
            "development_dependency": fixture.extraction.dependency,
            "preferred_future_path": fixture.extraction.preferred_future_path,
        },
        "measurement_metadata_summary": {
            "rew_versions": fixture.responses.values().map(|response| response.rew_version.as_str()).collect::<BTreeSet<_>>(),
            "sample_rates_hz": fixture.responses.values().map(|response| response.sample_rate_hz).collect::<BTreeSet<_>>(),
            "calibration_files": fixture.responses.values().map(|response| response.calibration.microphone_calibration_file.as_str()).collect::<BTreeSet<_>>(),
            "microphone_serials": fixture.responses.values().map(|response| response.calibration.microphone_serial).collect::<BTreeSet<_>>(),
            "all_embedded_calibration_applied": fixture.responses.values().all(|response| response.calibration.embedded_calibration_applied),
            "all_acoustic_timing_references_used": fixture.responses.values().all(|response| response.timing.used_acoustic_reference),
        },
        "measurement_metadata": fixture.responses.iter().map(|(response_id, response)| json!({
            "response_id": response_id,
            "title": response.measurement_title,
            "notes": response.measurement_notes,
            "rew_version": response.rew_version,
            "sample_rate_hz": response.sample_rate_hz,
            "calibration": {
                "embedded_applied": response.calibration.embedded_calibration_applied,
                "limit_applied": response.calibration.calibration_limit_applied,
                "microphone_serial": response.calibration.microphone_serial,
                "file": response.calibration.microphone_calibration_file,
                "spl_offset_db": response.calibration.spl_calibration_offset_db,
            },
            "quality": {
                "signal_to_noise_db": response.quality.signal_to_noise_db,
                "signal_to_distortion_db": response.quality.signal_to_distortion_db,
                "signal_dbfs": response.quality.signal_dbfs,
                "noise_and_distortion_dbfs": response.quality.noise_and_distortion_dbfs,
                "known_warnings": response.quality.known_warnings,
            },
            "timing": {
                "used_acoustic_reference": response.timing.used_acoustic_reference,
                "reference_stimulus": response.timing.reference_stimulus,
                "measurement_delay_ms": response.timing.measurement_delay_ms,
                "timing_offset_ms": response.timing.timing_offset_ms,
                "cumulative_start_time_offset_ms": response.timing.cumulative_start_time_offset_ms,
                "clock_adjustment_ppm": response.timing.clock_adjustment_ppm,
                "original_peak_time_ms": response.timing.original_peak_time_ms,
                "timeline_policy": response.timing.timeline_policy,
            },
        })).collect::<Vec<_>>(),
        "warnings": report.warnings,
        "source_quality_warnings": source_quality_warnings,
        "limitations": fixture.limitations,
    })
}

fn write_ranking_csv(path: &Path, report: &SubIntegrationReport) -> HarnessResult<()> {
    let mut csv = String::from(
        "rank,candidate_id,crossover_hz,total_score,deficit_rms_db,deficit_p95_db,deficit_worst_db,phase_irregularity_rms_ms,anchor_level_spread_db,needs_confirmation\n",
    );
    for candidate in &report.rankings {
        writeln!(
            csv,
            "{},{},{:.6},{:.9},{:.9},{:.9},{:.9},{},{},{}",
            candidate.rank,
            candidate.id,
            candidate.settings.crossover_hz,
            candidate.metrics.total_score,
            candidate.metrics.deficit_rms_db,
            candidate.metrics.deficit_p95_db,
            candidate.metrics.deficit_worst_db,
            candidate
                .metrics
                .phase_irregularity_rms_ms
                .map(|value| format!("{value:.9}"))
                .unwrap_or_default(),
            report
                .anchor_level_spread_db
                .map(|value| format!("{value:.9}"))
                .unwrap_or_default(),
            report.needs_confirmation,
        )
        .map_err(|error| invalid(error.to_string()))?;
    }
    fs::write(path, csv)?;
    Ok(())
}

fn write_response_csv(path: &Path, fixture: &MeasurementFixture) -> HarnessResult<()> {
    let mut file = fs::File::create(path)?;
    writeln!(
        file,
        "frequency_hz,candidate_id,left_db_spl,right_db_spl,lr_energy_average_db_spl"
    )?;
    for candidate in &fixture.candidates {
        for position in &candidate.positions {
            let left = response(fixture, &position.left_response_id)?;
            let right = response(fixture, &position.right_response_id)?;
            for (index, frequency) in fixture.frequency_grid_hz.iter().enumerate() {
                writeln!(
                    file,
                    "{frequency:.9},{},{:.9},{:.9},{:.9}",
                    candidate.id,
                    left.magnitude_db_spl[index],
                    right.magnitude_db_spl[index],
                    energy_average_db(left.magnitude_db_spl[index], right.magnitude_db_spl[index]),
                )?;
            }
        }
    }
    Ok(())
}

fn write_readme(
    path: &Path,
    report: &SubIntegrationReport,
    separated: &[SeparatedDiagnostic],
) -> HarnessResult<()> {
    let best = report
        .rankings
        .first()
        .ok_or_else(|| invalid("ranking report is empty"))?;
    let mut text = format!(
        "EQforBeginner Phase 3 measured candidate ranking\n\
         =============================================\n\n\
         Best measured candidate: {}\n\
         Algorithm: {}\n\
         Scoring band: {:.2}-{:.2} Hz\n\
         Total score (lower is better): {:.4}\n\
         Confirmation measurement required: YES\n\n\
         Scope: This ranks only the supplied 70/80/90 Hz crossover measurements.\n\
         The app did not change hardware. Polarity, sub level and delay were not\n\
         varied by this dataset, and there is only one listening position.\n\n\
         Separated XO80 model checks (not included in candidate score):\n",
        best.id,
        SUB_INTEGRATION_ALGORITHM_VERSION,
        report.band.lower_hz,
        report.band.upper_hz,
        best.metrics.total_score,
    );
    for diagnostic in separated {
        writeln!(
            text,
            "- {}: measured-state complex-sum RMSE {:.3} dB; inverted-sub counterfactual {:.3} dB; model gate {}",
            diagnostic.id,
            diagnostic.measured_state.complex_sum_magnitude_rmse_db,
            diagnostic
                .inverted_sub_counterfactual
                .complex_sum_magnitude_rmse_db,
            if diagnostic.model_gate_passed { "PASS" } else { "FAIL" },
        )
        .map_err(|error| invalid(error.to_string()))?;
    }
    text.push_str("\nWarnings:\n");
    for warning in &report.warnings {
        writeln!(text, "- {warning}").map_err(|error| invalid(error.to_string()))?;
    }
    fs::write(path, text)?;
    Ok(())
}

fn analyze_in(
    dataset: &Path,
    source_root: &Path,
    output: &Path,
) -> HarnessResult<GeneratedSubAnalysis> {
    let fixture: MeasurementFixture = serde_json::from_slice(&fs::read(dataset)?)?;
    if fixture.schema_version != FIXTURE_SCHEMA {
        return Err(invalid(format!(
            "unsupported Phase 3 fixture schema `{}`",
            fixture.schema_version
        )));
    }
    if fixture.extraction.level_alignment_applied || fixture.extraction.timeline_shift_applied {
        return Err(invalid(
            "Phase 3 fixture must preserve measured level and the original timeline",
        ));
    }
    if fixture.extraction.frequency_range_hz != [20.0, 500.0] {
        return Err(invalid("unexpected fixture frequency range"));
    }
    let sources = verify_sources(&fixture, source_root)?;
    let candidates = core_candidates(&fixture)?;
    let config = RankingConfig::default();
    let report = rank_candidates(&candidates, &config)?;
    let repeats = repeatability_metrics(&fixture)?;
    let separated = separated_diagnostics(&fixture)?;
    let shapes = shape_diagnostics(&fixture)?;
    if separated
        .iter()
        .any(|diagnostic| !diagnostic.model_gate_passed)
    {
        return Err(invalid(
            "separated main/sub model check failed; refusing to report the measured ranking",
        ));
    }

    fs::create_dir_all(output)?;
    let ranking_json = output.join("ranking.json");
    fs::write(
        &ranking_json,
        serde_json::to_vec_pretty(&report_json(
            &fixture, &config, &report, &sources, &repeats, &separated, &shapes,
        ))?,
    )?;
    let ranking_csv = output.join("ranking.csv");
    write_ranking_csv(&ranking_csv, &report)?;
    write_response_csv(&output.join("response-comparison.csv"), &fixture)?;
    let readme = output.join("README.txt");
    write_readme(&readme, &report, &separated)?;
    let best_candidate_id = report
        .rankings
        .first()
        .ok_or_else(|| invalid("ranking report is empty"))?
        .id
        .clone();
    Ok(GeneratedSubAnalysis {
        output_directory: output.to_path_buf(),
        ranking_json,
        ranking_csv,
        readme,
        best_candidate_id,
    })
}

fn verify_staged(result: &GeneratedSubAnalysis) -> HarnessResult<()> {
    for path in [
        &result.ranking_json,
        &result.ranking_csv,
        &result.readme,
        &result.output_directory.join("response-comparison.csv"),
    ] {
        if !path.is_file() {
            return Err(invalid(format!(
                "staged Phase 3 artifact `{}` is missing",
                path.display()
            )));
        }
    }
    let report: serde_json::Value = serde_json::from_slice(&fs::read(&result.ranking_json)?)?;
    if report["verification_state"] != "provisional-measured-candidate-ranking"
        || report["needs_confirmation"] != true
        || report["best_candidate_id"] != result.best_candidate_id
        || report["source_verification"]["all_hashes_verified"] != true
    {
        return Err(invalid(
            "staged Phase 3 report lost its provisional or source-verification state",
        ));
    }
    Ok(())
}

/// Analyze a versioned, extracted measurement fixture without overwriting an
/// existing nonempty directory. Original MDAT hashes are checked before scoring.
pub fn analyze_sub_dataset(
    dataset: &Path,
    source_root: &Path,
    output: &Path,
) -> HarnessResult<GeneratedSubAnalysis> {
    if output.file_name().is_none() {
        return Err(invalid("output path must name a directory"));
    }
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output_existed = ensure_output_is_available(output)?;
    fs::create_dir_all(parent)?;
    let staging = Builder::new()
        .prefix(".eqforbeginner-phase3-stage-")
        .tempdir_in(parent)?;
    let staged = analyze_in(dataset, source_root, staging.path())?;
    verify_staged(&staged)?;
    if ensure_output_is_available(output)? != output_existed {
        return Err(invalid("Phase 3 output path changed during generation"));
    }
    match fs::rename(staging.path(), output) {
        Ok(()) => {}
        Err(first_error) if output_existed => {
            if !ensure_output_is_available(output)? {
                return Err(invalid("Phase 3 output path disappeared during commit"));
            }
            fs::remove_dir(output)?;
            if let Err(commit_error) = fs::rename(staging.path(), output) {
                let restore_error = fs::create_dir(output).err();
                return Err(invalid(match restore_error {
                    Some(restore_error) => format!(
                        "could not commit Phase 3 output ({commit_error}); could not restore empty directory ({restore_error})"
                    ),
                    None => format!(
                        "could not commit Phase 3 output after replacing an empty directory ({first_error}; {commit_error})"
                    ),
                }));
            }
        }
        Err(error) => return Err(error.into()),
    }
    let _committed_staging = staging.keep();
    Ok(GeneratedSubAnalysis {
        output_directory: output.to_path_buf(),
        ranking_json: output.join("ranking.json"),
        ranking_csv: output.join("ranking.csv"),
        readme: output.join("README.txt"),
        best_candidate_id: staged.best_candidate_id,
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
    fn measured_fixture_is_hash_verified_and_ranks_xo90_provisionally() {
        let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_directory.join("../..");
        let dataset = workspace.join("measurments/derived/phase3-responses.json");
        let source_root = workspace.join("measurments");
        if skip_without(&dataset, "the local Phase 3 measurement fixture") {
            return;
        }
        let temp = tempdir().expect("temporary directory");
        let output = temp.path().join("phase3");

        let result = analyze_sub_dataset(&dataset, &source_root, &output)
            .expect("measured fixture should analyze");

        assert_eq!(result.best_candidate_id, "xo-90-main-delay-0.83ms");
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(result.ranking_json).expect("read report"))
                .expect("parse report");
        assert_eq!(report["needs_confirmation"], true);
        assert_eq!(report["confidence"], "limited-single-position");
        assert_eq!(
            report["algorithm_settings"]["magnitude_smoothing_fwhm_octaves"],
            1.0 / 3.0
        );
        assert_eq!(
            report["rankings"]
                .as_array()
                .expect("rankings")
                .iter()
                .map(|candidate| candidate["settings"]["crossover_hz"]
                    .as_f64()
                    .expect("crossover"))
                .collect::<Vec<_>>(),
            vec![90.0, 80.0, 70.0]
        );
        assert_eq!(report["source_verification"]["source_root_file_count"], 16);
        assert!(report["repeatability"]
            .as_array()
            .expect("repeatability")
            .iter()
            .all(|item| item["smoothed_1_12_octave_rmse_db_40_160_hz"]
                .as_f64()
                .expect("smoothed RMSE")
                < item["raw_bin_rmse_db_40_160_hz"]
                    .as_f64()
                    .expect("raw RMSE")));
        assert_eq!(
            report["source_quality_warnings"]
                .as_array()
                .expect("source quality warnings")
                .len(),
            2
        );
        assert!(report["separated_path_diagnostics_not_used_as_rank_score"]
            .as_array()
            .expect("separated diagnostics")
            .iter()
            .all(|item| item["model_gate_passed"] == true));
    }
}
