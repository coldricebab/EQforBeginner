use eqforbeginner_audio_io::{InputCaptureCancellation, MonoInputCapture};
use eqforbeginner_dsp_core::wireless_sweep::{
    recognize_wireless_sweep, WirelessSweepRecognition, WirelessSweepRecognitionConfig,
    WirelessSweepRejectionReason,
};
use hound::{SampleFormat, WavReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::sync::{Arc, Mutex};

pub const MAX_REFERENCE_WAV_BYTES: usize = 32 * 1024 * 1024;
const REQUIRED_SAMPLE_RATE_HZ: u32 = 48_000;
const MIN_DURATION_SECONDS: f64 = 1.0;
const MAX_DURATION_SECONDS: f64 = 60.0;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceChannel {
    Mono,
    Left,
    Right,
    IdenticalStereo,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WirelessSweepImport {
    pub sweep_id: String,
    pub sha256: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub frame_count: usize,
    pub duration_seconds: f64,
    pub reference_channel: ReferenceChannel,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    pub sweep_like: bool,
}

#[derive(Clone, Debug)]
pub struct WirelessSweepReference {
    pub metadata: WirelessSweepImport,
    pub samples: Vec<f32>,
}

#[derive(Default)]
pub struct WirelessSweepState {
    reference: Mutex<Option<Arc<WirelessSweepReference>>>,
    active_capture: Mutex<Option<InputCaptureCancellation>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WirelessSweepDetectionStatus {
    Detected,
    NotDetected,
    Ambiguous,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WirelessSweepCaptureResult {
    pub sweep_id: String,
    pub reference_sha256: String,
    pub status: WirelessSweepDetectionStatus,
    pub algorithm_version: String,
    pub rejection_reason: Option<String>,
    pub sample_rate_hz: u32,
    pub captured_frames: usize,
    pub detected_start_seconds: Option<f64>,
    pub detected_end_seconds: Option<f64>,
    pub correlation: Option<f64>,
    pub second_best_correlation: Option<f64>,
    pub confidence_margin: Option<f64>,
    pub clock_drift_ppm: Option<f64>,
    pub clock_drift_evidence: Option<String>,
    pub timing_fit_rms_samples: Option<f64>,
    pub polarity_inverted: Option<bool>,
    pub input_peak_dbfs: Option<f64>,
    pub clipped_samples: u64,
    pub stream_error_count: u64,
    pub dropout_suspected: bool,
    pub quality_accepted: bool,
    /// A single imported sweep cannot establish inter-channel timing, even
    /// when the intra-sweep sample-rate slope looks stable.
    pub timing_eligible: bool,
}

impl WirelessSweepState {
    pub fn import_reference(&self, bytes: &[u8]) -> Result<WirelessSweepImport, String> {
        if self
            .active_capture
            .lock()
            .map_err(|_| "wireless capture state lock was poisoned".to_string())?
            .is_some()
        {
            return Err("stop the active microphone capture before changing the WAV".to_string());
        }
        let reference = Arc::new(decode_reference_wav(bytes)?);
        let metadata = reference.metadata.clone();
        *self
            .reference
            .lock()
            .map_err(|_| "wireless reference state lock was poisoned".to_string())? =
            Some(reference);
        Ok(metadata)
    }

    pub fn begin_capture(
        &self,
        sweep_id: &str,
    ) -> Result<(Arc<WirelessSweepReference>, InputCaptureCancellation), String> {
        let mut active = self
            .active_capture
            .lock()
            .map_err(|_| "wireless capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err("a wireless microphone capture is already running".to_string());
        }
        let reference = self
            .reference
            .lock()
            .map_err(|_| "wireless reference state lock was poisoned".to_string())?
            .clone()
            .ok_or_else(|| "import a reference WAV before arming the microphone".to_string())?;
        if reference.metadata.sweep_id != sweep_id {
            return Err("the selected WAV changed; import it again before capture".to_string());
        }
        let cancellation = InputCaptureCancellation::new();
        *active = Some(cancellation.clone());
        Ok((reference, cancellation))
    }

    pub fn finish_capture(&self) {
        if let Ok(mut active) = self.active_capture.lock() {
            *active = None;
        }
    }

    pub fn cancel_capture(&self) -> Result<bool, String> {
        let cancellation = self
            .active_capture
            .lock()
            .map_err(|_| "wireless capture state lock was poisoned".to_string())?
            .clone();
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

pub fn analyze_capture(
    reference: &WirelessSweepReference,
    capture: &MonoInputCapture,
) -> Result<WirelessSweepCaptureResult, String> {
    if capture.cancelled {
        return Err("wireless microphone capture was cancelled".to_string());
    }
    if capture.sample_rate_hz != reference.metadata.sample_rate_hz {
        return Err(format!(
            "capture/reference sample-rate mismatch: {} Hz vs {} Hz",
            capture.sample_rate_hz, reference.metadata.sample_rate_hz
        ));
    }

    let reference_samples = reference
        .samples
        .iter()
        .map(|sample| f64::from(*sample))
        .collect::<Vec<_>>();
    let microphone_samples = capture
        .samples
        .iter()
        .map(|sample| f64::from(*sample))
        .collect::<Vec<_>>();
    let config = WirelessSweepRecognitionConfig {
        nominal_sample_rate_hz: reference.metadata.sample_rate_hz,
        maximum_reference_samples: reference_samples.len().max(1),
        maximum_capture_samples: microphone_samples.len().max(1),
        ..WirelessSweepRecognitionConfig::default()
    };
    let recognition = recognize_wireless_sweep(&reference_samples, &microphone_samples, &config)
        .map_err(|error| format!("wireless sweep recognition failed: {error}"))?;

    let sample_rate = f64::from(capture.sample_rate_hz);
    let (
        status,
        algorithm_version,
        rejection_reason,
        start,
        end,
        correlation,
        second,
        drift,
        drift_evidence,
        timing_fit,
        polarity_inverted,
    ) = match recognition {
        WirelessSweepRecognition::Detected(detection) => (
            WirelessSweepDetectionStatus::Detected,
            detection.algorithm_version.to_string(),
            None,
            Some(detection.estimated_sweep_start_sample / sample_rate),
            Some(detection.estimated_sweep_end_sample_exclusive / sample_rate),
            Some(detection.start_absolute_correlation),
            Some(detection.second_absolute_correlation),
            Some(detection.estimated_clock_drift_ppm),
            Some("intra_sweep_segment_fit".to_string()),
            Some(detection.timing_fit_rms_samples),
            Some(detection.polarity_inverted),
        ),
        WirelessSweepRecognition::NotDetected(outcome) => (
            WirelessSweepDetectionStatus::NotDetected,
            eqforbeginner_dsp_core::wireless_sweep::WIRELESS_SWEEP_RECOGNITION_VERSION.to_string(),
            Some(rejection_reason_name(outcome.reason).to_string()),
            None,
            None,
            Some(outcome.strongest_absolute_correlation),
            None,
            None,
            None,
            None,
            None,
        ),
        WirelessSweepRecognition::LikelyFalsePositive(outcome) => (
            WirelessSweepDetectionStatus::NotDetected,
            eqforbeginner_dsp_core::wireless_sweep::WIRELESS_SWEEP_RECOGNITION_VERSION.to_string(),
            Some(format!(
                "likely_false_positive:{}",
                rejection_reason_name(outcome.reason)
            )),
            Some(outcome.candidate_start_sample / sample_rate),
            None,
            Some(outcome.start_absolute_correlation),
            None,
            outcome.estimated_clock_drift_ppm,
            outcome
                .estimated_clock_drift_ppm
                .map(|_| "intra_sweep_segment_fit".to_string()),
            outcome.timing_fit_rms_samples,
            None,
        ),
        WirelessSweepRecognition::Ambiguous(outcome) => (
            WirelessSweepDetectionStatus::Ambiguous,
            eqforbeginner_dsp_core::wireless_sweep::WIRELESS_SWEEP_RECOGNITION_VERSION.to_string(),
            Some("multiple_complete_playback_candidates".to_string()),
            Some(outcome.strongest_candidate_start_sample / sample_rate),
            None,
            Some(outcome.strongest_absolute_correlation),
            Some(outcome.second_absolute_correlation),
            None,
            None,
            None,
            None,
        ),
    };

    let confidence_margin = correlation
        .zip(second)
        .map(|(primary, secondary)| (primary - secondary).clamp(0.0, 1.0));
    let dropout_suspected = capture.sample_drop_detected
        || capture.timed_out
        || !capture.capture_complete
        || capture.callback_format_error
        || capture.non_finite_sample_count > 0;
    let quality_accepted = !dropout_suspected
        && !capture.input_clipped
        && !capture.stream_error_detected
        && capture.stream_error_count == 0;

    Ok(WirelessSweepCaptureResult {
        sweep_id: reference.metadata.sweep_id.clone(),
        reference_sha256: reference.metadata.sha256.clone(),
        status,
        algorithm_version,
        rejection_reason,
        sample_rate_hz: capture.sample_rate_hz,
        captured_frames: capture.captured_samples,
        detected_start_seconds: start,
        detected_end_seconds: end,
        correlation,
        second_best_correlation: second,
        confidence_margin,
        clock_drift_ppm: drift,
        clock_drift_evidence: drift_evidence,
        timing_fit_rms_samples: timing_fit,
        polarity_inverted,
        input_peak_dbfs: capture.peak_dbfs.map(f64::from),
        clipped_samples: capture.clipped_sample_count,
        stream_error_count: capture.stream_error_count,
        dropout_suspected,
        quality_accepted,
        timing_eligible: false,
    })
}

fn rejection_reason_name(reason: WirelessSweepRejectionReason) -> &'static str {
    match reason {
        WirelessSweepRejectionReason::CorrelationBelowThreshold => "correlation_below_threshold",
        WirelessSweepRejectionReason::InconsistentSegments => "inconsistent_segments",
        WirelessSweepRejectionReason::ClockDriftOutOfRange => "clock_drift_out_of_range",
        WirelessSweepRejectionReason::TimingFitUnstable => "timing_fit_unstable",
        WirelessSweepRejectionReason::IncompleteCapture => "incomplete_capture",
    }
}

pub fn decode_reference_wav(bytes: &[u8]) -> Result<WirelessSweepReference, String> {
    if bytes.is_empty() {
        return Err("the selected WAV is empty".to_string());
    }
    if bytes.len() > MAX_REFERENCE_WAV_BYTES {
        return Err(format!(
            "the selected WAV exceeds the {} MiB safety limit",
            MAX_REFERENCE_WAV_BYTES / (1024 * 1024)
        ));
    }

    let mut reader =
        WavReader::new(Cursor::new(bytes)).map_err(|error| format!("invalid WAV: {error}"))?;
    let spec = reader.spec();
    if spec.sample_rate != REQUIRED_SAMPLE_RATE_HZ {
        return Err(format!(
            "wireless measurement requires a 48 kHz WAV; this file is {} Hz",
            spec.sample_rate
        ));
    }
    if !(1..=2).contains(&spec.channels) {
        return Err(format!(
            "wireless measurement accepts mono or stereo WAV files; this file has {} channels",
            spec.channels
        ));
    }
    match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Int, 8 | 16 | 24 | 32) | (SampleFormat::Float, 32) => {}
        _ => {
            return Err(format!(
                "unsupported WAV sample encoding: {:?} {}-bit",
                spec.sample_format, spec.bits_per_sample
            ));
        }
    }

    let interleaved = match spec.sample_format {
        SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map_err(|error| format!("could not decode WAV sample: {error}")))
            .collect::<Result<Vec<_>, _>>()?,
        SampleFormat::Int => {
            let scale = 2_f64.powi(i32::from(spec.bits_per_sample) - 1);
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| (f64::from(value) / scale) as f32)
                        .map_err(|error| format!("could not decode WAV sample: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?
        }
    };

    if interleaved.len() % usize::from(spec.channels) != 0 {
        return Err("the WAV ends in a partial audio frame".to_string());
    }
    if interleaved
        .iter()
        .any(|sample| !sample.is_finite() || sample.abs() > 1.000_1)
    {
        return Err("the WAV contains NaN, infinity, or out-of-range samples".to_string());
    }

    let frame_count = interleaved.len() / usize::from(spec.channels);
    let duration_seconds = frame_count as f64 / f64::from(spec.sample_rate);
    if !(MIN_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&duration_seconds) {
        return Err(format!(
            "the reference must be between {MIN_DURATION_SECONDS:.0} and {MAX_DURATION_SECONDS:.0} seconds; this file is {duration_seconds:.2} seconds"
        ));
    }

    let (samples, reference_channel) = select_reference_channel(&interleaved, spec.channels)?;
    let peak = samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    let rms = root_mean_square(&samples);
    if peak < 0.001 || rms < 0.000_1 {
        return Err("the WAV is silent or too quiet to use as a reference".to_string());
    }

    let sha256 = format!("{:x}", Sha256::digest(bytes));
    let metadata = WirelessSweepImport {
        sweep_id: sha256.clone(),
        sha256,
        sample_rate_hz: spec.sample_rate,
        channels: spec.channels,
        frame_count,
        duration_seconds,
        reference_channel,
        peak_dbfs: amplitude_dbfs(f64::from(peak)),
        rms_dbfs: amplitude_dbfs(rms),
        sweep_like: looks_like_rising_sweep(&samples, spec.sample_rate),
    };

    Ok(WirelessSweepReference { metadata, samples })
}

fn select_reference_channel(
    interleaved: &[f32],
    channels: u16,
) -> Result<(Vec<f32>, ReferenceChannel), String> {
    if channels == 1 {
        return Ok((interleaved.to_vec(), ReferenceChannel::Mono));
    }

    let mut left = Vec::with_capacity(interleaved.len() / 2);
    let mut right = Vec::with_capacity(interleaved.len() / 2);
    for frame in interleaved.chunks_exact(2) {
        left.push(frame[0]);
        right.push(frame[1]);
    }
    let left_rms = root_mean_square(&left);
    let right_rms = root_mean_square(&right);
    let stronger = left_rms.max(right_rms);
    let weaker = left_rms.min(right_rms);

    if weaker <= stronger * 0.001 {
        return if left_rms > right_rms {
            Ok((left, ReferenceChannel::Left))
        } else {
            Ok((right, ReferenceChannel::Right))
        };
    }

    let level_ratio = left_rms / right_rms;
    let correlation = normalized_correlation(&left, &right);
    if correlation >= 0.999 && (0.89..=1.12).contains(&level_ratio) {
        let downmixed = left
            .iter()
            .zip(&right)
            .map(|(left, right)| (left + right) * 0.5)
            .collect();
        return Ok((downmixed, ReferenceChannel::IdenticalStereo));
    }

    // REW-style measurement files can place the long measurement sweep on
    // one channel and shorter acoustic timing-reference markers on the other.
    // Select the measurement channel only when its total RMS energy is at
    // least 9.5 dB higher; similarly active stereo content remains rejected.
    if stronger >= weaker * 3.0 {
        return if left_rms > right_rms {
            Ok((left, ReferenceChannel::Left))
        } else {
            Ok((right, ReferenceChannel::Right))
        };
    }

    Err(format!(
        "the two stereo channels contain different active signals at similar levels (correlation {correlation:.4}); use mono, a dominant measurement channel with timing markers, one active channel, or identical L/R content"
    ))
}

fn root_mean_square(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy = samples
        .iter()
        .map(|sample| f64::from(*sample).powi(2))
        .sum::<f64>();
    (energy / samples.len() as f64).sqrt()
}

fn normalized_correlation(left: &[f32], right: &[f32]) -> f64 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let (dot, left_energy, right_energy) =
        left.iter()
            .zip(right)
            .fold((0.0, 0.0, 0.0), |accumulator, samples| {
                let left = f64::from(*samples.0);
                let right = f64::from(*samples.1);
                (
                    accumulator.0 + left * right,
                    accumulator.1 + left * left,
                    accumulator.2 + right * right,
                )
            });
    if left_energy <= f64::EPSILON || right_energy <= f64::EPSILON {
        0.0
    } else {
        dot / (left_energy * right_energy).sqrt()
    }
}

fn amplitude_dbfs(amplitude: f64) -> f64 {
    if amplitude <= f64::EPSILON {
        f64::NEG_INFINITY
    } else {
        20.0 * amplitude.log10()
    }
}

fn looks_like_rising_sweep(samples: &[f32], sample_rate_hz: u32) -> bool {
    let peak = samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    let activity_threshold = (peak * 0.003_162_277_6).max(0.000_01);
    let Some(first) = samples
        .iter()
        .position(|sample| sample.abs() >= activity_threshold)
    else {
        return false;
    };
    let Some(last) = samples
        .iter()
        .rposition(|sample| sample.abs() >= activity_threshold)
    else {
        return false;
    };
    if last <= first || last - first < sample_rate_hz as usize / 2 {
        return false;
    }

    let active = &samples[first..=last];
    let segment_length = active.len() / 12;
    if segment_length < 32 {
        return false;
    }
    let mut crossing_rates = Vec::with_capacity(12);
    for segment_index in 0..12 {
        let start = segment_index * segment_length;
        let end = if segment_index == 11 {
            active.len()
        } else {
            (segment_index + 1) * segment_length
        };
        let segment = &active[start..end];
        let crossings = segment
            .windows(2)
            .filter(|pair| (pair[0] < 0.0 && pair[1] >= 0.0) || (pair[0] >= 0.0 && pair[1] < 0.0))
            .count();
        crossing_rates.push(crossings as f64 / segment.len() as f64);
    }

    let early = (crossing_rates[0] + crossing_rates[1] + crossing_rates[2]) / 3.0;
    let late = (crossing_rates[9] + crossing_rates[10] + crossing_rates[11]) / 3.0;
    let rises = crossing_rates
        .windows(2)
        .filter(|pair| pair[1] >= pair[0] * 0.92)
        .count();
    early > 0.0 && late >= early * 6.0 && rises >= 8
}

#[cfg(test)]
mod tests {
    use super::*;
    use hound::{WavSpec, WavWriter};
    use std::f64::consts::TAU;

    fn rising_sweep_wav(channels: u16, active_channel: Option<usize>) -> Vec<u8> {
        let spec = WavSpec {
            channels,
            sample_rate: REQUIRED_SAMPLE_RATE_HZ,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).expect("create test WAV");
            let frames = REQUIRED_SAMPLE_RATE_HZ as usize * 2;
            let start_hz = 30.0_f64;
            let end_hz = 16_000.0_f64;
            let logarithmic_span = (end_hz / start_hz).ln();
            let duration = frames as f64 / f64::from(REQUIRED_SAMPLE_RATE_HZ);
            for frame in 0..frames {
                let time = frame as f64 / f64::from(REQUIRED_SAMPLE_RATE_HZ);
                let phase = TAU * start_hz * duration / logarithmic_span
                    * ((logarithmic_span * time / duration).exp() - 1.0);
                let sample = (phase.sin() * 0.25 * f64::from(i16::MAX)) as i16;
                for channel in 0..usize::from(channels) {
                    let value = match active_channel {
                        Some(active) if channel != active => 0,
                        _ => sample,
                    };
                    writer.write_sample(value).expect("write sample");
                }
            }
            writer.finalize().expect("finalize WAV");
        }
        cursor.into_inner()
    }

    #[test]
    fn accepts_a_48k_mono_rising_sweep_and_hashes_the_original() {
        let bytes = rising_sweep_wav(1, None);
        let reference = decode_reference_wav(&bytes).expect("decode reference");

        assert_eq!(reference.metadata.sample_rate_hz, 48_000);
        assert_eq!(reference.metadata.reference_channel, ReferenceChannel::Mono);
        assert_eq!(reference.samples.len(), 96_000);
        assert!(reference.metadata.sweep_like);
        assert_eq!(reference.metadata.sha256.len(), 64);
        assert_eq!(reference.metadata.sweep_id, reference.metadata.sha256);
    }

    #[test]
    fn selects_the_only_active_stereo_channel() {
        let bytes = rising_sweep_wav(2, Some(1));
        let reference = decode_reference_wav(&bytes).expect("decode reference");

        assert_eq!(
            reference.metadata.reference_channel,
            ReferenceChannel::Right
        );
        assert_eq!(reference.samples.len(), 96_000);
    }

    #[test]
    fn rejects_non_project_sample_rates() {
        let spec = WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).expect("create WAV");
            for _ in 0..44_100 {
                writer.write_sample(0_i16).expect("write");
            }
            writer.finalize().expect("finalize");
        }

        let error = decode_reference_wav(&cursor.into_inner()).expect_err("must reject 44.1k");
        assert!(error.contains("48 kHz"));
    }

    #[test]
    fn rejects_two_different_active_stereo_signals() {
        let spec = WavSpec {
            channels: 2,
            sample_rate: REQUIRED_SAMPLE_RATE_HZ,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).expect("create WAV");
            for frame in 0..REQUIRED_SAMPLE_RATE_HZ {
                let time = f64::from(frame) / f64::from(REQUIRED_SAMPLE_RATE_HZ);
                writer
                    .write_sample((TAU * 300.0 * time).sin() as f32 * 0.2)
                    .expect("write left");
                writer
                    .write_sample((TAU * 900.0 * time).sin() as f32 * 0.2)
                    .expect("write right");
            }
            writer.finalize().expect("finalize");
        }

        let error =
            decode_reference_wav(&cursor.into_inner()).expect_err("must reject different L/R");
        assert!(error.contains("different active signals"));
    }

    #[test]
    fn imported_reference_is_detected_in_a_complete_microphone_capture() {
        let bytes = rising_sweep_wav(1, None);
        let reference = decode_reference_wav(&bytes).expect("decode reference");
        let pre_roll = REQUIRED_SAMPLE_RATE_HZ as usize / 10;
        let post_roll = REQUIRED_SAMPLE_RATE_HZ as usize;
        let mut samples = Vec::with_capacity(pre_roll + reference.samples.len() + post_roll);
        samples.resize(pre_roll, 0.0);
        samples.extend(reference.samples.iter().map(|sample| sample * 0.4));
        samples.resize(samples.capacity(), 0.0);
        let captured_samples = samples.len();
        let peak_linear = samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        let capture = MonoInputCapture {
            host: "test".to_string(),
            device_id: "test::input".to_string(),
            device_name: Some("synthetic microphone".to_string()),
            sample_rate_hz: REQUIRED_SAMPLE_RATE_HZ,
            channels: 1,
            source_channels: 1,
            input_channel_index: 0,
            sample_format: "f32".to_string(),
            requested_duration_ms: 3_100,
            requested_samples: captured_samples,
            maximum_samples: captured_samples,
            elapsed_ms: 3_100,
            samples,
            captured_samples,
            capture_complete: true,
            automatic_completion_detected: false,
            timed_out: false,
            cancelled: false,
            peak_linear,
            peak_dbfs: Some(20.0 * peak_linear.log10()),
            clipping_threshold_linear: 0.999,
            clipped_sample_count: 0,
            input_clipped: false,
            non_finite_sample_count: 0,
            sample_drop_detected: false,
            xrun_count: 0,
            callback_lock_drop_frames: 0,
            timestamp_gap_frames: 0,
            timestamp_discontinuity_count: 0,
            missing_samples_at_end: 0,
            stream_error_detected: false,
            stream_error_count: 0,
            stream_error_details_lost: 0,
            stream_errors: Vec::new(),
            callback_format_error: false,
        };

        let result = analyze_capture(&reference, &capture).expect("analyze capture");

        assert_eq!(result.status, WirelessSweepDetectionStatus::Detected);
        assert!(result.correlation.is_some_and(|score| score > 0.99));
        assert!(result.quality_accepted);
        assert!(!result.timing_eligible);
        assert_eq!(
            result.clock_drift_evidence.as_deref(),
            Some("intra_sweep_segment_fit")
        );
    }

    #[test]
    fn imports_the_project_wireless_sweep_fixtures() {
        let fixture_root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/sweeps");
        for (file_name, expected_channel) in [
            ("Sweep_L_20-20k_refR.wav", ReferenceChannel::Left),
            ("Sweep_R_20-20k_refR.wav", ReferenceChannel::Right),
        ] {
            let bytes = std::fs::read(fixture_root.join(file_name)).expect("read sweep fixture");
            let reference = decode_reference_wav(&bytes).expect("decode sweep fixture");

            assert_eq!(reference.metadata.sample_rate_hz, REQUIRED_SAMPLE_RATE_HZ);
            assert_eq!(reference.metadata.channels, 2);
            assert_eq!(reference.metadata.reference_channel, expected_channel);
            assert!(reference.metadata.sweep_like);
        }
    }
}
