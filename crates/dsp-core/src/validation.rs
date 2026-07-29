//! Predicted convolution and automatic numerical validation.

use crate::analysis::frequency_response;
use crate::correction::MAXIMUM_SUPPORTED_BOOST_DB;
use crate::fir::FirFilter;
use crate::spatial::weighted_energy_average_db;
use crate::target::interpolate_log_frequency_grid;
use crate::{DspError, DspResult};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

const MINIMUM_PEAK_RMSE_IMPROVEMENT_FRACTION: f64 = 0.10;
const MINIMUM_MAX_PEAK_IMPROVEMENT_FRACTION: f64 = 0.05;
const MAXIMUM_NO_OP_RMSE_DB: f64 = 0.01;
const MAXIMUM_REALIZED_BOOST_DB: f64 = MAXIMUM_SUPPORTED_BOOST_DB;
const MAXIMUM_REALIZED_ATTENUATION_DB: f64 = 12.05;

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationThresholds {
    pub required_peak_rmse_improvement_fraction: f64,
    pub required_max_peak_error_improvement_fraction: f64,
    pub required_overall_rmse_improvement_fraction: f64,
    /// Below this raw error, an identity/no-op result is accepted as already
    /// converged instead of requiring a mathematically impossible improvement.
    pub minimum_actionable_rmse_db: f64,
    pub maximum_positive_correction_db: f64,
    pub maximum_attenuation_db: f64,
}

impl Default for ValidationThresholds {
    fn default() -> Self {
        Self {
            required_peak_rmse_improvement_fraction: MINIMUM_PEAK_RMSE_IMPROVEMENT_FRACTION,
            required_max_peak_error_improvement_fraction: MINIMUM_MAX_PEAK_IMPROVEMENT_FRACTION,
            required_overall_rmse_improvement_fraction: 0.0,
            minimum_actionable_rmse_db: MAXIMUM_NO_OP_RMSE_DB,
            maximum_positive_correction_db: MAXIMUM_REALIZED_BOOST_DB,
            maximum_attenuation_db: MAXIMUM_REALIZED_ATTENUATION_DB,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PredictionMetrics {
    pub raw_rmse_db: f64,
    pub predicted_rmse_db: f64,
    pub raw_peak_rmse_db: f64,
    pub predicted_peak_rmse_db: f64,
    pub raw_max_peak_error_db: f64,
    pub predicted_max_peak_error_db: f64,
    pub maximum_correction_gain_db: f64,
    pub maximum_correction_attenuation_db: f64,
    /// Maximum absolute sample in an optionally supplied predicted room-IR
    /// convolution. This is not a program-signal peak or an inter-sample true
    /// peak, and is absent for response-only prediction.
    pub predicted_impulse_peak: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationIssue {
    NonFiniteOutput,
    PositiveCorrectionGain { maximum_db: f64, allowed_db: f64 },
    ExcessiveAttenuation { maximum_db: f64, allowed_db: f64 },
    PeakRmseDidNotImprove { raw_db: f64, predicted_db: f64 },
    MaximumPeakDidNotImprove { raw_db: f64, predicted_db: f64 },
    OverallRmseDidNotImprove { raw_db: f64, predicted_db: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationReport {
    pub passed: bool,
    pub issues: Vec<ValidationIssue>,
    pub metrics: PredictionMetrics,
    pub frequencies_hz: Vec<f64>,
    pub raw_spatial_average_db: Vec<f64>,
    pub predicted_spatial_average_db: Vec<f64>,
    pub aligned_target_db: Vec<f64>,
}

/// Deterministic FFT overlap-free linear convolution for offline prediction.
pub fn fft_convolve(signal: &[f64], kernel: &[f64]) -> DspResult<Vec<f64>> {
    if signal.is_empty() || kernel.is_empty() {
        return Err(DspError::EmptyInput("convolution operands"));
    }
    if let Some(index) = signal.iter().position(|value| !value.is_finite()) {
        return Err(DspError::NonFinite {
            context: "convolution signal",
            index,
        });
    }
    if let Some(index) = kernel.iter().position(|value| !value.is_finite()) {
        return Err(DspError::NonFinite {
            context: "convolution kernel",
            index,
        });
    }
    let output_len = signal
        .len()
        .checked_add(kernel.len())
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| DspError::InvalidArgument("convolution length overflow".into()))?;
    let fft_size = output_len.next_power_of_two();
    let mut left = vec![Complex::new(0.0, 0.0); fft_size];
    let mut right = vec![Complex::new(0.0, 0.0); fft_size];
    for (bin, &sample) in left.iter_mut().zip(signal) {
        bin.re = sample;
    }
    for (bin, &sample) in right.iter_mut().zip(kernel) {
        bin.re = sample;
    }
    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_size);
    forward.process(&mut left);
    forward.process(&mut right);
    for (left_bin, right_bin) in left.iter_mut().zip(right) {
        *left_bin *= right_bin;
    }
    planner.plan_fft_inverse(fft_size).process(&mut left);
    let scale = 1.0 / fft_size as f64;
    let output: Vec<f64> = left
        .iter()
        .take(output_len)
        .map(|value| value.re * scale)
        .collect();
    if let Some(index) = output.iter().position(|value| !value.is_finite()) {
        return Err(DspError::NonFinite {
            context: "convolution output",
            index,
        });
    }
    Ok(output)
}

fn rmse(errors: impl Iterator<Item = f64>) -> Option<f64> {
    let values: Vec<f64> = errors.collect();
    if values.is_empty() {
        return None;
    }
    Some((values.iter().map(|value| value * value).sum::<f64>() / values.len() as f64).sqrt())
}

/// Suppress isolated-bin FFT ripple before scoring modal peaks. The nominal
/// 1/12-octave Gaussian window is applied in log-frequency space, matching the
/// design's conservative feature scale.
///
/// NOTE (2026-07-28 release review): the sigma conversion below shares the
/// design smoother's deliberate `FWHM/(2·√ln2)` convention (see
/// `correction.rs::gaussian_log_smooth`), so the realized width is √2× the
/// nominal value (≈0.118 octave). Design and validation must keep the same
/// convention so they score the same feature scale; the effective width is
/// documented in docs/validation.md.
pub const LOG_FREQUENCY_SMOOTHING_FWHM_OCTAVES: f64 = 1.0 / 12.0;

/// The width `log_frequency_smooth` actually realizes, in octaves
/// (nominal FWHM × √2). Recorded in evidence instead of the nominal value.
pub const LOG_FREQUENCY_SMOOTHING_EFFECTIVE_FWHM_OCTAVES: f64 =
    LOG_FREQUENCY_SMOOTHING_FWHM_OCTAVES * std::f64::consts::SQRT_2;

fn log_frequency_smooth(frequencies_hz: &[f64], values: &[f64]) -> Vec<f64> {
    let sigma = LOG_FREQUENCY_SMOOTHING_FWHM_OCTAVES / (2.0 * (2.0_f64.ln()).sqrt());
    let radius = 3.5 * sigma;
    let mut smoothed = Vec::with_capacity(values.len());
    for (index, &frequency) in frequencies_hz.iter().enumerate() {
        if frequency <= 0.0 {
            smoothed.push(values[index]);
            continue;
        }
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
        smoothed.push(if weight_sum > 0.0 {
            weighted_sum / weight_sum
        } else {
            values[index]
        });
    }
    smoothed
}

/// Compare two magnitude responses after smoothing their signed difference on
/// the same 1/12-octave scale used by broad-peak validation.
///
/// This suppresses narrow comb-filter shifts caused by small microphone
/// repositioning without removing broadband level or response errors.
pub fn log_frequency_smoothed_rmse_db(
    frequencies_hz: &[f64],
    reference_db: &[f64],
    candidate_db: &[f64],
) -> DspResult<f64> {
    if frequencies_hz.is_empty()
        || frequencies_hz.len() != reference_db.len()
        || frequencies_hz.len() != candidate_db.len()
    {
        return Err(DspError::ShapeMismatch(
            "smoothed RMSE frequency/reference/candidate grids must have equal nonzero lengths"
                .into(),
        ));
    }
    for (index, &frequency) in frequencies_hz.iter().enumerate() {
        if !frequency.is_finite() || frequency <= 0.0 {
            return Err(DspError::InvalidArgument(format!(
                "smoothed RMSE frequency bin {index} must be finite and positive"
            )));
        }
        if index > 0 && frequency <= frequencies_hz[index - 1] {
            return Err(DspError::InvalidArgument(
                "smoothed RMSE frequency grid must be strictly increasing".into(),
            ));
        }
    }
    let difference_db = reference_db
        .iter()
        .zip(candidate_db)
        .enumerate()
        .map(|(index, (reference, candidate))| {
            if !reference.is_finite() || !candidate.is_finite() {
                return Err(DspError::NonFinite {
                    context: "smoothed RMSE response",
                    index,
                });
            }
            Ok(candidate - reference)
        })
        .collect::<DspResult<Vec<_>>>()?;
    let smoothed_difference_db = log_frequency_smooth(frequencies_hz, &difference_db);
    // Log-frequency quadrature (2026-07-29 expert review, finding 5): on the
    // linear FFT grid the 20-100 Hz region - where nearly all correction and
    // capture noise lives - carries about 10% of the bins over 20-650 Hz, so
    // an unweighted RMSE structurally underweights exactly the corrected
    // band. Each bin is weighted by its log-frequency cell width instead, so
    // every octave contributes equally.
    let mut weighted_square_sum = 0.0;
    let mut weight_sum = 0.0;
    for (index, difference) in smoothed_difference_db.iter().enumerate() {
        let lower = if index == 0 {
            frequencies_hz[0]
        } else {
            (frequencies_hz[index - 1] * frequencies_hz[index]).sqrt()
        };
        let upper = if index + 1 == frequencies_hz.len() {
            frequencies_hz[index]
        } else {
            (frequencies_hz[index] * frequencies_hz[index + 1]).sqrt()
        };
        let weight = if upper > lower {
            (upper / lower).ln()
        } else {
            0.0
        };
        weighted_square_sum += weight * difference * difference;
        weight_sum += weight;
    }
    if weight_sum <= 0.0 {
        return rmse(smoothed_difference_db.into_iter())
            .ok_or(DspError::EmptyInput("smoothed RMSE response"));
    }
    Ok((weighted_square_sum / weight_sum).sqrt())
}

/// The gate smoother, exposed so callers can fit derived quantities (for
/// example the applied-correction scale) on exactly the curve the gate sees.
pub fn log_frequency_smoothed_curve(frequencies_hz: &[f64], values_db: &[f64]) -> Vec<f64> {
    log_frequency_smooth(frequencies_hz, values_db)
}

fn validate_thresholds(thresholds: &ValidationThresholds) -> DspResult<()> {
    let fractions = [
        thresholds.required_peak_rmse_improvement_fraction,
        thresholds.required_max_peak_error_improvement_fraction,
        thresholds.required_overall_rmse_improvement_fraction,
    ];
    if fractions
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(DspError::InvalidArgument(
            "validation improvement fractions must be finite and between zero and one".into(),
        ));
    }
    if thresholds.required_peak_rmse_improvement_fraction < MINIMUM_PEAK_RMSE_IMPROVEMENT_FRACTION
        || thresholds.required_max_peak_error_improvement_fraction
            < MINIMUM_MAX_PEAK_IMPROVEMENT_FRACTION
    {
        return Err(DspError::InvalidArgument(
            "validation peak-improvement thresholds cannot weaken the product safety minima".into(),
        ));
    }
    if !thresholds.minimum_actionable_rmse_db.is_finite()
        || thresholds.minimum_actionable_rmse_db < 0.0
        || thresholds.minimum_actionable_rmse_db > MAXIMUM_NO_OP_RMSE_DB
        || !thresholds.maximum_positive_correction_db.is_finite()
        || thresholds.maximum_positive_correction_db < 0.0
        || thresholds.maximum_positive_correction_db > MAXIMUM_REALIZED_BOOST_DB
        || !thresholds.maximum_attenuation_db.is_finite()
        || thresholds.maximum_attenuation_db <= 0.0
        || thresholds.maximum_attenuation_db > MAXIMUM_REALIZED_ATTENUATION_DB
    {
        return Err(DspError::InvalidArgument(
            "validation limits must be finite, nonnegative where applicable, and no weaker than the product safety ceilings".into(),
        ));
    }
    Ok(())
}

/// Score raw and predicted measured magnitudes on their original frequency
/// grid. The realized correction response may use a denser grid because only
/// its finite gain extrema are needed for the safety gates.
///
/// This function deliberately accepts no verification-state argument. Passing
/// an offline prediction can establish numerical fitness only; it cannot imply
/// that a filter was applied to hardware and remeasured.
#[allow(clippy::too_many_arguments)]
pub fn validate_frequency_prediction(
    frequencies_hz: &[f64],
    raw_position_levels_db: &[Vec<f64>],
    predicted_position_levels_db: &[Vec<f64>],
    weights: &[f64],
    aligned_target_db: &[f64],
    realized_correction_response_db: &[f64],
    predicted_impulse_peak: Option<f64>,
    thresholds: &ValidationThresholds,
) -> DspResult<ValidationReport> {
    validate_thresholds(thresholds)?;
    if frequencies_hz.len() < 2 {
        return Err(DspError::EmptyInput("prediction frequency grid"));
    }
    for (index, &frequency) in frequencies_hz.iter().enumerate() {
        if !frequency.is_finite() || frequency < 0.0 {
            return Err(DspError::InvalidArgument(format!(
                "prediction frequency bin {index} must be finite and nonnegative"
            )));
        }
        if index > 0 && frequency <= frequencies_hz[index - 1] {
            return Err(DspError::InvalidArgument(
                "prediction frequency grid must be strictly increasing".into(),
            ));
        }
    }
    if raw_position_levels_db.is_empty() {
        return Err(DspError::EmptyInput("raw prediction responses"));
    }
    if raw_position_levels_db.len() != predicted_position_levels_db.len()
        || raw_position_levels_db.len() != weights.len()
    {
        return Err(DspError::ShapeMismatch(
            "raw/predicted responses and spatial weights must have equal position counts".into(),
        ));
    }
    if aligned_target_db.len() != frequencies_hz.len() {
        return Err(DspError::ShapeMismatch(
            "prediction frequency and aligned-target grids must have equal lengths".into(),
        ));
    }
    if raw_position_levels_db
        .iter()
        .chain(predicted_position_levels_db)
        .any(|response| response.len() != frequencies_hz.len())
    {
        return Err(DspError::ShapeMismatch(
            "every raw and predicted response must match the prediction frequency grid".into(),
        ));
    }
    if realized_correction_response_db.is_empty() {
        return Err(DspError::EmptyInput("realized correction response"));
    }
    if let Some(peak) = predicted_impulse_peak {
        if !peak.is_finite() || peak < 0.0 {
            return Err(DspError::InvalidArgument(
                "predicted IR absolute peak must be finite and nonnegative when supplied".into(),
            ));
        }
    }

    let raw_spatial_average_db = weighted_energy_average_db(raw_position_levels_db, weights)?;
    let predicted_spatial_average_db =
        weighted_energy_average_db(predicted_position_levels_db, weights)?;

    let correction_bins: Vec<usize> = frequencies_hz
        .iter()
        .enumerate()
        .filter_map(|(index, frequency)| (20.0..=500.0).contains(frequency).then_some(index))
        .collect();
    if correction_bins.is_empty() {
        return Err(DspError::InvalidArgument(
            "validation FFT contains no bins in the 20-500 Hz correction band".into(),
        ));
    }
    let raw_rmse_db = rmse(
        correction_bins
            .iter()
            .map(|&index| raw_spatial_average_db[index] - aligned_target_db[index]),
    )
    .expect("correction bins checked nonempty");
    let predicted_rmse_db = rmse(
        correction_bins
            .iter()
            .map(|&index| predicted_spatial_average_db[index] - aligned_target_db[index]),
    )
    .expect("correction bins checked nonempty");
    let raw_residual_db: Vec<f64> = raw_spatial_average_db
        .iter()
        .zip(aligned_target_db)
        .map(|(raw, target)| raw - target)
        .collect();
    let predicted_residual_db: Vec<f64> = predicted_spatial_average_db
        .iter()
        .zip(aligned_target_db)
        .map(|(predicted, target)| predicted - target)
        .collect();
    let raw_broad_residual_db = log_frequency_smooth(frequencies_hz, &raw_residual_db);
    let predicted_broad_residual_db = log_frequency_smooth(frequencies_hz, &predicted_residual_db);
    let peak_bins: Vec<usize> = correction_bins
        .iter()
        .copied()
        .filter(|&index| raw_broad_residual_db[index] >= 1.0)
        .collect();
    let raw_peak_rmse_db = rmse(
        peak_bins
            .iter()
            .map(|&index| raw_broad_residual_db[index].max(0.0)),
    )
    .unwrap_or(0.0);
    let predicted_peak_rmse_db = rmse(
        peak_bins
            .iter()
            .map(|&index| predicted_broad_residual_db[index].max(0.0)),
    )
    .unwrap_or(0.0);
    let raw_max_peak_error_db = correction_bins
        .iter()
        .map(|&index| raw_broad_residual_db[index].max(0.0))
        .fold(0.0, f64::max);
    let predicted_max_peak_error_db = correction_bins
        .iter()
        .map(|&index| predicted_broad_residual_db[index].max(0.0))
        .fold(0.0, f64::max);
    let maximum_correction_gain_db = realized_correction_response_db
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let maximum_correction_attenuation_db = -realized_correction_response_db
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let metrics = PredictionMetrics {
        raw_rmse_db,
        predicted_rmse_db,
        raw_peak_rmse_db,
        predicted_peak_rmse_db,
        raw_max_peak_error_db,
        predicted_max_peak_error_db,
        maximum_correction_gain_db,
        maximum_correction_attenuation_db,
        predicted_impulse_peak,
    };

    let finite = raw_spatial_average_db
        .iter()
        .chain(&predicted_spatial_average_db)
        .chain(aligned_target_db)
        .chain(realized_correction_response_db.iter())
        .all(|value| value.is_finite())
        && [
            metrics.raw_rmse_db,
            metrics.predicted_rmse_db,
            metrics.raw_peak_rmse_db,
            metrics.predicted_peak_rmse_db,
            metrics.raw_max_peak_error_db,
            metrics.predicted_max_peak_error_db,
            metrics.maximum_correction_gain_db,
            metrics.maximum_correction_attenuation_db,
        ]
        .iter()
        .all(|value| value.is_finite())
        && match metrics.predicted_impulse_peak {
            Some(value) => value.is_finite() && value >= 0.0,
            None => true,
        };
    let mut issues = Vec::new();
    if !finite {
        issues.push(ValidationIssue::NonFiniteOutput);
    }
    if metrics.maximum_correction_gain_db > thresholds.maximum_positive_correction_db {
        issues.push(ValidationIssue::PositiveCorrectionGain {
            maximum_db: metrics.maximum_correction_gain_db,
            allowed_db: thresholds.maximum_positive_correction_db,
        });
    }
    if metrics.maximum_correction_attenuation_db > thresholds.maximum_attenuation_db {
        issues.push(ValidationIssue::ExcessiveAttenuation {
            maximum_db: metrics.maximum_correction_attenuation_db,
            allowed_db: thresholds.maximum_attenuation_db,
        });
    }
    if !peak_bins.is_empty() {
        let required_peak_rmse =
            metrics.raw_peak_rmse_db * (1.0 - thresholds.required_peak_rmse_improvement_fraction);
        if metrics.predicted_peak_rmse_db >= required_peak_rmse {
            issues.push(ValidationIssue::PeakRmseDidNotImprove {
                raw_db: metrics.raw_peak_rmse_db,
                predicted_db: metrics.predicted_peak_rmse_db,
            });
        }
        let required_max_peak = metrics.raw_max_peak_error_db
            * (1.0 - thresholds.required_max_peak_error_improvement_fraction);
        if metrics.predicted_max_peak_error_db >= required_max_peak {
            issues.push(ValidationIssue::MaximumPeakDidNotImprove {
                raw_db: metrics.raw_max_peak_error_db,
                predicted_db: metrics.predicted_max_peak_error_db,
            });
        }
    }
    let overall_failed = if metrics.raw_rmse_db <= thresholds.minimum_actionable_rmse_db {
        metrics.predicted_rmse_db > thresholds.minimum_actionable_rmse_db
    } else {
        let required_overall_rmse =
            metrics.raw_rmse_db * (1.0 - thresholds.required_overall_rmse_improvement_fraction);
        metrics.predicted_rmse_db >= required_overall_rmse
    };
    if overall_failed {
        issues.push(ValidationIssue::OverallRmseDidNotImprove {
            raw_db: metrics.raw_rmse_db,
            predicted_db: metrics.predicted_rmse_db,
        });
    }

    Ok(ValidationReport {
        passed: issues.is_empty(),
        issues,
        metrics,
        frequencies_hz: frequencies_hz.to_vec(),
        raw_spatial_average_db,
        predicted_spatial_average_db,
        aligned_target_db: aligned_target_db.to_vec(),
    })
}

/// Convolve every supplied IR with the FIR, re-analyze raw and predicted IRs
/// on one FFT grid, then delegate numerical scoring to the response-domain
/// validator. This remains the synthetic Phase 1 compatibility path.
pub fn validate_prediction(
    raw_impulses: &[Vec<f64>],
    weights: &[f64],
    fir: &FirFilter,
    design_frequencies_hz: &[f64],
    design_aligned_target_db: &[f64],
    thresholds: &ValidationThresholds,
) -> DspResult<ValidationReport> {
    if raw_impulses.is_empty() {
        return Err(DspError::EmptyInput("raw validation impulses"));
    }
    if raw_impulses.len() != weights.len() {
        return Err(DspError::ShapeMismatch(
            "validation impulses and spatial weights must have equal lengths".into(),
        ));
    }
    if design_frequencies_hz.len() != design_aligned_target_db.len() {
        return Err(DspError::ShapeMismatch(
            "validation design frequencies and target must have equal lengths".into(),
        ));
    }
    if raw_impulses.iter().any(Vec::is_empty) {
        return Err(DspError::EmptyInput("raw validation impulse"));
    }

    let predicted_impulses: Vec<Vec<f64>> = raw_impulses
        .iter()
        .map(|impulse| fft_convolve(impulse, &fir.taps))
        .collect::<DspResult<_>>()?;
    let maximum_ir_len = raw_impulses
        .iter()
        .map(Vec::len)
        .chain(predicted_impulses.iter().map(Vec::len))
        .max()
        .expect("nonempty checked above");
    let analysis_fft_size = maximum_ir_len.next_power_of_two();
    let raw_responses = raw_impulses
        .iter()
        .map(|impulse| frequency_response(impulse, fir.sample_rate_hz, analysis_fft_size))
        .collect::<DspResult<Vec<_>>>()?;
    let predicted_responses = predicted_impulses
        .iter()
        .map(|impulse| frequency_response(impulse, fir.sample_rate_hz, analysis_fft_size))
        .collect::<DspResult<Vec<_>>>()?;
    let frequencies_hz = raw_responses[0].frequencies_hz.clone();
    let raw_levels: Vec<Vec<f64>> = raw_responses
        .iter()
        .map(|response| response.magnitude_db.clone())
        .collect();
    let predicted_levels: Vec<Vec<f64>> = predicted_responses
        .iter()
        .map(|response| response.magnitude_db.clone())
        .collect();
    let aligned_target_db = interpolate_log_frequency_grid(
        design_frequencies_hz,
        design_aligned_target_db,
        &frequencies_hz,
    )?;
    let correction_response_db = fir.response_db(analysis_fft_size)?;
    let predicted_impulse_peak = predicted_impulses
        .iter()
        .flatten()
        .map(|sample| sample.abs())
        .fold(0.0, f64::max);

    validate_frequency_prediction(
        &frequencies_hz,
        &raw_levels,
        &predicted_levels,
        weights,
        &aligned_target_db,
        &correction_response_db,
        Some(predicted_impulse_peak),
        thresholds,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_convolution_matches_known_linear_result() {
        let result = fft_convolve(&[1.0, 2.0, 3.0], &[4.0, 5.0]).unwrap();
        let expected = [4.0, 13.0, 22.0, 15.0];
        for (actual, expected) in result.iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-12);
        }
    }

    #[test]
    fn convolution_rejects_non_finite_input() {
        assert!(fft_convolve(&[1.0, f64::NAN], &[1.0]).is_err());
        assert!(fft_convolve(&[f64::MAX, f64::MAX], &[2.0]).is_err());
    }

    #[test]
    fn smoothed_rmse_rejects_narrow_alternating_position_ripple_but_keeps_broad_error() {
        let frequencies_hz = (0..193)
            .map(|index| 20.0 * 2.0_f64.powf(index as f64 / 96.0))
            .collect::<Vec<_>>();
        let reference_db = vec![0.0; frequencies_hz.len()];
        let narrow_ripple_db = (0..frequencies_hz.len())
            .map(|index| if index % 2 == 0 { 6.0 } else { -6.0 })
            .collect::<Vec<_>>();
        let narrow_raw_rmse_db = rmse(
            reference_db
                .iter()
                .zip(&narrow_ripple_db)
                .map(|(reference, candidate)| candidate - reference),
        )
        .unwrap();
        let narrow_smoothed_rmse_db =
            log_frequency_smoothed_rmse_db(&frequencies_hz, &reference_db, &narrow_ripple_db)
                .unwrap();
        assert!(narrow_raw_rmse_db > 5.9);
        assert!(narrow_smoothed_rmse_db < 0.5);

        let broad_error_db = vec![4.0; frequencies_hz.len()];
        let broad_smoothed_rmse_db =
            log_frequency_smoothed_rmse_db(&frequencies_hz, &reference_db, &broad_error_db)
                .unwrap();
        assert!((broad_smoothed_rmse_db - 4.0).abs() < 1.0e-12);
    }

    #[test]
    fn smoothed_rmse_validates_response_grids() {
        assert!(log_frequency_smoothed_rmse_db(&[], &[], &[]).is_err());
        assert!(log_frequency_smoothed_rmse_db(&[20.0], &[0.0], &[]).is_err());
        assert!(log_frequency_smoothed_rmse_db(&[20.0, 20.0], &[0.0; 2], &[0.0; 2]).is_err());
        assert!(log_frequency_smoothed_rmse_db(&[20.0], &[f64::NAN], &[0.0]).is_err());
    }

    fn flat_validation_inputs() -> (Vec<Vec<f64>>, FirFilter, Vec<f64>, Vec<f64>) {
        let mut impulse = vec![0.0; 1_024];
        impulse[0] = 1.0;
        let filter = FirFilter {
            sample_rate_hz: 48_000,
            taps: vec![1.0],
            design_gain_db: vec![0.0],
            safety_normalization_db: 0.0,
        };
        (vec![impulse], filter, vec![0.0, 24_000.0], vec![0.0, 0.0])
    }

    #[test]
    fn flat_identity_with_no_peak_bins_is_a_finite_valid_no_op() {
        let (impulses, filter, frequencies, target) = flat_validation_inputs();
        let report = validate_prediction(
            &impulses,
            &[1.0],
            &filter,
            &frequencies,
            &target,
            &ValidationThresholds::default(),
        )
        .unwrap();
        assert!(report.passed, "issues: {:?}", report.issues);
        assert_eq!(report.metrics.raw_peak_rmse_db, 0.0);
        assert_eq!(report.metrics.predicted_peak_rmse_db, 0.0);
    }

    #[test]
    fn non_finite_threshold_is_rejected_before_validation() {
        let (impulses, filter, frequencies, target) = flat_validation_inputs();
        let thresholds = ValidationThresholds {
            required_peak_rmse_improvement_fraction: f64::NAN,
            ..ValidationThresholds::default()
        };
        assert!(validate_prediction(
            &impulses,
            &[1.0],
            &filter,
            &frequencies,
            &target,
            &thresholds,
        )
        .is_err());
    }

    #[test]
    fn configurable_thresholds_cannot_weaken_product_safety_limits() {
        let (impulses, filter, frequencies, target) = flat_validation_inputs();
        for thresholds in [
            ValidationThresholds {
                required_peak_rmse_improvement_fraction: 0.0,
                ..ValidationThresholds::default()
            },
            ValidationThresholds {
                minimum_actionable_rmse_db: 100.0,
                ..ValidationThresholds::default()
            },
            ValidationThresholds {
                maximum_positive_correction_db: MAXIMUM_SUPPORTED_BOOST_DB + 0.01,
                ..ValidationThresholds::default()
            },
            ValidationThresholds {
                maximum_attenuation_db: 100.0,
                ..ValidationThresholds::default()
            },
        ] {
            assert!(validate_prediction(
                &impulses,
                &[1.0],
                &filter,
                &frequencies,
                &target,
                &thresholds,
            )
            .is_err());
        }
    }
}
