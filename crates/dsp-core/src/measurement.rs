//! Known-sweep deconvolution for a recognized live microphone capture.
//!
//! This module reuses [`crate::wireless_sweep`] for asynchronous playback
//! alignment, [`crate::analysis::frequency_response`] for the response grid,
//! and [`crate::validation::fft_convolve`] for a reconstruction-fit check.
//! The regularized frequency-domain division, microphone-calibration
//! application, and live-capture quality gates are new deterministic v1 code.
//!
//! The returned IR starts at the cross-correlation-derived sweep boundary; it
//! is never shifted to its largest impulse. Because a separately played
//! wireless sweep has no hardware-synchronous reference, its absolute timing
//! remains ineligible for inter-channel delay decisions.

use crate::analysis::{frequency_response, FrequencyResponse};
use crate::calibration::MicrophoneCalibration;
use crate::validation::fft_convolve;
use crate::wireless_sweep::WirelessSweepDetection;
use crate::{DspError, DspResult};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

pub const KNOWN_SWEEP_DECONVOLUTION_VERSION: &str = "known-sweep-deconvolution-v5";

const ABSOLUTE_MAXIMUM_REFERENCE_SAMPLES: usize = 2_000_000;
const ABSOLUTE_MAXIMUM_IR_SAMPLES: usize = 262_144;
const LEVEL_FLOOR: f64 = 1.0e-15;

#[derive(Debug, Clone, PartialEq)]
pub struct SweepDeconvolutionConfig {
    pub sample_rate_hz: u32,
    pub impulse_length_samples: usize,
    /// Fixed samples retained before the marker-referenced sweep boundary.
    /// This preserves negative acoustic arrival offsets without peak-aligning
    /// or otherwise moving the original measurement timeline.
    pub impulse_pre_zero_samples: usize,
    pub analysis_fft_size: usize,
    /// Relative Tikhonov power regularization in dB below the maximum
    /// reference-spectrum power.
    pub regularization_db: f64,
    /// Frequency range over which magnitude calibration is applied.
    pub calibration_band_hz: [f64; 2],
    pub require_calibration: bool,
    pub clipping_threshold_linear: f64,
    pub minimum_capture_snr_db: f64,
    pub minimum_reconstruction_fit_db: f64,
    /// When false the fit remains an auditable diagnostic but does not reject
    /// a capture whose independent timing and signal-quality gates passed.
    pub enforce_minimum_reconstruction_fit: bool,
    pub minimum_start_correlation: f64,
    pub maximum_clock_drift_ppm: f64,
    pub maximum_timing_fit_rms_samples: f64,
    pub minimum_pre_sweep_noise_samples: usize,
    pub pre_sweep_noise_guard_samples: usize,
    /// Longest stretch of pre-sweep audio used for the noise-floor estimate,
    /// counted backwards from the guard. Bounding it keeps the estimate
    /// independent of how much pre-roll the caller happens to retain: the
    /// retained region reaches back toward the start marker, whose decaying
    /// reverberation is not the ambient noise floor, and letting the window
    /// grow with the pre-roll would inflate the noise estimate and reject good
    /// captures. The samples nearest the sweep are the least contaminated, so
    /// the window is anchored at the guard rather than at the segment start.
    pub pre_sweep_noise_window_samples: usize,
    pub maximum_reference_samples: usize,
    pub maximum_absolute_sample: f64,
}

impl Default for SweepDeconvolutionConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 48_000,
            // 2026-07-29, expert review finding 4: the retained IR doubled from
            // 16,384 samples (0.341 s, ~2.9 Hz true resolution) to 32,768
            // (0.683 s, ~1.5 Hz), and the analysis FFT from 32,768 (1.465 Hz
            // grid) to 65,536 (0.732 Hz grid), so a high-Q low-frequency mode
            // no longer loses several dB of peak between bins and the 3 Hz
            // design-kernel floor is sampled by >=4 grid points instead of ~2.
            // 32,768 is the largest window that stays inside the bundled
            // sweeps' clean post-sweep gap (41,280 samples before the end
            // marker begins; 58,080 to its end), so the tail never overlaps
            // end-marker playback and the bundled files still pass the import
            // tail gate.
            //
            // This satisfies the review's "0.68 s -> 1.3 s" request literally:
            // those figures are `analysis_fft_size` expressed in seconds
            // (32_768/48_000 = 0.683, 65_536/48_000 = 1.365), matching its
            // "65,536 FFT (0.73 Hz)" in the same sentence. Growing the retained
            // window further would not help - the measured 20-50 Hz decay in a
            // real room reaches the noise floor by ~200 ms, so the extra span
            // is noise, and per-bin SNR falls 3 dB per doubling past the decay.
            // A zoom FFT is unnecessary for the same reason: the uniform
            // 0.732 Hz grid already meets the requested <100 Hz density.
            impulse_length_samples: 32_768,
            impulse_pre_zero_samples: 0,
            analysis_fft_size: 65_536,
            regularization_db: -100.0,
            calibration_band_hz: [20.0, 20_000.0],
            require_calibration: true,
            clipping_threshold_linear: 0.999,
            minimum_capture_snr_db: 30.0,
            minimum_reconstruction_fit_db: 12.0,
            enforce_minimum_reconstruction_fit: true,
            minimum_start_correlation: 0.45,
            maximum_clock_drift_ppm: 2_000.0,
            maximum_timing_fit_rms_samples: 24.0,
            minimum_pre_sweep_noise_samples: 1_024,
            pre_sweep_noise_guard_samples: 256,
            // 100 ms, which is the pre-roll every path retained before the
            // signed pre-zero window grew, so the estimate keeps reading the
            // same stretch of audio it always did.
            pre_sweep_noise_window_samples: 4_800,
            maximum_reference_samples: 1_000_000,
            maximum_absolute_sample: 4.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SweepMeasurementQualityIssue {
    CalibrationMissing,
    CalibrationCoverageInsufficient {
        required_hz: [f64; 2],
        available_hz: [f64; 2],
    },
    InputClipping {
        clipped_sample_count: usize,
    },
    InsufficientPreSweepNoise {
        available_samples: usize,
        required_samples: usize,
    },
    CaptureSnrTooLow {
        measured_db: f64,
        required_db: f64,
    },
    ReconstructionFitTooLow {
        measured_db: f64,
        required_db: f64,
    },
    StartCorrelationTooLow {
        measured: f64,
        required: f64,
    },
    ClockDriftOutOfRange {
        measured_ppm: f64,
        allowed_ppm: f64,
    },
    TimingFitUnstable {
        measured_rms_samples: f64,
        allowed_rms_samples: f64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SweepMeasurementQualityMetrics {
    pub accepted: bool,
    pub issues: Vec<SweepMeasurementQualityIssue>,
    pub capture_peak_dbfs: f64,
    pub clipped_sample_count: usize,
    pub pre_sweep_noise_rms_dbfs: Option<f64>,
    pub sweep_capture_rms_dbfs: f64,
    pub capture_snr_db: Option<f64>,
    pub reconstruction_fit_db: f64,
    pub reconstruction_fit_required: bool,
    pub start_absolute_correlation: f64,
    pub estimated_clock_drift_ppm: f64,
    pub timing_fit_rms_samples: f64,
    pub calibration_applied: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SweepMeasurementTiming {
    /// Original microphone-capture coordinate reported by recognition.
    pub recognized_sweep_start_capture_sample: f64,
    pub capture_segment_start_sample: usize,
    /// IR sample zero is the recognized sweep boundary, not the largest peak.
    pub impulse_start_relative_to_recognized_sweep_seconds: f64,
    /// Always false for this single asynchronously played sweep workflow.
    pub absolute_timing_eligible: bool,
    pub capture_samples_per_reference_sample: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SweepMeasurement {
    pub algorithm_version: &'static str,
    pub sample_rate_hz: u32,
    /// Retained before microphone magnitude calibration for audit/reprocessing.
    pub uncalibrated_impulse_samples: Vec<f64>,
    pub calibrated_impulse_samples: Vec<f64>,
    pub calibrated_frequency_response: FrequencyResponse,
    pub impulse_peak_sample_index: usize,
    pub timing: SweepMeasurementTiming,
    pub quality: SweepMeasurementQualityMetrics,
}

fn validate_config(config: &SweepDeconvolutionConfig) -> DspResult<()> {
    if !(8_000..=384_000).contains(&config.sample_rate_hz) {
        return Err(DspError::InvalidArgument(
            "measurement sample rate must be between 8000 and 384000 Hz".into(),
        ));
    }
    if !(2..=ABSOLUTE_MAXIMUM_IR_SAMPLES).contains(&config.impulse_length_samples) {
        return Err(DspError::InvalidArgument(format!(
            "measurement impulse length must be between 2 and {ABSOLUTE_MAXIMUM_IR_SAMPLES} samples"
        )));
    }
    if config.impulse_pre_zero_samples >= config.impulse_length_samples {
        return Err(DspError::InvalidArgument(
            "measurement pre-zero samples must be shorter than the retained impulse".into(),
        ));
    }
    if !config.analysis_fft_size.is_power_of_two()
        || config.analysis_fft_size < config.impulse_length_samples
    {
        return Err(DspError::InvalidArgument(
            "measurement analysis FFT must be a power of two at least as long as the impulse"
                .into(),
        ));
    }
    if !config.regularization_db.is_finite()
        || !(-300.0..=-20.0).contains(&config.regularization_db)
    {
        return Err(DspError::InvalidArgument(
            "measurement regularization must be finite and between -300 and -20 dB".into(),
        ));
    }
    let nyquist_hz = f64::from(config.sample_rate_hz) / 2.0;
    if config
        .calibration_band_hz
        .iter()
        .any(|value| !value.is_finite())
        || config.calibration_band_hz[0] <= 0.0
        || config.calibration_band_hz[1] <= config.calibration_band_hz[0]
        || config.calibration_band_hz[1] > nyquist_hz
    {
        return Err(DspError::InvalidArgument(
            "measurement calibration band must be finite, positive, increasing, and below Nyquist"
                .into(),
        ));
    }
    if !config.clipping_threshold_linear.is_finite()
        || config.clipping_threshold_linear <= 0.0
        || config.clipping_threshold_linear > config.maximum_absolute_sample
    {
        return Err(DspError::InvalidArgument(
            "measurement clipping threshold must be finite, positive, and no greater than the sample safety limit".into(),
        ));
    }
    for (name, value, maximum) in [
        ("minimum capture SNR", config.minimum_capture_snr_db, 180.0),
        (
            "minimum reconstruction fit",
            config.minimum_reconstruction_fit_db,
            180.0,
        ),
        (
            "maximum clock drift",
            config.maximum_clock_drift_ppm,
            50_000.0,
        ),
        (
            "maximum timing-fit RMS",
            config.maximum_timing_fit_rms_samples,
            f64::from(config.sample_rate_hz),
        ),
    ] {
        if !value.is_finite() || value < 0.0 || value > maximum {
            return Err(DspError::InvalidArgument(format!(
                "measurement {name} must be finite and between zero and {maximum}"
            )));
        }
    }
    if !config.minimum_start_correlation.is_finite()
        || config.minimum_start_correlation <= 0.0
        || config.minimum_start_correlation > 1.0
    {
        return Err(DspError::InvalidArgument(
            "measurement minimum start correlation must be finite and in (0, 1]".into(),
        ));
    }
    if config.minimum_pre_sweep_noise_samples == 0
        || config.minimum_pre_sweep_noise_samples > config.sample_rate_hz as usize * 10
        || config.pre_sweep_noise_guard_samples > config.sample_rate_hz as usize * 2
    {
        return Err(DspError::InvalidArgument(
            "measurement pre-sweep noise requirements exceed the safe duration limits".into(),
        ));
    }
    if config.maximum_reference_samples == 0
        || config.maximum_reference_samples > ABSOLUTE_MAXIMUM_REFERENCE_SAMPLES
    {
        return Err(DspError::InvalidArgument(format!(
            "measurement maximum reference length must be between 1 and {ABSOLUTE_MAXIMUM_REFERENCE_SAMPLES}"
        )));
    }
    if !config.maximum_absolute_sample.is_finite()
        || config.maximum_absolute_sample <= 0.0
        || config.maximum_absolute_sample > 1_000.0
    {
        return Err(DspError::InvalidArgument(
            "measurement sample safety limit must be finite and in (0, 1000]".into(),
        ));
    }
    Ok(())
}

fn validate_finite_bounded(
    samples: &[f64],
    context: &'static str,
    maximum_absolute_sample: f64,
) -> DspResult<()> {
    if samples.is_empty() {
        return Err(DspError::EmptyInput(context));
    }
    if let Some(index) = samples.iter().position(|sample| !sample.is_finite()) {
        return Err(DspError::NonFinite { context, index });
    }
    if let Some((index, sample)) = samples
        .iter()
        .enumerate()
        .find(|(_, sample)| sample.abs() > maximum_absolute_sample)
    {
        return Err(DspError::InvalidArgument(format!(
            "{context} sample {index} has magnitude {}, exceeding the configured safety limit {maximum_absolute_sample}",
            sample.abs()
        )));
    }
    Ok(())
}

fn rms(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    Some((samples.iter().map(|sample| sample * sample).sum::<f64>() / samples.len() as f64).sqrt())
}

fn level_db(linear: f64) -> f64 {
    20.0 * linear.max(LEVEL_FLOOR).log10()
}

/// Number of taps on each side of the fractional position in the windowed-sinc
/// interpolator, and the Kaiser shape parameter. Sixteen taps with beta = 8
/// keep the pass band flat within ~0.01 dB to beyond 20 kHz; the previous
/// 2-point linear interpolator imposed a fractional-delay comb of up to
/// -11.7 dB at 20 kHz on every stored response - inaudible to the 20-650 Hz
/// design but visibly wrong next to an REW measurement (2026-07-29 expert
/// review, item C). Near the buffer edges the kernel gracefully truncates and
/// renormalizes, which matches the previous behavior of clamping to the
/// available neighbors.
const SINC_INTERPOLATION_HALF_TAPS: usize = 8;
const SINC_INTERPOLATION_KAISER_BETA: f64 = 8.0;

fn bessel_i0(x: f64) -> f64 {
    // Series expansion; converges quickly for the beta values used here.
    let mut sum = 1.0;
    let mut term = 1.0;
    let half_x_squared = (x / 2.0) * (x / 2.0);
    for k in 1..=25 {
        term *= half_x_squared / ((k * k) as f64);
        sum += term;
        if term < 1.0e-16 * sum {
            break;
        }
    }
    sum
}

/// Phase resolution of the precomputed polyphase kernel table. Quantizing the
/// fractional position to 1/256 sample moves the realized response by well
/// under 0.01 dB at 20 kHz while removing two Bessel-series evaluations per
/// tap per output sample.
const SINC_INTERPOLATION_PHASES: usize = 256;

fn sinc_kernel_table() -> &'static Vec<[f64; SINC_INTERPOLATION_HALF_TAPS * 2]> {
    static TABLE: std::sync::OnceLock<Vec<[f64; SINC_INTERPOLATION_HALF_TAPS * 2]>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let denominator = bessel_i0(SINC_INTERPOLATION_KAISER_BETA);
        let half_span = SINC_INTERPOLATION_HALF_TAPS as f64;
        (0..=SINC_INTERPOLATION_PHASES)
            .map(|phase| {
                let fraction = phase as f64 / SINC_INTERPOLATION_PHASES as f64;
                let mut kernel = [0.0; SINC_INTERPOLATION_HALF_TAPS * 2];
                for (tap, weight) in kernel.iter_mut().enumerate() {
                    // Tap index 0 corresponds to `lower - (half - 1)`.
                    let offset = tap as f64 - (SINC_INTERPOLATION_HALF_TAPS as f64 - 1.0);
                    let distance = fraction - offset;
                    let normalized = distance / half_span;
                    if normalized.abs() >= 1.0 {
                        continue;
                    }
                    let sinc = if distance.abs() < 1.0e-12 {
                        1.0
                    } else {
                        let argument = std::f64::consts::PI * distance;
                        argument.sin() / argument
                    };
                    let window = bessel_i0(
                        SINC_INTERPOLATION_KAISER_BETA * (1.0 - normalized * normalized).sqrt(),
                    ) / denominator;
                    *weight = sinc * window;
                }
                kernel
            })
            .collect()
    })
}

fn interpolate_sample(samples: &[f64], position: f64) -> DspResult<f64> {
    if !position.is_finite() || position < 0.0 {
        return Err(DspError::InvalidArgument(
            "measurement resampling position must be finite and nonnegative".into(),
        ));
    }
    let lower = position.floor() as usize;
    if lower >= samples.len() {
        return Err(DspError::InvalidArgument(
            "recognized sweep does not retain enough post-roll for the requested impulse length"
                .into(),
        ));
    }
    let fraction = position - lower as f64;
    if fraction <= f64::EPSILON {
        return Ok(samples[lower]);
    }
    let phase = ((fraction * SINC_INTERPOLATION_PHASES as f64).round() as usize)
        .min(SINC_INTERPOLATION_PHASES);
    let kernel = &sinc_kernel_table()[phase];
    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;
    for (tap, weight) in kernel.iter().enumerate() {
        if *weight == 0.0 {
            continue;
        }
        let offset = tap as isize - (SINC_INTERPOLATION_HALF_TAPS as isize - 1);
        let Some(index) = lower.checked_add_signed(offset) else {
            continue;
        };
        if index >= samples.len() {
            continue;
        }
        weighted_sum += weight * samples[index];
        weight_sum += weight;
    }
    if !weight_sum.is_finite() || weight_sum.abs() < 1.0e-12 {
        return Ok(samples[lower]);
    }
    // Renormalizing keeps unity DC gain even where the kernel is truncated by
    // the buffer edge.
    Ok(weighted_sum / weight_sum)
}

fn forward_real(samples: &[f64], fft_size: usize) -> Vec<Complex<f64>> {
    let mut spectrum = vec![Complex::new(0.0, 0.0); fft_size];
    for (bin, sample) in spectrum.iter_mut().zip(samples) {
        bin.re = *sample;
    }
    FftPlanner::<f64>::new()
        .plan_fft_forward(fft_size)
        .process(&mut spectrum);
    spectrum
}

fn inverse_real(mut spectrum: Vec<Complex<f64>>) -> DspResult<Vec<f64>> {
    let fft_size = spectrum.len();
    FftPlanner::<f64>::new()
        .plan_fft_inverse(fft_size)
        .process(&mut spectrum);
    let scale = 1.0 / fft_size as f64;
    let samples: Vec<f64> = spectrum
        .into_iter()
        .map(|sample| sample.re * scale)
        .collect();
    if let Some(index) = samples.iter().position(|sample| !sample.is_finite()) {
        return Err(DspError::NonFinite {
            context: "deconvolved impulse response",
            index,
        });
    }
    Ok(samples)
}

fn reconstruction_fit_db(
    reference: &[f64],
    impulse: &[f64],
    aligned_capture: &[f64],
) -> DspResult<f64> {
    let reconstructed = fft_convolve(reference, impulse)?;
    let compared = reconstructed.len().min(aligned_capture.len());
    let signal_rms = rms(&aligned_capture[..compared]).unwrap_or(0.0);
    let residual_sum = reconstructed[..compared]
        .iter()
        .zip(&aligned_capture[..compared])
        .map(|(model, measured)| {
            let residual = model - measured;
            residual * residual
        })
        .sum::<f64>();
    let residual_rms = (residual_sum / compared as f64).sqrt();
    Ok(level_db(signal_rms) - level_db(residual_rms))
}

/// Deconvolve a previously recognized known sweep into an auditable IR.
///
/// `detection.capture_segment` must retain enough samples after the recognized
/// sweep for `config.impulse_length_samples`. The capture is linearly resampled
/// to the reference grid using the existing detector's intra-sweep slope before
/// regularized spectral division. No peak alignment or amplitude normalization
/// is performed.
pub fn deconvolve_recognized_sweep(
    reference: &[f64],
    detection: &WirelessSweepDetection,
    calibration: Option<&MicrophoneCalibration>,
    config: &SweepDeconvolutionConfig,
) -> DspResult<SweepMeasurement> {
    validate_config(config)?;
    if reference.len() > config.maximum_reference_samples {
        return Err(DspError::InvalidArgument(format!(
            "measurement reference has {} samples, exceeding the configured limit {}",
            reference.len(),
            config.maximum_reference_samples
        )));
    }
    validate_finite_bounded(
        reference,
        "measurement sweep reference",
        config.maximum_absolute_sample,
    )?;
    validate_finite_bounded(
        &detection.capture_segment.samples,
        "measurement microphone capture",
        config.maximum_absolute_sample,
    )?;
    if detection.nominal_sample_rate_hz != config.sample_rate_hz {
        return Err(DspError::ShapeMismatch(format!(
            "recognized sweep sample rate {} Hz does not match measurement rate {} Hz",
            detection.nominal_sample_rate_hz, config.sample_rate_hz
        )));
    }
    if detection.capture_segment.end_sample_exclusive
        != detection.capture_segment.start_sample + detection.capture_segment.samples.len()
    {
        return Err(DspError::ShapeMismatch(
            "recognized sweep capture-segment bounds do not match its sample count".into(),
        ));
    }
    for (name, value) in [
        (
            "recognized sweep start",
            detection.estimated_sweep_start_sample,
        ),
        (
            "recognized sweep end",
            detection.estimated_sweep_end_sample_exclusive,
        ),
        (
            "capture/reference sample ratio",
            detection.capture_samples_per_reference_sample,
        ),
        ("clock drift", detection.estimated_clock_drift_ppm),
        ("timing-fit RMS", detection.timing_fit_rms_samples),
        ("start correlation", detection.start_absolute_correlation),
    ] {
        if !value.is_finite() {
            return Err(DspError::InvalidArgument(format!(
                "{name} must be finite for measurement deconvolution"
            )));
        }
    }
    if detection.capture_samples_per_reference_sample <= 0.0 {
        return Err(DspError::InvalidArgument(
            "recognized sweep capture/reference sample ratio must be positive".into(),
        ));
    }

    let local_start =
        detection.estimated_sweep_start_sample - detection.capture_segment.start_sample as f64;
    if local_start < 0.0
        || local_start >= detection.capture_segment.samples.len() as f64
        || detection.estimated_sweep_end_sample_exclusive <= detection.estimated_sweep_start_sample
    {
        return Err(DspError::ShapeMismatch(
            "recognized sweep bounds lie outside the retained capture segment".into(),
        ));
    }
    let aligned_length = reference
        .len()
        .checked_add(config.impulse_length_samples)
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| DspError::InvalidArgument("measurement length overflow".into()))?;
    let pre_zero_capture_samples =
        config.impulse_pre_zero_samples as f64 * detection.capture_samples_per_reference_sample;
    if local_start < pre_zero_capture_samples {
        return Err(DspError::InvalidArgument(
            "recognized sweep does not retain enough pre-roll for the requested signed impulse timeline"
                .into(),
        ));
    }
    let aligned_start = local_start - pre_zero_capture_samples;
    let last_capture_position = aligned_start
        + (aligned_length - 1) as f64 * detection.capture_samples_per_reference_sample;
    if last_capture_position > (detection.capture_segment.samples.len() - 1) as f64 {
        return Err(DspError::InvalidArgument(
            "recognized sweep does not retain enough post-roll for the requested impulse length"
                .into(),
        ));
    }
    let aligned_capture: Vec<f64> = (0..aligned_length)
        .map(|index| {
            interpolate_sample(
                &detection.capture_segment.samples,
                aligned_start + index as f64 * detection.capture_samples_per_reference_sample,
            )
        })
        .collect::<DspResult<_>>()?;

    let fft_size = aligned_length.checked_next_power_of_two().ok_or_else(|| {
        DspError::InvalidArgument("measurement deconvolution FFT size overflow".into())
    })?;
    let reference_spectrum = forward_real(reference, fft_size);
    let capture_spectrum = forward_real(&aligned_capture, fft_size);
    let maximum_reference_power = reference_spectrum
        .iter()
        .map(Complex::norm_sqr)
        .fold(0.0_f64, f64::max);
    if !maximum_reference_power.is_finite() || maximum_reference_power <= LEVEL_FLOOR {
        return Err(DspError::InvalidArgument(
            "measurement sweep reference has insufficient spectral energy".into(),
        ));
    }
    let regularization_power =
        maximum_reference_power * 10.0_f64.powf(config.regularization_db / 10.0);
    let raw_transfer_spectrum: Vec<Complex<f64>> = capture_spectrum
        .iter()
        .zip(&reference_spectrum)
        .map(|(captured, reference_bin)| {
            *captured * reference_bin.conj() / (reference_bin.norm_sqr() + regularization_power)
        })
        .collect();
    let raw_full_impulse = inverse_real(raw_transfer_spectrum.clone())?;
    let uncalibrated_impulse_samples = raw_full_impulse[..config.impulse_length_samples].to_vec();

    let mut issues = Vec::new();
    let calibration_to_apply = match calibration {
        Some(calibration) if calibration.covers(config.calibration_band_hz) => Some(calibration),
        Some(calibration) => {
            issues.push(
                SweepMeasurementQualityIssue::CalibrationCoverageInsufficient {
                    required_hz: config.calibration_band_hz,
                    available_hz: calibration.frequency_range_hz(),
                },
            );
            None
        }
        None if config.require_calibration => {
            issues.push(SweepMeasurementQualityIssue::CalibrationMissing);
            None
        }
        None => None,
    };

    let mut calibrated_spectrum = raw_transfer_spectrum;
    if let Some(calibration) = calibration_to_apply {
        let bin_hz = f64::from(config.sample_rate_hz) / fft_size as f64;
        for (index, bin) in calibrated_spectrum.iter_mut().enumerate() {
            let mirrored_index = index.min(fft_size - index);
            let frequency_hz = mirrored_index as f64 * bin_hz;
            if frequency_hz >= config.calibration_band_hz[0]
                && frequency_hz <= config.calibration_band_hz[1]
            {
                let correction_db = calibration.correction_db_at(frequency_hz)?;
                *bin *= 10.0_f64.powf(correction_db / 20.0);
            }
        }
    }
    let calibrated_full_impulse = inverse_real(calibrated_spectrum)?;
    let calibrated_impulse_samples =
        calibrated_full_impulse[..config.impulse_length_samples].to_vec();

    let capture_peak = detection
        .capture_segment
        .samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f64, f64::max);
    let clipped_sample_count = detection
        .capture_segment
        .samples
        .iter()
        .filter(|sample| sample.abs() >= config.clipping_threshold_linear)
        .count();
    if clipped_sample_count > 0 {
        issues.push(SweepMeasurementQualityIssue::InputClipping {
            clipped_sample_count,
        });
    }

    let local_start_floor = local_start.floor() as usize;
    let noise_end = local_start_floor
        .saturating_sub(config.pre_sweep_noise_guard_samples)
        .min(detection.capture_segment.samples.len());
    let noise_start = noise_end.saturating_sub(config.pre_sweep_noise_window_samples);
    let available_noise_samples = noise_end - noise_start;
    let noise_rms = if available_noise_samples >= config.minimum_pre_sweep_noise_samples {
        rms(&detection.capture_segment.samples[noise_start..noise_end])
    } else {
        issues.push(SweepMeasurementQualityIssue::InsufficientPreSweepNoise {
            available_samples: available_noise_samples,
            required_samples: config.minimum_pre_sweep_noise_samples,
        });
        None
    };
    let sweep_capture_rms = rms(&aligned_capture[..reference.len()]).unwrap_or(0.0);
    let capture_snr_db = noise_rms.map(|noise| level_db(sweep_capture_rms) - level_db(noise));
    if let Some(measured_db) = capture_snr_db {
        if measured_db < config.minimum_capture_snr_db {
            issues.push(SweepMeasurementQualityIssue::CaptureSnrTooLow {
                measured_db,
                required_db: config.minimum_capture_snr_db,
            });
        }
    }

    let reconstruction_fit_db =
        reconstruction_fit_db(reference, &uncalibrated_impulse_samples, &aligned_capture)?;
    if config.enforce_minimum_reconstruction_fit
        && reconstruction_fit_db < config.minimum_reconstruction_fit_db
    {
        issues.push(SweepMeasurementQualityIssue::ReconstructionFitTooLow {
            measured_db: reconstruction_fit_db,
            required_db: config.minimum_reconstruction_fit_db,
        });
    }
    if detection.start_absolute_correlation < config.minimum_start_correlation {
        issues.push(SweepMeasurementQualityIssue::StartCorrelationTooLow {
            measured: detection.start_absolute_correlation,
            required: config.minimum_start_correlation,
        });
    }
    if detection.estimated_clock_drift_ppm.abs() > config.maximum_clock_drift_ppm {
        issues.push(SweepMeasurementQualityIssue::ClockDriftOutOfRange {
            measured_ppm: detection.estimated_clock_drift_ppm,
            allowed_ppm: config.maximum_clock_drift_ppm,
        });
    }
    if detection.timing_fit_rms_samples > config.maximum_timing_fit_rms_samples {
        issues.push(SweepMeasurementQualityIssue::TimingFitUnstable {
            measured_rms_samples: detection.timing_fit_rms_samples,
            allowed_rms_samples: config.maximum_timing_fit_rms_samples,
        });
    }

    let calibrated_frequency_response = frequency_response(
        &calibrated_impulse_samples,
        config.sample_rate_hz,
        config.analysis_fft_size,
    )?;
    let impulse_peak_sample_index = calibrated_impulse_samples
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.abs().total_cmp(&right.abs()))
        .map_or(0, |(index, _)| index);
    let quality = SweepMeasurementQualityMetrics {
        accepted: issues.is_empty(),
        issues,
        capture_peak_dbfs: level_db(capture_peak),
        clipped_sample_count,
        pre_sweep_noise_rms_dbfs: noise_rms.map(level_db),
        sweep_capture_rms_dbfs: level_db(sweep_capture_rms),
        capture_snr_db,
        reconstruction_fit_db,
        reconstruction_fit_required: config.enforce_minimum_reconstruction_fit,
        start_absolute_correlation: detection.start_absolute_correlation,
        estimated_clock_drift_ppm: detection.estimated_clock_drift_ppm,
        timing_fit_rms_samples: detection.timing_fit_rms_samples,
        calibration_applied: calibration_to_apply.is_some(),
    };

    Ok(SweepMeasurement {
        algorithm_version: KNOWN_SWEEP_DECONVOLUTION_VERSION,
        sample_rate_hz: config.sample_rate_hz,
        uncalibrated_impulse_samples,
        calibrated_impulse_samples,
        calibrated_frequency_response,
        impulse_peak_sample_index,
        timing: SweepMeasurementTiming {
            recognized_sweep_start_capture_sample: detection.estimated_sweep_start_sample,
            capture_segment_start_sample: detection.capture_segment.start_sample,
            impulse_start_relative_to_recognized_sweep_seconds: -(config.impulse_pre_zero_samples
                as f64)
                / f64::from(config.sample_rate_hz),
            absolute_timing_eligible: false,
            capture_samples_per_reference_sample: detection.capture_samples_per_reference_sample,
        },
        quality,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::parse_umik_calibration;
    use crate::wireless_sweep::{
        WirelessClockDriftEvidence, WirelessSweepCaptureSegment, WirelessSweepDetection,
        WIRELESS_SWEEP_RECOGNITION_VERSION,
    };

    fn logarithmic_sweep(length: usize, sample_rate_hz: f64) -> Vec<f64> {
        let start_hz = 20.0_f64;
        let end_hz = 20_000.0_f64;
        let duration = length as f64 / sample_rate_hz;
        let ratio = end_hz / start_hz;
        (0..length)
            .map(|index| {
                let time = index as f64 / sample_rate_hz;
                let phase = std::f64::consts::TAU * start_hz * duration / ratio.ln()
                    * (ratio.powf(time / duration) - 1.0);
                let fade = ((index as f64 / 256.0).min(1.0))
                    * (((length - 1 - index) as f64 / 256.0).min(1.0));
                0.4 * fade * phase.sin()
            })
            .collect()
    }

    fn deterministic_noise(length: usize, amplitude: f64) -> Vec<f64> {
        let mut state = 0x13a5_7c9d_u32;
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let unit = f64::from(state >> 8) / f64::from(1_u32 << 24);
                amplitude * (2.0 * unit - 1.0)
            })
            .collect()
    }

    fn measurement_config() -> SweepDeconvolutionConfig {
        SweepDeconvolutionConfig {
            impulse_length_samples: 1_024,
            analysis_fft_size: 2_048,
            regularization_db: -180.0,
            minimum_capture_snr_db: 20.0,
            minimum_reconstruction_fit_db: 20.0,
            minimum_pre_sweep_noise_samples: 512,
            pre_sweep_noise_guard_samples: 64,
            ..SweepDeconvolutionConfig::default()
        }
    }

    fn fixture_detection(
        reference: &[f64],
        impulse: &[f64],
        pre_roll: usize,
        noise_amplitude: f64,
    ) -> WirelessSweepDetection {
        let convolved = fft_convolve(reference, impulse).unwrap();
        let convolved_noise = deterministic_noise(convolved.len(), noise_amplitude);
        let mut samples = deterministic_noise(pre_roll, noise_amplitude);
        samples.extend(
            convolved
                .iter()
                .zip(convolved_noise)
                .map(|(sample, noise)| sample + noise),
        );
        samples.extend(deterministic_noise(2_048, noise_amplitude));
        WirelessSweepDetection {
            algorithm_version: WIRELESS_SWEEP_RECOGNITION_VERSION,
            nominal_sample_rate_hz: 48_000,
            estimated_sweep_start_sample: pre_roll as f64,
            estimated_sweep_end_sample_exclusive: (pre_roll + reference.len()) as f64,
            start_signed_correlation: 0.98,
            start_absolute_correlation: 0.98,
            second_absolute_correlation: 0.05,
            polarity_inverted: false,
            capture_samples_per_reference_sample: 1.0,
            estimated_clock_drift_ppm: 0.0,
            clock_drift_evidence: WirelessClockDriftEvidence::IntraSweepSegmentFit,
            timing_fit_rms_samples: 0.1,
            segment_matches: Vec::new(),
            capture_segment: WirelessSweepCaptureSegment {
                start_sample: 0,
                end_sample_exclusive: samples.len(),
                samples,
            },
        }
    }

    fn flat_calibration(level_db: f64) -> MicrophoneCalibration {
        parse_umik_calibration(&format!("10 {level_db}\n24000 {level_db}\n")).unwrap()
    }

    #[test]
    fn recovers_a_known_room_ir_and_reuses_the_response_analysis_grid() {
        let reference = logarithmic_sweep(32_768, 48_000.0);
        let mut expected_ir = vec![0.0; 256];
        expected_ir[0] = 0.8;
        expected_ir[80] = -0.2;
        expected_ir[200] = 0.1;
        let detection = fixture_detection(&reference, &expected_ir, 2_048, 1.0e-7);
        let config = measurement_config();

        let measurement = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&flat_calibration(0.0)),
            &config,
        )
        .unwrap();

        assert!(measurement.quality.accepted, "{:?}", measurement.quality);
        assert!(measurement.quality.calibration_applied);
        assert!(!measurement.timing.absolute_timing_eligible);
        assert_eq!(measurement.impulse_peak_sample_index, 0);
        for (actual, expected) in measurement
            .uncalibrated_impulse_samples
            .iter()
            .zip(&expected_ir)
        {
            assert!((actual - expected).abs() < 2.0e-4);
        }
        let independently_analyzed = frequency_response(
            &measurement.calibrated_impulse_samples,
            48_000,
            config.analysis_fft_size,
        )
        .unwrap();
        assert_eq!(
            measurement.calibrated_frequency_response,
            independently_analyzed
        );
        assert!(measurement.quality.capture_snr_db.unwrap() > 100.0);
        assert!(measurement.quality.reconstruction_fit_db > 70.0);
    }

    /// One decaying room-mode resonator placed exactly midway between the
    /// legacy 1.465 Hz grid bins (their worst case) and exactly on a v4 bin.
    fn low_frequency_mode_ir(bandwidth_hz: f64) -> Vec<f64> {
        let sample_rate = 48_000.0_f64;
        let mode_hz = 49.0 * sample_rate / 65_536.0; // 35.889 Hz
        let tau_samples = sample_rate / (std::f64::consts::PI * bandwidth_hz);
        (0..48_000)
            .map(|index| {
                1.0e-4
                    * (-(index as f64) / tau_samples).exp()
                    * (std::f64::consts::TAU * mode_hz * index as f64 / sample_rate).cos()
            })
            .collect()
    }

    fn legacy_v3_sizes() -> SweepDeconvolutionConfig {
        SweepDeconvolutionConfig {
            impulse_length_samples: 16_384,
            analysis_fft_size: 32_768,
            ..SweepDeconvolutionConfig::default()
        }
    }

    #[test]
    fn a_narrow_low_frequency_mode_is_no_longer_underestimated_by_the_analysis_window() {
        // Expert review finding 4: a high-Q room mode a few hertz wide used to
        // lose several dB to the 0.341 s retained window plus 1.465 Hz grid
        // scalloping. A 1.5 Hz-wide mode fits the doubled window but is
        // visibly clipped and straddled by the legacy sizes.
        let reference = logarithmic_sweep(96_000, 48_000.0);
        let mode_hz = 49.0 * 48_000.0 / 65_536.0;
        let mode_ir = low_frequency_mode_ir(1.5);
        let detection = fixture_detection(&reference, &mode_ir, 2_048, 1.0e-9);
        let calibration = flat_calibration(0.0);

        let current = SweepDeconvolutionConfig::default();
        assert_eq!(current.impulse_length_samples, 32_768);
        assert_eq!(current.analysis_fft_size, 65_536);

        let old = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&calibration),
            &legacy_v3_sizes(),
        )
        .unwrap();
        let new = deconvolve_recognized_sweep(&reference, &detection, Some(&calibration), &current)
            .unwrap();
        assert!(old.quality.accepted, "{:?}", old.quality);
        assert!(new.quality.accepted, "{:?}", new.quality);
        assert_eq!(new.algorithm_version, KNOWN_SWEEP_DECONVOLUTION_VERSION);

        // Ground truth from the full, untruncated synthetic mode on a grid
        // fine enough (0.183 Hz) that its own scalloping is negligible.
        let truth = frequency_response(&mode_ir, 48_000, 262_144).unwrap();
        let truth_db = truth.magnitude_db[196];
        assert!((truth.frequencies_hz[196] - mode_hz).abs() < 1.0e-9);

        let new_bin = 49;
        assert!(
            (new.calibrated_frequency_response.frequencies_hz[new_bin] - mode_hz).abs() < 1.0e-9
        );
        let new_db = new.calibrated_frequency_response.magnitude_db[new_bin];
        // The legacy grid straddles the mode; give it its better neighbor.
        let old_best_db = old.calibrated_frequency_response.magnitude_db[24]
            .max(old.calibrated_frequency_response.magnitude_db[25]);

        // Measured: truth -5.94 dB, v4 -6.21 dB, legacy best -8.55 dB.
        assert!(
            (new_db - truth_db).abs() <= 0.75,
            "v4 analysis reads the mode peak at {new_db:.2} dB but the true level is {truth_db:.2} dB"
        );
        assert!(
            new_db >= old_best_db + 2.0,
            "v4 recovers {new_db:.2} dB at the mode but the legacy window/grid already saw \
             {old_best_db:.2} dB; the resolution regression is gone"
        );
    }

    /// The noise-floor estimate must not depend on how much pre-roll the caller
    /// retains. Retaining more reaches back toward the start marker, whose
    /// decaying reverberation is not ambient noise; before this was bounded, a
    /// wider pre-zero window silently lowered reported SNR and rejected a
    /// capture the same room had passed.
    #[test]
    fn the_noise_floor_estimate_does_not_follow_the_retained_pre_roll() {
        let reference = logarithmic_sweep(16_384, 48_000.0);
        let impulse = [0.5, 0.0, 0.125];
        let config = SweepDeconvolutionConfig {
            minimum_pre_sweep_noise_samples: 512,
            pre_sweep_noise_guard_samples: 64,
            pre_sweep_noise_window_samples: 2_048,
            ..measurement_config()
        };

        // Same capture and same sweep position; the only difference is how much
        // pre-roll the detection retained, and the earlier region is louder -
        // exactly what a decaying marker tail looks like.
        let snr_for = |pre_roll: usize| {
            let mut detection = fixture_detection(&reference, &impulse, pre_roll, 1.0e-8);
            for (index, sample) in detection.capture_segment.samples.iter_mut().enumerate() {
                if index + 4_096 < pre_roll {
                    *sample += 0.02;
                }
            }
            deconvolve_recognized_sweep(
                &reference,
                &detection,
                Some(&flat_calibration(0.0)),
                &config,
            )
            .unwrap()
            .quality
            .capture_snr_db
            .unwrap()
        };

        let narrow = snr_for(4_096);
        let wide = snr_for(12_288);
        assert!(
            (narrow - wide).abs() < 0.5,
            "the retained pre-roll changed the reported SNR from {narrow:.2} dB to {wide:.2} dB"
        );
    }

    #[test]
    fn a_very_high_q_mode_capture_is_accepted_instead_of_failing_closed() {
        // The other half of finding 4: with the old window a ~1 Hz-wide mode
        // leaves so much untruncated decay outside the retained IR that the
        // reconstruction-fit gate rejects the whole capture and the user ends
        // up with nothing. The doubled window keeps the same capture inside
        // the existing 12 dB gate without loosening it.
        let reference = logarithmic_sweep(96_000, 48_000.0);
        let mode_ir = low_frequency_mode_ir(1.0);
        let detection = fixture_detection(&reference, &mode_ir, 2_048, 1.0e-9);
        let calibration = flat_calibration(0.0);

        let old = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&calibration),
            &legacy_v3_sizes(),
        )
        .unwrap();
        let new = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&calibration),
            &SweepDeconvolutionConfig::default(),
        )
        .unwrap();

        assert!(
            !old.quality.accepted
                && old.quality.issues.iter().any(|issue| matches!(
                    issue,
                    SweepMeasurementQualityIssue::ReconstructionFitTooLow { .. }
                )),
            "the legacy window was expected to fail closed here: {:?}",
            old.quality
        );
        assert!(new.quality.accepted, "{:?}", new.quality);
        assert!(new.quality.reconstruction_fit_db >= 12.0);
    }

    #[test]
    fn applies_magnitude_calibration_without_modifying_the_retained_raw_ir() {
        let reference = logarithmic_sweep(16_384, 48_000.0);
        let impulse = [0.5, 0.0, 0.125];
        let detection = fixture_detection(&reference, &impulse, 2_048, 1.0e-8);
        let config = measurement_config();
        let zero = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&flat_calibration(0.0)),
            &config,
        )
        .unwrap();
        let plus_six = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&flat_calibration(6.020_599_913_279_624)),
            &config,
        )
        .unwrap();

        assert_eq!(
            zero.uncalibrated_impulse_samples,
            plus_six.uncalibrated_impulse_samples
        );
        let one_khz_bin = (1_000.0 / zero.calibrated_frequency_response.bin_hz()).round() as usize;
        let realized_db = plus_six.calibrated_frequency_response.magnitude_db[one_khz_bin]
            - zero.calibrated_frequency_response.magnitude_db[one_khz_bin];
        // The calibration filter is band limited to 20-20 kHz and then the
        // deliberately short 1024-sample test IR is truncated, so its analyzed
        // realization is slightly below the exact spectral-domain 6.02 dB.
        assert!(
            (realized_db - 6.020_599_913_279_624).abs() < 0.5,
            "realized calibration was {realized_db} dB"
        );
    }

    #[test]
    fn reconstruction_fit_can_remain_diagnostic_when_independent_gates_qualify_timing() {
        let reference = logarithmic_sweep(16_384, 48_000.0);
        let detection = fixture_detection(&reference, &[0.5, 0.0, 0.125], 2_048, 1.0e-8);
        let mut config = measurement_config();
        config.minimum_reconstruction_fit_db = 180.0;
        config.enforce_minimum_reconstruction_fit = false;

        let measurement = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&flat_calibration(0.0)),
            &config,
        )
        .unwrap();

        assert!(measurement.quality.accepted);
        assert!(!measurement.quality.reconstruction_fit_required);
        assert!(measurement.quality.reconstruction_fit_db < 180.0);
        assert!(!measurement.quality.issues.iter().any(|issue| matches!(
            issue,
            SweepMeasurementQualityIssue::ReconstructionFitTooLow { .. }
        )));
    }

    #[test]
    fn quality_report_rejects_missing_calibration_clipping_and_bad_timing() {
        let reference = logarithmic_sweep(8_192, 48_000.0);
        let mut detection = fixture_detection(&reference, &[0.1], 2_048, 0.02);
        detection.capture_segment.samples[0] = 1.0;
        detection.start_absolute_correlation = 0.2;
        detection.estimated_clock_drift_ppm = 2_500.0;
        detection.timing_fit_rms_samples = 30.0;
        let mut config = measurement_config();
        config.minimum_capture_snr_db = 40.0;
        config.minimum_reconstruction_fit_db = 80.0;

        let measurement =
            deconvolve_recognized_sweep(&reference, &detection, None, &config).unwrap();
        assert!(!measurement.quality.accepted);
        assert!(!measurement.quality.calibration_applied);
        assert!(measurement
            .quality
            .issues
            .contains(&SweepMeasurementQualityIssue::CalibrationMissing));
        assert!(measurement
            .quality
            .issues
            .iter()
            .any(|issue| matches!(issue, SweepMeasurementQualityIssue::InputClipping { .. })));
        assert!(measurement.quality.issues.iter().any(|issue| matches!(
            issue,
            SweepMeasurementQualityIssue::StartCorrelationTooLow { .. }
        )));
        assert!(measurement.quality.issues.iter().any(|issue| matches!(
            issue,
            SweepMeasurementQualityIssue::ClockDriftOutOfRange { .. }
        )));
        assert!(measurement.quality.issues.iter().any(|issue| matches!(
            issue,
            SweepMeasurementQualityIssue::TimingFitUnstable { .. }
        )));
    }

    #[test]
    fn insufficient_calibration_coverage_blocks_acceptance_without_extrapolation() {
        let reference = logarithmic_sweep(8_192, 48_000.0);
        let detection = fixture_detection(&reference, &[0.5], 2_048, 1.0e-8);
        let calibration = parse_umik_calibration("100 0\n10000 0\n").unwrap();
        let measurement = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&calibration),
            &measurement_config(),
        )
        .unwrap();

        assert!(!measurement.quality.accepted);
        assert!(!measurement.quality.calibration_applied);
        assert!(measurement.quality.issues.iter().any(|issue| matches!(
            issue,
            SweepMeasurementQualityIssue::CalibrationCoverageInsufficient { .. }
        )));
    }

    #[test]
    fn refuses_an_ir_length_without_retained_post_roll() {
        let reference = logarithmic_sweep(8_192, 48_000.0);
        let mut detection = fixture_detection(&reference, &[0.5], 2_048, 0.0);
        detection
            .capture_segment
            .samples
            .truncate(2_048 + reference.len());
        detection.capture_segment.end_sample_exclusive = detection.capture_segment.samples.len();
        let error = deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&flat_calibration(0.0)),
            &measurement_config(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("enough post-roll"));
    }

    #[test]
    fn validates_nonfinite_samples_rate_and_configuration() {
        let reference = logarithmic_sweep(8_192, 48_000.0);
        let detection = fixture_detection(&reference, &[0.5], 2_048, 0.0);
        let mut bad_reference = reference.clone();
        bad_reference[3] = f64::NAN;
        assert_eq!(
            deconvolve_recognized_sweep(
                &bad_reference,
                &detection,
                Some(&flat_calibration(0.0)),
                &measurement_config()
            )
            .unwrap_err(),
            DspError::NonFinite {
                context: "measurement sweep reference",
                index: 3,
            }
        );

        let mut mismatched_rate = measurement_config();
        mismatched_rate.sample_rate_hz = 44_100;
        mismatched_rate.calibration_band_hz[1] = 20_000.0;
        assert!(matches!(
            deconvolve_recognized_sweep(
                &reference,
                &detection,
                Some(&flat_calibration(0.0)),
                &mismatched_rate
            ),
            Err(DspError::ShapeMismatch(_))
        ));

        let mut invalid_fft = measurement_config();
        invalid_fft.analysis_fft_size = 1_500;
        assert!(deconvolve_recognized_sweep(
            &reference,
            &detection,
            Some(&flat_calibration(0.0)),
            &invalid_fft
        )
        .is_err());
    }
}
