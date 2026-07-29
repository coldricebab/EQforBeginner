//! Deterministic scoring for manual single-subwoofer integration trials.
//!
//! The scorer compares measurements exactly as captured. It deliberately does
//! not level-align candidates: a hardware level change is part of the trial and
//! must remain visible. Lower scores are better, but every result still requires
//! a confirmation measurement after the user applies the selected setting.

use crate::{DspError, DspResult};
use std::collections::HashSet;
use std::f64::consts::{LN_2, PI};

const MIN_CROSSOVER_HZ: f64 = 10.0;
const MAX_CROSSOVER_HZ: f64 = 1_000.0;
const MAX_ABS_MAGNITUDE_DB: f64 = 1_000.0;
const MAX_ABS_DELAY_MS: f64 = 1_000.0;
const MIN_SUB_LEVEL_DB: f64 = -120.0;
const MAX_SUB_LEVEL_DB: f64 = 60.0;
const MAX_ABS_ARRIVAL_MS: f64 = 60_000.0;
const MAX_REPEATABILITY_MS: f64 = 1_000.0;
const MIN_SCORING_BINS: usize = 3;
const GAUSSIAN_RADIUS_SIGMA: f64 = 3.0;

/// Persisted with Phase 3 ranking reports so scoring changes remain auditable.
pub const SUB_INTEGRATION_ALGORITHM_VERSION: &str = "phase3-single-sub-ranking-v2";
/// Version for synthesizing delay/polarity candidates from measured isolated
/// paths at each physically configured crossover.
pub const SEPARATED_PATH_OPTIMIZATION_VERSION: &str =
    "phase3-separated-path-delay-polarity-search-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    Normal,
    Inverted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateSettings {
    pub crossover_hz: f64,
    pub main_delay_ms: Option<f64>,
    pub sub_level_db: Option<f64>,
    pub polarity: Option<Polarity>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimingEvidence {
    /// Arrival time on the preserved measurement timeline.
    pub arrival_time_ms: f64,
    /// RMS spread from a repeat at the same microphone position.
    pub repeatability_rms_ms: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CombinedResponse {
    pub frequencies_hz: Vec<f64>,
    pub magnitude_db: Vec<f64>,
    pub phase_rad: Option<Vec<f64>>,
    pub timing: Option<TimingEvidence>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PositionObservation {
    pub id: String,
    pub weight: f64,
    pub left_combined: CombinedResponse,
    pub right_combined: CombinedResponse,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubIntegrationCandidate {
    pub id: String,
    pub settings: CandidateSettings,
    pub positions: Vec<PositionObservation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankingConfig {
    /// Full width at half maximum on a log2 frequency axis.
    pub magnitude_smoothing_octaves: f64,
    pub group_delay_smoothing_octaves: f64,
    pub deficit_p95_weight: f64,
    pub deficit_worst_weight: f64,
    pub worst_seat_weight: f64,
    pub spatial_spread_weight: f64,
    pub phase_weight_db_per_ms: f64,
    pub timing_weight_db_per_ms: f64,
    pub delay_regularization_db_per_ms: f64,
    pub level_regularization_db_per_db: f64,
    pub anchor_warning_threshold_db: f64,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            magnitude_smoothing_octaves: 1.0 / 3.0,
            group_delay_smoothing_octaves: 0.5,
            deficit_p95_weight: 0.20,
            deficit_worst_weight: 0.05,
            worst_seat_weight: 0.25,
            spatial_spread_weight: 0.15,
            phase_weight_db_per_ms: 0.10,
            timing_weight_db_per_ms: 0.10,
            delay_regularization_db_per_ms: 0.02,
            level_regularization_db_per_db: 0.02,
            anchor_warning_threshold_db: 1.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservationDeficitMetrics {
    pub position_id: String,
    pub channel: Channel,
    pub rms_db: f64,
    pub p95_db: f64,
    pub worst_db: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateMetrics {
    pub deficit_rms_db: f64,
    pub deficit_p95_db: f64,
    pub deficit_worst_db: f64,
    pub worst_seat_rms_db: Option<f64>,
    pub spatial_spread_rms_db: Option<f64>,
    pub phase_irregularity_rms_ms: Option<f64>,
    pub timing_repeatability_rms_ms: Option<f64>,
    pub delay_regularization_db: f64,
    pub level_regularization_db: f64,
    pub total_score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedCandidate {
    pub rank: usize,
    pub id: String,
    pub settings: CandidateSettings,
    pub metrics: CandidateMetrics,
    pub observations: Vec<ObservationDeficitMetrics>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoringBand {
    pub lower_hz: f64,
    pub upper_hz: f64,
    pub bin_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubIntegrationReport {
    pub band: ScoringBand,
    pub rankings: Vec<RankedCandidate>,
    pub spatial_evidence_available: bool,
    pub phase_evidence_available: bool,
    pub timing_evidence_available: bool,
    pub anchor_level_spread_db: Option<f64>,
    pub needs_confirmation: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeparatedPathAnalysis {
    pub band: ScoringBand,
    /// RMSE between the measured combined magnitude and the complex sum of the
    /// separately measured main and sub responses.
    pub complex_sum_magnitude_rmse_db: f64,
    /// A cancellation loss exists only where the combined response falls below
    /// the louder separated path. Constructive gain is therefore not punished.
    pub cancellation_loss_rms_db: f64,
    pub cancellation_loss_p95_db: f64,
    pub cancellation_loss_worst_db: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeparatedCrossoverPaths {
    pub id: String,
    pub crossover_hz: f64,
    pub left_main: CombinedResponse,
    pub right_main: CombinedResponse,
    /// One mono-sub response measured from the same input/gain path. It is
    /// reused for L and R candidate sums; the caller must verify symmetric
    /// bass-management routing and retain this limitation in the report.
    pub sub: CombinedResponse,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeparatedPathOptimizationConfig {
    /// Main delay and sub polarity physically active during every isolated
    /// measurement. Candidate phase transforms are relative to this state.
    pub measured_main_delay_ms: f64,
    pub measured_polarity: Polarity,
    pub delay_minimum_ms: f64,
    pub delay_maximum_ms: f64,
    pub delay_step_ms: f64,
    pub ranking: RankingConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeparatedPathOptimizationReport {
    pub algorithm_version: &'static str,
    pub synthesized_candidate_count: usize,
    pub measured_main_delay_ms: f64,
    pub measured_polarity: Polarity,
    /// Marker-timeline relative sub-minus-main arrival estimate per measured
    /// crossover, and the delay window the search actually used around it.
    pub arrival_estimates: Vec<CrossoverArrivalEstimate>,
    /// Advisory only (2026-07-29 expert review, finding 11): with the winning
    /// crossover/delay/polarity held fixed, how the deficit score would move
    /// if the sub level changed by up to +/-6 dB. The main recommendation
    /// never depends on it and the confirmation measurement still gates
    /// everything.
    pub sub_level_advisory: Option<SubLevelAdvisory>,
    pub ranking: SubIntegrationReport,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubLevelAdvisory {
    pub best_gain_db: f64,
    pub deficit_rms_at_best_db: f64,
    pub deficit_rms_at_zero_db: f64,
}

/// Where the sub actually arrives relative to the mains on the shared marker
/// timeline, estimated by band-limited complex cross-correlation before any
/// candidate synthesis, plus the per-crossover search window derived from it
/// (2026-07-29 expert review, finding 2). A positive value means the sub is
/// late and the main must be delayed to meet it; a negative value means the
/// hardware needs delay on the sub side.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossoverArrivalEstimate {
    pub id: String,
    pub crossover_hz: f64,
    pub left_arrival_ms: f64,
    pub right_arrival_ms: f64,
    pub center_ms: f64,
    /// Disagreement between the two independent L/R estimates of the same
    /// physical offset - an honest empirical uncertainty for the estimate.
    pub left_right_spread_ms: f64,
    pub window_low_ms: f64,
    pub window_high_ms: f64,
    /// True when the half-period window around the estimated arrival had to
    /// be clipped by the configured hardware delay range.
    pub range_limited: bool,
}

#[derive(Debug, Clone)]
struct PreparedCandidate {
    responses: Vec<[Vec<f64>; 2]>,
    phase_irregularity_rms_ms: Option<f64>,
    timing_repeatability_rms_ms: Option<f64>,
}

/// Rank manual hardware-setting trials. All candidates must contain the same
/// positions, weights, channels, and exact common frequency grid.
pub fn rank_candidates(
    candidates: &[SubIntegrationCandidate],
    config: &RankingConfig,
) -> DspResult<SubIntegrationReport> {
    validate_config(config)?;
    let common_grid = validate_candidates(candidates)?;
    let minimum_crossover = candidates
        .iter()
        .map(|candidate| candidate.settings.crossover_hz)
        .fold(f64::INFINITY, f64::min);
    let maximum_crossover = candidates
        .iter()
        .map(|candidate| candidate.settings.crossover_hz)
        .fold(f64::NEG_INFINITY, f64::max);
    let (band_indices, band) = scoring_band(
        common_grid,
        0.5 * minimum_crossover,
        2.0 * maximum_crossover,
    )?;
    let band_frequencies: Vec<f64> = band_indices
        .iter()
        .map(|&index| common_grid[index])
        .collect();
    let frequency_weights = log_frequency_weights(&band_frequencies)?;

    let phase_evidence_available = candidates.iter().all(|candidate| {
        candidate.positions.iter().all(|position| {
            position.left_combined.phase_rad.is_some()
                && position.right_combined.phase_rad.is_some()
        })
    });
    let timing_evidence_available = candidates.iter().all(|candidate| {
        candidate.positions.iter().all(|position| {
            position.left_combined.timing.is_some() && position.right_combined.timing.is_some()
        })
    });
    let spatial_evidence_available = candidates[0].positions.len() > 1;

    let mut prepared = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let mut responses = Vec::with_capacity(candidate.positions.len());
        let mut phase_values = Vec::new();
        let mut phase_weights = Vec::new();
        let mut timing_values = Vec::new();
        let mut timing_weights = Vec::new();

        for position in &candidate.positions {
            let mut channels = [Vec::new(), Vec::new()];
            for (channel_index, response) in [&position.left_combined, &position.right_combined]
                .into_iter()
                .enumerate()
            {
                channels[channel_index] = gaussian_log_smooth_at(
                    common_grid,
                    &response.magnitude_db,
                    &band_indices,
                    config.magnitude_smoothing_octaves,
                )?;
                // Self-referenced anchor normalization (v2): subtract this
                // response's own 200-500 Hz median before any cross-candidate
                // comparison. The anchor band sits above every supported
                // crossover, so it carries playback/capture level, not
                // crossover behaviour; without this, a fraction of a dB of
                // session volume drift shifts one candidate's responses up,
                // its curves become the shared upper envelope, and every
                // other candidate is charged a fake deficit across the whole
                // band (2026-07-29 expert review, finding 1).
                if let Some(anchor_db) = response_anchor_db(common_grid, &response.magnitude_db)? {
                    for value in &mut channels[channel_index] {
                        *value -= anchor_db;
                    }
                }

                if phase_evidence_available {
                    let phase = response
                        .phase_rad
                        .as_ref()
                        .expect("availability checked above");
                    let irregularity = group_delay_irregularity_rms_ms(
                        common_grid,
                        phase,
                        &band_indices,
                        &frequency_weights,
                        config.group_delay_smoothing_octaves,
                    )?;
                    phase_values.push(irregularity);
                    phase_weights.push(position.weight);
                }
                if timing_evidence_available {
                    let timing = response
                        .timing
                        .as_ref()
                        .expect("availability checked above");
                    timing_values.push(timing.repeatability_rms_ms);
                    timing_weights.push(position.weight);
                }
            }
            responses.push(channels);
        }

        prepared.push(PreparedCandidate {
            responses,
            phase_irregularity_rms_ms: phase_evidence_available
                .then(|| weighted_rms(&phase_values, &phase_weights))
                .transpose()?,
            timing_repeatability_rms_ms: timing_evidence_available
                .then(|| weighted_rms(&timing_values, &timing_weights))
                .transpose()?,
        });
    }

    let position_count = candidates[0].positions.len();
    let mut envelopes = vec![[Vec::new(), Vec::new()]; position_count];
    for (position_index, position_envelopes) in envelopes.iter_mut().enumerate() {
        for (channel_index, channel_envelope) in position_envelopes.iter_mut().enumerate() {
            *channel_envelope = (0..band_indices.len())
                .map(|bin| {
                    prepared
                        .iter()
                        .map(|candidate| candidate.responses[position_index][channel_index][bin])
                        .fold(f64::NEG_INFINITY, f64::max)
                })
                .collect();
        }
    }

    let normalized_position_weights = normalize_positive_weights(
        &candidates[0]
            .positions
            .iter()
            .map(|position| position.weight)
            .collect::<Vec<_>>(),
    );
    let mut rankings = Vec::with_capacity(candidates.len());
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        let mut observations = Vec::with_capacity(position_count * 2);
        let mut all_deficits = Vec::new();
        let mut all_weights = Vec::new();
        let mut seat_rms = Vec::with_capacity(position_count);

        for (position_index, position) in candidate.positions.iter().enumerate() {
            let mut seat_values = Vec::new();
            let mut seat_weights = Vec::new();
            for (channel_index, channel) in [Channel::Left, Channel::Right].into_iter().enumerate()
            {
                let deficits: Vec<f64> = envelopes[position_index][channel_index]
                    .iter()
                    .zip(&prepared[candidate_index].responses[position_index][channel_index])
                    .map(|(envelope, value)| (envelope - value).max(0.0))
                    .collect();
                let rms_db = weighted_rms(&deficits, &frequency_weights)?;
                let p95_db = weighted_percentile(&deficits, &frequency_weights, 0.95)?;
                let worst_db = deficits.iter().copied().fold(0.0_f64, f64::max);
                observations.push(ObservationDeficitMetrics {
                    position_id: position.id.clone(),
                    channel,
                    rms_db,
                    p95_db,
                    worst_db,
                });
                seat_values.extend_from_slice(&deficits);
                seat_weights.extend_from_slice(&frequency_weights);
                all_deficits.extend_from_slice(&deficits);
                all_weights.extend(
                    frequency_weights
                        .iter()
                        .map(|weight| weight * normalized_position_weights[position_index]),
                );
            }
            seat_rms.push(weighted_rms(&seat_values, &seat_weights)?);
        }

        let deficit_rms_db = weighted_rms(&all_deficits, &all_weights)?;
        let deficit_p95_db = weighted_percentile(&all_deficits, &all_weights, 0.95)?;
        let deficit_worst_db = all_deficits.iter().copied().fold(0.0_f64, f64::max);
        let (worst_seat_rms_db, spatial_spread_rms_db) = if spatial_evidence_available {
            (
                Some(seat_rms.iter().copied().fold(0.0_f64, f64::max)),
                Some(weighted_standard_deviation(
                    &seat_rms,
                    &normalized_position_weights,
                )?),
            )
        } else {
            (None, None)
        };
        let delay_regularization_db = candidate.settings.main_delay_ms.map_or(0.0, |delay| {
            delay.abs() * config.delay_regularization_db_per_ms
        });
        let level_regularization_db = candidate.settings.sub_level_db.map_or(0.0, |level| {
            level.abs() * config.level_regularization_db_per_db
        });
        let mut total_score = deficit_rms_db
            + config.deficit_p95_weight * deficit_p95_db
            + config.deficit_worst_weight * deficit_worst_db
            + delay_regularization_db
            + level_regularization_db;
        if let Some(value) = worst_seat_rms_db {
            total_score += config.worst_seat_weight * value;
        }
        if let Some(value) = spatial_spread_rms_db {
            total_score += config.spatial_spread_weight * value;
        }
        if let Some(value) = prepared[candidate_index].phase_irregularity_rms_ms {
            total_score += config.phase_weight_db_per_ms * value;
        }
        if let Some(value) = prepared[candidate_index].timing_repeatability_rms_ms {
            total_score += config.timing_weight_db_per_ms * value;
        }
        if !total_score.is_finite() {
            return Err(DspError::InvalidArgument(format!(
                "candidate '{}' produced a non-finite score",
                candidate.id
            )));
        }

        rankings.push(RankedCandidate {
            rank: 0,
            id: candidate.id.clone(),
            settings: candidate.settings.clone(),
            metrics: CandidateMetrics {
                deficit_rms_db,
                deficit_p95_db,
                deficit_worst_db,
                worst_seat_rms_db,
                spatial_spread_rms_db,
                phase_irregularity_rms_ms: prepared[candidate_index].phase_irregularity_rms_ms,
                timing_repeatability_rms_ms: prepared[candidate_index].timing_repeatability_rms_ms,
                delay_regularization_db,
                level_regularization_db,
                total_score,
            },
            observations,
        });
    }

    rankings.sort_by(|left, right| {
        left.metrics
            .total_score
            .total_cmp(&right.metrics.total_score)
            .then_with(|| left.id.cmp(&right.id))
    });
    for (index, candidate) in rankings.iter_mut().enumerate() {
        candidate.rank = index + 1;
    }

    let anchor_level_spread_db = anchor_level_spread(candidates, common_grid)?;
    let mut warnings = Vec::new();
    if !spatial_evidence_available {
        warnings.push(
            "Only one listening position is available; spatial overfit penalties are unavailable and confidence is limited."
                .into(),
        );
    }
    if !phase_evidence_available {
        warnings.push(
            "At least one response lacks phase; the group-delay consistency term is unavailable."
                .into(),
        );
    }
    if !timing_evidence_available {
        warnings.push(
            "At least one response lacks repeatable arrival-time evidence; the timing reliability term is unavailable."
                .into(),
        );
    }
    match anchor_level_spread_db {
        Some(spread) if spread > config.anchor_warning_threshold_db => warnings.push(format!(
            "Candidate 200-500 Hz anchor levels span {spread:.2} dB before scoring; scoring \
             normalizes each response to its own anchor, but this much drift suggests the \
             playback or capture level changed between candidates - check Roon volume \
             leveling and hardware volume."
        )),
        None => warnings.push(
            "The common grid does not contain enough 200-500 Hz bins to check anchor-level stability."
                .into(),
        ),
        _ => {}
    }

    Ok(SubIntegrationReport {
        band,
        rankings,
        spatial_evidence_available,
        phase_evidence_available,
        timing_evidence_available,
        anchor_level_spread_db,
        needs_confirmation: true,
        warnings,
    })
}

/// Search main-delay and 0/180-degree sub-polarity candidates across isolated
/// paths measured at two or more real hardware crossover settings.
///
/// No crossover transfer function is invented here: each `SeparatedCrossoverPaths`
/// entry must already include the amplifier/subwoofer filtering produced by its
/// declared physical crossover. The complex sums remain predictions and always
/// require a newly measured L+sub/R+sub confirmation.
pub fn optimize_separated_paths(
    paths: &[SeparatedCrossoverPaths],
    config: &SeparatedPathOptimizationConfig,
) -> DspResult<SeparatedPathOptimizationReport> {
    validate_separated_optimization(paths, config)?;

    let minimum_crossover_hz = paths
        .iter()
        .map(|path| path.crossover_hz)
        .fold(f64::INFINITY, f64::min);
    let common_grid = &paths[0].left_main.frequencies_hz;
    let retained_indices = common_grid
        .iter()
        .enumerate()
        .filter_map(|(index, frequency_hz)| {
            (*frequency_hz >= (0.4 * minimum_crossover_hz).max(20.0) && *frequency_hz <= 500.0)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if retained_indices.len() < MIN_SCORING_BINS {
        return Err(DspError::InvalidArgument(
            "isolated paths do not contain enough 20-500 Hz bins for optimization".into(),
        ));
    }

    // Estimate each crossover's actual sub-minus-main arrival on the shared
    // marker timeline first, then search only a crossover half-period around
    // it, snapped to absolute multiples of the hardware delay step. A fixed
    // 0-5 ms grid cannot cover a DSP sub whose processing latency plus
    // placement offset exceeds it, and 5 ms is only 144 degrees at 80 Hz
    // (2026-07-29 expert review, finding 2).
    let mut arrival_estimates = Vec::with_capacity(paths.len());
    let mut path_grids = Vec::with_capacity(paths.len());
    let mut candidate_count = 0_usize;
    for path in paths {
        let left_arrival_ms = estimate_relative_arrival_ms(
            common_grid,
            &path.left_main,
            &path.sub,
            path.crossover_hz,
        )?;
        let right_arrival_ms = estimate_relative_arrival_ms(
            common_grid,
            &path.right_main,
            &path.sub,
            path.crossover_hz,
        )?;
        let center_ms = 0.5 * (left_arrival_ms + right_arrival_ms);
        let half_period_ms = 1_000.0 / (2.0 * path.crossover_hz);
        let window_low_ms = (center_ms - half_period_ms).max(config.delay_minimum_ms);
        let window_high_ms = (center_ms + half_period_ms).min(config.delay_maximum_ms);
        let range_limited = center_ms - half_period_ms < config.delay_minimum_ms - 1.0e-9
            || center_ms + half_period_ms > config.delay_maximum_ms + 1.0e-9;
        let grid = snapped_delay_grid(window_low_ms, window_high_ms, config.delay_step_ms)?;
        candidate_count = candidate_count
            .checked_add(grid.len().checked_mul(2).ok_or_else(|| {
                DspError::InvalidArgument("separated-path search size overflowed".into())
            })?)
            .ok_or_else(|| {
                DspError::InvalidArgument("separated-path search size overflowed".into())
            })?;
        arrival_estimates.push(CrossoverArrivalEstimate {
            id: path.id.clone(),
            crossover_hz: path.crossover_hz,
            left_arrival_ms,
            right_arrival_ms,
            center_ms,
            left_right_spread_ms: (left_arrival_ms - right_arrival_ms).abs(),
            window_low_ms,
            window_high_ms,
            range_limited,
        });
        path_grids.push(grid);
    }
    if candidate_count > 10_000 {
        return Err(DspError::InvalidArgument(
            "separated-path search is limited to 10000 synthesized candidates".into(),
        ));
    }

    let mut candidates = Vec::with_capacity(candidate_count);
    for (path, delays) in paths.iter().zip(&path_grids) {
        for &delay_ms in delays {
            for polarity in [Polarity::Normal, Polarity::Inverted] {
                let left_combined = synthesize_isolated_sum(
                    &path.left_main,
                    &path.sub,
                    &retained_indices,
                    delay_ms - config.measured_main_delay_ms,
                    polarity != config.measured_polarity,
                )?;
                let right_combined = synthesize_isolated_sum(
                    &path.right_main,
                    &path.sub,
                    &retained_indices,
                    delay_ms - config.measured_main_delay_ms,
                    polarity != config.measured_polarity,
                )?;
                candidates.push(SubIntegrationCandidate {
                    id: format!(
                        "{}-delay-{delay_ms:+.3}-{}",
                        path.id,
                        match polarity {
                            Polarity::Normal => "normal",
                            Polarity::Inverted => "inverted",
                        }
                    ),
                    settings: CandidateSettings {
                        crossover_hz: path.crossover_hz,
                        main_delay_ms: Some(delay_ms),
                        sub_level_db: None,
                        polarity: Some(polarity),
                    },
                    positions: vec![PositionObservation {
                        id: "P0".into(),
                        weight: 2.0,
                        left_combined,
                        right_combined,
                    }],
                });
            }
        }
    }
    let mut ranking = rank_candidates(&candidates, &config.ranking)?;
    for estimate in &arrival_estimates {
        if estimate.range_limited {
            ranking.warnings.push(format!(
                "{}: the estimated sub arrival ({:+.2} ms) needs a +/-{:.2} ms window, but the \
                 configured delay range clips it to [{:+.2}, {:+.2}] ms; if the winner sits at \
                 that edge, the hardware range - not the room - chose it.",
                estimate.id,
                estimate.center_ms,
                1_000.0 / (2.0 * estimate.crossover_hz),
                estimate.window_low_ms,
                estimate.window_high_ms,
            ));
        }
        if estimate.left_right_spread_ms > config.delay_step_ms.max(0.25) {
            ranking.warnings.push(format!(
                "{}: the left and right channels disagree about the sub arrival by {:.2} ms; \
                 treat the recommended delay as uncertain by at least that much.",
                estimate.id, estimate.left_right_spread_ms,
            ));
        }
    }
    if let Some(best) = ranking.rankings.first() {
        if let (Some(best_delay), Some(grid)) = (
            best.settings.main_delay_ms,
            paths
                .iter()
                .position(|path| path.crossover_hz == best.settings.crossover_hz)
                .map(|index| &path_grids[index]),
        ) {
            let at_edge = grid.first().zip(grid.last()).is_some_and(|(first, last)| {
                grid.len() > 1
                    && ((best_delay - first).abs() < 1.0e-9 || (best_delay - last).abs() < 1.0e-9)
            });
            if at_edge {
                ranking.warnings.push(format!(
                    "The winning delay ({best_delay:+.2} ms) sits at the edge of its search \
                     window; the true optimum may lie outside the configured hardware range."
                ));
            }
        }
    }
    ranking.warnings.push(
        "Crossover entries are separately measured hardware states; no crossover transfer function was synthesized."
            .into(),
    );
    ranking.warnings.push(
        "The mono sub-only path is reused for L and R prediction; symmetric bass-management routing must be confirmed."
            .into(),
    );
    ranking.warnings.push(
        "Every result is a complex-sum prediction and requires new measured L+sub/R+sub confirmation at the selected setting."
            .into(),
    );
    // Sub-level advisory: purely free synthesis (a scalar on the measured
    // complex sub path), scored on acoustic deficits only, never entering the
    // recommendation itself.
    let sub_level_advisory = ranking.rankings.first().and_then(|best| {
        let path = paths
            .iter()
            .find(|path| path.crossover_hz == best.settings.crossover_hz)?;
        let delay_ms = best.settings.main_delay_ms?;
        let polarity = best.settings.polarity?;
        let mut variants = Vec::new();
        for gain_step in -6_i32..=6 {
            let gain_db = f64::from(gain_step);
            let mut scaled_sub = path.sub.clone();
            for value in &mut scaled_sub.magnitude_db {
                *value += gain_db;
            }
            let left_combined = synthesize_isolated_sum(
                &path.left_main,
                &scaled_sub,
                &retained_indices,
                delay_ms - config.measured_main_delay_ms,
                polarity != config.measured_polarity,
            )
            .ok()?;
            let right_combined = synthesize_isolated_sum(
                &path.right_main,
                &scaled_sub,
                &retained_indices,
                delay_ms - config.measured_main_delay_ms,
                polarity != config.measured_polarity,
            )
            .ok()?;
            variants.push(SubIntegrationCandidate {
                id: format!("sub-gain-{gain_step:+03}"),
                settings: CandidateSettings {
                    crossover_hz: path.crossover_hz,
                    main_delay_ms: Some(delay_ms),
                    sub_level_db: Some(gain_db),
                    polarity: Some(polarity),
                },
                positions: vec![PositionObservation {
                    id: "P0".into(),
                    weight: 2.0,
                    left_combined,
                    right_combined,
                }],
            });
        }
        // Each gain variant is scored against its *own* one-octave envelope,
        // not against the other variants: a cross-variant envelope would
        // always reward the loudest sub. Self-referenced dip depth measures
        // integration quality (the crossover suckout) and is invariant to the
        // absolute gain itself.
        let dip_rms_for = |candidate: &SubIntegrationCandidate| -> Option<f64> {
            let mut deficits = Vec::new();
            let mut weights = Vec::new();
            for response in [
                &candidate.positions[0].left_combined,
                &candidate.positions[0].right_combined,
            ] {
                // Synthesized sums live on their own retained band grid.
                let variant_grid = &response.frequencies_hz;
                let all_indices: Vec<usize> = (0..variant_grid.len()).collect();
                let variant_weights = log_frequency_weights(variant_grid).ok()?;
                let fine = gaussian_log_smooth_at(
                    variant_grid,
                    &response.magnitude_db,
                    &all_indices,
                    config.ranking.magnitude_smoothing_octaves,
                )
                .ok()?;
                let wide =
                    gaussian_log_smooth_at(variant_grid, &response.magnitude_db, &all_indices, 1.0)
                        .ok()?;
                for ((fine_db, wide_db), weight) in fine.iter().zip(&wide).zip(&variant_weights) {
                    deficits.push((wide_db - fine_db).max(0.0));
                    weights.push(*weight);
                }
            }
            weighted_rms(&deficits, &weights).ok()
        };
        let deficit_rms_at_zero_db = variants
            .iter()
            .find(|candidate| candidate.settings.sub_level_db == Some(0.0))
            .and_then(&dip_rms_for)?;
        let (best_gain_db, deficit_rms_at_best_db) = variants
            .iter()
            .filter_map(|candidate| {
                let gain_db = candidate.settings.sub_level_db?;
                dip_rms_for(candidate).map(|deficit| (gain_db, deficit))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))?;
        Some(SubLevelAdvisory {
            best_gain_db,
            deficit_rms_at_best_db,
            deficit_rms_at_zero_db,
        })
    });
    Ok(SeparatedPathOptimizationReport {
        algorithm_version: SEPARATED_PATH_OPTIMIZATION_VERSION,
        synthesized_candidate_count: candidate_count,
        measured_main_delay_ms: config.measured_main_delay_ms,
        measured_polarity: config.measured_polarity,
        arrival_estimates,
        sub_level_advisory,
        ranking,
    })
}

fn validate_separated_optimization(
    paths: &[SeparatedCrossoverPaths],
    config: &SeparatedPathOptimizationConfig,
) -> DspResult<()> {
    if paths.len() < 2 || paths.len() > 12 {
        return Err(DspError::InvalidArgument(
            "separated-path optimization requires 2 to 12 measured crossover states".into(),
        ));
    }
    validate_config(&config.ranking)?;
    for (name, value) in [
        ("measured main delay", config.measured_main_delay_ms),
        ("minimum main delay", config.delay_minimum_ms),
        ("maximum main delay", config.delay_maximum_ms),
        ("main-delay step", config.delay_step_ms),
    ] {
        if !value.is_finite() {
            return Err(DspError::InvalidArgument(format!("{name} must be finite")));
        }
    }
    if config.delay_minimum_ms < -20.0
        || config.delay_maximum_ms > 50.0
        || config.delay_maximum_ms < config.delay_minimum_ms
        || config.measured_main_delay_ms < -20.0
        || config.measured_main_delay_ms > 50.0
    {
        return Err(DspError::InvalidArgument(
            "main delays must stay within -20 to 50 ms and the search range must increase".into(),
        ));
    }
    if !(0.01..=5.0).contains(&config.delay_step_ms) {
        return Err(DspError::InvalidArgument(
            "main-delay step must be between 0.01 and 5 ms".into(),
        ));
    }
    let mut ids = HashSet::new();
    let mut crossovers = HashSet::new();
    let common_grid = &paths[0].left_main.frequencies_hz;
    for path in paths {
        validate_id(&path.id, "separated crossover")?;
        validate_crossover(path.crossover_hz)?;
        if !ids.insert(path.id.as_str()) {
            return Err(DspError::InvalidArgument(format!(
                "duplicate separated crossover id '{}'",
                path.id
            )));
        }
        if !crossovers.insert(path.crossover_hz.to_bits()) {
            return Err(DspError::InvalidArgument(format!(
                "duplicate separated crossover {:.3} Hz",
                path.crossover_hz
            )));
        }
        for (label, response) in [
            ("left main", &path.left_main),
            ("right main", &path.right_main),
            ("sub", &path.sub),
        ] {
            validate_response(response, label)?;
            require_same_grid(common_grid, &response.frequencies_hz, label)?;
            if response.phase_rad.is_none() {
                return Err(DspError::InvalidArgument(format!(
                    "{label} phase is required for separated-path optimization"
                )));
            }
        }
    }
    Ok(())
}

/// Band-limited complex cross-correlation arrival estimate: the lag that best
/// aligns the sub path to the main path over the octave straddling the
/// crossover. Both paths already share the marker timeline, so the lag is the
/// physical sub-minus-main offset (positive = sub late = delay the main).
fn estimate_relative_arrival_ms(
    common_grid: &[f64],
    main: &CombinedResponse,
    sub: &CombinedResponse,
    crossover_hz: f64,
) -> DspResult<f64> {
    let main_phase = main
        .phase_rad
        .as_ref()
        .expect("validated: separated paths carry phase");
    let sub_phase = sub
        .phase_rad
        .as_ref()
        .expect("validated: separated paths carry phase");
    let band_low_hz = (0.5 * crossover_hz).max(20.0);
    let band_high_hz = (2.0 * crossover_hz).min(240.0);
    let mut cross = Vec::new();
    for (index, frequency) in common_grid.iter().enumerate() {
        if (band_low_hz..=band_high_hz).contains(frequency) {
            let weight = 10.0_f64.powf(main.magnitude_db[index] / 20.0)
                * 10.0_f64.powf(sub.magnitude_db[index] / 20.0);
            let phase = sub_phase[index] - main_phase[index];
            cross.push((*frequency, weight, phase));
        }
    }
    if cross.len() < MIN_SCORING_BINS {
        return Err(DspError::InvalidArgument(format!(
            "not enough {band_low_hz:.0}-{band_high_hz:.0} Hz bins to estimate the sub arrival"
        )));
    }
    let scan_step_ms = 0.05;
    let scan_limit_ms = 20.0;
    let mut best_tau_ms = 0.0;
    let mut best_magnitude = f64::NEG_INFINITY;
    let mut tau_ms = -scan_limit_ms;
    while tau_ms <= scan_limit_ms + 1.0e-9 {
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for (frequency, weight, phase) in &cross {
            let angle = phase + 2.0 * std::f64::consts::PI * frequency * tau_ms / 1_000.0;
            real += weight * angle.cos();
            imaginary += weight * angle.sin();
        }
        let magnitude = real.hypot(imaginary);
        if magnitude > best_magnitude {
            best_magnitude = magnitude;
            best_tau_ms = tau_ms;
        }
        tau_ms += scan_step_ms;
    }
    if !best_magnitude.is_finite() {
        return Err(DspError::InvalidArgument(
            "sub arrival estimation produced a non-finite correlation".into(),
        ));
    }
    Ok(best_tau_ms)
}

/// Absolute multiples of the hardware delay step inside [low, high]. Snapping
/// to absolute multiples means every candidate is a value the user can
/// actually dial in.
fn snapped_delay_grid(low_ms: f64, high_ms: f64, step_ms: f64) -> DspResult<Vec<f64>> {
    if high_ms < low_ms {
        // The clipped window collapsed; search the nearest representable value
        // so the caller still receives a ranked (and edge-flagged) result.
        let snapped = (low_ms / step_ms).round() * step_ms;
        return Ok(vec![snapped]);
    }
    let first_index = (low_ms / step_ms - 1.0e-9).ceil() as i64;
    let last_index = (high_ms / step_ms + 1.0e-9).floor() as i64;
    if last_index < first_index {
        let snapped = (0.5 * (low_ms + high_ms) / step_ms).round() * step_ms;
        return Ok(vec![snapped]);
    }
    let count = usize::try_from(last_index - first_index + 1)
        .map_err(|_| DspError::InvalidArgument("delay grid size overflowed".into()))?;
    if count > 1_001 {
        return Err(DspError::InvalidArgument(
            "main-delay search is limited to 1001 values".into(),
        ));
    }
    Ok((first_index..=last_index)
        .map(|index| index as f64 * step_ms)
        .collect())
}

fn synthesize_isolated_sum(
    main: &CombinedResponse,
    sub: &CombinedResponse,
    retained_indices: &[usize],
    additional_main_delay_ms: f64,
    invert_sub: bool,
) -> DspResult<CombinedResponse> {
    let main_phase = main.phase_rad.as_ref().ok_or_else(|| {
        DspError::InvalidArgument("main phase is required for isolated-path sum".into())
    })?;
    let sub_phase = sub.phase_rad.as_ref().ok_or_else(|| {
        DspError::InvalidArgument("sub phase is required for isolated-path sum".into())
    })?;
    let mut frequencies_hz = Vec::with_capacity(retained_indices.len());
    let mut magnitude_db = Vec::with_capacity(retained_indices.len());
    let mut phase_rad = Vec::with_capacity(retained_indices.len());
    for &index in retained_indices {
        let frequency_hz = main.frequencies_hz[index];
        let main_amplitude = 10.0_f64.powf(main.magnitude_db[index] / 20.0);
        let sub_amplitude = 10.0_f64.powf(sub.magnitude_db[index] / 20.0);
        let delayed_main_phase =
            main_phase[index] - 2.0 * PI * frequency_hz * additional_main_delay_ms / 1_000.0;
        let adjusted_sub_phase = sub_phase[index] + if invert_sub { PI } else { 0.0 };
        let real =
            main_amplitude * delayed_main_phase.cos() + sub_amplitude * adjusted_sub_phase.cos();
        let imaginary =
            main_amplitude * delayed_main_phase.sin() + sub_amplitude * adjusted_sub_phase.sin();
        let amplitude = real.hypot(imaginary);
        let level_db = 20.0 * amplitude.max(1.0e-15).log10();
        let phase = imaginary.atan2(real);
        if !level_db.is_finite() || !phase.is_finite() {
            return Err(DspError::NonFinite {
                context: "synthesized isolated-path response",
                index,
            });
        }
        frequencies_hz.push(frequency_hz);
        magnitude_db.push(level_db);
        phase_rad.push(phase);
    }
    Ok(CombinedResponse {
        frequencies_hz,
        magnitude_db,
        phase_rad: Some(phase_rad),
        timing: None,
    })
}

/// Compare separately captured main and sub paths with their measured sum.
/// Main and sub phase are required to form the complex prediction.
pub fn analyze_separated_paths(
    main: &CombinedResponse,
    sub: &CombinedResponse,
    combined: &CombinedResponse,
    crossover_hz: f64,
) -> DspResult<SeparatedPathAnalysis> {
    validate_crossover(crossover_hz)?;
    validate_response(main, "main response")?;
    validate_response(sub, "sub response")?;
    validate_response(combined, "combined response")?;
    require_same_grid(&main.frequencies_hz, &sub.frequencies_hz, "sub response")?;
    require_same_grid(
        &main.frequencies_hz,
        &combined.frequencies_hz,
        "combined response",
    )?;
    let main_phase = main.phase_rad.as_ref().ok_or_else(|| {
        DspError::InvalidArgument("main phase is required for separated-path analysis".into())
    })?;
    let sub_phase = sub.phase_rad.as_ref().ok_or_else(|| {
        DspError::InvalidArgument("sub phase is required for separated-path analysis".into())
    })?;
    let (indices, band) =
        scoring_band(&main.frequencies_hz, 0.5 * crossover_hz, 2.0 * crossover_hz)?;
    let frequencies: Vec<f64> = indices
        .iter()
        .map(|&index| main.frequencies_hz[index])
        .collect();
    let weights = log_frequency_weights(&frequencies)?;
    let mut prediction_errors = Vec::with_capacity(indices.len());
    let mut cancellation_losses = Vec::with_capacity(indices.len());

    for &index in &indices {
        let main_amplitude = 10.0_f64.powf(main.magnitude_db[index] / 20.0);
        let sub_amplitude = 10.0_f64.powf(sub.magnitude_db[index] / 20.0);
        if !main_amplitude.is_finite() || !sub_amplitude.is_finite() {
            return Err(DspError::NonFinite {
                context: "separated-path linear amplitude",
                index,
            });
        }
        let real =
            main_amplitude * main_phase[index].cos() + sub_amplitude * sub_phase[index].cos();
        let imaginary =
            main_amplitude * main_phase[index].sin() + sub_amplitude * sub_phase[index].sin();
        let predicted_db = 20.0 * real.hypot(imaginary).max(1.0e-15).log10();
        prediction_errors.push(predicted_db - combined.magnitude_db[index]);
        cancellation_losses.push(
            (main.magnitude_db[index].max(sub.magnitude_db[index]) - combined.magnitude_db[index])
                .max(0.0),
        );
    }

    Ok(SeparatedPathAnalysis {
        band,
        complex_sum_magnitude_rmse_db: weighted_rms(&prediction_errors, &weights)?,
        cancellation_loss_rms_db: weighted_rms(&cancellation_losses, &weights)?,
        cancellation_loss_p95_db: weighted_percentile(&cancellation_losses, &weights, 0.95)?,
        cancellation_loss_worst_db: cancellation_losses.iter().copied().fold(0.0_f64, f64::max),
    })
}

fn validate_config(config: &RankingConfig) -> DspResult<()> {
    let positive = [
        ("magnitude smoothing", config.magnitude_smoothing_octaves),
        (
            "group-delay smoothing",
            config.group_delay_smoothing_octaves,
        ),
        (
            "anchor warning threshold",
            config.anchor_warning_threshold_db,
        ),
    ];
    if let Some((name, _)) = positive
        .into_iter()
        .find(|(_, value)| !value.is_finite() || *value <= 0.0)
    {
        return Err(DspError::InvalidArgument(format!(
            "{name} must be finite and greater than zero"
        )));
    }
    let nonnegative = [
        ("deficit p95 weight", config.deficit_p95_weight),
        ("deficit worst weight", config.deficit_worst_weight),
        ("worst-seat weight", config.worst_seat_weight),
        ("spatial-spread weight", config.spatial_spread_weight),
        ("phase weight", config.phase_weight_db_per_ms),
        ("timing weight", config.timing_weight_db_per_ms),
        (
            "delay regularization",
            config.delay_regularization_db_per_ms,
        ),
        (
            "level regularization",
            config.level_regularization_db_per_db,
        ),
    ];
    if let Some((name, _)) = nonnegative
        .into_iter()
        .find(|(_, value)| !value.is_finite() || *value < 0.0)
    {
        return Err(DspError::InvalidArgument(format!(
            "{name} must be finite and nonnegative"
        )));
    }
    Ok(())
}

fn validate_candidates(candidates: &[SubIntegrationCandidate]) -> DspResult<&[f64]> {
    if candidates.is_empty() {
        return Err(DspError::EmptyInput("sub-integration candidates"));
    }
    let mut candidate_ids = HashSet::new();
    for candidate in candidates {
        validate_id(&candidate.id, "candidate")?;
        if !candidate_ids.insert(candidate.id.as_str()) {
            return Err(DspError::InvalidArgument(format!(
                "duplicate candidate id '{}'",
                candidate.id
            )));
        }
        validate_settings(&candidate.settings)?;
        if candidate.positions.is_empty() {
            return Err(DspError::EmptyInput("candidate positions"));
        }
        let mut position_ids = HashSet::new();
        for position in &candidate.positions {
            validate_id(&position.id, "position")?;
            if !position_ids.insert(position.id.as_str()) {
                return Err(DspError::InvalidArgument(format!(
                    "candidate '{}' has duplicate position id '{}'",
                    candidate.id, position.id
                )));
            }
            if !position.weight.is_finite() || position.weight <= 0.0 {
                return Err(DspError::InvalidArgument(format!(
                    "position '{}' weight must be finite and greater than zero",
                    position.id
                )));
            }
            validate_response(&position.left_combined, "left combined response")?;
            validate_response(&position.right_combined, "right combined response")?;
            require_same_grid(
                &position.left_combined.frequencies_hz,
                &position.right_combined.frequencies_hz,
                "right combined response",
            )?;
        }
    }

    let reference_positions = &candidates[0].positions;
    let common_grid = &reference_positions[0].left_combined.frequencies_hz;
    for candidate in candidates {
        if candidate.positions.len() != reference_positions.len() {
            return Err(DspError::ShapeMismatch(
                "all candidates must contain the same positions".into(),
            ));
        }
        for (position, reference) in candidate.positions.iter().zip(reference_positions) {
            if position.id != reference.id
                || position.weight.to_bits() != reference.weight.to_bits()
            {
                return Err(DspError::ShapeMismatch(
                    "all candidates must use identical position ids, order, and weights".into(),
                ));
            }
            require_same_grid(
                common_grid,
                &position.left_combined.frequencies_hz,
                "candidate",
            )?;
        }
    }
    Ok(common_grid)
}

fn validate_id(id: &str, context: &str) -> DspResult<()> {
    if id.trim().is_empty() {
        return Err(DspError::InvalidArgument(format!(
            "{context} id must not be empty"
        )));
    }
    Ok(())
}

fn validate_settings(settings: &CandidateSettings) -> DspResult<()> {
    validate_crossover(settings.crossover_hz)?;
    if let Some(delay) = settings.main_delay_ms {
        if !delay.is_finite() || delay.abs() > MAX_ABS_DELAY_MS {
            return Err(DspError::InvalidArgument(format!(
                "main delay must be finite and within +/-{MAX_ABS_DELAY_MS} ms"
            )));
        }
    }
    if let Some(level) = settings.sub_level_db {
        if !level.is_finite() || !(MIN_SUB_LEVEL_DB..=MAX_SUB_LEVEL_DB).contains(&level) {
            return Err(DspError::InvalidArgument(format!(
                "sub level must be finite and between {MIN_SUB_LEVEL_DB} and {MAX_SUB_LEVEL_DB} dB"
            )));
        }
    }
    Ok(())
}

fn validate_crossover(crossover_hz: f64) -> DspResult<()> {
    if !crossover_hz.is_finite() || !(MIN_CROSSOVER_HZ..=MAX_CROSSOVER_HZ).contains(&crossover_hz) {
        return Err(DspError::InvalidArgument(format!(
            "crossover must be finite and between {MIN_CROSSOVER_HZ} and {MAX_CROSSOVER_HZ} Hz"
        )));
    }
    Ok(())
}

fn validate_response(response: &CombinedResponse, context: &'static str) -> DspResult<()> {
    validate_frequency_grid(&response.frequencies_hz)?;
    if response.magnitude_db.len() != response.frequencies_hz.len() {
        return Err(DspError::ShapeMismatch(format!(
            "{context} magnitude length must match its frequency grid"
        )));
    }
    if let Some(index) = response
        .magnitude_db
        .iter()
        .position(|value| !value.is_finite() || value.abs() > MAX_ABS_MAGNITUDE_DB)
    {
        return Err(DspError::InvalidArgument(format!(
            "{context} magnitude at index {index} must be finite and within +/-{MAX_ABS_MAGNITUDE_DB} dB"
        )));
    }
    if let Some(phase) = &response.phase_rad {
        if phase.len() != response.frequencies_hz.len() {
            return Err(DspError::ShapeMismatch(format!(
                "{context} phase length must match its frequency grid"
            )));
        }
        if let Some(index) = phase.iter().position(|value| !value.is_finite()) {
            return Err(DspError::NonFinite {
                context: "response phase",
                index,
            });
        }
    }
    if let Some(timing) = &response.timing {
        if !timing.arrival_time_ms.is_finite()
            || timing.arrival_time_ms.abs() > MAX_ABS_ARRIVAL_MS
            || !timing.repeatability_rms_ms.is_finite()
            || !(0.0..=MAX_REPEATABILITY_MS).contains(&timing.repeatability_rms_ms)
        {
            return Err(DspError::InvalidArgument(format!(
                "{context} timing evidence is outside the supported finite range"
            )));
        }
    }
    Ok(())
}

fn validate_frequency_grid(frequencies_hz: &[f64]) -> DspResult<()> {
    if frequencies_hz.len() < 2 {
        return Err(DspError::InvalidArgument(
            "frequency grid must contain at least two bins".into(),
        ));
    }
    for (index, &frequency) in frequencies_hz.iter().enumerate() {
        if !frequency.is_finite() || frequency < 0.0 {
            return Err(DspError::InvalidArgument(format!(
                "frequency at index {index} must be finite and nonnegative"
            )));
        }
        if index > 0 && frequency <= frequencies_hz[index - 1] {
            return Err(DspError::InvalidArgument(format!(
                "frequency grid must be strictly increasing; duplicate or reversed bin at index {index}"
            )));
        }
    }
    Ok(())
}

fn require_same_grid(reference: &[f64], actual: &[f64], context: &str) -> DspResult<()> {
    if reference.len() != actual.len()
        || reference
            .iter()
            .zip(actual)
            .any(|(left, right)| left.to_bits() != right.to_bits())
    {
        return Err(DspError::ShapeMismatch(format!(
            "{context} must use the exact common frequency grid"
        )));
    }
    Ok(())
}

fn scoring_band(
    frequencies_hz: &[f64],
    requested_lower_hz: f64,
    requested_upper_hz: f64,
) -> DspResult<(Vec<usize>, ScoringBand)> {
    let lower_hz = requested_lower_hz.max(frequencies_hz[0].max(f64::MIN_POSITIVE));
    let upper_hz = requested_upper_hz.min(*frequencies_hz.last().expect("validated grid"));
    let indices: Vec<usize> = frequencies_hz
        .iter()
        .enumerate()
        .filter_map(|(index, frequency)| {
            (*frequency >= lower_hz && *frequency <= upper_hz).then_some(index)
        })
        .collect();
    if indices.len() < MIN_SCORING_BINS {
        return Err(DspError::InvalidArgument(format!(
            "scoring band {lower_hz:.2}-{upper_hz:.2} Hz contains fewer than {MIN_SCORING_BINS} bins"
        )));
    }
    Ok((
        indices,
        ScoringBand {
            lower_hz,
            upper_hz,
            bin_count: frequencies_hz
                .iter()
                .filter(|frequency| **frequency >= lower_hz && **frequency <= upper_hz)
                .count(),
        },
    ))
}

fn gaussian_log_smooth_at(
    frequencies_hz: &[f64],
    values: &[f64],
    target_indices: &[usize],
    fwhm_octaves: f64,
) -> DspResult<Vec<f64>> {
    let sigma_octaves = fwhm_octaves / (2.0 * (2.0 * LN_2).sqrt());
    let radius_octaves = GAUSSIAN_RADIUS_SIGMA * sigma_octaves;
    let radius_ratio = 2.0_f64.powf(radius_octaves);
    let mut smoothed = Vec::with_capacity(target_indices.len());
    for &target_index in target_indices {
        let target_hz = frequencies_hz[target_index];
        if target_hz <= 0.0 {
            return Err(DspError::InvalidArgument(
                "log-frequency smoothing requires positive scoring frequencies".into(),
            ));
        }
        let first =
            frequencies_hz.partition_point(|frequency| *frequency < target_hz / radius_ratio);
        let end =
            frequencies_hz.partition_point(|frequency| *frequency <= target_hz * radius_ratio);
        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;
        for index in first..end {
            let frequency = frequencies_hz[index];
            if frequency <= 0.0 {
                continue;
            }
            let distance = (frequency / target_hz).log2();
            let weight = (-0.5 * (distance / sigma_octaves).powi(2)).exp();
            weighted_sum += weight * values[index];
            weight_sum += weight;
        }
        let value = weighted_sum / weight_sum;
        if !value.is_finite() {
            return Err(DspError::NonFinite {
                context: "log-frequency smoothing",
                index: target_index,
            });
        }
        smoothed.push(value);
    }
    Ok(smoothed)
}

fn unwrap_phase(phase_rad: &[f64]) -> Vec<f64> {
    let mut result = Vec::with_capacity(phase_rad.len());
    result.push(phase_rad[0]);
    for &phase in &phase_rad[1..] {
        let previous_raw = phase_rad[result.len() - 1];
        let mut delta = phase - previous_raw;
        delta -= (delta / (2.0 * PI)).round() * 2.0 * PI;
        result.push(result.last().copied().expect("nonempty") + delta);
    }
    result
}

fn group_delay_ms(frequencies_hz: &[f64], phase_rad: &[f64]) -> DspResult<Vec<f64>> {
    let unwrapped = unwrap_phase(phase_rad);
    let mut result = Vec::with_capacity(phase_rad.len());
    for index in 0..phase_rad.len() {
        let (left, right) = if index == 0 {
            (0, 1)
        } else if index + 1 == phase_rad.len() {
            (index - 1, index)
        } else {
            (index - 1, index + 1)
        };
        let value = -(unwrapped[right] - unwrapped[left])
            / (2.0 * PI * (frequencies_hz[right] - frequencies_hz[left]))
            * 1_000.0;
        if !value.is_finite() {
            return Err(DspError::NonFinite {
                context: "group delay",
                index,
            });
        }
        result.push(value);
    }
    Ok(result)
}

fn group_delay_irregularity_rms_ms(
    frequencies_hz: &[f64],
    phase_rad: &[f64],
    band_indices: &[usize],
    frequency_weights: &[f64],
    smoothing_octaves: f64,
) -> DspResult<f64> {
    let group_delay = group_delay_ms(frequencies_hz, phase_rad)?;
    let smoothed = gaussian_log_smooth_at(
        frequencies_hz,
        &group_delay,
        band_indices,
        smoothing_octaves,
    )?;
    let median = weighted_percentile(&smoothed, frequency_weights, 0.5)?;
    let residuals: Vec<f64> = smoothed.iter().map(|value| value - median).collect();
    weighted_rms(&residuals, frequency_weights)
}

fn log_frequency_weights(frequencies_hz: &[f64]) -> DspResult<Vec<f64>> {
    if frequencies_hz.len() < 2 || frequencies_hz.iter().any(|frequency| *frequency <= 0.0) {
        return Err(DspError::InvalidArgument(
            "log-frequency weighting requires at least two positive bins".into(),
        ));
    }
    let logs: Vec<f64> = frequencies_hz
        .iter()
        .map(|frequency| frequency.ln())
        .collect();
    let mut weights = Vec::with_capacity(logs.len());
    for index in 0..logs.len() {
        let weight = if index == 0 {
            (logs[1] - logs[0]) / 2.0
        } else if index + 1 == logs.len() {
            (logs[index] - logs[index - 1]) / 2.0
        } else {
            (logs[index + 1] - logs[index - 1]) / 2.0
        };
        weights.push(weight);
    }
    Ok(weights)
}

fn normalize_positive_weights(weights: &[f64]) -> Vec<f64> {
    let maximum = weights.iter().copied().fold(0.0_f64, f64::max);
    weights.iter().map(|weight| weight / maximum).collect()
}

fn weighted_rms(values: &[f64], weights: &[f64]) -> DspResult<f64> {
    if values.is_empty() || values.len() != weights.len() {
        return Err(DspError::ShapeMismatch(
            "weighted RMS requires equal, nonempty values and weights".into(),
        ));
    }
    if values.iter().any(|value| !value.is_finite())
        || weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight <= 0.0)
    {
        return Err(DspError::InvalidArgument(
            "weighted RMS values must be finite and weights positive".into(),
        ));
    }
    let scale = values.iter().copied().map(f64::abs).fold(0.0, f64::max);
    if scale == 0.0 {
        return Ok(0.0);
    }
    let normalized = normalize_positive_weights(weights);
    let total_weight: f64 = normalized.iter().sum();
    let variance = values
        .iter()
        .zip(&normalized)
        .map(|(value, weight)| weight * (value / scale).powi(2))
        .sum::<f64>()
        / total_weight;
    Ok(scale * variance.sqrt())
}

fn weighted_percentile(values: &[f64], weights: &[f64], quantile: f64) -> DspResult<f64> {
    if values.is_empty() || values.len() != weights.len() {
        return Err(DspError::ShapeMismatch(
            "weighted percentile requires equal, nonempty values and weights".into(),
        ));
    }
    if !(0.0..=1.0).contains(&quantile)
        || values.iter().any(|value| !value.is_finite())
        || weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight <= 0.0)
    {
        return Err(DspError::InvalidArgument(
            "weighted percentile values and quantile are invalid".into(),
        ));
    }
    let normalized = normalize_positive_weights(weights);
    let mut pairs: Vec<(f64, f64)> = values.iter().copied().zip(normalized).collect();
    pairs.sort_by(|left, right| left.0.total_cmp(&right.0));
    let threshold = quantile * pairs.iter().map(|(_, weight)| weight).sum::<f64>();
    let mut cumulative = 0.0;
    for (value, weight) in &pairs {
        cumulative += weight;
        if cumulative >= threshold {
            return Ok(*value);
        }
    }
    Ok(pairs.last().expect("nonempty checked").0)
}

fn weighted_standard_deviation(values: &[f64], weights: &[f64]) -> DspResult<f64> {
    if values.is_empty() || values.len() != weights.len() {
        return Err(DspError::ShapeMismatch(
            "weighted spread requires equal, nonempty values and weights".into(),
        ));
    }
    let normalized = normalize_positive_weights(weights);
    let total_weight: f64 = normalized.iter().sum();
    let mean = values
        .iter()
        .zip(&normalized)
        .map(|(value, weight)| value * weight)
        .sum::<f64>()
        / total_weight;
    let residuals: Vec<f64> = values.iter().map(|value| value - mean).collect();
    weighted_rms(&residuals, &normalized)
}

/// One response's own 200-500 Hz median level. `None` when the grid does not
/// cover the anchor band with enough bins; scoring then proceeds without
/// normalization and the report carries the existing coverage warning.
fn response_anchor_db(common_grid: &[f64], magnitude_db: &[f64]) -> DspResult<Option<f64>> {
    let values: Vec<f64> = common_grid
        .iter()
        .zip(magnitude_db)
        .filter_map(|(frequency, value)| (200.0..=500.0).contains(frequency).then_some(*value))
        .collect();
    if values.len() < MIN_SCORING_BINS {
        return Ok(None);
    }
    let weights = vec![1.0; values.len()];
    weighted_percentile(&values, &weights, 0.5).map(Some)
}

fn anchor_level_spread(
    candidates: &[SubIntegrationCandidate],
    common_grid: &[f64],
) -> DspResult<Option<f64>> {
    let indices: Vec<usize> = common_grid
        .iter()
        .enumerate()
        .filter_map(|(index, frequency)| (200.0..=500.0).contains(frequency).then_some(index))
        .collect();
    if indices.len() < MIN_SCORING_BINS {
        return Ok(None);
    }
    let mut anchors = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let mut values = Vec::new();
        let mut weights = Vec::new();
        for position in &candidate.positions {
            for response in [&position.left_combined, &position.right_combined] {
                for &index in &indices {
                    values.push(response.magnitude_db[index]);
                    weights.push(position.weight);
                }
            }
        }
        anchors.push(weighted_percentile(&values, &weights, 0.5)?);
    }
    let minimum = anchors.iter().copied().fold(f64::INFINITY, f64::min);
    let maximum = anchors.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Ok(Some(maximum - minimum))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Vec<f64> {
        (0..241)
            .map(|index| 20.0 * 2.0_f64.powf(index as f64 / 60.0))
            .collect()
    }

    fn response_with<F>(phase: bool, timing: bool, magnitude: F) -> CombinedResponse
    where
        F: Fn(f64) -> f64,
    {
        let frequencies_hz = grid();
        let magnitude_db = frequencies_hz
            .iter()
            .map(|frequency| magnitude(*frequency))
            .collect();
        CombinedResponse {
            phase_rad: phase.then(|| vec![0.0; frequencies_hz.len()]),
            timing: timing.then_some(TimingEvidence {
                arrival_time_ms: 8.0,
                repeatability_rms_ms: 0.01,
            }),
            frequencies_hz,
            magnitude_db,
        }
    }

    fn position<F>(id: &str, magnitude: F) -> PositionObservation
    where
        F: Fn(f64) -> f64 + Copy,
    {
        PositionObservation {
            id: id.into(),
            weight: if id == "P0" { 2.0 } else { 1.0 },
            left_combined: response_with(true, true, magnitude),
            right_combined: response_with(true, true, magnitude),
        }
    }

    fn candidate<F>(id: &str, polarity: Polarity, magnitude: F) -> SubIntegrationCandidate
    where
        F: Fn(f64) -> f64 + Copy,
    {
        SubIntegrationCandidate {
            id: id.into(),
            settings: CandidateSettings {
                crossover_hz: 80.0,
                main_delay_ms: Some(0.0),
                sub_level_db: Some(0.0),
                polarity: Some(polarity),
            },
            positions: vec![position("P0", magnitude)],
        }
    }

    fn broad_notch(frequency: f64, center: f64, depth: f64) -> f64 {
        let distance = (frequency / center).log2() / 0.28;
        -depth * (-0.5 * distance * distance).exp()
    }

    fn set_group_delay<F>(candidate: &mut SubIntegrationCandidate, group_delay_ms: F)
    where
        F: Fn(f64) -> f64 + Copy,
    {
        let frequencies_hz = &candidate.positions[0].left_combined.frequencies_hz;
        let mut unwrapped = vec![0.0; frequencies_hz.len()];
        for index in 1..frequencies_hz.len() {
            let midpoint = (frequencies_hz[index - 1] * frequencies_hz[index]).sqrt();
            let delta_hz = frequencies_hz[index] - frequencies_hz[index - 1];
            unwrapped[index] =
                unwrapped[index - 1] - 2.0 * PI * group_delay_ms(midpoint) * delta_hz / 1_000.0;
        }
        let wrapped: Vec<f64> = unwrapped
            .into_iter()
            .map(|phase| (phase + PI).rem_euclid(2.0 * PI) - PI)
            .collect();
        candidate.positions[0].left_combined.phase_rad = Some(wrapped.clone());
        candidate.positions[0].right_combined.phase_rad = Some(wrapped);
    }

    #[test]
    fn a_global_level_offset_cannot_shift_the_ranking() {
        // Candidate "flat" is genuinely better; candidate "notched" carries a
        // broad crossover notch. Adding a +1 dB global playback-level offset
        // to the notched candidate must change neither the scores nor the
        // order: each response is normalized to its own 200-500 Hz anchor
        // before the cross-candidate envelope is built, so level drift cannot
        // raise the envelope and charge the other candidate a fake deficit.
        let flat = candidate("flat", Polarity::Normal, |_| 0.0);
        let notched = candidate("notched", Polarity::Normal, |frequency| {
            broad_notch(frequency, 80.0, 8.0)
        });
        let notched_louder = candidate("notched", Polarity::Normal, |frequency| {
            broad_notch(frequency, 80.0, 8.0) + 1.0
        });
        let baseline =
            rank_candidates(&[flat.clone(), notched], &RankingConfig::default()).unwrap();
        let offset = rank_candidates(&[flat, notched_louder], &RankingConfig::default()).unwrap();
        assert_eq!(baseline.rankings[0].id, "flat");
        assert_eq!(offset.rankings[0].id, "flat");
        for (before, after) in baseline.rankings.iter().zip(&offset.rankings) {
            assert_eq!(before.id, after.id);
            assert!(
                (before.metrics.total_score - after.metrics.total_score).abs() < 1.0e-9,
                "{}: {} vs {}",
                before.id,
                before.metrics.total_score,
                after.metrics.total_score
            );
        }
    }

    #[test]
    fn correct_polarity_beats_a_broad_cancellation_notch() {
        let correct = candidate("normal", Polarity::Normal, |_| 0.0);
        let wrong = candidate("inverted", Polarity::Inverted, |frequency| {
            broad_notch(frequency, 80.0, 14.0)
        });
        let report = rank_candidates(&[wrong, correct], &RankingConfig::default()).unwrap();
        assert_eq!(report.rankings[0].id, "normal");
        assert!(report.rankings[1].metrics.deficit_rms_db > 3.0);
        assert!(report.needs_confirmation);
        // Polarity is judged from the observation; it has no inherent penalty.
        assert_eq!(report.rankings[0].metrics.total_score, 0.001);
    }

    #[test]
    fn repeated_broad_dip_is_penalized_after_fractional_octave_smoothing() {
        let flat = candidate("flat", Polarity::Normal, |_| 0.0);
        let dipped = candidate("dip", Polarity::Normal, |frequency| {
            broad_notch(frequency, 95.0, 10.0)
        });
        let report = rank_candidates(&[dipped, flat], &RankingConfig::default()).unwrap();
        assert_eq!(report.rankings[0].id, "flat");
        assert!(report.rankings[1].metrics.deficit_p95_db > 2.0);
    }

    #[test]
    fn broad_group_delay_irregularity_is_only_an_auxiliary_term() {
        let mut flat = candidate("flat-gd", Polarity::Normal, |_| 0.0);
        set_group_delay(&mut flat, |_| 5.0);
        let mut hump = candidate("gd-hump", Polarity::Normal, |_| 0.0);
        set_group_delay(&mut hump, |frequency| {
            5.0 + 4.0 * (-0.5 * ((frequency / 80.0).log2() / 0.35).powi(2)).exp()
        });

        let report = rank_candidates(&[hump, flat], &RankingConfig::default()).unwrap();
        assert_eq!(report.rankings[0].id, "flat-gd");
        assert!(report.rankings[0].metrics.deficit_rms_db < 1.0e-12);
        assert!(
            report.rankings[1]
                .metrics
                .phase_irregularity_rms_ms
                .unwrap()
                > 0.5
        );
        assert!(report.rankings[1].metrics.total_score < 1.0);
    }

    #[test]
    fn multi_seat_penalty_rejects_a_p0_only_optimum() {
        // The bad seat carries a band-limited crossover notch, not a level
        // offset: level alone is playback volume and is normalized away.
        let mut overfit = candidate("p0-only", Polarity::Normal, |_| 0.0);
        overfit.positions.push(position("P1", |frequency| {
            broad_notch(frequency, 80.0, 12.0)
        }));
        let mut robust = candidate("robust", Polarity::Normal, |_| -1.0);
        robust.positions.push(position("P1", |_| -1.0));

        let report = rank_candidates(&[overfit, robust], &RankingConfig::default()).unwrap();
        assert_eq!(report.rankings[0].id, "robust");
        assert!(report.spatial_evidence_available);
        assert!(report.rankings[1].metrics.worst_seat_rms_db.unwrap() > 5.0);
    }

    #[test]
    fn missing_phase_timing_and_spatial_evidence_are_explicit() {
        let mut candidate = candidate("only", Polarity::Normal, |_| 0.0);
        candidate.positions[0].left_combined.phase_rad = None;
        candidate.positions[0].right_combined.timing = None;
        let report = rank_candidates(&[candidate], &RankingConfig::default()).unwrap();
        assert!(!report.phase_evidence_available);
        assert!(!report.timing_evidence_available);
        assert!(!report.spatial_evidence_available);
        assert_eq!(report.warnings.len(), 3);
        assert!(report.rankings[0]
            .metrics
            .phase_irregularity_rms_ms
            .is_none());
    }

    #[test]
    fn deterministic_ties_use_candidate_id() {
        let z = candidate("z", Polarity::Normal, |_| 0.0);
        let a = candidate("a", Polarity::Inverted, |_| 0.0);
        let report = rank_candidates(&[z, a], &RankingConfig::default()).unwrap();
        assert_eq!(report.rankings[0].id, "a");
        assert_eq!(report.rankings[1].id, "z");
        assert_eq!(
            report.rankings[0].metrics.total_score,
            report.rankings[1].metrics.total_score
        );
    }

    #[test]
    fn excessive_declared_delay_and_level_receive_regularization() {
        let modest = candidate("modest", Polarity::Normal, |_| 0.0);
        let mut excessive = candidate("excessive", Polarity::Normal, |_| 0.0);
        excessive.settings.main_delay_ms = Some(20.0);
        excessive.settings.sub_level_db = Some(12.0);

        let report = rank_candidates(&[excessive, modest], &RankingConfig::default()).unwrap();
        assert_eq!(report.rankings[0].id, "modest");
        assert!((report.rankings[1].metrics.delay_regularization_db - 0.4).abs() < 1.0e-12);
        assert!((report.rankings[1].metrics.level_regularization_db - 0.24).abs() < 1.0e-12);
        assert!(report.rankings[1].metrics.total_score > 0.64);
    }

    #[test]
    fn invalid_ids_grids_and_ranges_are_rejected() {
        let duplicate_a = candidate("same", Polarity::Normal, |_| 0.0);
        let duplicate_b = candidate("same", Polarity::Normal, |_| 0.0);
        assert!(
            rank_candidates(&[duplicate_a, duplicate_b], &RankingConfig::default())
                .unwrap_err()
                .to_string()
                .contains("duplicate candidate")
        );

        let mut bad_grid = candidate("bad-grid", Polarity::Normal, |_| 0.0);
        bad_grid.positions[0].right_combined.frequencies_hz[3] =
            bad_grid.positions[0].right_combined.frequencies_hz[2];
        assert!(rank_candidates(&[bad_grid], &RankingConfig::default())
            .unwrap_err()
            .to_string()
            .contains("strictly increasing"));

        let mut bad_range = candidate("bad-range", Polarity::Normal, |_| 0.0);
        bad_range.settings.crossover_hz = 0.0;
        assert!(rank_candidates(&[bad_range], &RankingConfig::default()).is_err());
    }

    #[test]
    fn separated_paths_distinguish_constructive_sum_and_cancellation() {
        let main = response_with(true, false, |_| 0.0);
        let sub = response_with(true, false, |_| 0.0);
        let constructive_db = 20.0 * 2.0_f64.log10();
        let constructive = response_with(false, false, |_| constructive_db);
        let good = analyze_separated_paths(&main, &sub, &constructive, 80.0).unwrap();
        assert!(good.complex_sum_magnitude_rmse_db < 1.0e-12);
        assert_eq!(good.cancellation_loss_worst_db, 0.0);

        let mut opposing_sub = sub;
        opposing_sub.phase_rad = Some(vec![PI; opposing_sub.frequencies_hz.len()]);
        let cancelled = response_with(false, false, |_| -40.0);
        let bad = analyze_separated_paths(&main, &opposing_sub, &cancelled, 80.0).unwrap();
        assert!(bad.cancellation_loss_rms_db > 39.0);
        assert!(bad.complex_sum_magnitude_rmse_db > 200.0);
    }

    #[test]
    fn anchor_drift_is_reported_and_normalized_out_of_scoring() {
        let low = candidate("low", Polarity::Normal, |_| 0.0);
        let high = candidate("high", Polarity::Normal, |_| 3.0);
        let report = rank_candidates(&[low, high], &RankingConfig::default()).unwrap();
        // The raw drift is still measured and reported as a volume-hygiene
        // warning, but a pure level offset no longer separates the scores:
        // both candidates normalize to the same shape.
        assert!((report.anchor_level_spread_db.unwrap() - 3.0).abs() < 1.0e-12);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("check Roon volume")));
        assert!(
            (report.rankings[0].metrics.total_score - report.rankings[1].metrics.total_score).abs()
                < 1.0e-9
        );
    }

    fn shaped_response<F>(magnitude: F, delay_ms: f64, inverted: bool) -> CombinedResponse
    where
        F: Fn(f64) -> f64,
    {
        let frequencies_hz = grid();
        let phase_rad = frequencies_hz
            .iter()
            .map(|frequency_hz| {
                -2.0 * PI * frequency_hz * delay_ms / 1_000.0 + if inverted { PI } else { 0.0 }
            })
            .collect::<Vec<_>>();
        CombinedResponse {
            magnitude_db: frequencies_hz
                .iter()
                .map(|frequency| magnitude(*frequency))
                .collect(),
            phase_rad: Some(phase_rad),
            timing: None,
            frequencies_hz,
        }
    }

    /// Physically shaped separated paths: the main rolls off 24 dB/oct below
    /// the crossover and the sub 24 dB/oct above it, so the 200-500 Hz anchor
    /// band is main-dominated and independent of sub alignment - matching the
    /// real systems the anchor normalization relies on.
    fn separated_crossover(
        id: &str,
        crossover_hz: f64,
        sub_delay_ms: f64,
    ) -> SeparatedCrossoverPaths {
        let main = move |frequency: f64| -24.0 * (crossover_hz / frequency).log2().max(0.0);
        let sub = move |frequency: f64| -24.0 * (frequency / crossover_hz).log2().max(0.0);
        SeparatedCrossoverPaths {
            id: id.into(),
            crossover_hz,
            left_main: shaped_response(main, 0.0, false),
            right_main: shaped_response(main, 0.0, false),
            sub: shaped_response(sub, sub_delay_ms, true),
        }
    }

    #[test]
    fn isolated_paths_find_measured_crossover_polarity_and_main_delay() {
        // xo80's sub offset (2 ms) is reachable inside the 0-4 ms search
        // grid; xo100's (6 ms) is not, so its best alignment keeps a residual
        // crossover notch. The winner must be decided by that band structure,
        // not by any overall level difference.
        let paths = [
            separated_crossover("xo80", 80.0, 2.0),
            separated_crossover("xo100", 100.0, 6.0),
        ];
        let report = optimize_separated_paths(
            &paths,
            &SeparatedPathOptimizationConfig {
                measured_main_delay_ms: 0.0,
                measured_polarity: Polarity::Normal,
                delay_minimum_ms: 0.0,
                delay_maximum_ms: 4.0,
                delay_step_ms: 1.0,
                ranking: RankingConfig::default(),
            },
        )
        .unwrap();
        let best = &report.ranking.rankings[0];
        // Adaptive windows: xo80's estimated arrival (~2 ms) opens a
        // half-period window clipped to [0, 4] -> five 1 ms steps; xo100's
        // (~6 ms) window clips to [1, 4] -> four steps. (5 + 4) * 2 = 18.
        assert_eq!(report.synthesized_candidate_count, 18);
        assert_eq!(best.settings.crossover_hz, 80.0);
        assert_eq!(best.settings.main_delay_ms, Some(2.0));
        assert_eq!(best.settings.polarity, Some(Polarity::Inverted));
        assert!(report.ranking.needs_confirmation);
        // The pre-search arrival estimate found the true sub offsets on the
        // shared timeline, and the unreachable xo100 window is flagged.
        let xo80 = &report.arrival_estimates[0];
        assert!((xo80.center_ms - 2.0).abs() <= 0.1, "{}", xo80.center_ms);
        assert!(xo80.left_right_spread_ms < 0.1);
        let xo100 = &report.arrival_estimates[1];
        assert!((xo100.center_ms - 6.0).abs() <= 0.1, "{}", xo100.center_ms);
        assert!(xo100.range_limited);
        assert!(report
            .ranking
            .warnings
            .iter()
            .any(|warning| warning.contains("hardware range")));
        assert!(report
            .ranking
            .warnings
            .iter()
            .any(|warning| warning.contains("no crossover transfer function")));
    }

    #[test]
    fn isolated_path_search_rejects_unsafe_or_underspecified_grids() {
        let one = [separated_crossover("xo80", 80.0, -6.0)];
        let config = SeparatedPathOptimizationConfig {
            measured_main_delay_ms: 0.0,
            measured_polarity: Polarity::Normal,
            delay_minimum_ms: 0.0,
            delay_maximum_ms: 1.0,
            delay_step_ms: 0.3,
            ranking: RankingConfig::default(),
        };
        assert!(optimize_separated_paths(&one, &config)
            .unwrap_err()
            .to_string()
            .contains("2 to 12"));

        // A range that is not an exact multiple of the step is no longer an
        // error: the grid snaps to absolute hardware-step multiples inside
        // the adaptive window instead, so every candidate stays dialable.
        let two = [
            separated_crossover("xo80", 80.0, -6.0),
            separated_crossover("xo90", 90.0, -6.0),
        ];
        let report = optimize_separated_paths(&two, &config).unwrap();
        for candidate in &report.ranking.rankings {
            let delay = candidate.settings.main_delay_ms.unwrap();
            let steps = delay / 0.3;
            assert!(
                (steps - steps.round()).abs() < 1.0e-6,
                "delay {delay} is not a hardware-step multiple"
            );
        }

        let bad_step = SeparatedPathOptimizationConfig {
            delay_step_ms: 0.001,
            ..config
        };
        assert!(optimize_separated_paths(&two, &bad_step)
            .unwrap_err()
            .to_string()
            .contains("between 0.01 and 5"));
    }
}
