//! Cross-platform audio-device discovery and bounded microphone capture for the
//! 48 kHz measurement path.
//!
//! Input capture is real CPAL hardware I/O. It opens the exact host-qualified
//! device ID selected by the caller, extracts one selected channel from a
//! native 48 kHz PCM configuration, and never substitutes synthetic samples
//! when hardware is missing.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Data, ErrorKind, FromSample, InputCallbackInfo, Sample, SampleFormat, StreamInstant,
    SupportedBufferSize, SupportedStreamConfig, SupportedStreamConfigRange,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::mem;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The only project rate admitted by the first real-audio vertical slice.
pub const PROJECT_SAMPLE_RATE_HZ: u32 = 48_000;
/// Samples at or above this absolute level are treated as digital full-scale
/// clipping. This is one signed 16-bit quantization step below full scale.
pub const INPUT_CLIPPING_THRESHOLD_LINEAR: f32 = 32_767.0 / 32_768.0;

const CAPTURE_COMPLETION_GRACE: Duration = Duration::from_secs(2);
const PLAYBACK_COMPLETION_GRACE: Duration = Duration::from_secs(2);
const PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Let the driver convert the frames already handed to it before the stream is
/// dropped, so the sweep's end marker is not truncated.
const PLAYBACK_DRAIN: Duration = Duration::from_millis(500);
const PLAYBACK_DEADLINE_GRACE: Duration = Duration::from_secs(10);
/// How long a device may take to publish a rate the host has already accepted.
const OUTPUT_RATE_SETTLE: Duration = Duration::from_millis(500);
/// Playback rate bounds. The floor keeps a typo from opening a stream that
/// would take minutes to drain; the ceiling covers every rate a CoreAudio or
/// WASAPI device realistically reports.
const MINIMUM_PLAYBACK_RATE_HZ: u32 = 8_000;
const MAXIMUM_PLAYBACK_RATE_HZ: u32 = 768_000;
const MAXIMUM_PLAYBACK_CHANNELS: usize = 8;
/// Ten minutes of 8-channel 48 kHz audio. A measurement sweep is far shorter;
/// this only bounds what a caller can ask the process to hold.
const MAXIMUM_PLAYBACK_SAMPLES: usize = 8 * 48_000 * 600;
const AUTOMATIC_COMPLETION_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_RECORDED_STREAM_ERRORS: usize = 16;
const TIMESTAMP_GAP_TOLERANCE_FRAMES: u64 = 2;

type AutomaticCompletionDetector<'a> = dyn FnMut(usize, &[f32]) -> bool + 'a;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceInventory {
    pub host: String,
    pub sample_rate_hz: u32,
    pub inputs: Vec<AudioDeviceDescriptor>,
    pub outputs: Vec<AudioDeviceDescriptor>,
    pub warnings: Vec<AudioScanWarning>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceDescriptor {
    /// CPAL's host-qualified ID. Stability is backend-dependent, so a saved
    /// project must revalidate it against current metadata before opening.
    pub id: String,
    pub name: String,
    pub manufacturer: Option<String>,
    pub device_type: String,
    pub interface_type: String,
    pub is_default: bool,
    /// The rate the device is running at right now, as the host reports it -
    /// on macOS the Audio MIDI Setup value, on Windows the shared mix format.
    /// `None` when the host could not be asked. A device that is not already
    /// at 48 kHz still measures correctly, but see
    /// [`play_interleaved_output`] for what in-app playback does to it.
    pub current_sample_rate_hz: Option<u32>,
    /// Only configurations that explicitly contain 48 kHz are retained.
    pub configurations: Vec<ProjectStreamConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStreamConfig {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_format: String,
    pub minimum_buffer_size_frames: Option<u32>,
    pub maximum_buffer_size_frames: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDirection {
    Input,
    Output,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioScanWarning {
    pub device_id: Option<String>,
    pub direction: Option<AudioDirection>,
    pub message: String,
}

/// A bounded, synchronous capture request.
///
/// `maximum_samples` is an explicit caller-controlled memory ceiling and must
/// be large enough to contain `duration_ms` at 48 kHz. This avoids silently
/// shortening a requested measurement.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonoInputCaptureRequest {
    /// Host-qualified CPAL device ID returned by
    /// [`scan_default_host_inputs`].
    pub device_id: String,
    /// Zero-based channel to extract from the native interleaved input stream.
    /// CoreAudio can expose a physically mono USB microphone through a
    /// two-channel PCM configuration.
    #[serde(default)]
    pub input_channel_index: u16,
    pub duration_ms: u64,
    pub maximum_samples: usize,
}

/// One bounded playback of a decoded interleaved buffer.
///
/// This exists only for the optional in-app sweep playback. It carries no
/// microphone state and is never consulted by sweep recognition.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterleavedOutputPlaybackRequest {
    /// Host-qualified CPAL device ID returned by
    /// [`scan_default_host_outputs`].
    pub device_id: String,
    /// Interleaved PCM at `sample_rate_hz`, `channels` samples per frame.
    pub frames: Vec<f32>,
    pub channels: u16,
    /// The rate `frames` is authored at, and the rate the device is opened at.
    /// Callers should hand over the device's own rate - see
    /// [`output_device_sample_rate`] and [`play_interleaved_output`].
    pub sample_rate_hz: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPlaybackReport {
    pub host: String,
    pub device_id: String,
    pub device_name: Option<String>,
    pub sample_rate_hz: u32,
    /// The device's own rate before the sweep, as the host reported it.
    pub device_rate_before_hz: Option<u32>,
    /// Whether the sweep had to move the device off that rate to reach 48 kHz.
    pub device_rate_forced: bool,
    /// Whether the device was put back on `device_rate_before_hz` afterwards.
    /// `false` with `device_rate_forced` also false is the ordinary case: the
    /// host never moved the device, so there was nothing to undo.
    pub device_rate_restored: bool,
    /// Why the restore could not be completed, when it could not.
    pub device_rate_restore_error: Option<String>,
    pub source_channels: u16,
    pub device_channels: u16,
    pub sample_format: String,
    pub frame_count: usize,
    pub frames_written: usize,
    pub elapsed_ms: u64,
    pub completed: bool,
    pub cancelled: bool,
    pub timed_out: bool,
    pub callback_format_error: bool,
    pub stream_error_count: u64,
    pub first_stream_error: Option<String>,
}

/// Cloneable stop signal for a blocking output playback.
///
/// Playback polls this a few times per callback period, so a cancel stops the
/// sweep within tens of milliseconds without touching the real-time callback.
#[derive(Clone, Debug, Default)]
pub struct OutputPlaybackCancellation {
    cancelled: Arc<AtomicBool>,
}

impl OutputPlaybackCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct PlaybackShared {
    frames: Vec<f32>,
    source_channels: usize,
    device_channels: usize,
    frame_count: usize,
    next_frame: AtomicU64,
    source_exhausted: AtomicBool,
    callback_format_error: AtomicBool,
    stream_error_count: AtomicU64,
    first_stream_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputStreamErrorReport {
    pub kind: String,
    pub message: String,
}

/// Cloneable stop signal for a blocking input capture.
///
/// Calling [`cancel`](Self::cancel) does not take the audio buffer lock and
/// wakes an active capture wait immediately. A token is single-use: create a
/// new token for each capture.
#[derive(Clone, Debug, Default)]
pub struct InputCaptureCancellation {
    inner: Arc<InputCaptureCancellationInner>,
}

#[derive(Debug, Default)]
struct InputCaptureCancellationInner {
    cancelled: AtomicBool,
    wait: Mutex<()>,
    wake: Condvar,
}

impl InputCaptureCancellationInner {
    /// Wake the capture wait loop without losing the notification. The waiter
    /// checks its predicates while holding `wait` and then parks on `wake`
    /// atomically; a notify issued outside that mutex can land between the
    /// check and the park and be lost, leaving cancellation or completion
    /// unnoticed until the poll interval or deadline expires (2026-07-28
    /// release review). Briefly serializing on `wait` closes that window; a
    /// poisoned lock still notifies, because the waiter will surface its own
    /// poisoning error.
    fn wake_waiters(&self) {
        let guard = self.wait.lock();
        self.wake.notify_all();
        drop(guard);
    }
}

impl InputCaptureCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.wake_waiters();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }
}

/// Captured mono samples and quality evidence from the real input stream.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonoInputCapture {
    pub host: String,
    pub device_id: String,
    pub device_name: Option<String>,
    pub sample_rate_hz: u32,
    /// The returned `samples` buffer is always mono.
    pub channels: u16,
    /// Channel count of the native device stream before extraction.
    pub source_channels: u16,
    /// Zero-based native input channel retained in `samples`.
    pub input_channel_index: u16,
    pub sample_format: String,
    pub requested_duration_ms: u64,
    pub requested_samples: usize,
    pub maximum_samples: usize,
    pub elapsed_ms: u64,
    pub samples: Vec<f32>,
    pub captured_samples: usize,
    pub capture_complete: bool,
    /// True when a caller-supplied, data-driven endpoint detector ended the
    /// stream before its maximum capture window.
    pub automatic_completion_detected: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub peak_linear: f32,
    pub peak_dbfs: Option<f32>,
    pub clipping_threshold_linear: f32,
    pub clipped_sample_count: u64,
    pub input_clipped: bool,
    pub non_finite_sample_count: u64,
    /// True only when captured PCM is known to be missing: CPAL reported an
    /// xrun, a callback could not append its frames, or the requested sample
    /// count was not received before the deadline. Backend timestamp
    /// irregularities remain separately auditable diagnostics because they do
    /// not, by themselves, prove that PCM frames were lost.
    pub sample_drop_detected: bool,
    pub xrun_count: u64,
    pub callback_lock_drop_frames: u64,
    pub timestamp_gap_frames: u64,
    pub timestamp_discontinuity_count: u64,
    pub missing_samples_at_end: usize,
    pub stream_error_detected: bool,
    pub stream_error_count: u64,
    pub stream_error_details_lost: u64,
    pub stream_errors: Vec<InputStreamErrorReport>,
    /// A raw CPAL buffer with a type different from the negotiated format is a
    /// backend contract violation and invalidates the capture.
    pub callback_format_error: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioIoError {
    message: String,
}

impl AudioIoError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for AudioIoError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AudioIoError {}

#[derive(Debug)]
struct CaptureBuffer {
    samples: Vec<f32>,
    previous_capture_timestamp: Option<StreamInstant>,
    previous_callback_frames: usize,
    timestamp_gap_frames: u64,
    timestamp_discontinuity_count: u64,
}

#[derive(Debug)]
struct CaptureShared {
    buffer: Mutex<CaptureBuffer>,
    cancellation: InputCaptureCancellation,
    target_samples: usize,
    capture_done: AtomicBool,
    captured_samples: AtomicU64,
    terminal_stream_error: AtomicBool,
    callback_format_error: AtomicBool,
    callback_lock_drop_frames: AtomicU64,
    xrun_count: AtomicU64,
    stream_error_count: AtomicU64,
    stream_error_details_lost: AtomicU64,
    stream_errors: Mutex<Vec<InputStreamErrorReport>>,
}

impl CaptureShared {
    fn new(
        target_samples: usize,
        cancellation: InputCaptureCancellation,
    ) -> Result<Self, AudioIoError> {
        let mut samples = Vec::new();
        samples.try_reserve_exact(target_samples).map_err(|error| {
            AudioIoError::new(format!(
                "could not reserve a bounded {target_samples}-sample input buffer: {error}"
            ))
        })?;

        Ok(Self {
            buffer: Mutex::new(CaptureBuffer {
                samples,
                previous_capture_timestamp: None,
                previous_callback_frames: 0,
                timestamp_gap_frames: 0,
                timestamp_discontinuity_count: 0,
            }),
            cancellation,
            target_samples,
            capture_done: AtomicBool::new(false),
            captured_samples: AtomicU64::new(0),
            terminal_stream_error: AtomicBool::new(false),
            callback_format_error: AtomicBool::new(false),
            callback_lock_drop_frames: AtomicU64::new(0),
            xrun_count: AtomicU64::new(0),
            stream_error_count: AtomicU64::new(0),
            stream_error_details_lost: AtomicU64::new(0),
            stream_errors: Mutex::new(Vec::with_capacity(MAX_RECORDED_STREAM_ERRORS)),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallbackDataError {
    FormatMismatch,
    UnsupportedFormat,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SignalMetrics {
    peak_linear: f32,
    peak_dbfs: Option<f32>,
    clipped_sample_count: u64,
    non_finite_sample_count: u64,
}

/// Open a host-qualified 48 kHz input device, extract the requested native
/// channel to mono, and block until the requested number of real microphone
/// samples is captured or the guarded deadline is reached.
///
/// No fallback device is selected. Missing/disconnected devices, microphone
/// permission failures, and the absence of a native 48 kHz PCM configuration
/// containing the selected channel are returned as explicit errors. Runtime stream errors are
/// preserved in [`MonoInputCapture`] so the caller can reject rather than
/// silently average a damaged measurement.
pub fn capture_mono_input(
    request: &MonoInputCaptureRequest,
) -> Result<MonoInputCapture, AudioIoError> {
    capture_mono_input_with_cancellation(request, &InputCaptureCancellation::new())
}

/// The cancellable form of [`capture_mono_input`].
///
/// The caller may clone `cancellation` onto a UI/control thread and call
/// [`InputCaptureCancellation::cancel`] to wake and stop this blocking capture
/// without waiting for its duration deadline.
pub fn capture_mono_input_with_cancellation(
    request: &MonoInputCaptureRequest,
    cancellation: &InputCaptureCancellation,
) -> Result<MonoInputCapture, AudioIoError> {
    capture_mono_input_internal(request, cancellation, None, None)
}

/// Capture with an optional data-driven endpoint detector.
///
/// `automatic_completion` receives a bounded, immutable tail snapshot about
/// four times per second. Its first argument is the snapshot's absolute sample
/// offset in the complete capture. Returning `true` ends the stream
/// successfully; cancellation remains a distinct rejected outcome. The
/// snapshot is copied before the detector runs so correlation or marker
/// analysis never holds the real-time callback buffer lock. Keeping this copy
/// bounded also prevents a growing whole-capture clone from starving the
/// real-time callback.
pub fn capture_mono_input_with_automatic_completion<F>(
    request: &MonoInputCaptureRequest,
    cancellation: &InputCaptureCancellation,
    maximum_snapshot_samples: usize,
    automatic_completion: &mut F,
) -> Result<MonoInputCapture, AudioIoError>
where
    F: FnMut(usize, &[f32]) -> bool,
{
    if maximum_snapshot_samples == 0 {
        return Err(AudioIoError::new(
            "automatic-completion snapshot length must be greater than zero",
        ));
    }
    capture_mono_input_internal(
        request,
        cancellation,
        Some(maximum_snapshot_samples),
        Some(automatic_completion),
    )
}

fn capture_mono_input_internal(
    request: &MonoInputCaptureRequest,
    cancellation: &InputCaptureCancellation,
    automatic_completion_snapshot_samples: Option<usize>,
    mut automatic_completion: Option<&mut AutomaticCompletionDetector<'_>>,
) -> Result<MonoInputCapture, AudioIoError> {
    let requested_samples = validate_capture_request(request)?;
    if cancellation.is_cancelled() {
        return Err(AudioIoError::new(
            "input capture was cancelled before the stream started",
        ));
    }
    let parsed_device_id = request
        .device_id
        .parse::<cpal::DeviceId>()
        .map_err(|error| {
            AudioIoError::new(format!(
                "input device ID `{}` is not a valid host-qualified CPAL ID: {error}",
                request.device_id
            ))
        })?;
    let host_id = parsed_device_id.host();
    let host = cpal::host_from_id(host_id).map_err(|error| {
        AudioIoError::new(format!(
            "could not open audio host `{}` for input device `{}`: {error}",
            host_id, request.device_id
        ))
    })?;
    let device = host.device_by_id(&parsed_device_id).ok_or_else(|| {
        AudioIoError::new(format!(
            "input device `{}` was not found on audio host `{}`; reconnect it and rescan devices",
            request.device_id, host_id
        ))
    })?;
    let selected_config = select_project_input_config(
        device.supported_input_configs().map_err(|error| {
            AudioIoError::new(format!(
                "could not query input configurations for `{}`: {error}",
                request.device_id
            ))
        })?,
        request.input_channel_index,
    )
    .ok_or_else(|| {
        AudioIoError::new(format!(
            "input device `{}` has no native 48 kHz PCM configuration containing input channel {}",
            request.device_id,
            u32::from(request.input_channel_index) + 1
        ))
    })?;
    let device_name = device
        .description()
        .ok()
        .map(|description| description.name().to_string());
    let sample_format = selected_config.sample_format();
    let sample_format_name = sample_format.to_string();
    let stream_config = selected_config.config();
    let source_channels = stream_config.channels;
    let input_channel_index = request.input_channel_index;
    let shared = Arc::new(CaptureShared::new(requested_samples, cancellation.clone())?);

    let data_shared = Arc::clone(&shared);
    let data_callback = move |data: &Data, info: &InputCallbackInfo| {
        capture_input_callback(
            data,
            info,
            sample_format,
            source_channels,
            input_channel_index,
            &data_shared,
        );
    };
    let error_shared = Arc::clone(&shared);
    let error_callback = move |error: cpal::Error| {
        capture_error_callback(error, &error_shared);
    };
    let stream = device
        .build_input_stream_raw(
            stream_config,
            sample_format,
            data_callback,
            error_callback,
            Some(CAPTURE_COMPLETION_GRACE),
        )
        .map_err(|error| {
            AudioIoError::new(format!(
                "could not build the 48 kHz {source_channels}-channel input stream for `{}`: {error}",
                request.device_id,
            ))
        })?;

    if cancellation.is_cancelled() {
        drop(stream);
        return Err(AudioIoError::new(
            "input capture was cancelled before the stream started",
        ));
    }
    let started_at = Instant::now();
    stream.play().map_err(|error| {
        AudioIoError::new(format!(
            "could not start the input stream for `{}`: {error}",
            request.device_id
        ))
    })?;

    let capture_timeout = Duration::from_millis(request.duration_ms)
        .checked_add(CAPTURE_COMPLETION_GRACE)
        .ok_or_else(|| AudioIoError::new("capture deadline overflowed"))?;
    let requested_samples_u64 = u64::try_from(requested_samples).unwrap_or(u64::MAX);
    let deadline = started_at
        .checked_add(capture_timeout)
        .ok_or_else(|| AudioIoError::new("capture deadline overflowed"))?;
    let mut automatic_completion_detected = false;
    let mut deadline_reached = false;
    let mut automatic_completion_snapshot =
        automatic_completion_snapshot_samples.map(Vec::<f32>::with_capacity);
    loop {
        // Predicates are checked while holding the wait mutex so a
        // `wake_waiters` from the audio callback or `cancel()` cannot slip
        // between the check and the park below.
        let wait_guard = shared
            .cancellation
            .inner
            .wait
            .lock()
            .map_err(|_| AudioIoError::new("input capture wait lock was poisoned"))?;
        if shared.captured_samples.load(Ordering::Acquire) >= requested_samples_u64
            || shared.terminal_stream_error.load(Ordering::Acquire)
            || shared.callback_format_error.load(Ordering::Acquire)
            || shared.cancellation.is_cancelled()
        {
            drop(wait_guard);
            break;
        }

        let now = Instant::now();
        let Some(remaining) = deadline.checked_duration_since(now) else {
            drop(wait_guard);
            deadline_reached = true;
            break;
        };
        let wait_duration = if automatic_completion.is_some() {
            remaining.min(AUTOMATIC_COMPLETION_POLL_INTERVAL)
        } else {
            remaining
        };
        let (wait_guard, wait_status) = shared
            .cancellation
            .inner
            .wake
            .wait_timeout(wait_guard, wait_duration)
            .map_err(|_| AudioIoError::new("input capture wait lock was poisoned"))?;
        drop(wait_guard);

        if let Some(detector) = automatic_completion.as_deref_mut() {
            let snapshot = automatic_completion_snapshot
                .as_mut()
                .expect("automatic completion requires a snapshot buffer");
            let snapshot_start_sample = {
                let buffer = shared
                    .buffer
                    .lock()
                    .map_err(|_| AudioIoError::new("input capture buffer lock was poisoned"))?;
                let snapshot_length = automatic_completion_snapshot_samples
                    .expect("automatic completion requires a snapshot bound");
                let snapshot_start_sample = buffer.samples.len().saturating_sub(snapshot_length);
                snapshot.clear();
                snapshot.extend_from_slice(&buffer.samples[snapshot_start_sample..]);
                snapshot_start_sample
            };
            if detector(snapshot_start_sample, snapshot) {
                automatic_completion_detected = true;
                break;
            }
        }
        if wait_status.timed_out() && Instant::now() >= deadline {
            deadline_reached = true;
            break;
        }
    }
    drop(stream);

    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut buffer = shared
        .buffer
        .lock()
        .map_err(|_| AudioIoError::new("input capture buffer lock was poisoned"))?;
    let timestamp_gap_frames = buffer.timestamp_gap_frames;
    let timestamp_discontinuity_count = buffer.timestamp_discontinuity_count;
    let samples = mem::take(&mut buffer.samples);
    drop(buffer);

    let captured_samples = samples.len();
    let missing_samples_at_end = if automatic_completion_detected {
        0
    } else {
        requested_samples.saturating_sub(captured_samples)
    };
    let xrun_count = shared.xrun_count.load(Ordering::Relaxed);
    let callback_lock_drop_frames = shared.callback_lock_drop_frames.load(Ordering::Relaxed);
    let stream_error_count = shared.stream_error_count.load(Ordering::Relaxed);
    let stream_error_details_lost = shared.stream_error_details_lost.load(Ordering::Relaxed);
    let callback_format_error = shared.callback_format_error.load(Ordering::Acquire);
    let cancelled = shared.cancellation.is_cancelled();
    let stream_errors = shared
        .stream_errors
        .lock()
        .map_err(|_| AudioIoError::new("input stream error-report lock was poisoned"))?
        .clone();
    let metrics = signal_metrics(&samples, INPUT_CLIPPING_THRESHOLD_LINEAR);
    let timed_out = deadline_reached
        && captured_samples < requested_samples
        && !automatic_completion_detected
        && !cancelled;
    let sample_drop_detected = known_pcm_loss(
        xrun_count,
        callback_lock_drop_frames,
        missing_samples_at_end,
        cancelled,
    );

    Ok(MonoInputCapture {
        host: host_id.name().to_string(),
        device_id: request.device_id.clone(),
        device_name,
        sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        channels: 1,
        source_channels,
        input_channel_index,
        sample_format: sample_format_name,
        requested_duration_ms: request.duration_ms,
        requested_samples,
        maximum_samples: request.maximum_samples,
        elapsed_ms,
        samples,
        captured_samples,
        capture_complete: (captured_samples == requested_samples || automatic_completion_detected)
            && !callback_format_error
            && !cancelled,
        automatic_completion_detected,
        timed_out,
        cancelled,
        peak_linear: metrics.peak_linear,
        peak_dbfs: metrics.peak_dbfs,
        clipping_threshold_linear: INPUT_CLIPPING_THRESHOLD_LINEAR,
        clipped_sample_count: metrics.clipped_sample_count,
        input_clipped: metrics.clipped_sample_count > 0,
        non_finite_sample_count: metrics.non_finite_sample_count,
        sample_drop_detected,
        xrun_count,
        callback_lock_drop_frames,
        timestamp_gap_frames,
        timestamp_discontinuity_count,
        missing_samples_at_end,
        stream_error_detected: stream_error_count > 0,
        stream_error_count,
        stream_error_details_lost,
        stream_errors,
        callback_format_error,
    })
}

/// Play an already-decoded interleaved buffer on one output device and block
/// until it has been handed to the driver.
///
/// This is a convenience for users who have no second player available; it is
/// deliberately *not* part of the measurement path. Nothing here observes the
/// microphone, and the sweep-recognition code never learns that this function
/// exists: a capture analyses whatever actually reached the microphone, on the
/// same code path whether the sweep came from this function, from Roon, or
/// from a phone held in front of the speaker.
///
/// Source channel `i` is written to device channel `i`; any further device
/// channels stay silent. No fallback device is selected and nothing is
/// resampled here - a device without a native PCM configuration at
/// `request.sample_rate_hz` wide enough for the buffer is an explicit error.
///
/// Pass the device's own rate ([`output_device_sample_rate`]) and the device
/// format is never rewritten, which is what keeps other clients of a shared
/// device undisturbed. Any other rate is still honoured: the device is moved
/// for the length of the sweep and put back straight afterwards - see
/// `restore_output_sample_rate` for what that costs and where it can fail.
pub fn play_interleaved_output(
    request: &InterleavedOutputPlaybackRequest,
    cancellation: &OutputPlaybackCancellation,
) -> Result<OutputPlaybackReport, AudioIoError> {
    let source_channels = validate_playback_request(request)?;
    if cancellation.is_cancelled() {
        return Err(AudioIoError::new(
            "sweep playback was cancelled before the stream started",
        ));
    }
    let (host_id, device) = resolve_output_device(&request.device_id)?;

    // Read the device's own rate before the 48 kHz stream rewrites it; asking
    // afterwards would only ever report 48 kHz back.
    let previous_config = device.default_output_config().ok();
    let previous_rate_hz = previous_config
        .as_ref()
        .map(|configuration| configuration.sample_rate());

    let played = play_sweep_on_device(&device, host_id, request, source_channels, cancellation);
    // Deliberately before the `?`: a stream that failed part-way through has
    // already moved the device, so the restore must not depend on the playback
    // having succeeded.
    let restored =
        restore_output_sample_rate(&device, previous_config.as_ref(), request.sample_rate_hz);

    let mut report = played?;
    report.device_rate_before_hz = previous_rate_hz;
    report.device_rate_forced = previous_rate_hz.is_some_and(|rate| rate != request.sample_rate_hz);
    match restored {
        Ok(restored) => report.device_rate_restored = restored,
        Err(error) => report.device_rate_restore_error = Some(error.to_string()),
    }
    Ok(report)
}

/// The rate an output device is running at right now, without opening a stream.
///
/// Callers of [`play_interleaved_output`] should author the sweep at this rate.
/// Opening a device at any other rate makes CPAL rewrite the device format,
/// which is global state on macOS: every other client of that device moves with
/// it, and on a virtual loopback shared with a convolution host the change has
/// been observed to stall CoreAudio's `AudioComponentInstanceNew` indefinitely.
/// Handing the device the rate it already has avoids the write entirely.
pub fn output_device_sample_rate(device_id: &str) -> Result<u32, AudioIoError> {
    let (_, device) = resolve_output_device(device_id)?;
    device
        .default_output_config()
        .map(|configuration| configuration.sample_rate())
        .map_err(|error| {
            AudioIoError::new(format!(
                "could not read the current sample rate of output device `{device_id}`: {error}"
            ))
        })
}

/// Look up a host-qualified CPAL output device ID without opening a stream.
fn resolve_output_device(device_id: &str) -> Result<(cpal::HostId, cpal::Device), AudioIoError> {
    let parsed_device_id = device_id.parse::<cpal::DeviceId>().map_err(|error| {
        AudioIoError::new(format!(
            "output device ID `{device_id}` is not a valid host-qualified CPAL ID: {error}"
        ))
    })?;
    let host_id = parsed_device_id.host();
    let host = cpal::host_from_id(host_id).map_err(|error| {
        AudioIoError::new(format!(
            "could not open audio host `{host_id}` for output device `{device_id}`: {error}"
        ))
    })?;
    let device = host.device_by_id(&parsed_device_id).ok_or_else(|| {
        AudioIoError::new(format!(
            "output device `{device_id}` was not found on audio host `{host_id}`; reconnect it and rescan devices"
        ))
    })?;
    Ok((host_id, device))
}

/// Put an output device back on the rate it was using before the sweep.
///
/// Forcing 48 kHz is intentional: the sweep has to leave the machine as it was
/// authored, so recognition sees the same signal whether it was played here or
/// by an external player. On macOS that force is *global* state - CPAL's
/// CoreAudio output path writes `kAudioDevicePropertyStreamFormat`, falling
/// back to `kAudioDevicePropertyNominalSampleRate`, on the device object, so
/// every other client of that device moves with it. The case that hurts is a
/// virtual loopback device shared with a convolution host: the host sees an
/// external format change and stops. Restoring right away keeps that
/// disturbance to the length of the sweep instead of leaving the machine on a
/// rate the user never chose.
///
/// Windows arrives here with nothing to do. CPAL's WASAPI output path runs the
/// shared-mode engine with `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`, which converts
/// into the device's existing mix format rather than rewriting it, so the rate
/// reads back unchanged and this returns `Ok(false)`.
///
/// The restore is a stream build that is dropped without ever being played:
/// CPAL applies the device format before it touches the audio unit, so the rate
/// is back without a sample being rendered.
fn restore_output_sample_rate(
    device: &cpal::Device,
    previous: Option<&SupportedStreamConfig>,
    played_rate_hz: u32,
) -> Result<bool, AudioIoError> {
    let Some(previous) = previous else {
        return Ok(false);
    };
    if previous.sample_rate() == played_rate_hz {
        // The device was handed the rate it already had, so nothing moved.
        // This is the path every caller should be on.
        return Ok(false);
    }
    // Ask the host rather than assuming: only a host that actually moved the
    // device has anything to undo.
    let current_rate_hz = device
        .default_output_config()
        .map(|configuration| configuration.sample_rate())
        .map_err(|error| {
            AudioIoError::new(format!(
                "could not read the output device rate back after the sweep: {error}"
            ))
        })?;
    if current_rate_hz == previous.sample_rate() {
        return Ok(false);
    }

    // `err()` drops the built stream immediately; it is never played.
    let build_error = device
        .build_output_stream_raw(
            previous.config(),
            previous.sample_format(),
            |_: &mut Data, _: &cpal::OutputCallbackInfo| {},
            |_: cpal::Error| {},
            Some(PLAYBACK_COMPLETION_GRACE),
        )
        .err();
    if wait_for_output_rate(device, previous.sample_rate()) {
        return Ok(true);
    }
    Err(AudioIoError::new(match build_error {
        Some(error) => format!(
            "could not restore the output device to {} Hz after the sweep: {error}",
            previous.sample_rate()
        ),
        None => format!(
            "the output device did not return to {} Hz after the sweep",
            previous.sample_rate()
        ),
    }))
}

/// Wait for a device to report the rate it was just asked for.
///
/// CoreAudio publishes a device rate change asynchronously: the property write
/// returns before the new value is readable, so a single read straight after
/// the restore can still show the borrowed rate and report a false failure.
/// The pre-restore check needs none of this - by then the playback stream has
/// been open for the whole sweep and the value settled long ago.
fn wait_for_output_rate(device: &cpal::Device, wanted_hz: u32) -> bool {
    let deadline = Instant::now() + OUTPUT_RATE_SETTLE;
    loop {
        if device
            .default_output_config()
            .is_ok_and(|configuration| configuration.sample_rate() == wanted_hz)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(PLAYBACK_POLL_INTERVAL);
    }
}

fn play_sweep_on_device(
    device: &cpal::Device,
    host_id: cpal::HostId,
    request: &InterleavedOutputPlaybackRequest,
    source_channels: usize,
    cancellation: &OutputPlaybackCancellation,
) -> Result<OutputPlaybackReport, AudioIoError> {
    let selected_config = select_output_config(
        device.supported_output_configs().map_err(|error| {
            AudioIoError::new(format!(
                "could not query output configurations for `{}`: {error}",
                request.device_id
            ))
        })?,
        request.channels,
        request.sample_rate_hz,
    )
    .ok_or_else(|| {
        AudioIoError::new(format!(
            "output device `{}` has no native {} Hz PCM configuration with at least {} channels",
            request.device_id, request.sample_rate_hz, request.channels
        ))
    })?;
    let device_name = device
        .description()
        .ok()
        .map(|description| description.name().to_string());
    let sample_format = selected_config.sample_format();
    let sample_format_name = sample_format.to_string();
    let stream_config = selected_config.config();
    let device_channels = stream_config.channels;
    let frame_count = request.frames.len() / source_channels;

    let shared = Arc::new(PlaybackShared {
        frames: request.frames.clone(),
        source_channels,
        device_channels: usize::from(device_channels),
        frame_count,
        next_frame: AtomicU64::new(0),
        source_exhausted: AtomicBool::new(false),
        callback_format_error: AtomicBool::new(false),
        stream_error_count: AtomicU64::new(0),
        first_stream_error: Mutex::new(None),
    });

    let data_shared = Arc::clone(&shared);
    let data_callback = move |data: &mut Data, _: &cpal::OutputCallbackInfo| {
        write_playback_callback(data, sample_format, &data_shared);
    };
    let error_shared = Arc::clone(&shared);
    let error_callback = move |error: cpal::Error| {
        error_shared
            .stream_error_count
            .fetch_add(1, Ordering::Relaxed);
        if let Ok(mut first) = error_shared.first_stream_error.lock() {
            if first.is_none() {
                *first = Some(error.to_string());
            }
        }
    };
    let stream = device
        .build_output_stream_raw(
            stream_config,
            sample_format,
            data_callback,
            error_callback,
            Some(PLAYBACK_COMPLETION_GRACE),
        )
        .map_err(|error| {
            AudioIoError::new(format!(
                "could not build the {} Hz {device_channels}-channel output stream for `{}`: {error}",
                request.sample_rate_hz, request.device_id,
            ))
        })?;

    let started_at = Instant::now();
    stream.play().map_err(|error| {
        AudioIoError::new(format!(
            "could not start the output stream for `{}`: {error}",
            request.device_id
        ))
    })?;

    // The source duration plus a generous grace; a device that stops calling
    // back must not hang the command thread.
    let deadline = started_at
        .checked_add(
            Duration::from_millis(playback_duration_ms(frame_count, request.sample_rate_hz))
                .checked_add(PLAYBACK_DEADLINE_GRACE)
                .ok_or_else(|| AudioIoError::new("playback deadline overflowed"))?,
        )
        .ok_or_else(|| AudioIoError::new("playback deadline overflowed"))?;
    let mut deadline_reached = false;
    loop {
        if cancellation.is_cancelled() {
            break;
        }
        if shared.source_exhausted.load(Ordering::Acquire)
            || shared.callback_format_error.load(Ordering::Acquire)
        {
            // The last frames are written but not yet converted by the
            // hardware. Let the driver drain them before the stream is
            // dropped, or the sweep's tail (which carries the end marker)
            // would be cut off.
            std::thread::sleep(PLAYBACK_DRAIN);
            break;
        }
        if Instant::now() >= deadline {
            deadline_reached = true;
            break;
        }
        std::thread::sleep(PLAYBACK_POLL_INTERVAL);
    }
    drop(stream);

    let cancelled = cancellation.is_cancelled();
    let frames_written = usize::try_from(shared.next_frame.load(Ordering::Acquire))
        .unwrap_or(usize::MAX)
        .min(frame_count);
    let callback_format_error = shared.callback_format_error.load(Ordering::Acquire);
    let first_stream_error = shared
        .first_stream_error
        .lock()
        .map_err(|_| AudioIoError::new("playback stream error-report lock was poisoned"))?
        .clone();

    Ok(OutputPlaybackReport {
        host: host_id.name().to_string(),
        device_id: request.device_id.clone(),
        device_name,
        sample_rate_hz: request.sample_rate_hz,
        // Owned by `play_interleaved_output`, which is the only caller that
        // knows what the device was doing before this stream opened.
        device_rate_before_hz: None,
        device_rate_forced: false,
        device_rate_restored: false,
        device_rate_restore_error: None,
        source_channels: request.channels,
        device_channels,
        sample_format: sample_format_name,
        frame_count,
        frames_written,
        elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        completed: frames_written == frame_count && !cancelled && !callback_format_error,
        cancelled,
        timed_out: deadline_reached && !cancelled,
        callback_format_error,
        stream_error_count: shared.stream_error_count.load(Ordering::Relaxed),
        first_stream_error,
    })
}

fn validate_playback_request(
    request: &InterleavedOutputPlaybackRequest,
) -> Result<usize, AudioIoError> {
    if request.device_id.trim().is_empty() {
        return Err(AudioIoError::new(
            "sweep playback requires a non-empty host-qualified output device ID",
        ));
    }
    let channels = usize::from(request.channels);
    if channels == 0 || channels > MAXIMUM_PLAYBACK_CHANNELS {
        return Err(AudioIoError::new(format!(
            "sweep playback supports 1 to {MAXIMUM_PLAYBACK_CHANNELS} channels, not {}",
            request.channels
        )));
    }
    if !(MINIMUM_PLAYBACK_RATE_HZ..=MAXIMUM_PLAYBACK_RATE_HZ).contains(&request.sample_rate_hz) {
        return Err(AudioIoError::new(format!(
            "sweep playback supports {MINIMUM_PLAYBACK_RATE_HZ} to {MAXIMUM_PLAYBACK_RATE_HZ} Hz, not {}",
            request.sample_rate_hz
        )));
    }
    if request.frames.is_empty() || request.frames.len() % channels != 0 {
        return Err(AudioIoError::new(
            "sweep playback buffer must be a non-empty whole number of interleaved frames",
        ));
    }
    if request.frames.len() > MAXIMUM_PLAYBACK_SAMPLES {
        return Err(AudioIoError::new(format!(
            "sweep playback buffer of {} samples exceeds the {MAXIMUM_PLAYBACK_SAMPLES}-sample safety limit",
            request.frames.len()
        )));
    }
    if let Some(index) = request.frames.iter().position(|sample| !sample.is_finite()) {
        return Err(AudioIoError::new(format!(
            "sweep playback buffer sample {index} is not a finite value"
        )));
    }
    Ok(channels)
}

fn playback_duration_ms(frame_count: usize, sample_rate_hz: u32) -> u64 {
    let numerator = (frame_count as u128)
        .saturating_mul(1_000)
        .saturating_add(u128::from(sample_rate_hz) - 1);
    u64::try_from(numerator / u128::from(sample_rate_hz)).unwrap_or(u64::MAX)
}

fn select_output_config(
    configurations: impl Iterator<Item = SupportedStreamConfigRange>,
    minimum_channels: u16,
    sample_rate_hz: u32,
) -> Option<SupportedStreamConfig> {
    configurations
        .filter(|configuration| {
            configuration.channels() >= minimum_channels
                && configuration.contains_rate(sample_rate_hz)
                && is_supported_pcm_format(configuration.sample_format())
        })
        .filter_map(|configuration| {
            let priority = (
                configuration.channels(),
                sample_format_priority(configuration.sample_format()),
            );
            configuration
                .try_with_sample_rate(sample_rate_hz)
                .map(|selected| (priority, selected))
        })
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, configuration)| configuration)
}

fn write_playback_callback(
    data: &mut Data,
    negotiated_format: SampleFormat,
    shared: &PlaybackShared,
) {
    if data.sample_format() != negotiated_format {
        shared.callback_format_error.store(true, Ordering::Release);
        return;
    }

    macro_rules! write_format {
        ($sample_type:ty) => {
            match data.as_slice_mut::<$sample_type>() {
                Some(samples) => write_playback_samples(samples, shared),
                None => shared.callback_format_error.store(true, Ordering::Release),
            }
        };
    }

    match negotiated_format {
        SampleFormat::I8 => write_format!(i8),
        SampleFormat::I16 => write_format!(i16),
        SampleFormat::I24 => write_format!(cpal::I24),
        SampleFormat::I32 => write_format!(i32),
        SampleFormat::I64 => write_format!(i64),
        SampleFormat::U8 => write_format!(u8),
        SampleFormat::U16 => write_format!(u16),
        SampleFormat::U24 => write_format!(cpal::U24),
        SampleFormat::U32 => write_format!(u32),
        SampleFormat::U64 => write_format!(u64),
        SampleFormat::F32 => write_format!(f32),
        SampleFormat::F64 => write_format!(f64),
        _ => shared.callback_format_error.store(true, Ordering::Release),
    }
}

fn write_playback_samples<T>(output: &mut [T], shared: &PlaybackShared)
where
    T: Sample + FromSample<f32>,
{
    let device_channels = shared.device_channels;
    if device_channels == 0 || output.len() % device_channels != 0 {
        shared.callback_format_error.store(true, Ordering::Release);
        return;
    }
    let silence = T::from_sample(0.0_f32);
    let copied_channels = shared.source_channels.min(device_channels);
    let mut next_frame = usize::try_from(shared.next_frame.load(Ordering::Acquire))
        .unwrap_or(usize::MAX)
        .min(shared.frame_count);

    for device_frame in output.chunks_exact_mut(device_channels) {
        if next_frame >= shared.frame_count {
            device_frame.fill(silence);
            continue;
        }
        let base = next_frame * shared.source_channels;
        for (channel, slot) in device_frame.iter_mut().enumerate() {
            *slot = if channel < copied_channels {
                // A malformed buffer must not be handed to an amplifier at
                // more than full scale; the adapter rejects non-finite
                // samples, and this bounds everything else.
                T::from_sample(shared.frames[base + channel].clamp(-1.0, 1.0))
            } else {
                silence
            };
        }
        next_frame += 1;
    }

    shared
        .next_frame
        .store(next_frame as u64, Ordering::Release);
    if next_frame >= shared.frame_count {
        shared.source_exhausted.store(true, Ordering::Release);
    }
}

fn validate_capture_request(request: &MonoInputCaptureRequest) -> Result<usize, AudioIoError> {
    if request.device_id.trim().is_empty() {
        return Err(AudioIoError::new(
            "input capture requires a non-empty host-qualified device ID",
        ));
    }
    if request.duration_ms == 0 {
        return Err(AudioIoError::new(
            "input capture duration must be greater than zero milliseconds",
        ));
    }

    let requested_samples = samples_for_duration_ms(request.duration_ms).ok_or_else(|| {
        AudioIoError::new("input capture duration is too large to represent at 48 kHz")
    })?;
    if request.maximum_samples < requested_samples {
        return Err(AudioIoError::new(format!(
            "maximumSamples {} is smaller than the {requested_samples} samples required for {} ms at 48 kHz",
            request.maximum_samples, request.duration_ms
        )));
    }

    Ok(requested_samples)
}

fn samples_for_duration_ms(duration_ms: u64) -> Option<usize> {
    let numerator = u128::from(duration_ms)
        .checked_mul(u128::from(PROJECT_SAMPLE_RATE_HZ))?
        .checked_add(999)?;
    usize::try_from(numerator / 1_000).ok()
}

fn select_project_input_config(
    configurations: impl Iterator<Item = SupportedStreamConfigRange>,
    input_channel_index: u16,
) -> Option<SupportedStreamConfig> {
    configurations
        .filter(|configuration| {
            configuration.channels() > input_channel_index
                && configuration.contains_rate(PROJECT_SAMPLE_RATE_HZ)
                && is_supported_pcm_format(configuration.sample_format())
        })
        .filter_map(|configuration| {
            let priority = (
                configuration.channels(),
                sample_format_priority(configuration.sample_format()),
            );
            configuration
                .try_with_sample_rate(PROJECT_SAMPLE_RATE_HZ)
                .map(|selected| (priority, selected))
        })
        .min_by_key(|(priority, _)| *priority)
        .map(|(_, configuration)| configuration)
}

fn is_supported_pcm_format(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I24
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U24
            | SampleFormat::U32
            | SampleFormat::U64
            | SampleFormat::F32
            | SampleFormat::F64
    )
}

fn sample_format_priority(format: SampleFormat) -> u8 {
    match format {
        SampleFormat::F32 => 0,
        SampleFormat::F64 => 1,
        SampleFormat::I32 => 2,
        SampleFormat::I24 => 3,
        SampleFormat::I16 => 4,
        SampleFormat::I8 => 5,
        SampleFormat::U32 => 6,
        SampleFormat::U24 => 7,
        SampleFormat::U16 => 8,
        SampleFormat::U8 => 9,
        SampleFormat::I64 => 10,
        SampleFormat::U64 => 11,
        _ => u8::MAX,
    }
}

fn capture_input_callback(
    data: &Data,
    info: &InputCallbackInfo,
    negotiated_format: SampleFormat,
    source_channels: u16,
    input_channel_index: u16,
    shared: &CaptureShared,
) {
    if shared.capture_done.load(Ordering::Acquire)
        || shared.callback_format_error.load(Ordering::Acquire)
    {
        return;
    }

    let source_channels_usize = usize::from(source_channels);
    if source_channels_usize == 0
        || input_channel_index >= source_channels
        || data.len() % source_channels_usize != 0
    {
        shared.callback_format_error.store(true, Ordering::Release);
        shared.cancellation.inner.wake_waiters();
        return;
    }
    let callback_frames = data.len() / source_channels_usize;
    // The monitor copies at most a bounded tail and never performs DSP while
    // holding this lock. Waiting for that short memcpy is safer than the old
    // try-lock path, which silently discarded a complete microphone callback
    // whenever its 250 ms monitor poll happened to overlap CoreAudio.
    let mut buffer = match shared.buffer.lock() {
        Ok(buffer) => buffer,
        Err(_) => {
            shared.callback_format_error.store(true, Ordering::Release);
            shared.cancellation.inner.wake_waiters();
            return;
        }
    };

    update_capture_timestamp(&mut buffer, info.timestamp().capture, callback_frames);
    let append_result = append_selected_channel_samples(
        data,
        negotiated_format,
        source_channels,
        input_channel_index,
        &mut buffer.samples,
        shared.target_samples,
    );
    if append_result.is_err() {
        shared.callback_format_error.store(true, Ordering::Release);
        shared.cancellation.inner.wake_waiters();
        return;
    }

    shared.captured_samples.store(
        u64::try_from(buffer.samples.len()).unwrap_or(u64::MAX),
        Ordering::Release,
    );
    if buffer.samples.len() >= shared.target_samples {
        shared.capture_done.store(true, Ordering::Release);
        shared.cancellation.inner.wake_waiters();
    }
}

fn capture_error_callback(error: cpal::Error, shared: &CaptureShared) {
    let kind = error.kind();
    shared.stream_error_count.fetch_add(1, Ordering::Relaxed);
    if kind == ErrorKind::Xrun {
        shared.xrun_count.fetch_add(1, Ordering::Relaxed);
    }
    if is_terminal_stream_error(kind) {
        shared.terminal_stream_error.store(true, Ordering::Release);
    }

    match shared.stream_errors.try_lock() {
        Ok(mut reports) if reports.len() < MAX_RECORDED_STREAM_ERRORS => {
            reports.push(InputStreamErrorReport {
                kind: format!("{kind:?}"),
                message: error.to_string(),
            });
        }
        Ok(_) | Err(_) => {
            shared
                .stream_error_details_lost
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    if is_terminal_stream_error(kind) {
        shared.cancellation.inner.wake_waiters();
    }
}

fn is_terminal_stream_error(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied | ErrorKind::Xrun
    )
}

fn update_capture_timestamp(
    buffer: &mut CaptureBuffer,
    capture_timestamp: StreamInstant,
    callback_frames: usize,
) {
    if let Some(previous_timestamp) = buffer.previous_capture_timestamp {
        match estimate_timestamp_gap_frames(
            previous_timestamp,
            capture_timestamp,
            buffer.previous_callback_frames,
        ) {
            Some(gap_frames) => {
                buffer.timestamp_gap_frames =
                    buffer.timestamp_gap_frames.saturating_add(gap_frames);
            }
            None => {
                buffer.timestamp_discontinuity_count =
                    buffer.timestamp_discontinuity_count.saturating_add(1);
            }
        }
    }
    buffer.previous_capture_timestamp = Some(capture_timestamp);
    buffer.previous_callback_frames = callback_frames;
}

fn estimate_timestamp_gap_frames(
    previous: StreamInstant,
    current: StreamInstant,
    previous_callback_frames: usize,
) -> Option<u64> {
    let elapsed = current.checked_duration_since(previous)?;
    let elapsed_frames = duration_to_nearest_frames(elapsed);
    let expected_frames = u64::try_from(previous_callback_frames).unwrap_or(u64::MAX);
    Some(
        elapsed_frames
            .saturating_sub(expected_frames)
            .saturating_sub(TIMESTAMP_GAP_TOLERANCE_FRAMES),
    )
}

fn duration_to_nearest_frames(duration: Duration) -> u64 {
    let numerator = duration
        .as_nanos()
        .saturating_mul(u128::from(PROJECT_SAMPLE_RATE_HZ))
        .saturating_add(500_000_000);
    u64::try_from(numerator / 1_000_000_000).unwrap_or(u64::MAX)
}

fn known_pcm_loss(
    xrun_count: u64,
    callback_lock_drop_frames: u64,
    missing_samples_at_end: usize,
    cancelled: bool,
) -> bool {
    xrun_count > 0 || callback_lock_drop_frames > 0 || (missing_samples_at_end > 0 && !cancelled)
}

fn append_selected_channel_samples(
    data: &Data,
    negotiated_format: SampleFormat,
    source_channels: u16,
    input_channel_index: u16,
    output: &mut Vec<f32>,
    maximum_samples: usize,
) -> Result<(), CallbackDataError> {
    if data.sample_format() != negotiated_format {
        return Err(CallbackDataError::FormatMismatch);
    }

    macro_rules! append_format {
        ($sample_type:ty) => {
            data.as_slice::<$sample_type>()
                .ok_or(CallbackDataError::FormatMismatch)
                .and_then(|samples| {
                    append_normalized_channel_samples(
                        samples,
                        source_channels,
                        input_channel_index,
                        output,
                        maximum_samples,
                    )
                })
        };
    }

    match negotiated_format {
        SampleFormat::I8 => append_format!(i8),
        SampleFormat::I16 => append_format!(i16),
        SampleFormat::I24 => append_format!(cpal::I24),
        SampleFormat::I32 => append_format!(i32),
        SampleFormat::I64 => append_format!(i64),
        SampleFormat::U8 => append_format!(u8),
        SampleFormat::U16 => append_format!(u16),
        SampleFormat::U24 => append_format!(cpal::U24),
        SampleFormat::U32 => append_format!(u32),
        SampleFormat::U64 => append_format!(u64),
        SampleFormat::F32 => append_format!(f32),
        SampleFormat::F64 => append_format!(f64),
        _ => Err(CallbackDataError::UnsupportedFormat),
    }
}

fn append_normalized_channel_samples<T>(
    input: &[T],
    source_channels: u16,
    input_channel_index: u16,
    output: &mut Vec<f32>,
    maximum_samples: usize,
) -> Result<(), CallbackDataError>
where
    T: Copy,
    f32: FromSample<T>,
{
    let source_channels = usize::from(source_channels);
    let input_channel_index = usize::from(input_channel_index);
    if source_channels == 0
        || input_channel_index >= source_channels
        || input.len() % source_channels != 0
    {
        return Err(CallbackDataError::FormatMismatch);
    }
    let remaining = maximum_samples.saturating_sub(output.len());
    output.extend(
        input
            .chunks_exact(source_channels)
            .take(remaining)
            .map(|frame| f32::from_sample(frame[input_channel_index])),
    );
    Ok(())
}

fn signal_metrics(samples: &[f32], clipping_threshold: f32) -> SignalMetrics {
    let mut peak_linear = 0.0_f32;
    let mut clipped_sample_count = 0_u64;
    let mut non_finite_sample_count = 0_u64;

    for &sample in samples {
        if !sample.is_finite() {
            non_finite_sample_count = non_finite_sample_count.saturating_add(1);
            continue;
        }
        let absolute = sample.abs();
        peak_linear = peak_linear.max(absolute);
        if absolute >= clipping_threshold {
            clipped_sample_count = clipped_sample_count.saturating_add(1);
        }
    }

    SignalMetrics {
        peak_linear,
        peak_dbfs: (peak_linear > 0.0).then(|| 20.0 * peak_linear.log10()),
        clipped_sample_count,
        non_finite_sample_count,
    }
}

/// Enumerate only input devices on the platform default host.
///
/// The Roon/manual-playback workflow never needs a computer output device.
/// This function therefore does not query the default output, enumerate output
/// devices, inspect output configurations, or open an output stream.
pub fn scan_default_host_inputs() -> Result<AudioDeviceInventory, AudioIoError> {
    let host = cpal::default_host();
    let host_name = host.id().name().to_string();
    let default_input_id = host
        .default_input_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let devices = host.input_devices().map_err(|error| {
        AudioIoError::new(format!(
            "could not enumerate {host_name} input devices: {error}"
        ))
    })?;

    let mut inputs = Vec::new();
    let mut warnings = Vec::new();

    for device in devices {
        let id = match device.id() {
            Ok(id) => id.to_string(),
            Err(error) => {
                warnings.push(AudioScanWarning {
                    device_id: None,
                    direction: Some(AudioDirection::Input),
                    message: format!("ignored an input device without a usable host ID: {error}"),
                });
                continue;
            }
        };

        let metadata = match device.description() {
            Ok(description) => DeviceMetadata {
                name: description.name().to_string(),
                manufacturer: description.manufacturer().map(str::to_string),
                device_type: description.device_type().to_string(),
                interface_type: description.interface_type().to_string(),
            },
            Err(error) => {
                warnings.push(AudioScanWarning {
                    device_id: Some(id.clone()),
                    direction: Some(AudioDirection::Input),
                    message: format!("input device metadata was unavailable: {error}"),
                });
                DeviceMetadata {
                    name: "Audio input (metadata unavailable)".to_string(),
                    manufacturer: None,
                    device_type: "unknown".to_string(),
                    interface_type: "unknown".to_string(),
                }
            }
        };

        match device.supported_input_configs() {
            Ok(configurations) => {
                if let Some(configurations) = direction_project_configs(configurations) {
                    inputs.push(descriptor(
                        &id,
                        &metadata,
                        default_input_id.as_deref() == Some(id.as_str()),
                        current_input_rate_hz(&device),
                        configurations,
                    ));
                }
            }
            Err(error) if error.kind() == cpal::ErrorKind::UnsupportedOperation => {}
            Err(error) => warnings.push(AudioScanWarning {
                device_id: Some(id),
                direction: Some(AudioDirection::Input),
                message: format!("input configurations were unavailable: {error}"),
            }),
        }
    }

    sort_devices(&mut inputs);

    Ok(AudioDeviceInventory {
        host: host_name,
        sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        inputs,
        outputs: Vec::new(),
        warnings,
    })
}

/// Enumerate only output devices on the platform default host.
///
/// This is the mirror of [`scan_default_host_inputs`] and exists for the
/// optional in-app sweep playback: the measurement path itself never needs an
/// output device, so a user who plays the sweep from Roon or another player
/// never calls this and no output device is ever queried or opened.
pub fn scan_default_host_outputs() -> Result<AudioDeviceInventory, AudioIoError> {
    let host = cpal::default_host();
    let host_name = host.id().name().to_string();
    let default_output_id = host
        .default_output_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let devices = host.output_devices().map_err(|error| {
        AudioIoError::new(format!(
            "could not enumerate {host_name} output devices: {error}"
        ))
    })?;

    let mut outputs = Vec::new();
    let mut warnings = Vec::new();

    for device in devices {
        let id = match device.id() {
            Ok(id) => id.to_string(),
            Err(error) => {
                warnings.push(AudioScanWarning {
                    device_id: None,
                    direction: Some(AudioDirection::Output),
                    message: format!("ignored an output device without a usable host ID: {error}"),
                });
                continue;
            }
        };

        let metadata = match device.description() {
            Ok(description) => DeviceMetadata {
                name: description.name().to_string(),
                manufacturer: description.manufacturer().map(str::to_string),
                device_type: description.device_type().to_string(),
                interface_type: description.interface_type().to_string(),
            },
            Err(error) => {
                warnings.push(AudioScanWarning {
                    device_id: Some(id.clone()),
                    direction: Some(AudioDirection::Output),
                    message: format!("output device metadata was unavailable: {error}"),
                });
                DeviceMetadata {
                    name: "Audio output (metadata unavailable)".to_string(),
                    manufacturer: None,
                    device_type: "unknown".to_string(),
                    interface_type: "unknown".to_string(),
                }
            }
        };

        match device.supported_output_configs() {
            Ok(configurations) => {
                if let Some(configurations) = direction_project_configs(configurations) {
                    outputs.push(descriptor(
                        &id,
                        &metadata,
                        default_output_id.as_deref() == Some(id.as_str()),
                        current_output_rate_hz(&device),
                        configurations,
                    ));
                }
            }
            Err(error) if error.kind() == cpal::ErrorKind::UnsupportedOperation => {}
            Err(error) => warnings.push(AudioScanWarning {
                device_id: Some(id),
                direction: Some(AudioDirection::Output),
                message: format!("output configurations were unavailable: {error}"),
            }),
        }
    }

    sort_devices(&mut outputs);

    Ok(AudioDeviceInventory {
        host: host_name,
        sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        inputs: Vec::new(),
        outputs,
        warnings,
    })
}

/// Enumerate the platform default host without opening any audio stream.
///
/// This legacy full-duplex inventory remains available to library callers.
/// The desktop measurement wizard uses [`scan_default_host_inputs`] instead.
pub fn scan_default_host() -> Result<AudioDeviceInventory, AudioIoError> {
    let host = cpal::default_host();
    let host_name = host.id().name().to_string();
    let default_input_id = host
        .default_input_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let default_output_id = host
        .default_output_device()
        .and_then(|device| device.id().ok())
        .map(|id| id.to_string());
    let devices = host.devices().map_err(|error| {
        AudioIoError::new(format!(
            "could not enumerate {host_name} audio devices: {error}"
        ))
    })?;

    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut warnings = Vec::new();

    for device in devices {
        let id = match device.id() {
            Ok(id) => id.to_string(),
            Err(error) => {
                warnings.push(AudioScanWarning {
                    device_id: None,
                    direction: None,
                    message: format!("ignored an audio device without a usable host ID: {error}"),
                });
                continue;
            }
        };

        let metadata = match device.description() {
            Ok(description) => DeviceMetadata {
                name: description.name().to_string(),
                manufacturer: description.manufacturer().map(str::to_string),
                device_type: description.device_type().to_string(),
                interface_type: description.interface_type().to_string(),
            },
            Err(error) => {
                warnings.push(AudioScanWarning {
                    device_id: Some(id.clone()),
                    direction: None,
                    message: format!("device metadata was unavailable: {error}"),
                });
                DeviceMetadata {
                    // `Device`'s Display implementation asks for the same
                    // description again and can panic through `ToString` when
                    // that lookup fails. Keep this fallback infallible.
                    name: "Audio device (metadata unavailable)".to_string(),
                    manufacturer: None,
                    device_type: "unknown".to_string(),
                    interface_type: "unknown".to_string(),
                }
            }
        };

        match device.supported_input_configs() {
            Ok(configurations) => {
                if let Some(configurations) = direction_project_configs(configurations) {
                    inputs.push(descriptor(
                        &id,
                        &metadata,
                        default_input_id.as_deref() == Some(id.as_str()),
                        current_input_rate_hz(&device),
                        configurations,
                    ));
                }
            }
            Err(error) if error.kind() == cpal::ErrorKind::UnsupportedOperation => {}
            Err(error) => warnings.push(AudioScanWarning {
                device_id: Some(id.clone()),
                direction: Some(AudioDirection::Input),
                message: format!("input configurations were unavailable: {error}"),
            }),
        }

        match device.supported_output_configs() {
            Ok(configurations) => {
                if let Some(configurations) = direction_project_configs(configurations) {
                    outputs.push(descriptor(
                        &id,
                        &metadata,
                        default_output_id.as_deref() == Some(id.as_str()),
                        current_output_rate_hz(&device),
                        configurations,
                    ));
                }
            }
            Err(error) if error.kind() == cpal::ErrorKind::UnsupportedOperation => {}
            Err(error) => warnings.push(AudioScanWarning {
                device_id: Some(id.clone()),
                direction: Some(AudioDirection::Output),
                message: format!("output configurations were unavailable: {error}"),
            }),
        }
    }

    sort_devices(&mut inputs);
    sort_devices(&mut outputs);

    Ok(AudioDeviceInventory {
        host: host_name,
        sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        inputs,
        outputs,
        warnings,
    })
}

#[derive(Clone, Debug)]
struct DeviceMetadata {
    name: String,
    manufacturer: Option<String>,
    device_type: String,
    interface_type: String,
}

fn descriptor(
    id: &str,
    metadata: &DeviceMetadata,
    is_default: bool,
    current_sample_rate_hz: Option<u32>,
    configurations: Vec<ProjectStreamConfig>,
) -> AudioDeviceDescriptor {
    AudioDeviceDescriptor {
        id: id.to_string(),
        name: metadata.name.clone(),
        manufacturer: metadata.manufacturer.clone(),
        device_type: metadata.device_type.clone(),
        interface_type: metadata.interface_type.clone(),
        is_default,
        current_sample_rate_hz,
        configurations,
    }
}

/// The rate a device is running at right now, or `None` if the host will not
/// say.
///
/// CPAL reads this from the host's own view of the device - CoreAudio's current
/// stream format, WASAPI's shared mix format - so it is the value the user sees
/// in Audio MIDI Setup or the Windows sound control panel, not a project
/// constant. Scanning reports it so the wizard can warn before in-app playback
/// moves a shared device; see [`play_interleaved_output`].
fn current_input_rate_hz(device: &cpal::Device) -> Option<u32> {
    device
        .default_input_config()
        .ok()
        .map(|configuration| configuration.sample_rate())
}

fn current_output_rate_hz(device: &cpal::Device) -> Option<u32> {
    device
        .default_output_config()
        .ok()
        .map(|configuration| configuration.sample_rate())
}

fn sort_devices(devices: &mut [AudioDeviceDescriptor]) {
    devices.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn project_rate_configs(
    configurations: impl Iterator<Item = SupportedStreamConfigRange>,
) -> Vec<ProjectStreamConfig> {
    let mut result = configurations
        .filter(|configuration| configuration.contains_rate(PROJECT_SAMPLE_RATE_HZ))
        .map(|configuration| {
            let (minimum_buffer_size_frames, maximum_buffer_size_frames) =
                match configuration.buffer_size() {
                    SupportedBufferSize::Range { min, max } => (Some(*min), Some(*max)),
                    SupportedBufferSize::Unknown => (None, None),
                };
            ProjectStreamConfig {
                sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
                channels: configuration.channels(),
                sample_format: configuration.sample_format().to_string(),
                minimum_buffer_size_frames,
                maximum_buffer_size_frames,
            }
        })
        .collect::<Vec<_>>();
    result.sort();
    result.dedup();
    result
}

/// CPAL backends may report an empty iterator for the opposite direction. A
/// non-empty iterator identifies the direction even when none of its ranges
/// contains the project's required 48 kHz rate.
fn direction_project_configs(
    configurations: impl Iterator<Item = SupportedStreamConfigRange>,
) -> Option<Vec<ProjectStreamConfig>> {
    let reported = configurations.collect::<Vec<_>>();
    if reported.is_empty() {
        None
    } else {
        Some(project_rate_configs(reported.into_iter()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::{SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

    #[test]
    fn keeps_only_48k_configs_and_deduplicates_them() {
        let configurations = vec![
            SupportedStreamConfigRange::new(
                2,
                44_100,
                96_000,
                SupportedBufferSize::Range { min: 64, max: 512 },
                SampleFormat::F32,
            ),
            SupportedStreamConfigRange::new(
                2,
                44_100,
                96_000,
                SupportedBufferSize::Range { min: 64, max: 512 },
                SampleFormat::F32,
            ),
            SupportedStreamConfigRange::new(
                1,
                44_100,
                44_100,
                SupportedBufferSize::Unknown,
                SampleFormat::I16,
            ),
        ];

        let selected = project_rate_configs(configurations.into_iter());

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].sample_rate_hz, 48_000);
        assert_eq!(selected[0].channels, 2);
        assert_eq!(selected[0].sample_format, "f32");
        assert_eq!(selected[0].minimum_buffer_size_frames, Some(64));
        assert_eq!(selected[0].maximum_buffer_size_frames, Some(512));
    }

    #[test]
    fn direction_requires_a_reported_range_but_keeps_ineligible_devices_visible() {
        assert_eq!(direction_project_configs(Vec::new().into_iter()), None);

        let only_44k = vec![SupportedStreamConfigRange::new(
            1,
            44_100,
            44_100,
            SupportedBufferSize::Unknown,
            SampleFormat::F32,
        )];
        assert_eq!(
            direction_project_configs(only_44k.into_iter()),
            Some(Vec::new())
        );
    }

    #[test]
    fn default_device_sorts_before_named_devices() {
        let metadata = |name: &str| DeviceMetadata {
            name: name.to_string(),
            manufacturer: None,
            device_type: "unknown".to_string(),
            interface_type: "unknown".to_string(),
        };
        let mut devices = vec![
            descriptor("coreaudio:z", &metadata("Alpha"), false, None, Vec::new()),
            descriptor("coreaudio:a", &metadata("Zulu"), true, None, Vec::new()),
        ];

        sort_devices(&mut devices);

        assert!(devices[0].is_default);
        assert_eq!(devices[1].name, "Alpha");
    }

    #[test]
    fn tauri_contract_uses_camel_case_fields_and_snake_case_direction() {
        let inventory = AudioDeviceInventory {
            host: "CoreAudio".to_string(),
            sample_rate_hz: 48_000,
            inputs: Vec::new(),
            outputs: vec![AudioDeviceDescriptor {
                id: "coreaudio:loopback".to_string(),
                name: "Virtual loopback".to_string(),
                manufacturer: None,
                device_type: "unknown".to_string(),
                interface_type: "virtual".to_string(),
                is_default: true,
                current_sample_rate_hz: Some(44_100),
                configurations: Vec::new(),
            }],
            warnings: vec![AudioScanWarning {
                device_id: Some("coreaudio:input".to_string()),
                direction: Some(AudioDirection::Input),
                message: "example".to_string(),
            }],
        };

        let value = serde_json::to_value(inventory).expect("inventory should serialize");

        assert_eq!(value["sampleRateHz"], 48_000);
        assert!(value.get("sample_rate_hz").is_none());
        assert_eq!(value["warnings"][0]["deviceId"], "coreaudio:input");
        assert_eq!(value["warnings"][0]["direction"], "input");
        // The wizard warns before playback when this is not the project rate,
        // so it has to survive the crossing.
        assert_eq!(value["outputs"][0]["currentSampleRateHz"], 44_100);
        assert!(value["outputs"][0].get("current_sample_rate_hz").is_none());
    }

    #[test]
    fn the_playback_report_names_the_rate_it_borrowed_and_gave_back() {
        let report = OutputPlaybackReport {
            host: "CoreAudio".to_string(),
            device_id: "coreaudio:loopback".to_string(),
            device_name: Some("Virtual loopback".to_string()),
            sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
            device_rate_before_hz: Some(44_100),
            device_rate_forced: true,
            device_rate_restored: true,
            device_rate_restore_error: None,
            source_channels: 2,
            device_channels: 2,
            sample_format: "f32".to_string(),
            frame_count: 48_000,
            frames_written: 48_000,
            elapsed_ms: 1_000,
            completed: true,
            cancelled: false,
            timed_out: false,
            callback_format_error: false,
            stream_error_count: 0,
            first_stream_error: None,
        };

        let value = serde_json::to_value(report).expect("report should serialize");

        assert_eq!(value["sampleRateHz"], 48_000);
        assert_eq!(value["deviceRateBeforeHz"], 44_100);
        assert_eq!(value["deviceRateForced"], true);
        assert_eq!(value["deviceRateRestored"], true);
        assert_eq!(value["deviceRateRestoreError"], serde_json::Value::Null);
        assert!(value.get("device_rate_before_hz").is_none());
    }

    #[test]
    fn capture_duration_is_rounded_up_and_must_fit_the_explicit_bound() {
        assert_eq!(samples_for_duration_ms(1), Some(48));
        assert_eq!(samples_for_duration_ms(1_001), Some(48_048));

        let too_small = MonoInputCaptureRequest {
            device_id: "coreaudio:measurement-mic".to_string(),
            input_channel_index: 0,
            duration_ms: 1_000,
            maximum_samples: 47_999,
        };
        let error = validate_capture_request(&too_small).expect_err("bound must be enforced");
        assert!(error.to_string().contains("48000 samples"));

        let valid = MonoInputCaptureRequest {
            maximum_samples: 48_000,
            ..too_small
        };
        assert_eq!(validate_capture_request(&valid), Ok(48_000));
    }

    #[test]
    fn capture_request_requires_a_device_and_positive_duration() {
        let missing_device = MonoInputCaptureRequest {
            device_id: " ".to_string(),
            input_channel_index: 0,
            duration_ms: 100,
            maximum_samples: 4_800,
        };
        assert!(validate_capture_request(&missing_device)
            .expect_err("device ID is required")
            .to_string()
            .contains("host-qualified"));

        let zero_duration = MonoInputCaptureRequest {
            device_id: "coreaudio:measurement-mic".to_string(),
            input_channel_index: 0,
            duration_ms: 0,
            maximum_samples: 0,
        };
        assert!(validate_capture_request(&zero_duration)
            .expect_err("zero duration is invalid")
            .to_string()
            .contains("greater than zero"));
    }

    #[test]
    fn automatic_completion_requires_a_bounded_nonempty_snapshot() {
        let request = MonoInputCaptureRequest {
            device_id: "unused-for-preflight".into(),
            input_channel_index: 0,
            duration_ms: 1_000,
            maximum_samples: 48_000,
        };
        let cancellation = InputCaptureCancellation::new();
        let mut detector = |_: usize, _: &[f32]| false;

        let error =
            capture_mono_input_with_automatic_completion(&request, &cancellation, 0, &mut detector)
                .unwrap_err();

        assert!(error
            .to_string()
            .contains("snapshot length must be greater than zero"));
    }

    #[test]
    fn selects_the_lowest_channel_48k_pcm_stream_containing_the_requested_channel() {
        let configurations = vec![
            SupportedStreamConfigRange::new(
                2,
                48_000,
                48_000,
                SupportedBufferSize::Unknown,
                SampleFormat::F32,
            ),
            SupportedStreamConfigRange::new(
                1,
                48_000,
                48_000,
                SupportedBufferSize::Unknown,
                SampleFormat::I16,
            ),
            SupportedStreamConfigRange::new(
                1,
                48_000,
                48_000,
                SupportedBufferSize::Unknown,
                SampleFormat::F32,
            ),
            SupportedStreamConfigRange::new(
                1,
                48_000,
                48_000,
                SupportedBufferSize::Unknown,
                SampleFormat::DsdU8,
            ),
        ];

        let selected = select_project_input_config(configurations.clone().into_iter(), 0)
            .expect("48 kHz mono PCM should be selected for channel one");

        assert_eq!(selected.channels(), 1);
        assert_eq!(selected.sample_rate(), 48_000);
        assert_eq!(selected.sample_format(), SampleFormat::F32);

        let second_channel = select_project_input_config(configurations.into_iter(), 1)
            .expect("48 kHz stereo PCM should be selected for channel two");
        assert_eq!(second_channel.channels(), 2);
        assert_eq!(second_channel.sample_format(), SampleFormat::F32);
    }

    #[test]
    fn extracts_and_normalizes_one_interleaved_channel_without_exceeding_bound() {
        let mut signed = Vec::with_capacity(3);
        append_normalized_channel_samples(
            &[12_i16, i16::MIN, 13, 0, 14, i16::MAX],
            2,
            1,
            &mut signed,
            3,
        )
        .unwrap();
        assert_eq!(signed[0], -1.0);
        assert_eq!(signed[1], 0.0);
        assert!((signed[2] - INPUT_CLIPPING_THRESHOLD_LINEAR).abs() < f32::EPSILON);

        let mut unsigned = Vec::with_capacity(3);
        append_normalized_channel_samples(
            &[u16::MIN, 1, 32_768, 2, u16::MAX, 3],
            2,
            0,
            &mut unsigned,
            3,
        )
        .unwrap();
        assert_eq!(unsigned[0], -1.0);
        assert_eq!(unsigned[1], 0.0);
        assert!((unsigned[2] - INPUT_CLIPPING_THRESHOLD_LINEAR).abs() < f32::EPSILON);

        let mut bounded = Vec::with_capacity(2);
        append_normalized_channel_samples(
            &[9.0_f32, 0.25, 9.0, 0.5, 9.0, 0.75],
            2,
            1,
            &mut bounded,
            2,
        )
        .unwrap();
        assert_eq!(bounded, vec![0.25, 0.5]);

        assert_eq!(
            append_normalized_channel_samples(&[0.0_f32, 1.0, 2.0], 2, 1, &mut Vec::new(), 2,),
            Err(CallbackDataError::FormatMismatch),
        );
    }

    #[test]
    fn signal_metrics_report_peak_clipping_and_non_finite_samples() {
        let metrics = signal_metrics(
            &[0.0, -0.5, INPUT_CLIPPING_THRESHOLD_LINEAR, -1.0, f32::NAN],
            INPUT_CLIPPING_THRESHOLD_LINEAR,
        );

        assert_eq!(metrics.peak_linear, 1.0);
        assert_eq!(metrics.peak_dbfs, Some(0.0));
        assert_eq!(metrics.clipped_sample_count, 2);
        assert_eq!(metrics.non_finite_sample_count, 1);
    }

    #[test]
    fn capture_timestamp_gap_uses_a_small_rounding_tolerance() {
        let start = StreamInstant::ZERO;
        let contiguous = StreamInstant::from_nanos(2_666_667);
        assert_eq!(
            estimate_timestamp_gap_frames(start, contiguous, 128),
            Some(0)
        );

        let gap = StreamInstant::from_nanos(2_916_667);
        assert_eq!(estimate_timestamp_gap_frames(start, gap, 128), Some(10));
        assert_eq!(estimate_timestamp_gap_frames(gap, start, 128), None);
    }

    #[test]
    fn timestamp_irregularity_alone_is_not_pcm_loss() {
        assert!(!known_pcm_loss(0, 0, 0, false));
        assert!(known_pcm_loss(1, 0, 0, false));
        assert!(known_pcm_loss(0, 128, 0, false));
        assert!(known_pcm_loss(0, 0, 128, false));
        assert!(!known_pcm_loss(0, 0, 128, true));
    }

    #[test]
    fn cancellation_is_shared_between_control_and_capture_handles() {
        let capture_handle = InputCaptureCancellation::new();
        let control_handle = capture_handle.clone();
        assert!(!capture_handle.is_cancelled());

        control_handle.cancel();

        assert!(capture_handle.is_cancelled());
    }

    #[test]
    fn capture_request_serializes_for_the_native_command_boundary() {
        let request = MonoInputCaptureRequest {
            device_id: "coreaudio:measurement-mic".to_string(),
            input_channel_index: 1,
            duration_ms: 2_500,
            maximum_samples: 120_000,
        };

        let value = serde_json::to_value(request).expect("request should serialize");

        assert_eq!(value["deviceId"], "coreaudio:measurement-mic");
        assert_eq!(value["inputChannelIndex"], 1);
        assert_eq!(value["durationMs"], 2_500);
        assert_eq!(value["maximumSamples"], 120_000);
        assert!(value.get("maximum_samples").is_none());
    }

    fn playback_shared(
        frames: Vec<f32>,
        source_channels: usize,
        device_channels: usize,
    ) -> PlaybackShared {
        let frame_count = frames.len() / source_channels;
        PlaybackShared {
            frames,
            source_channels,
            device_channels,
            frame_count,
            next_frame: AtomicU64::new(0),
            source_exhausted: AtomicBool::new(false),
            callback_format_error: AtomicBool::new(false),
            stream_error_count: AtomicU64::new(0),
            first_stream_error: Mutex::new(None),
        }
    }

    #[test]
    fn playback_writes_source_channels_in_order_and_silences_the_rest() {
        // Two source channels onto a four-channel device: 3 and 4 stay silent.
        let shared = playback_shared(vec![0.25, -0.5, 0.75, -1.0], 2, 4);
        let mut output = [0.9_f32; 12];

        write_playback_samples(&mut output, &shared);

        assert_eq!(
            output[..8],
            [0.25, -0.5, 0.0, 0.0, 0.75, -1.0, 0.0, 0.0],
            "source channel i must land on device channel i"
        );
        // The buffer outlives the source, so the tail is silence, not a repeat.
        assert_eq!(output[8..], [0.0; 4]);
        assert!(shared.source_exhausted.load(Ordering::Acquire));
        assert_eq!(shared.next_frame.load(Ordering::Acquire), 2);
    }

    #[test]
    fn playback_resumes_where_the_previous_callback_stopped() {
        let shared = playback_shared(vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6], 1, 1);
        let mut first = [0.0_f32; 4];
        let mut second = [0.0_f32; 4];

        write_playback_samples(&mut first, &shared);
        write_playback_samples(&mut second, &shared);

        assert_eq!(first, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(second[..2], [0.5, 0.6]);
        assert_eq!(second[2..], [0.0, 0.0]);
    }

    #[test]
    fn playback_bounds_an_out_of_range_sample_at_full_scale() {
        // The adapter rejects non-finite samples; anything else must still be
        // incapable of driving an amplifier past full scale.
        let shared = playback_shared(vec![4.0, -4.0], 1, 1);
        let mut output = [0.0_f32; 2];

        write_playback_samples(&mut output, &shared);

        assert_eq!(output, [1.0, -1.0]);
    }

    #[test]
    fn playback_rejects_a_request_no_device_could_honour() {
        let base = InterleavedOutputPlaybackRequest {
            device_id: "coreaudio:speaker".to_string(),
            frames: vec![0.0, 0.0],
            channels: 2,
            sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        };
        assert_eq!(validate_playback_request(&base).unwrap(), 2);

        for (request, expected) in [
            (
                InterleavedOutputPlaybackRequest {
                    device_id: "   ".to_string(),
                    ..base.clone()
                },
                "host-qualified output device ID",
            ),
            (
                InterleavedOutputPlaybackRequest {
                    channels: 0,
                    ..base.clone()
                },
                "1 to 8 channels",
            ),
            (
                InterleavedOutputPlaybackRequest {
                    frames: vec![0.0, 0.0, 0.0],
                    ..base.clone()
                },
                "whole number of interleaved frames",
            ),
            (
                InterleavedOutputPlaybackRequest {
                    frames: Vec::new(),
                    ..base.clone()
                },
                "whole number of interleaved frames",
            ),
            (
                InterleavedOutputPlaybackRequest {
                    frames: vec![0.0, f32::NAN],
                    ..base.clone()
                },
                "sample 1 is not a finite value",
            ),
        ] {
            let error = validate_playback_request(&request).expect_err(expected);
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn the_output_config_search_takes_the_narrowest_48k_pcm_configuration() {
        let configurations = vec![
            SupportedStreamConfigRange::new(
                1,
                48_000,
                48_000,
                SupportedBufferSize::Range { min: 64, max: 512 },
                SampleFormat::F32,
            ),
            SupportedStreamConfigRange::new(
                2,
                48_000,
                48_000,
                SupportedBufferSize::Range { min: 64, max: 512 },
                SampleFormat::F32,
            ),
            SupportedStreamConfigRange::new(
                4,
                48_000,
                48_000,
                SupportedBufferSize::Range { min: 64, max: 512 },
                SampleFormat::F32,
            ),
        ];

        // A stereo sweep needs two channels; the two-channel configuration is
        // the least invasive one that fits.
        let selected = select_output_config(configurations.clone().into_iter(), 2, 48_000)
            .expect("a 48 kHz stereo output configuration exists");
        assert_eq!(selected.channels(), 2);
        assert_eq!(selected.sample_rate(), 48_000);

        // Nothing wide enough is an explicit absence, never a narrower fallback.
        assert!(select_output_config(configurations.into_iter(), 6, 48_000).is_none());
    }

    /// The search opens the rate it was asked for or nothing at all. Silently
    /// substituting a neighbouring rate would let CPAL rewrite the device
    /// format behind the caller's back, which is the whole thing the caller is
    /// trying to avoid by asking for the device's own rate.
    #[test]
    fn the_output_config_search_never_substitutes_a_different_rate() {
        let configurations = vec![SupportedStreamConfigRange::new(
            2,
            44_100,
            44_100,
            SupportedBufferSize::Range { min: 64, max: 512 },
            SampleFormat::F32,
        )];

        assert!(select_output_config(configurations.clone().into_iter(), 2, 48_000).is_none());
        let selected = select_output_config(configurations.into_iter(), 2, 44_100)
            .expect("the device's own rate is selectable");
        assert_eq!(selected.sample_rate(), 44_100);
    }

    #[test]
    fn playback_rejects_an_implausible_rate() {
        let base = InterleavedOutputPlaybackRequest {
            device_id: "coreaudio:speaker".to_string(),
            frames: vec![0.0, 0.0],
            channels: 2,
            sample_rate_hz: 44_100,
        };
        assert_eq!(validate_playback_request(&base).unwrap(), 2);

        for rate in [
            0,
            MINIMUM_PLAYBACK_RATE_HZ - 1,
            MAXIMUM_PLAYBACK_RATE_HZ + 1,
        ] {
            let request = InterleavedOutputPlaybackRequest {
                sample_rate_hz: rate,
                ..base.clone()
            };
            let error = validate_playback_request(&request)
                .expect_err("an out-of-range rate must be rejected");
            assert!(error.to_string().contains("Hz"), "{error}");
        }
    }

    #[test]
    fn playback_duration_follows_the_requested_rate() {
        assert_eq!(playback_duration_ms(48_000, 48_000), 1_000);
        assert_eq!(playback_duration_ms(44_100, 44_100), 1_000);
        // A buffer authored at 44.1 kHz lasts longer than 48 kHz would suggest.
        assert_eq!(playback_duration_ms(48_000, 44_100), 1_089);
    }

    #[test]
    #[ignore = "requires a local CoreAudio or WASAPI host"]
    fn local_input_only_scan_does_not_open_streams_or_enumerate_outputs() {
        let inventory =
            scan_default_host_inputs().expect("default audio input host should enumerate");
        println!("{inventory:#?}");
        assert!(!inventory.host.is_empty());
        assert_eq!(inventory.sample_rate_hz, PROJECT_SAMPLE_RATE_HZ);
        assert!(inventory.outputs.is_empty());
        assert!(inventory
            .warnings
            .iter()
            .all(|warning| warning.direction != Some(AudioDirection::Output)));
    }

    /// The regression this guards: in-app playback used to open every device at
    /// 48 kHz, which makes CPAL's CoreAudio path write the rate on the device
    /// object. On a virtual loopback shared with a convolution host that write
    /// stopped the host outright, and worse, stalled
    /// `AudioComponentInstanceNew` forever, so playback never returned and the
    /// device was left on a rate the user never chose. Playing at the device's
    /// own rate rewrites nothing.
    #[test]
    #[ignore = "opens a real output stream on the default device"]
    fn local_hardware_playback_leaves_the_device_rate_alone() {
        let inventory =
            scan_default_host_outputs().expect("default audio output host should enumerate");
        let device = inventory
            .outputs
            .iter()
            .find(|device| {
                device.is_default
                    && device.configurations.iter().any(|configuration| {
                        configuration.channels >= 2
                            && !configuration.sample_format.starts_with("dsd")
                    })
            })
            .expect("requires a default PCM output");
        let before =
            output_device_sample_rate(&device.id).expect("the device should report a rate");

        // Silence: this exercises the device format, not the speakers.
        let request = InterleavedOutputPlaybackRequest {
            device_id: device.id.clone(),
            frames: vec![0.0; 2 * (before as usize / 10)],
            channels: 2,
            sample_rate_hz: before,
        };
        let started = Instant::now();
        let report = play_interleaved_output(&request, &OutputPlaybackCancellation::new())
            .expect("real output playback should open and complete");

        assert_eq!(report.sample_rate_hz, before);
        assert_eq!(report.device_rate_before_hz, Some(before));
        assert!(
            !report.device_rate_forced,
            "playing at the device's own rate must not move it"
        );
        assert!(!report.device_rate_restored);
        assert_eq!(report.device_rate_restore_error, None);
        // The stall this guards against never returns at all; a wall-clock
        // bound turns that into a failure instead of a hung test run.
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "playback took {:?}, which suggests a CoreAudio stall",
            started.elapsed()
        );

        let after =
            output_device_sample_rate(&device.id).expect("the device should still report a rate");
        assert_eq!(after, before, "the sweep must not change the device rate");
    }

    #[test]
    #[ignore = "opens a real 48 kHz mono input and requires microphone permission"]
    fn local_hardware_mono_capture_smoke_test() {
        let inventory =
            scan_default_host_inputs().expect("default audio input host should enumerate");
        let device = inventory
            .inputs
            .iter()
            .find(|device| {
                device.configurations.iter().any(|configuration| {
                    configuration.sample_rate_hz == PROJECT_SAMPLE_RATE_HZ
                        && configuration.channels >= 1
                        && !configuration.sample_format.starts_with("dsd")
                })
            })
            .expect("requires a connected 48 kHz PCM input");
        let request = MonoInputCaptureRequest {
            device_id: device.id.clone(),
            input_channel_index: 0,
            duration_ms: 100,
            maximum_samples: 4_800,
        };

        let capture =
            capture_mono_input(&request).expect("real input capture should open and complete");

        assert_eq!(capture.sample_rate_hz, PROJECT_SAMPLE_RATE_HZ);
        assert_eq!(capture.channels, 1);
        assert_eq!(capture.input_channel_index, 0);
        assert_eq!(capture.captured_samples, 4_800);
        assert!(capture.capture_complete);
        assert!(!capture.cancelled);
        assert_eq!(capture.samples.len(), capture.captured_samples);
    }
}
