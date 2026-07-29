//! Spatially robust, limited-band correction with tightly bounded boost.

use crate::spatial::{summarize_spatial, weighted_percentile, SpatialSummary};
use crate::target::vertical_alignment_db;
use crate::{DspError, DspResult};

const DB_EPSILON: f64 = 1.0e-10;

/// Product safety ceiling. A design that appears to need more attenuation is
/// rejected at configuration time and must be reconsidered instead.
pub const MAXIMUM_SUPPORTED_ATTENUATION_DB: f64 = 12.0;
/// Product safety ceiling for broad, spatially repeated shallow deficits.
pub const MAXIMUM_SUPPORTED_BOOST_DB: f64 = 3.0;

#[derive(Debug, Clone, PartialEq)]
pub struct CorrectionSettings {
    pub correction_low_hz: f64,
    pub correction_full_end_hz: f64,
    pub taper_end_hz: f64,
    pub maximum_attenuation_db: f64,
    pub maximum_boost_db: f64,
    /// Gaussian smoothing full width at half maximum, in octaves.
    pub smoothing_fwhm_octaves: f64,
    pub deep_dip_threshold_db: f64,
    pub dip_protection_half_width_octaves: f64,
    pub spatial_lower_quantile: f64,
    pub minimum_supported_peak_db: f64,
    pub minimum_supported_dip_db: f64,
    /// Wider than the general 1/12-octave cut smoothing so a narrow null
    /// cannot become a boost request.
    pub boost_smoothing_fwhm_octaves: f64,
    pub minimum_boost_width_octaves: f64,
    pub target_reference_low_hz: f64,
    pub target_reference_high_hz: f64,
    /// Frequency bands (low, high in Hz) where boost is forbidden because the
    /// measurement's own per-octave SNR cannot support adding energy there
    /// (2026-07-29 expert review, finding 10). Cuts are unaffected. Empty by
    /// default; the live adapter fills it from measured band SNR.
    pub boost_disallowed_bands: Vec<(f64, f64)>,
}

impl Default for CorrectionSettings {
    fn default() -> Self {
        Self {
            correction_low_hz: 20.0,
            correction_full_end_hz: 500.0,
            taper_end_hz: 650.0,
            maximum_attenuation_db: MAXIMUM_SUPPORTED_ATTENUATION_DB,
            maximum_boost_db: MAXIMUM_SUPPORTED_BOOST_DB,
            smoothing_fwhm_octaves: 1.0 / 12.0,
            deep_dip_threshold_db: 8.0,
            dip_protection_half_width_octaves: 0.10,
            spatial_lower_quantile: 0.25,
            minimum_supported_peak_db: 0.75,
            minimum_supported_dip_db: 1.0,
            boost_smoothing_fwhm_octaves: 1.0 / 3.0,
            minimum_boost_width_octaves: 0.25,
            target_reference_low_hz: 200.0,
            target_reference_high_hz: 500.0,
            boost_disallowed_bands: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CorrectionWarning {
    AttenuationLimitReached { requested_db: f64, limit_db: f64 },
    BoostLimitReached { requested_db: f64, limit_db: f64 },
    SinglePositionOnly,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoomCorrectionDesign {
    pub gain_db: Vec<f64>,
    pub unsmoothed_gain_db: Vec<f64>,
    pub aligned_target_db: Vec<f64>,
    pub target_alignment_db: f64,
    pub protected_dip: Vec<bool>,
    /// Fraction of spatial weight whose response is above target by at least
    /// `minimum_supported_peak_db`.
    pub spatial_support: Vec<f64>,
    /// Fraction of spatial weight whose broadly smoothed response is below the
    /// target by at least `minimum_supported_dip_db`.
    pub spatial_dip_support: Vec<f64>,
    /// True only inside a sufficiently wide, spatially repeated shallow
    /// deficit. Protected deep dips are always false.
    pub boost_eligible: Vec<bool>,
    pub spatial_summary: SpatialSummary,
    pub warnings: Vec<CorrectionWarning>,
}

fn validate_settings(settings: &CorrectionSettings) -> DspResult<()> {
    let ordered_band = settings.correction_low_hz.is_finite()
        && settings.correction_full_end_hz.is_finite()
        && settings.taper_end_hz.is_finite()
        && settings.correction_low_hz > 0.0
        && settings.correction_full_end_hz > settings.correction_low_hz
        && settings.taper_end_hz > settings.correction_full_end_hz;
    if !ordered_band {
        return Err(DspError::InvalidArgument(
            "correction limits must be finite and ordered low < full end < taper end".into(),
        ));
    }
    if !settings.maximum_attenuation_db.is_finite()
        || settings.maximum_attenuation_db <= 0.0
        || settings.maximum_attenuation_db > MAXIMUM_SUPPORTED_ATTENUATION_DB
    {
        return Err(DspError::InvalidArgument(
            format!(
                "maximum attenuation must be finite, positive, and no greater than the {MAXIMUM_SUPPORTED_ATTENUATION_DB} dB product safety ceiling"
            ),
        ));
    }
    if !settings.maximum_boost_db.is_finite()
        || settings.maximum_boost_db < 0.0
        || settings.maximum_boost_db > MAXIMUM_SUPPORTED_BOOST_DB
    {
        return Err(DspError::InvalidArgument(format!(
            "maximum boost must be finite, nonnegative, and no greater than the {MAXIMUM_SUPPORTED_BOOST_DB} dB product safety ceiling"
        )));
    }
    if !settings.smoothing_fwhm_octaves.is_finite()
        || settings.smoothing_fwhm_octaves <= 0.0
        || !settings.dip_protection_half_width_octaves.is_finite()
        || settings.dip_protection_half_width_octaves <= 0.0
        || !settings.boost_smoothing_fwhm_octaves.is_finite()
        || settings.boost_smoothing_fwhm_octaves < settings.smoothing_fwhm_octaves
        || !settings.minimum_boost_width_octaves.is_finite()
        || settings.minimum_boost_width_octaves <= 0.0
    {
        return Err(DspError::InvalidArgument(
            "smoothing, boost-support, and dip-protection widths must be finite, positive, and boost smoothing cannot be narrower than cut smoothing".into(),
        ));
    }
    if !(0.0..=0.5).contains(&settings.spatial_lower_quantile) {
        return Err(DspError::InvalidArgument(
            "spatial lower quantile must be between zero and 0.5".into(),
        ));
    }
    if !settings.deep_dip_threshold_db.is_finite()
        || settings.deep_dip_threshold_db <= 0.0
        || !settings.minimum_supported_peak_db.is_finite()
        || settings.minimum_supported_peak_db <= 0.0
        || !settings.minimum_supported_dip_db.is_finite()
        || settings.minimum_supported_dip_db <= 0.0
    {
        return Err(DspError::InvalidArgument(
            "dip and spatial support thresholds must be finite and positive".into(),
        ));
    }
    for (low_hz, high_hz) in &settings.boost_disallowed_bands {
        if !low_hz.is_finite() || !high_hz.is_finite() || *low_hz <= 0.0 || high_hz <= low_hz {
            return Err(DspError::InvalidArgument(
                "boost-disallowed bands must be finite, positive, and increasing".into(),
            ));
        }
    }
    if !settings.target_reference_low_hz.is_finite()
        || !settings.target_reference_high_hz.is_finite()
        || settings.target_reference_low_hz <= 0.0
        || settings.target_reference_high_hz <= settings.target_reference_low_hz
    {
        return Err(DspError::InvalidArgument(
            "target reference limits must be finite, positive, and increasing".into(),
        ));
    }
    Ok(())
}

fn validate_grid(frequencies_hz: &[f64]) -> DspResult<()> {
    if frequencies_hz.len() < 2 {
        return Err(DspError::EmptyInput("frequency grid"));
    }
    for (index, &frequency) in frequencies_hz.iter().enumerate() {
        if !frequency.is_finite() || frequency < 0.0 {
            return Err(DspError::InvalidArgument(format!(
                "frequency bin {index} must be finite and nonnegative"
            )));
        }
        if index > 0 && frequency <= frequencies_hz[index - 1] {
            return Err(DspError::InvalidArgument(
                "frequency grid must be strictly increasing".into(),
            ));
        }
    }
    Ok(())
}

/// NOTE (2026-07-28 release review): this conversion deliberately uses
/// `FWHM/(2·√ln2)` instead of the textbook `FWHM/(2·√(2·ln2))`, so the
/// realized smoothing width is √2× the configured value (a nominal 1/12
/// octave acts as ≈0.118 octave, a nominal 1/3 as ≈0.471). Every product
/// gate — most critically the ≤0.5 dB realized protected-dip contract in
/// `phase4.rs` — plus the checked-in fixtures and the one real-room
/// verified session are calibrated against this effective width; switching
/// to the textbook formula makes the realized FIR leak >0.5 dB into
/// protected dips beside legitimate cuts (measured 0.55/0.61 dB in the
/// `multi_position_phase4_path_preserves_bounded_boost_and_protected_dips`
/// scenario). Do not "fix" the formula without redesigning the protection
/// geometry and re-verifying on hardware. The honest effective widths are
/// documented in docs/validation.md.
/// Absolute lower bound on the smoothing kernel width, in hertz (2026-07-29
/// expert review, finding 4). Below ~37 Hz a 0.118-octave kernel narrows past
/// what the 340 ms FIR can realize: a ~2 Hz-wide full-depth cut has a
/// minimum-phase decay constant of ~160 ms, its truncated tail leaves ~1 dB
/// of ripple, and the oversampled realization gate then fails the whole
/// design - the user ends up with no filter at all. Flooring the kernel at
/// 3 Hz keeps every requested feature wide enough to realize (decay ~106 ms)
/// at the cost of slightly shallower ultra-narrow subsonic cuts, which is the
/// conservative direction.
const MINIMUM_SMOOTHING_WIDTH_HZ: f64 = 3.0;

fn gaussian_log_smooth(frequencies_hz: &[f64], values: &[f64], fwhm_octaves: f64) -> Vec<f64> {
    let base_sigma = fwhm_octaves / (2.0 * (2.0_f64.ln()).sqrt());
    let mut smoothed = vec![0.0; values.len()];

    for (index, &frequency) in frequencies_hz.iter().enumerate() {
        if frequency <= 0.0 {
            smoothed[index] = values[index];
            continue;
        }
        let floor_fwhm_octaves = MINIMUM_SMOOTHING_WIDTH_HZ / (frequency * std::f64::consts::LN_2);
        let sigma = if floor_fwhm_octaves > fwhm_octaves {
            floor_fwhm_octaves / (2.0 * (2.0_f64.ln()).sqrt())
        } else {
            base_sigma
        };
        let radius = 3.5 * sigma;
        let low = frequency * 2.0_f64.powf(-radius);
        let high = frequency * 2.0_f64.powf(radius);
        let start = frequencies_hz.partition_point(|candidate| *candidate < low);
        let end = frequencies_hz.partition_point(|candidate| *candidate <= high);
        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;
        for neighbor in start..end {
            if frequencies_hz[neighbor] <= 0.0 {
                continue;
            }
            let distance = (frequencies_hz[neighbor] / frequency).log2();
            let weight = (-0.5 * (distance / sigma).powi(2)).exp();
            weighted_sum += weight * values[neighbor];
            weight_sum += weight;
        }
        smoothed[index] = if weight_sum > 0.0 {
            weighted_sum / weight_sum
        } else {
            values[index]
        };
    }
    smoothed
}

fn correction_band_weight(frequency_hz: f64, settings: &CorrectionSettings) -> f64 {
    if frequency_hz < settings.correction_low_hz || frequency_hz >= settings.taper_end_hz {
        return 0.0;
    }
    if frequency_hz <= settings.correction_full_end_hz {
        return 1.0;
    }
    let fraction = (frequency_hz - settings.correction_full_end_hz)
        / (settings.taper_end_hz - settings.correction_full_end_hz);
    0.5 * (1.0 + (std::f64::consts::PI * fraction).cos())
}

fn smoothstep(value: f64) -> f64 {
    let clamped = value.clamp(0.0, 1.0);
    clamped * clamped * (3.0 - 2.0 * clamped)
}

fn mark_sufficiently_wide_spans(
    frequencies_hz: &[f64],
    supported: &[bool],
    minimum_width_octaves: f64,
) -> Vec<bool> {
    let mut admitted = vec![false; supported.len()];
    let mut index = 0;
    while index < supported.len() {
        if !supported[index] || frequencies_hz[index] <= 0.0 {
            index += 1;
            continue;
        }
        let start = index;
        while index + 1 < supported.len() && supported[index + 1] {
            index += 1;
        }
        let end = index;
        let width_octaves = (frequencies_hz[end] / frequencies_hz[start]).log2();
        if width_octaves >= minimum_width_octaves {
            admitted[start..=end].fill(true);
        }
        index += 1;
    }
    admitted
}

/// Design correction from the distribution of all measurement positions.
/// Peaks retain the conservative lower-quantile cut gate. Positive correction
/// is admitted only when the broadly smoothed upper spatial quantile remains
/// below target over a sufficiently wide span. Deep/raw protected dips remain
/// at unity, and positive gain is capped by the 3 dB product ceiling.
pub fn design_limited_correction(
    frequencies_hz: &[f64],
    position_responses_db: &[Vec<f64>],
    weights: &[f64],
    target_db: &[f64],
    settings: &CorrectionSettings,
) -> DspResult<RoomCorrectionDesign> {
    validate_settings(settings)?;
    validate_grid(frequencies_hz)?;
    if target_db.len() != frequencies_hz.len() {
        return Err(DspError::ShapeMismatch(
            "target and frequency grids must have equal lengths".into(),
        ));
    }
    if position_responses_db
        .iter()
        .any(|response| response.len() != frequencies_hz.len())
    {
        return Err(DspError::ShapeMismatch(
            "all position responses must match the frequency grid".into(),
        ));
    }
    let spatial_summary = summarize_spatial(position_responses_db, weights)?;
    let target_alignment_db = vertical_alignment_db(
        frequencies_hz,
        target_db,
        &spatial_summary.energy_average_db,
        settings.target_reference_low_hz,
        settings.target_reference_high_hz,
    )?;
    let aligned_target_db: Vec<f64> = target_db
        .iter()
        .map(|level| level + target_alignment_db)
        .collect();
    let maximum_weight = weights.iter().copied().fold(0.0_f64, f64::max);
    let normalized_weights: Vec<f64> = weights
        .iter()
        .map(|weight| weight / maximum_weight)
        .collect();
    let total_weight: f64 = normalized_weights.iter().sum();
    let residuals_by_position: Vec<Vec<f64>> = position_responses_db
        .iter()
        .map(|response| {
            response
                .iter()
                .zip(&aligned_target_db)
                .map(|(measured, target)| measured - target)
                .collect()
        })
        .collect();
    let broad_residuals_by_position: Vec<Vec<f64>> = residuals_by_position
        .iter()
        .map(|residuals| {
            gaussian_log_smooth(
                frequencies_hz,
                residuals,
                settings.boost_smoothing_fwhm_octaves,
            )
        })
        .collect();
    let mut unsmoothed_gain_db = vec![0.0; frequencies_hz.len()];
    let mut spatial_support = vec![0.0; frequencies_hz.len()];
    let mut spatial_dip_support = vec![0.0; frequencies_hz.len()];
    let mut requested_boost_db = vec![0.0; frequencies_hz.len()];
    let mut raw_boost_supported = vec![false; frequencies_hz.len()];
    let mut largest_requested_attenuation = 0.0_f64;
    let mut largest_requested_boost = 0.0_f64;

    // Protection is decided on raw per-bin residuals and is needed as a
    // complete set before any boost decision: the boost path reads residuals
    // smoothed over the broad boost window, so a deep narrow null drags its
    // *neighbors'* smoothed residuals below the dip-support threshold while
    // the raw protection mask covers only the notch core. Without a guard,
    // bins whose raw response is on target receive boost purely from null
    // leakage (2026-07-28 release review). The mask is therefore precomputed
    // and the boost branch skips every bin within the boost smoother's
    // effective half-width of a protected center.
    let deepest_residual_db: Vec<f64> = (0..frequencies_hz.len())
        .map(|bin| {
            residuals_by_position
                .iter()
                .map(|response| response[bin])
                .fold(f64::INFINITY, f64::min)
        })
        .collect();
    // The bounded-cut cap must live at the same smoothness scale as the
    // request it limits: a raw per-bin cap injects FFT-grid wiggle into the
    // design inside null regions and the realized FIR then overshoots the
    // designed cut by whole decibels at protected bins, while an eroded
    // (windowed-minimum) cap makes the design *smaller* than the physical
    // realization floor and fails the same gate from the other side (both
    // observed on the measured fixture: raw 1.8-2.1 dB, eroded 0.7-1.4 dB
    // excess). Smoothing the deepest-residual curve with the cut kernel keeps
    // a broad, deep, shared null fully protected (its smoothed depth stays
    // below -ceiling, cap 0) while a narrow notch - which realization could
    // never honor bin-by-bin anyway, and whose narrowness makes it hard to
    // hear - releases a cap the FIR can actually track.
    let smoothed_deepest_db = gaussian_log_smooth(
        frequencies_hz,
        &deepest_residual_db,
        settings.smoothing_fwhm_octaves,
    );
    let protected_dip: Vec<bool> = deepest_residual_db
        .iter()
        .map(|deepest| *deepest <= -settings.deep_dip_threshold_db)
        .collect();
    // Guard centers are the *common* deep nulls: bins whose raw upper spatial
    // quantile is itself at protection depth. Only a null shared by the
    // quantile-majority of seats can drag the smoothed upper quantile below
    // the dip-support threshold on its skirt; a single-seat null is already
    // discarded by that quantile and must not blanket-ban boost around it.
    let mut protected_center_frequencies = Vec::new();
    for bin in 0..frequencies_hz.len() {
        if !protected_dip[bin] || frequencies_hz[bin] <= 0.0 {
            continue;
        }
        let residuals: Vec<f64> = residuals_by_position
            .iter()
            .map(|response| response[bin])
            .collect();
        let raw_upper_residual =
            weighted_percentile(&residuals, weights, 1.0 - settings.spatial_lower_quantile)?;
        if raw_upper_residual <= -settings.deep_dip_threshold_db {
            protected_center_frequencies.push(frequencies_hz[bin]);
        }
    }
    // Half of the boost smoother's *effective* FWHM (the configured value
    // times √2 — see `gaussian_log_smooth`): the reach over which a deep
    // null materially contaminates the smoothed residual.
    let boost_dip_guard_octaves = settings.boost_smoothing_fwhm_octaves / std::f64::consts::SQRT_2;

    for bin in 0..frequencies_hz.len() {
        let residuals: Vec<f64> = residuals_by_position
            .iter()
            .map(|response| response[bin])
            .collect();
        let support_weight: f64 = residuals
            .iter()
            .zip(&normalized_weights)
            .filter_map(|(residual, weight)| {
                (*residual >= settings.minimum_supported_peak_db).then_some(weight)
            })
            .sum();
        spatial_support[bin] = support_weight / total_weight;

        if frequencies_hz[bin] < settings.correction_low_hz
            || frequencies_hz[bin] > settings.taper_end_hz
        {
            continue;
        }
        let lower_excess =
            weighted_percentile(&residuals, weights, settings.spatial_lower_quantile)?;
        if lower_excess >= settings.minimum_supported_peak_db {
            let average_excess = spatial_summary.energy_average_db[bin] - aligned_target_db[bin];
            // The lower quantile is the robustness gate and dominant term; the
            // energy mean retains sensitivity to a broad peak's actual energy.
            let requested = (0.7 * lower_excess + 0.3 * average_excess).max(0.0);
            if !requested.is_finite() {
                return Err(DspError::InvalidArgument(format!(
                    "correction request overflowed at frequency bin {bin}"
                )));
            }
            largest_requested_attenuation = largest_requested_attenuation.max(requested);
            unsmoothed_gain_db[bin] =
                -soft_knee_attenuation_db(requested, settings.maximum_attenuation_db);
            continue;
        }

        // A boost needs at least two positions, no raw deep-dip protection,
        // and a broad upper spatial quantile that is still below target. The
        // upper quantile prevents one low seat from asking every seat to boost.
        let boost_snr_disallowed = settings
            .boost_disallowed_bands
            .iter()
            .any(|(low_hz, high_hz)| (*low_hz..*high_hz).contains(&frequencies_hz[bin]));
        if position_responses_db.len() < 2
            || settings.maximum_boost_db <= DB_EPSILON
            || protected_dip[bin]
            || boost_snr_disallowed
        {
            continue;
        }
        let broad_residuals: Vec<f64> = broad_residuals_by_position
            .iter()
            .map(|response| response[bin])
            .collect();
        let dip_support_weight: f64 = broad_residuals
            .iter()
            .zip(&normalized_weights)
            .filter_map(|(residual, weight)| {
                (*residual <= -settings.minimum_supported_dip_db).then_some(weight)
            })
            .sum();
        spatial_dip_support[bin] = dip_support_weight / total_weight;
        let upper_residual = weighted_percentile(
            &broad_residuals,
            weights,
            1.0 - settings.spatial_lower_quantile,
        )?;
        if upper_residual <= -settings.minimum_supported_dip_db {
            // Inside a protected null's smoothing reach the broad residual is
            // contaminated by the null's own skirt, so a bin there must also
            // show the deficit in the *raw* upper quantile before it may ask
            // for boost. A bin whose raw response is on target and only looks
            // low after broad smoothing is the null-skirt artifact this guard
            // exists for (2026-07-28 release review); a genuine shallow broad
            // deficit that happens to neighbor a deep null keeps its boost,
            // and the dip-protection taper still zeroes the null core itself.
            let near_protected_center = protected_center_frequencies.iter().any(|center| {
                (frequencies_hz[bin] / center).log2().abs() <= boost_dip_guard_octaves
            });
            if near_protected_center {
                let raw_upper_residual = weighted_percentile(
                    &residuals,
                    weights,
                    1.0 - settings.spatial_lower_quantile,
                )?;
                if raw_upper_residual > -settings.minimum_supported_dip_db {
                    continue;
                }
            }
            let requested = -upper_residual;
            if !requested.is_finite() {
                return Err(DspError::InvalidArgument(format!(
                    "boost request overflowed at frequency bin {bin}"
                )));
            }
            requested_boost_db[bin] = requested;
            raw_boost_supported[bin] = true;
        }
    }

    let boost_eligible = mark_sufficiently_wide_spans(
        frequencies_hz,
        &raw_boost_supported,
        settings.minimum_boost_width_octaves,
    );
    for bin in 0..frequencies_hz.len() {
        if boost_eligible[bin] && !protected_dip[bin] {
            let requested = requested_boost_db[bin];
            largest_requested_boost = largest_requested_boost.max(requested);
            unsmoothed_gain_db[bin] = requested.min(settings.maximum_boost_db);
        }
    }

    let smoothed = gaussian_log_smooth(
        frequencies_hz,
        &unsmoothed_gain_db,
        settings.smoothing_fwhm_octaves,
    );
    let protected_frequencies: Vec<f64> = frequencies_hz
        .iter()
        .zip(&protected_dip)
        .filter_map(|(frequency, protected)| (*protected && *frequency > 0.0).then_some(*frequency))
        .collect();
    let mut gain_db = Vec::with_capacity(frequencies_hz.len());
    for (bin, &frequency) in frequencies_hz.iter().enumerate() {
        let band_weight = correction_band_weight(frequency, settings);
        let dip_weight = if frequency <= 0.0 || protected_frequencies.is_empty() {
            1.0
        } else {
            let nearest_distance = protected_frequencies
                .iter()
                .map(|center| (frequency / center).log2().abs())
                .fold(f64::INFINITY, f64::min);
            smoothstep(nearest_distance / settings.dip_protection_half_width_octaves)
        };
        let weighted = smoothed[bin] * band_weight;
        let gain = if weighted >= 0.0 {
            // Boost keeps the hard exclusion taper around protected dips.
            (weighted * dip_weight).min(settings.maximum_boost_db)
        } else {
            // Cuts near a deep null are bounded instead of zeroed
            // (2026-07-29 expert review, finding 8): with the q25 gate a cut
            // request already means the weighted-quartile majority of seats
            // shows a real peak, so one seat's null must not erase the whole
            // correction. The bound keeps that seat's *predicted* level from
            // being pushed more than the attenuation ceiling below target:
            // cut <= deepest_raw_residual + ceiling. At a -14 dB null core
            // the cap is zero (full protection); at a -9 dB single-seat dip
            // a 3 dB cut survives. The cap follows the null's own shape, so
            // it tapers intrinsically and the realized protected-dip gate in
            // phase 4 now checks attenuation *beyond this designed cut*.
            let cut_cap_db = (smoothed_deepest_db[bin] + settings.maximum_attenuation_db)
                .clamp(0.0, settings.maximum_attenuation_db);
            weighted.max(-cut_cap_db)
        };
        gain_db.push(if gain.abs() < DB_EPSILON { 0.0 } else { gain });
    }
    validate_correction_invariant(&gain_db)?;

    let mut warnings = Vec::new();
    if largest_requested_attenuation > settings.maximum_attenuation_db + DB_EPSILON {
        warnings.push(CorrectionWarning::AttenuationLimitReached {
            requested_db: largest_requested_attenuation,
            limit_db: settings.maximum_attenuation_db,
        });
    }
    if largest_requested_boost > settings.maximum_boost_db + DB_EPSILON {
        warnings.push(CorrectionWarning::BoostLimitReached {
            requested_db: largest_requested_boost,
            limit_db: settings.maximum_boost_db,
        });
    }
    if position_responses_db.len() == 1 {
        warnings.push(CorrectionWarning::SinglePositionOnly);
    }

    Ok(RoomCorrectionDesign {
        gain_db,
        unsmoothed_gain_db,
        aligned_target_db,
        target_alignment_db,
        protected_dip,
        spatial_support,
        spatial_dip_support,
        boost_eligible,
        spatial_summary,
        warnings,
    })
}

/// Per-bin monotone soft clamp toward the attenuation cap (2026-07-29 expert
/// review, finding 3). Below two thirds of the cap the request passes through
/// exactly; above it, `knee + (cap - knee) * tanh((x - knee) / (cap - knee))`
/// saturates smoothly under the cap. Unlike the previous hard `min(x, cap)`
/// followed by a *uniform* whole-curve rescale in phase 4, one 14 dB mode no
/// longer drags every unrelated bin down with it: 0 maps to 0, the map is
/// monotone, |out| <= |in|, and |out| < cap, so every typed-safety invariant
/// (sign preservation, protection zeros, the cap itself) holds bin by bin.
fn soft_knee_attenuation_db(requested_db: f64, cap_db: f64) -> f64 {
    let knee_db = cap_db * (2.0 / 3.0);
    if requested_db <= knee_db {
        return requested_db;
    }
    let headroom_db = cap_db - knee_db;
    knee_db + headroom_db * ((requested_db - knee_db) / headroom_db).tanh()
}

/// Safety invariant used both immediately after design and by the validator.
pub fn validate_correction_invariant(gain_db: &[f64]) -> DspResult<()> {
    if gain_db.is_empty() {
        return Err(DspError::EmptyInput("correction gain"));
    }
    for (index, &gain) in gain_db.iter().enumerate() {
        if !gain.is_finite() {
            return Err(DspError::NonFinite {
                context: "correction gain",
                index,
            });
        }
        if gain > MAXIMUM_SUPPORTED_BOOST_DB + DB_EPSILON {
            return Err(DspError::InvalidArgument(format!(
                "boost safety invariant violated at bin {index}: {gain:.9} dB exceeds +{MAXIMUM_SUPPORTED_BOOST_DB:.1} dB"
            )));
        }
        if gain < -MAXIMUM_SUPPORTED_ATTENUATION_DB - DB_EPSILON {
            return Err(DspError::InvalidArgument(format!(
                "attenuation safety invariant violated at bin {index}: {gain:.9} dB is below -{MAXIMUM_SUPPORTED_ATTENUATION_DB:.1} dB"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Vec<f64> {
        (0..=800).map(f64::from).collect()
    }

    #[test]
    fn repeated_peak_is_cut_without_exceeding_bounded_gain() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        for (position, response) in responses.iter_mut().enumerate() {
            for value in &mut response[55..=65] {
                *value = 6.0 + position as f64 * 0.1;
            }
        }
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(design.gain_db[60] < -4.0);
        assert!(design
            .gain_db
            .iter()
            .all(|gain| *gain <= MAXIMUM_SUPPORTED_BOOST_DB));
        assert_eq!(design.gain_db[0], 0.0);
        assert_eq!(design.gain_db[650], 0.0);
        assert_eq!(design.gain_db[700], 0.0);
    }

    #[test]
    fn single_position_peak_does_not_dominate() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        responses[5][170] = 12.0;
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert_eq!(design.gain_db[170], 0.0);
    }

    #[test]
    fn non_finite_thresholds_and_excessive_product_limit_are_rejected() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let responses = vec![vec![0.0; frequencies.len()]; 2];
        for mutate in [
            |settings: &mut CorrectionSettings| settings.deep_dip_threshold_db = f64::NAN,
            |settings: &mut CorrectionSettings| settings.minimum_supported_peak_db = f64::INFINITY,
            |settings: &mut CorrectionSettings| settings.maximum_attenuation_db = 12.01,
            |settings: &mut CorrectionSettings| settings.maximum_boost_db = 3.01,
            |settings: &mut CorrectionSettings| settings.taper_end_hz = f64::INFINITY,
        ] {
            let mut settings = CorrectionSettings::default();
            mutate(&mut settings);
            assert!(design_limited_correction(
                &frequencies,
                &responses,
                &[1.0, 1.0],
                &target,
                &settings,
            )
            .is_err());
        }
    }

    #[test]
    fn correction_invariant_enforces_hard_gain_bounds() {
        assert!(validate_correction_invariant(&[-12.0, 0.0, 3.0]).is_ok());
        assert!(validate_correction_invariant(&[-12.01, 0.0]).is_err());
        assert!(validate_correction_invariant(&[0.0, 3.01]).is_err());
    }

    #[test]
    fn extreme_finite_weights_do_not_overflow_spatial_support() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let responses = vec![vec![0.0; frequencies.len()]; 2];
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[f64::MAX, f64::MAX],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(design.spatial_support.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn deep_dip_is_marked_and_not_filled_or_damaged_by_smoothing() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        for response in &mut responses {
            for value in &mut response[95..=105] {
                *value = 6.0;
            }
        }
        responses[0][100] = -14.0;
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(design.protected_dip[100]);
        // A one-bin single-seat notch under a five-seat peak no longer zeroes
        // the majority cut (2026-07-29 expert review, finding 8): the cap is
        // taken from the kernel-smoothed deepest residual, so structure the
        // FIR could never honor bin-by-bin releases a realizable bounded cut,
        // while the bin stays marked and can never receive boost. The
        // realized protected-dip gate still bounds realization against this
        // designed cut.
        assert!(design.gain_db[100] < -3.0, "{}", design.gain_db[100]);
        assert!(design.gain_db[100] > -6.0, "{}", design.gain_db[100]);
        assert!(!design.boost_eligible[100]);
    }

    #[test]
    fn broad_spatially_repeated_shallow_dip_can_receive_up_to_three_db() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        for (position, response) in responses.iter_mut().enumerate() {
            for value in &mut response[45..=95] {
                *value = -4.0 - position as f64 * 0.05;
            }
        }
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();

        assert!(design.boost_eligible[65]);
        assert!(design.spatial_dip_support[65] > 0.95);
        assert!(design.gain_db[65] > 2.5);
        assert!(design
            .gain_db
            .iter()
            .all(|gain| *gain <= MAXIMUM_SUPPORTED_BOOST_DB + DB_EPSILON));
        assert!(design
            .warnings
            .iter()
            .any(|warning| matches!(warning, CorrectionWarning::BoostLimitReached { .. })));
    }

    #[test]
    fn a_single_position_or_spatial_outlier_cannot_request_boost() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut single = vec![vec![0.0; frequencies.len()]];
        single[0][45..=95].fill(-4.0);
        let single_design = design_limited_correction(
            &frequencies,
            &single,
            &[1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(single_design.gain_db.iter().all(|gain| *gain <= 0.0));

        let mut multiple = vec![vec![0.0; frequencies.len()]; 6];
        multiple[5][45..=95].fill(-6.0);
        let outlier_design = design_limited_correction(
            &frequencies,
            &multiple,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(outlier_design.gain_db.iter().all(|gain| *gain <= 0.0));
    }

    #[test]
    fn narrow_or_deep_dips_remain_protected_from_boost() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut narrow = vec![vec![0.0; frequencies.len()]; 6];
        for response in &mut narrow {
            response[100] = -7.0;
        }
        let narrow_design = design_limited_correction(
            &frequencies,
            &narrow,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(!narrow_design.boost_eligible[100]);
        assert_eq!(narrow_design.gain_db[100], 0.0);

        let mut deep = vec![vec![0.0; frequencies.len()]; 6];
        for response in &mut deep {
            response[65..=85].fill(-12.0);
        }
        let deep_design = design_limited_correction(
            &frequencies,
            &deep,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(deep_design.protected_dip[75]);
        assert_eq!(deep_design.gain_db[75], 0.0);
    }

    #[test]
    fn a_deep_null_skirt_cannot_manufacture_boost() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        for response in &mut responses {
            // A deep narrow null shared by every seat: its broad-smoothed
            // skirt dips below the boost-support threshold well outside the
            // raw protection core.
            response[118..=122].fill(-20.0);
            // A genuinely broad, shallow, spatially shared deficit far away.
            for value in &mut response[300..=420] {
                *value = -2.0;
            }
        }
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(design.protected_dip[120]);
        assert_eq!(design.gain_db[120], 0.0);
        // No skirt boost anywhere inside the boost smoother's effective
        // reach of the protected null (±~0.24 octave around 120 Hz).
        for bin in 100..=145 {
            assert!(
                design.gain_db[bin] <= 0.0,
                "null skirt at {bin} Hz received boost {}",
                design.gain_db[bin]
            );
        }
        // The real broad deficit still receives bounded positive gain.
        assert!(design.gain_db[350] > 0.5);
        assert!(design
            .gain_db
            .iter()
            .all(|gain| *gain <= MAXIMUM_SUPPORTED_BOOST_DB));
    }

    #[test]
    fn a_boost_disallowed_band_withholds_boost_but_not_cuts() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        for response in &mut responses {
            for value in &mut response[300..=420] {
                *value = -2.0;
            }
            for value in &mut response[55..=65] {
                *value = 6.0;
            }
        }
        let settings = CorrectionSettings {
            boost_disallowed_bands: vec![(160.0, 640.0)],
            ..CorrectionSettings::default()
        };
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &settings,
        )
        .unwrap();
        // The genuine broad deficit sits inside the low-SNR band: no boost.
        assert!(design.gain_db[350] <= 0.0, "{}", design.gain_db[350]);
        assert!(!design.boost_eligible[350]);
        // Cuts are unaffected by the SNR band.
        assert!(design.gain_db[60] < -4.0, "{}", design.gain_db[60]);

        // Without the band the same deficit receives its bounded boost.
        let unrestricted = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(unrestricted.gain_db[350] > 0.5);
    }

    #[test]
    fn a_single_seat_null_bounds_the_cut_instead_of_erasing_it() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        // Five seats share a +6 dB modal peak around 60 Hz; one seat has a
        // -9 dB null there. The q25 gate still sees a real peak, so the cut
        // must survive - but bounded so the null seat is not predicted more
        // than the 12 dB ceiling below target: cap = -9 + 12 = 3 dB.
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        for response in responses.iter_mut().take(5) {
            for value in &mut response[55..=65] {
                *value = 6.0;
            }
        }
        for value in &mut responses[5][55..=65] {
            *value = -9.0;
        }
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(design.protected_dip[60]);
        assert!(design.gain_db[60] < -1.0, "{}", design.gain_db[60]);
        assert!(design.gain_db[60] >= -3.8, "{}", design.gain_db[60]);
        assert!(!design.boost_eligible[60]);

        // A null deeper than the ceiling still zeroes the cut completely.
        for value in &mut responses[5][55..=65] {
            *value = -14.0;
        }
        let fully_protected = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert_eq!(fully_protected.gain_db[60], 0.0);
    }

    #[test]
    fn subsonic_cuts_are_floored_to_a_realizable_width() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        for response in &mut responses {
            // A 3-bin-wide modal peak at 25 Hz, shared by every seat. Without
            // the hertz floor the 0.118-octave kernel is ~2 Hz here and the
            // requested cut is too narrow for the 340 ms FIR to realize.
            response[24..=26].fill(10.0);
        }
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(design.gain_db[25] < -3.0, "{}", design.gain_db[25]);
        // The floored kernel spreads the cut to at least ~3 Hz of width.
        assert!(design.gain_db[28] < -0.5, "{}", design.gain_db[28]);
        assert!(design.gain_db[22] < -0.5, "{}", design.gain_db[22]);
    }

    #[test]
    fn correction_returns_smoothly_to_unity_between_500_and_650_hz() {
        let frequencies = grid();
        let target = vec![0.0; frequencies.len()];
        let mut responses = vec![vec![0.0; frequencies.len()]; 6];
        for response in &mut responses {
            for value in &mut response[450..=650] {
                *value = 6.0;
            }
        }
        let design = design_limited_correction(
            &frequencies,
            &responses,
            &[2.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            &target,
            &CorrectionSettings::default(),
        )
        .unwrap();
        assert!(design.gain_db[500] < -5.0);
        assert!(design.gain_db[575] < 0.0);
        assert!(design.gain_db[575] > design.gain_db[500]);
        assert!(design.gain_db[625] > design.gain_db[575]);
        assert_eq!(design.gain_db[650], 0.0);
    }
}
