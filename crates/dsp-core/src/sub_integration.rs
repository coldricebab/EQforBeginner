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
/// Fixed scoring band for the separated-path search (v5). v4 derived the
/// band from the LISTED candidates (0.5x the minimum to 2x the maximum
/// crossover), which made every candidate's score - including a candidate
/// present in both lists - depend on which other crossovers the user
/// happened to include (field data 2026-08-09: 20-240 Hz with a 40 Hz
/// candidate, 25-240 Hz without one, and the 50 Hz candidate's best delay
/// moved between runs). The product's validated correction band is 20-500 Hz
/// everywhere else; above twice any offered crossover both branches are
/// main-dominated and near-identical across candidates, so the fixed band
/// adds the same contribution to every candidate, preserving the ranking
/// order while making each candidate's score independent of the list.
const SUB_SCORING_BAND_LOW_HZ: f64 = 20.0;
const SUB_SCORING_BAND_HIGH_HZ: f64 = 500.0;
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
/// paths at each physically configured crossover. v4 adds one change on top
/// of v3 (none of v3/v4 has shipped in a release):
/// 4. Candidates are scored at deployment level, not capture level: each
///    path's sub is first level-matched into the mains' own 200-500 Hz
///    anchor over its crossover-adjacent octave [fc/2, fc] - the calibration
///    a user performs after choosing that crossover - and the applied trim
///    is part of the recommendation. Without this, the v3 anchor term was
///    still level-sensitive: a sub captured ~10 dB hotter than the mains
///    cleared the anchor everywhere it played (its own dips hidden by the
///    surplus) while the mains' room holes were charged in full, so handing
///    a wider band to the louder branch kept buying the ranking through the
///    anchor door and the recommendation tracked the top of any candidate
///    list (110 of 40-120, 100 of 40-100, 90 of 40-90 on real data). The
///    trim makes the whole objective invariant to the captured sub level.
///
/// The v3 changes:
/// 1. A step too fine for the per-window/total candidate caps - which v2
///    rejected outright - runs in two stages (a coarsened scan of every
///    window, then the full requested resolution around each crossover's
///    best).
/// 2. The objective is level-neutral. v2 charged each candidate its deficit
///    below the loudest candidate at each frequency; because a higher
///    crossover hands a wider band to the sub, any sub running hotter than
///    the mains made the score a monotonic function of the crossover (the
///    highest candidate always won, confirmed on real data). v3 scores each
///    candidate only against its own physics - the smoothed pointwise-louder
///    of its two branches (interference cannot be masked by turning either
///    branch up) and its own midband anchor (a hole the cut-only EQ cannot
///    legally fill). Excess level above the anchor is deliberately
///    uncharged (cuts remove it, headroom is accounted at export, and the
///    sub-level advisory reports the gain trim); group-delay smoothness is
///    reported but not scored (an LR crossover's own group-delay bump
///    scales as 1/fc and biased the comparison toward high crossovers); the
///    delay regularizer references the measured arrival rather than zero;
///    and a small documented per-octave regularizer prefers the lower
///    crossover among equivalent splices (localization practice, far below
///    any real splice defect).
/// 3. One sub-minus-main arrival anchors every delay window, estimated from
///    the lowest-crossover path over 20 Hz to twice its crossover. Only
///    that path's correlation is alias-free inside the +/-20 ms scan
///    (aliases space at ~1/band-centroid); a high-crossover path's own
///    band estimate can lock a full bass cycle away, which centred the
///    delay windows 12-15 ms off on real data. A plan whose lowest
///    candidate sits above 55 Hz gets a warning that its anchor is
///    ambiguous.
pub const SEPARATED_PATH_OPTIMIZATION_VERSION: &str =
    "phase3-separated-path-delay-polarity-search-v5";
/// Tie-breaking preference for lower crossovers, in score dB per octave above
/// 40 Hz. This encodes textbook practice - a single subwoofer becomes
/// localizable and collapses stereo bass as the handoff rises - not any
/// measured room. It is far below a real splice defect (>= 0.5 dB), so it
/// only decides between candidates whose measured integration is equivalent.
pub const SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE: f64 = 0.1;
/// Hard bound on the per-candidate deployment sub trim. A capture needing
/// more than this to level-match the sub into the mains' anchor indicates a
/// gain-staging problem, not a calibration choice; the trim is clamped and
/// the report says so instead of silently scoring an implausible deployment.
pub const DEPLOYMENT_SUB_TRIM_LIMIT_DB: f64 = 24.0;
/// Version for synthesizing per-crossover states from one wide-band sub
/// capture and full-range main captures using a declared filter model.
pub const WIDE_BAND_CROSSOVER_SYNTHESIS_VERSION: &str = "phase3-wide-band-crossover-synthesis-v1";

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

/// Analog-prototype crossover alignments a consumer bass-management stage
/// commonly applies. These exist only for the wide-band synthesis path; the
/// measured-states path never assumes any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossoverAlignment {
    /// 24 dB/oct, squared 2nd-order Butterworth. High-pass and low-pass are
    /// in phase at the crossover. The default in most consumer DSP bass
    /// management (confirmed LR4 on the WiiM Amp Ultra sub-out/speaker pair).
    LinkwitzRiley4,
    /// 12 dB/oct, two cascaded first-order sections (Q=0.5). High-pass and
    /// low-pass are 180 degrees apart at the crossover.
    LinkwitzRiley2,
    /// 12 dB/oct, 2nd-order Butterworth (Q=0.707), -3 dB at the corner.
    /// Common as the satellite high-pass in THX-style bass management.
    Butterworth2,
}

impl CrossoverAlignment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinkwitzRiley4 => "LR4 24 dB/oct",
            Self::LinkwitzRiley2 => "LR2 12 dB/oct",
            Self::Butterworth2 => "BW2 12 dB/oct",
        }
    }
}

/// The three wide-band isolated captures the synthesis mode starts from:
/// both mains captured full range (bass management off, so no high-pass) and
/// one sub captured with the bass-management low-pass dialed to its maximum
/// (or bypassed where the hardware allows it).
#[derive(Debug, Clone, PartialEq)]
pub struct WideBandIsolatedPaths {
    pub left_main: CombinedResponse,
    pub right_main: CombinedResponse,
    pub sub: CombinedResponse,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WideBandSynthesisConfig {
    /// Crossover candidates to synthesize, each a value the user can actually
    /// dial into the hardware. Two to twelve strictly increasing values.
    pub candidate_crossovers_hz: Vec<f64>,
    /// The bass-management low-pass corner physically active while the sub
    /// was captured (the dial at its maximum). `None` means the low-pass was
    /// genuinely bypassed for the capture. When `Some`, the synthesis divides
    /// this modeled filter out while multiplying the candidate filter in, so
    /// the deployed state is modeled instead of a double-filtered one; with
    /// the same alignment and a candidate at or below this corner the
    /// replacement ratio never exceeds unity at any frequency, so measurement
    /// noise is never amplified.
    pub sub_measured_low_pass_hz: Option<f64>,
    /// Model applied to the mains for each candidate (the speaker high-pass
    /// bass management engages when a sub output is active).
    pub main_high_pass: CrossoverAlignment,
    /// Model applied to the sub for each candidate.
    pub sub_low_pass: CrossoverAlignment,
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
    /// `None` when every `SeparatedCrossoverPaths` entry was physically
    /// measured at its declared crossover (the model-free path). `Some`
    /// carries a human-readable description of the filter model when the
    /// entries were synthesized by `synthesize_wide_band_crossover_states`,
    /// so the report warns about the model dependence instead of falsely
    /// claiming no crossover transfer function was synthesized.
    pub synthesized_crossover_model: Option<String>,
    /// Score dB per octave above 40 Hz added to each candidate; see
    /// `SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE`.
    pub crossover_regularization_db_per_octave: f64,
    /// The raw (unfiltered) isolated captures the candidate states were
    /// synthesized from, when they exist (wide-band mode). The sub-minus-main
    /// arrival is a property of the drivers and placement, not of the
    /// candidate list, and v4 estimated it from the lowest-crossover
    /// CANDIDATE path - which made the whole search depend on which
    /// crossovers the user listed. Field data (2026-08-09, same captures):
    /// with a 40 Hz candidate present the anchor read -1.2 ms and the search
    /// chose 70 Hz / 0.38 ms; with the list starting at 50 Hz the fallback
    /// band widened to 20-100 Hz, a correlation alias one bass cycle away
    /// (+11.15 ms) beat the true peak, and every candidate's delay window -
    /// including 70's own - shifted a cycle (winner 80 Hz / 11.35 ms).
    /// `Some` anchors the arrival on these raw curves instead, as the
    /// median over a fixed band family with edge-degenerate members
    /// rejected (see `ARRIVAL_REFERENCE_BANDS_HZ`) - set-independent by
    /// construction and robust to a room whose low band is phase-incoherent
    /// on one channel. `None` (measured mode, where no unfiltered capture
    /// exists) keeps the lowest-path fallback and the report warns when the
    /// lowest crossover is high enough for the ambiguity to fit the scan.
    pub arrival_reference: Option<WideBandIsolatedPaths>,
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
    /// The deployment sub-level change the winner was scored at, with the
    /// deficit the same candidate would keep at the captured level for
    /// contrast. Since v4 this is part of the recommendation (apply it with
    /// the crossover/delay/polarity), not a free gain scan: the v3-era
    /// +/-6 dB rescan rewarded raw level through the anchor door the trim
    /// closes and pegged at its cap on hot-sub captures.
    pub sub_level_advisory: Option<SubLevelAdvisory>,
    pub ranking: SubIntegrationReport,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubLevelAdvisory {
    /// dB change to apply to the sub, relative to the level physically
    /// active during the isolated captures (`fixed_sub_level_db` upstream).
    pub best_gain_db: f64,
    /// The winner's scored deficit RMS (at the applied trim).
    pub deficit_rms_at_best_db: f64,
    /// The same crossover/delay/polarity re-scored at the captured sub
    /// level (trim 0).
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

/// Deployment sub-level trim for every path, and the trimmed paths scoring
/// runs on.
///
/// Each path's sub is shifted so that its smoothed weighted-median level over
/// the crossover-adjacent octave [fc/2, fc] sits at the mains' own 200-500 Hz
/// anchor (mean of both channels) - the level a user calibrates the sub to
/// after choosing that crossover. Scoring at this level is what keeps the
/// objective level-neutral end to end: at capture level, a sub running hotter
/// than the mains clears the midband anchor everywhere it plays (its own dips
/// hidden behind the surplus) while the mains' room holes are charged in
/// full, so the anchor's hole-charging silently rewards handing the widest
/// band to the loudest branch. The trim is a reported deployment setting, not
/// a search dimension, and a pure gain change on the captured sub cancels out
/// of it exactly, which makes the final ranking invariant to the captured sub
/// level. When a path cannot be matched (mains without 200-500 Hz coverage,
/// or a grid that cannot resolve [fc/2, fc]), its trim falls back to 0 dB
/// with a warning, matching the scorer's own anchor fallback.
fn deployment_trimmed_paths(
    paths: &[SeparatedCrossoverPaths],
    ranking: &RankingConfig,
) -> DspResult<(Vec<SeparatedCrossoverPaths>, Vec<f64>, Vec<String>)> {
    let mut trimmed = Vec::with_capacity(paths.len());
    let mut trims_db = Vec::with_capacity(paths.len());
    let mut warnings = Vec::new();
    for path in paths {
        let grid = &path.sub.frequencies_hz;
        let anchors = [
            response_anchor_db(&path.left_main.frequencies_hz, &path.left_main.magnitude_db)?,
            response_anchor_db(
                &path.right_main.frequencies_hz,
                &path.right_main.magnitude_db,
            )?,
        ];
        let band_indices: Vec<usize> = grid
            .iter()
            .enumerate()
            .filter_map(|(index, frequency)| {
                (*frequency >= 0.5 * path.crossover_hz && *frequency <= path.crossover_hz)
                    .then_some(index)
            })
            .collect();
        let trim_db = match (
            anchors[0],
            anchors[1],
            band_indices.len() >= MIN_SCORING_BINS,
        ) {
            (Some(left_anchor), Some(right_anchor), true) => {
                let band_frequencies: Vec<f64> =
                    band_indices.iter().map(|&index| grid[index]).collect();
                let weights = log_frequency_weights(&band_frequencies)?;
                let smoothed = gaussian_log_smooth_at(
                    grid,
                    &path.sub.magnitude_db,
                    &band_indices,
                    ranking.magnitude_smoothing_octaves,
                )?;
                let sub_level_db = weighted_percentile(&smoothed, &weights, 0.5)?;
                let raw_trim = 0.5 * (left_anchor + right_anchor) - sub_level_db;
                if raw_trim.abs() > DEPLOYMENT_SUB_TRIM_LIMIT_DB {
                    warnings.push(format!(
                        "{}: level-matching the sub into the mains' anchor needs {raw_trim:+.1} dB; \
                         the trim is clamped to {:+.1} dB - check the capture gain staging before \
                         trusting these scores.",
                        path.id,
                        raw_trim.signum() * DEPLOYMENT_SUB_TRIM_LIMIT_DB,
                    ));
                }
                raw_trim.clamp(-DEPLOYMENT_SUB_TRIM_LIMIT_DB, DEPLOYMENT_SUB_TRIM_LIMIT_DB)
            }
            _ => {
                warnings.push(format!(
                    "{}: the sub could not be level-matched (missing 200-500 Hz mains coverage or \
                     too few bins in [{:.0}, {:.0}] Hz); its candidates are scored at the captured \
                     sub level.",
                    path.id,
                    0.5 * path.crossover_hz,
                    path.crossover_hz,
                ));
                0.0
            }
        };
        let mut path = path.clone();
        for value in &mut path.sub.magnitude_db {
            *value += trim_db;
        }
        trims_db.push(trim_db);
        trimmed.push(path);
    }
    Ok((trimmed, trims_db, warnings))
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
    // Deployment level first: every downstream stage - arrival estimation,
    // candidate synthesis, branch references, the anchor - sees the sub at
    // the level the user will actually calibrate it to, and the applied trim
    // ships with the recommendation.
    let captured_paths = paths;
    let (trimmed_paths, path_trims_db, trim_warnings) =
        deployment_trimmed_paths(paths, &config.ranking)?;
    let paths = &trimmed_paths[..];

    let common_grid = &paths[0].left_main.frequencies_hz;
    // Fixed product band (v5): deriving the low edge from the listed
    // candidates made the synthesized sums - and with them every score -
    // depend on the candidate list.
    let retained_indices = common_grid
        .iter()
        .enumerate()
        .filter_map(|(index, frequency_hz)| {
            (*frequency_hz >= SUB_SCORING_BAND_LOW_HZ && *frequency_hz <= SUB_SCORING_BAND_HIGH_HZ)
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
    // One arrival per channel for the whole search. The true sub-minus-main
    // offset is a property of the drivers and placement, not of the
    // crossover dial, so every path shares it - and (v5) it must therefore
    // never depend on which crossovers the user happened to list.
    //
    // Preferred anchor: the raw wide-band captures the states were
    // synthesized from, over the fixed 20-45 Hz band. The unfiltered mains
    // still carry full content there and a <= 45 Hz carrier puts the
    // correlation's cycle ambiguity at >= 22 ms, outside the +/-20 ms scan -
    // unambiguous and candidate-set-independent by construction.
    //
    // Fallback (measured mode, no raw captures): the lowest-crossover
    // candidate path, measured over its own filtered overlap band. A band
    // centred on frequency f has correlation aliases spaced at roughly 1/f;
    // at a 40 Hz lowest candidate that spacing (~25 ms) falls outside the
    // scan, but from ~50 Hz up an alias fits inside it and can beat the true
    // peak (field data 2026-08-09: the same captures read -1.2 ms with a
    // 40 Hz candidate present and +11.15 ms - one 81 Hz cycle away - without
    // one, flipping the winner from 70 Hz / 0.38 ms to 80 Hz / 11.35 ms).
    // That ambiguity is warned about below. Precision beyond a couple of
    // milliseconds is not required here: the center only places the
    // half-period search window, and the splice scoring picks the exact
    // delay inside it.
    let anchor_path = paths
        .iter()
        .min_by(|left, right| left.crossover_hz.total_cmp(&right.crossover_hz))
        .expect("validated: at least two paths");
    let mut arrival_warnings: Vec<String> = Vec::new();
    let scan_last_index = (2.0 * ARRIVAL_SCAN_LIMIT_MS / ARRIVAL_SCAN_STEP_MS).round() as usize;
    // Correlation peak for one channel/band; `true` marks a peak pinned to a
    // scan edge (a degenerate correlation, not an arrival).
    let correlation_peak = |main: &CombinedResponse,
                            sub: &CombinedResponse,
                            low_hz: f64,
                            high_hz: f64|
     -> DspResult<(f64, bool)> {
        let curve = arrival_correlation_curve(common_grid, main, sub, low_hz, high_hz)?;
        let best_index = curve
            .iter()
            .enumerate()
            .max_by(|left, right| left.1.total_cmp(right.1))
            .map(|(index, _)| index)
            .expect("scan grid is nonempty");
        Ok((
            -ARRIVAL_SCAN_LIMIT_MS + best_index as f64 * ARRIVAL_SCAN_STEP_MS,
            best_index == 0 || best_index >= scan_last_index,
        ))
    };
    let median_ms = |values: &mut Vec<f64>| -> f64 {
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        if values.len() % 2 == 0 {
            0.5 * (values[middle - 1] + values[middle])
        } else {
            values[middle]
        }
    };
    // Per-channel arrival estimates. With a raw reference: the median over
    // the fixed band family, per channel, after rejecting edge-degenerate
    // members (see ARRIVAL_REFERENCE_BANDS_HZ). Fallback: the lowest
    // candidate path over its own filtered band, as measured mode requires.
    let mut reference_arrivals: Option<[f64; 2]> = None;
    if let Some(reference) = &config.arrival_reference {
        let mut per_channel: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
        let mut pooled: Vec<f64> = Vec::new();
        for (channel_index, main) in [&reference.left_main, &reference.right_main]
            .into_iter()
            .enumerate()
        {
            for (low_hz, high_hz) in ARRIVAL_REFERENCE_BANDS_HZ {
                if let Ok((tau_ms, at_edge)) =
                    correlation_peak(main, &reference.sub, low_hz, high_hz)
                {
                    if !at_edge {
                        per_channel[channel_index].push(tau_ms);
                        pooled.push(tau_ms);
                    }
                }
            }
        }
        if pooled.len() >= 2 {
            let pooled_median = median_ms(&mut pooled);
            let mut arrivals = [pooled_median; 2];
            for (channel_index, estimates) in per_channel.iter_mut().enumerate() {
                if !estimates.is_empty() {
                    arrivals[channel_index] = median_ms(estimates);
                }
            }
            reference_arrivals = Some(arrivals);
        } else {
            arrival_warnings.push(
                "The raw-capture arrival anchor was degenerate in every band (correlation \
                 peaks pinned to the scan edge); the anchor fell back to the lowest \
                 crossover candidate's path - treat the recommended delay with caution."
                    .to_string(),
            );
        }
    }
    let channel_arrivals_ms = match reference_arrivals {
        Some(arrivals) => arrivals,
        None => {
            if config.arrival_reference.is_none()
                && 1_000.0 / anchor_path.crossover_hz <= ARRIVAL_SCAN_LIMIT_MS
            {
                arrival_warnings.push(format!(
                    "The sub-arrival anchor was estimated from the lowest measured \
                     crossover ({:.0} Hz); at that band a correlation alias one bass \
                     cycle away fits inside the +/-{ARRIVAL_SCAN_LIMIT_MS:.0} ms scan \
                     and the anchor - and with it every candidate's delay window - can \
                     lock a cycle off. Include one candidate at or below 40 Hz (or use \
                     the wide-band mode) for an unambiguous anchor.",
                    anchor_path.crossover_hz
                ));
            }
            let band_high_hz = (2.0 * anchor_path.crossover_hz).min(240.0);
            let mut arrivals = [0.0_f64; 2];
            for (channel_index, main) in [&anchor_path.left_main, &anchor_path.right_main]
                .into_iter()
                .enumerate()
            {
                let (tau_ms, _) = correlation_peak(main, &anchor_path.sub, 20.0, band_high_hz)?;
                arrivals[channel_index] = tau_ms;
            }
            arrivals
        }
    };
    let center_ms = 0.5 * (channel_arrivals_ms[0] + channel_arrivals_ms[1]);
    let left_right_spread_ms = (channel_arrivals_ms[0] - channel_arrivals_ms[1]).abs();
    let mut arrival_estimates = Vec::with_capacity(paths.len());
    for path in paths {
        let half_period_ms = 1_000.0 / (2.0 * path.crossover_hz);
        let window_low_ms = (center_ms - half_period_ms).max(config.delay_minimum_ms);
        let window_high_ms = (center_ms + half_period_ms).min(config.delay_maximum_ms);
        let range_limited = center_ms - half_period_ms < config.delay_minimum_ms - 1.0e-9
            || center_ms + half_period_ms > config.delay_maximum_ms + 1.0e-9;
        arrival_estimates.push(CrossoverArrivalEstimate {
            id: path.id.clone(),
            crossover_hz: path.crossover_hz,
            left_arrival_ms: channel_arrivals_ms[0],
            right_arrival_ms: channel_arrivals_ms[1],
            center_ms,
            left_right_spread_ms,
            window_low_ms,
            window_high_ms,
            range_limited,
        });
    }

    // Stage-1 grid granularity, PER PATH (v5): each path's coarse step is
    // the smallest integer multiple of the hardware step that fits its own
    // arrival window under the 1,001-values-per-window cap. v4 chose one
    // multiplier from the widest window in the SET (to also meet a shared
    // 10,000-candidate total), which made every path's coarse grid - and,
    // through stage 2's refinement window of one coarse step around the
    // coarse best, occasionally the final fine-step choice - depend on which
    // other crossovers were listed. A path's grid now depends on its own
    // window alone; the total stays bounded by paths (<= 12) x 1,001 values
    // x 2 polarities. Coarse multiples of the step remain values the user
    // can dial, and the score varies over delay on a scale of roughly one
    // period of the highest scored frequency (>= ~2 ms at the 500 Hz band
    // edge), hundreds of times any coarse step chosen here, so the coarse
    // scan cannot skip past the optimum's basin.
    let overflowed = || DspError::InvalidArgument("separated-path search size overflowed".into());
    let mut path_grids = Vec::with_capacity(arrival_estimates.len());
    let mut path_coarse_steps_ms = Vec::with_capacity(arrival_estimates.len());
    let mut candidate_count = 0_usize;
    for estimate in &arrival_estimates {
        let span_ms = estimate.window_high_ms - estimate.window_low_ms;
        let mut multiplier = ((span_ms / (1_000.0 * config.delay_step_ms)).ceil() as usize).max(1);
        let (grid, coarse_step_ms) = loop {
            let coarse_step_ms = multiplier as f64 * config.delay_step_ms;
            // `snapped_delay_grid` fails above 1,001 values per window; that
            // simply means this granularity is still too fine.
            match snapped_delay_grid(
                estimate.window_low_ms,
                estimate.window_high_ms,
                coarse_step_ms,
            ) {
                Ok(grid) => break (grid, coarse_step_ms),
                Err(_) => multiplier = multiplier.checked_add(1).ok_or_else(overflowed)?,
            }
        };
        candidate_count = candidate_count
            .checked_add(grid.len().checked_mul(2).ok_or_else(overflowed)?)
            .ok_or_else(overflowed)?;
        path_grids.push(grid);
        path_coarse_steps_ms.push(coarse_step_ms);
    }

    let synthesize_grid_candidates =
        |grids: &[Vec<f64>]| -> DspResult<Vec<SubIntegrationCandidate>> {
            let mut candidates = Vec::new();
            for ((path, delays), trim_db) in paths.iter().zip(grids).zip(&path_trims_db) {
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
                                // The deployment trim this candidate was
                                // scored at - part of the recommendation.
                                sub_level_db: Some(*trim_db),
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
            Ok(candidates)
        };
    let mut ranking = rank_spliced_candidates(
        paths,
        common_grid,
        &synthesize_grid_candidates(&path_grids)?,
        center_ms,
        config,
    )?;

    // Stage 2: when stage 1 had to coarsen, re-rank at the full requested
    // resolution inside one coarse step either side of every crossover's
    // stage-1 optimum (clipped to its arrival window). Each fine window
    // contains its own coarse winner, so the final ranking can only match or
    // improve on stage 1, and every candidate stays a hardware-step multiple.
    let coarsest_step_ms = path_coarse_steps_ms
        .iter()
        .copied()
        .fold(config.delay_step_ms, f64::max);
    if coarsest_step_ms > config.delay_step_ms + 1.0e-12 {
        let mut fine_grids = Vec::with_capacity(paths.len());
        for ((path, estimate), path_coarse_step_ms) in paths
            .iter()
            .zip(&arrival_estimates)
            .zip(&path_coarse_steps_ms)
        {
            let best_delay_ms = ranking
                .rankings
                .iter()
                .find(|candidate| candidate.settings.crossover_hz == path.crossover_hz)
                .and_then(|candidate| candidate.settings.main_delay_ms)
                .ok_or_else(|| {
                    DspError::InvalidArgument("stage-one ranking lost a crossover candidate".into())
                })?;
            let refine_low_ms = (best_delay_ms - path_coarse_step_ms).max(estimate.window_low_ms);
            let refine_high_ms = (best_delay_ms + path_coarse_step_ms).min(estimate.window_high_ms);
            fine_grids.push(snapped_delay_grid(
                refine_low_ms,
                refine_high_ms,
                config.delay_step_ms,
            )?);
        }
        let fine_candidates = synthesize_grid_candidates(&fine_grids)?;
        candidate_count = candidate_count
            .checked_add(fine_candidates.len())
            .ok_or_else(overflowed)?;
        ranking = rank_spliced_candidates(paths, common_grid, &fine_candidates, center_ms, config)?;
        ranking.warnings.push(format!(
            "A {:.2} ms delay step over these arrival windows exceeds the per-window candidate \
             limit, so the search ran in two stages: a coarse scan of every window (per-window \
             steps up to {coarsest_step_ms:.2} ms), then the full {:.2} ms resolution within one \
             coarse step of each crossover's best delay. Both stages contain only hardware-step \
             multiples.",
            config.delay_step_ms, config.delay_step_ms,
        ));
    }
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
    match &config.synthesized_crossover_model {
        None => ranking.warnings.push(
            "Crossover entries are separately measured hardware states; no crossover transfer function was synthesized."
                .into(),
        ),
        Some(model) => ranking.warnings.push(format!(
            "Crossover states were synthesized from a filter model ({model}), not measured per \
             state; if the hardware's real slopes differ (especially 12 vs 24 dB/oct, which \
             flips the relative phase at the crossover), the delay and polarity recommendation \
             can be wrong. The measured combined confirmation remains the judge."
        )),
    }
    ranking.warnings.push(
        "The mono sub-only path is reused for L and R prediction; symmetric bass-management routing must be confirmed."
            .into(),
    );
    ranking.warnings.push(
        "Every result is a complex-sum prediction and requires new measured L+sub/R+sub confirmation at the selected setting."
            .into(),
    );
    ranking.warnings.push(format!(
        "Each candidate was scored with its sub level-matched into the mains' 200-500 Hz anchor \
         (the calibration applied at deployment): {}. Apply the winning entry's sub-level change \
         together with its crossover, delay, and polarity.",
        paths
            .iter()
            .zip(&path_trims_db)
            .map(|(path, trim_db)| format!("{} {trim_db:+.1} dB", path.id))
            .collect::<Vec<_>>()
            .join(", "),
    ));
    ranking.warnings.extend(trim_warnings);
    ranking.warnings.extend(arrival_warnings);
    // Sub-level advisory: the deployment trim the winner was scored at, plus
    // the deficit the same candidate keeps at the captured sub level, so the
    // report shows what applying the level change is worth. Nothing rescans
    // gain as a free dimension: the earlier +/-6 dB rescan rewarded raw
    // level through the anchor door the trim closes, and pegged at its cap
    // on hot-sub captures.
    let sub_level_advisory = ranking.rankings.first().and_then(|best| {
        let path_index = paths
            .iter()
            .position(|path| path.crossover_hz == best.settings.crossover_hz)?;
        let delay_ms = best.settings.main_delay_ms?;
        let polarity = best.settings.polarity?;
        let captured_path = &captured_paths[path_index];
        let left_combined = synthesize_isolated_sum(
            &captured_path.left_main,
            &captured_path.sub,
            &retained_indices,
            delay_ms - config.measured_main_delay_ms,
            polarity != config.measured_polarity,
        )
        .ok()?;
        let right_combined = synthesize_isolated_sum(
            &captured_path.right_main,
            &captured_path.sub,
            &retained_indices,
            delay_ms - config.measured_main_delay_ms,
            polarity != config.measured_polarity,
        )
        .ok()?;
        let at_captured_level = SubIntegrationCandidate {
            id: "winner-at-captured-sub-level".into(),
            settings: CandidateSettings {
                crossover_hz: best.settings.crossover_hz,
                main_delay_ms: Some(delay_ms),
                sub_level_db: Some(0.0),
                polarity: Some(polarity),
            },
            positions: vec![PositionObservation {
                id: "P0".into(),
                weight: 2.0,
                left_combined,
                right_combined,
            }],
        };
        let captured_ranking = rank_spliced_candidates(
            captured_paths,
            common_grid,
            &[at_captured_level],
            center_ms,
            config,
        )
        .ok()?;
        Some(SubLevelAdvisory {
            best_gain_db: path_trims_db[path_index],
            deficit_rms_at_best_db: best.metrics.deficit_rms_db,
            deficit_rms_at_zero_db: captured_ranking.rankings[0].metrics.deficit_rms_db,
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

/// Level-neutral scoring of synthesized splice candidates (search v3).
///
/// Every candidate derives from the same captures, so cross-candidate level
/// drift is impossible and the measured-mode envelope objective is both
/// unnecessary and biased here (a hotter branch buys the envelope). Each
/// candidate is instead charged, per frequency across the common band,
///
/// ```text
/// deficit(f) = max(0, max(main_s(f), sub_s(f), A) - sum_s(f))
/// ```
///
/// where `sum_s` is the candidate's smoothed complex-sum level, the branch
/// reference is `smooth(max(main_raw, sub_raw))` - the pointwise-louder
/// branch smoothed *after* the max, so a legitimate level step between
/// branches cannot leave a smoothing shoulder that reads as a defect (an
/// aligned sum is never below the louder branch bin by bin, and smoothing
/// both sides identically preserves that) - and `A` is the candidate's own
/// raw 200-500 Hz median. The branch reference makes destructive
/// interference visible even when one branch is much louder (the sum falling
/// below the louder branch is the definition of cancellation), and the
/// anchor charges a hole the cut-only EQ cannot legally fill even when the
/// branches are too weak there to show it. Being *above* the reference earns
/// nothing, so no candidate can buy the ranking with bandwidth or gain.
///
/// The caller must pass paths whose sub is already at deployment level
/// (`deployment_trimmed_paths`): the anchor comparison is only meaningful at
/// the level the system will actually play, and at capture level a hot sub
/// clears the anchor everywhere it plays, hiding its own dips.
fn rank_spliced_candidates(
    paths: &[SeparatedCrossoverPaths],
    common_grid: &[f64],
    candidates: &[SubIntegrationCandidate],
    arrival_center_ms: f64,
    config: &SeparatedPathOptimizationConfig,
) -> DspResult<SubIntegrationReport> {
    if candidates.is_empty() {
        return Err(DspError::EmptyInput("sub-integration candidates"));
    }
    let sum_grid = &candidates[0].positions[0].left_combined.frequencies_hz;
    let (band_indices, band) =
        scoring_band(sum_grid, SUB_SCORING_BAND_LOW_HZ, SUB_SCORING_BAND_HIGH_HZ)?;
    let band_frequencies: Vec<f64> = band_indices.iter().map(|&index| sum_grid[index]).collect();
    let frequency_weights = log_frequency_weights(&band_frequencies)?;
    // The branch responses live on the full common grid while the sums live
    // on the retained band grid; the scoring band is inside both, so the two
    // index sets address the same physical frequencies.
    let (branch_band_indices, _) = scoring_band(
        common_grid,
        SUB_SCORING_BAND_LOW_HZ,
        SUB_SCORING_BAND_HIGH_HZ,
    )?;
    if branch_band_indices.len() != band_indices.len() {
        return Err(DspError::ShapeMismatch(
            "branch and sum scoring bands must cover the same bins".into(),
        ));
    }
    let branch_reference: Vec<[Vec<f64>; 2]> = paths
        .iter()
        .map(|path| {
            let mut per_channel: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
            for (channel_index, main) in [&path.left_main, &path.right_main].into_iter().enumerate()
            {
                let louder_branch_db: Vec<f64> = main
                    .magnitude_db
                    .iter()
                    .zip(&path.sub.magnitude_db)
                    .map(|(main_db, sub_db)| main_db.max(*sub_db))
                    .collect();
                per_channel[channel_index] = gaussian_log_smooth_at(
                    common_grid,
                    &louder_branch_db,
                    &branch_band_indices,
                    config.ranking.magnitude_smoothing_octaves,
                )?;
            }
            Ok(per_channel)
        })
        .collect::<DspResult<_>>()?;

    let mut rankings = Vec::with_capacity(candidates.len());
    let mut anchors: Vec<f64> = Vec::new();
    let mut missing_anchor = false;
    for candidate in candidates {
        let path_index = paths
            .iter()
            .position(|path| path.crossover_hz == candidate.settings.crossover_hz)
            .ok_or_else(|| {
                DspError::InvalidArgument(
                    "candidate crossover does not match a measured path".into(),
                )
            })?;
        let position = &candidate.positions[0];
        let mut observations = Vec::with_capacity(2);
        let mut all_deficits = Vec::with_capacity(2 * band_indices.len());
        let mut all_weights = Vec::with_capacity(2 * band_indices.len());
        let mut phase_values = Vec::with_capacity(2);
        for (channel_index, (channel, response)) in [
            (Channel::Left, &position.left_combined),
            (Channel::Right, &position.right_combined),
        ]
        .into_iter()
        .enumerate()
        {
            let sum_smoothed = gaussian_log_smooth_at(
                sum_grid,
                &response.magnitude_db,
                &band_indices,
                config.ranking.magnitude_smoothing_octaves,
            )?;
            let anchor_db = response_anchor_db(sum_grid, &response.magnitude_db)?;
            match anchor_db {
                Some(anchor) => anchors.push(anchor),
                None => missing_anchor = true,
            }
            let reference_smoothed = &branch_reference[path_index][channel_index];
            let deficits: Vec<f64> = (0..band_indices.len())
                .map(|bin| {
                    let mut reference = reference_smoothed[bin];
                    if let Some(anchor) = anchor_db {
                        reference = reference.max(anchor);
                    }
                    (reference - sum_smoothed[bin]).max(0.0)
                })
                .collect();
            let phase = response
                .phase_rad
                .as_ref()
                .expect("synthesized sums carry phase");
            phase_values.push(group_delay_irregularity_rms_ms(
                sum_grid,
                phase,
                &band_indices,
                &frequency_weights,
                config.ranking.group_delay_smoothing_octaves,
            )?);
            observations.push(ObservationDeficitMetrics {
                position_id: position.id.clone(),
                channel,
                rms_db: weighted_rms(&deficits, &frequency_weights)?,
                p95_db: weighted_percentile(&deficits, &frequency_weights, 0.95)?,
                worst_db: deficits.iter().copied().fold(0.0_f64, f64::max),
            });
            all_deficits.extend_from_slice(&deficits);
            all_weights.extend_from_slice(&frequency_weights);
        }
        let deficit_rms_db = weighted_rms(&all_deficits, &all_weights)?;
        let deficit_p95_db = weighted_percentile(&all_deficits, &all_weights, 0.95)?;
        let deficit_worst_db = all_deficits.iter().copied().fold(0.0_f64, f64::max);
        // Group-delay smoothness is reported as a diagnostic but deliberately
        // NOT scored: an LR crossover's own group-delay bump scales as 1/fc,
        // so a millisecond-unit smoothness term structurally favors high
        // crossovers (and under a much louder branch it can prefer a
        // misaligned delay whose accidental phase curve is smoother than the
        // aligned one). The interference this term was meant to catch is
        // already charged by the branch-reference magnitude deficit exactly
        // where the branches actually overlap.
        let phase_irregularity_rms_ms = weighted_rms(&phase_values, &[1.0, 1.0])?;
        // Regularize toward the measured arrival, not toward zero: the
        // measured offset is the physically privileged delay, and deviating
        // from it should need magnitude evidence. A zero-referenced penalty
        // would drag the winner below the true alignment whenever the
        // deficit slope is shallow (e.g. a much louder branch).
        let delay_regularization_db = candidate.settings.main_delay_ms.map_or(0.0, |delay| {
            (delay - arrival_center_ms).abs() * config.ranking.delay_regularization_db_per_ms
        });
        let crossover_regularization_db = config.crossover_regularization_db_per_octave
            * (candidate.settings.crossover_hz / 40.0).log2();
        let total_score = deficit_rms_db
            + config.ranking.deficit_p95_weight * deficit_p95_db
            + config.ranking.deficit_worst_weight * deficit_worst_db
            + delay_regularization_db
            + crossover_regularization_db;
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
                worst_seat_rms_db: None,
                spatial_spread_rms_db: None,
                phase_irregularity_rms_ms: Some(phase_irregularity_rms_ms),
                timing_repeatability_rms_ms: None,
                delay_regularization_db,
                level_regularization_db: 0.0,
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
    let anchor_level_spread_db = (!anchors.is_empty()).then(|| {
        anchors.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - anchors.iter().copied().fold(f64::INFINITY, f64::min)
    });
    let mut warnings = vec![
        "Only one listening position is available; spatial overfit penalties are unavailable and confidence is limited."
            .to_string(),
        "At least one response lacks repeatable arrival-time evidence; the timing reliability term is unavailable."
            .to_string(),
    ];
    if missing_anchor {
        warnings.push(
            "The common grid does not contain enough 200-500 Hz bins to check anchor-level stability."
                .into(),
        );
    }
    Ok(SubIntegrationReport {
        band,
        rankings,
        spatial_evidence_available: false,
        phase_evidence_available: true,
        timing_evidence_available: false,
        anchor_level_spread_db,
        needs_confirmation: true,
        warnings,
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
    if !config.crossover_regularization_db_per_octave.is_finite()
        || config.crossover_regularization_db_per_octave < 0.0
    {
        return Err(DspError::InvalidArgument(
            "crossover regularization must be finite and nonnegative".into(),
        ));
    }
    if let Some(reference) = &config.arrival_reference {
        let common_grid = &paths[0].left_main.frequencies_hz;
        for (label, response) in [
            ("arrival-reference left main", &reference.left_main),
            ("arrival-reference right main", &reference.right_main),
            ("arrival-reference sub", &reference.sub),
        ] {
            validate_response(response, label)?;
            require_same_grid(common_grid, &response.frequencies_hz, label)?;
            if response.phase_rad.is_none() {
                return Err(DspError::InvalidArgument(format!(
                    "{label} phase is required for the arrival anchor"
                )));
            }
        }
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

/// Synthesize per-crossover isolated states from one wide-band sub capture
/// and full-range main captures, for `optimize_separated_paths`.
///
/// For a candidate crossover `x` the virtual state is
///
/// ```text
/// main'_c(f) = main_c(f) * HP_model(x, f)
/// sub'(f)    = sub(f)    * LP_model(x, f) / LP_model(f_meas, f)   (measured dial at f_meas)
/// sub'(f)    = sub(f)    * LP_model(x, f)                          (low-pass bypassed)
/// ```
///
/// This is the explicit model-based counterpart of the measured-states path:
/// the room, drivers, and placement stay measured complex data, and only the
/// bass-management filters are modeled. The division replaces the wide
/// measurement-state low-pass instead of stacking a second filter on top of
/// it; without it the deployed sub would be modeled with an extra ~1.8 ms of
/// low-frequency group delay (LR4 at 250 Hz), which would bias the delay
/// recommendation by about that much. The caller must label the resulting
/// report via `SeparatedPathOptimizationConfig::synthesized_crossover_model`.
pub fn synthesize_wide_band_crossover_states(
    paths: &WideBandIsolatedPaths,
    config: &WideBandSynthesisConfig,
) -> DspResult<Vec<SeparatedCrossoverPaths>> {
    if !(2..=12).contains(&config.candidate_crossovers_hz.len()) {
        return Err(DspError::InvalidArgument(
            "wide-band synthesis requires 2 to 12 candidate crossovers".into(),
        ));
    }
    let mut previous = None;
    for crossover_hz in &config.candidate_crossovers_hz {
        validate_crossover(*crossover_hz)?;
        if previous.is_some_and(|value| *crossover_hz <= value) {
            return Err(DspError::InvalidArgument(
                "candidate crossovers must be strictly increasing".into(),
            ));
        }
        previous = Some(*crossover_hz);
    }
    if let Some(measured_low_pass_hz) = config.sub_measured_low_pass_hz {
        validate_crossover(measured_low_pass_hz)?;
        let maximum_candidate = *config
            .candidate_crossovers_hz
            .last()
            .expect("validated nonempty");
        if maximum_candidate > measured_low_pass_hz {
            return Err(DspError::InvalidArgument(format!(
                "candidate crossover {maximum_candidate:.1} Hz exceeds the {measured_low_pass_hz:.1} Hz \
                 low-pass active during the sub capture; the replacement ratio would amplify \
                 measurement noise above the measured corner"
            )));
        }
    }
    let common_grid = &paths.left_main.frequencies_hz;
    for (label, response) in [
        ("left full-range main", &paths.left_main),
        ("right full-range main", &paths.right_main),
        ("wide-band sub", &paths.sub),
    ] {
        validate_response(response, "wide-band isolated path")?;
        require_same_grid(common_grid, &response.frequencies_hz, label)?;
        if response.phase_rad.is_none() {
            return Err(DspError::InvalidArgument(format!(
                "{label} phase is required for wide-band crossover synthesis"
            )));
        }
    }

    let mut states = Vec::with_capacity(config.candidate_crossovers_hz.len());
    for (index, &crossover_hz) in config.candidate_crossovers_hz.iter().enumerate() {
        let high_pass = |frequency_hz: f64| {
            high_pass_response(config.main_high_pass, crossover_hz, frequency_hz)
        };
        let low_pass = |frequency_hz: f64| match config.sub_measured_low_pass_hz {
            Some(measured_low_pass_hz) => complex_divide(
                low_pass_response(config.sub_low_pass, crossover_hz, frequency_hz),
                low_pass_response(config.sub_low_pass, measured_low_pass_hz, frequency_hz),
            ),
            None => low_pass_response(config.sub_low_pass, crossover_hz, frequency_hz),
        };
        states.push(SeparatedCrossoverPaths {
            id: format!("XO{:02}", index + 1),
            crossover_hz,
            left_main: apply_complex_filter(&paths.left_main, high_pass)?,
            right_main: apply_complex_filter(&paths.right_main, high_pass)?,
            sub: apply_complex_filter(&paths.sub, low_pass)?,
        });
    }
    Ok(states)
}

/// Multiply a measured complex response by a modeled filter, bin by bin.
fn apply_complex_filter<F>(response: &CombinedResponse, filter: F) -> DspResult<CombinedResponse>
where
    F: Fn(f64) -> (f64, f64),
{
    let phase_rad = response
        .phase_rad
        .as_ref()
        .expect("validated: wide-band paths carry phase");
    let mut magnitude_db = Vec::with_capacity(response.frequencies_hz.len());
    let mut filtered_phase_rad = Vec::with_capacity(response.frequencies_hz.len());
    for (index, &frequency_hz) in response.frequencies_hz.iter().enumerate() {
        let (real, imaginary) = filter(frequency_hz);
        let gain = real.hypot(imaginary);
        let level_db = response.magnitude_db[index] + 20.0 * gain.max(1.0e-15).log10();
        let phase = phase_rad[index] + imaginary.atan2(real);
        if !level_db.is_finite() || !phase.is_finite() {
            return Err(DspError::NonFinite {
                context: "wide-band synthesized response",
                index,
            });
        }
        magnitude_db.push(level_db);
        filtered_phase_rad.push(phase);
    }
    Ok(CombinedResponse {
        frequencies_hz: response.frequencies_hz.clone(),
        magnitude_db,
        phase_rad: Some(filtered_phase_rad),
        timing: None,
    })
}

/// Complex low-pass response of the analog prototype at `frequency_hz`.
fn low_pass_response(
    alignment: CrossoverAlignment,
    corner_hz: f64,
    frequency_hz: f64,
) -> (f64, f64) {
    let wn = frequency_hz / corner_hz;
    match alignment {
        // H1(jw) = 1 / (1 + jw); LR2 low-pass is H1 squared.
        CrossoverAlignment::LinkwitzRiley2 => {
            let section = complex_divide((1.0, 0.0), (1.0, wn));
            complex_multiply(section, section)
        }
        // H2(jw) = 1 / ((1 - w^2) + j*sqrt(2)*w), the Butterworth Q=1/sqrt(2).
        CrossoverAlignment::Butterworth2 => {
            complex_divide((1.0, 0.0), (1.0 - wn * wn, std::f64::consts::SQRT_2 * wn))
        }
        // LR4 low-pass is the squared 2nd-order Butterworth.
        CrossoverAlignment::LinkwitzRiley4 => {
            let section =
                complex_divide((1.0, 0.0), (1.0 - wn * wn, std::f64::consts::SQRT_2 * wn));
            complex_multiply(section, section)
        }
    }
}

/// Complex high-pass response of the analog prototype at `frequency_hz`.
fn high_pass_response(
    alignment: CrossoverAlignment,
    corner_hz: f64,
    frequency_hz: f64,
) -> (f64, f64) {
    let wn = frequency_hz / corner_hz;
    match alignment {
        // G1(jw) = jw / (1 + jw); LR2 high-pass is G1 squared.
        CrossoverAlignment::LinkwitzRiley2 => {
            let section = complex_divide((0.0, wn), (1.0, wn));
            complex_multiply(section, section)
        }
        // G2(jw) = -w^2 / ((1 - w^2) + j*sqrt(2)*w).
        CrossoverAlignment::Butterworth2 => complex_divide(
            (-wn * wn, 0.0),
            (1.0 - wn * wn, std::f64::consts::SQRT_2 * wn),
        ),
        CrossoverAlignment::LinkwitzRiley4 => {
            let section = complex_divide(
                (-wn * wn, 0.0),
                (1.0 - wn * wn, std::f64::consts::SQRT_2 * wn),
            );
            complex_multiply(section, section)
        }
    }
}

fn complex_multiply(left: (f64, f64), right: (f64, f64)) -> (f64, f64) {
    (
        left.0 * right.0 - left.1 * right.1,
        left.0 * right.1 + left.1 * right.0,
    )
}

fn complex_divide(numerator: (f64, f64), denominator: (f64, f64)) -> (f64, f64) {
    let magnitude_squared = denominator.0 * denominator.0 + denominator.1 * denominator.1;
    (
        (numerator.0 * denominator.0 + numerator.1 * denominator.1) / magnitude_squared,
        (numerator.1 * denominator.0 - numerator.0 * denominator.1) / magnitude_squared,
    )
}

const ARRIVAL_SCAN_STEP_MS: f64 = 0.05;
const ARRIVAL_SCAN_LIMIT_MS: f64 = 20.0;
/// Arrival-anchor bands when the raw wide-band captures are available (v5).
/// No single band is trustworthy in every room: measured on the 2026-08-03
/// captures, 20-45 Hz alone degenerated on the left channel (peak pinned to
/// the -20 ms scan edge - the room's sub/main relative phase is incoherent
/// that low, the same incoherence the SECS phase diagnostics measured),
/// while bands reaching past ~80 Hz drifted a half cycle (+4.65 ms) or
/// pinned to +20 ms on the right. The anchor therefore takes the MEDIAN of
/// this fixed band family evaluated per channel, after rejecting any
/// estimate whose peak sits at a scan edge (a pinned peak is a degenerate
/// correlation, not an arrival). All top edges stay at or below 80 Hz so
/// every member's cycle ambiguity (>= ~15 ms at the effective carrier)
/// stays out of reach of the median. On the same captures the family reads
/// {-1.40, -0.40, +4.25, -0.65, +0.50} ms after rejection -> median
/// -0.40 ms, consistent across channels to under 1 ms.
const ARRIVAL_REFERENCE_BANDS_HZ: [(f64, f64); 3] = [(20.0, 45.0), (20.0, 60.0), (20.0, 80.0)];

/// Band-limited complex cross-correlation of one main/sub pair, evaluated on
/// the fixed [-ARRIVAL_SCAN_LIMIT_MS, +ARRIVAL_SCAN_LIMIT_MS] lag grid. Both
/// paths share the marker timeline, so the lag that maximizes the magnitude
/// is that pair's sub-minus-main offset (positive = sub late = delay the
/// main). The caller pools these curves across paths - one path's curve can
/// carry near-periodic aliases when its filtered overlap band is narrow.
fn arrival_correlation_curve(
    common_grid: &[f64],
    main: &CombinedResponse,
    sub: &CombinedResponse,
    band_low_hz: f64,
    band_high_hz: f64,
) -> DspResult<Vec<f64>> {
    let main_phase = main
        .phase_rad
        .as_ref()
        .expect("validated: separated paths carry phase");
    let sub_phase = sub
        .phase_rad
        .as_ref()
        .expect("validated: separated paths carry phase");
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
    let mut curve = Vec::new();
    let mut tau_ms = -ARRIVAL_SCAN_LIMIT_MS;
    while tau_ms <= ARRIVAL_SCAN_LIMIT_MS + 1.0e-9 {
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for (frequency, weight, phase) in &cross {
            let angle = phase + 2.0 * std::f64::consts::PI * frequency * tau_ms / 1_000.0;
            real += weight * angle.cos();
            imaginary += weight * angle.sin();
        }
        let magnitude = real.hypot(imaginary);
        if !magnitude.is_finite() {
            return Err(DspError::InvalidArgument(
                "sub arrival estimation produced a non-finite correlation".into(),
            ));
        }
        curve.push(magnitude);
        tau_ms += ARRIVAL_SCAN_STEP_MS;
    }
    Ok(curve)
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
                synthesized_crossover_model: None,
                crossover_regularization_db_per_octave:
                    SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
                arrival_reference: None,
                ranking: RankingConfig::default(),
            },
        )
        .unwrap();
        let best = &report.ranking.rankings[0];
        // One anchor arrival from the lowest path (~2 ms) centres every
        // window: both 2 +/- half-period windows clip to [0, 4] -> five
        // 1 ms steps each. (5 + 5) * 2 = 20. The xo100 entry's planted 6 ms
        // offset stays unreachable inside [0, 4], so its best alignment
        // keeps a residual splice error and the window is range-flagged.
        assert_eq!(report.synthesized_candidate_count, 20);
        assert_eq!(best.settings.crossover_hz, 80.0);
        assert_eq!(best.settings.main_delay_ms, Some(2.0));
        assert_eq!(best.settings.polarity, Some(Polarity::Inverted));
        assert!(report.ranking.needs_confirmation);
        let xo80 = &report.arrival_estimates[0];
        assert!((xo80.center_ms - 2.0).abs() <= 0.1, "{}", xo80.center_ms);
        assert!(xo80.left_right_spread_ms < 0.1);
        let xo100 = &report.arrival_estimates[1];
        assert!(
            (xo100.center_ms - xo80.center_ms).abs() < 1.0e-9,
            "windows share one anchor: {}",
            xo100.center_ms
        );
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

    fn wrapped_phase_difference(left: f64, right: f64) -> f64 {
        let mut delta = left - right;
        delta -= (delta / (2.0 * PI)).round() * 2.0 * PI;
        delta.abs()
    }

    #[test]
    fn crossover_alignment_models_have_the_textbook_crossover_properties() {
        let corner_hz = 80.0;
        // Corner levels: LR alignments are -6.02 dB, BW2 is -3.01 dB.
        for (alignment, expected_db) in [
            (CrossoverAlignment::LinkwitzRiley4, -6.020_599_913_279_624),
            (CrossoverAlignment::LinkwitzRiley2, -6.020_599_913_279_624),
            (CrossoverAlignment::Butterworth2, -3.010_299_956_639_812),
        ] {
            for response in [
                low_pass_response(alignment, corner_hz, corner_hz),
                high_pass_response(alignment, corner_hz, corner_hz),
            ] {
                let level_db = 20.0 * response.0.hypot(response.1).log10();
                assert!(
                    (level_db - expected_db).abs() < 1.0e-9,
                    "{alignment:?}: {level_db}"
                );
            }
        }
        for index in 0..200 {
            let frequency_hz = 10.0 * 2.0_f64.powf(index as f64 / 25.0);
            // LR4: low-pass and high-pass are in phase everywhere, so their
            // sum is a unity-magnitude allpass. This is the property that
            // makes the polarity recommendation depend on the alignment.
            let low =
                low_pass_response(CrossoverAlignment::LinkwitzRiley4, corner_hz, frequency_hz);
            let high =
                high_pass_response(CrossoverAlignment::LinkwitzRiley4, corner_hz, frequency_hz);
            let sum = (low.0 + high.0, low.1 + high.1);
            assert!(
                (sum.0.hypot(sum.1) - 1.0).abs() < 1.0e-9,
                "LR4 sum at {frequency_hz}"
            );
            assert!(
                wrapped_phase_difference(low.1.atan2(low.0), high.1.atan2(high.0)) < 1.0e-9,
                "LR4 phase split at {frequency_hz}"
            );
            // LR2: the two sections are 180 degrees apart everywhere, so the
            // flat sum needs one branch inverted.
            let low =
                low_pass_response(CrossoverAlignment::LinkwitzRiley2, corner_hz, frequency_hz);
            let high =
                high_pass_response(CrossoverAlignment::LinkwitzRiley2, corner_hz, frequency_hz);
            let inverted_sum = (low.0 - high.0, low.1 - high.1);
            assert!(
                (inverted_sum.0.hypot(inverted_sum.1) - 1.0).abs() < 1.0e-9,
                "LR2 inverted sum at {frequency_hz}"
            );
            assert!(
                (wrapped_phase_difference(low.1.atan2(low.0), high.1.atan2(high.0)) - PI).abs()
                    < 1.0e-9,
                "LR2 phase split at {frequency_hz}"
            );
        }
    }

    #[test]
    fn wide_band_synthesis_replaces_the_measured_wide_low_pass_exactly() {
        // The sub's true unfiltered path is captured through the wide 250 Hz
        // low-pass. Synthesizing an 80 Hz state must reproduce
        // true_path x LP(80) - substitution - and not
        // true_path x LP(250) x LP(80) - stacking, which would carry the wide
        // filter's ~1.8 ms of low-frequency group delay into every candidate.
        let sub_true = shaped_response(|frequency| -0.01 * (frequency / 20.0), 5.0, false);
        let main_true = shaped_response(|_| 0.0, 0.0, false);
        let measured_sub = apply_complex_filter(&sub_true, |frequency_hz| {
            low_pass_response(CrossoverAlignment::LinkwitzRiley4, 250.0, frequency_hz)
        })
        .unwrap();
        let states = synthesize_wide_band_crossover_states(
            &WideBandIsolatedPaths {
                left_main: main_true.clone(),
                right_main: main_true.clone(),
                sub: measured_sub,
            },
            &WideBandSynthesisConfig {
                candidate_crossovers_hz: vec![80.0, 100.0],
                sub_measured_low_pass_hz: Some(250.0),
                main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
            },
        )
        .unwrap();
        let expected_sub = apply_complex_filter(&sub_true, |frequency_hz| {
            low_pass_response(CrossoverAlignment::LinkwitzRiley4, 80.0, frequency_hz)
        })
        .unwrap();
        let expected_main = apply_complex_filter(&main_true, |frequency_hz| {
            high_pass_response(CrossoverAlignment::LinkwitzRiley4, 80.0, frequency_hz)
        })
        .unwrap();
        let state = &states[0];
        assert_eq!(state.id, "XO01");
        assert_eq!(state.crossover_hz, 80.0);
        let synthesized_phase = state.sub.phase_rad.as_ref().unwrap();
        let expected_phase = expected_sub.phase_rad.as_ref().unwrap();
        for index in 0..state.sub.frequencies_hz.len() {
            assert!(
                (state.sub.magnitude_db[index] - expected_sub.magnitude_db[index]).abs() < 1.0e-9,
                "sub magnitude at bin {index}"
            );
            assert!(
                wrapped_phase_difference(synthesized_phase[index], expected_phase[index]) < 1.0e-9,
                "sub phase at bin {index}"
            );
            assert!(
                (state.left_main.magnitude_db[index] - expected_main.magnitude_db[index]).abs()
                    < 1.0e-9,
                "main magnitude at bin {index}"
            );
        }
    }

    #[test]
    fn wide_band_synthesis_finds_planted_delay_polarity_and_a_supported_crossover() {
        // The main rolls off naturally below 70 Hz, so a 40/60 Hz crossover
        // leaves a hole the sub is no longer allowed to fill; the sub is flat
        // to 150 Hz, physically 5 ms late, and wired inverted. The search
        // must recover the plant from a single wide sub capture. With LR
        // models the filter phases cancel between branches, so the planted
        // 5 ms remains the exact optimum.
        let main = shaped_response(
            |frequency| -24.0 * (70.0 / frequency).log2().max(0.0),
            0.0,
            false,
        );
        let sub_true = shaped_response(
            |frequency| -24.0 * (frequency / 150.0).log2().max(0.0),
            5.0,
            true,
        );
        let measured_sub = apply_complex_filter(&sub_true, |frequency_hz| {
            low_pass_response(CrossoverAlignment::LinkwitzRiley4, 250.0, frequency_hz)
        })
        .unwrap();
        let states = synthesize_wide_band_crossover_states(
            &WideBandIsolatedPaths {
                left_main: main.clone(),
                right_main: main,
                sub: measured_sub,
            },
            &WideBandSynthesisConfig {
                candidate_crossovers_hz: vec![40.0, 60.0, 80.0, 100.0],
                sub_measured_low_pass_hz: Some(250.0),
                main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
            },
        )
        .unwrap();
        let report = optimize_separated_paths(
            &states,
            &SeparatedPathOptimizationConfig {
                measured_main_delay_ms: 0.0,
                measured_polarity: Polarity::Normal,
                delay_minimum_ms: -10.0,
                delay_maximum_ms: 25.0,
                delay_step_ms: 0.5,
                synthesized_crossover_model: Some("LR4 high-pass + LR4 low-pass".into()),
                crossover_regularization_db_per_octave:
                    SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
                arrival_reference: None,
                ranking: RankingConfig::default(),
            },
        )
        .unwrap();
        let best = &report.ranking.rankings[0];
        assert!(
            best.settings.crossover_hz >= 80.0,
            "crossover {}",
            best.settings.crossover_hz
        );
        let delay = best.settings.main_delay_ms.unwrap();
        assert!((delay - 5.0).abs() <= 0.5, "delay {delay}");
        assert_eq!(best.settings.polarity, Some(Polarity::Inverted));
        assert!(report.ranking.needs_confirmation);
        assert!(report
            .ranking
            .warnings
            .iter()
            .any(|warning| warning.contains("synthesized from a filter model")));
        assert!(!report
            .ranking
            .warnings
            .iter()
            .any(|warning| warning.contains("no crossover transfer function")));
    }

    #[test]
    fn a_hotter_sub_cannot_buy_the_crossover_ranking() {
        // Both candidates splice cleanly (flat mains, clean aligned sub), and
        // the sub runs +10 dB hot. Handing the wider band to the louder sub
        // must not improve the score: the v2 envelope objective ranked the
        // highest crossover first exactly here (more band energy formed the
        // cross-candidate envelope), which made the recommendation a
        // monotonic function of the candidate list on any hot-sub system.
        let main = shaped_response(|_| 0.0, 0.0, false);
        let sub = shaped_response(
            |frequency| 10.0 - 24.0 * (frequency / 150.0).log2().max(0.0),
            0.0,
            false,
        );
        let states = synthesize_wide_band_crossover_states(
            &WideBandIsolatedPaths {
                left_main: main.clone(),
                right_main: main,
                sub,
            },
            &WideBandSynthesisConfig {
                candidate_crossovers_hz: vec![60.0, 120.0],
                sub_measured_low_pass_hz: None,
                main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
            },
        )
        .unwrap();
        let report = optimize_separated_paths(
            &states,
            &SeparatedPathOptimizationConfig {
                measured_main_delay_ms: 0.0,
                measured_polarity: Polarity::Normal,
                delay_minimum_ms: -5.0,
                delay_maximum_ms: 5.0,
                delay_step_ms: 0.5,
                synthesized_crossover_model: Some("LR4/LR4".into()),
                crossover_regularization_db_per_octave:
                    SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
                arrival_reference: None,
                ranking: RankingConfig::default(),
            },
        )
        .unwrap();
        let best_low = report
            .ranking
            .rankings
            .iter()
            .find(|candidate| candidate.settings.crossover_hz == 60.0)
            .unwrap();
        let best_high = report
            .ranking
            .rankings
            .iter()
            .find(|candidate| candidate.settings.crossover_hz == 120.0)
            .unwrap();
        // Both splices are genuinely clean - neither score is a fabricated
        // defect - and the hot sub's extra bandwidth buys nothing.
        assert!(
            best_low.metrics.deficit_rms_db < 0.8,
            "{}",
            best_low.metrics.deficit_rms_db
        );
        assert!(
            best_high.metrics.deficit_rms_db < 0.8,
            "{}",
            best_high.metrics.deficit_rms_db
        );
        assert!(
            best_high.metrics.total_score + 0.05 >= best_low.metrics.total_score,
            "high {} vs low {}",
            best_high.metrics.total_score,
            best_low.metrics.total_score
        );
        // With both splices clean, the winner is the physically aligned state
        // at the lower crossover (the documented localization tie-break), not
        // a level artifact or an accidental smoother-phase misalignment.
        let winner = &report.ranking.rankings[0];
        assert_eq!(winner.settings.crossover_hz, 60.0);
        assert_eq!(winner.settings.main_delay_ms, Some(0.0));
        assert_eq!(winner.settings.polarity, Some(Polarity::Normal));
    }

    #[test]
    fn the_ranking_is_invariant_to_the_captured_sub_level() {
        // A pure gain change on the captured sub is a microphone/output-level
        // accident, not room physics. The deployment trim must absorb it
        // exactly: same winner, same per-crossover ordering, same scores, and
        // trims that differ by exactly the injected gain. The v3 scorer
        // failed this - its anchor term compared the sum against a fixed
        // midband level, so +12 dB of capture gain re-ordered the ranking.
        let main = shaped_response(|frequency| broad_notch(frequency, 90.0, 6.0), 0.0, false);
        let sub_shape = |frequency: f64| -24.0 * (frequency / 150.0).log2().max(0.0);
        let config = SeparatedPathOptimizationConfig {
            measured_main_delay_ms: 0.0,
            measured_polarity: Polarity::Normal,
            delay_minimum_ms: -5.0,
            delay_maximum_ms: 5.0,
            delay_step_ms: 0.5,
            synthesized_crossover_model: Some("LR4/LR4".into()),
            crossover_regularization_db_per_octave:
                SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
            arrival_reference: None,
            ranking: RankingConfig::default(),
        };
        let report_for = |sub_gain_db: f64| {
            let states = synthesize_wide_band_crossover_states(
                &WideBandIsolatedPaths {
                    left_main: main.clone(),
                    right_main: main.clone(),
                    sub: shaped_response(
                        |frequency| sub_shape(frequency) + sub_gain_db,
                        0.0,
                        false,
                    ),
                },
                &WideBandSynthesisConfig {
                    candidate_crossovers_hz: vec![50.0, 80.0, 110.0],
                    sub_measured_low_pass_hz: None,
                    main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                    sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
                },
            )
            .unwrap();
            optimize_separated_paths(&states, &config).unwrap()
        };
        let quiet = report_for(0.0);
        let hot = report_for(12.0);
        // Exactly tied mirror-delay entries deep in the list may swap on
        // ~1e-13 float noise, so the invariance contract is stated on what
        // the user sees: the winner and each crossover's best entry. The v3
        // scorer failed even the winner comparison.
        let winner_quiet = &quiet.ranking.rankings[0];
        let winner_hot = &hot.ranking.rankings[0];
        assert_eq!(
            winner_quiet.settings.crossover_hz,
            winner_hot.settings.crossover_hz
        );
        assert_eq!(
            winner_quiet.settings.main_delay_ms,
            winner_hot.settings.main_delay_ms
        );
        assert_eq!(winner_quiet.settings.polarity, winner_hot.settings.polarity);
        for crossover_hz in [50.0, 80.0, 110.0] {
            let best_of = |report: &SeparatedPathOptimizationReport| {
                report
                    .ranking
                    .rankings
                    .iter()
                    .find(|candidate| candidate.settings.crossover_hz == crossover_hz)
                    .unwrap()
                    .clone()
            };
            let quiet_entry = best_of(&quiet);
            let hot_entry = best_of(&hot);
            assert_eq!(
                quiet_entry.settings.main_delay_ms,
                hot_entry.settings.main_delay_ms
            );
            assert_eq!(quiet_entry.settings.polarity, hot_entry.settings.polarity);
            assert!(
                (quiet_entry.metrics.total_score - hot_entry.metrics.total_score).abs() < 1.0e-6,
                "{} vs {}",
                quiet_entry.metrics.total_score,
                hot_entry.metrics.total_score
            );
            let quiet_trim = quiet_entry.settings.sub_level_db.unwrap();
            let hot_trim = hot_entry.settings.sub_level_db.unwrap();
            assert!(
                (quiet_trim - hot_trim - 12.0).abs() < 1.0e-9,
                "trims must absorb the injected gain exactly: {quiet_trim} vs {hot_trim}"
            );
        }
        let advisory = hot.sub_level_advisory.unwrap();
        assert!(
            (advisory.best_gain_db - quiet.sub_level_advisory.unwrap().best_gain_db + 12.0).abs()
                < 1.0e-9
        );
    }

    #[test]
    fn a_hot_sub_cannot_hide_its_own_dips_behind_the_anchor() {
        // The mains are genuinely clean full-range; the sub runs +10 dB hot
        // and has a real -12 dB room dip at 95 Hz. Handing the sub the band
        // that contains its own dip must be charged for it at deployment
        // level. At capture level the surplus lifts the dip's floor above
        // the midband anchor, so a level-sensitive anchor sees nothing wrong
        // with the high crossover - exactly the mechanism that made the
        // recommendation track the top of the candidate list on real data.
        let main = shaped_response(|_| 0.0, 0.0, false);
        let sub = shaped_response(
            |frequency| {
                10.0 - 24.0 * (frequency / 150.0).log2().max(0.0)
                    + broad_notch(frequency, 95.0, 12.0)
            },
            0.0,
            false,
        );
        let states = synthesize_wide_band_crossover_states(
            &WideBandIsolatedPaths {
                left_main: main.clone(),
                right_main: main,
                sub,
            },
            &WideBandSynthesisConfig {
                candidate_crossovers_hz: vec![50.0, 110.0],
                sub_measured_low_pass_hz: None,
                main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
            },
        )
        .unwrap();
        let report = optimize_separated_paths(
            &states,
            &SeparatedPathOptimizationConfig {
                measured_main_delay_ms: 0.0,
                measured_polarity: Polarity::Normal,
                delay_minimum_ms: -5.0,
                delay_maximum_ms: 5.0,
                delay_step_ms: 0.5,
                synthesized_crossover_model: Some("LR4/LR4".into()),
                crossover_regularization_db_per_octave:
                    SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
                arrival_reference: None,
                ranking: RankingConfig::default(),
            },
        )
        .unwrap();
        let best_low = report
            .ranking
            .rankings
            .iter()
            .find(|candidate| candidate.settings.crossover_hz == 50.0)
            .unwrap();
        let best_high = report
            .ranking
            .rankings
            .iter()
            .find(|candidate| candidate.settings.crossover_hz == 110.0)
            .unwrap();
        // The margin shrank from 0.1 when v5 fixed the scoring band at
        // 20-500 Hz (the wider band adds main-dominated bins that are
        // identical for both candidates, diluting the contrast without
        // changing the order); the winner assertion below is the functional
        // pin.
        assert!(
            best_high.metrics.deficit_rms_db > best_low.metrics.deficit_rms_db + 0.05,
            "the sub's own dip must charge the crossover that exposes it: high {} vs low {}",
            best_high.metrics.deficit_rms_db,
            best_low.metrics.deficit_rms_db
        );
        assert_eq!(report.ranking.rankings[0].settings.crossover_hz, 50.0);
        // The winner's reported trim is the deployment calibration, roughly
        // cancelling the +10 dB surplus.
        let trim = report.ranking.rankings[0].settings.sub_level_db.unwrap();
        assert!((-13.0..=-7.0).contains(&trim), "{trim}");
    }

    #[test]
    fn the_arrival_window_ignores_a_narrow_band_alias() {
        // The sub's phase is a clean 3 ms delay below 90 Hz but rides an
        // extra offset (11 ms) above it - the shape a plate filter plus room
        // reflections produce. A per-crossover octave band around a high
        // candidate sees mostly the aliased region and centres the delay
        // window a bass period away from the true alignment (the real
        // session's 12-15 ms recommendations); the shared 20 Hz-to-2x-max
        // band must keep every window on the multi-octave-coherent 3 ms.
        let main = shaped_response(|_| 0.0, 0.0, false);
        let frequencies_hz = grid();
        let sub = CombinedResponse {
            magnitude_db: frequencies_hz
                .iter()
                .map(|frequency| -24.0 * (frequency / 150.0).log2().max(0.0))
                .collect(),
            phase_rad: Some(
                frequencies_hz
                    .iter()
                    .map(|frequency| {
                        let delay_ms = if *frequency < 90.0 { 3.0 } else { 11.0 };
                        -2.0 * PI * frequency * delay_ms / 1_000.0
                    })
                    .collect(),
            ),
            timing: None,
            frequencies_hz,
        };
        let states = synthesize_wide_band_crossover_states(
            &WideBandIsolatedPaths {
                left_main: main.clone(),
                right_main: main,
                sub,
            },
            &WideBandSynthesisConfig {
                candidate_crossovers_hz: vec![40.0, 140.0],
                sub_measured_low_pass_hz: None,
                main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
            },
        )
        .unwrap();
        let report = optimize_separated_paths(
            &states,
            &SeparatedPathOptimizationConfig {
                measured_main_delay_ms: 0.0,
                measured_polarity: Polarity::Normal,
                delay_minimum_ms: -10.0,
                delay_maximum_ms: 25.0,
                delay_step_ms: 0.5,
                synthesized_crossover_model: Some("LR4/LR4".into()),
                crossover_regularization_db_per_octave:
                    SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
                arrival_reference: None,
                ranking: RankingConfig::default(),
            },
        )
        .unwrap();
        for estimate in &report.arrival_estimates {
            assert!(
                (estimate.center_ms - 3.0).abs() <= 1.0,
                "{} Hz window centred at {}",
                estimate.crossover_hz,
                estimate.center_ms
            );
        }
        let spread =
            (report.arrival_estimates[0].center_ms - report.arrival_estimates[1].center_ms).abs();
        assert!(spread < 0.2, "band-inconsistent centres: {spread}");
        let winner_delay = report.ranking.rankings[0].settings.main_delay_ms.unwrap();
        assert!((winner_delay - 3.0).abs() <= 1.0, "{winner_delay}");
    }

    #[test]
    fn wide_band_candidates_above_the_measured_low_pass_are_rejected() {
        let flat = shaped_response(|_| 0.0, 0.0, false);
        let paths = WideBandIsolatedPaths {
            left_main: flat.clone(),
            right_main: flat.clone(),
            sub: flat.clone(),
        };
        let error = synthesize_wide_band_crossover_states(
            &paths,
            &WideBandSynthesisConfig {
                candidate_crossovers_hz: vec![80.0, 300.0],
                sub_measured_low_pass_hz: Some(250.0),
                main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeds"), "{error}");
        let error = synthesize_wide_band_crossover_states(
            &paths,
            &WideBandSynthesisConfig {
                candidate_crossovers_hz: vec![80.0],
                sub_measured_low_pass_hz: Some(250.0),
                main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("2 to 12"), "{error}");
        // A genuinely bypassed measurement low-pass has nothing to replace,
        // so candidates are limited only by the product crossover range.
        assert!(synthesize_wide_band_crossover_states(
            &paths,
            &WideBandSynthesisConfig {
                candidate_crossovers_hz: vec![80.0, 300.0],
                sub_measured_low_pass_hz: None,
                main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
            },
        )
        .is_ok());
    }

    #[test]
    fn the_low_pass_substitution_never_amplifies_below_the_measured_corner() {
        // |LP(x, f)| <= |LP(f_meas, f)| for x <= f_meas at every frequency
        // and same alignment, so replacing the wide measurement filter with a
        // candidate filter can only attenuate measurement noise, never raise
        // it. This is the numerical-safety argument for the division.
        for alignment in [
            CrossoverAlignment::LinkwitzRiley4,
            CrossoverAlignment::LinkwitzRiley2,
            CrossoverAlignment::Butterworth2,
        ] {
            for candidate_hz in [40.0, 120.0, 250.0] {
                for index in 0..300 {
                    let frequency_hz = 5.0 * 2.0_f64.powf(index as f64 / 30.0);
                    let ratio = complex_divide(
                        low_pass_response(alignment, candidate_hz, frequency_hz),
                        low_pass_response(alignment, 250.0, frequency_hz),
                    );
                    assert!(
                        ratio.0.hypot(ratio.1) <= 1.0 + 1.0e-12,
                        "{alignment:?} {candidate_hz} Hz at {frequency_hz} Hz"
                    );
                }
            }
        }
    }

    #[test]
    fn a_too_fine_delay_step_refines_in_two_stages_instead_of_failing() {
        // 0.01 ms across the 40 Hz candidate's ~25 ms arrival window needs
        // ~2,500 values - over the 1,001 per-window cap that v2 rejected
        // outright. v3 coarsens stage 1 (3x -> 0.03 ms here) and then
        // restores the full 0.01 ms resolution around each crossover's best,
        // so the planted 2.00 ms offset - not itself a 0.03 ms multiple -
        // must still be recovered exactly.
        let paths = [
            separated_crossover("xo40", 40.0, 2.0),
            separated_crossover("xo80", 80.0, 2.0),
        ];
        let report = optimize_separated_paths(
            &paths,
            &SeparatedPathOptimizationConfig {
                measured_main_delay_ms: 0.0,
                measured_polarity: Polarity::Normal,
                delay_minimum_ms: -10.0,
                delay_maximum_ms: 25.0,
                delay_step_ms: 0.01,
                synthesized_crossover_model: None,
                crossover_regularization_db_per_octave:
                    SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
                arrival_reference: None,
                ranking: RankingConfig::default(),
            },
        )
        .unwrap();
        let best = &report.ranking.rankings[0];
        let delay = best.settings.main_delay_ms.unwrap();
        assert!((delay - 2.0).abs() < 1.0e-6, "{delay}");
        assert_eq!(best.settings.polarity, Some(Polarity::Inverted));
        assert!(report
            .ranking
            .warnings
            .iter()
            .any(|warning| warning.contains("two stages")));
        // The aggregate is far beyond what a single v2 grid could have held
        // per window, yet each stage stayed inside the caps.
        assert!(
            report.synthesized_candidate_count > 2_000,
            "{}",
            report.synthesized_candidate_count
        );
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
            synthesized_crossover_model: None,
            crossover_regularization_db_per_octave:
                SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
            arrival_reference: None,
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

    #[test]
    fn the_arrival_anchor_and_winner_are_candidate_set_invariant_in_wide_mode() {
        // v4 anchored the arrival on the lowest-crossover CANDIDATE path, so
        // dropping the 40 Hz candidate widened the anchor band, let a
        // correlation alias one bass cycle away win, and moved every
        // candidate's delay window - field data (2026-08-09) flipped
        // 70 Hz / 0.38 ms to 80 Hz / 11.35 ms with identical captures. v5
        // anchors on the raw wide-band captures, so nothing about the
        // estimate may depend on which crossovers are listed.
        let sub_true = shaped_response(|frequency| -0.01 * (frequency / 20.0), 5.0, false);
        let main_true = shaped_response(|_| 0.0, 0.0, false);
        let raw = WideBandIsolatedPaths {
            left_main: main_true.clone(),
            right_main: main_true,
            sub: apply_complex_filter(&sub_true, |frequency_hz| {
                low_pass_response(CrossoverAlignment::LinkwitzRiley4, 250.0, frequency_hz)
            })
            .unwrap(),
        };
        let optimize = |candidates: &[f64]| {
            let states = synthesize_wide_band_crossover_states(
                &raw,
                &WideBandSynthesisConfig {
                    candidate_crossovers_hz: candidates.to_vec(),
                    sub_measured_low_pass_hz: Some(250.0),
                    main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                    sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
                },
            )
            .unwrap();
            optimize_separated_paths(
                &states,
                &SeparatedPathOptimizationConfig {
                    measured_main_delay_ms: 0.0,
                    measured_polarity: Polarity::Normal,
                    delay_minimum_ms: -10.0,
                    delay_maximum_ms: 25.0,
                    delay_step_ms: 0.05,
                    synthesized_crossover_model: Some("test LR4".into()),
                    crossover_regularization_db_per_octave:
                        SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
                    arrival_reference: Some(raw.clone()),
                    ranking: RankingConfig::default(),
                },
            )
            .unwrap()
        };
        let with_forty = optimize(&[40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0]);
        let without_forty = optimize(&[50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0]);

        assert_eq!(
            with_forty.arrival_estimates[0].center_ms, without_forty.arrival_estimates[0].center_ms,
            "the arrival anchor moved with the candidate list"
        );
        let best_for = |report: &SeparatedPathOptimizationReport, crossover_hz: f64| {
            report
                .ranking
                .rankings
                .iter()
                .find(|candidate| candidate.settings.crossover_hz == crossover_hz)
                .map(|candidate| {
                    (
                        candidate.settings.main_delay_ms.unwrap(),
                        candidate.settings.polarity.unwrap(),
                    )
                })
                .expect("every listed crossover is ranked")
        };
        for crossover_hz in [50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0] {
            assert_eq!(
                best_for(&with_forty, crossover_hz),
                best_for(&without_forty, crossover_hz),
                "the {crossover_hz} Hz candidate's own best setting moved with the list"
            );
        }
    }

    #[test]
    fn a_high_lowest_crossover_without_a_reference_warns_about_ambiguity() {
        // Measured mode has no unfiltered capture; the lowest-path fallback
        // is kept but must say out loud when its band can lock a bass cycle
        // away (the class of silent failure the v5 anchor removes for the
        // wide mode).
        let sub_true = shaped_response(|frequency| -0.01 * (frequency / 20.0), 5.0, false);
        let main_true = shaped_response(|_| 0.0, 0.0, false);
        let raw = WideBandIsolatedPaths {
            left_main: main_true.clone(),
            right_main: main_true,
            sub: apply_complex_filter(&sub_true, |frequency_hz| {
                low_pass_response(CrossoverAlignment::LinkwitzRiley4, 250.0, frequency_hz)
            })
            .unwrap(),
        };
        let optimize = |candidates: &[f64]| {
            let states = synthesize_wide_band_crossover_states(
                &raw,
                &WideBandSynthesisConfig {
                    candidate_crossovers_hz: candidates.to_vec(),
                    sub_measured_low_pass_hz: Some(250.0),
                    main_high_pass: CrossoverAlignment::LinkwitzRiley4,
                    sub_low_pass: CrossoverAlignment::LinkwitzRiley4,
                },
            )
            .unwrap();
            optimize_separated_paths(
                &states,
                &SeparatedPathOptimizationConfig {
                    measured_main_delay_ms: 0.0,
                    measured_polarity: Polarity::Normal,
                    delay_minimum_ms: -10.0,
                    delay_maximum_ms: 25.0,
                    delay_step_ms: 0.05,
                    synthesized_crossover_model: Some("test LR4".into()),
                    crossover_regularization_db_per_octave:
                        SEPARATED_PATH_CROSSOVER_REGULARIZATION_DB_PER_OCTAVE,
                    arrival_reference: None,
                    ranking: RankingConfig::default(),
                },
            )
            .unwrap()
        };
        let ambiguous = optimize(&[50.0, 80.0]);
        assert!(
            ambiguous
                .ranking
                .warnings
                .iter()
                .any(|warning| warning.contains("at or below 40 Hz")),
            "warnings: {:?}",
            ambiguous.ranking.warnings
        );
        let anchored = optimize(&[40.0, 80.0]);
        assert!(
            !anchored
                .ranking
                .warnings
                .iter()
                .any(|warning| warning.contains("at or below 40 Hz")),
            "warnings: {:?}",
            anchored.ranking.warnings
        );
    }
}
