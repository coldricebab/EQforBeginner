//! Recognition of a known sweep played through an asynchronous wireless path.
//!
//! This module deliberately has no file or audio-device dependencies. Callers
//! decode a mono reference WAV and microphone capture into finite `f64` sample
//! arrays, then pass them here. A zero-mean normalized cross-correlation finds
//! a distinctive reference probe. Several additional reference segments are
//! matched locally; their observed positions estimate the effective
//! capture/reference sample-rate ratio and reject isolated correlation hits.
//!
//! A successful match preserves the microphone capture timeline. It does not
//! move an impulse peak to zero and does not claim a hardware-synchronous
//! timing reference: wireless buffering is included in the reported start.

use crate::{DspError, DspResult};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

pub const WIRELESS_SWEEP_RECOGNITION_VERSION: &str = "wireless-sweep-recognition-v1";

const MIN_TEMPLATE_SAMPLES: usize = 64;
const MAX_TEMPLATE_SAMPLES: usize = 65_536;
const ABSOLUTE_MAX_REFERENCE_SAMPLES: usize = 4_000_000;
const ABSOLUTE_MAX_CAPTURE_SAMPLES: usize = 12_000_000;
const CORRELATION_BLOCK_LAGS: usize = 65_536;
const MIN_CENTERED_TEMPLATE_ENERGY: f64 = 1.0e-18;

/// Conservative thresholds for recognizing an imported mono sweep.
///
/// Defaults target 48 kHz measurement sweeps. They are product defaults rather
/// than hidden constants, so a persisted project can reproduce the decision.
#[derive(Debug, Clone, PartialEq)]
pub struct WirelessSweepRecognitionConfig {
    /// Nominal sample rate shared by the decoded reference and capture arrays.
    pub nominal_sample_rate_hz: u32,
    /// Size of the distinctive mid-sweep probe used for the global search.
    pub probe_samples: usize,
    /// Size of each locally matched segment used for drift estimation.
    pub segment_samples: usize,
    /// Number of evenly spaced segments retained away from the file edges.
    pub segment_count: usize,
    /// Minimum absolute normalized correlation for the global probe.
    pub minimum_start_correlation: f64,
    /// Minimum absolute normalized correlation for a drift segment.
    pub minimum_segment_correlation: f64,
    /// A second well-separated peak at or above this fraction of the strongest
    /// peak makes the recognition ambiguous.
    pub ambiguity_peak_ratio: f64,
    /// Radius ignored around the strongest peak when looking for a second
    /// playback. Zero selects half the reference length automatically.
    pub ambiguity_exclusion_samples: usize,
    /// Maximum accepted absolute clock mismatch in parts per million.
    pub maximum_clock_drift_ppm: f64,
    /// Fixed local-search allowance in addition to the configured drift range.
    pub segment_search_margin_samples: usize,
    /// Maximum RMS residual of the segment-position line fit.
    pub maximum_timing_fit_rms_samples: f64,
    /// Minimum accepted segments. Must be at least three.
    pub minimum_segment_matches: usize,
    /// Samples retained before and after the inferred sweep bounds.
    pub capture_pre_roll_samples: usize,
    pub capture_post_roll_samples: usize,
    /// Per-call limits that may only tighten the absolute module limits.
    pub maximum_reference_samples: usize,
    pub maximum_capture_samples: usize,
    /// Maximum accepted absolute decoded sample. This catches unit mistakes and
    /// prevents finite-but-overflowing correlation arithmetic.
    pub maximum_absolute_sample: f64,
}

impl Default for WirelessSweepRecognitionConfig {
    fn default() -> Self {
        Self {
            nominal_sample_rate_hz: 48_000,
            probe_samples: 4_096,
            segment_samples: 1_024,
            segment_count: 7,
            minimum_start_correlation: 0.45,
            minimum_segment_correlation: 0.35,
            ambiguity_peak_ratio: 0.90,
            ambiguity_exclusion_samples: 0,
            maximum_clock_drift_ppm: 2_000.0,
            segment_search_margin_samples: 128,
            maximum_timing_fit_rms_samples: 24.0,
            minimum_segment_matches: 4,
            capture_pre_roll_samples: 4_800,
            // Must cover the deconvolution IR tail read from the retained
            // segment: the v4 default keeps a 32,768-sample impulse, and the
            // worst admitted clock ratio stretches that to ~32,834 capture
            // samples. Deconvolving callers may still need to widen this when
            // they request a longer impulse.
            capture_post_roll_samples: 40_000,
            maximum_reference_samples: 2_000_000,
            maximum_capture_samples: 8_000_000,
            maximum_absolute_sample: 4.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WirelessSweepRejectionReason {
    /// No global correlation peak met the configured threshold.
    CorrelationBelowThreshold,
    /// A global peak existed, but independent sweep segments did not repeat it.
    InconsistentSegments,
    /// Segment matches implied a non-positive or excessive clock mismatch.
    ClockDriftOutOfRange,
    /// Segment locations did not follow one stable sample-rate ratio.
    TimingFitUnstable,
    /// The inferred beginning or end of the sweep was outside the capture.
    IncompleteCapture,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WirelessSweepNotDetected {
    pub reason: WirelessSweepRejectionReason,
    pub strongest_absolute_correlation: f64,
    pub required_absolute_correlation: f64,
}

/// A strong isolated peak that failed independent consistency checks.
///
/// Keeping this separate from `NotDetected` prevents a caller from treating an
/// arbitrary sound that resembles one sweep fragment as a valid measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct WirelessSweepLikelyFalsePositive {
    pub reason: WirelessSweepRejectionReason,
    pub candidate_start_sample: f64,
    pub start_absolute_correlation: f64,
    pub matched_segment_count: usize,
    pub required_segment_count: usize,
    pub estimated_clock_drift_ppm: Option<f64>,
    pub timing_fit_rms_samples: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WirelessSweepAmbiguous {
    pub strongest_candidate_start_sample: f64,
    pub second_candidate_start_sample: f64,
    pub strongest_absolute_correlation: f64,
    pub second_absolute_correlation: f64,
    pub second_to_strongest_ratio: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WirelessSweepSegmentMatch {
    /// Start offset of this template segment on the original reference grid.
    pub reference_offset_samples: usize,
    /// Matched segment start on the unmodified microphone capture timeline.
    pub capture_offset_samples: usize,
    pub signed_correlation: f64,
    pub absolute_correlation: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WirelessSweepCaptureSegment {
    pub start_sample: usize,
    pub end_sample_exclusive: usize,
    pub samples: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WirelessClockDriftEvidence {
    /// One imported sweep was divided into frequency-changing segments.
    ///
    /// This is useful for capture resampling and gross mismatch rejection, but
    /// room group delay can bias the estimate. It is not sufficient evidence
    /// for inter-channel acoustic timing correction. Repeated identical
    /// markers or sweeps require a separate, stronger measurement workflow.
    IntraSweepSegmentFit,
    /// Two repeated timing-marker events surrounding the measurement sweep.
    ///
    /// This qualifies an asynchronous sample-rate ratio for magnitude
    /// deconvolution. A separate acoustic-arrival model and repeated channel
    /// measurements are still required before inter-channel delay correction.
    RepeatedTimingMarkers,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WirelessSweepDetection {
    pub algorithm_version: &'static str,
    pub nominal_sample_rate_hz: u32,
    /// Fractional start estimate on the original capture timeline.
    pub estimated_sweep_start_sample: f64,
    /// Exclusive fractional end estimate on the original capture timeline.
    pub estimated_sweep_end_sample_exclusive: f64,
    pub start_signed_correlation: f64,
    pub start_absolute_correlation: f64,
    pub second_absolute_correlation: f64,
    pub polarity_inverted: bool,
    /// Observed capture samples per one reference sample.
    pub capture_samples_per_reference_sample: f64,
    /// `(capture_samples_per_reference_sample - 1) * 1e6`.
    pub estimated_clock_drift_ppm: f64,
    pub clock_drift_evidence: WirelessClockDriftEvidence,
    pub timing_fit_rms_samples: f64,
    pub segment_matches: Vec<WirelessSweepSegmentMatch>,
    pub capture_segment: WirelessSweepCaptureSegment,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WirelessSweepRecognition {
    Detected(WirelessSweepDetection),
    NotDetected(WirelessSweepNotDetected),
    LikelyFalsePositive(WirelessSweepLikelyFalsePositive),
    Ambiguous(WirelessSweepAmbiguous),
}

#[derive(Debug, Clone, Copy)]
struct CorrelationPeak {
    lag: usize,
    signed: f64,
}

#[derive(Debug, Clone, Copy)]
struct TimingFit {
    intercept: f64,
    slope: f64,
    residual_rms_samples: f64,
}

fn validate_config(config: &WirelessSweepRecognitionConfig) -> DspResult<()> {
    if !(8_000..=384_000).contains(&config.nominal_sample_rate_hz) {
        return Err(DspError::InvalidArgument(
            "wireless sweep nominal sample rate must be between 8000 and 384000 Hz".into(),
        ));
    }
    for (name, value) in [
        ("probe", config.probe_samples),
        ("drift segment", config.segment_samples),
    ] {
        if !(MIN_TEMPLATE_SAMPLES..=MAX_TEMPLATE_SAMPLES).contains(&value) {
            return Err(DspError::InvalidArgument(format!(
                "wireless sweep {name} length must be between {MIN_TEMPLATE_SAMPLES} and {MAX_TEMPLATE_SAMPLES} samples"
            )));
        }
    }
    if !(3..=32).contains(&config.segment_count) {
        return Err(DspError::InvalidArgument(
            "wireless sweep segment count must be between 3 and 32".into(),
        ));
    }
    if config.minimum_segment_matches < 3 || config.minimum_segment_matches > config.segment_count {
        return Err(DspError::InvalidArgument(
            "wireless sweep minimum segment matches must be between 3 and segment_count".into(),
        ));
    }
    for (name, threshold) in [
        (
            "minimum start correlation",
            config.minimum_start_correlation,
        ),
        (
            "minimum segment correlation",
            config.minimum_segment_correlation,
        ),
        ("ambiguity peak ratio", config.ambiguity_peak_ratio),
    ] {
        if !threshold.is_finite() || threshold <= 0.0 || threshold > 1.0 {
            return Err(DspError::InvalidArgument(format!(
                "wireless sweep {name} must be finite and in (0, 1]"
            )));
        }
    }
    if config.ambiguity_peak_ratio < 0.5 {
        return Err(DspError::InvalidArgument(
            "wireless sweep ambiguity peak ratio must be at least 0.5".into(),
        ));
    }
    if !config.maximum_clock_drift_ppm.is_finite()
        || config.maximum_clock_drift_ppm < 0.0
        || config.maximum_clock_drift_ppm > 50_000.0
    {
        return Err(DspError::InvalidArgument(
            "wireless sweep maximum clock drift must be finite and between 0 and 50000 ppm".into(),
        ));
    }
    if !config.maximum_timing_fit_rms_samples.is_finite()
        || config.maximum_timing_fit_rms_samples <= 0.0
        || config.maximum_timing_fit_rms_samples > f64::from(config.nominal_sample_rate_hz) / 2.0
    {
        return Err(DspError::InvalidArgument(
            "wireless sweep maximum timing-fit RMS must be finite, positive, and under 0.5 seconds"
                .into(),
        ));
    }
    if config.segment_search_margin_samples > config.nominal_sample_rate_hz as usize * 2 {
        return Err(DspError::InvalidArgument(
            "wireless sweep segment search margin must not exceed two seconds".into(),
        ));
    }
    if config.maximum_reference_samples == 0
        || config.maximum_reference_samples > ABSOLUTE_MAX_REFERENCE_SAMPLES
    {
        return Err(DspError::InvalidArgument(format!(
            "wireless sweep maximum reference samples must be between 1 and {ABSOLUTE_MAX_REFERENCE_SAMPLES}"
        )));
    }
    if config.maximum_capture_samples == 0
        || config.maximum_capture_samples > ABSOLUTE_MAX_CAPTURE_SAMPLES
    {
        return Err(DspError::InvalidArgument(format!(
            "wireless sweep maximum capture samples must be between 1 and {ABSOLUTE_MAX_CAPTURE_SAMPLES}"
        )));
    }
    if !config.maximum_absolute_sample.is_finite()
        || config.maximum_absolute_sample <= 0.0
        || config.maximum_absolute_sample > 1_000.0
    {
        return Err(DspError::InvalidArgument(
            "wireless sweep maximum absolute sample must be finite and in (0, 1000]".into(),
        ));
    }
    Ok(())
}

fn validate_samples(
    samples: &[f64],
    context: &'static str,
    maximum_samples: usize,
    maximum_absolute_sample: f64,
) -> DspResult<()> {
    if samples.is_empty() {
        return Err(DspError::EmptyInput(context));
    }
    if samples.len() > maximum_samples {
        return Err(DspError::InvalidArgument(format!(
            "{context} has {} samples, exceeding the configured safety limit {maximum_samples}",
            samples.len()
        )));
    }
    for (index, &sample) in samples.iter().enumerate() {
        if !sample.is_finite() {
            return Err(DspError::NonFinite { context, index });
        }
        if sample.abs() > maximum_absolute_sample {
            return Err(DspError::InvalidArgument(format!(
                "{context} sample {index} has absolute value {}, exceeding the configured safety limit {maximum_absolute_sample}",
                sample.abs()
            )));
        }
    }
    Ok(())
}

fn centered_template(template: &[f64], context: &'static str) -> DspResult<(Vec<f64>, f64)> {
    let mean = template.iter().sum::<f64>() / template.len() as f64;
    let centered: Vec<f64> = template.iter().map(|sample| sample - mean).collect();
    let energy = centered.iter().map(|sample| sample * sample).sum::<f64>();
    if !energy.is_finite() || energy <= MIN_CENTERED_TEMPLATE_ENERGY {
        return Err(DspError::InvalidArgument(format!(
            "{context} is silent or has insufficient variation for correlation"
        )));
    }
    Ok((centered, energy))
}

/// All valid-lag zero-mean normalized cross-correlations.
///
/// The template FFT is reused over fixed-size capture blocks, so memory stays
/// bounded even when callers use the configured multi-minute capture limit.
fn normalized_cross_correlations(signal: &[f64], template: &[f64]) -> DspResult<Vec<f64>> {
    if signal.len() < template.len() {
        return Err(DspError::ShapeMismatch(format!(
            "correlation signal length {} is shorter than template length {}",
            signal.len(),
            template.len()
        )));
    }
    let (centered, template_energy) = centered_template(template, "correlation template")?;
    let valid_lags = signal.len() - template.len() + 1;
    let block_lags = CORRELATION_BLOCK_LAGS.min(valid_lags);
    let convolution_len = block_lags
        .checked_add(template.len().saturating_mul(2))
        .and_then(|value| value.checked_sub(2))
        .ok_or_else(|| DspError::InvalidArgument("correlation FFT size overflow".into()))?;
    let fft_size = convolution_len
        .checked_next_power_of_two()
        .ok_or_else(|| DspError::InvalidArgument("correlation FFT size overflow".into()))?;

    let mut planner = FftPlanner::<f64>::new();
    let forward = planner.plan_fft_forward(fft_size);
    let inverse = planner.plan_fft_inverse(fft_size);

    let mut kernel = vec![Complex::new(0.0, 0.0); fft_size];
    for (slot, &sample) in kernel.iter_mut().zip(centered.iter().rev()) {
        slot.re = sample;
    }
    forward.process(&mut kernel);

    let mut buffer = vec![Complex::new(0.0, 0.0); fft_size];
    let mut correlations = vec![0.0; valid_lags];
    let inverse_scale = 1.0 / fft_size as f64;

    let mut block_start = 0;
    while block_start < valid_lags {
        let count = (valid_lags - block_start).min(block_lags);
        let signal_count = count + template.len() - 1;
        buffer.fill(Complex::new(0.0, 0.0));
        for (slot, &sample) in buffer
            .iter_mut()
            .zip(&signal[block_start..(block_start + signal_count)])
        {
            slot.re = sample;
        }
        forward.process(&mut buffer);
        for (value, &kernel_value) in buffer.iter_mut().zip(&kernel) {
            *value *= kernel_value;
        }
        inverse.process(&mut buffer);

        let first_window = &signal[block_start..(block_start + template.len())];
        let mut window_sum = first_window.iter().sum::<f64>();
        let mut window_square_sum = first_window
            .iter()
            .map(|sample| sample * sample)
            .sum::<f64>();
        for local_lag in 0..count {
            if local_lag > 0 {
                let removed = signal[block_start + local_lag - 1];
                let added = signal[block_start + local_lag + template.len() - 1];
                window_sum += added - removed;
                window_square_sum += added * added - removed * removed;
            }
            let centered_window_energy =
                (window_square_sum - window_sum * window_sum / template.len() as f64).max(0.0);
            let denominator = (template_energy * centered_window_energy).sqrt();
            let numerator_index = local_lag + template.len() - 1;
            let numerator = buffer[numerator_index].re * inverse_scale;
            let correlation = if denominator > MIN_CENTERED_TEMPLATE_ENERGY {
                (numerator / denominator).clamp(-1.0, 1.0)
            } else {
                0.0
            };
            if !correlation.is_finite() {
                return Err(DspError::NonFinite {
                    context: "normalized cross-correlation",
                    index: block_start + local_lag,
                });
            }
            correlations[block_start + local_lag] = correlation;
        }
        block_start += count;
    }

    Ok(correlations)
}

fn strongest_peak(
    correlations: &[f64],
    allowed_start: usize,
    allowed_end_exclusive: usize,
) -> CorrelationPeak {
    let mut peak = CorrelationPeak {
        lag: allowed_start,
        signed: correlations[allowed_start],
    };
    for (lag, &signed) in correlations
        .iter()
        .enumerate()
        .take(allowed_end_exclusive)
        .skip(allowed_start + 1)
    {
        if signed.abs() > peak.signed.abs() {
            peak = CorrelationPeak { lag, signed };
        }
    }
    peak
}

fn strongest_separated_peak(
    correlations: &[f64],
    allowed_start: usize,
    allowed_end_exclusive: usize,
    primary_lag: usize,
    exclusion_samples: usize,
) -> Option<CorrelationPeak> {
    correlations
        .iter()
        .enumerate()
        .take(allowed_end_exclusive)
        .skip(allowed_start)
        .filter(|(lag, _)| lag.abs_diff(primary_lag) > exclusion_samples)
        .map(|(lag, &signed)| CorrelationPeak { lag, signed })
        .max_by(|left, right| left.signed.abs().total_cmp(&right.signed.abs()))
}

fn direct_normalized_correlation(signal: &[f64], template: &[f64]) -> DspResult<f64> {
    let (centered, template_energy) = centered_template(template, "drift template")?;
    let signal_mean = signal.iter().sum::<f64>() / signal.len() as f64;
    let mut numerator = 0.0;
    let mut signal_energy = 0.0;
    for (&signal_sample, &template_sample) in signal.iter().zip(&centered) {
        let signal_centered = signal_sample - signal_mean;
        numerator += signal_centered * template_sample;
        signal_energy += signal_centered * signal_centered;
    }
    if signal_energy <= MIN_CENTERED_TEMPLATE_ENERGY {
        return Ok(0.0);
    }
    Ok((numerator / (template_energy * signal_energy).sqrt()).clamp(-1.0, 1.0))
}

fn best_local_match(
    capture: &[f64],
    template: &[f64],
    predicted_lag: f64,
    half_width: usize,
) -> DspResult<Option<CorrelationPeak>> {
    if capture.len() < template.len() {
        return Ok(None);
    }
    let maximum_lag = capture.len() - template.len();
    let center = predicted_lag.round();
    let start = (center - half_width as f64).ceil().max(0.0) as usize;
    let end = (center + half_width as f64).floor().min(maximum_lag as f64) as usize;
    if start > end {
        return Ok(None);
    }

    let mut best: Option<CorrelationPeak> = None;
    for lag in start..=end {
        let signed =
            direct_normalized_correlation(&capture[lag..(lag + template.len())], template)?;
        if best
            .as_ref()
            .is_none_or(|current| signed.abs() > current.signed.abs())
        {
            best = Some(CorrelationPeak { lag, signed });
        }
    }
    Ok(best)
}

fn segment_offsets(reference_len: usize, segment_len: usize, count: usize) -> Vec<usize> {
    let usable_start_range = reference_len - segment_len;
    (0..count)
        .map(|index| {
            // Avoid fade-ins and fade-outs while still spanning most of the file.
            ((index + 1) * usable_start_range) / (count + 1)
        })
        .collect()
}

fn fit_segment_timing(matches: &[WirelessSweepSegmentMatch]) -> Option<TimingFit> {
    if matches.len() < 2 {
        return None;
    }
    let count = matches.len() as f64;
    let mean_reference = matches
        .iter()
        .map(|entry| entry.reference_offset_samples as f64)
        .sum::<f64>()
        / count;
    let mean_capture = matches
        .iter()
        .map(|entry| entry.capture_offset_samples as f64)
        .sum::<f64>()
        / count;
    let variance = matches
        .iter()
        .map(|entry| {
            let centered = entry.reference_offset_samples as f64 - mean_reference;
            centered * centered
        })
        .sum::<f64>();
    if variance <= f64::EPSILON {
        return None;
    }
    let covariance = matches
        .iter()
        .map(|entry| {
            (entry.reference_offset_samples as f64 - mean_reference)
                * (entry.capture_offset_samples as f64 - mean_capture)
        })
        .sum::<f64>();
    let slope = covariance / variance;
    let intercept = mean_capture - slope * mean_reference;
    let residual_rms_samples = (matches
        .iter()
        .map(|entry| {
            let predicted = intercept + slope * entry.reference_offset_samples as f64;
            (entry.capture_offset_samples as f64 - predicted).powi(2)
        })
        .sum::<f64>()
        / count)
        .sqrt();
    if [slope, intercept, residual_rms_samples]
        .iter()
        .all(|value| value.is_finite())
    {
        Some(TimingFit {
            intercept,
            slope,
            residual_rms_samples,
        })
    } else {
        None
    }
}

fn false_positive(
    reason: WirelessSweepRejectionReason,
    candidate_start_sample: f64,
    start_absolute_correlation: f64,
    matched_segment_count: usize,
    required_segment_count: usize,
    fit: Option<TimingFit>,
) -> WirelessSweepRecognition {
    WirelessSweepRecognition::LikelyFalsePositive(WirelessSweepLikelyFalsePositive {
        reason,
        candidate_start_sample,
        start_absolute_correlation,
        matched_segment_count,
        required_segment_count,
        estimated_clock_drift_ppm: fit.map(|value| (value.slope - 1.0) * 1_000_000.0),
        timing_fit_rms_samples: fit.map(|value| value.residual_rms_samples),
    })
}

/// Recognize one complete playback of `reference` in `microphone_capture`.
///
/// Recognition failures are data outcomes, not malformed-input errors:
/// `NotDetected`, `LikelyFalsePositive`, and `Ambiguous` let the UI give an
/// actionable retry message. `DspError` is reserved for invalid configuration,
/// unsafe array sizes/amplitudes, and non-finite inputs.
pub fn recognize_wireless_sweep(
    reference: &[f64],
    microphone_capture: &[f64],
    config: &WirelessSweepRecognitionConfig,
) -> DspResult<WirelessSweepRecognition> {
    validate_config(config)?;
    validate_samples(
        reference,
        "wireless sweep reference",
        config.maximum_reference_samples,
        config.maximum_absolute_sample,
    )?;
    validate_samples(
        microphone_capture,
        "wireless microphone capture",
        config.maximum_capture_samples,
        config.maximum_absolute_sample,
    )?;
    if reference.len() < config.probe_samples || reference.len() < config.segment_samples {
        return Err(DspError::ShapeMismatch(format!(
            "wireless sweep reference length {} must be at least the probe length {} and segment length {}",
            reference.len(),
            config.probe_samples,
            config.segment_samples
        )));
    }
    if microphone_capture.len() < config.probe_samples {
        return Err(DspError::ShapeMismatch(format!(
            "wireless microphone capture length {} is shorter than probe length {}",
            microphone_capture.len(),
            config.probe_samples
        )));
    }
    centered_template(reference, "wireless sweep reference")?;

    let probe_offset = (reference.len() - config.probe_samples) / 2;
    let probe = &reference[probe_offset..(probe_offset + config.probe_samples)];
    let correlations = normalized_cross_correlations(microphone_capture, probe)?;

    // Permit the configured maximum drift while excluding peaks that cannot
    // correspond to a substantially complete reference playback.
    let drift_span =
        (reference.len() as f64 * config.maximum_clock_drift_ppm / 1_000_000.0).ceil() as usize;
    let allowed_start = probe_offset.saturating_sub(drift_span);
    let minimum_tail = reference
        .len()
        .saturating_sub(probe_offset + config.probe_samples);
    let allowed_end_exclusive = correlations
        .len()
        .saturating_sub(minimum_tail.saturating_sub(drift_span));
    if allowed_start >= allowed_end_exclusive {
        return Ok(WirelessSweepRecognition::NotDetected(
            WirelessSweepNotDetected {
                reason: WirelessSweepRejectionReason::IncompleteCapture,
                strongest_absolute_correlation: 0.0,
                required_absolute_correlation: config.minimum_start_correlation,
            },
        ));
    }

    let primary = strongest_peak(&correlations, allowed_start, allowed_end_exclusive);
    let primary_absolute = primary.signed.abs();
    if primary_absolute < config.minimum_start_correlation {
        return Ok(WirelessSweepRecognition::NotDetected(
            WirelessSweepNotDetected {
                reason: WirelessSweepRejectionReason::CorrelationBelowThreshold,
                strongest_absolute_correlation: primary_absolute,
                required_absolute_correlation: config.minimum_start_correlation,
            },
        ));
    }

    let exclusion_samples = if config.ambiguity_exclusion_samples == 0 {
        (reference.len() / 2).max(config.probe_samples)
    } else {
        config.ambiguity_exclusion_samples
    };
    let secondary = strongest_separated_peak(
        &correlations,
        allowed_start,
        allowed_end_exclusive,
        primary.lag,
        exclusion_samples,
    );
    let second_absolute = secondary.map_or(0.0, |peak| peak.signed.abs());
    if let Some(second) = secondary {
        let ratio = second.signed.abs() / primary_absolute;
        if second.signed.abs() >= config.minimum_start_correlation
            && ratio >= config.ambiguity_peak_ratio
        {
            return Ok(WirelessSweepRecognition::Ambiguous(
                WirelessSweepAmbiguous {
                    strongest_candidate_start_sample: primary.lag as f64 - probe_offset as f64,
                    second_candidate_start_sample: second.lag as f64 - probe_offset as f64,
                    strongest_absolute_correlation: primary_absolute,
                    second_absolute_correlation: second.signed.abs(),
                    second_to_strongest_ratio: ratio,
                },
            ));
        }
    }

    let initial_start = primary.lag as f64 - probe_offset as f64;
    let drift_search =
        (reference.len() as f64 * config.maximum_clock_drift_ppm / 1_000_000.0).ceil() as usize;
    let search_half_width = config
        .segment_search_margin_samples
        .checked_add(drift_search)
        .ok_or_else(|| DspError::InvalidArgument("segment search range overflow".into()))?;
    let mut segment_matches = Vec::with_capacity(config.segment_count);
    for offset in segment_offsets(
        reference.len(),
        config.segment_samples,
        config.segment_count,
    ) {
        let template = &reference[offset..(offset + config.segment_samples)];
        let predicted_lag = initial_start + offset as f64;
        if let Some(best) = best_local_match(
            microphone_capture,
            template,
            predicted_lag,
            search_half_width,
        )? {
            if best.signed.abs() >= config.minimum_segment_correlation {
                segment_matches.push(WirelessSweepSegmentMatch {
                    reference_offset_samples: offset,
                    capture_offset_samples: best.lag,
                    signed_correlation: best.signed,
                    absolute_correlation: best.signed.abs(),
                });
            }
        }
    }

    if segment_matches.len() < config.minimum_segment_matches {
        return Ok(false_positive(
            WirelessSweepRejectionReason::InconsistentSegments,
            initial_start,
            primary_absolute,
            segment_matches.len(),
            config.minimum_segment_matches,
            None,
        ));
    }
    let Some(fit) = fit_segment_timing(&segment_matches) else {
        return Ok(false_positive(
            WirelessSweepRejectionReason::TimingFitUnstable,
            initial_start,
            primary_absolute,
            segment_matches.len(),
            config.minimum_segment_matches,
            None,
        ));
    };
    let drift_ppm = (fit.slope - 1.0) * 1_000_000.0;
    if fit.slope <= 0.0 || drift_ppm.abs() > config.maximum_clock_drift_ppm {
        return Ok(false_positive(
            WirelessSweepRejectionReason::ClockDriftOutOfRange,
            initial_start,
            primary_absolute,
            segment_matches.len(),
            config.minimum_segment_matches,
            Some(fit),
        ));
    }
    if fit.residual_rms_samples > config.maximum_timing_fit_rms_samples {
        return Ok(false_positive(
            WirelessSweepRejectionReason::TimingFitUnstable,
            initial_start,
            primary_absolute,
            segment_matches.len(),
            config.minimum_segment_matches,
            Some(fit),
        ));
    }

    let estimated_start = fit.intercept;
    let estimated_end_exclusive = fit.intercept + fit.slope * reference.len() as f64;
    if estimated_start < 0.0
        || estimated_end_exclusive <= estimated_start
        || estimated_end_exclusive > microphone_capture.len() as f64
    {
        return Ok(false_positive(
            WirelessSweepRejectionReason::IncompleteCapture,
            initial_start,
            primary_absolute,
            segment_matches.len(),
            config.minimum_segment_matches,
            Some(fit),
        ));
    }

    let core_start = estimated_start.floor() as usize;
    let core_end = estimated_end_exclusive.ceil() as usize;
    let capture_start = core_start.saturating_sub(config.capture_pre_roll_samples);
    let capture_end = core_end
        .saturating_add(config.capture_post_roll_samples)
        .min(microphone_capture.len());
    let capture_segment = WirelessSweepCaptureSegment {
        start_sample: capture_start,
        end_sample_exclusive: capture_end,
        samples: microphone_capture[capture_start..capture_end].to_vec(),
    };

    Ok(WirelessSweepRecognition::Detected(WirelessSweepDetection {
        algorithm_version: WIRELESS_SWEEP_RECOGNITION_VERSION,
        nominal_sample_rate_hz: config.nominal_sample_rate_hz,
        estimated_sweep_start_sample: estimated_start,
        estimated_sweep_end_sample_exclusive: estimated_end_exclusive,
        start_signed_correlation: primary.signed,
        start_absolute_correlation: primary_absolute,
        second_absolute_correlation: second_absolute,
        polarity_inverted: primary.signed < 0.0,
        capture_samples_per_reference_sample: fit.slope,
        estimated_clock_drift_ppm: drift_ppm,
        clock_drift_evidence: WirelessClockDriftEvidence::IntraSweepSegmentFit,
        timing_fit_rms_samples: fit.residual_rms_samples,
        segment_matches,
        capture_segment,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deterministic_reference(length: usize) -> Vec<f64> {
        let mut state = 0x1234_5678_u32;
        (0..length)
            .map(|index| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (f64::from(state >> 8) / f64::from(1_u32 << 24)) * 2.0 - 1.0;
                let t = index as f64 / length as f64;
                let chirp = (2.0 * std::f64::consts::PI * (13.0 * t + 487.0 * t * t)).sin();
                (0.82 * chirp + 0.18 * noise) * (0.7 + 0.3 * (7.0 * t).sin().abs())
            })
            .collect()
    }

    fn deterministic_noise(length: usize, amplitude: f64) -> Vec<f64> {
        let mut state = 0x9e37_79b9_u32;
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let unit = f64::from((state >> 8) & 0x00ff_ffff) / f64::from(0x00ff_ffff);
                (2.0 * unit - 1.0) * amplitude
            })
            .collect()
    }

    fn logarithmic_sweep(length: usize, sample_rate_hz: f64) -> Vec<f64> {
        let start_hz: f64 = 30.0;
        let end_hz: f64 = 18_000.0;
        let duration = length as f64 / sample_rate_hz;
        let logarithmic_span = (end_hz / start_hz).ln();
        (0..length)
            .map(|index| {
                let time = index as f64 / sample_rate_hz;
                let phase = 2.0 * std::f64::consts::PI * start_hz * duration / logarithmic_span
                    * ((logarithmic_span * time / duration).exp() - 1.0);
                let edge = (length / 100).max(1);
                let fade = if index < edge {
                    index as f64 / edge as f64
                } else if index + edge >= length {
                    (length - index - 1) as f64 / edge as f64
                } else {
                    1.0
                };
                phase.sin() * fade.clamp(0.0, 1.0)
            })
            .collect()
    }

    fn test_config() -> WirelessSweepRecognitionConfig {
        WirelessSweepRecognitionConfig {
            probe_samples: 512,
            segment_samples: 256,
            segment_count: 7,
            minimum_start_correlation: 0.65,
            minimum_segment_correlation: 0.60,
            ambiguity_peak_ratio: 0.90,
            maximum_clock_drift_ppm: 3_000.0,
            segment_search_margin_samples: 24,
            maximum_timing_fit_rms_samples: 2.0,
            minimum_segment_matches: 5,
            capture_pre_roll_samples: 50,
            capture_post_roll_samples: 80,
            maximum_reference_samples: 100_000,
            maximum_capture_samples: 200_000,
            ..WirelessSweepRecognitionConfig::default()
        }
    }

    fn add_at(capture: &mut [f64], start: usize, signal: &[f64], gain: f64) {
        for (destination, &sample) in capture[start..(start + signal.len())]
            .iter_mut()
            .zip(signal)
        {
            *destination += gain * sample;
        }
    }

    fn time_stretch(reference: &[f64], ratio: f64) -> Vec<f64> {
        let length = (((reference.len() - 1) as f64 * ratio).round() as usize) + 1;
        (0..length)
            .map(|output_index| {
                let source = output_index as f64 / ratio;
                let lower = source.floor() as usize;
                let fraction = source - lower as f64;
                if lower + 1 < reference.len() {
                    reference[lower] * (1.0 - fraction) + reference[lower + 1] * fraction
                } else {
                    reference[reference.len() - 1]
                }
            })
            .collect()
    }

    #[test]
    fn detects_delayed_noisy_playback_and_returns_original_capture_segment() {
        let reference = deterministic_reference(8_192);
        let start = 1_200;
        let mut capture = deterministic_noise(12_000, 0.025);
        add_at(&mut capture, start, &reference, 0.8);

        let result = recognize_wireless_sweep(&reference, &capture, &test_config()).unwrap();
        let WirelessSweepRecognition::Detected(detection) = result else {
            panic!("expected a detected sweep, got {result:?}");
        };
        assert!((detection.estimated_sweep_start_sample - start as f64).abs() < 0.2);
        assert!(detection.estimated_clock_drift_ppm.abs() < 20.0);
        assert!(detection.start_absolute_correlation > 0.99);
        assert!(!detection.polarity_inverted);
        assert_eq!(detection.capture_segment.start_sample, start - 50);
        assert_eq!(
            detection.capture_segment.end_sample_exclusive,
            start + reference.len() + 80
        );
        assert_eq!(
            detection.capture_segment.samples,
            capture[detection.capture_segment.start_sample
                ..detection.capture_segment.end_sample_exclusive]
        );
    }

    #[test]
    fn global_search_remains_correct_across_fft_capture_blocks() {
        let reference = deterministic_reference(8_192);
        let start = CORRELATION_BLOCK_LAGS + 4_321;
        let mut capture = deterministic_noise(start + reference.len() + 900, 0.015);
        add_at(&mut capture, start, &reference, 0.8);

        let result = recognize_wireless_sweep(&reference, &capture, &test_config()).unwrap();
        let WirelessSweepRecognition::Detected(detection) = result else {
            panic!("expected a cross-block detection, got {result:?}");
        };
        assert!((detection.estimated_sweep_start_sample - start as f64).abs() < 0.2);
        assert!(detection.start_absolute_correlation > 0.99);
    }

    #[test]
    fn recognizes_a_log_sweep_after_deterministic_room_reflections() {
        let reference = logarithmic_sweep(48_000, 48_000.0);
        let mut room_output = vec![0.0; reference.len() + 73];
        add_at(&mut room_output, 0, &reference, 0.78);
        add_at(&mut room_output, 19, &reference, 0.16);
        add_at(&mut room_output, 73, &reference, -0.08);
        let start = 1_500;
        let mut capture = deterministic_noise(start + room_output.len() + 1_000, 0.005);
        add_at(&mut capture, start, &room_output, 1.0);
        let mut config = test_config();
        config.minimum_start_correlation = 0.35;
        config.minimum_segment_correlation = 0.30;
        config.maximum_timing_fit_rms_samples = 24.0;

        let result = recognize_wireless_sweep(&reference, &capture, &config).unwrap();
        let WirelessSweepRecognition::Detected(detection) = result else {
            panic!("expected reflected log sweep detection, got {result:?}");
        };
        assert!((detection.estimated_sweep_start_sample - start as f64).abs() < 12.0);
        assert!(detection.start_absolute_correlation > 0.9);
        assert!(detection.timing_fit_rms_samples < 12.0);
    }

    #[test]
    fn estimates_positive_wireless_clock_drift_from_segment_positions() {
        let reference = deterministic_reference(32_768);
        let expected_ratio = 1.000_75;
        let stretched = time_stretch(&reference, expected_ratio);
        let start = 2_000;
        let mut capture = deterministic_noise(start + stretched.len() + 1_000, 0.01);
        add_at(&mut capture, start, &stretched, 0.9);
        let mut config = test_config();
        config.maximum_reference_samples = 100_000;
        config.maximum_capture_samples = 200_000;
        config.maximum_timing_fit_rms_samples = 2.0;

        let result = recognize_wireless_sweep(&reference, &capture, &config).unwrap();
        let WirelessSweepRecognition::Detected(detection) = result else {
            panic!("expected drifted sweep detection, got {result:?}");
        };
        assert!(
            (detection.capture_samples_per_reference_sample - expected_ratio).abs() < 0.000_04,
            "estimated ratio {}",
            detection.capture_samples_per_reference_sample
        );
        assert!(
            (detection.estimated_clock_drift_ppm - 750.0).abs() < 40.0,
            "estimated drift {} ppm",
            detection.estimated_clock_drift_ppm
        );
        assert!((detection.estimated_sweep_start_sample - start as f64).abs() < 1.0);
        assert_eq!(detection.segment_matches.len(), config.segment_count);
    }

    #[test]
    fn estimates_negative_wireless_clock_drift() {
        let reference = deterministic_reference(32_768);
        let expected_ratio = 0.999_4;
        let stretched = time_stretch(&reference, expected_ratio);
        let start = 1_600;
        let mut capture = deterministic_noise(start + stretched.len() + 800, 0.01);
        add_at(&mut capture, start, &stretched, 0.85);

        let result = recognize_wireless_sweep(&reference, &capture, &test_config()).unwrap();
        let WirelessSweepRecognition::Detected(detection) = result else {
            panic!("expected negative-drift sweep detection, got {result:?}");
        };
        assert!(
            (detection.estimated_clock_drift_ppm + 600.0).abs() < 45.0,
            "estimated drift {} ppm",
            detection.estimated_clock_drift_ppm
        );
        assert!((detection.estimated_sweep_start_sample - start as f64).abs() < 1.0);
    }

    #[test]
    fn detects_inverted_polarity_without_losing_the_sweep() {
        let reference = deterministic_reference(8_192);
        let start = 900;
        let mut capture = deterministic_noise(11_000, 0.01);
        add_at(&mut capture, start, &reference, -0.75);

        let result = recognize_wireless_sweep(&reference, &capture, &test_config()).unwrap();
        let WirelessSweepRecognition::Detected(detection) = result else {
            panic!("expected inverted sweep detection, got {result:?}");
        };
        assert!(detection.polarity_inverted);
        assert!(detection.start_signed_correlation < 0.0);
        assert!((detection.estimated_sweep_start_sample - start as f64).abs() < 0.2);
    }

    #[test]
    fn noise_only_capture_is_not_detected() {
        let reference = deterministic_reference(8_192);
        let capture = deterministic_noise(12_000, 0.1);
        let result = recognize_wireless_sweep(&reference, &capture, &test_config()).unwrap();
        let WirelessSweepRecognition::NotDetected(rejection) = result else {
            panic!("expected no detection, got {result:?}");
        };
        assert_eq!(
            rejection.reason,
            WirelessSweepRejectionReason::CorrelationBelowThreshold
        );
        assert!(rejection.strongest_absolute_correlation < rejection.required_absolute_correlation);
    }

    #[test]
    fn two_complete_playbacks_are_reported_as_ambiguous() {
        let reference = deterministic_reference(8_192);
        let first = 800;
        let second = 10_000;
        let mut capture = deterministic_noise(20_000, 0.005);
        add_at(&mut capture, first, &reference, 0.8);
        add_at(&mut capture, second, &reference, 0.8);

        let result = recognize_wireless_sweep(&reference, &capture, &test_config()).unwrap();
        let WirelessSweepRecognition::Ambiguous(ambiguity) = result else {
            panic!("expected ambiguity, got {result:?}");
        };
        let candidates = [
            ambiguity.strongest_candidate_start_sample.round() as usize,
            ambiguity.second_candidate_start_sample.round() as usize,
        ];
        assert!(candidates.contains(&first));
        assert!(candidates.contains(&second));
        assert!(ambiguity.second_to_strongest_ratio > 0.98);
    }

    #[test]
    fn isolated_probe_hit_is_rejected_as_likely_false_positive() {
        let reference = deterministic_reference(8_192);
        let config = test_config();
        let probe_offset = (reference.len() - config.probe_samples) / 2;
        let candidate_start = 1_000;
        let mut capture = deterministic_noise(12_000, 0.02);
        add_at(
            &mut capture,
            candidate_start + probe_offset,
            &reference[probe_offset..(probe_offset + config.probe_samples)],
            0.9,
        );

        let result = recognize_wireless_sweep(&reference, &capture, &config).unwrap();
        let WirelessSweepRecognition::LikelyFalsePositive(rejection) = result else {
            panic!("expected a likely false positive, got {result:?}");
        };
        assert_eq!(
            rejection.reason,
            WirelessSweepRejectionReason::InconsistentSegments
        );
        assert!(rejection.matched_segment_count < rejection.required_segment_count);
        assert!(rejection.start_absolute_correlation > 0.95);
    }

    #[test]
    fn excessive_drift_is_rejected_after_it_is_estimated() {
        let reference = deterministic_reference(32_768);
        let stretched = time_stretch(&reference, 1.004);
        let start = 1_000;
        let mut capture = deterministic_noise(start + stretched.len() + 1_000, 0.005);
        add_at(&mut capture, start, &stretched, 0.9);
        let mut config = test_config();
        // Search broadly enough to estimate it, but accept only 3000 ppm.
        config.maximum_clock_drift_ppm = 3_000.0;
        config.segment_search_margin_samples = 80;

        let result = recognize_wireless_sweep(&reference, &capture, &config).unwrap();
        let WirelessSweepRecognition::LikelyFalsePositive(rejection) = result else {
            panic!("expected excessive drift rejection, got {result:?}");
        };
        assert_eq!(
            rejection.reason,
            WirelessSweepRejectionReason::ClockDriftOutOfRange
        );
        assert!(rejection.estimated_clock_drift_ppm.unwrap().abs() > 3_000.0);
    }

    #[test]
    fn malformed_and_unsafe_inputs_are_errors_not_recognition_outcomes() {
        let reference = deterministic_reference(8_192);
        let capture = deterministic_noise(12_000, 0.01);
        let config = test_config();

        assert_eq!(
            recognize_wireless_sweep(&[], &capture, &config).unwrap_err(),
            DspError::EmptyInput("wireless sweep reference")
        );
        assert_eq!(
            recognize_wireless_sweep(&reference, &[], &config).unwrap_err(),
            DspError::EmptyInput("wireless microphone capture")
        );

        let mut non_finite_reference = reference.clone();
        non_finite_reference[17] = f64::NAN;
        assert_eq!(
            recognize_wireless_sweep(&non_finite_reference, &capture, &config).unwrap_err(),
            DspError::NonFinite {
                context: "wireless sweep reference",
                index: 17
            }
        );

        let mut non_finite_capture = capture.clone();
        non_finite_capture[23] = f64::INFINITY;
        assert_eq!(
            recognize_wireless_sweep(&reference, &non_finite_capture, &config).unwrap_err(),
            DspError::NonFinite {
                context: "wireless microphone capture",
                index: 23
            }
        );

        let mut excessive_capture = capture.clone();
        excessive_capture[31] = config.maximum_absolute_sample + 0.1;
        assert!(
            recognize_wireless_sweep(&reference, &excessive_capture, &config)
                .unwrap_err()
                .to_string()
                .contains("safety limit")
        );

        let silent_reference = vec![0.0; reference.len()];
        assert!(
            recognize_wireless_sweep(&silent_reference, &capture, &config)
                .unwrap_err()
                .to_string()
                .contains("insufficient variation")
        );
    }

    #[test]
    fn invalid_thresholds_lengths_and_size_limits_are_rejected() {
        let reference = deterministic_reference(8_192);
        let capture = deterministic_noise(12_000, 0.01);

        let mut config = test_config();
        config.minimum_start_correlation = f64::NAN;
        assert!(matches!(
            recognize_wireless_sweep(&reference, &capture, &config),
            Err(DspError::InvalidArgument(_))
        ));

        let mut config = test_config();
        config.minimum_segment_matches = 2;
        assert!(matches!(
            recognize_wireless_sweep(&reference, &capture, &config),
            Err(DspError::InvalidArgument(_))
        ));

        let mut config = test_config();
        config.probe_samples = reference.len() + 1;
        assert!(matches!(
            recognize_wireless_sweep(&reference, &capture, &config),
            Err(DspError::ShapeMismatch(_))
        ));

        let mut config = test_config();
        config.maximum_capture_samples = capture.len() - 1;
        assert!(recognize_wireless_sweep(&reference, &capture, &config)
            .unwrap_err()
            .to_string()
            .contains("configured safety limit"));
    }

    #[test]
    fn incomplete_capture_does_not_produce_a_detected_segment() {
        let reference = deterministic_reference(8_192);
        let start = 300;
        let retained = 6_000;
        let mut capture = deterministic_noise(start + retained, 0.005);
        add_at(&mut capture, start, &reference[..retained], 0.8);
        let result = recognize_wireless_sweep(&reference, &capture, &test_config()).unwrap();
        assert!(!matches!(result, WirelessSweepRecognition::Detected(_)));
    }
}
