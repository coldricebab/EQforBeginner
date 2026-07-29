//! Phase 4 response-domain 48 kHz offline replay.
//!
//! This module designs from the measured magnitude response stored by the
//! measurement system. A separately stored room IR may have windowing and
//! calibration semantics that a plain FFT does not reproduce, so an optional IR
//! is used only for finite linear-convolution diagnostics. No result from this
//! module can represent a hardware-applied, closed-loop verification.

use crate::correction::{
    design_limited_correction, validate_correction_invariant, CorrectionSettings,
    CorrectionWarning, RoomCorrectionDesign, MAXIMUM_SUPPORTED_ATTENUATION_DB,
    MAXIMUM_SUPPORTED_BOOST_DB,
};
use crate::fir::{synthesize_minimum_phase, FirFilter};
use crate::stereo::{blend_stereo_correction, StereoBlendSettings, StereoGainDesign};
use crate::target::{interpolate_log_frequency_grid, TargetCurve, TargetPreset};
use crate::validation::{
    fft_convolve, validate_frequency_prediction, ValidationReport, ValidationThresholds,
};
use crate::{DspError, DspResult};
use std::collections::HashSet;

pub const PHASE4_OFFLINE_ALGORITHM_VERSION: &str = "phase4-response-replay-v2";
pub const DEFAULT_PHASE4_FIR_FFT_SIZE: usize = 16_384;
pub const MAXIMUM_PROTECTED_DIP_ATTENUATION_DB: f64 = 0.5;
pub const MAXIMUM_PROTECTED_DIP_BOOST_DB: f64 = 0.05;
pub const DEFAULT_SAFE_REDESIGN_MAXIMUM_ATTENUATION_DB: f64 = 11.5;
const FIR_RESPONSE_OVERSAMPLE_FACTOR: usize = 16;

/// The only state that an offline Phase 4 computation can produce.
///
/// A future measured closed-loop validator must use a different result type;
/// adding a caller-provided boolean here would allow prediction to masquerade
/// as hardware evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfflineVerificationState {
    PredictedOnlyMeasured,
}

impl OfflineVerificationState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PredictedOnlyMeasured => "predicted-only-measured",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimedCombinedImpulse {
    pub samples: Vec<f64>,
    /// Original first-sample time from the measurement. It is preserved through
    /// causal FIR convolution and is never replaced with a peak-relative zero.
    pub start_time_seconds: f64,
    /// Explicit direct-arrival time on the retained acoustic-reference
    /// timeline. `None` means arrival evidence is unavailable; consumers must
    /// not silently replace it with the largest array sample.
    pub arrival_time_seconds: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeasuredStereoPosition {
    pub id: String,
    pub weight: f64,
    pub left_magnitude_db: Vec<f64>,
    pub right_magnitude_db: Vec<f64>,
    pub left_timed_combined_ir: Option<TimedCombinedImpulse>,
    pub right_timed_combined_ir: Option<TimedCombinedImpulse>,
}

/// A single- or multi-position response set on one exact measured grid.
#[derive(Debug, Clone, PartialEq)]
pub struct MeasuredStereoResponseSet {
    pub sample_rate_hz: u32,
    pub frequencies_hz: Vec<f64>,
    pub positions: Vec<MeasuredStereoPosition>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phase4OfflineConfig {
    pub target: TargetCurve,
    pub correction: CorrectionSettings,
    pub stereo_blend: StereoBlendSettings,
    pub validation: ValidationThresholds,
    /// Native FFT size used to synthesize each causal minimum-phase FIR. The
    /// default is explicitly versioned rather than inferred from an REW IR.
    pub fir_fft_size: usize,
    pub maximum_protected_dip_attenuation_db: f64,
    pub maximum_protected_dip_boost_db: f64,
    /// Uniform attenuation target used only after the original unconstrained
    /// request is recorded as exceeding the configured correction limit.
    pub safe_redesign_maximum_attenuation_db: f64,
}

impl Default for Phase4OfflineConfig {
    fn default() -> Self {
        Self {
            target: TargetCurve::preset(TargetPreset::BkStyle),
            correction: CorrectionSettings::default(),
            stereo_blend: StereoBlendSettings::default(),
            validation: ValidationThresholds::default(),
            fir_fft_size: DEFAULT_PHASE4_FIR_FFT_SIZE,
            maximum_protected_dip_attenuation_db: MAXIMUM_PROTECTED_DIP_ATTENUATION_DB,
            maximum_protected_dip_boost_db: MAXIMUM_PROTECTED_DIP_BOOST_DB,
            safe_redesign_maximum_attenuation_db: DEFAULT_SAFE_REDESIGN_MAXIMUM_ATTENUATION_DB,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StereoChannel {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeDomainConvolutionDiagnostic {
    pub position_id: String,
    pub channel: StereoChannel,
    pub input_start_time_seconds: f64,
    pub predicted_start_time_seconds: f64,
    pub input_sample_count: usize,
    pub predicted_sample_count: usize,
    /// Maximum absolute sample of the convolved room IR. This is not a playback
    /// signal level, clipping prediction, or true-peak measurement.
    pub maximum_absolute_ir_sample: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProtectedDipValidation {
    pub allowed_attenuation_db: f64,
    pub allowed_boost_db: f64,
    pub left_maximum_attenuation_db: f64,
    pub right_maximum_attenuation_db: f64,
    pub left_maximum_boost_db: f64,
    pub right_maximum_boost_db: f64,
    pub left_passed: bool,
    pub right_passed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SafeRedesignChannel {
    pub applied: bool,
    /// Maximum unconstrained request reported before the correction module
    /// capped it. This preserves why redesign was required.
    pub original_requested_maximum_attenuation_db: f64,
    pub original_limit_db: f64,
    pub redesign_target_maximum_attenuation_db: f64,
    /// Uniform multiplier applied to both the smoothed and unsmoothed bounded
    /// request. It is one when no over-limit warning was produced.
    pub redesign_strength: f64,
    pub redesigned_maximum_attenuation_db: f64,
    pub resolved: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SafeRedesignReport {
    pub left: SafeRedesignChannel,
    pub right: SafeRedesignChannel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phase4OfflineResult {
    pub algorithm_version: &'static str,
    pub verification_state: OfflineVerificationState,
    /// Numerical response-domain gates only. This is deliberately separate
    /// from `verification_state` and is not closed-loop evidence.
    pub numerical_passed: bool,
    pub sample_rate_hz: u32,
    pub fir_fft_size: usize,
    pub measured_frequencies_hz: Vec<f64>,
    pub design_frequencies_hz: Vec<f64>,
    pub native_frequencies_hz: Vec<f64>,
    pub position_weights: Vec<f64>,
    pub left_design: RoomCorrectionDesign,
    pub right_design: RoomCorrectionDesign,
    pub measured_grid_stereo_design: StereoGainDesign,
    pub native_stereo_design: StereoGainDesign,
    pub left_fir: FirFilter,
    pub right_fir: FirFilter,
    pub left_realized_response_db: Vec<f64>,
    pub right_realized_response_db: Vec<f64>,
    pub left_predicted_position_db: Vec<Vec<f64>>,
    pub right_predicted_position_db: Vec<Vec<f64>>,
    pub left_validation: ValidationReport,
    pub right_validation: ValidationReport,
    pub protected_dip_validation: ProtectedDipValidation,
    pub safe_redesign: SafeRedesignReport,
    pub time_domain_diagnostics: Vec<TimeDomainConvolutionDiagnostic>,
}

fn validate_config(config: &Phase4OfflineConfig) -> DspResult<()> {
    if config.fir_fft_size < 1_024 || config.fir_fft_size % 2 != 0 {
        return Err(DspError::InvalidArgument(
            "Phase 4 FIR FFT size must be even and at least 1024".into(),
        ));
    }
    if !config.maximum_protected_dip_attenuation_db.is_finite()
        || config.maximum_protected_dip_attenuation_db < 0.0
        || config.maximum_protected_dip_attenuation_db > MAXIMUM_PROTECTED_DIP_ATTENUATION_DB
    {
        return Err(DspError::InvalidArgument(format!(
            "protected-dip attenuation gate must be finite and between 0 and {MAXIMUM_PROTECTED_DIP_ATTENUATION_DB} dB"
        )));
    }
    if !config.maximum_protected_dip_boost_db.is_finite()
        || config.maximum_protected_dip_boost_db < 0.0
        || config.maximum_protected_dip_boost_db > MAXIMUM_PROTECTED_DIP_BOOST_DB
    {
        return Err(DspError::InvalidArgument(format!(
            "protected-dip boost gate must be finite and between 0 and {MAXIMUM_PROTECTED_DIP_BOOST_DB} dB"
        )));
    }
    if !config.safe_redesign_maximum_attenuation_db.is_finite()
        || config.safe_redesign_maximum_attenuation_db <= 0.0
        || config.safe_redesign_maximum_attenuation_db
            > DEFAULT_SAFE_REDESIGN_MAXIMUM_ATTENUATION_DB
        || config.safe_redesign_maximum_attenuation_db > config.correction.maximum_attenuation_db
    {
        return Err(DspError::InvalidArgument(format!(
            "safe redesign attenuation must be finite, positive, no greater than {DEFAULT_SAFE_REDESIGN_MAXIMUM_ATTENUATION_DB} dB, and no greater than the configured correction limit"
        )));
    }
    Ok(())
}

fn validate_input(input: &MeasuredStereoResponseSet) -> DspResult<()> {
    if input.sample_rate_hz != 48_000 {
        return Err(DspError::InvalidArgument(format!(
            "Phase 4 offline replay requires 48000 Hz, got {} Hz",
            input.sample_rate_hz
        )));
    }
    if input.frequencies_hz.len() < 3 {
        return Err(DspError::EmptyInput("Phase 4 measured frequency grid"));
    }
    for (index, &frequency) in input.frequencies_hz.iter().enumerate() {
        if !frequency.is_finite() || frequency <= 0.0 {
            return Err(DspError::InvalidArgument(format!(
                "Phase 4 measured frequency bin {index} must be finite and positive"
            )));
        }
        if index > 0 && frequency <= input.frequencies_hz[index - 1] {
            return Err(DspError::InvalidArgument(
                "Phase 4 measured frequency grid must be strictly increasing".into(),
            ));
        }
    }
    if input.positions.is_empty() {
        return Err(DspError::EmptyInput("Phase 4 measured positions"));
    }
    let mut ids = HashSet::with_capacity(input.positions.len());
    for position in &input.positions {
        if position.id.trim().is_empty() || !ids.insert(position.id.as_str()) {
            return Err(DspError::InvalidArgument(
                "Phase 4 position IDs must be nonempty and unique".into(),
            ));
        }
        if !position.weight.is_finite() || position.weight <= 0.0 {
            return Err(DspError::InvalidArgument(format!(
                "Phase 4 position '{}' must have a finite positive weight",
                position.id
            )));
        }
        for (channel, response) in [
            ("left", &position.left_magnitude_db),
            ("right", &position.right_magnitude_db),
        ] {
            if response.len() != input.frequencies_hz.len() {
                return Err(DspError::ShapeMismatch(format!(
                    "Phase 4 {} response at '{}' does not match the measured grid",
                    channel, position.id
                )));
            }
            if let Some(index) = response.iter().position(|value| !value.is_finite()) {
                return Err(DspError::NonFinite {
                    context: "Phase 4 measured magnitude",
                    index,
                });
            }
        }
        for impulse in [
            position.left_timed_combined_ir.as_ref(),
            position.right_timed_combined_ir.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !impulse.start_time_seconds.is_finite() {
                return Err(DspError::InvalidArgument(format!(
                    "Phase 4 timed IR at '{}' must preserve a finite start time",
                    position.id
                )));
            }
            if impulse
                .arrival_time_seconds
                .is_some_and(|arrival| !arrival.is_finite())
            {
                return Err(DspError::InvalidArgument(format!(
                    "Phase 4 timed IR at '{}' has a non-finite explicit arrival time",
                    position.id
                )));
            }
            if impulse.samples.is_empty() {
                return Err(DspError::EmptyInput("Phase 4 timed combined IR"));
            }
            if let Some(index) = impulse
                .samples
                .iter()
                .position(|sample| !sample.is_finite())
            {
                return Err(DspError::NonFinite {
                    context: "Phase 4 timed combined IR",
                    index,
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn native_frequency_grid(sample_rate_hz: u32, fft_size: usize) -> Vec<f64> {
    let bin_hz = f64::from(sample_rate_hz) / fft_size as f64;
    (0..=fft_size / 2)
        .map(|index| index as f64 * bin_hz)
        .collect()
}

pub(crate) fn resample_correction_to_native_grid(
    measured_frequencies_hz: &[f64],
    measured_gain_db: &[f64],
    native_frequencies_hz: &[f64],
    correction_low_hz: f64,
    taper_end_hz: f64,
) -> DspResult<Vec<f64>> {
    if measured_frequencies_hz.len() != measured_gain_db.len() || measured_frequencies_hz.len() < 2
    {
        return Err(DspError::ShapeMismatch(
            "measured correction frequency and gain grids must be equal and nonempty".into(),
        ));
    }
    validate_correction_invariant(measured_gain_db)?;
    let first_measured = measured_frequencies_hz[0];
    let mut query_indices = Vec::new();
    let mut queries = Vec::new();
    for (index, &frequency) in native_frequencies_hz.iter().enumerate() {
        if frequency >= correction_low_hz && frequency >= first_measured && frequency < taper_end_hz
        {
            query_indices.push(index);
            queries.push(frequency);
        }
    }
    let interpolated =
        interpolate_log_frequency_grid(measured_frequencies_hz, measured_gain_db, &queries)?;
    let mut native = vec![0.0; native_frequencies_hz.len()];
    for (index, gain) in query_indices.into_iter().zip(interpolated) {
        native[index] = gain.clamp(
            -MAXIMUM_SUPPORTED_ATTENUATION_DB,
            MAXIMUM_SUPPORTED_BOOST_DB,
        );
    }
    // Fade the band edge in below `correction_low_hz` instead of stepping
    // from unity to the full edge gain across one native bin. A hard step
    // produces Gibbs overshoot in the realized response; the 16×-oversampled
    // ceiling check then converts that overshoot into a global attenuation-only
    // normalization (every cut deepens, out-of-band unity drops), and a design
    // already near the attenuation limit can trip the realized gate outright
    // (2026-07-28 release review). The fade spans three native bins, capped at
    // half an octave, sits entirely below the product band, and scales the
    // edge gain itself, so it stays inside the same [-limit, +limit] bounds.
    let edge_low_hz = correction_low_hz.max(first_measured);
    let edge_gain_db = interpolate_log_frequency_grid(
        measured_frequencies_hz,
        measured_gain_db,
        &[edge_low_hz.min(*measured_frequencies_hz.last().expect("validated nonempty"))],
    )?[0]
        .clamp(
            -MAXIMUM_SUPPORTED_ATTENUATION_DB,
            MAXIMUM_SUPPORTED_BOOST_DB,
        );
    if native_frequencies_hz.len() >= 2 && edge_gain_db.abs() > 0.0 {
        let native_spacing_hz = native_frequencies_hz[1] - native_frequencies_hz[0];
        let fade_start_hz =
            (edge_low_hz - 3.0 * native_spacing_hz).max(edge_low_hz * 0.5_f64.sqrt());
        if fade_start_hz < edge_low_hz {
            for (index, &frequency) in native_frequencies_hz.iter().enumerate() {
                if frequency > fade_start_hz && frequency < edge_low_hz && native[index] == 0.0 {
                    let progress = (frequency - fade_start_hz) / (edge_low_hz - fade_start_hz);
                    let weight = 0.5 * (1.0 - (std::f64::consts::PI * progress).cos());
                    native[index] = edge_gain_db * weight;
                }
            }
        }
    }
    validate_correction_invariant(&native)?;
    Ok(native)
}

fn realized_response_on_measured_grid(
    fir: &FirFilter,
    response_fft_size: usize,
    measured_frequencies_hz: &[f64],
) -> DspResult<(Vec<f64>, Vec<f64>)> {
    let dense_response_db = fir.response_db(response_fft_size)?;
    let dense_frequencies_hz = native_frequency_grid(fir.sample_rate_hz, response_fft_size);
    let measured_response_db = interpolate_log_frequency_grid(
        &dense_frequencies_hz,
        &dense_response_db,
        measured_frequencies_hz,
    )?;
    Ok((dense_response_db, measured_response_db))
}

fn convolve_timed_ir(
    position_id: &str,
    channel: StereoChannel,
    impulse: &TimedCombinedImpulse,
    fir: &FirFilter,
) -> DspResult<TimeDomainConvolutionDiagnostic> {
    let predicted = fft_convolve(&impulse.samples, &fir.taps)?;
    let maximum_absolute_ir_sample = predicted
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f64, f64::max);
    if !maximum_absolute_ir_sample.is_finite() {
        return Err(DspError::InvalidArgument(
            "Phase 4 timed convolution produced a non-finite IR diagnostic".into(),
        ));
    }
    Ok(TimeDomainConvolutionDiagnostic {
        position_id: position_id.into(),
        channel,
        input_start_time_seconds: impulse.start_time_seconds,
        // A causal minimum-phase FIR begins at t=0, so linear convolution does
        // not replace or shift the measurement's original first-sample time.
        predicted_start_time_seconds: impulse.start_time_seconds,
        input_sample_count: impulse.samples.len(),
        predicted_sample_count: predicted.len(),
        maximum_absolute_ir_sample,
    })
}

/// Realized attenuation at protected bins *beyond the designed cut*. The
/// design may now carry a bounded cut inside a protected region (one seat's
/// null no longer erases a majority peak; 2026-07-29 expert review, finding
/// 8), so the safety contract is that realization must not dig deeper than
/// the bounded design intent - not that it must be flat where the design
/// deliberately is not.
fn maximum_protected_attenuation_db(
    mask: &[bool],
    response_db: &[f64],
    designed_gain_db: &[f64],
) -> DspResult<f64> {
    if mask.len() != response_db.len() || mask.len() != designed_gain_db.len() {
        return Err(DspError::ShapeMismatch(
            "protected-dip mask, realized response, and designed gain must have equal lengths"
                .into(),
        ));
    }
    Ok(mask
        .iter()
        .zip(response_db)
        .zip(designed_gain_db)
        .filter_map(|((protected, response), designed)| {
            protected.then_some(((-response) - (-designed).max(0.0)).max(0.0))
        })
        .fold(0.0, f64::max))
}

fn maximum_protected_boost_db(mask: &[bool], response_db: &[f64]) -> DspResult<f64> {
    if mask.len() != response_db.len() {
        return Err(DspError::ShapeMismatch(
            "protected-dip mask and realized response must have equal lengths".into(),
        ));
    }
    Ok(mask
        .iter()
        .zip(response_db)
        .filter_map(|(protected, response)| protected.then_some(response.max(0.0)))
        .fold(0.0, f64::max))
}

fn maximum_cut_db(gain_db: &[f64]) -> f64 {
    -gain_db.iter().copied().fold(0.0, f64::min)
}

fn apply_safe_redesign(
    design: &mut RoomCorrectionDesign,
    redesign_target_db: f64,
    configured_limit_db: f64,
) -> DspResult<SafeRedesignChannel> {
    let over_limit = design.warnings.iter().find_map(|warning| match warning {
        CorrectionWarning::AttenuationLimitReached {
            requested_db,
            limit_db,
        } => Some((*requested_db, *limit_db)),
        CorrectionWarning::BoostLimitReached { .. } | CorrectionWarning::SinglePositionOnly => None,
    });
    let original_requested_maximum_attenuation_db = over_limit
        .map(|(requested, _)| requested)
        .unwrap_or_else(|| maximum_cut_db(&design.unsmoothed_gain_db));
    let original_limit_db = over_limit
        .map(|(_, limit)| limit)
        .unwrap_or(configured_limit_db);
    // Scale relative to the curve the strength actually multiplies: `gain_db`
    // is already clamped at the configured limit, so dividing by the larger
    // *unclamped* request would double-attenuate — the realized maximum cut
    // would land at `limit·target/requested`, far below the redesign target,
    // and every unrelated bin would be weakened by the same excess factor
    // (2026-07-28 release review). Dividing by the clamped curve's own maximum
    // converges the realized cut to the target exactly while keeping the
    // multiplier in [0, 1], which preserves sign, protection zeros, and the
    // boost ceiling.
    let clamped_maximum_attenuation_db = maximum_cut_db(&design.gain_db);
    let redesign_strength = if over_limit.is_some() && clamped_maximum_attenuation_db > 0.0 {
        (redesign_target_db / clamped_maximum_attenuation_db).clamp(0.0, 1.0)
    } else {
        1.0
    };
    if over_limit.is_some() {
        for gain in &mut design.gain_db {
            *gain *= redesign_strength;
        }
        for gain in &mut design.unsmoothed_gain_db {
            *gain *= redesign_strength;
        }
    }
    validate_correction_invariant(&design.gain_db)?;
    validate_correction_invariant(&design.unsmoothed_gain_db)?;
    let redesigned_maximum_attenuation_db = maximum_cut_db(&design.gain_db);
    let resolved =
        over_limit.is_none() || redesigned_maximum_attenuation_db <= redesign_target_db + 1.0e-9;
    Ok(SafeRedesignChannel {
        applied: over_limit.is_some(),
        original_requested_maximum_attenuation_db,
        original_limit_db,
        redesign_target_maximum_attenuation_db: redesign_target_db,
        redesign_strength,
        redesigned_maximum_attenuation_db,
        resolved,
    })
}

/// Design and numerically validate a 48 kHz minimum-phase FIR from measured
/// magnitude responses. The returned status is always predicted-only.
pub fn run_phase4_offline(
    input: &MeasuredStereoResponseSet,
    config: &Phase4OfflineConfig,
) -> DspResult<Phase4OfflineResult> {
    validate_config(config)?;
    validate_input(input)?;

    let design_end = config.correction.taper_end_hz;
    let design_indices: Vec<usize> = input
        .frequencies_hz
        .iter()
        .enumerate()
        .filter_map(|(index, frequency)| (*frequency <= design_end).then_some(index))
        .collect();
    if design_indices.len() < 3 {
        return Err(DspError::InvalidArgument(
            "measured responses do not cover the Phase 4 correction/taper band".into(),
        ));
    }
    let design_frequencies_hz: Vec<f64> = design_indices
        .iter()
        .map(|&index| input.frequencies_hz[index])
        .collect();
    let position_weights: Vec<f64> = input
        .positions
        .iter()
        .map(|position| position.weight)
        .collect();
    let left_design_levels: Vec<Vec<f64>> = input
        .positions
        .iter()
        .map(|position| {
            design_indices
                .iter()
                .map(|&index| position.left_magnitude_db[index])
                .collect()
        })
        .collect();
    let right_design_levels: Vec<Vec<f64>> = input
        .positions
        .iter()
        .map(|position| {
            design_indices
                .iter()
                .map(|&index| position.right_magnitude_db[index])
                .collect()
        })
        .collect();
    let target_db = config.target.evaluate(&design_frequencies_hz)?;
    let mut left_design = design_limited_correction(
        &design_frequencies_hz,
        &left_design_levels,
        &position_weights,
        &target_db,
        &config.correction,
    )?;
    let mut right_design = design_limited_correction(
        &design_frequencies_hz,
        &right_design_levels,
        &position_weights,
        &target_db,
        &config.correction,
    )?;
    let safe_redesign = SafeRedesignReport {
        left: apply_safe_redesign(
            &mut left_design,
            config.safe_redesign_maximum_attenuation_db,
            config.correction.maximum_attenuation_db,
        )?,
        right: apply_safe_redesign(
            &mut right_design,
            config.safe_redesign_maximum_attenuation_db,
            config.correction.maximum_attenuation_db,
        )?,
    };
    let measured_grid_stereo_design = blend_stereo_correction(
        &design_frequencies_hz,
        &left_design.gain_db,
        &right_design.gain_db,
        &config.stereo_blend,
    )?;

    let native_frequencies_hz = native_frequency_grid(input.sample_rate_hz, config.fir_fft_size);
    let native_left_gain_db = resample_correction_to_native_grid(
        &design_frequencies_hz,
        &measured_grid_stereo_design.left_gain_db,
        &native_frequencies_hz,
        config.correction.correction_low_hz,
        config.correction.taper_end_hz,
    )?;
    let native_right_gain_db = resample_correction_to_native_grid(
        &design_frequencies_hz,
        &measured_grid_stereo_design.right_gain_db,
        &native_frequencies_hz,
        config.correction.correction_low_hz,
        config.correction.taper_end_hz,
    )?;
    let native_common_gain_db = resample_correction_to_native_grid(
        &design_frequencies_hz,
        &measured_grid_stereo_design.common_gain_db,
        &native_frequencies_hz,
        config.correction.correction_low_hz,
        config.correction.taper_end_hz,
    )?;
    let native_channel_specific_mix = interpolate_log_frequency_grid(
        &design_frequencies_hz,
        &measured_grid_stereo_design.channel_specific_mix,
        &native_frequencies_hz,
    )?;
    let native_stereo_design = StereoGainDesign {
        left_gain_db: native_left_gain_db,
        right_gain_db: native_right_gain_db,
        common_gain_db: native_common_gain_db,
        channel_specific_mix: native_channel_specific_mix,
    };
    let left_fir =
        synthesize_minimum_phase(&native_stereo_design.left_gain_db, input.sample_rate_hz)?;
    let right_fir =
        synthesize_minimum_phase(&native_stereo_design.right_gain_db, input.sample_rate_hz)?;

    let response_fft_size = config
        .fir_fft_size
        .checked_mul(FIR_RESPONSE_OVERSAMPLE_FACTOR)
        .ok_or_else(|| DspError::InvalidArgument("Phase 4 response FFT size overflow".into()))?;
    let (left_dense_response_db, left_realized_response_db) =
        realized_response_on_measured_grid(&left_fir, response_fft_size, &input.frequencies_hz)?;
    let (right_dense_response_db, right_realized_response_db) =
        realized_response_on_measured_grid(&right_fir, response_fft_size, &input.frequencies_hz)?;
    let left_predicted_position_db: Vec<Vec<f64>> = input
        .positions
        .iter()
        .map(|position| {
            position
                .left_magnitude_db
                .iter()
                .zip(&left_realized_response_db)
                .map(|(measured, correction)| measured + correction)
                .collect()
        })
        .collect();
    let right_predicted_position_db: Vec<Vec<f64>> = input
        .positions
        .iter()
        .map(|position| {
            position
                .right_magnitude_db
                .iter()
                .zip(&right_realized_response_db)
                .map(|(measured, correction)| measured + correction)
                .collect()
        })
        .collect();

    let mut time_domain_diagnostics = Vec::new();
    for position in &input.positions {
        if let Some(impulse) = &position.left_timed_combined_ir {
            time_domain_diagnostics.push(convolve_timed_ir(
                &position.id,
                StereoChannel::Left,
                impulse,
                &left_fir,
            )?);
        }
        if let Some(impulse) = &position.right_timed_combined_ir {
            time_domain_diagnostics.push(convolve_timed_ir(
                &position.id,
                StereoChannel::Right,
                impulse,
                &right_fir,
            )?);
        }
    }
    let left_ir_peak = time_domain_diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.channel == StereoChannel::Left)
        .map(|diagnostic| diagnostic.maximum_absolute_ir_sample)
        .reduce(f64::max);
    let right_ir_peak = time_domain_diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.channel == StereoChannel::Right)
        .map(|diagnostic| diagnostic.maximum_absolute_ir_sample)
        .reduce(f64::max);

    let left_design_realized_response: Vec<f64> = design_indices
        .iter()
        .map(|&index| left_realized_response_db[index])
        .collect();
    let right_design_realized_response: Vec<f64> = design_indices
        .iter()
        .map(|&index| right_realized_response_db[index])
        .collect();
    let left_design_predicted: Vec<Vec<f64>> = left_predicted_position_db
        .iter()
        .map(|response| {
            design_indices
                .iter()
                .map(|&index| response[index])
                .collect()
        })
        .collect();
    let right_design_predicted: Vec<Vec<f64>> = right_predicted_position_db
        .iter()
        .map(|response| {
            design_indices
                .iter()
                .map(|&index| response[index])
                .collect()
        })
        .collect();
    let left_validation = validate_frequency_prediction(
        &design_frequencies_hz,
        &left_design_levels,
        &left_design_predicted,
        &position_weights,
        &left_design.aligned_target_db,
        &left_dense_response_db,
        left_ir_peak,
        &config.validation,
    )?;
    let right_validation = validate_frequency_prediction(
        &design_frequencies_hz,
        &right_design_levels,
        &right_design_predicted,
        &position_weights,
        &right_design.aligned_target_db,
        &right_dense_response_db,
        right_ir_peak,
        &config.validation,
    )?;

    let left_protected_attenuation_db = maximum_protected_attenuation_db(
        &left_design.protected_dip,
        &left_design_realized_response,
        &measured_grid_stereo_design.left_gain_db,
    )?;
    let right_protected_attenuation_db = maximum_protected_attenuation_db(
        &right_design.protected_dip,
        &right_design_realized_response,
        &measured_grid_stereo_design.right_gain_db,
    )?;
    let left_protected_boost_db =
        maximum_protected_boost_db(&left_design.protected_dip, &left_design_realized_response)?;
    let right_protected_boost_db =
        maximum_protected_boost_db(&right_design.protected_dip, &right_design_realized_response)?;
    let protected_dip_validation = ProtectedDipValidation {
        allowed_attenuation_db: config.maximum_protected_dip_attenuation_db,
        allowed_boost_db: config.maximum_protected_dip_boost_db,
        left_maximum_attenuation_db: left_protected_attenuation_db,
        right_maximum_attenuation_db: right_protected_attenuation_db,
        left_maximum_boost_db: left_protected_boost_db,
        right_maximum_boost_db: right_protected_boost_db,
        left_passed: left_protected_attenuation_db <= config.maximum_protected_dip_attenuation_db
            && left_protected_boost_db <= config.maximum_protected_dip_boost_db,
        right_passed: right_protected_attenuation_db <= config.maximum_protected_dip_attenuation_db
            && right_protected_boost_db <= config.maximum_protected_dip_boost_db,
    };
    let numerical_passed = left_validation.passed
        && right_validation.passed
        && protected_dip_validation.left_passed
        && protected_dip_validation.right_passed
        && safe_redesign.left.resolved
        && safe_redesign.right.resolved;

    Ok(Phase4OfflineResult {
        algorithm_version: PHASE4_OFFLINE_ALGORITHM_VERSION,
        verification_state: OfflineVerificationState::PredictedOnlyMeasured,
        numerical_passed,
        sample_rate_hz: input.sample_rate_hz,
        fir_fft_size: config.fir_fft_size,
        measured_frequencies_hz: input.frequencies_hz.clone(),
        design_frequencies_hz,
        native_frequencies_hz,
        position_weights,
        left_design,
        right_design,
        measured_grid_stereo_design,
        native_stereo_design,
        left_fir,
        right_fir,
        left_realized_response_db,
        right_realized_response_db,
        left_predicted_position_db,
        right_predicted_position_db,
        left_validation,
        right_validation,
        protected_dip_validation,
        safe_redesign,
        time_domain_diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_gaussian(frequency_hz: f64, center_hz: f64, width_octaves: f64, gain_db: f64) -> f64 {
        let distance = (frequency_hz / center_hz).log2();
        gain_db * (-4.0 * std::f64::consts::LN_2 * (distance / width_octaves).powi(2)).exp()
    }

    fn measured_input(position_count: usize, timed_ir: bool) -> MeasuredStereoResponseSet {
        let frequencies_hz: Vec<f64> = (20..=1_000).map(f64::from).collect();
        let target = TargetCurve::preset(TargetPreset::BkStyle)
            .evaluate(&frequencies_hz)
            .unwrap();
        let positions = (0..position_count)
            .map(|position| {
                let offset = position as f64 * 0.08;
                let left_magnitude_db = frequencies_hz
                    .iter()
                    .zip(&target)
                    .map(|(frequency, target)| {
                        target
                            + log_gaussian(*frequency, 55.0, 0.28, 6.0 + offset)
                            + log_gaussian(*frequency, 83.0, 0.055, -14.0)
                            + log_gaussian(*frequency, 260.0, 0.35, 3.5)
                    })
                    .collect();
                let right_magnitude_db = frequencies_hz
                    .iter()
                    .zip(&target)
                    .map(|(frequency, target)| {
                        target
                            + log_gaussian(*frequency, 55.0, 0.28, 5.8 - offset)
                            + log_gaussian(*frequency, 84.0, 0.055, -14.0)
                            + log_gaussian(*frequency, 340.0, 0.35, 3.8)
                    })
                    .collect();
                let impulse = timed_ir.then(|| TimedCombinedImpulse {
                    samples: vec![0.0, 0.5, -0.1, 0.02],
                    start_time_seconds: -1.0 + position as f64 * 0.001,
                    arrival_time_seconds: Some(0.003 + position as f64 * 0.001),
                });
                MeasuredStereoPosition {
                    id: format!("P{position}"),
                    weight: if position == 0 { 2.0 } else { 1.0 },
                    left_magnitude_db,
                    right_magnitude_db,
                    left_timed_combined_ir: impulse.clone(),
                    right_timed_combined_ir: impulse,
                }
            })
            .collect();
        MeasuredStereoResponseSet {
            sample_rate_hz: 48_000,
            frequencies_hz,
            positions,
        }
    }

    #[test]
    fn default_is_explicit_48k_predicted_only_response_design() {
        let config = Phase4OfflineConfig::default();
        assert_eq!(config.fir_fft_size, 16_384);
        let result = run_phase4_offline(&measured_input(1, true), &config).unwrap();
        assert_eq!(
            result.verification_state.as_str(),
            "predicted-only-measured"
        );
        assert_eq!(result.fir_fft_size, config.fir_fft_size);
        assert_eq!(result.left_fir.taps.len(), config.fir_fft_size);
        assert_eq!(result.right_fir.taps.len(), config.fir_fft_size);
        assert!(
            result.numerical_passed,
            "left={:?}, right={:?}, protected={:?}",
            result.left_validation.issues,
            result.right_validation.issues,
            result.protected_dip_validation
        );
        assert!(result
            .left_validation
            .metrics
            .predicted_impulse_peak
            .is_some());
        assert_eq!(result.time_domain_diagnostics.len(), 2);
        assert!(result.time_domain_diagnostics.iter().all(|diagnostic| {
            diagnostic.input_start_time_seconds == diagnostic.predicted_start_time_seconds
                && diagnostic.predicted_sample_count
                    == diagnostic.input_sample_count + config.fir_fft_size - 1
                && diagnostic.maximum_absolute_ir_sample.is_finite()
        }));
        assert!(result
            .left_design
            .warnings
            .contains(&CorrectionWarning::SinglePositionOnly));
    }

    #[test]
    fn native_grid_preserves_bounded_gain_and_is_unity_outside_design_band() {
        let measured_frequencies: Vec<f64> = (20..=700).map(f64::from).collect();
        let measured_gain = measured_frequencies
            .iter()
            .map(|frequency| {
                if *frequency < 100.0 {
                    3.0
                } else if *frequency < 640.0 {
                    -4.0
                } else {
                    -0.1
                }
            })
            .collect::<Vec<_>>();
        let native = native_frequency_grid(48_000, 16_384);
        let result = resample_correction_to_native_grid(
            &measured_frequencies,
            &measured_gain,
            &native,
            20.0,
            650.0,
        )
        .unwrap();
        assert!(result
            .iter()
            .all(|gain| *gain <= MAXIMUM_SUPPORTED_BOOST_DB));
        assert!(result.iter().any(|gain| *gain > 2.5));
        // Below the band a short raised-cosine fade (three native bins) makes
        // the edge continuous instead of a one-bin step; beneath the fade and
        // above the taper end the response is exactly unity.
        let native_spacing_hz = native[1] - native[0];
        let fade_start_hz = 20.0 - 3.0 * native_spacing_hz;
        let edge_gain_db = 3.0;
        for (frequency, gain) in native.iter().zip(&result) {
            if *frequency <= fade_start_hz || *frequency >= 650.0 {
                assert_eq!(*gain, 0.0, "native bin {frequency} Hz was not unity");
            } else if *frequency < 20.0 {
                assert!(
                    *gain >= 0.0 && *gain <= edge_gain_db,
                    "fade bin {frequency} Hz left the edge-gain fade range: {gain}"
                );
            }
        }
        // The fade removes the hard band-edge step: no adjacent-bin jump
        // below 30 Hz may exceed the edge gain scaled by one fade step.
        let mut previous = 0.0_f64;
        for (frequency, gain) in native.iter().zip(&result) {
            if *frequency > 30.0 {
                break;
            }
            assert!(
                (gain - previous).abs() <= edge_gain_db * 0.75,
                "band edge still steps by {} dB at {frequency} Hz",
                (gain - previous).abs()
            );
            previous = *gain;
        }
    }

    #[test]
    fn multi_position_prediction_is_deterministic_and_has_no_ir_peak_without_ir() {
        let input = measured_input(2, false);
        let config = Phase4OfflineConfig {
            fir_fft_size: 4_096,
            ..Phase4OfflineConfig::default()
        };
        let first = run_phase4_offline(&input, &config).unwrap();
        let second = run_phase4_offline(&input, &config).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.position_weights, vec![2.0, 1.0]);
        assert_eq!(first.left_validation.metrics.predicted_impulse_peak, None);
        assert!(first.time_domain_diagnostics.is_empty());
        assert!(first
            .left_design
            .warnings
            .iter()
            .all(|warning| warning != &CorrectionWarning::SinglePositionOnly));
    }

    #[test]
    fn multi_position_phase4_path_preserves_bounded_boost_and_protected_dips() {
        let mut input = measured_input(3, false);
        for position in &mut input.positions {
            for (index, frequency) in input.frequencies_hz.iter().enumerate() {
                if (115.0..=175.0).contains(frequency) {
                    position.left_magnitude_db[index] -= 3.5;
                    position.right_magnitude_db[index] -= 3.5;
                }
            }
        }
        // Product FIR resolution. A 4,096-tap FIR (~11.7 Hz resolution)
        // physically cannot render the ±0.1-octave protection notch at 84 Hz
        // beside the 55 Hz cut, and before the null-skirt boost guard existed
        // this scenario only passed because a boost manufactured from the
        // null's own smoothing skirt lifted the realized response back over
        // the dip (2026-07-28 release review). At the shipped 16,384-tap
        // resolution the realized protected-dip attenuation is ~0.001 dB; a
        // coarser configuration failing the realized gate is the fail-closed
        // design working, not a passing case to assert.
        let config = Phase4OfflineConfig::default();
        let result = run_phase4_offline(&input, &config).unwrap();
        let maximum_gain = result
            .measured_grid_stereo_design
            .left_gain_db
            .iter()
            .chain(&result.measured_grid_stereo_design.right_gain_db)
            .copied()
            .fold(0.0_f64, f64::max);
        assert!(maximum_gain > 1.0);
        assert!(maximum_gain <= MAXIMUM_SUPPORTED_BOOST_DB);
        assert!(result
            .left_design
            .boost_eligible
            .iter()
            .any(|eligible| *eligible));
        assert!(result
            .right_design
            .boost_eligible
            .iter()
            .any(|eligible| *eligible));
        assert!(result.protected_dip_validation.left_passed);
        assert!(result.protected_dip_validation.right_passed);
    }

    #[test]
    fn protected_dip_gate_detects_excess_attenuation() {
        let mask = [false, true, false];
        let response = [0.0, -0.75, -3.0];
        let flat_design = [0.0, 0.0, 0.0];
        let maximum = maximum_protected_attenuation_db(&mask, &response, &flat_design).unwrap();
        assert_eq!(maximum, 0.75);
        // A deliberately bounded designed cut inside the protected region is
        // not counted; only realization *beyond* the design is.
        let bounded_design = [0.0, -0.7, 0.0];
        let excess = maximum_protected_attenuation_db(&mask, &response, &bounded_design).unwrap();
        assert!((excess - 0.05).abs() < 1.0e-12, "{excess}");
        assert!(maximum > Phase4OfflineConfig::default().maximum_protected_dip_attenuation_db);

        let response = [0.0, 0.1, 3.0];
        let maximum = maximum_protected_boost_db(&mask, &response).unwrap();
        assert_eq!(maximum, 0.1);
        assert!(maximum > Phase4OfflineConfig::default().maximum_protected_dip_boost_db);
    }

    #[test]
    fn over_limit_request_is_recorded_and_uniformly_redesigned() {
        let mut input = measured_input(1, false);
        for (index, frequency) in input.frequencies_hz.iter().copied().enumerate() {
            let excess = log_gaussian(frequency, 40.0, 0.35, 20.0);
            input.positions[0].left_magnitude_db[index] += excess;
            input.positions[0].right_magnitude_db[index] += excess;
        }
        let config = Phase4OfflineConfig {
            fir_fft_size: 4_096,
            ..Phase4OfflineConfig::default()
        };
        let result = run_phase4_offline(&input, &config).unwrap();
        for redesign in [&result.safe_redesign.left, &result.safe_redesign.right] {
            assert!(redesign.applied);
            assert!(redesign.original_requested_maximum_attenuation_db > 12.0);
            assert!(redesign.redesign_strength > 0.0 && redesign.redesign_strength < 1.0);
            assert!(
                redesign.redesigned_maximum_attenuation_db
                    <= config.safe_redesign_maximum_attenuation_db + 1.0e-9
            );
            assert!(redesign.resolved);
        }
        assert!(result
            .left_design
            .warnings
            .iter()
            .any(|warning| matches!(warning, CorrectionWarning::AttenuationLimitReached { .. })));
    }

    #[test]
    fn rejects_invalid_grids_shapes_config_and_timed_ir() {
        let mut duplicate_grid = measured_input(1, false);
        duplicate_grid.frequencies_hz[1] = duplicate_grid.frequencies_hz[0];
        assert!(run_phase4_offline(&duplicate_grid, &Phase4OfflineConfig::default()).is_err());

        let mut wrong_shape = measured_input(1, false);
        wrong_shape.positions[0].left_magnitude_db.pop();
        assert!(run_phase4_offline(&wrong_shape, &Phase4OfflineConfig::default()).is_err());

        let mut wrong_rate = measured_input(1, false);
        wrong_rate.sample_rate_hz = 44_100;
        assert!(run_phase4_offline(&wrong_rate, &Phase4OfflineConfig::default()).is_err());

        let mut invalid_ir = measured_input(1, true);
        invalid_ir.positions[0]
            .left_timed_combined_ir
            .as_mut()
            .unwrap()
            .start_time_seconds = f64::NAN;
        assert!(run_phase4_offline(&invalid_ir, &Phase4OfflineConfig::default()).is_err());

        let invalid_config = Phase4OfflineConfig {
            fir_fft_size: 3_001,
            ..Phase4OfflineConfig::default()
        };
        assert!(run_phase4_offline(&measured_input(1, false), &invalid_config).is_err());

        let invalid_config = Phase4OfflineConfig {
            maximum_protected_dip_attenuation_db: 0.51,
            ..Phase4OfflineConfig::default()
        };
        assert!(run_phase4_offline(&measured_input(1, false), &invalid_config).is_err());

        let invalid_config = Phase4OfflineConfig {
            maximum_protected_dip_boost_db: 0.051,
            ..Phase4OfflineConfig::default()
        };
        assert!(run_phase4_offline(&measured_input(1, false), &invalid_config).is_err());
    }
}
