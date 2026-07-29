//! Stateful developer-beta path from a real asynchronous microphone capture to
//! a verified minimum-phase Roon package.
//!
//! The room-correction algorithms are not reimplemented here. Live captures
//! are adapted to the versioned Phase 4 bounded-correction engine, then the
//! admitted physical correction intent is passed to the versioned Phase 6
//! native-rate engine and the existing
//! strict Roon exporter. New code in this module owns only session persistence,
//! live-evidence admission, closed-loop comparison, and measured headroom.

use crate::wireless_sweep::{decode_reference_wav, ReferenceChannel};
use chrono::Local;
use eqforbeginner_audio_io::{
    InputCaptureCancellation, MonoInputCapture, MonoInputCaptureRequest, PROJECT_SAMPLE_RATE_HZ,
};
use eqforbeginner_dsp_core::analysis::{frequency_response, FrequencyResponse};
use eqforbeginner_dsp_core::calibration::{
    parse_umik_calibration, MicrophoneCalibration, UMIK_CALIBRATION_PARSER_VERSION,
};
use eqforbeginner_dsp_core::measurement::{
    deconvolve_recognized_sweep, SweepDeconvolutionConfig, SweepMeasurement,
    KNOWN_SWEEP_DECONVOLUTION_VERSION,
};
use eqforbeginner_dsp_core::phase4::{
    run_phase4_offline, MeasuredStereoPosition, MeasuredStereoResponseSet, Phase4OfflineConfig,
    Phase4OfflineResult, TimedCombinedImpulse, PHASE4_OFFLINE_ALGORITHM_VERSION,
};
use eqforbeginner_dsp_core::phase6::{
    compare_stereo_filter_responses, design_native_rate_filters, native_fft_size, Phase6Config,
    Phase6DesignIntent, Phase6NativeResult, PHASE6_ALGORITHM_VERSION,
};
use eqforbeginner_dsp_core::smoothing::gaussian_log_frequency_smooth_at_db;
use eqforbeginner_dsp_core::spatial::weighted_energy_average_db;
use eqforbeginner_dsp_core::stereo::StereoBlendSettings;
use eqforbeginner_dsp_core::sub_integration::{
    optimize_separated_paths, CombinedResponse, Polarity, RankingConfig, SeparatedCrossoverPaths,
    SeparatedPathOptimizationConfig, SEPARATED_PATH_OPTIMIZATION_VERSION,
};
use eqforbeginner_dsp_core::target::{
    interpolate_log_frequency_grid, parse_target_txt, TargetCurve, TargetPreset,
    TARGET_TXT_PARSER_VERSION,
};
use eqforbeginner_dsp_core::validation::{
    fft_convolve, log_frequency_smoothed_curve, log_frequency_smoothed_rmse_db,
    validate_frequency_prediction, ValidationIssue, ValidationReport, ValidationThresholds,
};
use eqforbeginner_dsp_core::wireless_sweep::{
    recognize_wireless_sweep, WirelessClockDriftEvidence, WirelessSweepDetection,
    WirelessSweepRecognition, WirelessSweepRecognitionConfig, WirelessSweepRejectionReason,
};
use eqforbeginner_export::{
    create_roon_six_rate_zip, create_roon_zip, validate_roon_six_rate_zip, validate_roon_zip,
    write_stereo_wav, StereoFir,
};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;

// Historic on-disk format id, predates the product rename; not user-visible branding
// — do not rename. It is written into and compared against persisted live measurement
// snapshots under the app data directory.
pub const LIVE_MEASUREMENT_PROJECT_VERSION: &str = "similarrew-live-project-v5";
/// v4 (2026-07-29): the overall-improvement judgment moved from unsmoothed
/// linear-grid RMSE to gate-smoothed, octave-cell-weighted RMSE over the
/// correction band. On the first real v5 session the linear judgment failed a
/// working filter: 31% of its bins sat in 300-500 Hz, where a small P0
/// microphone reposition between the baseline and verification captures had
/// already shifted the unfiltered response (the session's own unfiltered
/// repeats read 8.5-8.8 dB RMSE at 300-400 Hz against the baseline's 6.6
/// before any filter was loaded), while the corrected 50-100 Hz band - which
/// genuinely improved by 1.0-1.6 dB - held only 8% of the vote. Smoothing
/// removes the narrow comb displacement the agreement gate already tolerates,
/// and octave cells weight the comparison the way the correction itself is
/// distributed. The unsmoothed RMSEs remain reported as diagnostics.
pub const LIVE_CLOSED_LOOP_VERSION: &str = "live-closed-loop-validation-v4";
pub const LIVE_NATIVE_BINDING_VERSION: &str = "verified-trial-native48-response-binding-v1";
pub const LIVE_HEADROOM_VERSION: &str = "validation-signal-and-response-peak-v3";
pub const LIVE_RESULT_PLOT_VERSION: &str = "measured-fr-result-plot-v2";
pub const LIVE_RESULT_PLOT_SMOOTHING_FWHM_OCTAVES: f64 = 1.0 / 12.0;
pub const LIVE_CAPTURE_ENDPOINT_VERSION: &str = "known-marker-capture-endpoint-v4";
pub const LIVE_LEVEL_ASSESSMENT_VERSION: &str = "umik-sweep-level-assessment-v2";
pub const SWEEP_MARKER_CHANNEL_ANALYSIS_VERSION: &str = "uploaded-wav-marker-channel-analysis-v1";
pub const LIVE_SUBWOOFER_SETUP_VERSION: &str = "manual-single-sub-settings-v1";
pub const LIVE_SUBWOOFER_SEARCH_PLAN_VERSION: &str = "live-separated-path-search-plan-v1";
pub const LIVE_ACCEPTED_MEASUREMENT_CACHE_VERSION: &str = "accepted-measurement-snapshot-cache-v3";
/// Session subdirectory holding a REW-importable copy of every accepted capture.
///
/// REW's own `.mdat` is a Java-serialized save format with no documented
/// third-party writer, so this app does not try to synthesize one: a file REW
/// silently misreads would be worse than no file. What REW does document as
/// importable is an impulse response in a WAV, so each accepted capture is also
/// written as a mono 48 kHz float WAV named after what was measured. Importing
/// those into REW and using its own "Save All Measurements" produces a genuine
/// `.mdat` written by REW itself.
const LIVE_REW_EXPORT_DIRECTORY: &str = "rew";
pub const LIVE_REW_EXPORT_VERSION: &str = "rew-impulse-export-v1";
pub const MAX_LIVE_SWEEP_BYTES: usize = 32 * 1024 * 1024;
const REQUIRED_CALIBRATION_BAND_HZ: [f64; 2] = [20.0, 20_000.0];
const MAX_CALIBRATION_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_LIVE_TARGET_TEXT_BYTES: usize = 2 * 1024 * 1024;
const CAPTURE_DEADLINE_GRACE_MILLISECONDS: u64 = 1_000;
#[cfg(test)]
const CAPTURE_DEADLINE_GRACE_SAMPLES: usize =
    PROJECT_SAMPLE_RATE_HZ as usize * CAPTURE_DEADLINE_GRACE_MILLISECONDS as usize / 1_000;
const MARKER_SEARCH_STEP_SAMPLES: usize = PROJECT_SAMPLE_RATE_HZ as usize / 2;
const MARKER_SEARCH_MARGIN_SAMPLES: usize = PROJECT_SAMPLE_RATE_HZ as usize;
const MINIMUM_MARKER_CORRELATION: f64 = 0.20;
const MINIMUM_MARKER_SEGMENT_CORRELATION: f64 = 0.16;
const MINIMUM_ROOM_BIASED_MARKER_CORRELATION: f64 = 0.30;
const REQUIRED_ROOM_BIASED_MARKER_SEGMENTS: usize = 5;
const MINIMUM_REPEATED_MARKER_CANDIDATE_RATIO: f64 = 0.50;
const MAXIMUM_SINGLE_MARKER_INTERNAL_SLOPE_PPM: f64 = 25_000.0;
const MAXIMUM_MARKER_PAIR_CLOCK_DRIFT_PPM: f64 = 5_000.0;
/// The fixed right-speaker marker can arrive after the measured left main or
/// sub path. Retain 100 ms before that marker-referenced boundary without
/// moving the IR peak or erasing the original signed timeline.
///
/// 4,800 rather than the original 1,024 (2026-07-29 ablation on the first real
/// session). Marker referencing cancels the shared flight time, so the largest
/// impulse sample lands essentially on the boundary itself - measured index
/// 1,012-1,061 across all 18 captures of that session. With only 1,024 samples
/// ahead of it, an early low-frequency arrival is clipped by the retention
/// window rather than by the room, and because the clipping is relative to a
/// peak that moves by a fraction of a millisecond between captures, what is
/// lost changes from capture to capture. Widening it to 4,800 moved that
/// session's central-repeat agreement from 1.3814 ms to 0.2394 ms without
/// changing the compared bin population.
///
/// The bundled sweeps leave 12,960 samples between the start marker's end and
/// the main sweep, so this is a code constant, not a sweep-asset limit; the
/// remaining headroom is bounded in practice by the start marker's own
/// reverberant tail rather than by the file layout.
const MARKER_REFERENCED_IMPULSE_PRE_ZERO_SAMPLES: usize = 4_800;

// The retained pre-zero region has to stay long enough to hold a full cycle of
// the lowest corrected frequency (20 Hz = 2,400 samples) with margin, and short
// enough to stay inside the 12,960-sample gap the bundled sweeps leave between
// the start marker and the main sweep - past that the marker itself enters the
// retained window. Changing this constant changes the deconvolution version and
// invalidates every cached measurement, so it is pinned here deliberately.
const _: () = {
    assert!(MARKER_REFERENCED_IMPULSE_PRE_ZERO_SAMPLES >= 2 * 2_400);
    assert!(MARKER_REFERENCED_IMPULSE_PRE_ZERO_SAMPLES <= 12_960);
};
const UMIK_MAXIMUM_VOLUME_DIGITAL_GAIN_DB: f64 = 24.0;
const SPL_REFERENCE_DB: f64 = 94.0;
const LEVEL_RECOMMENDED_MINIMUM_PEAK_DBFS: f64 = -30.0;
const LEVEL_MINIMUM_ACCEPTED_PEAK_DBFS: f64 = -48.0;
const LEVEL_HIGH_PEAK_DBFS: f64 = -6.0;
const LEVEL_CLIPPING_PEAK_DBFS: f64 = -1.0;
const LEVEL_RECOMMENDED_MINIMUM_SPL_DB: f64 = 65.0;
const LEVEL_RECOMMENDED_MAXIMUM_SPL_DB: f64 = 85.0;
const LIVE_MINIMUM_CAPTURE_SNR_DB: f64 = 20.0;
const ACTIVE_GAP_SECONDS: f64 = 0.20;
const ACTIVE_GUARD_SECONDS: f64 = 0.02;
const MAXIMUM_PREDICTED_VERIFIED_RMSE_DB: f64 = 3.0;
/// Largest tolerated change of the >=650 Hz-band start-marker RMS between the
/// accepted baseline P0 pair and the verification pair. The marker plays from
/// the same speaker at the same position and the product filter is unity at
/// and above 650 Hz, so a shift here is a playback/capture volume change, not
/// the filter (2026-07-29 expert review, finding 5).
const MAXIMUM_VERIFICATION_MARKER_LEVEL_SHIFT_DB: f64 = 0.5;
/// The applied-correction scale fit only runs when the designed correction has
/// at least this much smoothed RMS in the agreement band; below it there is no
/// signal to fit a scale to (a flat room), and the fit would be noise.
const MINIMUM_SCALE_FIT_CORRECTION_RMS_DB: f64 = 0.5;
const MINIMUM_APPLIED_CORRECTION_SCALE: f64 = 0.6;
const MAXIMUM_APPLIED_CORRECTION_SCALE: f64 = 1.4;
const PREDICTION_VERIFICATION_LOW_HZ: f64 = 20.0;
const PREDICTION_VERIFICATION_HIGH_HZ: f64 = 650.0;
const MAXIMUM_P0_REPEAT_LEVEL_SHIFT_DB: f64 = 1.0;
// P0_END is hand-repositioned after the surrounding points, not held in a
// measurement jig. Keep a loose gross-change guard while allowing normal
// low-frequency modal differences from returning to the center area.
const MAXIMUM_P0_REPEAT_SHAPE_RMSE_DB: f64 = 6.0;
const MAGNITUDE_ONLY_MAXIMUM_TIMING_FIT_RMS_SAMPLES: f64 = 96.0;
const TRUE_PEAK_OVERSAMPLE: usize = 4;
const HEADROOM_SAFETY_MARGIN_DB: f64 = 1.0;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveChannel {
    Left,
    Right,
}

impl LiveChannel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveCaptureKind {
    SubMainOnly,
    SubOnly,
    Baseline,
    Verification,
}

impl LiveCaptureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::SubMainOnly => "sub_main_only",
            Self::SubOnly => "sub_only",
            Self::Baseline => "baseline",
            Self::Verification => "verification",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LiveSystemMode {
    #[serde(rename = "stereo_2_0")]
    Stereo20,
    #[serde(rename = "single_sub_2_1")]
    SingleSub21,
}

impl LiveSystemMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stereo20 => "stereo_2_0",
            Self::SingleSub21 => "single_sub_2_1",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionSummary {
    pub session_id: String,
    pub output_directory: String,
    pub project_version: String,
    pub system_mode: LiveSystemMode,
    pub system_declaration_path: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferSetupRequest {
    pub crossover_hz: f64,
    pub main_delay_ms: f64,
    pub polarity_degrees: u16,
    pub sub_level_db: f64,
    pub confirmed_on_hardware: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferSetupSummary {
    pub algorithm_version: &'static str,
    pub crossover_hz: f64,
    pub main_delay_ms: f64,
    pub polarity_degrees: u16,
    pub sub_level_db: f64,
    pub confirmed_on_hardware: bool,
    pub settings_path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferCrossoverCandidate {
    pub id: String,
    pub crossover_hz: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferSearchRequest {
    pub crossover_hz: Vec<f64>,
    pub measured_main_delay_ms: f64,
    pub measured_polarity_degrees: u16,
    pub fixed_sub_level_db: f64,
    pub delay_minimum_ms: f64,
    pub delay_maximum_ms: f64,
    pub delay_step_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferSearchSummary {
    pub algorithm_version: &'static str,
    pub candidates: Vec<LiveSubwooferCrossoverCandidate>,
    pub measured_main_delay_ms: f64,
    pub measured_polarity_degrees: u16,
    pub fixed_sub_level_db: f64,
    pub delay_minimum_ms: f64,
    pub delay_maximum_ms: f64,
    pub delay_step_ms: f64,
    pub fixed_timing_reference_channel: ReferenceChannel,
    pub sub_sweep_channel: LiveChannel,
    pub plan_path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferRankedSetting {
    pub rank: usize,
    pub crossover_hz: f64,
    pub main_delay_ms: f64,
    pub polarity_degrees: u16,
    pub total_score: f64,
    pub deficit_rms_db: f64,
    pub deficit_p95_db: f64,
    pub worst_deficit_db: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferOptimizationSummary {
    pub algorithm_version: &'static str,
    pub synthesized_candidate_count: usize,
    pub best: LiveSubwooferRankedSetting,
    pub rankings: Vec<LiveSubwooferRankedSetting>,
    pub scoring_lower_hz: f64,
    pub scoring_upper_hz: f64,
    pub fixed_sub_level_db: f64,
    pub needs_combined_confirmation: bool,
    /// Per-crossover sub-minus-main arrival estimated on the shared marker
    /// timeline before candidate synthesis. Positive means the sub is late
    /// (delay the main by this much); negative means the delay belongs on the
    /// sub side. The search windows one crossover half-period around it.
    pub arrival_estimates: Vec<LiveSubwooferArrivalEstimate>,
    /// Advisory only: predicted crossover-dip change if the sub level moved,
    /// with the winning crossover/delay/polarity held fixed.
    pub sub_level_advisory: Option<LiveSubwooferSubLevelAdvisory>,
    pub warnings: Vec<String>,
    pub report_path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferSubLevelAdvisory {
    pub best_gain_db: f64,
    pub deficit_rms_at_best_db: f64,
    pub deficit_rms_at_zero_db: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSubwooferArrivalEstimate {
    pub crossover_hz: f64,
    pub center_ms: f64,
    /// Disagreement between the independent L/R estimates of the same
    /// physical offset - an empirical uncertainty for the recommendation.
    pub left_right_spread_ms: f64,
    pub window_low_ms: f64,
    pub window_high_ms: f64,
    pub range_limited: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalibrationImportSummary {
    pub file_name: String,
    pub sha256: String,
    pub parser_version: String,
    pub serial_number: Option<String>,
    pub sensitivity_factor_db: Option<f64>,
    pub point_count: usize,
    pub minimum_frequency_hz: f64,
    pub maximum_frequency_hz: f64,
    pub correction_band_covered: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetImportSummary {
    pub file_name: String,
    pub sha256: String,
    pub parser_version: String,
    pub point_count: usize,
    pub minimum_frequency_hz: f64,
    pub maximum_frequency_hz: f64,
    pub correction_band_covered: bool,
    pub alignment_lower_hz: f64,
    pub alignment_upper_hz: f64,
    pub stored_path: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSweepImportSummary {
    pub channel: LiveChannel,
    pub sha256: String,
    pub source_channels: u16,
    pub sample_rate_hz: u32,
    pub source_duration_seconds: f64,
    pub measurement_start_seconds: f64,
    pub measurement_end_seconds: f64,
    pub measurement_duration_seconds: f64,
    pub measurement_peak_dbfs: f64,
    pub source_reference_channel: ReferenceChannel,
    pub timing_marker_count: usize,
    pub marker_channel_analysis_version: &'static str,
    pub start_marker_channel: Option<ReferenceChannel>,
    pub end_marker_channel: Option<ReferenceChannel>,
    pub start_marker_channel_separation_db: Option<f64>,
    pub end_marker_channel_separation_db: Option<f64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveCaptureProgressPhase {
    WaitingForStart,
    StartMarkerDetected,
    MeasuringSweep,
    EndMarkerDetected,
    SavingMeasurement,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveMeasurementLevelStatus {
    Waiting,
    TooLow,
    Good,
    High,
    Clipping,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCaptureProgress {
    pub algorithm_version: &'static str,
    pub phase: LiveCaptureProgressPhase,
    pub elapsed_seconds: f64,
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    /// An estimated unweighted acoustic level. It assumes the UMIK sensitivity
    /// header is valid and that the OS input-volume stage adds 0 dB.
    pub estimated_spl_db: Option<f64>,
    pub level_status: LiveMeasurementLevelStatus,
    pub start_marker_detected: bool,
    pub end_marker_detected: bool,
    pub automatic_completion_armed: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveMeasurementLevelAssessment {
    pub algorithm_version: &'static str,
    pub status: LiveMeasurementLevelStatus,
    pub acceptable_for_measurement: bool,
    pub measurement_peak_dbfs: f64,
    pub measurement_rms_dbfs: f64,
    pub estimated_spl_db: Option<f64>,
    pub estimated_spl_assumption: Option<&'static str>,
    pub minimum_accepted_peak_dbfs: f64,
    pub recommended_peak_minimum_dbfs: f64,
    pub recommended_peak_maximum_dbfs: f64,
    pub recommended_spl_minimum_db: f64,
    pub recommended_spl_maximum_db: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveAudioStreamDiagnostics {
    pub xrun_count: u64,
    pub callback_lock_drop_frames: u64,
    pub timestamp_gap_frames: u64,
    pub timestamp_discontinuity_count: u64,
    pub missing_samples_at_end: usize,
    pub stream_error_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCaptureSummary {
    pub kind: LiveCaptureKind,
    pub channel: LiveChannel,
    pub position_id: String,
    pub input_device_id: String,
    pub input_device_name: Option<String>,
    pub input_channel_index: u16,
    pub source_channel_count: u16,
    pub accepted: bool,
    pub issue_codes: Vec<String>,
    pub diagnostic_codes: Vec<String>,
    pub audio_stream_diagnostics: LiveAudioStreamDiagnostics,
    pub capture_peak_dbfs: Option<f64>,
    pub capture_snr_db: Option<f64>,
    pub reconstruction_fit_db: Option<f64>,
    pub reconstruction_fit_required: bool,
    pub correlation: Option<f64>,
    pub clock_drift_ppm: Option<f64>,
    pub start_marker_detected: bool,
    pub end_marker_detected: bool,
    /// RMS level of the recognized start-marker segment in the capture, in
    /// dBFS. The marker always plays from the same reference speaker at the
    /// same microphone position, so agreement across captures proves the
    /// playback/capture gain stayed put (2026-07-29 expert review, finding 1).
    pub start_marker_rms_dbfs: Option<f64>,
    /// Sweep-vs-noise SNR per octave band (20-40/40-80/80-160/160-320/
    /// 320-640 Hz). A comparative diagnostic; boost additionally requires its
    /// band to clear `LIVE_MINIMUM_BOOST_BAND_SNR_DB`.
    pub octave_band_snr_db: Option<Vec<Option<f64>>>,
    pub automatic_completion_detected: bool,
    pub level_assessment: LiveMeasurementLevelAssessment,
    pub captured_frames: usize,
    pub raw_wav_path: String,
    pub measurement_snapshot_path: Option<String>,
    pub frequency_bin_count: usize,
    pub timing_eligible: bool,
    pub restored_from_cache: bool,
    pub cache_source_session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveMeasurementCacheRestoreSummary {
    pub algorithm_version: &'static str,
    pub source_session_id: Option<String>,
    pub source_session_ids: Vec<String>,
    pub restored_captures: Vec<LiveCaptureSummary>,
    pub scanned_snapshot_count: usize,
    pub compatible_snapshot_count: usize,
}

/// Which cached measurement kinds a restore may touch. The dedicated
/// time-alignment repeats are excluded from the general restore because their
/// presence is what turns the optional time-correction branch on; the user
/// opts into them with a separate button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveRestoreScope {
    /// Every restorable kind. (Closed-loop verification captures are never
    /// restored at all.)
    General,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveDesignSummary {
    pub algorithm_version: String,
    pub numerical_passed: bool,
    pub position_count: usize,
    pub trial_wav_path: String,
    pub trial_zip_path: String,
    pub left_raw_rmse_db: f64,
    pub left_predicted_rmse_db: f64,
    pub right_raw_rmse_db: f64,
    pub right_predicted_rmse_db: f64,
    pub maximum_attenuation_db: f64,
    pub maximum_boost_db: f64,
    pub protected_dips_passed: bool,
    pub warning: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveFrequencyResponsePlot {
    pub algorithm_version: &'static str,
    pub display_smoothing_fwhm_octaves: f64,
    pub frequencies_hz: Vec<f64>,
    pub raw_left_db: Vec<f64>,
    pub raw_right_db: Vec<f64>,
    pub raw_average_db: Vec<f64>,
    pub target_left_db: Vec<f64>,
    pub target_right_db: Vec<f64>,
    pub target_average_db: Vec<f64>,
    pub predicted_left_db: Vec<f64>,
    pub predicted_right_db: Vec<f64>,
    pub predicted_average_db: Vec<f64>,
    pub verified_left_db: Vec<f64>,
    pub verified_right_db: Vec<f64>,
    pub verified_average_db: Vec<f64>,
    pub correction_low_hz: f64,
    pub correction_high_hz: f64,
    pub taper_end_hz: f64,
    pub protected_dip_frequencies_hz: Vec<f64>,
    pub corrected_peak_frequencies_hz: Vec<f64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveVerificationSummary {
    pub algorithm_version: String,
    pub passed: bool,
    pub left_passed: bool,
    pub right_passed: bool,
    pub left_raw_rmse_db: f64,
    pub left_verified_rmse_db: f64,
    pub right_raw_rmse_db: f64,
    pub right_verified_rmse_db: f64,
    pub left_predicted_verified_rmse_db: f64,
    pub right_predicted_verified_rmse_db: f64,
    pub left_unsmoothed_predicted_verified_rmse_db: f64,
    pub right_unsmoothed_predicted_verified_rmse_db: f64,
    /// The values the v4 improvement judgment actually compares: gate-smoothed,
    /// octave-cell-weighted RMSE against the aligned target over 20-500 Hz.
    /// `None` only on a degenerate grid, where the unsmoothed judgment stands.
    pub left_gate_raw_rmse_db: Option<f64>,
    pub left_gate_verified_rmse_db: Option<f64>,
    pub right_gate_raw_rmse_db: Option<f64>,
    pub right_gate_verified_rmse_db: Option<f64>,
    pub prediction_verification_smoothing_fwhm_octaves: f64,
    pub maximum_allowed_predicted_verified_rmse_db: f64,
    /// Least-squares scale of the designed correction actually observed in
    /// the verification capture (1 = applied once). `None` when the designed
    /// correction is too small to fit a scale.
    pub left_applied_correction_scale: Option<f64>,
    pub right_applied_correction_scale: Option<f64>,
    pub issues: Vec<String>,
    pub frequency_response: LiveFrequencyResponsePlot,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveExportSummary {
    pub zip_path: String,
    pub project_path: String,
    pub zip_sha256: String,
    pub algorithm_version: String,
    pub recommended_headroom_db: f64,
    pub measured_true_peak_ratio_db: f64,
    pub maximum_filter_response_gain_db: f64,
    pub fir_worst_case_peak_bound_db: f64,
    /// L1-bound-based figure that no signal can ever exceed; the default
    /// recommendation uses the response peak instead (headroom v3).
    pub absolute_safe_headroom_db: f64,
    pub final_48k_binding_maximum_magnitude_difference_db: f64,
    pub final_48k_binding_maximum_relative_group_delay_difference_ms: f64,
    pub native_rate_count: usize,
    pub cross_rate_passed: bool,
    pub verification: LiveVerificationSummary,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveZipArtifactKind {
    Trial,
    Final,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveZipDownloadSummary {
    pub artifact_kind: LiveZipArtifactKind,
    pub file_name: String,
    pub saved_path: String,
    pub byte_count: u64,
    pub sha256: String,
}

#[derive(Clone, Debug)]
struct CalibrationEntry {
    summary: CalibrationImportSummary,
    profile: MicrophoneCalibration,
}

#[derive(Clone, Debug)]
struct TargetEntry {
    summary: TargetImportSummary,
    curve: TargetCurve,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveSweepReference {
    summary: LiveSweepImportSummary,
    pub(crate) samples: Vec<f64>,
    pub(crate) source_frame_count: usize,
    measurement_source_start_sample: usize,
    timing_markers: Vec<TimingMarker>,
}

#[derive(Clone, Debug)]
struct TimingMarker {
    source_start_sample: usize,
    samples: Vec<f64>,
    source_channel: ReferenceChannel,
    channel_separation_db: Option<f64>,
    is_start_marker: bool,
}

#[derive(Clone, Copy, Debug)]
struct TimingMarkerMatch {
    capture_start_sample: f64,
    absolute_correlation: f64,
}

#[derive(Clone, Copy, Debug)]
struct TimingMarkerPairDetection {
    first: TimingMarkerMatch,
    last: TimingMarkerMatch,
    capture_samples_per_reference_sample: f64,
    estimated_sweep_start_sample: f64,
    estimated_sweep_end_sample_exclusive: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct LiveCaptureMonitorUpdate {
    pub should_complete: bool,
    pub progress: LiveCaptureProgress,
}

pub(crate) struct LiveCaptureMonitor {
    sweep: LiveSweepReference,
    sensitivity_factor_db: Option<f64>,
    start_marker: Option<TimingMarkerMatch>,
    end_marker: Option<TimingMarkerMatch>,
    expected_sweep_start_sample: Option<f64>,
    expected_sweep_end_sample: Option<f64>,
    next_start_search_sample: usize,
    next_end_search_sample: usize,
    automatic_completion_sample: Option<usize>,
    observed_peak_linear: f64,
    observed_sample_count: usize,
    last_observed_sample: usize,
    measurement_peak_linear: f64,
    measurement_energy: f64,
    measurement_sample_count: usize,
    last_level_sample: usize,
}

#[derive(Clone, Debug)]
struct StoredMeasurement {
    summary: LiveCaptureSummary,
    calibrated_frequency_response: FrequencyResponse,
    calibrated_impulse_samples: Vec<f64>,
    recognized_sweep_start_capture_sample: f64,
    frequencies_hz: Vec<f64>,
    magnitude_db: Vec<f64>,
    evidence: LiveCaptureEvidence,
}

#[derive(Clone, Debug)]
struct LiveDesign {
    summary: LiveDesignSummary,
    target_name: String,
    target_version: String,
    custom_target: Option<TargetImportSummary>,
    response_set: MeasuredStereoResponseSet,
    result: Phase4OfflineResult,
    full_left_aligned_target_db: Vec<f64>,
    full_right_aligned_target_db: Vec<f64>,
    evidence_sha256: String,
    user_declared_active_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveCaptureEvidence {
    session_id: String,
    generation: u64,
    system_mode: LiveSystemMode,
    subwoofer_setup: Option<LiveSubwooferSetupSummary>,
    subwoofer_search: Option<LiveSubwooferSearchSummary>,
    calibration_sha256: String,
    sweep_sha256: String,
    design_sha256: Option<String>,
    input_device_id: String,
    input_channel_index: u16,
}

#[derive(Clone, Debug)]
struct LiveSession {
    id: String,
    root: PathBuf,
    system_mode: LiveSystemMode,
    system_declaration_path: PathBuf,
    subwoofer_setup: Option<LiveSubwooferSetupSummary>,
    subwoofer_search: Option<LiveSubwooferSearchSummary>,
    subwoofer_optimization: Option<LiveSubwooferOptimizationSummary>,
    next_artifact_index: u64,
    evidence_generation: u64,
    locked_input_device_id: Option<String>,
    locked_input_channel_index: Option<u16>,
    calibration: Option<CalibrationEntry>,
    custom_target: Option<TargetEntry>,
    sweeps: BTreeMap<LiveChannel, LiveSweepReference>,
    measurements: BTreeMap<(LiveCaptureKind, String, LiveChannel), StoredMeasurement>,
    design: Option<LiveDesign>,
    last_export: Option<LiveExportSummary>,
}

#[derive(Default)]
pub struct LiveMeasurementState {
    session: Mutex<Option<LiveSession>>,
    active_capture: Mutex<Option<InputCaptureCancellation>>,
}

fn unix_milliseconds() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn safe_file_label(name: &str) -> String {
    let label: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    if label.is_empty() {
        "unnamed.txt".to_string()
    } else {
        label
    }
}

fn validate_position_id(position_id: &str, kind: LiveCaptureKind) -> Result<String, String> {
    if matches!(
        kind,
        LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly
    ) {
        let trimmed = position_id.trim();
        if trimmed.len() == 4
            && trimmed.starts_with("XO")
            && trimmed[2..]
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            return Ok(trimmed.to_string());
        }
        return Err(format!(
            "unsupported {} crossover candidate `{trimmed}`",
            kind.as_str()
        ));
    }
    let allowed: BTreeSet<&str> = match kind {
        LiveCaptureKind::Baseline => ["P0", "P0_END", "P1", "P2", "P3", "P4", "P5"]
            .into_iter()
            .collect(),
        // Closed-loop verification is a central-seat experiment: the filter is
        // judged where it was designed to be judged.
        LiveCaptureKind::Verification => ["P0"].into_iter().collect(),
        LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly => {
            unreachable!("handled before fixed-position validation")
        }
    };
    let trimmed = position_id.trim();
    if !allowed.contains(trimmed) {
        return Err(format!(
            "unsupported {} position `{trimmed}`",
            kind.as_str()
        ));
    }
    Ok(trimmed.to_string())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))
}

/// Human-readable REW measurement name for one capture, e.g. `L+Sub P0` or
/// `Sub XO02`.
///
/// REW names an imported measurement after its file, and that name is the only
/// context it carries once it is sitting in REW's measurement list next to
/// measurements from other sessions. It therefore has to say which acoustic path
/// was measured, not which internal capture kind produced it.
fn rew_measurement_label(
    system_mode: LiveSystemMode,
    kind: LiveCaptureKind,
    channel: LiveChannel,
    position_id: &str,
) -> String {
    let channel_label = match channel {
        LiveChannel::Left => "L",
        LiveChannel::Right => "R",
    };
    let path = match (kind, system_mode) {
        (LiveCaptureKind::SubOnly, _) => "Sub".to_string(),
        // A main-only capture is the speaker measured with the subwoofer
        // physically switched off, so it is the bare channel.
        (LiveCaptureKind::SubMainOnly, _) => channel_label.to_string(),
        (
            LiveCaptureKind::Baseline | LiveCaptureKind::Verification,
            LiveSystemMode::SingleSub21,
        ) => format!("{channel_label}+Sub"),
        (LiveCaptureKind::Baseline | LiveCaptureKind::Verification, LiveSystemMode::Stereo20) => {
            channel_label.to_string()
        }
    };
    // A verification capture measures the same physical path as its baseline,
    // with the trial filter active. Without this the two are indistinguishable
    // in REW and the wrong one can be read as the "before" curve.
    let state = if matches!(kind, LiveCaptureKind::Verification) {
        " filtered"
    } else {
        ""
    };
    format!("{path} {position_id}{state}")
}

/// Reserve a collision-free REW export path for one capture.
///
/// The timestamp is local wall-clock time because it exists for a human reading
/// a file list, not for any ordering decision the app makes; capture ordering
/// comes from the artifact index, which is independent of the clock.
fn reserve_rew_export_path(root: &Path, label: &str) -> Result<PathBuf, String> {
    let directory = root.join(LIVE_REW_EXPORT_DIRECTORY);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create the REW export directory: {error}"))?;
    let stamp = Local::now().format("%Y-%m-%d %H-%M-%S").to_string();
    let candidate = directory.join(format!("{label} {stamp}.wav"));
    if !candidate.exists() {
        return Ok(candidate);
    }
    // Same path, same label, same second: an immediate retry. Both captures are
    // evidence, so the later one is suffixed rather than overwriting the first.
    for attempt in 2..=64u32 {
        let candidate = directory.join(format!("{label} {stamp} #{attempt}.wav"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("could not allocate a unique REW export file name".to_string())
}

/// Write one calibrated impulse response as a mono 48 kHz float WAV.
///
/// This is the same calibrated impulse the design reads, so REW shows what the
/// app designed from. The microphone correction is already applied — loading a
/// UMIK calibration file in REW on top of this would apply it twice.
fn write_rew_impulse_wav(path: &Path, samples: &[f64]) -> Result<(), String> {
    if samples.is_empty() || samples.iter().any(|sample| !sample.is_finite()) {
        return Err("impulse response is empty or not finite".to_string());
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: PROJECT_SAMPLE_RATE_HZ,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    for &sample in samples {
        writer
            .write_sample(sample as f32)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    }
    writer
        .finalize()
        .map_err(|error| format!("could not finalize {}: {error}", path.display()))
}

fn advance_evidence_generation(session: &mut LiveSession) -> Result<(), String> {
    session.last_export = None;
    session.evidence_generation = session
        .evidence_generation
        .checked_add(1)
        .ok_or_else(|| "live evidence generation overflowed".to_string())?;
    Ok(())
}

fn validate_capture_evidence(
    session: &LiveSession,
    evidence: &LiveCaptureEvidence,
    kind: LiveCaptureKind,
    channel: LiveChannel,
) -> Result<(), String> {
    if session.id != evidence.session_id {
        return Err("live project changed while the microphone capture was running".to_string());
    }
    if session.evidence_generation != evidence.generation {
        return Err(
            "live measurement evidence changed while the microphone capture was running"
                .to_string(),
        );
    }
    if session.system_mode != evidence.system_mode {
        return Err("speaker-system mode changed during capture".to_string());
    }
    if session.subwoofer_setup != evidence.subwoofer_setup {
        return Err("single-sub hardware settings changed during capture".to_string());
    }
    if session.subwoofer_search != evidence.subwoofer_search {
        return Err("single-sub separated-path search plan changed during capture".to_string());
    }
    if session.locked_input_device_id.as_deref() != Some(evidence.input_device_id.as_str()) {
        return Err("microphone input device changed during capture".to_string());
    }
    if session.locked_input_channel_index != Some(evidence.input_channel_index) {
        return Err("microphone input channel changed during capture".to_string());
    }
    let calibration_sha256 = session
        .calibration
        .as_ref()
        .map(|entry| entry.summary.sha256.as_str())
        .ok_or_else(|| "microphone calibration disappeared during capture".to_string())?;
    if calibration_sha256 != evidence.calibration_sha256 {
        return Err("microphone calibration changed during capture".to_string());
    }
    let sweep_sha256 = session
        .sweeps
        .get(&channel)
        .map(|sweep| sweep.summary.sha256.as_str())
        .ok_or_else(|| format!("{} sweep disappeared during capture", channel.as_str()))?;
    if sweep_sha256 != evidence.sweep_sha256 {
        return Err(format!("{} sweep changed during capture", channel.as_str()));
    }
    match kind {
        LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly | LiveCaptureKind::Baseline
            if evidence.design_sha256.is_some() =>
        {
            return Err(format!(
                "{} capture unexpectedly carries trial-filter evidence",
                kind.as_str()
            ));
        }
        LiveCaptureKind::Verification => {
            let current_design = session.design.as_ref().ok_or_else(|| {
                "trial design disappeared during verification capture".to_string()
            })?;
            if evidence.design_sha256.as_deref() != Some(current_design.evidence_sha256.as_str()) {
                return Err(
                    "verification capture is not bound to the current trial filter".to_string(),
                );
            }
        }
        LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly | LiveCaptureKind::Baseline => {}
    }
    Ok(())
}

fn validate_stored_evidence(
    session: &LiveSession,
    measurement: &StoredMeasurement,
) -> Result<(), String> {
    let evidence = &measurement.evidence;
    if evidence.session_id != session.id {
        return Err("stored measurement belongs to a different live project".to_string());
    }
    if evidence.system_mode != session.system_mode {
        return Err("stored measurement uses a different speaker-system mode".to_string());
    }
    if matches!(
        measurement.summary.kind,
        LiveCaptureKind::Baseline | LiveCaptureKind::Verification
    ) && evidence.subwoofer_setup != session.subwoofer_setup
    {
        return Err("stored measurement uses different single-sub hardware settings".to_string());
    }
    if evidence.subwoofer_search != session.subwoofer_search {
        return Err(
            "stored measurement uses a different single-sub separated-path search plan".to_string(),
        );
    }
    if session.locked_input_device_id.as_deref() != Some(evidence.input_device_id.as_str()) {
        return Err("stored measurement uses a different microphone input device".to_string());
    }
    if session.locked_input_channel_index != Some(evidence.input_channel_index) {
        return Err("stored measurement uses a different microphone input channel".to_string());
    }
    let calibration_sha256 = session
        .calibration
        .as_ref()
        .map(|entry| entry.summary.sha256.as_str())
        .ok_or_else(|| "microphone calibration is absent".to_string())?;
    if evidence.calibration_sha256 != calibration_sha256 {
        return Err("stored measurement uses a different microphone calibration".to_string());
    }
    let sweep_sha256 = session
        .sweeps
        .get(&measurement.summary.channel)
        .map(|sweep| sweep.summary.sha256.as_str())
        .ok_or_else(|| "stored measurement sweep is absent".to_string())?;
    if evidence.sweep_sha256 != sweep_sha256 {
        return Err(format!(
            "stored {} measurement uses a different {} sweep",
            measurement.summary.kind.as_str(),
            measurement.summary.channel.as_str()
        ));
    }
    match measurement.summary.kind {
        LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly | LiveCaptureKind::Baseline
            if evidence.design_sha256.is_some() =>
        {
            Err(format!(
                "stored {} measurement unexpectedly carries trial-filter evidence",
                measurement.summary.kind.as_str()
            ))
        }
        LiveCaptureKind::Verification => {
            let design = session
                .design
                .as_ref()
                .ok_or_else(|| "trial design is absent".to_string())?;
            if design.user_declared_active_at_unix_ms.is_none() {
                return Err(
                    "the user has not declared the exact trial filter active in Roon".to_string(),
                );
            }
            if evidence.design_sha256.as_deref() != Some(design.evidence_sha256.as_str()) {
                return Err(
                    "stored verification measurement belongs to a different trial filter"
                        .to_string(),
                );
            }
            Ok(())
        }
        LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly | LiveCaptureKind::Baseline => {
            Ok(())
        }
    }
}

fn isolated_measurement_response(
    measurement: &StoredMeasurement,
    label: &str,
) -> Result<CombinedResponse, String> {
    if !measurement.summary.accepted {
        return Err(format!(
            "{label} did not pass live measurement quality checks"
        ));
    }
    let response = &measurement.calibrated_frequency_response;
    if response.frequencies_hz.len() != response.magnitude_db.len()
        || response.frequencies_hz.len() != response.phase_rad.len()
    {
        return Err(format!(
            "{label} has inconsistent frequency-response arrays"
        ));
    }
    let indices = response
        .frequencies_hz
        .iter()
        .enumerate()
        .filter_map(|(index, frequency_hz)| {
            (*frequency_hz > 0.0 && *frequency_hz <= 500.0).then_some(index)
        })
        .collect::<Vec<_>>();
    if indices.len() < 3 {
        return Err(format!(
            "{label} does not contain enough 20-500 Hz evidence"
        ));
    }
    Ok(CombinedResponse {
        frequencies_hz: indices
            .iter()
            .map(|index| response.frequencies_hz[*index])
            .collect(),
        magnitude_db: indices
            .iter()
            .map(|index| response.magnitude_db[*index])
            .collect(),
        phase_rad: Some(
            indices
                .iter()
                .map(|index| response.phase_rad[*index])
                .collect(),
        ),
        timing: None,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveProjectDeclaration<'a> {
    project_version: &'static str,
    session_id: &'a str,
    system_mode: LiveSystemMode,
    system_scope: &'static str,
}

impl LiveMeasurementState {
    #[cfg(test)]
    pub fn start_session(&self, base_directory: &Path) -> Result<LiveSessionSummary, String> {
        self.start_session_with_mode(base_directory, LiveSystemMode::Stereo20)
    }

    pub fn start_session_with_mode(
        &self,
        base_directory: &Path,
        system_mode: LiveSystemMode,
    ) -> Result<LiveSessionSummary, String> {
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err("stop the active microphone capture before starting a project".to_string());
        }
        fs::create_dir_all(base_directory).map_err(|error| {
            format!(
                "could not create live-project base directory {}: {error}",
                base_directory.display()
            )
        })?;
        let timestamp = unix_milliseconds()?;
        let mut selected = None;
        for suffix in 0..1_000_u16 {
            let id = format!("live-{timestamp}-{suffix:03}");
            let root = base_directory.join(&id);
            match fs::create_dir(&root) {
                Ok(()) => {
                    selected = Some((id, root));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "could not create live project {}: {error}",
                        root.display()
                    ));
                }
            }
        }
        let (id, root) = selected
            .ok_or_else(|| "could not allocate a unique live project directory".to_string())?;
        for child in ["inputs", "captures", "trial", "final", "snapshots", "rew"] {
            fs::create_dir(root.join(child)).map_err(|error| {
                format!("could not create live project directory `{child}`: {error}")
            })?;
        }
        let system_declaration_path = root.join("project.json");
        let declaration = LiveProjectDeclaration {
            project_version: LIVE_MEASUREMENT_PROJECT_VERSION,
            session_id: &id,
            system_mode,
            system_scope: match system_mode {
                LiveSystemMode::Stereo20 => "stereo-main-path",
                LiveSystemMode::SingleSub21 => "measured-separated-path-single-sub-manual-control",
            },
        };
        write_new_file(
            &system_declaration_path,
            &serde_json::to_vec_pretty(&declaration).map_err(|error| {
                format!("could not serialize live project declaration: {error}")
            })?,
        )?;
        let session = LiveSession {
            id: id.clone(),
            root: root.clone(),
            system_mode,
            system_declaration_path: system_declaration_path.clone(),
            subwoofer_setup: None,
            subwoofer_search: None,
            subwoofer_optimization: None,
            next_artifact_index: 1,
            evidence_generation: 1,
            locked_input_device_id: None,
            locked_input_channel_index: None,
            calibration: None,
            custom_target: None,
            sweeps: BTreeMap::new(),
            measurements: BTreeMap::new(),
            design: None,
            last_export: None,
        };
        *self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())? = Some(session);
        Ok(LiveSessionSummary {
            session_id: id,
            output_directory: root.display().to_string(),
            project_version: LIVE_MEASUREMENT_PROJECT_VERSION.to_string(),
            system_mode,
            system_declaration_path: system_declaration_path.display().to_string(),
        })
    }

    pub fn configure_subwoofer_search(
        &self,
        request: LiveSubwooferSearchRequest,
    ) -> Result<LiveSubwooferSearchSummary, String> {
        if !(2..=12).contains(&request.crossover_hz.len()) {
            return Err(
                "separated-path optimization requires 2 to 12 real crossover settings".to_string(),
            );
        }
        let mut previous_crossover = None;
        for crossover_hz in &request.crossover_hz {
            if !crossover_hz.is_finite() || !(30.0..=200.0).contains(crossover_hz) {
                return Err(
                    "every crossover candidate must be a finite value from 30 to 200 Hz"
                        .to_string(),
                );
            }
            if previous_crossover.is_some_and(|previous| *crossover_hz <= previous) {
                return Err(
                    "crossover candidates must be unique and strictly increasing".to_string(),
                );
            }
            previous_crossover = Some(*crossover_hz);
        }
        if !request.measured_main_delay_ms.is_finite()
            || !(-20.0..=50.0).contains(&request.measured_main_delay_ms)
        {
            return Err(
                "the main delay active during isolated captures must be from -20 to 50 ms"
                    .to_string(),
            );
        }
        if !matches!(request.measured_polarity_degrees, 0 | 180) {
            return Err(
                "the subwoofer polarity active during isolated captures must be 0 or 180 degrees"
                    .to_string(),
            );
        }
        if !request.fixed_sub_level_db.is_finite()
            || !(-30.0..=12.0).contains(&request.fixed_sub_level_db)
        {
            return Err(
                "the subwoofer level active during isolated captures must be from -30 to +12 dB"
                    .to_string(),
            );
        }
        for (label, value) in [
            ("minimum delay", request.delay_minimum_ms),
            ("maximum delay", request.delay_maximum_ms),
            ("delay step", request.delay_step_ms),
        ] {
            if !value.is_finite() {
                return Err(format!("{label} must be finite"));
            }
        }
        if request.delay_minimum_ms < -20.0
            || request.delay_maximum_ms > 50.0
            || request.delay_maximum_ms < request.delay_minimum_ms
        {
            return Err(
                "the delay search must increase within the -20 to 50 ms hardware range".to_string(),
            );
        }
        if !(0.01..=5.0).contains(&request.delay_step_ms) {
            return Err("the delay step must be from 0.01 to 5 ms".to_string());
        }
        // The configured range is the hardware's outer capability; the search
        // itself windows a crossover half-period around the measured sub
        // arrival and snaps candidates to absolute step multiples, so the
        // range no longer has to be an exact multiple of the step. The size
        // bound below is the worst case (full range at this step).
        let delay_steps =
            (request.delay_maximum_ms - request.delay_minimum_ms) / request.delay_step_ms;
        if delay_steps.ceil() as usize + 1 > 1_001 {
            return Err("the delay search is limited to 1001 values".to_string());
        }
        let synthesized_candidate_count = request
            .crossover_hz
            .len()
            .checked_mul(delay_steps.ceil() as usize + 1)
            .and_then(|count| count.checked_mul(2))
            .ok_or_else(|| "the separated-path search size overflowed".to_string())?;
        if synthesized_candidate_count > 10_000 {
            return Err(
                "the crossover, delay, and polarity search is limited to 10000 candidates"
                    .to_string(),
            );
        }

        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err(
                "finish the active microphone capture before changing the subwoofer search plan"
                    .to_string(),
            );
        }
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard.as_mut().ok_or_else(|| {
            "start a live project before configuring subwoofer optimization".to_string()
        })?;
        if session.system_mode != LiveSystemMode::SingleSub21 {
            return Err(format!(
                "subwoofer optimization is unavailable for the `{}` project mode",
                session.system_mode.as_str()
            ));
        }
        let fixed_timing_reference_channel = [LiveChannel::Left, LiveChannel::Right]
            .into_iter()
            .map(|channel| {
                let sweep = session.sweeps.get(&channel).ok_or_else(|| {
                    format!("import the {} measurement sweep first", channel.as_str())
                })?;
                if sweep.summary.timing_marker_count < 2 {
                    return Err(format!(
                        "the {} sweep needs both timing markers for separated-path phase comparison",
                        channel.as_str()
                    ));
                }
                match (
                    sweep.summary.start_marker_channel,
                    sweep.summary.end_marker_channel,
                ) {
                    (Some(ReferenceChannel::Left), Some(ReferenceChannel::Left)) => {
                        Ok(ReferenceChannel::Left)
                    }
                    (Some(ReferenceChannel::Right), Some(ReferenceChannel::Right)) => {
                        Ok(ReferenceChannel::Right)
                    }
                    _ => Err(format!(
                        "the {} sweep must place both timing markers on one fixed L or R speaker",
                        channel.as_str()
                    )),
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        if fixed_timing_reference_channel[0] != fixed_timing_reference_channel[1] {
            return Err(
                "the L and R sweep WAVs must use the same fixed acoustic timing-reference speaker"
                    .to_string(),
            );
        }
        let fixed_timing_reference_channel = fixed_timing_reference_channel[0];
        let sub_sweep_channel = match fixed_timing_reference_channel {
            ReferenceChannel::Left => LiveChannel::Right,
            ReferenceChannel::Right => LiveChannel::Left,
            ReferenceChannel::Mono | ReferenceChannel::IdenticalStereo => {
                unreachable!("fixed L/R marker checked above")
            }
        };
        let candidates = request
            .crossover_hz
            .iter()
            .enumerate()
            .map(|(index, crossover_hz)| LiveSubwooferCrossoverCandidate {
                id: format!("XO{:02}", index + 1),
                crossover_hz: *crossover_hz,
            })
            .collect::<Vec<_>>();
        if let Some(existing) = session.subwoofer_search.as_ref() {
            if existing.candidates == candidates
                && existing.measured_main_delay_ms == request.measured_main_delay_ms
                && existing.measured_polarity_degrees == request.measured_polarity_degrees
                && existing.fixed_sub_level_db == request.fixed_sub_level_db
                && existing.delay_minimum_ms == request.delay_minimum_ms
                && existing.delay_maximum_ms == request.delay_maximum_ms
                && existing.delay_step_ms == request.delay_step_ms
                && existing.fixed_timing_reference_channel == fixed_timing_reference_channel
                && existing.sub_sweep_channel == sub_sweep_channel
            {
                return Ok(existing.clone());
            }
        }
        let artifact_index = session.next_artifact_index;
        session.next_artifact_index = session
            .next_artifact_index
            .checked_add(1)
            .ok_or_else(|| "live artifact counter overflowed".to_string())?;
        let path = session
            .root
            .join("inputs")
            .join(format!("{artifact_index:06}-single-sub-search-plan.json"));
        let summary = LiveSubwooferSearchSummary {
            algorithm_version: LIVE_SUBWOOFER_SEARCH_PLAN_VERSION,
            candidates,
            measured_main_delay_ms: request.measured_main_delay_ms,
            measured_polarity_degrees: request.measured_polarity_degrees,
            fixed_sub_level_db: request.fixed_sub_level_db,
            delay_minimum_ms: request.delay_minimum_ms,
            delay_maximum_ms: request.delay_maximum_ms,
            delay_step_ms: request.delay_step_ms,
            fixed_timing_reference_channel,
            sub_sweep_channel,
            plan_path: path.display().to_string(),
        };
        write_new_file(
            &path,
            &serde_json::to_vec_pretty(&summary)
                .map_err(|error| format!("could not serialize subwoofer search plan: {error}"))?,
        )?;
        session.subwoofer_search = Some(summary.clone());
        session.subwoofer_optimization = None;
        session.subwoofer_setup = None;
        session.measurements.clear();
        session.design = None;
        advance_evidence_generation(session)?;
        Ok(summary)
    }

    pub fn record_subwoofer_setup(
        &self,
        request: LiveSubwooferSetupRequest,
    ) -> Result<LiveSubwooferSetupSummary, String> {
        if !request.crossover_hz.is_finite() || !(30.0..=200.0).contains(&request.crossover_hz) {
            return Err("subwoofer crossover must be a finite value from 30 to 200 Hz".to_string());
        }
        if !request.main_delay_ms.is_finite() || !(-20.0..=50.0).contains(&request.main_delay_ms) {
            return Err("main relative delay must be a finite value from -20 to 50 ms".to_string());
        }
        if !matches!(request.polarity_degrees, 0 | 180) {
            return Err("subwoofer polarity must be either 0 or 180 degrees".to_string());
        }
        if !request.sub_level_db.is_finite() || !(-30.0..=12.0).contains(&request.sub_level_db) {
            return Err("subwoofer level must be a finite value from -30 to +12 dB".to_string());
        }
        if !request.confirmed_on_hardware {
            return Err(
                "confirm that the displayed settings are applied on the real hardware".to_string(),
            );
        }
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err(
                "finish the active microphone capture before changing subwoofer settings"
                    .to_string(),
            );
        }
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard.as_mut().ok_or_else(|| {
            "start a live project before recording subwoofer settings".to_string()
        })?;
        if session.system_mode != LiveSystemMode::SingleSub21 {
            return Err(format!(
                "subwoofer settings are unavailable for the `{}` project mode",
                session.system_mode.as_str()
            ));
        }
        if let Some(search) = session.subwoofer_search.as_ref() {
            let optimization = session.subwoofer_optimization.as_ref().ok_or_else(|| {
                "finish the separated main/sub optimization before recording its hardware settings"
                    .to_string()
            })?;
            let best = &optimization.best;
            let matches_recommendation = (request.crossover_hz - best.crossover_hz).abs() < 1.0e-9
                && (request.main_delay_ms - best.main_delay_ms).abs() < 1.0e-9
                && request.polarity_degrees == best.polarity_degrees
                && (request.sub_level_db - search.fixed_sub_level_db).abs() < 1.0e-9;
            if !matches_recommendation {
                return Err(format!(
                    "apply the measured-path recommendation exactly: {:.1} Hz, {:.3} ms, {} degrees, {:.1} dB",
                    best.crossover_hz,
                    best.main_delay_ms,
                    best.polarity_degrees,
                    search.fixed_sub_level_db,
                ));
            }
        }
        if let Some(existing) = session.subwoofer_setup.as_ref() {
            if existing.crossover_hz == request.crossover_hz
                && existing.main_delay_ms == request.main_delay_ms
                && existing.polarity_degrees == request.polarity_degrees
                && existing.sub_level_db == request.sub_level_db
                && existing.confirmed_on_hardware == request.confirmed_on_hardware
            {
                return Ok(existing.clone());
            }
        }
        let artifact_index = session.next_artifact_index;
        session.next_artifact_index = session
            .next_artifact_index
            .checked_add(1)
            .ok_or_else(|| "live artifact counter overflowed".to_string())?;
        let path = session
            .root
            .join("inputs")
            .join(format!("{artifact_index:06}-single-sub-settings.json"));
        let summary = LiveSubwooferSetupSummary {
            algorithm_version: LIVE_SUBWOOFER_SETUP_VERSION,
            crossover_hz: request.crossover_hz,
            main_delay_ms: request.main_delay_ms,
            polarity_degrees: request.polarity_degrees,
            sub_level_db: request.sub_level_db,
            confirmed_on_hardware: request.confirmed_on_hardware,
            settings_path: path.display().to_string(),
        };
        write_new_file(
            &path,
            &serde_json::to_vec_pretty(&summary)
                .map_err(|error| format!("could not serialize subwoofer settings: {error}"))?,
        )?;
        session.subwoofer_setup = Some(summary.clone());
        session.measurements.retain(|(kind, _, _), _| {
            matches!(
                kind,
                LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly
            )
        });
        session.design = None;
        advance_evidence_generation(session)?;
        Ok(summary)
    }

    pub fn optimize_subwoofer_paths(&self) -> Result<LiveSubwooferOptimizationSummary, String> {
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err(
                "finish the active microphone capture before optimizing subwoofer settings"
                    .to_string(),
            );
        }
        let (session_id, evidence_generation, search, paths) = {
            let guard = self
                .session
                .lock()
                .map_err(|_| "live session state lock was poisoned".to_string())?;
            let session = guard.as_ref().ok_or_else(|| {
                "start a live project before optimizing subwoofer settings".to_string()
            })?;
            if session.system_mode != LiveSystemMode::SingleSub21 {
                return Err(
                    "separated main/sub optimization requires a single-sub 2.1 project".to_string(),
                );
            }
            let search = session
                .subwoofer_search
                .clone()
                .ok_or_else(|| "save a separated-path crossover search plan first".to_string())?;
            let mut paths = Vec::with_capacity(search.candidates.len());
            let mut marker_levels: Vec<(String, Option<f64>)> = Vec::new();
            for candidate in &search.candidates {
                let left_key = (
                    LiveCaptureKind::SubMainOnly,
                    candidate.id.clone(),
                    LiveChannel::Left,
                );
                let right_key = (
                    LiveCaptureKind::SubMainOnly,
                    candidate.id.clone(),
                    LiveChannel::Right,
                );
                let sub_key = (
                    LiveCaptureKind::SubOnly,
                    candidate.id.clone(),
                    search.sub_sweep_channel,
                );
                let left = session.measurements.get(&left_key).ok_or_else(|| {
                    format!(
                        "capture the {} Hz left main-only path before optimization",
                        candidate.crossover_hz
                    )
                })?;
                let right = session.measurements.get(&right_key).ok_or_else(|| {
                    format!(
                        "capture the {} Hz right main-only path before optimization",
                        candidate.crossover_hz
                    )
                })?;
                let sub = session.measurements.get(&sub_key).ok_or_else(|| {
                    format!(
                        "capture the {} Hz sub-only path before optimization",
                        candidate.crossover_hz
                    )
                })?;
                for measurement in [left, right, sub] {
                    validate_stored_evidence(session, measurement)?;
                }
                for (role, measurement) in [
                    ("left main-only", left),
                    ("right main-only", right),
                    ("sub-only", sub),
                ] {
                    marker_levels.push((
                        format!("the {} Hz {role} capture", candidate.crossover_hz),
                        measurement.summary.start_marker_rms_dbfs,
                    ));
                }
                paths.push(SeparatedCrossoverPaths {
                    id: candidate.id.clone(),
                    crossover_hz: candidate.crossover_hz,
                    left_main: isolated_measurement_response(left, "left main-only capture")?,
                    right_main: isolated_measurement_response(right, "right main-only capture")?,
                    sub: isolated_measurement_response(sub, "sub-only capture")?,
                });
            }
            validate_isolated_marker_levels(&marker_levels)?;
            (
                session.id.clone(),
                session.evidence_generation,
                search,
                paths,
            )
        };
        let measured_polarity = match search.measured_polarity_degrees {
            0 => Polarity::Normal,
            180 => Polarity::Inverted,
            _ => {
                return Err(
                    "stored isolated-capture polarity is neither 0 nor 180 degrees".to_string(),
                )
            }
        };
        let report = optimize_separated_paths(
            &paths,
            &SeparatedPathOptimizationConfig {
                measured_main_delay_ms: search.measured_main_delay_ms,
                measured_polarity,
                delay_minimum_ms: search.delay_minimum_ms,
                delay_maximum_ms: search.delay_maximum_ms,
                delay_step_ms: search.delay_step_ms,
                ranking: RankingConfig::default(),
            },
        )
        .map_err(|error| format!("separated main/sub optimization failed: {error}"))?;
        let rankings = report
            .ranking
            .rankings
            .iter()
            .take(20)
            .map(|candidate| {
                let main_delay_ms = candidate.settings.main_delay_ms.ok_or_else(|| {
                    format!("ranked candidate `{}` omitted its main delay", candidate.id)
                })?;
                let polarity_degrees = match candidate.settings.polarity {
                    Some(Polarity::Normal) => 0,
                    Some(Polarity::Inverted) => 180,
                    None => {
                        return Err(format!(
                            "ranked candidate `{}` omitted its sub polarity",
                            candidate.id
                        ))
                    }
                };
                Ok(LiveSubwooferRankedSetting {
                    rank: candidate.rank,
                    crossover_hz: candidate.settings.crossover_hz,
                    main_delay_ms,
                    polarity_degrees,
                    total_score: candidate.metrics.total_score,
                    deficit_rms_db: candidate.metrics.deficit_rms_db,
                    deficit_p95_db: candidate.metrics.deficit_p95_db,
                    worst_deficit_db: candidate.metrics.deficit_worst_db,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let best = rankings
            .first()
            .cloned()
            .ok_or_else(|| "separated-path optimization produced no ranking".to_string())?;

        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "live project was closed during optimization".to_string())?;
        if session.id != session_id
            || session.evidence_generation != evidence_generation
            || session.subwoofer_search.as_ref() != Some(&search)
        {
            return Err(
                "live measurement evidence changed during subwoofer optimization; run it again"
                    .to_string(),
            );
        }
        let artifact_index = session.next_artifact_index;
        session.next_artifact_index = session
            .next_artifact_index
            .checked_add(1)
            .ok_or_else(|| "live artifact counter overflowed".to_string())?;
        let path = session
            .root
            .join("inputs")
            .join(format!("{artifact_index:06}-single-sub-optimization.json"));
        let summary = LiveSubwooferOptimizationSummary {
            algorithm_version: SEPARATED_PATH_OPTIMIZATION_VERSION,
            synthesized_candidate_count: report.synthesized_candidate_count,
            best,
            rankings,
            scoring_lower_hz: report.ranking.band.lower_hz,
            scoring_upper_hz: report.ranking.band.upper_hz,
            fixed_sub_level_db: search.fixed_sub_level_db,
            needs_combined_confirmation: report.ranking.needs_confirmation,
            arrival_estimates: report
                .arrival_estimates
                .iter()
                .map(|estimate| LiveSubwooferArrivalEstimate {
                    crossover_hz: estimate.crossover_hz,
                    center_ms: estimate.center_ms,
                    left_right_spread_ms: estimate.left_right_spread_ms,
                    window_low_ms: estimate.window_low_ms,
                    window_high_ms: estimate.window_high_ms,
                    range_limited: estimate.range_limited,
                })
                .collect(),
            sub_level_advisory: report.sub_level_advisory.as_ref().map(|advisory| {
                LiveSubwooferSubLevelAdvisory {
                    best_gain_db: advisory.best_gain_db,
                    deficit_rms_at_best_db: advisory.deficit_rms_at_best_db,
                    deficit_rms_at_zero_db: advisory.deficit_rms_at_zero_db,
                }
            }),
            warnings: report.ranking.warnings,
            report_path: path.display().to_string(),
        };
        write_new_file(
            &path,
            &serde_json::to_vec_pretty(&summary)
                .map_err(|error| format!("could not serialize subwoofer optimization: {error}"))?,
        )?;
        session.subwoofer_optimization = Some(summary.clone());
        session.subwoofer_setup = None;
        session.design = None;
        advance_evidence_generation(session)?;
        Ok(summary)
    }

    pub fn import_target(
        &self,
        file_name: &str,
        contents: &str,
    ) -> Result<TargetImportSummary, String> {
        if contents.len() > MAX_LIVE_TARGET_TEXT_BYTES {
            return Err(format!(
                "target TXT exceeds the {} MiB safety limit",
                MAX_LIVE_TARGET_TEXT_BYTES / (1024 * 1024)
            ));
        }
        let curve = parse_target_txt(contents).map_err(|error| error.to_string())?;
        let first = curve
            .knots()
            .first()
            .ok_or_else(|| "target TXT contains no points".to_string())?;
        let last = curve
            .knots()
            .last()
            .ok_or_else(|| "target TXT contains no points".to_string())?;
        let sha256 = sha256_hex(contents.as_bytes());

        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err(
                "stop the active microphone capture before changing the target".to_string(),
            );
        }
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "start a live project before importing a target".to_string())?;
        if let Some(existing) = session.custom_target.as_ref() {
            if existing.summary.sha256 == sha256 {
                return Ok(existing.summary.clone());
            }
        }
        let artifact_index = session.next_artifact_index;
        session.next_artifact_index = session
            .next_artifact_index
            .checked_add(1)
            .ok_or_else(|| "live artifact counter overflowed".to_string())?;
        let path = session.root.join("inputs").join(format!(
            "{artifact_index:06}-target-{}",
            safe_file_label(file_name)
        ));
        write_new_file(&path, contents.as_bytes())?;
        let summary = TargetImportSummary {
            file_name: safe_file_label(file_name),
            sha256,
            parser_version: TARGET_TXT_PARSER_VERSION.to_string(),
            point_count: curve.knots().len(),
            minimum_frequency_hz: first.frequency_hz,
            maximum_frequency_hz: last.frequency_hz,
            correction_band_covered: first.frequency_hz <= 20.0 && last.frequency_hz >= 500.0,
            alignment_lower_hz: 200.0,
            alignment_upper_hz: 500.0,
            stored_path: path.display().to_string(),
        };
        session.custom_target = Some(TargetEntry {
            summary: summary.clone(),
            curve,
        });
        session
            .measurements
            .retain(|(kind, _, _), _| *kind != LiveCaptureKind::Verification);
        session.design = None;
        advance_evidence_generation(session)?;
        Ok(summary)
    }

    pub fn import_calibration(
        &self,
        file_name: &str,
        contents: &str,
    ) -> Result<CalibrationImportSummary, String> {
        if contents.len() > MAX_CALIBRATION_TEXT_BYTES {
            return Err(format!(
                "microphone calibration exceeds the {} MiB safety limit",
                MAX_CALIBRATION_TEXT_BYTES / (1024 * 1024)
            ));
        }
        let profile = parse_umik_calibration(contents).map_err(|error| error.to_string())?;
        let range = profile.frequency_range_hz();
        let summary = CalibrationImportSummary {
            file_name: safe_file_label(file_name),
            sha256: sha256_hex(contents.as_bytes()),
            parser_version: UMIK_CALIBRATION_PARSER_VERSION.to_string(),
            serial_number: profile.serial_number.clone(),
            sensitivity_factor_db: profile.sensitivity_factor_db,
            point_count: profile.points.len(),
            minimum_frequency_hz: range[0],
            maximum_frequency_hz: range[1],
            correction_band_covered: profile.covers(REQUIRED_CALIBRATION_BAND_HZ),
        };
        if !summary.correction_band_covered {
            return Err(format!(
                "calibration covers {:.1}-{:.1} Hz but live correction requires 20-20000 Hz",
                range[0], range[1]
            ));
        }
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err(
                "stop the active microphone capture before changing calibration".to_string(),
            );
        }
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "start a live project before importing calibration".to_string())?;
        if let Some(existing) = session.calibration.as_ref() {
            if existing.summary.sha256 == summary.sha256 {
                return Ok(existing.summary.clone());
            }
        }
        let artifact_index = session.next_artifact_index;
        session.next_artifact_index = session
            .next_artifact_index
            .checked_add(1)
            .ok_or_else(|| "live artifact counter overflowed".to_string())?;
        let path = session.root.join("inputs").join(format!(
            "{artifact_index:06}-calibration-{}",
            summary.file_name
        ));
        write_new_file(&path, contents.as_bytes())?;
        session.calibration = Some(CalibrationEntry {
            summary: summary.clone(),
            profile,
        });
        session.measurements.clear();
        if session.subwoofer_search.is_some() {
            session.subwoofer_optimization = None;
            session.subwoofer_setup = None;
        }
        session.design = None;
        advance_evidence_generation(session)?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eqforbeginner_dsp_core::analysis::frequency_response;
    use eqforbeginner_dsp_core::fixture::SyntheticRoomFixture;
    use hound::{SampleFormat, WavSpec, WavWriter};
    use std::f64::consts::TAU;
    use std::io::Cursor;
    use tempfile::tempdir;

    fn test_sweep_wav() -> Vec<u8> {
        let spec = WavSpec {
            channels: 1,
            sample_rate: PROJECT_SAMPLE_RATE_HZ,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).unwrap();
            let silence = PROJECT_SAMPLE_RATE_HZ as usize / 2;
            for _ in 0..silence {
                writer.write_sample(0.0_f32).unwrap();
            }
            let length = PROJECT_SAMPLE_RATE_HZ as usize * 2;
            let duration = length as f64 / f64::from(PROJECT_SAMPLE_RATE_HZ);
            let ratio = 20_000.0_f64 / 20.0;
            for index in 0..length {
                let time = index as f64 / f64::from(PROJECT_SAMPLE_RATE_HZ);
                let phase =
                    TAU * 20.0 * duration / ratio.ln() * (ratio.powf(time / duration) - 1.0);
                let fade = ((index as f64 / 512.0).min(1.0))
                    * (((length - 1 - index) as f64 / 512.0).min(1.0));
                writer
                    .write_sample((0.25 * fade * phase.sin()) as f32)
                    .unwrap();
            }
            for _ in 0..silence {
                writer.write_sample(0.0_f32).unwrap();
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    fn capture_from_samples(
        samples: Vec<f32>,
        automatic_completion_detected: bool,
    ) -> MonoInputCapture {
        let captured_samples = samples.len();
        let peak = samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        MonoInputCapture {
            host: "synthetic".into(),
            device_id: "synthetic::input".into(),
            device_name: Some("synthetic input".into()),
            sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
            channels: 1,
            source_channels: 1,
            input_channel_index: 0,
            sample_format: "f32".into(),
            requested_duration_ms: captured_samples as u64 * 1_000 / 48_000,
            requested_samples: captured_samples,
            maximum_samples: captured_samples,
            elapsed_ms: captured_samples as u64 * 1_000 / 48_000,
            samples,
            captured_samples,
            capture_complete: true,
            automatic_completion_detected,
            timed_out: false,
            cancelled: false,
            peak_linear: peak,
            peak_dbfs: Some(amplitude_dbfs(f64::from(peak)) as f32),
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
        }
    }

    fn synthetic_capture(reference: &[f64], system_ir: &[f64]) -> MonoInputCapture {
        let response = fft_convolve(reference, system_ir).unwrap();
        let mut samples = vec![0.0_f32; 4_800];
        samples.extend(response.iter().map(|sample| *sample as f32));
        // Keep more post-roll than the default 32,768-sample deconvolution
        // tail even when the synthetic system IR is a single tap.
        samples.resize(samples.len() + 40_000, 0.0);
        capture_from_samples(samples, false)
    }

    fn marker_wrapped_capture_samples(
        sweep: &LiveSweepReference,
        offset_samples: usize,
        gain: f64,
    ) -> Vec<f32> {
        let length =
            offset_samples + sweep.source_frame_count + CAPTURE_DEADLINE_GRACE_SAMPLES + 4_800;
        let mut samples = vec![0.0_f32; length];
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = (((index as u64)
                .wrapping_mul(1_103_515_245)
                .wrapping_add(12_345)
                & 0xffff) as f64
                / 65_535.0
                * 2.0
                - 1.0) as f32
                * 0.000_01;
        }
        for marker in &sweep.timing_markers {
            let start = offset_samples + marker.source_start_sample;
            for (destination, &source) in samples[start..start + marker.samples.len()]
                .iter_mut()
                .zip(&marker.samples)
            {
                *destination += (source * gain) as f32;
            }
        }
        let sweep_start = offset_samples + sweep.measurement_source_start_sample;
        for (destination, &source) in samples[sweep_start..sweep_start + sweep.samples.len()]
            .iter_mut()
            .zip(&sweep.samples)
        {
            *destination += (source * gain) as f32;
        }
        samples
    }

    /// Physically shaped isolated path for the separated-path adapter test:
    /// the main rolls off 24 dB/oct below the crossover and the sub 24 dB/oct
    /// above it, so the 200-500 Hz anchor band is main-dominated exactly as in
    /// a real system and the anchor normalization cannot erase the difference
    /// the test needs to detect.
    fn shaped_isolated_test_response(
        crossover_hz: f64,
        is_sub: bool,
        delay_ms: f64,
        inverted: bool,
    ) -> eqforbeginner_dsp_core::analysis::FrequencyResponse {
        let frequencies_hz = (20..=500).map(f64::from).collect::<Vec<_>>();
        let magnitude_db = frequencies_hz
            .iter()
            .map(|frequency| {
                if is_sub {
                    -24.0 * (frequency / crossover_hz).log2().max(0.0)
                } else {
                    -24.0 * (crossover_hz / frequency).log2().max(0.0)
                }
            })
            .collect::<Vec<_>>();
        let phase_rad = frequencies_hz
            .iter()
            .map(|frequency_hz| {
                -TAU * frequency_hz * delay_ms / 1_000.0 + if inverted { TAU / 2.0 } else { 0.0 }
            })
            .collect::<Vec<_>>();
        eqforbeginner_dsp_core::analysis::FrequencyResponse {
            sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
            fft_size: 96_000,
            magnitude_db,
            phase_rad,
            frequencies_hz,
        }
    }

    #[test]
    fn isolated_marker_levels_gate_volume_drift_and_missing_evidence() {
        let same = vec![
            ("the 80 Hz left main-only capture".to_string(), Some(-30.02)),
            (
                "the 80 Hz right main-only capture".to_string(),
                Some(-29.88),
            ),
            ("the 80 Hz sub-only capture".to_string(), Some(-30.11)),
        ];
        validate_isolated_marker_levels(&same).unwrap();

        let mut drifted = same.clone();
        drifted[2].1 = Some(-31.2);
        let error = validate_isolated_marker_levels(&drifted).unwrap_err();
        assert!(error.contains("volume changed"), "{error}");

        let mut missing = same;
        missing[1].1 = None;
        let error = validate_isolated_marker_levels(&missing).unwrap_err();
        assert!(error.contains("marker-level recording"), "{error}");
    }

    #[test]
    fn isolated_leakage_gate_rejects_unmuted_paths_and_passes_clean_ones() {
        let search = LiveSubwooferSearchSummary {
            algorithm_version: LIVE_SUBWOOFER_SEARCH_PLAN_VERSION,
            candidates: vec![LiveSubwooferCrossoverCandidate {
                id: "XO01".into(),
                crossover_hz: 80.0,
            }],
            measured_main_delay_ms: 0.0,
            measured_polarity_degrees: 0,
            fixed_sub_level_db: 0.0,
            delay_minimum_ms: 0.0,
            delay_maximum_ms: 4.0,
            delay_step_ms: 0.5,
            fixed_timing_reference_channel: ReferenceChannel::Right,
            sub_sweep_channel: LiveChannel::Left,
            plan_path: String::new(),
        };
        let response =
            |shape: &dyn Fn(f64) -> f64| eqforbeginner_dsp_core::analysis::FrequencyResponse {
                sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
                fft_size: 96_000,
                frequencies_hz: (20..=500).map(f64::from).collect(),
                magnitude_db: (20..=500).map(|f| shape(f64::from(f))).collect(),
                phase_rad: vec![0.0; 481],
            };
        // Synthetic capture: 0.5 s of room before the sweep, 2 s of sweep.
        // `tone_hz` plays during the sweep span; band-limited noise (the same
        // tone at `noise_amplitude`) runs through the whole capture including
        // the pre-sweep region, which is what stationary ambient rumble looks
        // like to the band-SNR measurement.
        const SWEEP_START: f64 = 24_000.0;
        const SWEEP_END: f64 = 120_000.0;
        let capture = |tone_hz: f64, signal_amplitude: f64, noise_amplitude: f64| -> Vec<f64> {
            (0..SWEEP_END as usize)
                .map(|index| {
                    let time = index as f64 / 48_000.0;
                    let tone = (std::f64::consts::TAU * tone_hz * time).sin();
                    let in_sweep = index as f64 >= SWEEP_START;
                    noise_amplitude * tone
                        + if in_sweep {
                            signal_amplitude * tone
                        } else {
                            0.0
                        }
                })
                .collect()
        };
        let leakage_code = |finding: Option<IsolatedLeakageFinding>| match finding {
            Some(IsolatedLeakageFinding::Leakage(code)) => code,
            Some(IsolatedLeakageFinding::NoiseLimited(code)) => {
                panic!("expected a hard rejection, got NoiseLimited({code})")
            }
            None => panic!("expected a hard rejection, got None"),
        };

        // A real low-passed sub: flat to the crossover, -24 dB/oct above.
        let clean_sub = response(&|f| -24.0 * (f / 80.0_f64).log2().max(0.0));
        assert!(isolated_path_leakage_issue(
            LiveCaptureKind::SubOnly,
            "XO01",
            Some(&search),
            &clean_sub,
            &capture(300.0, 0.05, 1.0e-6),
            SWEEP_START,
            SWEEP_END,
        )
        .is_none());
        // Mains left playing during the sub-only sweep: flat everywhere, and
        // the high band is far above the quiet room - a real acoustic path.
        let leaking_sub = response(&|_| 0.0);
        let issue = leakage_code(isolated_path_leakage_issue(
            LiveCaptureKind::SubOnly,
            "XO01",
            Some(&search),
            &leaking_sub,
            &capture(300.0, 0.05, 1.0e-6),
            SWEEP_START,
            SWEEP_END,
        ));
        assert!(issue.contains("sub_only_high_band_leakage"), "{issue}");
        // A bass-managed main: high-passed below the crossover.
        let clean_main = response(&|f| -24.0 * (80.0_f64 / f).log2().max(0.0));
        assert!(isolated_path_leakage_issue(
            LiveCaptureKind::SubMainOnly,
            "XO01",
            Some(&search),
            &clean_main,
            &capture(30.0, 0.05, 1.0e-6),
            SWEEP_START,
            SWEEP_END,
        )
        .is_none());
        // The sub stayed live during the main-only sweep: full sub-band level
        // in the response and a sub band far above the room noise.
        let leaking_main = response(&|_| 0.0);
        let issue = leakage_code(isolated_path_leakage_issue(
            LiveCaptureKind::SubMainOnly,
            "XO01",
            Some(&search),
            &leaking_main,
            &capture(30.0, 0.05, 1.0e-6),
            SWEEP_START,
            SWEEP_END,
        ));
        assert!(issue.contains("main_only_sub_band_leakage"), "{issue}");
        // Non-isolated kinds and unknown positions are out of scope.
        assert!(isolated_path_leakage_issue(
            LiveCaptureKind::Baseline,
            "P0",
            Some(&search),
            &leaking_main,
            &capture(30.0, 0.05, 1.0e-6),
            SWEEP_START,
            SWEEP_END,
        )
        .is_none());
        assert!(isolated_path_leakage_issue(
            LiveCaptureKind::SubOnly,
            "XO99",
            Some(&search),
            &leaking_sub,
            &capture(300.0, 0.05, 1.0e-6),
            SWEEP_START,
            SWEEP_END,
        )
        .is_none());

        // The first real v5 session's failure mode: the sub band reads at
        // passband level in the FR, but the capture's sub band is barely above
        // the pre-sweep ambient noise (measured 9.9-10.9 dB on the real
        // captures with the sub physically off, versus 32.1-33.1 dB with the
        // sub genuinely playing). The capture is admitted and the finding
        // becomes a diagnostic instead of a rejection.
        let noisy = capture(30.0, 0.03, 0.02); // ~8 dB band SNR
        let finding = isolated_path_leakage_issue(
            LiveCaptureKind::SubMainOnly,
            "XO01",
            Some(&search),
            &leaking_main,
            &noisy,
            SWEEP_START,
            SWEEP_END,
        );
        match finding {
            Some(IsolatedLeakageFinding::NoiseLimited(code)) => {
                assert!(code.contains("main_only_sub_band_noise_limited"), "{code}");
                assert!(code.contains("band_snr:"), "{code}");
            }
            Some(IsolatedLeakageFinding::Leakage(code)) => {
                panic!("a noise-dominated band must downgrade to a diagnostic, got Leakage({code})")
            }
            None => panic!("a noise-dominated band must downgrade to a diagnostic, got None"),
        }
    }

    #[test]
    fn applied_scale_fit_flags_double_unapplied_and_inverted_filters() {
        let frequencies: Vec<f64> = (20..=650).map(f64::from).collect();
        let raw: Vec<f64> = frequencies
            .iter()
            .map(|f| 4.0 * (-0.5 * ((f / 60.0_f64).log2() / 0.4).powi(2)).exp())
            .collect();
        let predicted = vec![0.0; frequencies.len()];
        let correction_applied_twice: Vec<f64> = raw.iter().map(|value| -value).collect();
        let exact =
            fitted_applied_correction_scale(&frequencies, &raw, &predicted, &predicted).unwrap();
        assert!((exact - 1.0).abs() < 1.0e-9, "{exact}");
        let doubled = fitted_applied_correction_scale(
            &frequencies,
            &raw,
            &predicted,
            &correction_applied_twice,
        )
        .unwrap();
        assert!((doubled - 2.0).abs() < 1.0e-9, "{doubled}");
        let unapplied =
            fitted_applied_correction_scale(&frequencies, &raw, &predicted, &raw).unwrap();
        assert!(unapplied.abs() < 1.0e-9, "{unapplied}");
        let inverted_curve: Vec<f64> = raw.iter().map(|value| 2.0 * value).collect();
        let inverted =
            fitted_applied_correction_scale(&frequencies, &raw, &predicted, &inverted_curve)
                .unwrap();
        assert!((inverted + 1.0).abs() < 1.0e-9, "{inverted}");
        // A flat room has no correction to scale.
        let flat = vec![0.0; frequencies.len()];
        assert!(fitted_applied_correction_scale(&frequencies, &flat, &flat, &flat).is_none());
    }

    fn sparse_room_response(input: &[f32]) -> Vec<f32> {
        let taps = [(137_usize, 0.78_f64), (431, 0.17), (1_217, -0.06)];
        let mut output = vec![0.0_f64; input.len() + taps.last().unwrap().0 + 1];
        for (index, &sample) in input.iter().enumerate() {
            for &(delay, gain) in &taps {
                output[index + delay] += f64::from(sample) * gain;
            }
        }
        output.into_iter().map(|sample| sample as f32).collect()
    }

    fn low_level_reverberant_room_response(input: &[f32]) -> Vec<f32> {
        let mut impulse = vec![0.0; 9_601];
        impulse[137] = 0.08;
        for echo in 0..48 {
            let delay = 241 + echo * 193;
            let sign = if echo % 3 == 0 { -1.0 } else { 1.0 };
            impulse[delay] = sign * 0.065 * (-0.020 * echo as f64).exp();
        }
        let input = input
            .iter()
            .map(|sample| f64::from(*sample))
            .collect::<Vec<_>>();
        let mut output = fft_convolve(&input, &impulse).unwrap();
        for (index, sample) in output.iter_mut().enumerate() {
            let noise = ((index as u64)
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407)
                >> 40) as f64
                / ((1_u64 << 24) - 1) as f64
                * 2.0
                - 1.0;
            *sample += noise * 0.000_12;
        }
        output.into_iter().map(|sample| sample as f32).collect()
    }

    #[test]
    fn longest_region_selects_the_measurement_sweep_between_markers() {
        let mut samples = vec![0.0_f32; 48_000 * 4];
        for sample in &mut samples[10_000..20_000] {
            *sample = 0.1;
        }
        for sample in &mut samples[40_000..140_000] {
            *sample = 0.2;
        }
        for sample in &mut samples[160_000..170_000] {
            *sample = 0.1;
        }
        let (start, end) = longest_active_region(&samples, 48_000).unwrap();
        assert_eq!(start, 40_000 - 960);
        assert_eq!(end, 140_000 + 960);
    }

    #[test]
    fn a_lone_after_marker_is_not_mislabeled_as_the_start_marker() {
        let mut left = vec![0.0_f32; 48_000 * 4];
        let mut right = vec![0.0_f32; 48_000 * 4];
        for sample in &mut left[48_000..96_000] {
            *sample = 0.2;
        }
        for sample in &mut right[144_000..153_600] {
            *sample = 0.1;
        }

        let markers = timing_markers_from_channels(&[left, right], (48_000, 96_000), 48_000);

        assert_eq!(markers.len(), 1);
        assert!(!markers[0].is_start_marker);
        assert_eq!(markers[0].source_channel, ReferenceChannel::Right);
    }

    #[test]
    fn complete_room_biased_marker_is_retained_only_for_pair_validation() {
        let complete = WirelessSweepRecognition::LikelyFalsePositive(
            eqforbeginner_dsp_core::wireless_sweep::WirelessSweepLikelyFalsePositive {
                reason: WirelessSweepRejectionReason::ClockDriftOutOfRange,
                candidate_start_sample: 95_812.0,
                start_absolute_correlation: 0.372,
                matched_segment_count: 5,
                required_segment_count: 3,
                estimated_clock_drift_ppm: Some(-39_985.0),
                timing_fit_rms_samples: Some(145.5),
            },
        );
        let candidates = recognition_marker_candidates(complete);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].capture_start_sample, 95_812.0);

        let incomplete = WirelessSweepRecognition::LikelyFalsePositive(
            eqforbeginner_dsp_core::wireless_sweep::WirelessSweepLikelyFalsePositive {
                reason: WirelessSweepRejectionReason::ClockDriftOutOfRange,
                candidate_start_sample: 95_812.0,
                start_absolute_correlation: 0.372,
                matched_segment_count: 4,
                required_segment_count: 3,
                estimated_clock_drift_ppm: Some(-39_985.0),
                timing_fit_rms_samples: Some(145.5),
            },
        );
        assert!(recognition_marker_candidates(incomplete).is_empty());

        let inconsistent = WirelessSweepRecognition::LikelyFalsePositive(
            eqforbeginner_dsp_core::wireless_sweep::WirelessSweepLikelyFalsePositive {
                reason: WirelessSweepRejectionReason::InconsistentSegments,
                candidate_start_sample: 95_812.0,
                start_absolute_correlation: 0.372,
                matched_segment_count: 5,
                required_segment_count: 3,
                estimated_clock_drift_ppm: None,
                timing_fit_rms_samples: None,
            },
        );
        assert!(recognition_marker_candidates(inconsistent).is_empty());
    }

    #[test]
    fn p0_repeat_gate_separates_level_shift_from_response_shape() {
        let frequencies = (20..=500).map(f64::from).collect::<Vec<_>>();
        let initial = frequencies
            .iter()
            .map(|frequency| -20.0 * (frequency / 100.0).log10())
            .collect::<Vec<_>>();
        let stable = initial
            .iter()
            .enumerate()
            .map(|(index, value)| value + 0.2 + 0.1 * (index as f64 / 20.0).sin())
            .collect::<Vec<_>>();
        validate_p0_repeat_response(&frequencies, &initial, &stable, LiveChannel::Left).unwrap();

        let level_changed = initial.iter().map(|value| value + 1.2).collect::<Vec<_>>();
        assert!(validate_p0_repeat_response(
            &frequencies,
            &initial,
            &level_changed,
            LiveChannel::Left,
        )
        .unwrap_err()
        .contains("level shift"));

        // Returning a microphone stand to the center after six surrounding
        // measurements naturally changes modal detail. This must remain a
        // useful second center sample rather than demand jig-level placement.
        let realistically_repositioned = initial
            .iter()
            .enumerate()
            .map(|(index, value)| value + 6.8 * (index as f64 / 12.0).sin())
            .collect::<Vec<_>>();
        validate_p0_repeat_response(
            &frequencies,
            &initial,
            &realistically_repositioned,
            LiveChannel::Right,
        )
        .unwrap();

        let shape_changed = initial
            .iter()
            .enumerate()
            .map(|(index, value)| value + 10.0 * (index as f64 / 12.0).sin())
            .collect::<Vec<_>>();
        assert!(validate_p0_repeat_response(
            &frequencies,
            &initial,
            &shape_changed,
            LiveChannel::Right,
        )
        .unwrap_err()
        .contains("response-shape"));
    }

    #[test]
    fn bundled_umik_calibration_imports_through_the_live_session() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../testdata/umik-quoted-metadata-90deg.txt");
        let contents = fs::read_to_string(fixture_path).unwrap();

        let summary = state
            .import_calibration("umik-quoted-metadata-90deg.txt", &contents)
            .unwrap();

        assert_eq!(summary.serial_number.as_deref(), Some("0000000"));
        assert_eq!(summary.sensitivity_factor_db, Some(-2.434));
        assert!(summary.point_count > 600);
        assert!(summary.correction_band_covered);
    }

    #[test]
    fn single_sub_session_persists_settings_and_gates_baseline_capture() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        let session = state
            .start_session_with_mode(temporary.path(), LiveSystemMode::SingleSub21)
            .unwrap();
        assert_eq!(session.system_mode, LiveSystemMode::SingleSub21);
        assert!(Path::new(&session.system_declaration_path).is_file());
        let declaration: serde_json::Value =
            serde_json::from_slice(&fs::read(&session.system_declaration_path).unwrap()).unwrap();
        assert_eq!(declaration["systemMode"], "single_sub_2_1");
        assert_eq!(
            serde_json::from_str::<LiveSystemMode>("\"stereo_2_0\"").unwrap(),
            LiveSystemMode::Stereo20
        );
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let wav = test_sweep_wav();
        state.import_sweep(LiveChannel::Left, &wav).unwrap();
        let error = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap_err();
        assert!(error.contains("crossover, delay, polarity, and sub level"));

        let setup = state
            .record_subwoofer_setup(LiveSubwooferSetupRequest {
                crossover_hz: 90.0,
                main_delay_ms: 0.83,
                polarity_degrees: 180,
                sub_level_db: 0.0,
                confirmed_on_hardware: true,
            })
            .unwrap();
        assert_eq!(setup.algorithm_version, LIVE_SUBWOOFER_SETUP_VERSION);
        assert!(Path::new(&setup.settings_path).is_file());
        let (_, _, _, _, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        assert_eq!(evidence.system_mode, LiveSystemMode::SingleSub21);
        assert_eq!(evidence.subwoofer_setup, Some(setup));
        state.finish_capture();

        let changed = state
            .record_subwoofer_setup(LiveSubwooferSetupRequest {
                crossover_hz: 80.0,
                main_delay_ms: 0.5,
                polarity_degrees: 0,
                sub_level_db: -1.0,
                confirmed_on_hardware: true,
            })
            .unwrap();
        let guard = state.session.lock().unwrap();
        let active_session = guard.as_ref().unwrap();
        assert_eq!(active_session.subwoofer_setup, Some(changed));
        assert!(validate_capture_evidence(
            active_session,
            &evidence,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
        )
        .unwrap_err()
        .contains("evidence changed"));
    }

    #[test]
    fn stereo_session_rejects_subwoofer_settings() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        let error = state
            .record_subwoofer_setup(LiveSubwooferSetupRequest {
                crossover_hz: 80.0,
                main_delay_ms: 0.0,
                polarity_degrees: 0,
                sub_level_db: 0.0,
                confirmed_on_hardware: true,
            })
            .unwrap_err();
        assert!(error.contains("stereo_2_0"));
    }

    /// The retained impulse must still hold the *right* half-window after the
    /// peak: eight cycles of the lowest corrected frequency. The left half is a
    /// compile-time invariant next to the constant itself.
    #[test]
    fn the_retained_impulse_still_covers_the_window_after_the_peak() {
        let right_half = SweepDeconvolutionConfig::default().impulse_length_samples
            - MARKER_REFERENCED_IMPULSE_PRE_ZERO_SAMPLES;
        assert!(
            8.0 * f64::from(PROJECT_SAMPLE_RATE_HZ) / right_half as f64 <= 20.0,
            "the retained impulse no longer covers eight cycles of 20 Hz after the peak"
        );
    }

    #[test]
    fn a_sweep_layout_without_room_for_the_ir_tail_is_rejected_at_import() {
        let required = SweepDeconvolutionConfig::default().impulse_length_samples;
        // Gap plus marker shorter than the IR tail: doomed, rejected.
        let error = validate_sweep_ir_tail_capacity(
            LiveChannel::Left,
            500_000,
            500_000 + 4_800,
            7_200,
            required,
        )
        .unwrap_err();
        assert!(error.contains("impulse-response tail"), "{error}");
        // Exactly enough tail passes (boundary), as does a generous layout.
        validate_sweep_ir_tail_capacity(
            LiveChannel::Left,
            500_000,
            500_000 + required - 7_200,
            7_200,
            required,
        )
        .unwrap();
        validate_sweep_ir_tail_capacity(
            LiveChannel::Right,
            500_000,
            500_000 + 48_000,
            24_000,
            required,
        )
        .unwrap();
    }

    #[test]
    fn bundled_wireless_sweeps_import_with_measurement_and_timing_regions() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/sweeps");
        let left_bytes = fs::read(fixture_root.join("Sweep_L_20-20k_refR.wav")).unwrap();
        let right_bytes = fs::read(fixture_root.join("Sweep_R_20-20k_refR.wav")).unwrap();

        let left = state.import_sweep(LiveChannel::Left, &left_bytes).unwrap();
        let right = state
            .import_sweep(LiveChannel::Right, &right_bytes)
            .unwrap();

        for summary in [&left, &right] {
            assert_eq!(summary.sample_rate_hz, PROJECT_SAMPLE_RATE_HZ);
            assert_eq!(summary.source_channels, 2);
            assert_eq!(summary.timing_marker_count, 2);
            assert_eq!(summary.start_marker_channel, Some(ReferenceChannel::Right));
            assert_eq!(summary.end_marker_channel, Some(ReferenceChannel::Right));
            assert!(summary
                .start_marker_channel_separation_db
                .is_some_and(|separation| separation > 20.0));
            assert!(summary
                .end_marker_channel_separation_db
                .is_some_and(|separation| separation > 20.0));
            assert!((9.5..=11.0).contains(&summary.measurement_duration_seconds));
            assert!(summary.measurement_peak_dbfs < -10.0);
        }

        // Import retries are idempotent and do not hit create_new collisions.
        assert_eq!(
            state
                .import_sweep(LiveChannel::Left, &left_bytes)
                .unwrap()
                .sha256,
            left.sha256
        );

        // Exercise the same recognizer/deconvolution adapter used by a real
        // capture, using the repository's exact L sweep through an identity
        // synthetic room. This does not replace hardware validation.
        let (_, _, left_sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        let capture = synthetic_capture(&left_sweep.samples, &[0.2]);
        let measurement = analyze_and_store_capture(
            &state,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".to_string(),
            &left_sweep,
            &calibration,
            &evidence,
            &capture,
        )
        .unwrap();
        state.finish_capture();
        assert!(measurement.accepted, "{:?}", measurement.issue_codes);
    }

    #[test]
    fn bundled_timing_markers_drive_automatic_completion_and_storage() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../assets/sweeps/Sweep_L_20-20k_refR.wav");
        let sweep_bytes = fs::read(fixture_path).unwrap();
        state.import_sweep(LiveChannel::Left, &sweep_bytes).unwrap();

        let (_, _, sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        let playback_offset_samples = 48_000;
        let samples = sparse_room_response(&marker_wrapped_capture_samples(
            &sweep,
            playback_offset_samples,
            0.5,
        ));
        let capture_f64 = samples
            .iter()
            .map(|sample| f64::from(*sample))
            .collect::<Vec<_>>();
        let marker_pair = detect_timing_marker_pair(&sweep, &capture_f64)
            .unwrap()
            .expect("the repository sweep's start and end markers must be recognized");
        let expected_sweep_start =
            playback_offset_samples + sweep.measurement_source_start_sample + 137;
        assert!(
            (marker_pair.estimated_sweep_start_sample - expected_sweep_start as f64).abs() <= 2.0
        );
        assert!(marker_pair.first.absolute_correlation >= MINIMUM_MARKER_CORRELATION);
        assert!(marker_pair.last.absolute_correlation >= MINIMUM_MARKER_CORRELATION);

        let mut monitor = LiveCaptureMonitor::new(&sweep, None);
        let snapshot_bound = monitor.maximum_snapshot_samples();
        let mut saw_start = false;
        let mut saw_end = false;
        let mut automatic_completion_sample = None;
        for capture_end in (12_000..=samples.len() + 12_000).step_by(12_000) {
            let capture_end = capture_end.min(samples.len());
            let snapshot_start = capture_end.saturating_sub(snapshot_bound);
            let update = monitor.inspect(snapshot_start, &samples[snapshot_start..capture_end]);
            saw_start |= update.progress.start_marker_detected;
            saw_end |= update.progress.end_marker_detected;
            if update.should_complete {
                automatic_completion_sample = Some(capture_end);
                assert_eq!(
                    update.progress.phase,
                    LiveCaptureProgressPhase::SavingMeasurement
                );
                break;
            }
            if capture_end == samples.len() {
                break;
            }
        }
        let automatic_completion_sample =
            automatic_completion_sample.expect("complete end marker must auto-complete");
        assert!(saw_start);
        assert!(saw_end);
        assert!(automatic_completion_sample < samples.len());
        let last_marker = sweep.timing_markers.last().unwrap();
        let expected_end_marker_completion = playback_offset_samples
            + last_marker.source_start_sample
            + last_marker.samples.len()
            + 137;
        assert!(
            automatic_completion_sample <= expected_end_marker_completion + 12_000,
            "capture must stop on the first monitor poll after the complete end marker"
        );

        let capture = capture_from_samples(samples[..automatic_completion_sample].to_vec(), true);
        let measurement = analyze_and_store_capture(
            &state,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".to_string(),
            &sweep,
            &calibration,
            &evidence,
            &capture,
        )
        .unwrap();
        state.finish_capture();
        assert!(measurement.accepted, "{:?}", measurement.issue_codes);
        assert!(measurement.start_marker_detected);
        assert!(measurement.end_marker_detected);
        assert!(measurement.automatic_completion_detected);
        assert!(measurement.level_assessment.acceptable_for_measurement);
        assert!(Path::new(&measurement.raw_wav_path).is_file());
        let snapshot_path = Path::new(
            measurement
                .measurement_snapshot_path
                .as_deref()
                .expect("accepted measurement must persist its snapshot"),
        );
        assert!(snapshot_path.is_file());
        let snapshot: serde_json::Value =
            serde_json::from_slice(&fs::read(snapshot_path).unwrap()).unwrap();
        assert_eq!(snapshot["startMarkerSourceChannel"], "right");
        assert_eq!(snapshot["endMarkerSourceChannel"], "right");
        assert_eq!(
            snapshot["markerChannelAnalysisVersion"],
            SWEEP_MARKER_CHANNEL_ANALYSIS_VERSION
        );
        assert_eq!(snapshot["rewExportAlgorithm"], LIVE_REW_EXPORT_VERSION);

        // The accepted capture is also written where REW can open it. REW names
        // an imported measurement after its file, so the name has to identify
        // the acoustic path and when it was measured without any other context.
        let session_root = snapshot_path.parent().unwrap().parent().unwrap();
        let exports: Vec<PathBuf> = fs::read_dir(session_root.join("rew"))
            .expect("the session must have a REW export directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        assert_eq!(exports.len(), 1, "{exports:?}");
        let exported = &exports[0];
        let exported_name = exported.file_name().unwrap().to_str().unwrap();
        assert!(
            exported_name.starts_with("L P0 ") && exported_name.ends_with(".wav"),
            "unexpected REW export name `{exported_name}`"
        );
        // `L P0 2026-07-29 14-30-05.wav`
        let stamp = exported_name
            .trim_start_matches("L P0 ")
            .trim_end_matches(".wav");
        assert_eq!(stamp.len(), 19, "expected a date and time in `{stamp}`");
        assert_eq!(stamp.matches('-').count(), 4);
        let reader = hound::WavReader::open(exported).expect("REW export must be a readable WAV");
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, PROJECT_SAMPLE_RATE_HZ);
        assert_eq!(spec.bits_per_sample, 32);
        assert_eq!(spec.sample_format, hound::SampleFormat::Float);
        assert_eq!(
            reader.len() as usize,
            SweepDeconvolutionConfig::default().impulse_length_samples,
            "the export must carry the whole calibrated impulse REW will analyze"
        );
    }

    /// The five names a user asked to be able to recognize in REW.
    #[test]
    fn rew_export_names_say_which_acoustic_path_was_measured() {
        let name =
            |mode, kind, channel, position| rew_measurement_label(mode, kind, channel, position);
        // A 2.1 baseline is the main plus the subwoofer playing together.
        assert_eq!(
            name(
                LiveSystemMode::SingleSub21,
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0"
            ),
            "L+Sub P0"
        );
        assert_eq!(
            name(
                LiveSystemMode::SingleSub21,
                LiveCaptureKind::Baseline,
                LiveChannel::Right,
                "P2"
            ),
            "R+Sub P2"
        );
        // The separated-path stage measures each source alone.
        assert_eq!(
            name(
                LiveSystemMode::SingleSub21,
                LiveCaptureKind::SubMainOnly,
                LiveChannel::Left,
                "XO01"
            ),
            "L XO01"
        );
        assert_eq!(
            name(
                LiveSystemMode::SingleSub21,
                LiveCaptureKind::SubOnly,
                LiveChannel::Left,
                "XO02"
            ),
            "Sub XO02"
        );
        // Without a subwoofer the baseline is just the speaker.
        assert_eq!(
            name(
                LiveSystemMode::Stereo20,
                LiveCaptureKind::Baseline,
                LiveChannel::Right,
                "P0"
            ),
            "R P0"
        );
        // The closed-loop capture measures the same path with the trial filter
        // active; reading it as the unfiltered baseline would invert the result.
        assert_eq!(
            name(
                LiveSystemMode::SingleSub21,
                LiveCaptureKind::Verification,
                LiveChannel::Left,
                "P0"
            ),
            "L+Sub P0 filtered"
        );
    }

    #[test]
    fn bundled_timing_markers_survive_low_level_reverberant_capture() {
        let state = LiveMeasurementState::default();
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../assets/sweeps/Sweep_L_20-20k_refR.wav");
        let sweep_bytes = fs::read(fixture_path).unwrap();
        let temporary = tempdir().unwrap();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        state.import_sweep(LiveChannel::Left, &sweep_bytes).unwrap();
        let (_, _, sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();

        let samples = low_level_reverberant_room_response(&marker_wrapped_capture_samples(
            &sweep, 48_000, 0.035,
        ));
        let capture = samples
            .iter()
            .map(|sample| f64::from(*sample))
            .collect::<Vec<_>>();
        let pair = detect_timing_marker_pair(&sweep, &capture)
            .unwrap()
            .expect("both timing markers should survive safe low-level room playback");
        assert!(pair.first.absolute_correlation >= MINIMUM_MARKER_CORRELATION);
        assert!(pair.last.absolute_correlation >= MINIMUM_MARKER_CORRELATION);

        let mut monitor = LiveCaptureMonitor::new(&sweep, calibration.sensitivity_factor_db);
        let snapshot_bound = monitor.maximum_snapshot_samples();
        let mut automatic_completion_sample = None;
        for capture_end in (12_000..=samples.len() + 12_000).step_by(12_000) {
            let capture_end = capture_end.min(samples.len());
            let snapshot_start = capture_end.saturating_sub(snapshot_bound);
            let update = monitor.inspect(snapshot_start, &samples[snapshot_start..capture_end]);
            if update.should_complete {
                automatic_completion_sample = Some(capture_end);
                break;
            }
            if capture_end == samples.len() {
                break;
            }
        }
        let automatic_completion_sample = automatic_completion_sample
            .expect("the low-level repeated marker pair must still stop capture automatically");
        let stored = analyze_and_store_capture(
            &state,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".to_string(),
            &sweep,
            &calibration,
            &evidence,
            &capture_from_samples(samples[..automatic_completion_sample].to_vec(), true),
        )
        .unwrap();
        state.finish_capture();
        assert!(stored.accepted, "{:?}", stored.issue_codes);
        assert_eq!(
            stored.level_assessment.status,
            LiveMeasurementLevelStatus::TooLow
        );
        assert!(stored.level_assessment.acceptable_for_measurement);
        assert!(stored.level_assessment.measurement_peak_dbfs >= LEVEL_MINIMUM_ACCEPTED_PEAK_DBFS);
    }

    #[test]
    fn marker_referenced_deconvolution_retains_an_earlier_main_arrival() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../assets/sweeps/Sweep_L_20-20k_refR.wav");
        state
            .import_sweep(LiveChannel::Left, &fs::read(fixture_path).unwrap())
            .unwrap();
        let (_, _, sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();

        let playback_offset = 48_000;
        let marker_speaker_delay = 800;
        let main_speaker_delay = 100;
        let mut samples = vec![0.0_f32; playback_offset + sweep.source_frame_count + 24_000];
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = ((((index as u64)
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223)
                >> 16)
                & 0xffff) as f64
                / 65_535.0
                * 2.0
                - 1.0) as f32
                * 0.000_01;
        }
        for marker in &sweep.timing_markers {
            let start = playback_offset + marker.source_start_sample + marker_speaker_delay;
            for (destination, &source) in samples[start..start + marker.samples.len()]
                .iter_mut()
                .zip(&marker.samples)
            {
                *destination += (source * 0.4) as f32;
            }
        }
        let main_start =
            playback_offset + sweep.measurement_source_start_sample + main_speaker_delay;
        for (destination, &source) in samples[main_start..main_start + sweep.samples.len()]
            .iter_mut()
            .zip(&sweep.samples)
        {
            *destination += (source * 0.4) as f32;
        }

        let summary = analyze_and_store_capture(
            &state,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".to_string(),
            &sweep,
            &calibration,
            &evidence,
            &capture_from_samples(samples, true),
        )
        .unwrap();
        state.finish_capture();

        assert!(summary.accepted, "{:?}", summary.issue_codes);
        assert!(summary.reconstruction_fit_db.is_some_and(|fit| fit >= 12.0));
        let snapshot: serde_json::Value =
            serde_json::from_slice(&fs::read(summary.measurement_snapshot_path.unwrap()).unwrap())
                .unwrap();
        let impulse = snapshot["calibratedImpulseSamples"].as_array().unwrap();
        let peak_index = impulse
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| {
                left.as_f64()
                    .unwrap()
                    .abs()
                    .total_cmp(&right.as_f64().unwrap().abs())
            })
            .map(|(index, _)| index)
            .unwrap();
        assert_eq!(
            peak_index,
            MARKER_REFERENCED_IMPULSE_PRE_ZERO_SAMPLES + main_speaker_delay - marker_speaker_delay
        );
    }

    #[test]
    fn marker_qualified_room_decay_keeps_linear_reconstruction_fit_diagnostic() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../assets/sweeps/Sweep_L_20-20k_refR.wav");
        state
            .import_sweep(LiveChannel::Left, &fs::read(fixture_path).unwrap())
            .unwrap();
        let (_, _, sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();

        let playback_offset = 48_000;
        let direct_delay = 137;
        // Past the 32,768-sample retained IR (1,024 of which are pre-zero) so
        // the linear reconstruction cannot model this decay, yet still fully
        // ahead of the end marker so pair detection stays clean.
        let long_decay_delay = 40_000;
        let mut captured = vec![0.0_f32; playback_offset + sweep.source_frame_count + 24_000];
        for (index, sample) in captured.iter_mut().enumerate() {
            *sample = ((((index as u64)
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223)
                >> 16)
                & 0xffff) as f64
                / 65_535.0
                * 2.0
                - 1.0) as f32
                * 0.000_01;
        }
        for marker in &sweep.timing_markers {
            let start = playback_offset + marker.source_start_sample + direct_delay;
            for (destination, &source) in captured[start..start + marker.samples.len()]
                .iter_mut()
                .zip(&marker.samples)
            {
                *destination += (source * 0.4) as f32;
            }
        }
        let main_start = playback_offset + sweep.measurement_source_start_sample;
        for (index, &source) in sweep.samples.iter().enumerate() {
            captured[main_start + direct_delay + index] += (source * 0.30) as f32;
            captured[main_start + long_decay_delay + index] += (source * 0.20) as f32;
        }
        let summary = analyze_and_store_capture(
            &state,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".to_string(),
            &sweep,
            &calibration,
            &evidence,
            &capture_from_samples(captured, true),
        )
        .unwrap();
        state.finish_capture();

        assert!(summary.reconstruction_fit_db.is_some_and(|fit| fit < 12.0));
        assert!(!summary.reconstruction_fit_required);
        assert!(summary.accepted, "{:?}", summary.issue_codes);
        assert!(summary.issue_codes.is_empty());
    }

    #[test]
    fn unequal_repeated_marker_candidates_still_form_one_strict_pair() {
        let state = LiveMeasurementState::default();
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../assets/sweeps/Sweep_R_20-20k_refR.wav");
        let temporary = tempdir().unwrap();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        state
            .import_sweep(LiveChannel::Right, &fs::read(fixture_path).unwrap())
            .unwrap();
        let (_, _, sweep, _, _, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Right,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        state.finish_capture();

        let playback_offset = 48_000;
        let echo_samples = 137;
        let mut capture = vec![0.0; playback_offset + sweep.source_frame_count + echo_samples + 1];
        for (marker_index, marker) in sweep.timing_markers.iter().enumerate() {
            let direct_gain = if marker_index == 0 { 0.55 } else { 1.0 };
            let echo_gain = if marker_index == 0 { 0.75 } else { 0.20 };
            let start = playback_offset + marker.source_start_sample;
            for (sample_index, &source) in marker.samples.iter().enumerate() {
                capture[start + sample_index] += source * direct_gain;
                capture[start + sample_index + echo_samples] += source * echo_gain;
            }
        }

        let candidates = recognize_timing_marker(&sweep.timing_markers[0], &capture).unwrap();
        assert_eq!(candidates.len(), 2);
        let strongest = candidates
            .iter()
            .map(|candidate| candidate.absolute_correlation)
            .fold(0.0_f64, f64::max);
        let weakest = candidates
            .iter()
            .map(|candidate| candidate.absolute_correlation)
            .fold(f64::INFINITY, f64::min);
        assert!(weakest / strongest < 0.90);
        assert!(weakest / strongest >= MINIMUM_REPEATED_MARKER_CANDIDATE_RATIO);

        let pair = detect_timing_marker_pair(&sweep, &capture)
            .unwrap()
            .expect("unequal but valid repeated markers must retain both pair candidates");
        let drift_ppm = (pair.capture_samples_per_reference_sample - 1.0) * 1_000_000.0;
        assert!(drift_ppm.abs() <= MAXIMUM_MARKER_PAIR_CLOCK_DRIFT_PPM);
    }

    #[test]
    fn separated_subwoofer_plan_uses_the_fixed_marker_speaker_and_gates_roles() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state
            .start_session_with_mode(temporary.path(), LiveSystemMode::SingleSub21)
            .unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/sweeps");
        state
            .import_sweep(
                LiveChannel::Left,
                &fs::read(fixture_root.join("Sweep_L_20-20k_refR.wav")).unwrap(),
            )
            .unwrap();
        state
            .import_sweep(
                LiveChannel::Right,
                &fs::read(fixture_root.join("Sweep_R_20-20k_refR.wav")).unwrap(),
            )
            .unwrap();
        let request = LiveSubwooferSearchRequest {
            crossover_hz: vec![70.0, 80.0, 90.0],
            measured_main_delay_ms: 0.83,
            measured_polarity_degrees: 0,
            fixed_sub_level_db: 0.0,
            delay_minimum_ms: 0.0,
            delay_maximum_ms: 5.0,
            delay_step_ms: 0.05,
        };
        let plan = state.configure_subwoofer_search(request).unwrap();
        assert_eq!(plan.candidates.len(), 3);
        assert_eq!(plan.fixed_timing_reference_channel, ReferenceChannel::Right);
        assert_eq!(plan.sub_sweep_channel, LiveChannel::Left);
        assert!(Path::new(&plan.plan_path).is_file());

        state
            .begin_capture(
                LiveCaptureKind::SubMainOnly,
                LiveChannel::Left,
                "XO01",
                "synthetic::input",
                0,
            )
            .unwrap();
        state.finish_capture();
        let wrong_sub_channel = state
            .begin_capture(
                LiveCaptureKind::SubOnly,
                LiveChannel::Right,
                "XO01",
                "synthetic::input",
                0,
            )
            .unwrap_err();
        assert!(wrong_sub_channel.contains("must use the left sweep"));
        let unknown_candidate = state
            .begin_capture(
                LiveCaptureKind::SubMainOnly,
                LiveChannel::Left,
                "XO09",
                "synthetic::input",
                0,
            )
            .unwrap_err();
        assert!(unknown_candidate.contains("not in the current search plan"));
        let setup_before_optimization = state
            .record_subwoofer_setup(LiveSubwooferSetupRequest {
                crossover_hz: 90.0,
                main_delay_ms: 0.83,
                polarity_degrees: 0,
                sub_level_db: 0.0,
                confirmed_on_hardware: true,
            })
            .unwrap_err();
        assert!(setup_before_optimization.contains("finish the separated main/sub optimization"));
    }

    #[test]
    fn live_separated_path_adapter_returns_the_core_prediction_and_persists_it() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state
            .start_session_with_mode(temporary.path(), LiveSystemMode::SingleSub21)
            .unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/sweeps");
        state
            .import_sweep(
                LiveChannel::Left,
                &fs::read(fixture_root.join("Sweep_L_20-20k_refR.wav")).unwrap(),
            )
            .unwrap();
        state
            .import_sweep(
                LiveChannel::Right,
                &fs::read(fixture_root.join("Sweep_R_20-20k_refR.wav")).unwrap(),
            )
            .unwrap();
        let plan = state
            .configure_subwoofer_search(LiveSubwooferSearchRequest {
                crossover_hz: vec![80.0, 100.0],
                measured_main_delay_ms: 0.0,
                measured_polarity_degrees: 0,
                fixed_sub_level_db: 0.0,
                delay_minimum_ms: 0.0,
                delay_maximum_ms: 4.0,
                delay_step_ms: 1.0,
            })
            .unwrap();
        let (_, _, sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::SubMainOnly,
                LiveChannel::Left,
                "XO01",
                "synthetic::input",
                0,
            )
            .unwrap();
        // The template capture is analyzed as a main-only isolated path, so
        // its spectrum must look like one: a first difference adds a
        // 6 dB/oct high-pass tilt, keeping the sub-band well below the
        // passband as the leakage gate demands of a bass-managed main.
        let raw_samples =
            sparse_room_response(&marker_wrapped_capture_samples(&sweep, 48_000, 0.5));
        let samples: Vec<f32> = raw_samples
            .iter()
            .enumerate()
            .map(|(index, sample)| {
                if index == 0 {
                    *sample
                } else {
                    sample - raw_samples[index - 1]
                }
            })
            .collect();
        let capture = capture_from_samples(samples, true);
        analyze_and_store_capture(
            &state,
            LiveCaptureKind::SubMainOnly,
            LiveChannel::Left,
            "XO01".to_string(),
            &sweep,
            &calibration,
            &evidence,
            &capture,
        )
        .unwrap();
        state.finish_capture();

        {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().unwrap();
            let base = session
                .measurements
                .get(&(
                    LiveCaptureKind::SubMainOnly,
                    "XO01".to_string(),
                    LiveChannel::Left,
                ))
                .unwrap()
                .clone();
            for (candidate_index, candidate) in plan.candidates.iter().enumerate() {
                // The 80 Hz candidate's sub offset (2 ms) is reachable inside
                // the 0-4 ms search grid; the 100 Hz candidate's (6 ms) is
                // not, so its best alignment keeps a residual crossover
                // notch. The winner must be decided by that band structure,
                // never by an overall level difference (which scoring
                // normalizes away as session volume).
                let crossover_hz = candidate.crossover_hz;
                let sub_delay_ms = if candidate_index == 0 { 2.0 } else { 6.0 };
                for (kind, channel, response) in [
                    (
                        LiveCaptureKind::SubMainOnly,
                        LiveChannel::Left,
                        shaped_isolated_test_response(crossover_hz, false, 0.0, false),
                    ),
                    (
                        LiveCaptureKind::SubMainOnly,
                        LiveChannel::Right,
                        shaped_isolated_test_response(crossover_hz, false, 0.0, false),
                    ),
                    (
                        LiveCaptureKind::SubOnly,
                        plan.sub_sweep_channel,
                        shaped_isolated_test_response(crossover_hz, true, sub_delay_ms, true),
                    ),
                ] {
                    let mut stored = base.clone();
                    stored.summary.kind = kind;
                    stored.summary.channel = channel;
                    stored.summary.position_id = candidate.id.clone();
                    stored.summary.frequency_bin_count = response.frequencies_hz.len();
                    stored.frequencies_hz = response.frequencies_hz.clone();
                    stored.magnitude_db = response.magnitude_db.clone();
                    stored.calibrated_frequency_response = response;
                    stored.evidence.generation = session.evidence_generation;
                    stored.evidence.subwoofer_search = Some(plan.clone());
                    stored.evidence.sweep_sha256 = session.sweeps[&channel].summary.sha256.clone();
                    session
                        .measurements
                        .insert((kind, candidate.id.clone(), channel), stored);
                }
            }
        }

        let result = state.optimize_subwoofer_paths().unwrap();
        // Adaptive arrival-centered windows: (5 + 4) * 2 = 18 candidates.
        assert_eq!(result.synthesized_candidate_count, 18);
        assert_eq!(result.best.crossover_hz, 80.0);
        assert_eq!(result.best.main_delay_ms, 2.0);
        assert_eq!(result.best.polarity_degrees, 180);
        assert!(result.needs_combined_confirmation);
        assert_eq!(result.arrival_estimates.len(), 2);
        assert!((result.arrival_estimates[0].center_ms - 2.0).abs() <= 0.1);
        assert!(result.arrival_estimates[1].range_limited);
        assert!(Path::new(&result.report_path).is_file());
    }

    #[test]
    fn umik_sensitivity_level_estimate_and_digital_safety_gate_are_separate() {
        let estimated = estimated_umik_spl_db(Some(-30.0), Some(-2.434)).unwrap();
        assert!((estimated - 90.434).abs() < 1.0e-9);
        assert_eq!(
            measurement_level_status(Some(-18.0), Some(75.0)),
            LiveMeasurementLevelStatus::Good
        );
        assert_eq!(
            measurement_level_status(Some(-31.0), Some(75.0)),
            LiveMeasurementLevelStatus::TooLow
        );
        assert_eq!(
            measurement_level_status(Some(-18.0), Some(55.0)),
            LiveMeasurementLevelStatus::Good
        );
        assert_eq!(
            measurement_level_status(Some(-0.5), Some(75.0)),
            LiveMeasurementLevelStatus::Clipping
        );
    }

    #[test]
    fn active_capture_blocks_mutation_and_stale_evidence_is_rejected() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let wav = test_sweep_wav();
        state.import_sweep(LiveChannel::Left, &wav).unwrap();
        state.import_sweep(LiveChannel::Right, &wav).unwrap();

        let (_, _, sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        assert!(state
            .import_calibration("changed.txt", "10 1\n24000 1\n")
            .unwrap_err()
            .contains("active microphone capture"));
        assert!(state
            .import_sweep(LiveChannel::Left, &wav)
            .unwrap_err()
            .contains("active microphone capture"));
        assert!(state.design_trial("bk").unwrap_err().contains("active"));
        assert!(state.verification_summary().unwrap_err().contains("active"));
        assert!(state.finalize_export().unwrap_err().contains("active"));

        state.finish_capture();
        assert!(state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Right,
                "P0",
                "different-microphone",
                0,
            )
            .err()
            .unwrap()
            .contains("locked to microphone"));
        assert!(state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Right,
                "P0",
                "synthetic::input",
                1,
            )
            .err()
            .unwrap()
            .contains("locked to microphone input channel"));
        state
            .import_calibration("changed.txt", "10 1\n24000 1\n")
            .unwrap();
        let capture = synthetic_capture(&sweep.samples, &[0.2]);
        let error = analyze_and_store_capture(
            &state,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".into(),
            &sweep,
            &calibration,
            &evidence,
            &capture,
        )
        .unwrap_err();
        assert!(
            error.contains("evidence changed") || error.contains("calibration changed"),
            "{error}"
        );
    }

    #[test]
    fn capture_request_covers_the_full_wireless_source_not_only_the_extracted_sweep() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/sweeps");
        let bytes = fs::read(fixture_root.join("Sweep_L_20-20k_refR.wav")).unwrap();
        state.import_sweep(LiveChannel::Left, &bytes).unwrap();
        let sweep =
            state.session.lock().unwrap().as_ref().unwrap().sweeps[&LiveChannel::Left].clone();

        let request =
            live_capture_request("test-device".into(), 1, sweep.source_frame_count, 20).unwrap();
        assert_eq!(request.input_channel_index, 1);
        let full_source_ms = (u64::try_from(sweep.source_frame_count).unwrap() * 1_000)
            .div_ceil(u64::from(PROJECT_SAMPLE_RATE_HZ));
        assert_eq!(
            request.duration_ms,
            20_000 + full_source_ms + CAPTURE_DEADLINE_GRACE_MILLISECONDS
        );
        assert!(request.duration_ms > capture_duration_ms(sweep.samples.len(), 20).unwrap());
    }

    #[test]
    fn accepted_measurement_cache_restores_only_exact_input_evidence() {
        let temporary = tempdir().unwrap();
        let calibration_text = "10 0\n24000 0\n";
        let wav = test_sweep_wav();
        let source = LiveMeasurementState::default();
        let source_session = source.start_session(temporary.path()).unwrap();
        source
            .import_calibration("umik.txt", calibration_text)
            .unwrap();
        source.import_sweep(LiveChannel::Left, &wav).unwrap();
        source.import_sweep(LiveChannel::Right, &wav).unwrap();
        let (_, _, sweep, calibration, evidence, _) = source
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        let capture = synthetic_capture(&sweep.samples, &[0.2]);
        let accepted = analyze_and_store_capture(
            &source,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".into(),
            &sweep,
            &calibration,
            &evidence,
            &capture,
        )
        .unwrap();
        source.finish_capture();
        assert!(accepted.accepted);
        let snapshot_path = PathBuf::from(
            accepted
                .measurement_snapshot_path
                .as_deref()
                .expect("accepted snapshot path"),
        );
        let mut legacy_snapshot =
            serde_json::from_slice::<serde_json::Value>(&fs::read(&snapshot_path).unwrap())
                .unwrap();
        let legacy_object = legacy_snapshot.as_object_mut().unwrap();
        legacy_object.remove("diagnosticCodes");
        legacy_object.remove("audioStreamDiagnostics");
        legacy_object.remove("recognizedSweepStartCaptureSample");
        fs::write(
            &snapshot_path,
            serde_json::to_vec_pretty(&legacy_snapshot).unwrap(),
        )
        .unwrap();

        let restored_state = LiveMeasurementState::default();
        restored_state.start_session(temporary.path()).unwrap();
        restored_state
            .import_calibration("same.txt", calibration_text)
            .unwrap();
        restored_state
            .import_sweep(LiveChannel::Left, &wav)
            .unwrap();
        restored_state
            .import_sweep(LiveChannel::Right, &wav)
            .unwrap();
        let restored = restored_state
            .restore_accepted_measurements("synthetic::input", 0, LiveRestoreScope::General)
            .unwrap();
        assert_eq!(
            restored.source_session_id.as_deref(),
            Some(source_session.session_id.as_str())
        );
        assert_eq!(restored.restored_captures.len(), 1);
        assert!(restored.restored_captures[0].restored_from_cache);
        assert_eq!(
            restored_state
                .session
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .measurements
                .len(),
            1
        );

        let wrong_device_state = LiveMeasurementState::default();
        wrong_device_state.start_session(temporary.path()).unwrap();
        wrong_device_state
            .import_calibration("same.txt", calibration_text)
            .unwrap();
        wrong_device_state
            .import_sweep(LiveChannel::Left, &wav)
            .unwrap();
        wrong_device_state
            .import_sweep(LiveChannel::Right, &wav)
            .unwrap();
        let wrong_device = wrong_device_state
            .restore_accepted_measurements("different::input", 0, LiveRestoreScope::General)
            .unwrap();
        assert!(wrong_device.restored_captures.is_empty());
    }

    #[test]
    fn accepted_measurement_cache_restores_the_newest_pass_for_each_measurement_key() {
        let temporary = tempdir().unwrap();
        let calibration_text = "10 0\n24000 0\n";
        let wav = test_sweep_wav();

        let older = LiveMeasurementState::default();
        let older_session = older.start_session(temporary.path()).unwrap();
        older
            .import_calibration("umik.txt", calibration_text)
            .unwrap();
        older.import_sweep(LiveChannel::Left, &wav).unwrap();
        older.import_sweep(LiveChannel::Right, &wav).unwrap();
        for channel in [LiveChannel::Left, LiveChannel::Right] {
            let (_, _, sweep, calibration, evidence, _) = older
                .begin_capture(
                    LiveCaptureKind::Baseline,
                    channel,
                    "P0",
                    "synthetic::input",
                    0,
                )
                .unwrap();
            let capture = synthetic_capture(&sweep.samples, &[0.2]);
            let summary = analyze_and_store_capture(
                &older,
                LiveCaptureKind::Baseline,
                channel,
                "P0".into(),
                &sweep,
                &calibration,
                &evidence,
                &capture,
            )
            .unwrap();
            older.finish_capture();
            assert!(summary.accepted);
        }

        let newer = LiveMeasurementState::default();
        let newer_session = newer.start_session(temporary.path()).unwrap();
        newer
            .import_calibration("umik.txt", calibration_text)
            .unwrap();
        newer.import_sweep(LiveChannel::Left, &wav).unwrap();
        newer.import_sweep(LiveChannel::Right, &wav).unwrap();
        let (_, _, sweep, calibration, evidence, _) = newer
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Right,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        let capture = synthetic_capture(&sweep.samples, &[0.2]);
        let summary = analyze_and_store_capture(
            &newer,
            LiveCaptureKind::Baseline,
            LiveChannel::Right,
            "P0".into(),
            &sweep,
            &calibration,
            &evidence,
            &capture,
        )
        .unwrap();
        newer.finish_capture();
        assert!(summary.accepted);

        let restored_state = LiveMeasurementState::default();
        restored_state.start_session(temporary.path()).unwrap();
        restored_state
            .import_calibration("same.txt", calibration_text)
            .unwrap();
        restored_state
            .import_sweep(LiveChannel::Left, &wav)
            .unwrap();
        restored_state
            .import_sweep(LiveChannel::Right, &wav)
            .unwrap();
        let restored = restored_state
            .restore_accepted_measurements("synthetic::input", 0, LiveRestoreScope::General)
            .unwrap();

        assert_eq!(restored.restored_captures.len(), 2);
        assert_eq!(restored.compatible_snapshot_count, 3);
        assert_eq!(
            restored.source_session_ids,
            vec![
                newer_session.session_id.clone(),
                older_session.session_id.clone()
            ]
        );
        assert_eq!(
            restored.source_session_id.as_deref(),
            Some(newer_session.session_id.as_str())
        );
        let restored_left = restored
            .restored_captures
            .iter()
            .find(|capture| capture.channel == LiveChannel::Left)
            .unwrap();
        let restored_right = restored
            .restored_captures
            .iter()
            .find(|capture| capture.channel == LiveChannel::Right)
            .unwrap();
        assert_eq!(
            restored_left.cache_source_session_id.as_deref(),
            Some(older_session.session_id.as_str())
        );
        assert_eq!(
            restored_right.cache_source_session_id.as_deref(),
            Some(newer_session.session_id.as_str())
        );

        // A restored measurement must be indistinguishable from the session's
        // own capture, including the reconstructed response's analysis grid.
        // The restore path once kept a hardcoded 32,768-point reconstruction
        // after the capture-time analysis doubled to 65,536, which silently
        // moved the separated-path sub recommendation on identical cached data
        // (6.8 ms fresh versus 7.6 ms restored on the real session): the
        // arrival estimator saw the same phase curves at half the density.
        let expected_fft = SweepDeconvolutionConfig::default().analysis_fft_size;
        let restored_guard = restored_state.session.lock().unwrap();
        let restored_session = restored_guard.as_ref().unwrap();
        let newer_guard = newer.session.lock().unwrap();
        let newer_inner = newer_guard.as_ref().unwrap();
        {
            let (state_session, channel) = (restored_session, LiveChannel::Right);
            let restored_measurement = &state_session.measurements
                [&(LiveCaptureKind::Baseline, "P0".to_string(), channel)];
            let fresh_measurement =
                &newer_inner.measurements[&(LiveCaptureKind::Baseline, "P0".to_string(), channel)];
            assert_eq!(
                restored_measurement.calibrated_frequency_response.fft_size,
                expected_fft
            );
            let restored_fr = &restored_measurement.calibrated_frequency_response;
            let fresh_fr = &fresh_measurement.calibrated_frequency_response;
            // The grid must match the capture-time analysis exactly; the
            // values may differ by last-bit float noise because serde_json's
            // default parser does not guarantee bit-exact f64 roundtrip (that
            // needs its `float_roundtrip` feature), which perturbs the stored
            // impulse by ULPs. Measured here: <=7.2e-15 dB and <=3.4e-16 rad,
            // eleven orders of magnitude under anything the pipeline gates.
            assert_eq!(restored_fr.fft_size, fresh_fr.fft_size);
            assert_eq!(restored_fr.frequencies_hz, fresh_fr.frequencies_hz);
            let max_diff = |a: &[f64], b: &[f64]| {
                assert_eq!(a.len(), b.len());
                a.iter()
                    .zip(b)
                    .map(|(x, y)| (x - y).abs())
                    .fold(0.0_f64, f64::max)
            };
            let magnitude_diff = max_diff(&restored_fr.magnitude_db, &fresh_fr.magnitude_db);
            let phase_diff = max_diff(&restored_fr.phase_rad, &fresh_fr.phase_rad);
            assert!(
                magnitude_diff < 1.0e-9 && phase_diff < 1.0e-9,
                "restored response drifted from the capture-time response:                  {magnitude_diff:e} dB, {phase_diff:e} rad"
            );
        }
    }

    #[test]
    fn rejected_retry_does_not_evict_an_accepted_measurement() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let wav = test_sweep_wav();
        state.import_sweep(LiveChannel::Left, &wav).unwrap();
        state.import_sweep(LiveChannel::Right, &wav).unwrap();
        let (_, _, sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        let accepted_capture = synthetic_capture(&sweep.samples, &[0.2]);
        let accepted = analyze_and_store_capture(
            &state,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".into(),
            &sweep,
            &calibration,
            &evidence,
            &accepted_capture,
        )
        .unwrap();
        state.finish_capture();
        assert!(accepted.accepted);

        let (_, _, sweep, calibration, evidence, _) = state
            .begin_capture(
                LiveCaptureKind::Baseline,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .unwrap();
        let mut rejected_capture = synthetic_capture(&sweep.samples, &[0.2]);
        rejected_capture.sample_drop_detected = true;
        rejected_capture.callback_lock_drop_frames = 512;
        let rejected = analyze_and_store_capture(
            &state,
            LiveCaptureKind::Baseline,
            LiveChannel::Left,
            "P0".into(),
            &sweep,
            &calibration,
            &evidence,
            &rejected_capture,
        )
        .unwrap();
        state.finish_capture();
        assert!(!rejected.accepted);
        assert_eq!(rejected.issue_codes, vec!["audio_monitor_contention_drop"]);
        assert!(
            state.session.lock().unwrap().as_ref().unwrap().measurements[&(
                LiveCaptureKind::Baseline,
                "P0".to_string(),
                LiveChannel::Left
            )]
                .summary
                .accepted
        );
    }

    #[test]
    fn live_project_reuses_phase4_then_requires_measured_verification_for_phase6_zip() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let wav = test_sweep_wav();
        state.import_sweep(LiveChannel::Left, &wav).unwrap();
        state.import_sweep(LiveChannel::Right, &wav).unwrap();
        let left_room = vec![0.2];
        let right_room = vec![0.2];
        for (channel, room) in [
            (LiveChannel::Left, &left_room),
            (LiveChannel::Right, &right_room),
        ] {
            let (_, _, sweep, calibration, evidence, _) = state
                .begin_capture(
                    LiveCaptureKind::Baseline,
                    channel,
                    "P0",
                    "synthetic::input",
                    0,
                )
                .unwrap();
            let capture = synthetic_capture(&sweep.samples, room);
            let summary = analyze_and_store_capture(
                &state,
                LiveCaptureKind::Baseline,
                channel,
                "P0".into(),
                &sweep,
                &calibration,
                &evidence,
                &capture,
            )
            .unwrap();
            state.finish_capture();
            assert!(
                summary.accepted,
                "{:?}, correlation {:?}, drift {:?}, fit {:?}",
                summary.issue_codes,
                summary.correlation,
                summary.clock_drift_ppm,
                summary.reconstruction_fit_db
            );
        }
        let fixture = SyntheticRoomFixture::phase1_48k().unwrap();
        {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().unwrap();
            for (channel, impulse) in [
                (LiveChannel::Left, &fixture.left_impulses[0]),
                (LiveChannel::Right, &fixture.right_impulses[0]),
            ] {
                let response = frequency_response(impulse, 48_000, 32_768).unwrap();
                let indices = response
                    .frequencies_hz
                    .iter()
                    .enumerate()
                    .filter_map(|(index, frequency)| {
                        (*frequency > 0.0 && *frequency <= 20_000.0).then_some(index)
                    })
                    .collect::<Vec<_>>();
                let stored = session
                    .measurements
                    .get_mut(&(LiveCaptureKind::Baseline, "P0".to_string(), channel))
                    .unwrap();
                stored.frequencies_hz = indices
                    .iter()
                    .map(|index| response.frequencies_hz[*index])
                    .collect();
                stored.magnitude_db = indices
                    .iter()
                    .map(|index| response.magnitude_db[*index])
                    .collect();
                stored.calibrated_impulse_samples = impulse.clone();
            }
        }
        let target_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../assets/targets/Harman-6dB_REW.txt");
        let target_text = fs::read_to_string(target_path).unwrap();
        let target = state
            .import_target("Harman-6dB_REW.txt", &target_text)
            .unwrap();
        assert_eq!(target.point_count, 26);
        assert!(target.correction_band_covered);
        assert!(Path::new(&target.stored_path).is_file());
        let design = state.design_trial("custom").unwrap();
        assert!(design.numerical_passed);
        let trial_download_path = temporary.path().join("downloaded-trial.zip");
        let trial_download = state
            .save_zip_artifact(LiveZipArtifactKind::Trial, &trial_download_path)
            .unwrap();
        assert_eq!(trial_download.artifact_kind, LiveZipArtifactKind::Trial);
        assert_eq!(
            fs::read(&trial_download_path).unwrap(),
            fs::read(&design.trial_zip_path).unwrap()
        );
        validate_roon_zip(
            &trial_download_path,
            &BTreeSet::from([PROJECT_SAMPLE_RATE_HZ]),
        )
        .unwrap();
        assert!(state
            .save_zip_artifact(
                LiveZipArtifactKind::Final,
                &temporary.path().join("premature-final.zip")
            )
            .unwrap_err()
            .contains("create the verified final package"));
        assert!(state.finalize_export().is_err());
        let declaration_error = state
            .begin_capture(
                LiveCaptureKind::Verification,
                LiveChannel::Left,
                "P0",
                "synthetic::input",
                0,
            )
            .err()
            .unwrap();
        assert!(declaration_error.contains("declare the exact trial filter"));
        assert!(state.set_trial_activation(true).unwrap());
        {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().unwrap();
            let result = session.design.as_ref().unwrap().result.clone();
            let design_sha256 = session.design.as_ref().unwrap().evidence_sha256.clone();
            let calibration_sha256 = session.calibration.as_ref().unwrap().summary.sha256.clone();
            for (channel, predicted) in [
                (LiveChannel::Left, &result.left_predicted_position_db[0]),
                (LiveChannel::Right, &result.right_predicted_position_db[0]),
            ] {
                let mut verified = session
                    .measurements
                    .get(&(LiveCaptureKind::Baseline, "P0".to_string(), channel))
                    .unwrap()
                    .clone();
                verified.summary.kind = LiveCaptureKind::Verification;
                verified.magnitude_db[..predicted.len()].copy_from_slice(predicted);
                verified.evidence = LiveCaptureEvidence {
                    session_id: session.id.clone(),
                    generation: session.evidence_generation,
                    system_mode: session.system_mode,
                    subwoofer_setup: session.subwoofer_setup.clone(),
                    subwoofer_search: session.subwoofer_search.clone(),
                    calibration_sha256: calibration_sha256.clone(),
                    sweep_sha256: session.sweeps[&channel].summary.sha256.clone(),
                    design_sha256: Some(design_sha256.clone()),
                    input_device_id: "synthetic::input".to_string(),
                    input_channel_index: 0,
                };
                session.measurements.insert(
                    (LiveCaptureKind::Verification, "P0".to_string(), channel),
                    verified,
                );
            }
        }
        {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().unwrap();
            session
                .measurements
                .get_mut(&(
                    LiveCaptureKind::Verification,
                    "P0".to_string(),
                    LiveChannel::Left,
                ))
                .unwrap()
                .evidence
                .design_sha256 = Some("stale-trial-filter".to_string());
        }
        assert!(state
            .verification_summary()
            .unwrap_err()
            .contains("different trial filter"));
        {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().unwrap();
            let current_design_sha256 = session.design.as_ref().unwrap().evidence_sha256.clone();
            session
                .measurements
                .get_mut(&(
                    LiveCaptureKind::Verification,
                    "P0".to_string(),
                    LiveChannel::Left,
                ))
                .unwrap()
                .evidence
                .design_sha256 = Some(current_design_sha256);
        }
        {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().unwrap();
            for channel in [LiveChannel::Left, LiveChannel::Right] {
                let inactive_response = session
                    .measurements
                    .get(&(LiveCaptureKind::Baseline, "P0".to_string(), channel))
                    .unwrap()
                    .magnitude_db
                    .clone();
                session
                    .measurements
                    .get_mut(&(LiveCaptureKind::Verification, "P0".to_string(), channel))
                    .unwrap()
                    .magnitude_db = inactive_response;
            }
        }
        let inactive_trial = state.verification_summary().unwrap();
        assert!(!inactive_trial.passed);
        assert!(
            inactive_trial
                .issues
                .iter()
                .any(|issue| issue.contains("_verification:")),
            "an absent trial must still fail target-improvement gates: {:?}",
            inactive_trial.issues
        );
        {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().unwrap();
            let result = session.design.as_ref().unwrap().result.clone();
            for (channel, predicted) in [
                (LiveChannel::Left, &result.left_predicted_position_db[0]),
                (LiveChannel::Right, &result.right_predicted_position_db[0]),
            ] {
                session
                    .measurements
                    .get_mut(&(LiveCaptureKind::Verification, "P0".to_string(), channel))
                    .unwrap()
                    .magnitude_db[..predicted.len()]
                    .copy_from_slice(predicted);
            }
        }
        let exported = state.finalize_export().unwrap();
        assert_eq!(exported.native_rate_count, 6);
        assert!(exported.cross_rate_passed);
        assert!(exported.verification.passed);
        assert_eq!(
            exported.verification.algorithm_version,
            LIVE_CLOSED_LOOP_VERSION
        );
        assert_eq!(
            exported
                .verification
                .prediction_verification_smoothing_fwhm_octaves,
            eqforbeginner_dsp_core::validation::LOG_FREQUENCY_SMOOTHING_EFFECTIVE_FWHM_OCTAVES
        );
        assert!(exported
            .verification
            .left_unsmoothed_predicted_verified_rmse_db
            .is_finite());
        assert!(exported
            .verification
            .right_unsmoothed_predicted_verified_rmse_db
            .is_finite());
        let plot = &exported.verification.frequency_response;
        assert_eq!(plot.algorithm_version, LIVE_RESULT_PLOT_VERSION);
        assert_eq!(
            plot.display_smoothing_fwhm_octaves,
            LIVE_RESULT_PLOT_SMOOTHING_FWHM_OCTAVES
        );
        assert!(plot.frequencies_hz.len() >= 400);
        assert!(plot.frequencies_hz[0] <= 20.1);
        assert!(plot.frequencies_hz.last().copied().unwrap() >= 19_000.0);
        for series in [
            &plot.raw_left_db,
            &plot.raw_right_db,
            &plot.raw_average_db,
            &plot.target_left_db,
            &plot.target_right_db,
            &plot.target_average_db,
            &plot.predicted_left_db,
            &plot.predicted_right_db,
            &plot.predicted_average_db,
            &plot.verified_left_db,
            &plot.verified_right_db,
            &plot.verified_average_db,
        ] {
            assert_eq!(series.len(), plot.frequencies_hz.len());
            assert!(series.iter().all(|value| value.is_finite()));
        }
        assert!(
            exported.final_48k_binding_maximum_magnitude_difference_db
                <= Phase6Config::default().maximum_magnitude_difference_db
        );
        assert!(
            exported.final_48k_binding_maximum_relative_group_delay_difference_ms
                <= Phase6Config::default().maximum_relative_group_delay_difference_ms
        );
        assert!(exported.fir_worst_case_peak_bound_db.is_finite());
        assert!(exported.recommended_headroom_db >= HEADROOM_SAFETY_MARGIN_DB);
        assert!(Path::new(&exported.zip_path).exists());
        assert!(Path::new(&exported.project_path).exists());
        let final_download_path = temporary.path().join("downloaded-final.zip");
        let final_download = state
            .save_zip_artifact(LiveZipArtifactKind::Final, &final_download_path)
            .unwrap();
        assert_eq!(final_download.artifact_kind, LiveZipArtifactKind::Final);
        assert_eq!(final_download.sha256, exported.zip_sha256);
        assert_eq!(
            fs::read(&final_download_path).unwrap(),
            fs::read(&exported.zip_path).unwrap()
        );
        validate_roon_six_rate_zip(&final_download_path).unwrap();
    }

    /// Field regression for LIVE_CLOSED_LOOP_VERSION v4 plus the
    /// redesign-and-reverify loop. Part one reproduces the first real v5
    /// session's failure shape: the verified response follows the prediction,
    /// but narrow comb displacement from a small microphone reposition sits in
    /// the 300-500 Hz band, where the old unsmoothed linear judgment held 31%
    /// of its bins - it failed a working filter. Part two proves a genuinely
    /// regressed verification still fails, and that the user can then
    /// recapture the baseline, redesign, redeclare, reverify, and export.
    #[test]
    fn a_repositioned_verification_passes_v4_and_a_failure_supports_redesign() {
        let temporary = tempdir().unwrap();
        let state = LiveMeasurementState::default();
        state.start_session(temporary.path()).unwrap();
        state
            .import_calibration("umik.txt", "10 0\n24000 0\n")
            .unwrap();
        let wav = test_sweep_wav();
        state.import_sweep(LiveChannel::Left, &wav).unwrap();
        state.import_sweep(LiveChannel::Right, &wav).unwrap();
        let capture_p0 = |state: &LiveMeasurementState| {
            for channel in [LiveChannel::Left, LiveChannel::Right] {
                let (_, _, sweep, calibration, evidence, _) = state
                    .begin_capture(
                        LiveCaptureKind::Baseline,
                        channel,
                        "P0",
                        "synthetic::input",
                        0,
                    )
                    .unwrap();
                let capture = synthetic_capture(&sweep.samples, &[0.2]);
                let summary = analyze_and_store_capture(
                    state,
                    LiveCaptureKind::Baseline,
                    channel,
                    "P0".into(),
                    &sweep,
                    &calibration,
                    &evidence,
                    &capture,
                )
                .unwrap();
                state.finish_capture();
                assert!(summary.accepted, "{:?}", summary.issue_codes);
            }
        };
        let fixture = SyntheticRoomFixture::phase1_48k().unwrap();
        let inject_fixture_p0 = |state: &LiveMeasurementState| {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().unwrap();
            for (channel, impulse) in [
                (LiveChannel::Left, &fixture.left_impulses[0]),
                (LiveChannel::Right, &fixture.right_impulses[0]),
            ] {
                let response = frequency_response(impulse, 48_000, 32_768).unwrap();
                let indices = response
                    .frequencies_hz
                    .iter()
                    .enumerate()
                    .filter_map(|(index, frequency)| {
                        (*frequency > 0.0 && *frequency <= 20_000.0).then_some(index)
                    })
                    .collect::<Vec<_>>();
                let stored = session
                    .measurements
                    .get_mut(&(LiveCaptureKind::Baseline, "P0".to_string(), channel))
                    .unwrap();
                stored.frequencies_hz = indices
                    .iter()
                    .map(|index| response.frequencies_hz[*index])
                    .collect();
                stored.magnitude_db = indices
                    .iter()
                    .map(|index| response.magnitude_db[*index])
                    .collect();
                stored.calibrated_impulse_samples = impulse.clone();
            }
        };
        capture_p0(&state);
        inject_fixture_p0(&state);
        let target_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../assets/targets/Harman-6dB_REW.txt");
        state
            .import_target(
                "Harman-6dB_REW.txt",
                &fs::read_to_string(&target_path).unwrap(),
            )
            .unwrap();
        let first_design = state.design_trial("custom").unwrap();
        assert!(first_design.numerical_passed);
        assert!(state.set_trial_activation(true).unwrap());

        // Install a verification pair: exactly the prediction, plus alternating
        // +/-4 dB single-bin ripple confined to 300-500 Hz - the shape a small
        // P0 reposition produces and a filter cannot have caused.
        let install_verification =
            |state: &LiveMeasurementState,
             distort: &dyn Fn(LiveChannel, usize, f64, f64) -> f64| {
                let mut guard = state.session.lock().unwrap();
                let session = guard.as_mut().unwrap();
                let result = session.design.as_ref().unwrap().result.clone();
                let design_sha256 = session.design.as_ref().unwrap().evidence_sha256.clone();
                let calibration_sha256 =
                    session.calibration.as_ref().unwrap().summary.sha256.clone();
                for (channel, predicted) in [
                    (LiveChannel::Left, &result.left_predicted_position_db[0]),
                    (LiveChannel::Right, &result.right_predicted_position_db[0]),
                ] {
                    let mut verified = session
                        .measurements
                        .get(&(LiveCaptureKind::Baseline, "P0".to_string(), channel))
                        .unwrap()
                        .clone();
                    verified.summary.kind = LiveCaptureKind::Verification;
                    let grid = verified.frequencies_hz.clone();
                    for (index, value) in predicted.iter().enumerate() {
                        verified.magnitude_db[index] = distort(channel, index, grid[index], *value);
                    }
                    verified.evidence = LiveCaptureEvidence {
                        session_id: session.id.clone(),
                        generation: session.evidence_generation,
                        system_mode: session.system_mode,
                        subwoofer_setup: session.subwoofer_setup.clone(),
                        subwoofer_search: session.subwoofer_search.clone(),
                        calibration_sha256: calibration_sha256.clone(),
                        sweep_sha256: session.sweeps[&channel].summary.sha256.clone(),
                        design_sha256: Some(design_sha256.clone()),
                        input_device_id: "synthetic::input".to_string(),
                        input_channel_index: 0,
                    };
                    session.measurements.insert(
                        (LiveCaptureKind::Verification, "P0".to_string(), channel),
                        verified,
                    );
                }
            };
        install_verification(&state, &|channel, index, frequency, predicted| {
            if channel == LiveChannel::Left && (300.0..500.0).contains(&frequency) {
                predicted + if index % 2 == 0 { 4.0 } else { -4.0 }
            } else {
                predicted
            }
        });
        let repositioned = state.verification_summary().unwrap();
        assert!(
            repositioned.left_verified_rmse_db > repositioned.left_raw_rmse_db,
            "the ripple must be large enough that the old unsmoothed judgment \
             would have failed: {} vs {}",
            repositioned.left_verified_rmse_db,
            repositioned.left_raw_rmse_db
        );
        assert!(repositioned.passed, "{:?}", repositioned.issues);
        let gate_raw = repositioned.left_gate_raw_rmse_db.unwrap();
        let gate_verified = repositioned.left_gate_verified_rmse_db.unwrap();
        assert!(
            gate_verified < gate_raw,
            "smoothed octave gate: {gate_verified} vs {gate_raw}"
        );

        // A genuinely broken verification - a broad 4 dB error across two full
        // octaves - must still fail, because broad errors survive smoothing.
        install_verification(&state, &|_, _, frequency, predicted| {
            if (80.0..320.0).contains(&frequency) {
                predicted + 4.0
            } else {
                predicted
            }
        });
        let regressed = state.verification_summary().unwrap();
        assert!(!regressed.passed);
        assert!(
            regressed
                .issues
                .iter()
                .any(|issue| issue.contains("SmoothedOctaveRmseDidNotImprove")),
            "{:?}",
            regressed.issues
        );

        // The retry loop: recapture the baseline, redesign, redeclare,
        // reverify. The new baseline invalidates the old design and trial.
        capture_p0(&state);
        {
            let guard = state.session.lock().unwrap();
            assert!(
                guard.as_ref().unwrap().design.is_none(),
                "a recaptured baseline must invalidate the previous design"
            );
        }
        inject_fixture_p0(&state);
        let second_design = state.design_trial("custom").unwrap();
        assert!(second_design.numerical_passed);
        assert_ne!(
            first_design.trial_zip_path, second_design.trial_zip_path,
            "the redesign must produce a new trial artifact"
        );
        assert!(state.set_trial_activation(true).unwrap());
        install_verification(&state, &|_, _, _, predicted| predicted);
        let retried = state.verification_summary().unwrap();
        assert!(retried.passed, "{:?}", retried.issues);
        state.finalize_export().unwrap();
    }
}

fn select_design_band(values: &[f64], expected_length: usize) -> Result<Vec<f64>, String> {
    if values.len() < expected_length {
        return Err(format!(
            "measurement has {} response bins but design requires {expected_length}",
            values.len()
        ));
    }
    Ok(values[..expected_length].to_vec())
}

fn measurement_at<'session>(
    session: &'session LiveSession,
    kind: LiveCaptureKind,
    position_id: &str,
    channel: LiveChannel,
) -> Result<&'session StoredMeasurement, String> {
    let measurement = session
        .measurements
        .get(&(kind, position_id.to_string(), channel))
        .filter(|measurement| measurement.summary.accepted)
        .ok_or_else(|| {
            format!(
                "an accepted {} {position_id} {} measurement is required",
                kind.as_str(),
                channel.as_str()
            )
        })?;
    validate_stored_evidence(session, measurement)?;
    Ok(measurement)
}

fn p0_measurement(
    session: &LiveSession,
    kind: LiveCaptureKind,
    channel: LiveChannel,
) -> Result<&StoredMeasurement, String> {
    measurement_at(session, kind, "P0", channel)
}

fn rmse_between(left: &[f64], right: &[f64]) -> Result<f64, String> {
    if left.len() != right.len() || left.is_empty() {
        return Err("RMSE inputs must have equal nonzero lengths".to_string());
    }
    Ok((left
        .iter()
        .zip(right)
        .map(|(left, right)| (left - right).powi(2))
        .sum::<f64>()
        / left.len() as f64)
        .sqrt())
}

fn energy_average_pair(left: &[f64], right: &[f64]) -> Result<Vec<f64>, String> {
    if left.len() != right.len() || left.is_empty() {
        return Err("L/R energy-average inputs must have equal nonzero lengths".to_string());
    }
    left.iter()
        .zip(right)
        .enumerate()
        .map(|(index, (left, right))| {
            if !left.is_finite() || !right.is_finite() {
                return Err(format!(
                    "L/R energy-average input is non-finite at bin {index}"
                ));
            }
            let maximum = left.max(*right);
            let average = maximum
                + 10.0
                    * ((10.0_f64.powf((left - maximum) / 10.0)
                        + 10.0_f64.powf((right - maximum) / 10.0))
                        / 2.0)
                        .log10();
            if !average.is_finite() {
                return Err(format!(
                    "L/R energy average became non-finite at bin {index}"
                ));
            }
            Ok(average)
        })
        .collect()
}

fn result_plot_frequency_grid(
    source_frequencies_hz: &[f64],
    point_count: usize,
) -> Result<Vec<f64>, String> {
    let first = source_frequencies_hz
        .first()
        .copied()
        .ok_or_else(|| "result plot has no source frequency grid".to_string())?;
    let last = source_frequencies_hz
        .last()
        .copied()
        .ok_or_else(|| "result plot has no source frequency grid".to_string())?;
    let minimum_hz = first.max(20.0);
    let maximum_hz = last.min(20_000.0);
    if point_count < 2
        || !minimum_hz.is_finite()
        || !maximum_hz.is_finite()
        || minimum_hz <= 0.0
        || maximum_hz <= minimum_hz
    {
        return Err("result plot frequency bounds are invalid".to_string());
    }
    let ratio = maximum_hz / minimum_hz;
    let mut frequencies_hz = (0..point_count)
        .map(|index| {
            let fraction = index as f64 / (point_count - 1) as f64;
            minimum_hz * ratio.powf(fraction)
        })
        .collect::<Vec<_>>();
    for boundary in [500.0, 650.0] {
        if boundary > minimum_hz && boundary < maximum_hz {
            frequencies_hz.push(boundary);
        }
    }
    frequencies_hz.sort_by(f64::total_cmp);
    frequencies_hz.dedup_by(|left, right| (*left - *right).abs() <= 1.0e-9);
    Ok(frequencies_hz)
}

fn protected_dip_centers(frequencies_hz: &[f64], mask: &[bool]) -> Vec<f64> {
    if frequencies_hz.len() != mask.len() {
        return Vec::new();
    }
    let mut centers = Vec::new();
    let mut index = 0;
    while index < mask.len() {
        if !mask[index] || frequencies_hz[index] < 20.0 || frequencies_hz[index] > 500.0 {
            index += 1;
            continue;
        }
        let start = index;
        while index + 1 < mask.len() && mask[index + 1] && frequencies_hz[index + 1] <= 500.0 {
            index += 1;
        }
        let end = index;
        centers.push(
            (frequencies_hz[start].ln() + frequencies_hz[end].ln())
                .mul_add(0.5, 0.0)
                .exp(),
        );
        index += 1;
    }
    centers.truncate(12);
    centers
}

fn corrected_peak_centers(
    frequencies_hz: &[f64],
    left_gain_db: &[f64],
    right_gain_db: &[f64],
) -> Vec<f64> {
    if frequencies_hz.len() != left_gain_db.len()
        || frequencies_hz.len() != right_gain_db.len()
        || frequencies_hz.len() < 3
    {
        return Vec::new();
    }
    let attenuation = left_gain_db
        .iter()
        .zip(right_gain_db)
        .map(|(left, right)| (-left).max(-right).max(0.0))
        .collect::<Vec<_>>();
    let mut candidates = (1..frequencies_hz.len() - 1)
        .filter_map(|index| {
            let frequency = frequencies_hz[index];
            let value = attenuation[index];
            ((20.0..=500.0).contains(&frequency)
                && value >= 0.75
                && value >= attenuation[index - 1]
                && value >= attenuation[index + 1]
                && (value > attenuation[index - 1] || value > attenuation[index + 1]))
                .then_some((frequency, value))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.1.total_cmp(&left.1));
    let mut selected = Vec::new();
    for (frequency, _) in candidates {
        if selected.iter().all(|selected_frequency: &f64| {
            (frequency / *selected_frequency).log2().abs() >= 1.0 / 12.0
        }) {
            selected.push(frequency);
        }
        if selected.len() == 12 {
            break;
        }
    }
    selected.sort_by(f64::total_cmp);
    selected
}

fn build_frequency_response_plot(
    design: &LiveDesign,
    verified_left: &StoredMeasurement,
    verified_right: &StoredMeasurement,
) -> Result<LiveFrequencyResponsePlot, String> {
    let source_frequencies_hz = &design.response_set.frequencies_hz;
    let weights = &design.result.position_weights;
    let raw_left_positions = design
        .response_set
        .positions
        .iter()
        .map(|position| position.left_magnitude_db.clone())
        .collect::<Vec<_>>();
    let raw_right_positions = design
        .response_set
        .positions
        .iter()
        .map(|position| position.right_magnitude_db.clone())
        .collect::<Vec<_>>();
    let raw_left = weighted_energy_average_db(&raw_left_positions, weights).map_err(|error| {
        format!("could not summarize raw L responses for the result plot: {error}")
    })?;
    let raw_right = weighted_energy_average_db(&raw_right_positions, weights).map_err(|error| {
        format!("could not summarize raw R responses for the result plot: {error}")
    })?;
    let predicted_left =
        weighted_energy_average_db(&design.result.left_predicted_position_db, weights).map_err(
            |error| {
                format!("could not summarize predicted L responses for the result plot: {error}")
            },
        )?;
    let predicted_right =
        weighted_energy_average_db(&design.result.right_predicted_position_db, weights).map_err(
            |error| {
                format!("could not summarize predicted R responses for the result plot: {error}")
            },
        )?;
    let plot_frequencies_hz = result_plot_frequency_grid(source_frequencies_hz, 420)?;
    let interpolate = |values: &[f64], label: &str| {
        interpolate_log_frequency_grid(source_frequencies_hz, values, &plot_frequencies_hz)
            .map_err(|error| format!("could not interpolate {label} for the result plot: {error}"))
    };
    let smooth = |values: &[f64], label: &str| {
        gaussian_log_frequency_smooth_at_db(
            source_frequencies_hz,
            values,
            &plot_frequencies_hz,
            LIVE_RESULT_PLOT_SMOOTHING_FWHM_OCTAVES,
        )
        .map_err(|error| format!("could not smooth {label} for the result plot: {error}"))
    };
    let raw_left_plot = smooth(&raw_left, "raw L spatial average")?;
    let raw_right_plot = smooth(&raw_right, "raw R spatial average")?;
    let raw_average_plot = energy_average_pair(&raw_left_plot, &raw_right_plot)?;
    let target_left_plot = interpolate(&design.full_left_aligned_target_db, "aligned L target")?;
    let target_right_plot = interpolate(&design.full_right_aligned_target_db, "aligned R target")?;
    let target_average_plot = energy_average_pair(&target_left_plot, &target_right_plot)?;
    let predicted_left_plot = smooth(&predicted_left, "predicted L spatial average")?;
    let predicted_right_plot = smooth(&predicted_right, "predicted R spatial average")?;
    let predicted_average_plot = energy_average_pair(&predicted_left_plot, &predicted_right_plot)?;
    let verified_left_plot = smooth(&verified_left.magnitude_db, "verified P0 L")?;
    let verified_right_plot = smooth(&verified_right.magnitude_db, "verified P0 R")?;
    let verified_average_plot = energy_average_pair(&verified_left_plot, &verified_right_plot)?;
    let protected_mask = design
        .result
        .left_design
        .protected_dip
        .iter()
        .zip(&design.result.right_design.protected_dip)
        .map(|(left, right)| *left || *right)
        .collect::<Vec<_>>();
    Ok(LiveFrequencyResponsePlot {
        algorithm_version: LIVE_RESULT_PLOT_VERSION,
        display_smoothing_fwhm_octaves: LIVE_RESULT_PLOT_SMOOTHING_FWHM_OCTAVES,
        frequencies_hz: plot_frequencies_hz.clone(),
        raw_left_db: raw_left_plot,
        raw_right_db: raw_right_plot,
        raw_average_db: raw_average_plot,
        target_left_db: target_left_plot,
        target_right_db: target_right_plot,
        target_average_db: target_average_plot,
        predicted_left_db: predicted_left_plot,
        predicted_right_db: predicted_right_plot,
        predicted_average_db: predicted_average_plot,
        verified_left_db: verified_left_plot,
        verified_right_db: verified_right_plot,
        verified_average_db: verified_average_plot,
        correction_low_hz: 20.0,
        correction_high_hz: 500.0,
        taper_end_hz: 650.0,
        protected_dip_frequencies_hz: protected_dip_centers(
            &design.result.design_frequencies_hz,
            &protected_mask,
        ),
        corrected_peak_frequencies_hz: corrected_peak_centers(
            &design.result.design_frequencies_hz,
            &design.result.measured_grid_stereo_design.left_gain_db,
            &design.result.measured_grid_stereo_design.right_gain_db,
        ),
    })
}

/// Octave cells for the improvement judgment, spanning the correction band.
/// The last cell ends at the 500 Hz full-correction edge: between 500 and
/// 650 Hz the filter is a raised-cosine return to unity, so raw/verified
/// differences there are measurement variation the filter cannot have caused.
const IMPROVEMENT_GATE_CELLS_HZ: [(f64, f64); 5] = [
    (20.0, 40.0),
    (40.0, 80.0),
    (80.0, 160.0),
    (160.0, 320.0),
    (320.0, 500.0),
];

/// RMS across octave cells of the per-cell RMSE against the aligned target.
/// `None` when any cell lacks bins, in which case the caller keeps the plain
/// unsmoothed judgment so the gate can never silently weaken.
fn octave_cell_rmse_db(frequencies_hz: &[f64], curve_db: &[f64], target_db: &[f64]) -> Option<f64> {
    let mut cell_mean_squares = Vec::with_capacity(IMPROVEMENT_GATE_CELLS_HZ.len());
    for (low_hz, high_hz) in IMPROVEMENT_GATE_CELLS_HZ {
        let mut sum = 0.0;
        let mut count = 0_usize;
        for ((frequency, value), target) in frequencies_hz.iter().zip(curve_db).zip(target_db) {
            if (low_hz..high_hz).contains(frequency) && value.is_finite() && target.is_finite() {
                let error = value - target;
                sum += error * error;
                count += 1;
            }
        }
        if count < 4 {
            return None;
        }
        cell_mean_squares.push(sum / count as f64);
    }
    Some((cell_mean_squares.iter().sum::<f64>() / cell_mean_squares.len() as f64).sqrt())
}

/// Least-squares scale of the designed correction observed in a verification
/// capture, fitted on the gate-smoothed curves:
/// `verified - raw ~= scale * (predicted - raw)`. `None` when the designed
/// correction is too small in the agreement band to carry a scale.
fn fitted_applied_correction_scale(
    frequencies_hz: &[f64],
    raw_band: &[f64],
    predicted_band: &[f64],
    verified_band: &[f64],
) -> Option<f64> {
    let correction: Vec<f64> = predicted_band
        .iter()
        .zip(raw_band)
        .map(|(predicted, raw)| predicted - raw)
        .collect();
    let response: Vec<f64> = verified_band
        .iter()
        .zip(raw_band)
        .map(|(verified, raw)| verified - raw)
        .collect();
    let correction_smoothed = eqforbeginner_dsp_core::validation::log_frequency_smoothed_curve(
        frequencies_hz,
        &correction,
    );
    let response_smoothed =
        eqforbeginner_dsp_core::validation::log_frequency_smoothed_curve(frequencies_hz, &response);
    let correction_energy: f64 = correction_smoothed.iter().map(|value| value * value).sum();
    let correction_rms = (correction_energy / correction_smoothed.len().max(1) as f64).sqrt();
    if correction_rms < MINIMUM_SCALE_FIT_CORRECTION_RMS_DB || correction_energy <= 0.0 {
        return None;
    }
    let cross: f64 = correction_smoothed
        .iter()
        .zip(&response_smoothed)
        .map(|(correction, response)| correction * response)
        .sum();
    Some(cross / correction_energy)
}

fn validate_closed_loop(session: &LiveSession) -> Result<LiveVerificationSummary, String> {
    let design = session
        .design
        .as_ref()
        .ok_or_else(|| "create a Phase 4 trial filter before verification".to_string())?;
    let design_length = design.result.design_frequencies_hz.len();
    let raw_left = p0_measurement(session, LiveCaptureKind::Baseline, LiveChannel::Left)?;
    let raw_right = p0_measurement(session, LiveCaptureKind::Baseline, LiveChannel::Right)?;
    let verified_left = p0_measurement(session, LiveCaptureKind::Verification, LiveChannel::Left)?;
    let verified_right =
        p0_measurement(session, LiveCaptureKind::Verification, LiveChannel::Right)?;
    for measurement in [raw_left, raw_right, verified_left, verified_right] {
        if measurement.frequencies_hz != design.response_set.frequencies_hz {
            return Err(format!(
                "measurement grid changed for {} {}",
                measurement.summary.kind.as_str(),
                measurement.summary.channel.as_str()
            ));
        }
    }
    // Session-gain gate (2026-07-29 expert review, finding 5): the >=650 Hz
    // marker band is untouched by the correction, so a marker RMS shift
    // between the baseline and verification captures is a playback/capture
    // volume change - which would otherwise fail the improvement and
    // agreement gates for reasons that have nothing to do with the filter.
    // No compensation is applied (levels are never silently aligned); the
    // user is told to restore the volume and remeasure. Captures restored
    // from caches that predate marker-level recording skip the gate.
    let mut session_gain_issues = Vec::new();
    for (label, raw, verified) in [
        ("left", raw_left, verified_left),
        ("right", raw_right, verified_right),
    ] {
        if let (Some(baseline_rms), Some(verified_rms)) = (
            raw.summary.start_marker_rms_dbfs,
            verified.summary.start_marker_rms_dbfs,
        ) {
            let shift_db = verified_rms - baseline_rms;
            if shift_db.abs() > MAXIMUM_VERIFICATION_MARKER_LEVEL_SHIFT_DB {
                session_gain_issues.push(format!(
                    "{label}_session_gain_shifted:{shift_db:+.2}dB/allowed{MAXIMUM_VERIFICATION_MARKER_LEVEL_SHIFT_DB:.2}dB"
                ));
            }
        }
    }
    let raw_left_band = select_design_band(&raw_left.magnitude_db, design_length)?;
    let raw_right_band = select_design_band(&raw_right.magnitude_db, design_length)?;
    let verified_left_band = select_design_band(&verified_left.magnitude_db, design_length)?;
    let verified_right_band = select_design_band(&verified_right.magnitude_db, design_length)?;
    let thresholds = ValidationThresholds::default();
    let left_report = validate_frequency_prediction(
        &design.result.design_frequencies_hz,
        std::slice::from_ref(&raw_left_band),
        std::slice::from_ref(&verified_left_band),
        &[1.0],
        &design.result.left_design.aligned_target_db,
        &design.result.left_realized_response_db,
        None,
        &thresholds,
    )
    .map_err(|error| format!("left closed-loop validation failed: {error}"))?;
    let right_report = validate_frequency_prediction(
        &design.result.design_frequencies_hz,
        std::slice::from_ref(&raw_right_band),
        std::slice::from_ref(&verified_right_band),
        &[1.0],
        &design.result.right_design.aligned_target_db,
        &design.result.right_realized_response_db,
        None,
        &thresholds,
    )
    .map_err(|error| format!("right closed-loop validation failed: {error}"))?;
    let p0_index = design
        .response_set
        .positions
        .iter()
        .position(|position| position.id == "P0")
        .ok_or_else(|| "designed response set lost P0".to_string())?;
    let predicted_left = design
        .result
        .left_predicted_position_db
        .get(p0_index)
        .ok_or_else(|| "left prediction lost P0".to_string())?;
    let predicted_right = design
        .result
        .right_predicted_position_db
        .get(p0_index)
        .ok_or_else(|| "right prediction lost P0".to_string())?;
    let predicted_left_band = select_design_band(predicted_left, design_length)?;
    let predicted_right_band = select_design_band(predicted_right, design_length)?;
    let agreement_indices = design
        .result
        .design_frequencies_hz
        .iter()
        .enumerate()
        .filter_map(|(index, frequency)| {
            (PREDICTION_VERIFICATION_LOW_HZ..=PREDICTION_VERIFICATION_HIGH_HZ)
                .contains(frequency)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if agreement_indices.is_empty() {
        return Err("closed-loop prediction agreement band has no response bins".to_string());
    }
    let agreement_frequencies_hz = agreement_indices
        .iter()
        .map(|index| design.result.design_frequencies_hz[*index])
        .collect::<Vec<_>>();
    let select_agreement_band = |values: &[f64]| {
        agreement_indices
            .iter()
            .map(|index| values[*index])
            .collect::<Vec<_>>()
    };
    let predicted_left_agreement_band = select_agreement_band(&predicted_left_band);
    let predicted_right_agreement_band = select_agreement_band(&predicted_right_band);
    let verified_left_agreement_band = select_agreement_band(&verified_left_band);
    let verified_right_agreement_band = select_agreement_band(&verified_right_band);
    let raw_left_agreement_band =
        select_agreement_band(&select_design_band(&raw_left.magnitude_db, design_length)?);
    let raw_right_agreement_band =
        select_agreement_band(&select_design_band(&raw_right.magnitude_db, design_length)?);
    // Applied-correction scale (2026-07-29 expert review, finding 5):
    // least-squares fit of
    // `verified - raw ~= s * (predicted - raw)` on the gate-smoothed curves.
    // s ~ 0 is an unloaded filter, ~2 a double convolution, negative an
    // inversion - cases the residual gate alone can pass. The fit is skipped
    // when the designed correction is too small to carry a scale (flat room).
    let left_applied_correction_scale = fitted_applied_correction_scale(
        &agreement_frequencies_hz,
        &raw_left_agreement_band,
        &predicted_left_agreement_band,
        &verified_left_agreement_band,
    );
    let right_applied_correction_scale = fitted_applied_correction_scale(
        &agreement_frequencies_hz,
        &raw_right_agreement_band,
        &predicted_right_agreement_band,
        &verified_right_agreement_band,
    );
    let left_unsmoothed_predicted_verified_rmse_db = rmse_between(
        &predicted_left_agreement_band,
        &verified_left_agreement_band,
    )?;
    let right_unsmoothed_predicted_verified_rmse_db = rmse_between(
        &predicted_right_agreement_band,
        &verified_right_agreement_band,
    )?;
    let left_predicted_verified_rmse_db = log_frequency_smoothed_rmse_db(
        &agreement_frequencies_hz,
        &predicted_left_agreement_band,
        &verified_left_agreement_band,
    )
    .map_err(|error| format!("left prediction-agreement scoring failed: {error}"))?;
    let right_predicted_verified_rmse_db = log_frequency_smoothed_rmse_db(
        &agreement_frequencies_hz,
        &predicted_right_agreement_band,
        &verified_right_agreement_band,
    )
    .map_err(|error| format!("right prediction-agreement scoring failed: {error}"))?;
    // Improvement judgment on the gate-smoothed curves with octave-cell
    // weighting (see LIVE_CLOSED_LOOP_VERSION). The reports' own unsmoothed
    // linear judgment is replaced; every other report issue stands unchanged.
    let smoothed_cell_rmse = |values_db: &[f64], target_db: &[f64]| -> Option<f64> {
        let smoothed =
            log_frequency_smoothed_curve(&design.result.design_frequencies_hz, values_db);
        octave_cell_rmse_db(&design.result.design_frequencies_hz, &smoothed, target_db)
    };
    let left_gate_raw_rmse_db =
        smoothed_cell_rmse(&raw_left_band, &design.result.left_design.aligned_target_db);
    let left_gate_verified_rmse_db = smoothed_cell_rmse(
        &verified_left_band,
        &design.result.left_design.aligned_target_db,
    );
    let right_gate_raw_rmse_db = smoothed_cell_rmse(
        &raw_right_band,
        &design.result.right_design.aligned_target_db,
    );
    let right_gate_verified_rmse_db = smoothed_cell_rmse(
        &verified_right_band,
        &design.result.right_design.aligned_target_db,
    );
    let left_gate_predicted_rmse_db = smoothed_cell_rmse(
        &predicted_left_band,
        &design.result.left_design.aligned_target_db,
    );
    let right_gate_predicted_rmse_db = smoothed_cell_rmse(
        &predicted_right_band,
        &design.result.right_design.aligned_target_db,
    );
    let mut issues = Vec::new();
    let mut improvement_passed = [true, true];
    for (index, (channel, report, gate_raw, gate_verified)) in [
        (
            LiveChannel::Left,
            &left_report,
            left_gate_raw_rmse_db,
            left_gate_verified_rmse_db,
        ),
        (
            LiveChannel::Right,
            &right_report,
            right_gate_raw_rmse_db,
            right_gate_verified_rmse_db,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for issue in &report.issues {
            match issue {
                ValidationIssue::OverallRmseDidNotImprove { .. } => {
                    // Replaced below by the smoothed octave-cell judgment.
                }
                other => issues.push(format!("{}_verification:{other:?}", channel.as_str())),
            }
        }
        match (gate_raw, gate_verified) {
            (Some(raw_db), Some(verified_db)) => {
                // Pass when the measurement improved on the unfiltered
                // baseline, or when it delivered at least the improvement the
                // accepted design predicted on this same metric. The second
                // branch matters when a design's improvement lives in
                // features narrower than the gate smoothing (the synthetic
                // fixture's single-bin modes); an unloaded filter fails both
                // branches, because its measurement equals the baseline while
                // the prediction sits below it.
                let predicted_db = match index {
                    0 => left_gate_predicted_rmse_db,
                    _ => right_gate_predicted_rmse_db,
                };
                let improved_on_raw = verified_db < raw_db;
                let delivered_prediction =
                    predicted_db.is_some_and(|predicted_db| verified_db <= predicted_db);
                if !improved_on_raw && !delivered_prediction {
                    improvement_passed[index] = false;
                    issues.push(format!(
                        "{}_verification:SmoothedOctaveRmseDidNotImprove {{ raw_db: {raw_db:.4}, verified_db: {verified_db:.4}, predicted_db: {} }}",
                        channel.as_str(),
                        predicted_db.map_or("unavailable".to_string(), |value| format!("{value:.4}")),
                    ));
                }
            }
            _ => {
                // Degenerate grid: keep the original unsmoothed judgment so
                // the gate never weakens when its inputs are unavailable.
                if let Some(original) = report
                    .issues
                    .iter()
                    .find(|issue| matches!(issue, ValidationIssue::OverallRmseDidNotImprove { .. }))
                {
                    improvement_passed[index] = false;
                    issues.push(format!("{}_verification:{original:?}", channel.as_str()));
                }
            }
        }
    }
    issues.extend(session_gain_issues);
    for (label, scale) in [
        ("left", left_applied_correction_scale),
        ("right", right_applied_correction_scale),
    ] {
        if let Some(scale) = scale {
            if !(MINIMUM_APPLIED_CORRECTION_SCALE..=MAXIMUM_APPLIED_CORRECTION_SCALE)
                .contains(&scale)
            {
                issues.push(format!(
                    "{label}_applied_correction_scale:{scale:.2}/allowed{MINIMUM_APPLIED_CORRECTION_SCALE:.2}-{MAXIMUM_APPLIED_CORRECTION_SCALE:.2}"
                ));
            }
        }
    }
    if left_predicted_verified_rmse_db > MAXIMUM_PREDICTED_VERIFIED_RMSE_DB {
        issues.push(format!(
            "left_smoothed_predicted_verified_rmse:{left_predicted_verified_rmse_db:.3}dB"
        ));
    }
    if right_predicted_verified_rmse_db > MAXIMUM_PREDICTED_VERIFIED_RMSE_DB {
        issues.push(format!(
            "right_smoothed_predicted_verified_rmse:{right_predicted_verified_rmse_db:.3}dB"
        ));
    }
    let non_improvement_issues_pass = |report: &ValidationReport| {
        report
            .issues
            .iter()
            .all(|issue| matches!(issue, ValidationIssue::OverallRmseDidNotImprove { .. }))
    };
    let left_passed = non_improvement_issues_pass(&left_report)
        && improvement_passed[0]
        && left_predicted_verified_rmse_db <= MAXIMUM_PREDICTED_VERIFIED_RMSE_DB;
    let right_passed = non_improvement_issues_pass(&right_report)
        && improvement_passed[1]
        && right_predicted_verified_rmse_db <= MAXIMUM_PREDICTED_VERIFIED_RMSE_DB;
    let frequency_response = build_frequency_response_plot(design, verified_left, verified_right)?;
    Ok(LiveVerificationSummary {
        algorithm_version: LIVE_CLOSED_LOOP_VERSION.to_string(),
        passed: left_passed && right_passed && issues.is_empty(),
        left_passed,
        right_passed,
        left_raw_rmse_db: left_report.metrics.raw_rmse_db,
        left_verified_rmse_db: left_report.metrics.predicted_rmse_db,
        right_raw_rmse_db: right_report.metrics.raw_rmse_db,
        right_verified_rmse_db: right_report.metrics.predicted_rmse_db,
        left_predicted_verified_rmse_db,
        right_predicted_verified_rmse_db,
        left_unsmoothed_predicted_verified_rmse_db,
        right_unsmoothed_predicted_verified_rmse_db,
        left_gate_raw_rmse_db,
        left_gate_verified_rmse_db,
        right_gate_raw_rmse_db,
        right_gate_verified_rmse_db,
        // Record the width the gate smoother actually realizes (nominal × √2),
        // not the nominal constant — see the 2026-07-28 release-review note on
        // `log_frequency_smooth`.
        prediction_verification_smoothing_fwhm_octaves:
            eqforbeginner_dsp_core::validation::LOG_FREQUENCY_SMOOTHING_EFFECTIVE_FWHM_OCTAVES,
        maximum_allowed_predicted_verified_rmse_db: MAXIMUM_PREDICTED_VERIFIED_RMSE_DB,
        left_applied_correction_scale,
        right_applied_correction_scale,
        issues,
        frequency_response,
    })
}

fn oversampled_true_peak(samples: &[f64], factor: usize) -> Result<f64, String> {
    if samples.is_empty() || factor < 2 {
        return Err("true-peak analysis needs samples and oversampling >= 2".to_string());
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err("true-peak input contains NaN or infinity".to_string());
    }
    let base_size = samples
        .len()
        .checked_next_power_of_two()
        .ok_or_else(|| "true-peak base FFT overflowed".to_string())?;
    let oversampled_size = base_size
        .checked_mul(factor)
        .ok_or_else(|| "true-peak oversampled FFT overflowed".to_string())?;
    let mut spectrum = vec![Complex::new(0.0, 0.0); base_size];
    for (bin, sample) in spectrum.iter_mut().zip(samples) {
        bin.re = *sample;
    }
    let mut planner = FftPlanner::<f64>::new();
    planner.plan_fft_forward(base_size).process(&mut spectrum);
    let half = base_size / 2;
    let mut expanded = vec![Complex::new(0.0, 0.0); oversampled_size];
    expanded[0] = spectrum[0];
    for index in 1..half {
        expanded[index] = spectrum[index];
        expanded[oversampled_size - index] = spectrum[base_size - index];
    }
    expanded[half] = spectrum[half] * 0.5;
    expanded[oversampled_size - half] = spectrum[half] * 0.5;
    planner
        .plan_fft_inverse(oversampled_size)
        .process(&mut expanded);
    let scale = 1.0 / base_size as f64;
    let retained = samples
        .len()
        .checked_mul(factor)
        .ok_or_else(|| "true-peak retained length overflowed".to_string())?;
    let peak = expanded
        .iter()
        .take(retained)
        .map(|sample| sample.re.abs() * scale)
        .fold(0.0_f64, f64::max);
    if !peak.is_finite() {
        return Err("true-peak analysis produced a non-finite result".to_string());
    }
    Ok(peak)
}

struct LiveHeadroomMetrics {
    validation_signal_true_peak_ratio_db: f64,
    maximum_filter_response_gain_db: f64,
    fir_worst_case_peak_bound_db: f64,
    recommended_headroom_db: f64,
    absolute_safe_headroom_db: f64,
}

fn fir_worst_case_peak_bound_db(taps: &[f64]) -> Result<f64, String> {
    if taps.is_empty() || taps.iter().any(|tap| !tap.is_finite()) {
        return Err("FIR peak bound requires finite nonempty taps".to_string());
    }
    let l1_norm = taps.iter().map(|tap| tap.abs()).sum::<f64>();
    if !l1_norm.is_finite() || l1_norm <= 0.0 {
        return Err("FIR peak bound is non-finite or silent".to_string());
    }
    Ok(20.0 * l1_norm.log10())
}

fn validation_signal_headroom_db(
    session: &LiveSession,
    native: &Phase6NativeResult,
) -> Result<LiveHeadroomMetrics, String> {
    let filter = native
        .filters
        .iter()
        .find(|filter| filter.sample_rate_hz == PROJECT_SAMPLE_RATE_HZ)
        .ok_or_else(|| "native filter set has no 48 kHz filter".to_string())?;
    let mut maximum_ratio_db = f64::NEG_INFINITY;
    let mut maximum_fir_bound_db = f64::NEG_INFINITY;
    for (channel, fir) in [
        (LiveChannel::Left, &filter.left_fir.taps),
        (LiveChannel::Right, &filter.right_fir.taps),
    ] {
        let reference = &session
            .sweeps
            .get(&channel)
            .ok_or_else(|| format!("{} sweep disappeared", channel.as_str()))?
            .samples;
        let input_peak = oversampled_true_peak(reference, TRUE_PEAK_OVERSAMPLE)?;
        let filtered = fft_convolve(reference, fir)
            .map_err(|error| format!("validation-signal convolution failed: {error}"))?;
        let output_peak = oversampled_true_peak(&filtered, TRUE_PEAK_OVERSAMPLE)?;
        if input_peak <= 0.0 {
            return Err(format!("{} sweep has zero true peak", channel.as_str()));
        }
        maximum_ratio_db = maximum_ratio_db.max(20.0 * (output_peak / input_peak).log10());
        maximum_fir_bound_db = maximum_fir_bound_db.max(fir_worst_case_peak_bound_db(fir)?);
    }
    // v3 (2026-07-29 expert review, finding 12): the default recommendation
    // uses max(sweep true-peak growth, max_f |H(f)|) plus the inter-sample
    // margin. The L1 worst-case sample bound stays reported as the
    // "absolutely safe" figure, but a cut-only filter commonly carries an L1
    // of 1.5-3 (+3.5..+9.5 dB) that no real program material approaches, and
    // recommending it as the default wastes half the volume range.
    let mut maximum_response_gain_db = f64::NEG_INFINITY;
    for fir in [&filter.left_fir, &filter.right_fir] {
        let response_db = fir
            .response_db(fir.taps.len().next_power_of_two() * 4)
            .map_err(|error| format!("headroom response evaluation failed: {error}"))?;
        let channel_maximum = response_db
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        maximum_response_gain_db = maximum_response_gain_db.max(channel_maximum);
    }
    let round_up = |value: f64| (value * 10.0).ceil() / 10.0;
    let recommended = round_up(
        maximum_ratio_db.max(maximum_response_gain_db).max(0.0) + HEADROOM_SAFETY_MARGIN_DB,
    );
    let absolute_safe =
        round_up(maximum_ratio_db.max(maximum_fir_bound_db).max(0.0) + HEADROOM_SAFETY_MARGIN_DB);
    Ok(LiveHeadroomMetrics {
        validation_signal_true_peak_ratio_db: maximum_ratio_db,
        maximum_filter_response_gain_db: maximum_response_gain_db,
        fir_worst_case_peak_bound_db: maximum_fir_bound_db,
        recommended_headroom_db: recommended,
        absolute_safe_headroom_db: absolute_safe,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FinalProjectSnapshot {
    project_version: &'static str,
    session_id: String,
    system_mode: LiveSystemMode,
    system_declaration_path: String,
    subwoofer_setup: Option<LiveSubwooferSetupSummary>,
    subwoofer_search: Option<LiveSubwooferSearchSummary>,
    subwoofer_optimization: Option<LiveSubwooferOptimizationSummary>,
    verification_state: &'static str,
    correction_algorithm: &'static str,
    deconvolution_algorithm: &'static str,
    calibration_algorithm: &'static str,
    closed_loop_algorithm: &'static str,
    native_rate_algorithm: &'static str,
    native_48k_binding_algorithm: &'static str,
    headroom_algorithm: &'static str,
    target_name: String,
    target_version: String,
    custom_target: Option<TargetImportSummary>,
    calibration: CalibrationImportSummary,
    sweeps: Vec<LiveSweepImportSummary>,
    captures: Vec<LiveCaptureSummary>,
    design: LiveDesignSummary,
    verified_trial_wav_sha256: String,
    trial_activation_attestation: &'static str,
    trial_activation_declared_at_unix_ms: u64,
    verification: LiveVerificationSummary,
    recommended_headroom_db: f64,
    measured_true_peak_ratio_db: f64,
    maximum_filter_response_gain_db: f64,
    fir_worst_case_peak_bound_db: f64,
    absolute_safe_headroom_db: f64,
    final_48k_binding_maximum_magnitude_difference_db: f64,
    final_48k_binding_maximum_relative_group_delay_difference_ms: f64,
    final_zip: String,
}

fn live_zip_artifact_source(
    session: &LiveSession,
    artifact_kind: LiveZipArtifactKind,
) -> Result<(PathBuf, Option<String>), String> {
    match artifact_kind {
        LiveZipArtifactKind::Trial => {
            let design = session.design.as_ref().ok_or_else(|| {
                "create the predicted trial before downloading its ZIP".to_string()
            })?;
            if !design.summary.numerical_passed {
                return Err(
                    "the current predicted trial did not pass numerical validation".to_string(),
                );
            }
            Ok((PathBuf::from(&design.summary.trial_zip_path), None))
        }
        LiveZipArtifactKind::Final => {
            let exported = session.last_export.as_ref().ok_or_else(|| {
                "create the verified final package before downloading its ZIP".to_string()
            })?;
            if !exported.cross_rate_passed || !exported.verification.passed {
                return Err("the current final package did not pass export validation".to_string());
            }
            Ok((
                PathBuf::from(&exported.zip_path),
                Some(exported.zip_sha256.clone()),
            ))
        }
    }
}

impl LiveMeasurementState {
    pub fn zip_download_file_name(
        &self,
        artifact_kind: LiveZipArtifactKind,
    ) -> Result<String, String> {
        let guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_ref()
            .ok_or_else(|| "start a live project before downloading a ZIP".to_string())?;
        let (source, _) = live_zip_artifact_source(session, artifact_kind)?;
        source
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .ok_or_else(|| "the generated ZIP has no valid UTF-8 file name".to_string())
    }

    pub fn save_zip_artifact(
        &self,
        artifact_kind: LiveZipArtifactKind,
        destination: &Path,
    ) -> Result<LiveZipDownloadSummary, String> {
        if !destination.is_absolute() {
            return Err("the ZIP save destination must be an absolute path".to_string());
        }
        if !destination
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            return Err("the ZIP save destination must use a .zip extension".to_string());
        }

        let (session_root, source, expected_sha256) = {
            let guard = self
                .session
                .lock()
                .map_err(|_| "live session state lock was poisoned".to_string())?;
            let session = guard
                .as_ref()
                .ok_or_else(|| "start a live project before downloading a ZIP".to_string())?;
            let (source, expected_sha256) = live_zip_artifact_source(session, artifact_kind)?;
            (session.root.clone(), source, expected_sha256)
        };

        let canonical_root = session_root.canonicalize().map_err(|error| {
            format!(
                "could not resolve the live project directory {}: {error}",
                session_root.display()
            )
        })?;
        let canonical_source = source.canonicalize().map_err(|error| {
            format!(
                "could not resolve the generated ZIP {}: {error}",
                source.display()
            )
        })?;
        if !canonical_source.starts_with(&canonical_root) {
            return Err("the generated ZIP is outside the current live project".to_string());
        }

        match artifact_kind {
            LiveZipArtifactKind::Trial => {
                validate_roon_zip(&canonical_source, &BTreeSet::from([PROJECT_SAMPLE_RATE_HZ]))
                    .map_err(|error| {
                        format!("predicted trial ZIP failed readback before download: {error}")
                    })?;
            }
            LiveZipArtifactKind::Final => {
                validate_roon_six_rate_zip(&canonical_source).map_err(|error| {
                    format!("verified final ZIP failed readback before download: {error}")
                })?;
            }
        }

        let source_bytes = fs::read(&canonical_source).map_err(|error| {
            format!(
                "could not read generated ZIP {}: {error}",
                canonical_source.display()
            )
        })?;
        let source_sha256 = sha256_hex(&source_bytes);
        if expected_sha256
            .as_deref()
            .is_some_and(|expected| expected != source_sha256)
        {
            return Err(
                "the final ZIP changed after its verified project record was written".to_string(),
            );
        }

        let destination_parent = destination
            .parent()
            .ok_or_else(|| "the ZIP save destination has no parent directory".to_string())?
            .canonicalize()
            .map_err(|error| {
                format!(
                    "could not resolve the ZIP save directory for {}: {error}",
                    destination.display()
                )
            })?;
        let destination_name = destination
            .file_name()
            .ok_or_else(|| "the ZIP save destination has no file name".to_string())?;
        let canonical_destination = destination_parent.join(destination_name);

        if let Ok(metadata) = fs::symlink_metadata(&canonical_destination) {
            if metadata.file_type().is_symlink() {
                return Err("refusing to replace a symbolic-link ZIP destination".to_string());
            }
            if canonical_destination
                .canonicalize()
                .is_ok_and(|path| path == canonical_source)
            {
                let byte_count = u64::try_from(source_bytes.len())
                    .map_err(|_| "generated ZIP byte count overflowed u64".to_string())?;
                return Ok(LiveZipDownloadSummary {
                    artifact_kind,
                    file_name: destination_name.to_string_lossy().into_owned(),
                    saved_path: canonical_destination.display().to_string(),
                    byte_count,
                    sha256: source_sha256,
                });
            }
        }
        if destination_parent.starts_with(&canonical_root) {
            return Err(
                "choose a ZIP save location outside the app's internal live-project directory"
                    .to_string(),
            );
        }

        let mut temporary = NamedTempFile::new_in(&destination_parent).map_err(|error| {
            format!(
                "could not create a temporary ZIP in {}: {error}",
                destination_parent.display()
            )
        })?;
        temporary
            .write_all(&source_bytes)
            .map_err(|error| format!("could not write the downloaded ZIP: {error}"))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| format!("could not sync the downloaded ZIP: {error}"))?;
        temporary
            .persist(&canonical_destination)
            .map_err(|error| format!("could not save the downloaded ZIP: {}", error.error))?;

        let saved_bytes = fs::read(&canonical_destination).map_err(|error| {
            format!(
                "could not read back saved ZIP {}: {error}",
                canonical_destination.display()
            )
        })?;
        if sha256_hex(&saved_bytes) != source_sha256 {
            return Err("saved ZIP did not match the generated package".to_string());
        }
        let byte_count = u64::try_from(saved_bytes.len())
            .map_err(|_| "saved ZIP byte count overflowed u64".to_string())?;
        Ok(LiveZipDownloadSummary {
            artifact_kind,
            file_name: destination_name.to_string_lossy().into_owned(),
            saved_path: canonical_destination.display().to_string(),
            byte_count,
            sha256: source_sha256,
        })
    }

    pub fn finalize_export(&self) -> Result<LiveExportSummary, String> {
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err("finish the active microphone capture before final export".to_string());
        }
        let (session, artifact_index) = {
            let mut guard = self
                .session
                .lock()
                .map_err(|_| "live session state lock was poisoned".to_string())?;
            let session = guard
                .as_mut()
                .ok_or_else(|| "start a live project before export".to_string())?;
            let index = session.next_artifact_index;
            session.next_artifact_index = session
                .next_artifact_index
                .checked_add(1)
                .ok_or_else(|| "live artifact counter overflowed".to_string())?;
            (session.clone(), index)
        };
        let verification = validate_closed_loop(&session)?;
        if !verification.passed {
            return Err(format!(
                "closed-loop verification did not pass: {}",
                verification.issues.join(", ")
            ));
        }
        let design = session
            .design
            .as_ref()
            .ok_or_else(|| "trial design disappeared before export".to_string())?;
        let trial_activation_declared_at_unix_ms = design
            .user_declared_active_at_unix_ms
            .ok_or_else(|| "the exact trial filter is not declared active in Roon".to_string())?;
        let mut intent_frequencies_hz = design.result.design_frequencies_hz.clone();
        let mut intent_left_gain_db = design
            .result
            .measured_grid_stereo_design
            .left_gain_db
            .clone();
        let mut intent_right_gain_db = design
            .result
            .measured_grid_stereo_design
            .right_gain_db
            .clone();
        // A measured FFT grid normally lands one bin below the nominal taper
        // endpoint. Phase 4 defines the gain at 650 Hz as unity, so preserve
        // that physical endpoint explicitly instead of changing either
        // correction algorithm or pretending the measured grid hit it.
        if intent_frequencies_hz.last().copied().unwrap_or(0.0) < 650.0 {
            if design
                .response_set
                .frequencies_hz
                .last()
                .copied()
                .unwrap_or(0.0)
                < 650.0
            {
                return Err(
                    "measured response does not extend through the 650 Hz unity endpoint"
                        .to_string(),
                );
            }
            intent_frequencies_hz.push(650.0);
            intent_left_gain_db.push(0.0);
            intent_right_gain_db.push(0.0);
        }
        let intent = Phase6DesignIntent {
            frequencies_hz: intent_frequencies_hz,
            left_gain_db: intent_left_gain_db,
            right_gain_db: intent_right_gain_db,
            correction_low_hz: 20.0,
            taper_end_hz: 650.0,
        };
        let phase6_config = Phase6Config::default();
        let native = design_native_rate_filters(&intent, &phase6_config)
            .map_err(|error| format!("existing Phase 6 native redesign failed: {error}"))?;
        if !native.cross_rate_passed {
            return Err("native-rate filter comparison did not pass".to_string());
        }
        let native_48k = native
            .filters
            .iter()
            .find(|filter| filter.sample_rate_hz == PROJECT_SAMPLE_RATE_HZ)
            .ok_or_else(|| "native filter set has no 48 kHz evidence member".to_string())?;
        let (verified_left_fir, verified_right_fir) = (
            design.result.left_fir.clone(),
            design.result.right_fir.clone(),
        );
        let source_binding = compare_stereo_filter_responses(
            &verified_left_fir,
            &verified_right_fir,
            &native_48k.left_fir,
            &native_48k.right_fir,
            &phase6_config,
        )
        .map_err(|error| format!("verified-trial/native-48k binding failed: {error}"))?;
        if !source_binding.passed {
            return Err(format!(
                "final 48 kHz filter no longer matches the verified trial response: {:?}",
                source_binding
            ));
        }
        let final_48k_binding_maximum_magnitude_difference_db = source_binding
            .left
            .maximum_magnitude_difference_db
            .max(source_binding.right.maximum_magnitude_difference_db);
        let final_48k_binding_maximum_relative_group_delay_difference_ms = source_binding
            .left
            .maximum_relative_group_delay_difference_ms
            .max(
                source_binding
                    .right
                    .maximum_relative_group_delay_difference_ms,
            );
        let headroom = validation_signal_headroom_db(&session, &native)?;
        let measured_true_peak_ratio_db = headroom.validation_signal_true_peak_ratio_db;
        let maximum_filter_response_gain_db = headroom.maximum_filter_response_gain_db;
        let fir_worst_case_peak_bound_db = headroom.fir_worst_case_peak_bound_db;
        let recommended_headroom_db = headroom.recommended_headroom_db;
        let absolute_safe_headroom_db = headroom.absolute_safe_headroom_db;
        let package_directory = session
            .root
            .join("final")
            .join(format!("package-{artifact_index:06}"));
        fs::create_dir(&package_directory).map_err(|error| {
            format!(
                "could not create final package directory {}: {error}",
                package_directory.display()
            )
        })?;
        let mut wav_paths = Vec::with_capacity(native.filters.len());
        for filter in &native.filters {
            let path = package_directory.join(format!(
                "EQforBeginner_{}_stereo.wav",
                filter.sample_rate_hz
            ));
            let fir = StereoFir {
                sample_rate: filter.sample_rate_hz,
                left: filter.left_fir.taps.iter().map(|tap| *tap as f32).collect(),
                right: filter
                    .right_fir
                    .taps
                    .iter()
                    .map(|tap| *tap as f32)
                    .collect(),
            };
            write_stereo_wav(&path, &fir)
                .map_err(|error| format!("could not write native filter: {error}"))?;
            wav_paths.push(path);
        }
        let zip_path = session.root.join("final").join(format!(
            "EQforBeginner_{}_verified_{artifact_index:06}.zip",
            session.id
        ));
        let algorithm_version = format!(
            "{PHASE4_OFFLINE_ALGORITHM_VERSION}+{LIVE_CLOSED_LOOP_VERSION}+\
             {PHASE6_ALGORITHM_VERSION}+{LIVE_NATIVE_BINDING_VERSION}"
        );
        let correction_line = "Correction: minimum phase, 20-500 Hz, unity taper through 650 Hz;\n\
             broad spatially repeated shallow dips may receive at most +3 dB,\n\
             while deep/narrow dips remain protected";
        let timing_line =
            "Timing limitation: the single asynchronous sweep did not authorize an L/R delay\n\
             correction; this package is minimum-phase magnitude correction.";
        let readme = format!(
            "EQforBeginner verified developer-beta convolution\n\
             Project: {}\n\
             Target: {} ({})\n\
             {correction_line}\n\
             Baseline positions: {}\n\
             Closed-loop P0 L/R verification: passed\n\
             Native rates: 44.1, 48, 88.2, 96, 176.4, 192 kHz\n\
             Verified-trial to final-48k response binding: {:.5} dB magnitude,\n\
             {:.5} ms relative group delay (passed)\n\
             Recommended Roon headroom: {:.1} dB\n\
             Headroom basis (v3): the larger of the 4x-oversampled registered-sweep\n\
             ratio ({:.3} dB) and the filter's maximum frequency-response gain\n\
             ({:.3} dB), plus {:.1} dB inter-sample safety margin. The L1 worst-case\n\
             sample-peak bound ({:.3} dB) gives the mathematically absolute-safe\n\
             setting of {:.1} dB; real program material does not approach it.\n\
             Disable every previous convolution, load only this ZIP, set the recommended\n\
             headroom, and watch Roon's clipping indicator.\n\
             {timing_line}\n",
            session.id,
            design.target_name,
            design.target_version,
            design.response_set.positions.len(),
            final_48k_binding_maximum_magnitude_difference_db,
            final_48k_binding_maximum_relative_group_delay_difference_ms,
            recommended_headroom_db,
            measured_true_peak_ratio_db,
            maximum_filter_response_gain_db,
            HEADROOM_SAFETY_MARGIN_DB,
            fir_worst_case_peak_bound_db,
            absolute_safe_headroom_db,
        );
        create_roon_six_rate_zip(&zip_path, &wav_paths, &algorithm_version, &readme)
            .map_err(|error| format!("could not create final six-rate Roon ZIP: {error}"))?;
        validate_roon_six_rate_zip(&zip_path)
            .map_err(|error| format!("final Roon ZIP readback failed: {error}"))?;
        let zip_bytes = fs::read(&zip_path)
            .map_err(|error| format!("could not hash final Roon ZIP: {error}"))?;
        let zip_sha256 = sha256_hex(&zip_bytes);
        let calibration = session
            .calibration
            .as_ref()
            .ok_or_else(|| "calibration disappeared before export".to_string())?
            .summary
            .clone();
        let project = FinalProjectSnapshot {
            project_version: LIVE_MEASUREMENT_PROJECT_VERSION,
            session_id: session.id.clone(),
            system_mode: session.system_mode,
            system_declaration_path: session.system_declaration_path.display().to_string(),
            subwoofer_setup: session.subwoofer_setup.clone(),
            subwoofer_search: session.subwoofer_search.clone(),
            subwoofer_optimization: session.subwoofer_optimization.clone(),
            verification_state: "hardware-remeasured-minimum-phase",
            correction_algorithm: PHASE4_OFFLINE_ALGORITHM_VERSION,
            deconvolution_algorithm: KNOWN_SWEEP_DECONVOLUTION_VERSION,
            calibration_algorithm: UMIK_CALIBRATION_PARSER_VERSION,
            closed_loop_algorithm: LIVE_CLOSED_LOOP_VERSION,
            native_rate_algorithm: PHASE6_ALGORITHM_VERSION,
            native_48k_binding_algorithm: LIVE_NATIVE_BINDING_VERSION,
            headroom_algorithm: LIVE_HEADROOM_VERSION,
            target_name: design.target_name.clone(),
            target_version: design.target_version.clone(),
            custom_target: design.custom_target.clone(),
            calibration,
            sweeps: session
                .sweeps
                .values()
                .map(|sweep| sweep.summary.clone())
                .collect(),
            captures: session
                .measurements
                .values()
                .map(|measurement| measurement.summary.clone())
                .collect(),
            design: design.summary.clone(),
            verified_trial_wav_sha256: design.evidence_sha256.clone(),
            trial_activation_attestation: "user-declared-manual-roon",
            trial_activation_declared_at_unix_ms,
            verification: verification.clone(),
            recommended_headroom_db,
            measured_true_peak_ratio_db,
            maximum_filter_response_gain_db,
            fir_worst_case_peak_bound_db,
            absolute_safe_headroom_db,
            final_48k_binding_maximum_magnitude_difference_db,
            final_48k_binding_maximum_relative_group_delay_difference_ms,
            final_zip: zip_path.display().to_string(),
        };
        let project_path = package_directory.join("project.json");
        write_new_file(
            &project_path,
            &serde_json::to_vec_pretty(&project)
                .map_err(|error| format!("could not serialize final project: {error}"))?,
        )?;
        let summary = LiveExportSummary {
            zip_path: zip_path.display().to_string(),
            project_path: project_path.display().to_string(),
            zip_sha256,
            algorithm_version,
            recommended_headroom_db,
            measured_true_peak_ratio_db,
            maximum_filter_response_gain_db,
            fir_worst_case_peak_bound_db,
            absolute_safe_headroom_db,
            final_48k_binding_maximum_magnitude_difference_db,
            final_48k_binding_maximum_relative_group_delay_difference_ms,
            native_rate_count: native.filters.len(),
            cross_rate_passed: native.cross_rate_passed,
            verification,
        };
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let current = guard
            .as_mut()
            .ok_or_else(|| "live project was closed during final export".to_string())?;
        if current.id != session.id
            || current.evidence_generation != session.evidence_generation
            || current
                .design
                .as_ref()
                .map(|current_design| current_design.evidence_sha256.as_str())
                != Some(design.evidence_sha256.as_str())
        {
            return Err(
                "live measurement evidence changed during final export; discard the stale package and export again"
                    .to_string(),
            );
        }
        current.last_export = Some(summary.clone());
        Ok(summary)
    }

    pub fn verification_summary(&self) -> Result<LiveVerificationSummary, String> {
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err(
                "finish the active microphone capture before closed-loop validation".to_string(),
            );
        }
        let guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        validate_closed_loop(
            guard
                .as_ref()
                .ok_or_else(|| "start a live project before verification".to_string())?,
        )
    }
}

fn active_regions(samples: &[f32], sample_rate_hz: u32) -> Result<Vec<(usize, usize)>, String> {
    let peak = samples
        .iter()
        .map(|sample| sample.abs())
        .fold(0.0_f32, f32::max);
    if peak < 0.001 {
        return Err("measurement sweep channel is silent or too quiet".to_string());
    }
    let threshold = (peak * 0.001).max(0.000_01);
    let maximum_gap = (f64::from(sample_rate_hz) * ACTIVE_GAP_SECONDS).round() as usize;
    let mut runs = Vec::new();
    let mut run_start = None;
    let mut last_active = None;
    for (index, sample) in samples.iter().enumerate() {
        if sample.abs() < threshold {
            continue;
        }
        if let Some(previous) = last_active {
            if index.saturating_sub(previous) > maximum_gap {
                if let Some(start) = run_start.take() {
                    runs.push((start, previous + 1));
                }
            }
        }
        run_start.get_or_insert(index);
        last_active = Some(index);
    }
    if let (Some(start), Some(end)) = (run_start, last_active) {
        runs.push((start, end + 1));
    }
    Ok(runs)
}

fn longest_active_region(samples: &[f32], sample_rate_hz: u32) -> Result<(usize, usize), String> {
    let runs = active_regions(samples, sample_rate_hz)?;
    let guard = (f64::from(sample_rate_hz) * ACTIVE_GUARD_SECONDS).round() as usize;
    let (start, end) = runs
        .into_iter()
        .max_by_key(|(start, end)| end.saturating_sub(*start))
        .ok_or_else(|| "could not find an active measurement sweep region".to_string())?;
    let start = start.saturating_sub(guard);
    let end = end.saturating_add(guard).min(samples.len());
    if end.saturating_sub(start) < sample_rate_hz as usize {
        return Err("the longest active sweep region is shorter than one second".to_string());
    }
    Ok((start, end))
}

fn decode_live_wav_channels(bytes: &[u8]) -> Result<(u32, Vec<Vec<f32>>), String> {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("invalid measurement WAV: {error}"))?;
    let spec = reader.spec();
    if spec.sample_rate != PROJECT_SAMPLE_RATE_HZ || !(1..=2).contains(&spec.channels) {
        return Err(format!(
            "live measurement WAV must be 48 kHz mono/stereo; found {} Hz, {} channels",
            spec.sample_rate, spec.channels
        ));
    }
    let interleaved = match spec.sample_format {
        hound::SampleFormat::Float if spec.bits_per_sample == 32 => reader
            .samples::<f32>()
            .map(|sample| {
                sample.map_err(|error| format!("could not decode measurement WAV: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int if matches!(spec.bits_per_sample, 8 | 16 | 24 | 32) => {
            let scale = 2_f64.powi(i32::from(spec.bits_per_sample) - 1);
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| (f64::from(value) / scale) as f32)
                        .map_err(|error| format!("could not decode measurement WAV: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => {
            return Err(format!(
                "unsupported measurement WAV encoding: {:?} {}-bit",
                spec.sample_format, spec.bits_per_sample
            ));
        }
    };
    if interleaved.len() % usize::from(spec.channels) != 0 {
        return Err("measurement WAV ends in a partial frame".to_string());
    }
    let frames = interleaved.len() / usize::from(spec.channels);
    let mut channels = (0..spec.channels)
        .map(|_| Vec::with_capacity(frames))
        .collect::<Vec<_>>();
    for frame in interleaved.chunks_exact(usize::from(spec.channels)) {
        for (channel, sample) in channels.iter_mut().zip(frame) {
            if !sample.is_finite() || sample.abs() > 1.000_1 {
                return Err("measurement WAV contains a non-finite/out-of-range sample".to_string());
            }
            channel.push(*sample);
        }
    }
    Ok((spec.sample_rate, channels))
}

fn timing_markers_from_channels(
    channels: &[Vec<f32>],
    main_region: (usize, usize),
    sample_rate_hz: u32,
) -> Vec<TimingMarker> {
    let minimum_marker_samples = sample_rate_hz as usize / 10;
    let marker_guard = sample_rate_hz as usize / 100;
    let Some(frame_count) = channels.first().map(Vec::len) else {
        return Vec::new();
    };
    if channels.iter().any(|channel| channel.len() != frame_count) {
        return Vec::new();
    }
    let aggregate = (0..frame_count)
        .map(|frame| {
            channels
                .iter()
                .map(|channel| channel[frame].abs())
                .fold(0.0_f32, f32::max)
        })
        .collect::<Vec<_>>();
    let Ok(regions) = active_regions(&aggregate, sample_rate_hz) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for (start, end) in regions {
        if (start < main_region.1 && end > main_region.0)
            || end.saturating_sub(start) < minimum_marker_samples
        {
            continue;
        }
        let guarded_start = start.saturating_sub(marker_guard);
        let guarded_end = end.saturating_add(marker_guard).min(frame_count);
        let (source_channel, channel_separation_db, channel_index) =
            classify_marker_channel(channels, guarded_start, guarded_end);
        candidates.push(TimingMarker {
            source_start_sample: guarded_start,
            samples: channels[channel_index][guarded_start..guarded_end]
                .iter()
                .map(|sample| f64::from(*sample))
                .collect(),
            source_channel,
            channel_separation_db,
            is_start_marker: false,
        });
    }
    candidates.sort_by_key(|marker| marker.source_start_sample);
    let before = candidates
        .iter()
        .rev()
        .find(|marker| marker.source_start_sample + marker.samples.len() <= main_region.0)
        .cloned();
    let after = candidates
        .iter()
        .find(|marker| marker.source_start_sample >= main_region.1)
        .cloned();
    let mut selected = Vec::with_capacity(2);
    if let Some(mut marker) = before {
        marker.is_start_marker = true;
        selected.push(marker);
    }
    if let Some(marker) = after {
        selected.push(marker);
    }
    selected
}

fn classify_marker_channel(
    channels: &[Vec<f32>],
    start: usize,
    end: usize,
) -> (ReferenceChannel, Option<f64>, usize) {
    if channels.len() == 1 {
        return (ReferenceChannel::Mono, None, 0);
    }
    let left = &channels[0][start..end];
    let right = &channels[1][start..end];
    let mut left_energy = 0.0_f64;
    let mut right_energy = 0.0_f64;
    let mut cross = 0.0_f64;
    for (&left, &right) in left.iter().zip(right) {
        let left = f64::from(left);
        let right = f64::from(right);
        left_energy += left * left;
        right_energy += right * right;
        cross += left * right;
    }
    let separation_db = 10.0
        * (left_energy.max(right_energy).max(1.0e-30) / left_energy.min(right_energy).max(1.0e-30))
            .log10();
    let correlation = cross / (left_energy * right_energy).max(1.0e-60).sqrt();
    let level_difference_db = 10.0 * (left_energy.max(1.0e-30) / right_energy.max(1.0e-30)).log10();
    if correlation >= 0.999 && level_difference_db.abs() <= 1.0 {
        (ReferenceChannel::IdenticalStereo, Some(separation_db), 0)
    } else if left_energy >= right_energy {
        (ReferenceChannel::Left, Some(separation_db), 0)
    } else {
        (ReferenceChannel::Right, Some(separation_db), 1)
    }
}

fn reference_channel_matches(channel: LiveChannel, reference: ReferenceChannel) -> bool {
    matches!(
        (channel, reference),
        (LiveChannel::Left, ReferenceChannel::Left)
            | (LiveChannel::Right, ReferenceChannel::Right)
            | (
                _,
                ReferenceChannel::Mono | ReferenceChannel::IdenticalStereo
            )
    )
}

fn amplitude_dbfs(amplitude: f64) -> f64 {
    20.0 * amplitude.max(1.0e-15).log10()
}

impl LiveMeasurementState {
    pub fn import_sweep(
        &self,
        channel: LiveChannel,
        bytes: &[u8],
    ) -> Result<LiveSweepImportSummary, String> {
        if bytes.len() > MAX_LIVE_SWEEP_BYTES {
            return Err(format!(
                "measurement sweep exceeds the {} MiB safety limit",
                MAX_LIVE_SWEEP_BYTES / (1024 * 1024)
            ));
        }
        let decoded = decode_reference_wav(bytes)?;
        if !reference_channel_matches(channel, decoded.metadata.reference_channel) {
            return Err(format!(
                "the selected {} file's dominant measurement channel is {:?}",
                channel.as_str(),
                decoded.metadata.reference_channel
            ));
        }
        let (sample_rate_hz, channels) = decode_live_wav_channels(bytes)?;
        let measurement_channel = match (channel, channels.len()) {
            (_, 1) | (LiveChannel::Left, _) => 0,
            (LiveChannel::Right, _) => 1,
        };
        let selected_samples = channels
            .get(measurement_channel)
            .ok_or_else(|| format!("{} sweep channel is absent", channel.as_str()))?;
        let (start, end) = longest_active_region(selected_samples, sample_rate_hz)?;
        let timing_markers = timing_markers_from_channels(&channels, (start, end), sample_rate_hz);
        // Capture auto-completes at the end marker's end, so the source WAV
        // itself must leave at least the deconvolution IR length between the
        // main sweep's end and the end marker's end. A file that cannot
        // satisfy this fails *every* capture only after the user has played
        // the whole sweep; reject it once at import instead (2026-07-28
        // release review). Markerless files keep the fallback-recognizer
        // path, whose capture does not stop at a marker.
        if let Some(end_marker) = timing_markers.iter().find(|marker| !marker.is_start_marker) {
            validate_sweep_ir_tail_capacity(
                channel,
                end,
                end_marker.source_start_sample,
                end_marker.samples.len(),
                SweepDeconvolutionConfig::default().impulse_length_samples,
            )?;
        }
        let samples = selected_samples[start..end]
            .iter()
            .map(|sample| f64::from(*sample))
            .collect::<Vec<_>>();
        let peak = samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f64, f64::max);
        let sample_rate = f64::from(decoded.metadata.sample_rate_hz);
        let summary = LiveSweepImportSummary {
            channel,
            sha256: sha256_hex(bytes),
            source_channels: decoded.metadata.channels,
            sample_rate_hz: decoded.metadata.sample_rate_hz,
            source_duration_seconds: decoded.metadata.duration_seconds,
            measurement_start_seconds: start as f64 / sample_rate,
            measurement_end_seconds: end as f64 / sample_rate,
            measurement_duration_seconds: samples.len() as f64 / sample_rate,
            measurement_peak_dbfs: amplitude_dbfs(peak),
            source_reference_channel: decoded.metadata.reference_channel,
            timing_marker_count: timing_markers.len(),
            marker_channel_analysis_version: SWEEP_MARKER_CHANNEL_ANALYSIS_VERSION,
            start_marker_channel: timing_markers
                .iter()
                .find(|marker| marker.is_start_marker)
                .map(|marker| marker.source_channel),
            end_marker_channel: timing_markers
                .iter()
                .find(|marker| !marker.is_start_marker)
                .map(|marker| marker.source_channel),
            start_marker_channel_separation_db: timing_markers
                .iter()
                .find(|marker| marker.is_start_marker)
                .and_then(|marker| marker.channel_separation_db),
            end_marker_channel_separation_db: timing_markers
                .iter()
                .find(|marker| !marker.is_start_marker)
                .and_then(|marker| marker.channel_separation_db),
        };
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err("stop the active microphone capture before changing sweeps".to_string());
        }
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "start a live project before importing sweeps".to_string())?;
        if let Some(existing) = session.sweeps.get(&channel) {
            if existing.summary.sha256 == summary.sha256 {
                return Ok(existing.summary.clone());
            }
        }
        let artifact_index = session.next_artifact_index;
        session.next_artifact_index = session
            .next_artifact_index
            .checked_add(1)
            .ok_or_else(|| "live artifact counter overflowed".to_string())?;
        let path = session.root.join("inputs").join(format!(
            "{artifact_index:06}-{}-sweep.wav",
            channel.as_str()
        ));
        write_new_file(&path, bytes)?;
        session.sweeps.insert(
            channel,
            LiveSweepReference {
                summary: summary.clone(),
                samples,
                source_frame_count: decoded.metadata.frame_count,
                measurement_source_start_sample: start,
                timing_markers,
            },
        );
        session.measurements.clear();
        if session.subwoofer_search.is_some() {
            session.subwoofer_search = None;
            session.subwoofer_optimization = None;
            session.subwoofer_setup = None;
        }
        session.design = None;
        advance_evidence_generation(session)?;
        Ok(summary)
    }

    pub fn begin_capture(
        &self,
        kind: LiveCaptureKind,
        channel: LiveChannel,
        position_id: &str,
        input_device_id: &str,
        input_channel_index: u16,
    ) -> Result<
        (
            String,
            PathBuf,
            LiveSweepReference,
            MicrophoneCalibration,
            LiveCaptureEvidence,
            InputCaptureCancellation,
        ),
        String,
    > {
        let position_id = validate_position_id(position_id, kind)?;
        let mut active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err("a live microphone capture is already running".to_string());
        }
        if input_device_id.trim().is_empty() {
            return Err("select a microphone input device before capture".to_string());
        }
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "start a live project before capturing".to_string())?;
        let separated_path_capture = matches!(
            kind,
            LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly
        );
        if separated_path_capture {
            if session.system_mode != LiveSystemMode::SingleSub21 {
                return Err(
                    "separated main/sub captures require a single-sub 2.1 project".to_string(),
                );
            }
            let search = session.subwoofer_search.as_ref().ok_or_else(|| {
                "save a separated-path crossover search plan before capturing".to_string()
            })?;
            if !search
                .candidates
                .iter()
                .any(|candidate| candidate.id == position_id)
            {
                return Err(format!(
                    "crossover candidate `{position_id}` is not in the current search plan"
                ));
            }
            if kind == LiveCaptureKind::SubOnly && channel != search.sub_sweep_channel {
                return Err(format!(
                    "sub-only capture must use the {} sweep so the fixed marker speaker remains available",
                    search.sub_sweep_channel.as_str()
                ));
            }
        } else if session.system_mode == LiveSystemMode::SingleSub21
            && session.subwoofer_setup.is_none()
        {
            return Err(
                "record and confirm the real 2.1 crossover, delay, polarity, and sub level before capturing"
                    .to_string(),
            );
        }
        match (
            session.locked_input_device_id.as_deref(),
            session.locked_input_channel_index,
        ) {
            (Some(locked), _) if locked != input_device_id => {
                return Err(format!(
                    "this project is locked to microphone `{locked}`; start a new project to use a different input"
                ));
            }
            (Some(_), Some(locked_channel)) if locked_channel != input_channel_index => {
                return Err(format!(
                    "this project is locked to microphone input channel {}; start a new project to use channel {}",
                    u32::from(locked_channel) + 1,
                    u32::from(input_channel_index) + 1,
                ));
            }
            (None, None) => {
                session.locked_input_device_id = Some(input_device_id.to_string());
                session.locked_input_channel_index = Some(input_channel_index);
                advance_evidence_generation(session)?;
            }
            (Some(_), Some(_)) => {}
            _ => {
                return Err(
                    "live project microphone device/channel lock is inconsistent".to_string(),
                );
            }
        }
        if kind == LiveCaptureKind::Verification {
            let design = session.design.as_ref().ok_or_else(|| {
                "create the 48 kHz trial filter before verification capture".to_string()
            })?;
            if design.user_declared_active_at_unix_ms.is_none() {
                return Err(
                    "declare the exact trial filter active in Roon before verification capture"
                        .to_string(),
                );
            }
            if position_id != "P0" {
                return Err("verification is captured at the central position P0".to_string());
            }
        }
        let calibration_entry = session
            .calibration
            .as_ref()
            .ok_or_else(|| "import a covering UMIK calibration before capturing".to_string())?;
        let calibration = calibration_entry.profile.clone();
        let sweep =
            session.sweeps.get(&channel).cloned().ok_or_else(|| {
                format!("import the {} measurement sweep first", channel.as_str())
            })?;
        let evidence = LiveCaptureEvidence {
            session_id: session.id.clone(),
            generation: session.evidence_generation,
            system_mode: session.system_mode,
            subwoofer_setup: session.subwoofer_setup.clone(),
            subwoofer_search: session.subwoofer_search.clone(),
            calibration_sha256: calibration_entry.summary.sha256.clone(),
            sweep_sha256: sweep.summary.sha256.clone(),
            design_sha256: match kind {
                LiveCaptureKind::SubMainOnly
                | LiveCaptureKind::SubOnly
                | LiveCaptureKind::Baseline => None,
                LiveCaptureKind::Verification => Some(
                    session
                        .design
                        .as_ref()
                        .expect("verification design checked above")
                        .evidence_sha256
                        .clone(),
                ),
            },
            input_device_id: input_device_id.to_string(),
            input_channel_index,
        };
        let cancellation = InputCaptureCancellation::new();
        *active = Some(cancellation.clone());
        Ok((
            position_id,
            session.root.clone(),
            sweep,
            calibration,
            evidence,
            cancellation,
        ))
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
            .map_err(|_| "live capture state lock was poisoned".to_string())?
            .clone();
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn set_trial_activation(&self, active_in_roon: bool) -> Result<bool, String> {
        let active_capture = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active_capture.is_some() {
            return Err(
                "finish the active microphone capture before changing the trial declaration"
                    .to_string(),
            );
        }
        let declared_at = if active_in_roon {
            Some(
                u64::try_from(unix_milliseconds()?)
                    .map_err(|_| "trial declaration timestamp overflowed".to_string())?,
            )
        } else {
            None
        };
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "start a live project before declaring the trial".to_string())?;
        let design = session.design.as_mut().ok_or_else(|| {
            "create the 48 kHz trial filter before declaring it active".to_string()
        })?;
        design.user_declared_active_at_unix_ms = declared_at;
        session
            .measurements
            .retain(|(kind, _, _), _| *kind != LiveCaptureKind::Verification);
        advance_evidence_generation(session)?;
        Ok(active_in_roon)
    }
}

fn capture_duration_ms(source_frames: usize, wait_seconds: u64) -> Result<u64, String> {
    let source_ms = u128::try_from(source_frames)
        .ok()
        .and_then(|samples| samples.checked_mul(1_000))
        .and_then(|duration| duration.checked_add(u128::from(PROJECT_SAMPLE_RATE_HZ) - 1))
        .map(|duration| duration / u128::from(PROJECT_SAMPLE_RATE_HZ))
        .ok_or_else(|| "live sweep duration overflowed".to_string())?;
    u128::from(wait_seconds)
        .checked_mul(1_000)
        .and_then(|duration| duration.checked_add(source_ms))
        .and_then(|duration| duration.checked_add(u128::from(CAPTURE_DEADLINE_GRACE_MILLISECONDS)))
        .and_then(|duration| u64::try_from(duration).ok())
        .ok_or_else(|| "live capture duration overflowed".to_string())
}

pub fn live_capture_request(
    device_id: String,
    input_channel_index: u16,
    source_frames: usize,
    wait_seconds: u64,
) -> Result<MonoInputCaptureRequest, String> {
    if !(5..=60).contains(&wait_seconds) {
        return Err("live capture waitSeconds must be between 5 and 60".to_string());
    }
    let duration_ms = capture_duration_ms(source_frames, wait_seconds)?;
    let maximum_samples = u128::from(duration_ms)
        .checked_mul(u128::from(PROJECT_SAMPLE_RATE_HZ))
        .and_then(|samples| samples.checked_add(999))
        .and_then(|samples| usize::try_from(samples / 1_000).ok())
        .ok_or_else(|| "live capture sample count overflowed".to_string())?;
    Ok(MonoInputCaptureRequest {
        device_id,
        input_channel_index,
        duration_ms,
        maximum_samples,
    })
}

fn write_raw_capture(path: &Path, capture: &MonoInputCapture) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: capture.sample_rate_hz,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|error| format!("could not create raw capture WAV: {error}"))?;
    for sample in &capture.samples {
        writer
            .write_sample(*sample)
            .map_err(|error| format!("could not write raw capture WAV: {error}"))?;
    }
    writer
        .finalize()
        .map_err(|error| format!("could not finalize raw capture WAV: {error}"))
}

fn quality_issue_codes(measurement: &SweepMeasurement, capture: &MonoInputCapture) -> Vec<String> {
    let mut issues = measurement
        .quality
        .issues
        .iter()
        .map(|issue| format!("{issue:?}"))
        .collect::<Vec<_>>();
    if capture.xrun_count > 0 {
        issues.push("audio_stream_xrun".to_string());
    }
    if capture.callback_lock_drop_frames > 0 {
        issues.push("audio_monitor_contention_drop".to_string());
    }
    if capture.sample_drop_detected
        && capture.xrun_count == 0
        && capture.callback_lock_drop_frames == 0
        && capture.missing_samples_at_end == 0
    {
        issues.push("audio_stream_sample_drop".to_string());
    }
    if capture.stream_error_detected || capture.stream_error_count > 0 {
        issues.push("audio_stream_runtime_error".to_string());
    }
    if capture.timed_out || !capture.capture_complete {
        issues.push("audio_capture_incomplete".to_string());
    }
    if capture.callback_format_error {
        issues.push("audio_callback_format_error".to_string());
    }
    if capture.input_clipped && !issues.iter().any(|issue| issue.contains("InputClipping")) {
        issues.push("audio_input_clipping".to_string());
    }
    issues
}

fn capture_diagnostic_codes(capture: &MonoInputCapture) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if capture.timestamp_gap_frames > 0 {
        diagnostics.push("audio_capture_timestamp_gap".to_string());
    }
    if capture.timestamp_discontinuity_count > 0 {
        diagnostics.push("audio_capture_timestamp_regression".to_string());
    }
    diagnostics
}

fn audio_stream_diagnostics(capture: &MonoInputCapture) -> LiveAudioStreamDiagnostics {
    LiveAudioStreamDiagnostics {
        xrun_count: capture.xrun_count,
        callback_lock_drop_frames: capture.callback_lock_drop_frames,
        timestamp_gap_frames: capture.timestamp_gap_frames,
        timestamp_discontinuity_count: capture.timestamp_discontinuity_count,
        missing_samples_at_end: capture.missing_samples_at_end,
        stream_error_count: capture.stream_error_count,
    }
}

fn recognition_marker_candidates(recognition: WirelessSweepRecognition) -> Vec<TimingMarkerMatch> {
    match recognition {
        WirelessSweepRecognition::Detected(detection) => vec![TimingMarkerMatch {
            capture_start_sample: detection.estimated_sweep_start_sample,
            absolute_correlation: detection.start_absolute_correlation,
        }],
        WirelessSweepRecognition::Ambiguous(outcome) => vec![
            TimingMarkerMatch {
                capture_start_sample: outcome.strongest_candidate_start_sample,
                absolute_correlation: outcome.strongest_absolute_correlation,
            },
            TimingMarkerMatch {
                capture_start_sample: outcome.second_candidate_start_sample,
                absolute_correlation: outcome.second_absolute_correlation,
            },
        ],
        // A room can bias the frequency-dependent timing fit inside one
        // marker even though every independent marker segment matches. Admit
        // only that complete-segment case as a tentative marker: automatic
        // completion still requires the second marker at the known source
        // spacing, whose pair fit supplies the trustworthy clock estimate.
        WirelessSweepRecognition::LikelyFalsePositive(outcome)
            if (outcome.start_absolute_correlation >= 0.60
                && outcome.matched_segment_count >= outcome.required_segment_count)
                || (outcome.start_absolute_correlation
                    >= MINIMUM_ROOM_BIASED_MARKER_CORRELATION
                    && outcome.matched_segment_count >= REQUIRED_ROOM_BIASED_MARKER_SEGMENTS
                    && matches!(
                        outcome.reason,
                        WirelessSweepRejectionReason::ClockDriftOutOfRange
                            | WirelessSweepRejectionReason::TimingFitUnstable
                    )) =>
        {
            vec![TimingMarkerMatch {
                capture_start_sample: outcome.candidate_start_sample,
                absolute_correlation: outcome.start_absolute_correlation,
            }]
        }
        WirelessSweepRecognition::NotDetected(_)
        | WirelessSweepRecognition::LikelyFalsePositive(_) => Vec::new(),
    }
}

fn timing_marker_recognition_config(
    marker_length: usize,
    capture_length: usize,
) -> WirelessSweepRecognitionConfig {
    let probe_samples = 4_096.min(marker_length).max(64);
    let segment_samples = 1_024.min(marker_length / 3).max(64);
    WirelessSweepRecognitionConfig {
        nominal_sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        probe_samples,
        segment_samples,
        segment_count: 5,
        minimum_start_correlation: MINIMUM_MARKER_CORRELATION,
        minimum_segment_correlation: MINIMUM_MARKER_SEGMENT_CORRELATION,
        ambiguity_peak_ratio: MINIMUM_REPEATED_MARKER_CANDIDATE_RATIO,
        maximum_clock_drift_ppm: MAXIMUM_SINGLE_MARKER_INTERNAL_SLOPE_PPM,
        maximum_timing_fit_rms_samples: MAGNITUDE_ONLY_MAXIMUM_TIMING_FIT_RMS_SAMPLES,
        minimum_segment_matches: 3,
        capture_pre_roll_samples: 0,
        capture_post_roll_samples: 0,
        maximum_reference_samples: marker_length.max(1),
        maximum_capture_samples: capture_length.max(1),
        ..WirelessSweepRecognitionConfig::default()
    }
}

fn recognize_timing_marker(
    marker: &TimingMarker,
    capture_samples: &[f64],
) -> Result<Vec<TimingMarkerMatch>, String> {
    if capture_samples.len() < marker.samples.len() {
        return Ok(Vec::new());
    }
    let config = timing_marker_recognition_config(marker.samples.len(), capture_samples.len());
    let recognition = recognize_wireless_sweep(&marker.samples, capture_samples, &config)
        .map_err(|error| format!("timing-marker recognition failed: {error}"))?;
    Ok(recognition_marker_candidates(recognition))
}

fn detect_timing_marker_pair(
    sweep: &LiveSweepReference,
    capture_samples: &[f64],
) -> Result<Option<TimingMarkerPairDetection>, String> {
    let Some(first_marker) = sweep.timing_markers.first() else {
        return Ok(None);
    };
    let Some(last_marker) = sweep.timing_markers.last() else {
        return Ok(None);
    };
    if first_marker.source_start_sample >= last_marker.source_start_sample {
        return Ok(None);
    }
    let first_candidates = recognize_timing_marker(first_marker, capture_samples)?;
    let last_candidates = recognize_timing_marker(last_marker, capture_samples)?;
    let source_gap = (last_marker.source_start_sample - first_marker.source_start_sample) as f64;
    let mut best: Option<(f64, TimingMarkerPairDetection)> = None;
    for first in first_candidates {
        for &last in &last_candidates {
            let capture_gap = last.capture_start_sample - first.capture_start_sample;
            if capture_gap <= 0.0 {
                continue;
            }
            let ratio = capture_gap / source_gap;
            let drift_ppm = (ratio - 1.0) * 1_000_000.0;
            if !ratio.is_finite() || drift_ppm.abs() > MAXIMUM_MARKER_PAIR_CLOCK_DRIFT_PPM {
                continue;
            }
            let playback_offset =
                first.capture_start_sample - ratio * first_marker.source_start_sample as f64;
            let estimated_sweep_start =
                playback_offset + ratio * sweep.measurement_source_start_sample as f64;
            let estimated_sweep_end = estimated_sweep_start + ratio * sweep.samples.len() as f64;
            if playback_offset.is_finite()
                && estimated_sweep_start >= 0.0
                && estimated_sweep_end > estimated_sweep_start
                && estimated_sweep_end <= capture_samples.len() as f64
            {
                let confidence = first.absolute_correlation.min(last.absolute_correlation);
                let score = confidence - drift_ppm.abs() / 100_000.0;
                let detection = TimingMarkerPairDetection {
                    first,
                    last,
                    capture_samples_per_reference_sample: ratio,
                    estimated_sweep_start_sample: estimated_sweep_start,
                    estimated_sweep_end_sample_exclusive: estimated_sweep_end,
                };
                if best
                    .as_ref()
                    .is_none_or(|(best_score, _)| score > *best_score)
                {
                    best = Some((score, detection));
                }
            }
        }
    }
    Ok(best.map(|(_, detection)| detection))
}

fn rms_dbfs(energy: f64, sample_count: usize) -> Option<f64> {
    if sample_count == 0 || !energy.is_finite() || energy < 0.0 {
        return None;
    }
    Some(amplitude_dbfs((energy / sample_count as f64).sqrt()))
}

fn estimated_umik_spl_db(rms_dbfs: Option<f64>, sensitivity_factor_db: Option<f64>) -> Option<f64> {
    let estimate =
        SPL_REFERENCE_DB + UMIK_MAXIMUM_VOLUME_DIGITAL_GAIN_DB - sensitivity_factor_db? + rms_dbfs?;
    estimate.is_finite().then_some(estimate)
}

fn measurement_level_status(
    peak_dbfs: Option<f64>,
    estimated_spl_db: Option<f64>,
) -> LiveMeasurementLevelStatus {
    let Some(peak_dbfs) = peak_dbfs else {
        return LiveMeasurementLevelStatus::Waiting;
    };
    if peak_dbfs >= LEVEL_CLIPPING_PEAK_DBFS {
        return LiveMeasurementLevelStatus::Clipping;
    }
    if peak_dbfs > LEVEL_HIGH_PEAK_DBFS
        || estimated_spl_db.is_some_and(|spl| spl > LEVEL_RECOMMENDED_MAXIMUM_SPL_DB)
    {
        return LiveMeasurementLevelStatus::High;
    }
    if peak_dbfs < LEVEL_RECOMMENDED_MINIMUM_PEAK_DBFS {
        return LiveMeasurementLevelStatus::TooLow;
    }
    LiveMeasurementLevelStatus::Good
}

impl LiveCaptureMonitor {
    pub(crate) fn new(sweep: &LiveSweepReference, sensitivity_factor_db: Option<f64>) -> Self {
        Self {
            sweep: sweep.clone(),
            sensitivity_factor_db,
            start_marker: None,
            end_marker: None,
            expected_sweep_start_sample: None,
            expected_sweep_end_sample: None,
            next_start_search_sample: 0,
            next_end_search_sample: 0,
            automatic_completion_sample: None,
            observed_peak_linear: 0.0,
            observed_sample_count: 0,
            last_observed_sample: 0,
            measurement_peak_linear: 0.0,
            measurement_energy: 0.0,
            measurement_sample_count: 0,
            last_level_sample: 0,
        }
    }

    pub(crate) fn initial_progress(&self) -> LiveCaptureProgress {
        self.progress(0, false)
    }

    pub(crate) fn maximum_snapshot_samples(&self) -> usize {
        self.sweep
            .timing_markers
            .iter()
            .map(|marker| marker.samples.len())
            .max()
            .unwrap_or_default()
            .saturating_add(MARKER_SEARCH_MARGIN_SAMPLES * 2)
            .max(usize::try_from(PROJECT_SAMPLE_RATE_HZ).unwrap_or(48_000) * 4)
    }

    pub(crate) fn inspect(
        &mut self,
        snapshot_start_sample: usize,
        samples: &[f32],
    ) -> LiveCaptureMonitorUpdate {
        let captured_samples = snapshot_start_sample.saturating_add(samples.len());
        self.update_observed_input(snapshot_start_sample, samples);
        self.find_start_marker(snapshot_start_sample, samples);
        self.update_measurement_level(snapshot_start_sample, samples);
        self.find_end_marker(snapshot_start_sample, samples);
        let should_complete = self
            .automatic_completion_sample
            .is_some_and(|completion| captured_samples >= completion);
        LiveCaptureMonitorUpdate {
            should_complete,
            progress: self.progress(captured_samples, should_complete),
        }
    }

    fn automatic_completion_armed(&self) -> bool {
        self.sweep.timing_markers.len() >= 2
    }

    fn find_start_marker(&mut self, snapshot_start_sample: usize, samples: &[f32]) {
        let captured_samples = snapshot_start_sample.saturating_add(samples.len());
        if self.start_marker.is_some()
            || !self.automatic_completion_armed()
            || captured_samples < self.next_start_search_sample
        {
            return;
        }
        let marker = &self.sweep.timing_markers[0];
        if captured_samples < marker.samples.len() {
            self.next_start_search_sample = marker.samples.len();
            return;
        }
        let window_length = marker
            .samples
            .len()
            .saturating_add(MARKER_SEARCH_MARGIN_SAMPLES * 2);
        let window_start = captured_samples
            .saturating_sub(window_length)
            .max(snapshot_start_sample);
        let window_offset = window_start.saturating_sub(snapshot_start_sample);
        let window = samples[window_offset..]
            .iter()
            .map(|sample| f64::from(*sample))
            .collect::<Vec<_>>();
        let matched = recognize_timing_marker(marker, &window)
            .ok()
            .and_then(|candidates| {
                candidates.into_iter().max_by(|left, right| {
                    left.absolute_correlation
                        .total_cmp(&right.absolute_correlation)
                })
            })
            .map(|candidate| TimingMarkerMatch {
                capture_start_sample: candidate.capture_start_sample + window_start as f64,
                absolute_correlation: candidate.absolute_correlation,
            });
        self.next_start_search_sample = captured_samples.saturating_add(MARKER_SEARCH_STEP_SAMPLES);
        let Some(matched) = matched else {
            return;
        };
        let source_delta =
            self.sweep.measurement_source_start_sample as f64 - marker.source_start_sample as f64;
        self.expected_sweep_start_sample = Some(matched.capture_start_sample + source_delta);
        self.expected_sweep_end_sample = self
            .expected_sweep_start_sample
            .map(|start| start + self.sweep.samples.len() as f64);
        self.last_level_sample = self
            .expected_sweep_start_sample
            .unwrap_or(0.0)
            .max(0.0)
            .floor() as usize;
        self.start_marker = Some(matched);

        let last_marker = self
            .sweep
            .timing_markers
            .last()
            .expect("automatic completion requires two markers");
        let expected_last_start = matched.capture_start_sample
            + (last_marker.source_start_sample - marker.source_start_sample) as f64;
        self.next_end_search_sample = expected_last_start.max(0.0).floor() as usize
            + last_marker
                .samples
                .len()
                .saturating_sub(MARKER_SEARCH_MARGIN_SAMPLES);
    }

    fn find_end_marker(&mut self, snapshot_start_sample: usize, samples: &[f32]) {
        let captured_samples = snapshot_start_sample.saturating_add(samples.len());
        if self.end_marker.is_some()
            || self.start_marker.is_none()
            || captured_samples < self.next_end_search_sample
        {
            return;
        }
        let first_marker = &self.sweep.timing_markers[0];
        let last_marker = self
            .sweep
            .timing_markers
            .last()
            .expect("start detection requires two markers");
        let start_match = self.start_marker.expect("checked above");
        let expected = start_match.capture_start_sample
            + (last_marker.source_start_sample - first_marker.source_start_sample) as f64;
        let window_start = (expected - MARKER_SEARCH_MARGIN_SAMPLES as f64)
            .max(0.0)
            .floor() as usize;
        let maximum_window_end =
            (expected + last_marker.samples.len() as f64 + MARKER_SEARCH_MARGIN_SAMPLES as f64)
                .ceil() as usize;
        let window_start = window_start.max(snapshot_start_sample);
        let window_end = captured_samples.min(maximum_window_end);
        if window_end <= window_start || window_end - window_start < last_marker.samples.len() {
            self.next_end_search_sample =
                captured_samples.saturating_add(MARKER_SEARCH_STEP_SAMPLES);
            return;
        }
        let local_window_start = window_start.saturating_sub(snapshot_start_sample);
        let local_window_end = window_end.saturating_sub(snapshot_start_sample);
        let window = samples[local_window_start..local_window_end]
            .iter()
            .map(|sample| f64::from(*sample))
            .collect::<Vec<_>>();
        let matched = recognize_timing_marker(last_marker, &window)
            .ok()
            .and_then(|candidates| {
                candidates.into_iter().min_by(|left, right| {
                    (left.capture_start_sample + window_start as f64 - expected)
                        .abs()
                        .total_cmp(
                            &(right.capture_start_sample + window_start as f64 - expected).abs(),
                        )
                })
            })
            .map(|candidate| TimingMarkerMatch {
                capture_start_sample: candidate.capture_start_sample + window_start as f64,
                absolute_correlation: candidate.absolute_correlation,
            })
            .filter(|candidate| {
                (candidate.capture_start_sample - expected).abs()
                    <= MARKER_SEARCH_MARGIN_SAMPLES as f64
            });
        self.next_end_search_sample = captured_samples.saturating_add(MARKER_SEARCH_STEP_SAMPLES);
        let Some(matched) = matched else {
            return;
        };
        // The uploaded end marker already follows the measurement sweep. Once
        // the complete marker is present, no fixed extra wait is needed; final
        // deconvolution still rejects a WAV whose built-in post-sweep spacing
        // does not contain the required IR tail.
        self.automatic_completion_sample =
            Some(matched.capture_start_sample.ceil() as usize + last_marker.samples.len());
        self.end_marker = Some(matched);
    }

    fn update_measurement_level(&mut self, snapshot_start_sample: usize, samples: &[f32]) {
        let (Some(start), Some(end)) = (
            self.expected_sweep_start_sample,
            self.expected_sweep_end_sample,
        ) else {
            return;
        };
        let start = start.max(0.0).floor() as usize;
        let end = end.ceil() as usize;
        let captured_samples = snapshot_start_sample.saturating_add(samples.len());
        let from = self
            .last_level_sample
            .max(start)
            .max(snapshot_start_sample)
            .min(captured_samples);
        let to = captured_samples.min(end);
        if to <= from {
            return;
        }
        let local_from = from.saturating_sub(snapshot_start_sample);
        let local_to = to.saturating_sub(snapshot_start_sample);
        for &sample in &samples[local_from..local_to] {
            let sample = f64::from(sample);
            if !sample.is_finite() {
                continue;
            }
            self.measurement_peak_linear = self.measurement_peak_linear.max(sample.abs());
            self.measurement_energy += sample * sample;
            self.measurement_sample_count += 1;
        }
        self.last_level_sample = to;
    }

    fn update_observed_input(&mut self, snapshot_start_sample: usize, samples: &[f32]) {
        let captured_samples = snapshot_start_sample.saturating_add(samples.len());
        let from = self
            .last_observed_sample
            .max(snapshot_start_sample)
            .min(captured_samples);
        if from >= captured_samples {
            return;
        }
        let local_from = from.saturating_sub(snapshot_start_sample);
        for &sample in &samples[local_from..] {
            let sample = f64::from(sample);
            if !sample.is_finite() {
                continue;
            }
            self.observed_peak_linear = self.observed_peak_linear.max(sample.abs());
            self.observed_sample_count += 1;
        }
        self.last_observed_sample = captured_samples;
    }

    fn progress(&self, captured_samples: usize, saving: bool) -> LiveCaptureProgress {
        let peak_dbfs = if self.measurement_sample_count > 0 {
            Some(amplitude_dbfs(self.measurement_peak_linear))
        } else {
            (self.observed_sample_count > 0).then(|| amplitude_dbfs(self.observed_peak_linear))
        };
        let rms_dbfs = rms_dbfs(self.measurement_energy, self.measurement_sample_count);
        let estimated_spl_db = estimated_umik_spl_db(rms_dbfs, self.sensitivity_factor_db);
        let phase = if saving {
            LiveCaptureProgressPhase::SavingMeasurement
        } else if self.end_marker.is_some() {
            LiveCaptureProgressPhase::EndMarkerDetected
        } else if self.start_marker.is_some()
            && self
                .expected_sweep_start_sample
                .is_some_and(|start| captured_samples as f64 >= start)
        {
            LiveCaptureProgressPhase::MeasuringSweep
        } else if self.start_marker.is_some() {
            LiveCaptureProgressPhase::StartMarkerDetected
        } else {
            LiveCaptureProgressPhase::WaitingForStart
        };
        LiveCaptureProgress {
            algorithm_version: LIVE_CAPTURE_ENDPOINT_VERSION,
            phase,
            elapsed_seconds: captured_samples as f64 / f64::from(PROJECT_SAMPLE_RATE_HZ),
            peak_dbfs,
            rms_dbfs,
            estimated_spl_db,
            level_status: if self.start_marker.is_some() {
                measurement_level_status(peak_dbfs, estimated_spl_db)
            } else {
                LiveMeasurementLevelStatus::Waiting
            },
            start_marker_detected: self.start_marker.is_some(),
            end_marker_detected: self.end_marker.is_some(),
            automatic_completion_armed: self.automatic_completion_armed(),
        }
    }
}

fn assess_measurement_level(
    capture_samples: &[f64],
    detection: &WirelessSweepDetection,
    sensitivity_factor_db: Option<f64>,
    input_clipped: bool,
) -> Result<LiveMeasurementLevelAssessment, String> {
    let start = detection.estimated_sweep_start_sample.max(0.0).floor() as usize;
    let end = detection.estimated_sweep_end_sample_exclusive.ceil() as usize;
    let samples = capture_samples.get(start..end).ok_or_else(|| {
        "recognized measurement bounds lie outside the microphone capture".to_string()
    })?;
    if samples.is_empty() {
        return Err("recognized measurement has no level samples".to_string());
    }
    let mut peak = 0.0_f64;
    let mut energy = 0.0_f64;
    for (index, &sample) in samples.iter().enumerate() {
        if !sample.is_finite() {
            return Err(format!(
                "recognized measurement level sample {index} is not finite"
            ));
        }
        peak = peak.max(sample.abs());
        energy += sample * sample;
    }
    let peak_dbfs = amplitude_dbfs(peak);
    let rms_dbfs = amplitude_dbfs((energy / samples.len() as f64).sqrt());
    let estimated_spl_db = estimated_umik_spl_db(Some(rms_dbfs), sensitivity_factor_db);
    let status = if input_clipped {
        LiveMeasurementLevelStatus::Clipping
    } else {
        measurement_level_status(Some(peak_dbfs), estimated_spl_db)
    };
    // The estimated SPL depends on the calibration header and OS input-volume
    // assumption, so it is guidance rather than an acceptance gate. A quieter
    // digital peak remains usable when the independent SNR, reconstruction-fit,
    // and sweep-correlation checks pass.
    let acceptable_for_measurement = !input_clipped
        && (LEVEL_MINIMUM_ACCEPTED_PEAK_DBFS..=LEVEL_HIGH_PEAK_DBFS).contains(&peak_dbfs);
    Ok(LiveMeasurementLevelAssessment {
        algorithm_version: LIVE_LEVEL_ASSESSMENT_VERSION,
        status,
        acceptable_for_measurement,
        measurement_peak_dbfs: peak_dbfs,
        measurement_rms_dbfs: rms_dbfs,
        estimated_spl_db,
        estimated_spl_assumption: estimated_spl_db
            .map(|_| "UMIK Sens Factor with operating-system input gain fixed at 0 dB"),
        minimum_accepted_peak_dbfs: LEVEL_MINIMUM_ACCEPTED_PEAK_DBFS,
        recommended_peak_minimum_dbfs: LEVEL_RECOMMENDED_MINIMUM_PEAK_DBFS,
        recommended_peak_maximum_dbfs: LEVEL_HIGH_PEAK_DBFS,
        recommended_spl_minimum_db: LEVEL_RECOMMENDED_MINIMUM_SPL_DB,
        recommended_spl_maximum_db: LEVEL_RECOMMENDED_MAXIMUM_SPL_DB,
    })
}

fn timing_marker_rate_ratio(
    sweep: &LiveSweepReference,
    capture_samples: &[f64],
    main_detection: &WirelessSweepDetection,
) -> Result<Option<(f64, f64)>, String> {
    if sweep.timing_markers.len() < 2 {
        return Ok(None);
    }
    let mut matched = Vec::with_capacity(sweep.timing_markers.len());
    for marker in &sweep.timing_markers {
        let candidates = recognize_timing_marker(marker, capture_samples)?;
        let source_delta =
            marker.source_start_sample as f64 - sweep.measurement_source_start_sample as f64;
        let expected = main_detection.estimated_sweep_start_sample + source_delta;
        let selected = candidates
            .into_iter()
            .min_by(|left, right| {
                (left.capture_start_sample - expected)
                    .abs()
                    .total_cmp(&(right.capture_start_sample - expected).abs())
            })
            .filter(|candidate| {
                (candidate.capture_start_sample - expected).abs()
                    <= f64::from(PROJECT_SAMPLE_RATE_HZ)
            });
        let Some(capture_match) = selected else {
            return Ok(None);
        };
        matched.push((
            marker.source_start_sample as f64,
            capture_match.capture_start_sample,
        ));
    }
    matched.sort_by(|left, right| left.0.total_cmp(&right.0));
    let first = matched
        .first()
        .ok_or_else(|| "timing-marker fit has no first point".to_string())?;
    let last = matched
        .last()
        .ok_or_else(|| "timing-marker fit has no last point".to_string())?;
    let source_gap = last.0 - first.0;
    let capture_gap = last.1 - first.1;
    if source_gap <= 0.0 || capture_gap <= 0.0 {
        return Ok(None);
    }
    let ratio = capture_gap / source_gap;
    let playback_offset = first.1 - ratio * first.0;
    let drift_ppm = (ratio - 1.0) * 1_000_000.0;
    if !ratio.is_finite() || !playback_offset.is_finite() || drift_ppm.abs() > 2_000.0 {
        return Err(format!(
            "repeated timing markers imply an excessive {drift_ppm:.1} ppm clock mismatch"
        ));
    }
    Ok(Some((ratio, playback_offset)))
}

fn measurement_detection(
    sweep: &LiveSweepReference,
    capture_samples: &[f64],
    detection: &WirelessSweepDetection,
) -> Result<WirelessSweepDetection, String> {
    let mut adjusted = detection.clone();
    if let Some((ratio, playback_offset)) =
        timing_marker_rate_ratio(sweep, capture_samples, detection)?
    {
        adjusted.capture_samples_per_reference_sample = ratio;
        adjusted.estimated_clock_drift_ppm = (ratio - 1.0) * 1_000_000.0;
        adjusted.clock_drift_evidence = WirelessClockDriftEvidence::RepeatedTimingMarkers;
        adjusted.timing_fit_rms_samples = 0.0;
        adjusted.estimated_sweep_start_sample =
            playback_offset + ratio * sweep.measurement_source_start_sample as f64;
        adjusted.estimated_sweep_end_sample_exclusive =
            adjusted.estimated_sweep_start_sample + ratio * sweep.samples.len() as f64;
    } else {
        // The intra-sweep frequency slope is retained as diagnostics but room
        // group delay can bias it. Without repeated markers, do not warp the
        // measured magnitude response using that non-qualifying estimate.
        adjusted.capture_samples_per_reference_sample = 1.0;
        adjusted.estimated_clock_drift_ppm = 0.0;
    }
    Ok(adjusted)
}

/// Reject at import a marker-carrying sweep whose own layout can never satisfy
/// the capture-time IR-tail requirement. Capture stops at the end marker's
/// end, so the available tail after the main sweep is
/// `(end_marker_start - main_sweep_end) + end_marker_length`; deconvolution
/// later requires `impulse_length` samples there (times a clock ratio of
/// ~1.0). Without this check a doomed custom WAV fails only after every full
/// sweep playback (2026-07-28 release review).
fn validate_sweep_ir_tail_capacity(
    channel: LiveChannel,
    main_sweep_end_sample: usize,
    end_marker_start_sample: usize,
    end_marker_length_samples: usize,
    required_tail_samples: usize,
) -> Result<(), String> {
    let end_marker_end = end_marker_start_sample.saturating_add(end_marker_length_samples);
    let available_tail = end_marker_end.saturating_sub(main_sweep_end_sample);
    if available_tail < required_tail_samples {
        return Err(format!(
            "the {} sweep leaves only {available_tail} samples between the main sweep's end \
             and the end marker's end, but deconvolution needs {required_tail_samples} \
             (about {:.2} s at 48 kHz) of impulse-response tail there; captures from this \
             file would always fail, so it is rejected at import",
            channel.as_str(),
            required_tail_samples as f64 / f64::from(PROJECT_SAMPLE_RATE_HZ),
        ));
    }
    Ok(())
}

/// The five octave bands used for the per-band SNR diagnostic and the boost
/// SNR requirement, covering the correction range.
const OCTAVE_SNR_BANDS_HZ: [(f64, f64); 5] = [
    (20.0, 40.0),
    (40.0, 80.0),
    (80.0, 160.0),
    (160.0, 320.0),
    (320.0, 640.0),
];
/// Boost may only be granted where the measured per-octave SNR clears this
/// bar (2026-07-29 expert review, finding 10): adding energy into a band the
/// measurement cannot even resolve above its own noise builds the boost on
/// noise. Cuts keep the broadband >=20 dB gate - a cut of a well-measured
/// peak is not invalidated by a noisy neighboring octave.
const LIVE_MINIMUM_BOOST_BAND_SNR_DB: f64 = 25.0;

/// Mean per-sample power of `samples[span]` restricted to [low_hz, high_hz)
/// with 2nd-order Butterworth edges, in dB. `None` for degenerate spans.
fn band_power_db(
    samples: &[f64],
    span: std::ops::Range<usize>,
    low_hz: f64,
    high_hz: f64,
) -> Option<f64> {
    let span = span.start.min(samples.len())..span.end.min(samples.len());
    if span.len() < 1_024 {
        return None;
    }
    let sample_rate = f64::from(PROJECT_SAMPLE_RATE_HZ);
    let biquad = |f0: f64, high_pass: bool| {
        let w0 = 2.0 * std::f64::consts::PI * f0 / sample_rate;
        let alpha = w0.sin() * std::f64::consts::FRAC_1_SQRT_2;
        let cos_w0 = w0.cos();
        let (b0, b1, b2) = if high_pass {
            ((1.0 + cos_w0) / 2.0, -(1.0 + cos_w0), (1.0 + cos_w0) / 2.0)
        } else {
            ((1.0 - cos_w0) / 2.0, 1.0 - cos_w0, (1.0 - cos_w0) / 2.0)
        };
        (b0, b1, b2, 1.0 + alpha, -2.0 * cos_w0, 1.0 - alpha)
    };
    let stages = [biquad(low_hz, true), biquad(high_hz, false)];
    let mut states = [[0.0_f64; 4]; 2];
    let settle = 512.min(span.len() / 4);
    let mut sum = 0.0;
    let mut counted = 0_usize;
    for (index, &input) in samples[span].iter().enumerate() {
        let mut value = input;
        for (stage, state) in stages.iter().zip(&mut states) {
            let (b0, b1, b2, a0, a1, a2) = *stage;
            let output =
                (b0 * value + b1 * state[0] + b2 * state[1] - a1 * state[2] - a2 * state[3]) / a0;
            state[1] = state[0];
            state[0] = value;
            state[3] = state[2];
            state[2] = output;
            value = output;
        }
        if index >= settle {
            sum += value * value;
            counted += 1;
        }
    }
    if counted == 0 {
        return None;
    }
    let mean = sum / counted as f64;
    (mean.is_finite() && mean > 0.0).then(|| 10.0 * mean.log10())
}

/// Per-octave SNR of the recognized sweep against the pre-sweep noise floor.
/// Powers are per-sample within each segment, so the sweep's equal-time-per-
/// octave layout and the stationary noise compare fairly; this is a
/// comparative diagnostic and boost gate, not calibrated SPL.
fn octave_band_snr_db(
    samples: &[f64],
    sweep_start_sample: f64,
    sweep_end_sample: f64,
) -> Option<Vec<Option<f64>>> {
    if !sweep_start_sample.is_finite() || !sweep_end_sample.is_finite() {
        return None;
    }
    let sweep_start = sweep_start_sample.max(0.0) as usize;
    let sweep_end = (sweep_end_sample.max(0.0) as usize).min(samples.len());
    let noise_end = sweep_start.saturating_sub(256);
    if sweep_end <= sweep_start || noise_end < 2_048 {
        return None;
    }
    Some(
        OCTAVE_SNR_BANDS_HZ
            .iter()
            .map(|&(low_hz, high_hz)| {
                let signal = band_power_db(samples, sweep_start..sweep_end, low_hz, high_hz)?;
                let noise = band_power_db(samples, 0..noise_end, low_hz, high_hz)?;
                Some(signal - noise)
            })
            .collect(),
    )
}

/// Reject an isolated 2.1 capture whose spectrum contradicts its declared
/// role (2026-07-29 expert review, finding 9). A sub-only capture with real
/// energy well above the crossover means the mains were not actually muted
/// (or bass management leaked the sweep into them); a main-only capture with
/// full sub-band energy means the sub stayed live. Either silently breaks the
/// complex-sum premise the crossover search rests on. Thresholds are
/// deliberately loose (a real 24 dB/oct sub sits ~28 dB down one octave-plus
/// above its crossover; a bass-managed main sits well below -6 dB half an
/// octave under it) so room modes and measurement noise do not false-trip
/// them; they are constants precisely so field data can retune them.
const SUB_ONLY_HIGH_BAND_REJECTION_DB: f64 = 12.0;
const MAIN_ONLY_SUB_BAND_REJECTION_DB: f64 = 6.0;
/// A magnitude excess alone cannot distinguish the other path being live from
/// stationary ambient noise filling the checked band - and the first real v5
/// session hit exactly that (2026-07-29): a main-only capture at ~79 dB SPL
/// with the sub physically powered off failed at 6.0 dB excess because the
/// room's 20-40 Hz noise floor sat at the passband level. Before rejecting,
/// the checked band must therefore be measurable above the room's own noise
/// in the capture domain: sweep-span band power against pre-sweep band power,
/// exactly as the octave SNR diagnostic measures. Replayed on the real
/// captures, the sub-powered-off noisy session reads 9.9-10.9 dB here while
/// a genuinely live subwoofer reads 32.1-33.1 dB, so 20 dB separates them
/// with ~10 dB of margin on each side. (An impulse-domain arrival-vs-tail
/// test was tried first and discarded: this room's noise is intermittent,
/// and a burst landing inside the arrival window is indistinguishable from
/// a real arrival by position alone.) Below the bar the capture is admitted
/// and the finding becomes a diagnostic, because rejecting it would demand a
/// playback level the level guidance then calls too loud.
const LEAKAGE_BAND_MINIMUM_SNR_DB: f64 = 20.0;

/// One rejection-worthy finding from an isolated capture, split by whether the
/// evidence is measurable above the room's noise.
enum IsolatedLeakageFinding {
    /// The flagged band is well above the ambient noise floor: the other path
    /// was live. Hard rejection.
    Leakage(String),
    /// The flagged band is indistinguishable from the room's noise floor, so
    /// leakage cannot be judged either way. Recorded as a diagnostic on an
    /// otherwise accepted capture.
    NoiseLimited(String),
}

/// Sweep-span band power against pre-sweep band power for one arbitrary band,
/// mirroring `octave_band_snr_db`'s span conventions.
fn checked_band_snr_db(
    samples: &[f64],
    sweep_start_sample: f64,
    sweep_end_sample: f64,
    low_hz: f64,
    high_hz: f64,
) -> Option<f64> {
    if !sweep_start_sample.is_finite() || !sweep_end_sample.is_finite() {
        return None;
    }
    let sweep_start = sweep_start_sample.max(0.0) as usize;
    let sweep_end = (sweep_end_sample.max(0.0) as usize).min(samples.len());
    let noise_end = sweep_start.saturating_sub(256);
    if sweep_end <= sweep_start || noise_end < 2_048 {
        return None;
    }
    let signal = band_power_db(samples, sweep_start..sweep_end, low_hz, high_hz)?;
    let noise = band_power_db(samples, 0..noise_end, low_hz, high_hz)?;
    Some(signal - noise)
}

/// Mean level of `response` restricted to [low_hz, high_hz), in dB. `None`
/// when fewer than eight bins fall inside the band.
fn band_mean_level_db(
    response: &eqforbeginner_dsp_core::analysis::FrequencyResponse,
    low_hz: f64,
    high_hz: f64,
) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0_usize;
    for (frequency, magnitude) in response.frequencies_hz.iter().zip(&response.magnitude_db) {
        if (low_hz..high_hz).contains(frequency) && magnitude.is_finite() {
            sum += magnitude;
            count += 1;
        }
    }
    (count >= 8).then(|| sum / count as f64)
}

fn isolated_path_leakage_issue(
    kind: LiveCaptureKind,
    position_id: &str,
    search: Option<&LiveSubwooferSearchSummary>,
    response: &eqforbeginner_dsp_core::analysis::FrequencyResponse,
    capture_samples: &[f64],
    sweep_start_sample: f64,
    sweep_end_sample: f64,
) -> Option<IsolatedLeakageFinding> {
    if !matches!(
        kind,
        LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly
    ) {
        return None;
    }
    let crossover_hz = search?
        .candidates
        .iter()
        .find(|candidate| candidate.id == position_id)?
        .crossover_hz;
    let (excess_db, checked_low_hz, checked_high_hz, label) = match kind {
        LiveCaptureKind::SubOnly => {
            let passband = band_mean_level_db(response, 0.5 * crossover_hz, 0.9 * crossover_hz)?;
            let checked_low = 2.2 * crossover_hz;
            let checked_high = (10.0 * crossover_hz).min(500.0);
            let high_band = band_mean_level_db(response, checked_low, checked_high)?;
            (
                high_band - (passband - SUB_ONLY_HIGH_BAND_REJECTION_DB),
                checked_low,
                checked_high,
                "sub_only_high_band",
            )
        }
        LiveCaptureKind::SubMainOnly => {
            let passband = band_mean_level_db(
                response,
                1.5 * crossover_hz,
                (4.0 * crossover_hz).min(500.0),
            )?;
            let checked_low = 0.3 * crossover_hz;
            let checked_high = 0.5 * crossover_hz;
            let sub_band = band_mean_level_db(response, checked_low, checked_high)?;
            (
                sub_band - (passband - MAIN_ONLY_SUB_BAND_REJECTION_DB),
                checked_low,
                checked_high,
                "main_only_sub_band",
            )
        }
        _ => return None,
    };
    if excess_db <= 0.0 {
        return None;
    }
    // The band is too loud for the declared role - but is it the other path
    // playing, or the room's own noise floor? When the capture-domain SNR is
    // unavailable (spans too short), keep the plain magnitude rejection so
    // this can never be weaker than the original gate.
    match checked_band_snr_db(
        capture_samples,
        sweep_start_sample,
        sweep_end_sample,
        checked_low_hz,
        checked_high_hz,
    ) {
        Some(band_snr_db) if band_snr_db < LEAKAGE_BAND_MINIMUM_SNR_DB => {
            Some(IsolatedLeakageFinding::NoiseLimited(format!(
                "{label}_noise_limited:{excess_db:.1}dB,band_snr:{band_snr_db:.1}dB"
            )))
        }
        _ => Some(IsolatedLeakageFinding::Leakage(format!(
            "{label}_leakage:{excess_db:.1}dB"
        ))),
    }
}

/// RMS of the recognized start-marker span restricted to the >=650 Hz band,
/// in dBFS. `None` when the span is degenerate or falls outside the capture.
///
/// The band restriction matters: about a quarter of the bundled marker's
/// energy sits below 650 Hz, where the correction filter is allowed to cut,
/// so a full-band marker RMS would shift by up to ~1 dB when the verification
/// sweep plays through the trial - and the session-gain gate would blame the
/// user's volume. At and above 650 Hz the product filter is unity by
/// construction, so this band isolates true playback/capture gain.
fn marker_capture_rms_dbfs(
    capture_samples: &[f64],
    marker_start_sample: f64,
    marker_length_samples: f64,
) -> Option<f64> {
    if !marker_start_sample.is_finite() || !marker_length_samples.is_finite() {
        return None;
    }
    let start = marker_start_sample.max(0.0).floor() as usize;
    let length = marker_length_samples.ceil() as usize;
    let end = start.checked_add(length)?.min(capture_samples.len());
    if end <= start || end - start < 256 {
        return None;
    }
    // RBJ high-pass biquad at the 650 Hz correction-return frequency,
    // Butterworth Q. 12 dB/oct leaves <1% of the marker's low-band energy,
    // which moves the RMS by well under 0.05 dB even against a full-depth cut.
    let sample_rate = f64::from(PROJECT_SAMPLE_RATE_HZ);
    let w0 = 2.0 * std::f64::consts::PI * 650.0 / sample_rate;
    let alpha = w0.sin() * std::f64::consts::FRAC_1_SQRT_2;
    let cos_w0 = w0.cos();
    let b0 = (1.0 + cos_w0) / 2.0;
    let b1 = -(1.0 + cos_w0);
    let b2 = (1.0 + cos_w0) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;
    let mut x1 = 0.0;
    let mut x2 = 0.0;
    let mut y1 = 0.0;
    let mut y2 = 0.0;
    let settle = 128.min((end - start) / 4);
    let mut sum_squares = 0.0;
    let mut counted = 0_usize;
    for (index, &sample) in capture_samples[start..end].iter().enumerate() {
        let filtered = (b0 * sample + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2) / a0;
        x2 = x1;
        x1 = sample;
        y2 = y1;
        y1 = filtered;
        if index >= settle {
            sum_squares += filtered * filtered;
            counted += 1;
        }
    }
    if counted == 0 {
        return None;
    }
    let mean_square = sum_squares / counted as f64;
    if !mean_square.is_finite() || mean_square <= 0.0 {
        return None;
    }
    Some(amplitude_dbfs(mean_square.sqrt()))
}

/// Every isolated capture that feeds one crossover synthesis must have been
/// made at the same playback/capture gain: the scoring compares levels across
/// captures, and the complex sum assumes the main and sub paths kept their
/// real relative level. The start marker is the constant-source probe for
/// that. A missing value means the capture predates marker-level recording
/// and must be retaken (2026-07-29 expert review, finding 1).
const MAXIMUM_ISOLATED_MARKER_LEVEL_DEVIATION_DB: f64 = 0.3;

fn validate_isolated_marker_levels(levels: &[(String, Option<f64>)]) -> Result<(), String> {
    let mut values = Vec::with_capacity(levels.len());
    for (label, level) in levels {
        match level {
            Some(value) if value.is_finite() => values.push(*value),
            _ => {
                return Err(format!(
                    "{label} has no start-marker level evidence (it was captured before \
                     marker-level recording existed); recapture the isolated paths in one \
                     session at one volume"
                ))
            }
        }
    }
    let mut sorted = values.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    for ((label, _), value) in levels.iter().zip(&values) {
        let deviation = (value - median).abs();
        if deviation > MAXIMUM_ISOLATED_MARKER_LEVEL_DEVIATION_DB {
            return Err(format!(
                "{label} was captured {deviation:.2} dB away from the session's median \
                 marker level (allowed {MAXIMUM_ISOLATED_MARKER_LEVEL_DEVIATION_DB:.1} dB); \
                 the playback or capture volume changed between isolated captures, which \
                 breaks the crossover comparison. Fix the volume and recapture the \
                 isolated paths"
            ));
        }
    }
    Ok(())
}

fn marker_pair_measurement_detection(
    pair: &TimingMarkerPairDetection,
    capture_samples: &[f64],
    impulse_length_samples: usize,
) -> Result<WirelessSweepDetection, String> {
    // The retained segment must still contain the whole signed pre-zero window
    // after the capture/reference ratio stretches it, plus a little slack for
    // the pre-sweep noise estimate.
    let pre_roll = (MARKER_REFERENCED_IMPULSE_PRE_ZERO_SAMPLES as f64
        * pair.capture_samples_per_reference_sample)
        .ceil() as usize
        + 960;
    let core_start = pair.estimated_sweep_start_sample.floor() as usize;
    let core_end = pair.estimated_sweep_end_sample_exclusive.ceil() as usize;
    let required_post_roll =
        (impulse_length_samples as f64 * pair.capture_samples_per_reference_sample).ceil() as usize;
    let capture_start = core_start.saturating_sub(pre_roll);
    let capture_end = core_end
        .checked_add(required_post_roll)
        .ok_or_else(|| "timing-marker capture bound overflowed".to_string())?;
    if capture_end > capture_samples.len() {
        return Err(
            "end timing marker was found, but the microphone capture lacks the required impulse-response tail"
                .to_string(),
        );
    }
    let correlation = pair
        .first
        .absolute_correlation
        .min(pair.last.absolute_correlation);
    Ok(WirelessSweepDetection {
        algorithm_version:
            eqforbeginner_dsp_core::wireless_sweep::WIRELESS_SWEEP_RECOGNITION_VERSION,
        nominal_sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        estimated_sweep_start_sample: pair.estimated_sweep_start_sample,
        estimated_sweep_end_sample_exclusive: pair.estimated_sweep_end_sample_exclusive,
        start_signed_correlation: correlation,
        start_absolute_correlation: correlation,
        second_absolute_correlation: 0.0,
        polarity_inverted: false,
        capture_samples_per_reference_sample: pair.capture_samples_per_reference_sample,
        estimated_clock_drift_ppm: (pair.capture_samples_per_reference_sample - 1.0) * 1_000_000.0,
        clock_drift_evidence: WirelessClockDriftEvidence::RepeatedTimingMarkers,
        timing_fit_rms_samples: 0.0,
        segment_matches: Vec::new(),
        capture_segment: eqforbeginner_dsp_core::wireless_sweep::WirelessSweepCaptureSegment {
            start_sample: capture_start,
            end_sample_exclusive: capture_end,
            samples: capture_samples[capture_start..capture_end].to_vec(),
        },
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MeasurementSnapshot<'a> {
    project_version: &'static str,
    session_id: &'a str,
    system_mode: LiveSystemMode,
    subwoofer_setup: Option<&'a LiveSubwooferSetupSummary>,
    subwoofer_search: Option<&'a LiveSubwooferSearchSummary>,
    capture_kind: LiveCaptureKind,
    channel: LiveChannel,
    position_id: &'a str,
    input_device_id: &'a str,
    input_device_name: Option<&'a str>,
    input_channel_index: u16,
    source_channel_count: u16,
    sweep_sha256: &'a str,
    calibration_sha256: &'a str,
    evidence_generation: u64,
    trial_design_sha256: Option<&'a str>,
    detector_algorithm: &'static str,
    deconvolution_algorithm: &'static str,
    /// Identifies how the sibling REW-importable WAV in `rew/` was written.
    rew_export_algorithm: &'static str,
    sample_rate_hz: u32,
    raw_capture_wav: String,
    accepted: bool,
    issue_codes: &'a [String],
    diagnostic_codes: &'a [String],
    audio_stream_diagnostics: &'a LiveAudioStreamDiagnostics,
    capture_peak_dbfs: Option<f64>,
    capture_snr_db: Option<f64>,
    reconstruction_fit_db: Option<f64>,
    reconstruction_fit_required: bool,
    correlation: Option<f64>,
    clock_drift_ppm: Option<f64>,
    recognized_sweep_start_capture_sample: f64,
    impulse_start_relative_to_recognized_sweep_seconds: f64,
    marker_channel_analysis_version: &'static str,
    start_marker_source_channel: Option<ReferenceChannel>,
    end_marker_source_channel: Option<ReferenceChannel>,
    start_marker_detected: bool,
    end_marker_detected: bool,
    start_marker_rms_dbfs: Option<f64>,
    octave_band_snr_db: Option<&'a Vec<Option<f64>>>,
    automatic_completion_detected: bool,
    level_assessment: &'a LiveMeasurementLevelAssessment,
    timing_eligible: bool,
    frequency_hz: &'a [f64],
    calibrated_magnitude_db: &'a [f64],
    calibrated_impulse_samples: &'a [f64],
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedSubwooferSetup {
    crossover_hz: f64,
    main_delay_ms: f64,
    polarity_degrees: u16,
    sub_level_db: f64,
    confirmed_on_hardware: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedSubwooferCandidate {
    id: String,
    crossover_hz: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedSubwooferSearch {
    candidates: Vec<CachedSubwooferCandidate>,
    measured_main_delay_ms: f64,
    measured_polarity_degrees: u16,
    fixed_sub_level_db: f64,
    delay_minimum_ms: f64,
    delay_maximum_ms: f64,
    delay_step_ms: f64,
    fixed_timing_reference_channel: ReferenceChannel,
    sub_sweep_channel: LiveChannel,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedLevelAssessment {
    status: LiveMeasurementLevelStatus,
    acceptable_for_measurement: bool,
    measurement_peak_dbfs: f64,
    measurement_rms_dbfs: f64,
    estimated_spl_db: Option<f64>,
    estimated_spl_assumption: Option<String>,
    minimum_accepted_peak_dbfs: f64,
    recommended_peak_minimum_dbfs: f64,
    recommended_peak_maximum_dbfs: f64,
    recommended_spl_minimum_db: f64,
    recommended_spl_maximum_db: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedAudioStreamDiagnostics {
    #[serde(default)]
    xrun_count: u64,
    #[serde(default)]
    callback_lock_drop_frames: u64,
    #[serde(default)]
    timestamp_gap_frames: u64,
    #[serde(default)]
    timestamp_discontinuity_count: u64,
    #[serde(default)]
    missing_samples_at_end: usize,
    #[serde(default)]
    stream_error_count: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedMeasurementSnapshot {
    project_version: String,
    session_id: String,
    system_mode: LiveSystemMode,
    subwoofer_setup: Option<CachedSubwooferSetup>,
    subwoofer_search: Option<CachedSubwooferSearch>,
    capture_kind: LiveCaptureKind,
    channel: LiveChannel,
    position_id: String,
    input_device_id: String,
    input_device_name: Option<String>,
    input_channel_index: u16,
    source_channel_count: u16,
    sweep_sha256: String,
    calibration_sha256: String,
    trial_design_sha256: Option<String>,
    deconvolution_algorithm: String,
    sample_rate_hz: u32,
    raw_capture_wav: String,
    accepted: bool,
    issue_codes: Vec<String>,
    #[serde(default)]
    diagnostic_codes: Vec<String>,
    #[serde(default)]
    audio_stream_diagnostics: CachedAudioStreamDiagnostics,
    capture_peak_dbfs: Option<f64>,
    capture_snr_db: Option<f64>,
    reconstruction_fit_db: Option<f64>,
    #[serde(default)]
    reconstruction_fit_required: bool,
    correlation: Option<f64>,
    clock_drift_ppm: Option<f64>,
    #[serde(default)]
    recognized_sweep_start_capture_sample: Option<f64>,
    start_marker_detected: bool,
    end_marker_detected: bool,
    #[serde(default)]
    start_marker_rms_dbfs: Option<f64>,
    #[serde(default)]
    octave_band_snr_db: Option<Vec<Option<f64>>>,
    automatic_completion_detected: bool,
    level_assessment: CachedLevelAssessment,
    timing_eligible: bool,
    frequency_hz: Vec<f64>,
    calibrated_magnitude_db: Vec<f64>,
    calibrated_impulse_samples: Vec<f64>,
}

fn cached_subwoofer_setup_matches(
    cached: Option<&CachedSubwooferSetup>,
    current: Option<&LiveSubwooferSetupSummary>,
) -> bool {
    match (cached, current) {
        (None, None) => true,
        (Some(cached), Some(current)) => {
            cached.crossover_hz == current.crossover_hz
                && cached.main_delay_ms == current.main_delay_ms
                && cached.polarity_degrees == current.polarity_degrees
                && cached.sub_level_db == current.sub_level_db
                && cached.confirmed_on_hardware == current.confirmed_on_hardware
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn cached_subwoofer_search_matches(
    cached: Option<&CachedSubwooferSearch>,
    current: Option<&LiveSubwooferSearchSummary>,
) -> bool {
    match (cached, current) {
        (None, None) => true,
        (Some(cached), Some(current)) => {
            cached.candidates.len() == current.candidates.len()
                && cached
                    .candidates
                    .iter()
                    .zip(&current.candidates)
                    .all(|(cached, current)| {
                        cached.id == current.id && cached.crossover_hz == current.crossover_hz
                    })
                && cached.measured_main_delay_ms == current.measured_main_delay_ms
                && cached.measured_polarity_degrees == current.measured_polarity_degrees
                && cached.fixed_sub_level_db == current.fixed_sub_level_db
                && cached.delay_minimum_ms == current.delay_minimum_ms
                && cached.delay_maximum_ms == current.delay_maximum_ms
                && cached.delay_step_ms == current.delay_step_ms
                && cached.fixed_timing_reference_channel == current.fixed_timing_reference_channel
                && cached.sub_sweep_channel == current.sub_sweep_channel
        }
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn cached_snapshot_matches_session(
    cached: &CachedMeasurementSnapshot,
    session: &LiveSession,
    input_device_id: &str,
    input_channel_index: u16,
) -> bool {
    if cached.project_version != LIVE_MEASUREMENT_PROJECT_VERSION
        || cached.deconvolution_algorithm != KNOWN_SWEEP_DECONVOLUTION_VERSION
        || cached.sample_rate_hz != PROJECT_SAMPLE_RATE_HZ
        || cached.system_mode != session.system_mode
        || cached.input_device_id != input_device_id
        || cached.input_channel_index != input_channel_index
        || !cached.accepted
        || !cached.issue_codes.is_empty()
        || cached.trial_design_sha256.is_some()
        || cached.capture_kind == LiveCaptureKind::Verification
    {
        return false;
    }
    let Some(calibration) = session.calibration.as_ref() else {
        return false;
    };
    let Some(sweep) = session.sweeps.get(&cached.channel) else {
        return false;
    };
    if cached.calibration_sha256 != calibration.summary.sha256
        || cached.sweep_sha256 != sweep.summary.sha256
        || !cached_subwoofer_search_matches(
            cached.subwoofer_search.as_ref(),
            session.subwoofer_search.as_ref(),
        )
    {
        return false;
    }
    match cached.capture_kind {
        LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly => {
            session.system_mode == LiveSystemMode::SingleSub21 && session.subwoofer_search.is_some()
        }
        LiveCaptureKind::Baseline => cached_subwoofer_setup_matches(
            cached.subwoofer_setup.as_ref(),
            session.subwoofer_setup.as_ref(),
        ),
        LiveCaptureKind::Verification => false,
    }
}

fn restore_cached_snapshot(
    cached: CachedMeasurementSnapshot,
    snapshot_path: &Path,
    evidence: LiveCaptureEvidence,
) -> Result<StoredMeasurement, String> {
    if cached.frequency_hz.len() != cached.calibrated_magnitude_db.len()
        || cached.frequency_hz.len() < 3
        || cached.calibrated_impulse_samples.is_empty()
        || cached
            .frequency_hz
            .iter()
            .chain(&cached.calibrated_magnitude_db)
            .chain(&cached.calibrated_impulse_samples)
            .any(|value| !value.is_finite())
    {
        return Err("cached measurement arrays are empty, inconsistent, or non-finite".to_string());
    }
    // Reconstruct the response on the exact analysis grid the capture used,
    // so a restored measurement is indistinguishable from the session's own.
    // This was a hardcoded 32,768 - correct in the v3 era, silently stale
    // after the v4 analysis doubled to 65,536 - and the mismatch moved the
    // separated-path sub recommendation on the same cached data (6.8 ms fresh
    // versus 7.6 ms restored, replayed and pinned on the real session): the
    // arrival estimator sampled the same phase curves at half the density.
    // The cache compatibility check already requires this deconvolution
    // version, so every restorable snapshot carries an impulse this grid fits.
    let response = frequency_response(
        &cached.calibrated_impulse_samples,
        PROJECT_SAMPLE_RATE_HZ,
        SweepDeconvolutionConfig::default().analysis_fft_size,
    )
    .map_err(|error| format!("could not reconstruct cached response phase: {error}"))?;
    let raw_path = PathBuf::from(&cached.raw_capture_wav);
    if !raw_path.is_file() {
        return Err(format!(
            "cached raw capture is missing: {}",
            raw_path.display()
        ));
    }
    let captured_frames = hound::WavReader::open(&raw_path)
        .map_err(|error| format!("could not inspect cached raw WAV: {error}"))?
        .duration() as usize;
    let level_assessment = LiveMeasurementLevelAssessment {
        algorithm_version: LIVE_LEVEL_ASSESSMENT_VERSION,
        status: cached.level_assessment.status,
        acceptable_for_measurement: cached.level_assessment.acceptable_for_measurement,
        measurement_peak_dbfs: cached.level_assessment.measurement_peak_dbfs,
        measurement_rms_dbfs: cached.level_assessment.measurement_rms_dbfs,
        estimated_spl_db: cached.level_assessment.estimated_spl_db,
        estimated_spl_assumption: cached
            .level_assessment
            .estimated_spl_assumption
            .as_ref()
            .map(|_| "UMIK Sens Factor with operating-system input gain fixed at 0 dB"),
        minimum_accepted_peak_dbfs: cached.level_assessment.minimum_accepted_peak_dbfs,
        recommended_peak_minimum_dbfs: cached.level_assessment.recommended_peak_minimum_dbfs,
        recommended_peak_maximum_dbfs: cached.level_assessment.recommended_peak_maximum_dbfs,
        recommended_spl_minimum_db: cached.level_assessment.recommended_spl_minimum_db,
        recommended_spl_maximum_db: cached.level_assessment.recommended_spl_maximum_db,
    };
    let diagnostics = LiveAudioStreamDiagnostics {
        xrun_count: cached.audio_stream_diagnostics.xrun_count,
        callback_lock_drop_frames: cached.audio_stream_diagnostics.callback_lock_drop_frames,
        timestamp_gap_frames: cached.audio_stream_diagnostics.timestamp_gap_frames,
        timestamp_discontinuity_count: cached
            .audio_stream_diagnostics
            .timestamp_discontinuity_count,
        missing_samples_at_end: cached.audio_stream_diagnostics.missing_samples_at_end,
        stream_error_count: cached.audio_stream_diagnostics.stream_error_count,
    };
    let summary = LiveCaptureSummary {
        kind: cached.capture_kind,
        channel: cached.channel,
        position_id: cached.position_id,
        input_device_id: cached.input_device_id,
        input_device_name: cached.input_device_name,
        input_channel_index: cached.input_channel_index,
        source_channel_count: cached.source_channel_count,
        accepted: true,
        issue_codes: Vec::new(),
        diagnostic_codes: cached.diagnostic_codes,
        audio_stream_diagnostics: diagnostics,
        capture_peak_dbfs: cached.capture_peak_dbfs,
        capture_snr_db: cached.capture_snr_db,
        reconstruction_fit_db: cached.reconstruction_fit_db,
        reconstruction_fit_required: cached.reconstruction_fit_required,
        correlation: cached.correlation,
        clock_drift_ppm: cached.clock_drift_ppm,
        start_marker_detected: cached.start_marker_detected,
        end_marker_detected: cached.end_marker_detected,
        start_marker_rms_dbfs: cached.start_marker_rms_dbfs,
        octave_band_snr_db: cached.octave_band_snr_db,
        automatic_completion_detected: cached.automatic_completion_detected,
        level_assessment,
        captured_frames,
        raw_wav_path: raw_path.display().to_string(),
        measurement_snapshot_path: Some(snapshot_path.display().to_string()),
        frequency_bin_count: cached.frequency_hz.len(),
        timing_eligible: cached.timing_eligible,
        restored_from_cache: true,
        cache_source_session_id: Some(cached.session_id),
    };
    Ok(StoredMeasurement {
        summary,
        calibrated_frequency_response: response,
        calibrated_impulse_samples: cached.calibrated_impulse_samples,
        recognized_sweep_start_capture_sample: cached
            .recognized_sweep_start_capture_sample
            .unwrap_or(0.0),
        frequencies_hz: cached.frequency_hz,
        magnitude_db: cached.calibrated_magnitude_db,
        evidence,
    })
}

impl LiveMeasurementState {
    pub fn restore_accepted_measurements(
        &self,
        input_device_id: &str,
        input_channel_index: u16,
        scope: LiveRestoreScope,
    ) -> Result<LiveMeasurementCacheRestoreSummary, String> {
        if input_device_id.trim().is_empty() {
            return Err("measurement cache restore requires an input device".to_string());
        }
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err(
                "finish the active microphone capture before restoring measurements".to_string(),
            );
        }
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "start a live project before restoring measurements".to_string())?;
        if session.calibration.is_none() || session.sweeps.len() != 2 {
            return Err(
                "import the calibration and both exact sweep WAV files before restoring measurements"
                    .to_string(),
            );
        }
        if session.system_mode == LiveSystemMode::SingleSub21
            && session.subwoofer_search.is_none()
            && session.subwoofer_setup.is_none()
        {
            return Err(
                "save the single-sub search plan before restoring 2.1 measurements".to_string(),
            );
        }
        if session.design.is_some() {
            return Err(
                "measurement cache restore is disabled after a trial filter has been designed"
                    .to_string(),
            );
        }
        if session
            .locked_input_device_id
            .as_deref()
            .is_some_and(|locked| locked != input_device_id)
            || session
                .locked_input_channel_index
                .is_some_and(|locked| locked != input_channel_index)
        {
            return Err(
                "the selected microphone does not match this live project's locked input"
                    .to_string(),
            );
        }

        let base_directory = session
            .root
            .parent()
            .ok_or_else(|| "live project has no cache parent directory".to_string())?;
        let mut project_roots = fs::read_dir(base_directory)
            .map_err(|error| format!("could not scan measurement cache: {error}"))?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .filter(|path| {
                path != &session.root
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("live-"))
            })
            .collect::<Vec<_>>();
        project_roots.sort_by(|left, right| right.file_name().cmp(&left.file_name()));

        let mut scanned_snapshot_count = 0_usize;
        let mut compatible_snapshot_count = 0_usize;
        let mut selected_source_sessions = Vec::new();
        let mut selected =
            BTreeMap::<(LiveCaptureKind, String, LiveChannel), StoredMeasurement>::new();

        for project_root in project_roots {
            let snapshot_directory = project_root.join("snapshots");
            let Ok(entries) = fs::read_dir(&snapshot_directory) else {
                continue;
            };
            let mut snapshot_paths = entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "json")
                })
                .collect::<Vec<_>>();
            snapshot_paths.sort();
            let canonical_project_root = project_root.canonicalize().ok();
            let mut source_measurements = BTreeMap::new();
            for snapshot_path in snapshot_paths {
                scanned_snapshot_count = scanned_snapshot_count.saturating_add(1);
                let Ok(bytes) = fs::read(&snapshot_path) else {
                    continue;
                };
                let Ok(cached) = serde_json::from_slice::<CachedMeasurementSnapshot>(&bytes) else {
                    continue;
                };
                let LiveRestoreScope::General = scope;
                if !cached_snapshot_matches_session(
                    &cached,
                    session,
                    input_device_id,
                    input_channel_index,
                ) {
                    continue;
                }
                let Ok(position_id) =
                    validate_position_id(&cached.position_id, cached.capture_kind)
                else {
                    continue;
                };
                let raw_path = PathBuf::from(&cached.raw_capture_wav);
                let canonical_raw_path = raw_path.canonicalize().ok();
                let raw_is_inside_source = canonical_project_root
                    .as_ref()
                    .zip(canonical_raw_path.as_ref())
                    .is_some_and(|(root, raw)| raw.starts_with(root));
                if !raw_is_inside_source {
                    continue;
                }
                let Some(sweep) = session.sweeps.get(&cached.channel) else {
                    continue;
                };
                let evidence = LiveCaptureEvidence {
                    session_id: session.id.clone(),
                    generation: session.evidence_generation,
                    system_mode: session.system_mode,
                    subwoofer_setup: session.subwoofer_setup.clone(),
                    subwoofer_search: session.subwoofer_search.clone(),
                    calibration_sha256: session
                        .calibration
                        .as_ref()
                        .expect("calibration checked above")
                        .summary
                        .sha256
                        .clone(),
                    sweep_sha256: sweep.summary.sha256.clone(),
                    design_sha256: None,
                    input_device_id: input_device_id.to_string(),
                    input_channel_index,
                };
                let kind = cached.capture_kind;
                let channel = cached.channel;
                compatible_snapshot_count = compatible_snapshot_count.saturating_add(1);
                if let Ok(restored) = restore_cached_snapshot(cached, &snapshot_path, evidence) {
                    source_measurements.insert((kind, position_id, channel), restored);
                }
            }
            let mut source_contributed = false;
            for (key, measurement) in source_measurements {
                if session
                    .measurements
                    .get(&key)
                    .is_some_and(|current| current.summary.accepted)
                    || selected.contains_key(&key)
                {
                    continue;
                }
                selected.insert(key, measurement);
                source_contributed = true;
            }
            if source_contributed {
                if let Some(source_session) = project_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                {
                    selected_source_sessions.push(source_session);
                }
            }
        }

        let mut restored_captures = Vec::new();
        for (key, measurement) in selected {
            if session
                .measurements
                .get(&key)
                .is_some_and(|current| current.summary.accepted)
            {
                continue;
            }
            restored_captures.push(measurement.summary.clone());
            session.measurements.insert(key, measurement);
        }
        if !restored_captures.is_empty() {
            session.locked_input_device_id = Some(input_device_id.to_string());
            session.locked_input_channel_index = Some(input_channel_index);
            if restored_captures.iter().any(|capture| {
                matches!(
                    capture.kind,
                    LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly
                )
            }) {
                session.subwoofer_optimization = None;
            }
            session.design = None;
            advance_evidence_generation(session)?;
        }
        let selected_source_session = selected_source_sessions.first().cloned();
        Ok(LiveMeasurementCacheRestoreSummary {
            algorithm_version: LIVE_ACCEPTED_MEASUREMENT_CACHE_VERSION,
            source_session_id: selected_source_session,
            source_session_ids: selected_source_sessions,
            restored_captures,
            scanned_snapshot_count,
            compatible_snapshot_count,
        })
    }
}

impl LiveMeasurementState {
    fn reserve_capture_paths(
        &self,
        kind: LiveCaptureKind,
        channel: LiveChannel,
        position_id: &str,
        evidence: &LiveCaptureEvidence,
    ) -> Result<(String, PathBuf, PathBuf, PathBuf), String> {
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "live project was closed during capture".to_string())?;
        validate_capture_evidence(session, evidence, kind, channel)?;
        let index = session.next_artifact_index;
        session.next_artifact_index = session
            .next_artifact_index
            .checked_add(1)
            .ok_or_else(|| "live artifact counter overflowed".to_string())?;
        let stem = format!(
            "{index:06}-{}-{}-{}",
            kind.as_str(),
            position_id,
            channel.as_str()
        );
        let rew_path = reserve_rew_export_path(
            &session.root,
            &rew_measurement_label(session.system_mode, kind, channel, position_id),
        )?;
        Ok((
            session.id.clone(),
            session.root.join("captures").join(format!("{stem}.wav")),
            session.root.join("snapshots").join(format!("{stem}.json")),
            rew_path,
        ))
    }

    fn store_measurement(
        &self,
        kind: LiveCaptureKind,
        channel: LiveChannel,
        position_id: String,
        evidence: LiveCaptureEvidence,
        measurement: StoredMeasurement,
    ) -> Result<(), String> {
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "live project was closed during capture".to_string())?;
        validate_capture_evidence(session, &evidence, kind, channel)?;
        let key = (kind, position_id.clone(), channel);
        if !measurement.summary.accepted
            && session
                .measurements
                .get(&key)
                .is_some_and(|stored| stored.summary.accepted)
        {
            // A failed retry remains auditable in its raw WAV and snapshot, but
            // it must not evict the last accepted physical measurement.
            return Ok(());
        }
        if matches!(
            kind,
            LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly
        ) {
            session.measurements.retain(|(stored_kind, _, _), _| {
                matches!(
                    stored_kind,
                    LiveCaptureKind::SubMainOnly | LiveCaptureKind::SubOnly
                )
            });
            session.subwoofer_optimization = None;
            session.subwoofer_setup = None;
            session.design = None;
        }
        session.measurements.insert(key, measurement);
        if matches!(kind, LiveCaptureKind::Baseline) {
            session.design = None;
        }
        advance_evidence_generation(session)?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub fn analyze_and_store_capture(
    state: &LiveMeasurementState,
    kind: LiveCaptureKind,
    channel: LiveChannel,
    position_id: String,
    sweep: &LiveSweepReference,
    calibration: &MicrophoneCalibration,
    evidence: &LiveCaptureEvidence,
    capture: &MonoInputCapture,
) -> Result<LiveCaptureSummary, String> {
    if capture.cancelled {
        return Err("live microphone capture was cancelled".to_string());
    }
    if capture.sample_rate_hz != PROJECT_SAMPLE_RATE_HZ {
        return Err(format!(
            "live capture must be 48000 Hz, got {} Hz",
            capture.sample_rate_hz
        ));
    }
    if capture.device_id != evidence.input_device_id {
        return Err(format!(
            "audio backend opened `{}` but this project is locked to `{}`",
            capture.device_id, evidence.input_device_id
        ));
    }
    if capture.input_channel_index != evidence.input_channel_index {
        return Err(format!(
            "audio backend captured input channel {} but this project is locked to channel {}",
            u32::from(capture.input_channel_index) + 1,
            u32::from(evidence.input_channel_index) + 1,
        ));
    }
    let (session_id, raw_path, snapshot_path, rew_path) =
        state.reserve_capture_paths(kind, channel, &position_id, evidence)?;
    write_raw_capture(&raw_path, capture)?;

    let microphone_samples = capture
        .samples
        .iter()
        .map(|sample| f64::from(*sample))
        .collect::<Vec<_>>();
    let mut deconvolution_config = SweepDeconvolutionConfig {
        minimum_capture_snr_db: LIVE_MINIMUM_CAPTURE_SNR_DB,
        maximum_timing_fit_rms_samples: MAGNITUDE_ONLY_MAXIMUM_TIMING_FIT_RMS_SAMPLES,
        ..SweepDeconvolutionConfig::default()
    };
    let marker_pair = detect_timing_marker_pair(sweep, &microphone_samples)?;
    let start_marker_rms_dbfs = marker_pair.as_ref().and_then(|pair| {
        let source_marker_samples = sweep
            .timing_markers
            .iter()
            .find(|marker| marker.is_start_marker)
            .map(|marker| marker.samples.len())?;
        marker_capture_rms_dbfs(
            &microphone_samples,
            pair.first.capture_start_sample,
            source_marker_samples as f64 * pair.capture_samples_per_reference_sample,
        )
    });
    if marker_pair.is_some() {
        // This correlation belongs to a short acoustic marker, not to the
        // broadband measurement sweep. The repeated pair spacing, clock-drift
        // bound, reconstruction fit, and SNR provide the remaining independent
        // gates for a marker-driven capture.
        deconvolution_config.minimum_start_correlation = MINIMUM_MARKER_CORRELATION;
        deconvolution_config.impulse_pre_zero_samples = MARKER_REFERENCED_IMPULSE_PRE_ZERO_SAMPLES;
        deconvolution_config.enforce_minimum_reconstruction_fit = false;
    }
    let (deconvolution_detection, start_marker_detected, end_marker_detected) = if let Some(pair) =
        marker_pair
    {
        (
            marker_pair_measurement_detection(
                &pair,
                &microphone_samples,
                deconvolution_config.impulse_length_samples,
            )?,
            true,
            true,
        )
    } else {
        let recognition_config = WirelessSweepRecognitionConfig {
            nominal_sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
            maximum_reference_samples: sweep.samples.len().max(1),
            maximum_capture_samples: microphone_samples.len().max(1),
            // Room group delay biases frequency-changing segments. This live path
            // never promotes the fit to channel timing; it retains a wider bound
            // only for magnitude deconvolution and still rejects gross instability.
            maximum_timing_fit_rms_samples: MAGNITUDE_ONLY_MAXIMUM_TIMING_FIT_RMS_SAMPLES,
            // The retained segment must carry the whole deconvolution IR tail
            // even at the worst admitted clock ratio, exactly like the
            // marker-pair branch derives its own required post-roll.
            capture_post_roll_samples: deconvolution_config.impulse_length_samples + 4_800,
            ..WirelessSweepRecognitionConfig::default()
        };
        let recognition =
            recognize_wireless_sweep(&sweep.samples, &microphone_samples, &recognition_config)
                .map_err(|error| format!("live sweep recognition failed: {error}"))?;
        let detection = match recognition {
            WirelessSweepRecognition::Detected(detection) => detection,
            WirelessSweepRecognition::NotDetected(outcome) => {
                return Err(format!(
                        "neither the start/end marker pair nor the measurement sweep was detected: {:?}, strongest sweep correlation {:.3}; raw capture retained at {}",
                        outcome.reason,
                        outcome.strongest_absolute_correlation,
                        raw_path.display()
                    ));
            }
            WirelessSweepRecognition::LikelyFalsePositive(outcome) => {
                return Err(format!(
                        "start/end markers were incomplete and only a likely false-positive sweep fragment was detected: {:?}, timing-fit {:?} samples, drift {:?} ppm; raw capture retained at {}",
                        outcome.reason,
                        outcome.timing_fit_rms_samples,
                        outcome.estimated_clock_drift_ppm,
                        raw_path.display()
                    ));
            }
            WirelessSweepRecognition::Ambiguous(outcome) => {
                return Err(format!(
                        "multiple complete sweep playbacks were detected ({:.3}/{:.3}); play exactly once. Raw capture retained at {}",
                        outcome.strongest_absolute_correlation,
                        outcome.second_absolute_correlation,
                        raw_path.display()
                    ));
            }
        };
        let adjusted = measurement_detection(sweep, &microphone_samples, &detection)?;
        let repeated_markers =
            adjusted.clock_drift_evidence == WirelessClockDriftEvidence::RepeatedTimingMarkers;
        (adjusted, repeated_markers, repeated_markers)
    };
    let octave_band_snr = octave_band_snr_db(
        &microphone_samples,
        deconvolution_detection.estimated_sweep_start_sample,
        deconvolution_detection.estimated_sweep_end_sample_exclusive,
    );
    let level_assessment = assess_measurement_level(
        &microphone_samples,
        &deconvolution_detection,
        calibration.sensitivity_factor_db,
        capture.input_clipped,
    )?;
    let measurement = deconvolve_recognized_sweep(
        &sweep.samples,
        &deconvolution_detection,
        Some(calibration),
        &deconvolution_config,
    )
    .map_err(|error| {
        format!(
            "live sweep deconvolution failed: {error}; raw capture retained at {}",
            raw_path.display()
        )
    })?;
    let mut issue_codes = quality_issue_codes(&measurement, capture);
    let mut extra_diagnostic_codes = Vec::new();
    match isolated_path_leakage_issue(
        kind,
        &position_id,
        evidence.subwoofer_search.as_ref(),
        &measurement.calibrated_frequency_response,
        &microphone_samples,
        deconvolution_detection.estimated_sweep_start_sample,
        deconvolution_detection.estimated_sweep_end_sample_exclusive,
    ) {
        Some(IsolatedLeakageFinding::Leakage(code)) => issue_codes.push(code),
        Some(IsolatedLeakageFinding::NoiseLimited(code)) => extra_diagnostic_codes.push(code),
        None => {}
    }
    if !level_assessment.acceptable_for_measurement {
        issue_codes.push(match level_assessment.status {
            LiveMeasurementLevelStatus::TooLow | LiveMeasurementLevelStatus::Waiting => {
                "measurement_level_too_low".to_string()
            }
            LiveMeasurementLevelStatus::Good => "measurement_level_outside_headroom".to_string(),
            LiveMeasurementLevelStatus::High => "measurement_level_too_high".to_string(),
            LiveMeasurementLevelStatus::Clipping => "measurement_level_clipping".to_string(),
        });
    }
    let accepted = measurement.quality.accepted
        && issue_codes.is_empty()
        && level_assessment.acceptable_for_measurement
        && capture.capture_complete
        && !capture.input_clipped
        && !capture.sample_drop_detected
        && !capture.stream_error_detected;
    let retained_indices = measurement
        .calibrated_frequency_response
        .frequencies_hz
        .iter()
        .enumerate()
        .filter_map(|(index, frequency)| {
            (*frequency > 0.0 && *frequency <= 20_000.0).then_some(index)
        })
        .collect::<Vec<_>>();
    let frequencies_hz = retained_indices
        .iter()
        .map(|index| measurement.calibrated_frequency_response.frequencies_hz[*index])
        .collect::<Vec<_>>();
    let magnitude_db = retained_indices
        .iter()
        .map(|index| measurement.calibrated_frequency_response.magnitude_db[*index])
        .collect::<Vec<_>>();
    let mut summary = LiveCaptureSummary {
        kind,
        channel,
        position_id: position_id.clone(),
        input_device_id: capture.device_id.clone(),
        input_device_name: capture.device_name.clone(),
        input_channel_index: capture.input_channel_index,
        source_channel_count: capture.source_channels,
        accepted,
        issue_codes,
        diagnostic_codes: {
            let mut codes = capture_diagnostic_codes(capture);
            codes.extend(extra_diagnostic_codes);
            codes
        },
        audio_stream_diagnostics: audio_stream_diagnostics(capture),
        capture_peak_dbfs: capture.peak_dbfs.map(f64::from),
        capture_snr_db: measurement.quality.capture_snr_db,
        reconstruction_fit_db: Some(measurement.quality.reconstruction_fit_db),
        reconstruction_fit_required: measurement.quality.reconstruction_fit_required,
        correlation: Some(deconvolution_detection.start_absolute_correlation),
        clock_drift_ppm: Some(deconvolution_detection.estimated_clock_drift_ppm),
        start_marker_detected,
        end_marker_detected,
        start_marker_rms_dbfs,
        octave_band_snr_db: octave_band_snr,
        automatic_completion_detected: capture.automatic_completion_detected,
        level_assessment,
        captured_frames: capture.captured_samples,
        raw_wav_path: raw_path.display().to_string(),
        measurement_snapshot_path: Some(snapshot_path.display().to_string()),
        frequency_bin_count: frequencies_hz.len(),
        timing_eligible: false,
        restored_from_cache: false,
        cache_source_session_id: None,
    };
    let snapshot = MeasurementSnapshot {
        project_version: LIVE_MEASUREMENT_PROJECT_VERSION,
        session_id: &session_id,
        system_mode: evidence.system_mode,
        subwoofer_setup: evidence.subwoofer_setup.as_ref(),
        subwoofer_search: evidence.subwoofer_search.as_ref(),
        capture_kind: kind,
        channel,
        position_id: &position_id,
        input_device_id: &capture.device_id,
        input_device_name: capture.device_name.as_deref(),
        input_channel_index: capture.input_channel_index,
        source_channel_count: capture.source_channels,
        sweep_sha256: &sweep.summary.sha256,
        calibration_sha256: &evidence.calibration_sha256,
        evidence_generation: evidence.generation,
        trial_design_sha256: evidence.design_sha256.as_deref(),
        detector_algorithm: LIVE_CAPTURE_ENDPOINT_VERSION,
        deconvolution_algorithm: KNOWN_SWEEP_DECONVOLUTION_VERSION,
        rew_export_algorithm: LIVE_REW_EXPORT_VERSION,
        sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        raw_capture_wav: raw_path.display().to_string(),
        accepted,
        issue_codes: &summary.issue_codes,
        diagnostic_codes: &summary.diagnostic_codes,
        audio_stream_diagnostics: &summary.audio_stream_diagnostics,
        capture_peak_dbfs: summary.capture_peak_dbfs,
        capture_snr_db: summary.capture_snr_db,
        reconstruction_fit_db: summary.reconstruction_fit_db,
        reconstruction_fit_required: summary.reconstruction_fit_required,
        correlation: summary.correlation,
        clock_drift_ppm: summary.clock_drift_ppm,
        recognized_sweep_start_capture_sample: measurement
            .timing
            .recognized_sweep_start_capture_sample,
        impulse_start_relative_to_recognized_sweep_seconds: measurement
            .timing
            .impulse_start_relative_to_recognized_sweep_seconds,
        marker_channel_analysis_version: sweep.summary.marker_channel_analysis_version,
        start_marker_source_channel: sweep.summary.start_marker_channel,
        end_marker_source_channel: sweep.summary.end_marker_channel,
        start_marker_detected: summary.start_marker_detected,
        end_marker_detected: summary.end_marker_detected,
        start_marker_rms_dbfs: summary.start_marker_rms_dbfs,
        octave_band_snr_db: summary.octave_band_snr_db.as_ref(),
        automatic_completion_detected: summary.automatic_completion_detected,
        level_assessment: &summary.level_assessment,
        timing_eligible: false,
        frequency_hz: &frequencies_hz,
        calibrated_magnitude_db: &magnitude_db,
        calibrated_impulse_samples: &measurement.calibrated_impulse_samples,
    };
    let snapshot_bytes = serde_json::to_vec_pretty(&snapshot)
        .map_err(|error| format!("could not serialize measurement snapshot: {error}"))?;
    write_new_file(&snapshot_path, &snapshot_bytes)?;
    if accepted {
        // A convenience copy for REW. Failing to write it must not discard a
        // physically valid measurement, so the failure is reported as a
        // diagnostic on the capture instead of an error.
        if let Err(error) =
            write_rew_impulse_wav(&rew_path, &measurement.calibrated_impulse_samples)
        {
            summary
                .diagnostic_codes
                .push(format!("rew_export_failed:{error}"));
        }
    }

    state.store_measurement(
        kind,
        channel,
        position_id,
        evidence.clone(),
        StoredMeasurement {
            summary: summary.clone(),
            calibrated_frequency_response: measurement.calibrated_frequency_response,
            calibrated_impulse_samples: measurement.calibrated_impulse_samples,
            recognized_sweep_start_capture_sample: measurement
                .timing
                .recognized_sweep_start_capture_sample,
            frequencies_hz,
            magnitude_db,
            evidence: evidence.clone(),
        },
    )?;
    Ok(summary)
}

fn target_from_name(
    target: &str,
    custom_target: Option<&TargetEntry>,
) -> Result<TargetCurve, String> {
    match target {
        "bk" => Ok(TargetCurve::preset(TargetPreset::BkStyle)),
        "harman" => Ok(TargetCurve::preset(TargetPreset::HarmanStyle)),
        "custom" => custom_target
            .map(|entry| entry.curve.clone())
            .ok_or_else(|| {
                "import a valid custom target TXT before selecting the custom target".to_string()
            }),
        other => Err(format!(
            "unsupported live target `{other}`; choose `bk`, `harman`, or `custom`"
        )),
    }
}

fn position_sort_key(position_id: &str) -> (u8, &str) {
    match position_id {
        "P0" => (0, position_id),
        "P1" => (1, position_id),
        "P2" => (2, position_id),
        "P3" => (3, position_id),
        "P4" => (4, position_id),
        "P5" => (5, position_id),
        "P0_END" => (6, position_id),
        _ => (7, position_id),
    }
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

fn validate_p0_repeat_response(
    frequencies_hz: &[f64],
    initial_db: &[f64],
    repeated_db: &[f64],
    channel: LiveChannel,
) -> Result<(), String> {
    if frequencies_hz.len() != initial_db.len()
        || initial_db.len() != repeated_db.len()
        || frequencies_hz.is_empty()
    {
        return Err("P0 repeat response grids are inconsistent".to_string());
    }
    let differences = frequencies_hz
        .iter()
        .zip(initial_db)
        .zip(repeated_db)
        .filter_map(|((frequency, initial), repeated)| {
            (*frequency >= 20.0 && *frequency <= 500.0).then_some(repeated - initial)
        })
        .collect::<Vec<_>>();
    let mut reference_band = frequencies_hz
        .iter()
        .zip(initial_db)
        .zip(repeated_db)
        .filter_map(|((frequency, initial), repeated)| {
            (*frequency >= 200.0 && *frequency <= 500.0).then_some(repeated - initial)
        })
        .collect::<Vec<_>>();
    let level_shift_db = median(&mut reference_band)
        .ok_or_else(|| "P0 repeat has no finite 200-500 Hz reference bins".to_string())?;
    if differences.is_empty() || differences.iter().any(|value| !value.is_finite()) {
        return Err("P0 repeat has no finite 20-500 Hz comparison bins".to_string());
    }
    let shape_rmse_db = (differences
        .iter()
        .map(|difference| (difference - level_shift_db).powi(2))
        .sum::<f64>()
        / differences.len() as f64)
        .sqrt();
    if level_shift_db.abs() > MAXIMUM_P0_REPEAT_LEVEL_SHIFT_DB
        || shape_rmse_db > MAXIMUM_P0_REPEAT_SHAPE_RMSE_DB
    {
        return Err(format!(
            "{} P0 end-repeat is unstable: level shift {level_shift_db:.2} dB \
             (limit {:.2}), response-shape RMSE {shape_rmse_db:.2} dB (limit {:.2}). \
             Keep volume/gain fixed, return the microphone approximately to the listening-center \
             area, and repeat both channels.",
            channel.as_str(),
            MAXIMUM_P0_REPEAT_LEVEL_SHIFT_DB,
            MAXIMUM_P0_REPEAT_SHAPE_RMSE_DB,
        ));
    }
    Ok(())
}

fn timed_impulse(measurement: &StoredMeasurement) -> TimedCombinedImpulse {
    TimedCombinedImpulse {
        samples: measurement.calibrated_impulse_samples.clone(),
        start_time_seconds: measurement.recognized_sweep_start_capture_sample
            / f64::from(PROJECT_SAMPLE_RATE_HZ),
        arrival_time_seconds: None,
    }
}

fn build_response_set(session: &LiveSession) -> Result<MeasuredStereoResponseSet, String> {
    let mut ids = session
        .measurements
        .keys()
        .filter(|(kind, _, _)| *kind == LiveCaptureKind::Baseline)
        .map(|(_, id, _)| id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| position_sort_key(left).cmp(&position_sort_key(right)));
    let has_end_repeat = ids.iter().any(|id| id == "P0_END");
    if has_end_repeat {
        for channel in [LiveChannel::Left, LiveChannel::Right] {
            let initial =
                session
                    .measurements
                    .get(&(LiveCaptureKind::Baseline, "P0".to_string(), channel));
            let repeated = session.measurements.get(&(
                LiveCaptureKind::Baseline,
                "P0_END".to_string(),
                channel,
            ));
            if let (Some(initial), Some(repeated)) = (initial, repeated) {
                if initial.summary.accepted && repeated.summary.accepted {
                    if initial.frequencies_hz != repeated.frequencies_hz {
                        return Err(format!(
                            "{} P0 end-repeat uses a different response grid",
                            channel.as_str()
                        ));
                    }
                    validate_p0_repeat_response(
                        &initial.frequencies_hz,
                        &initial.magnitude_db,
                        &repeated.magnitude_db,
                        channel,
                    )?;
                }
            }
        }
    }
    let mut positions = Vec::new();
    let mut frequencies = None;
    for id in ids {
        let left =
            session
                .measurements
                .get(&(LiveCaptureKind::Baseline, id.clone(), LiveChannel::Left));
        let right =
            session
                .measurements
                .get(&(LiveCaptureKind::Baseline, id.clone(), LiveChannel::Right));
        let (Some(left), Some(right)) = (left, right) else {
            continue;
        };
        if !left.summary.accepted || !right.summary.accepted {
            continue;
        }
        validate_stored_evidence(session, left)?;
        validate_stored_evidence(session, right)?;
        if left.frequencies_hz != right.frequencies_hz {
            return Err(format!("left/right response grids differ at position {id}"));
        }
        if let Some(expected) = &frequencies {
            if expected != &left.frequencies_hz {
                return Err(format!("response grid differs at position {id}"));
            }
        } else {
            frequencies = Some(left.frequencies_hz.clone());
        }
        let weight = match id.as_str() {
            "P0" | "P0_END" if has_end_repeat => 1.0,
            "P0" => 2.0,
            _ => 1.0,
        };
        positions.push(MeasuredStereoPosition {
            id,
            weight,
            left_magnitude_db: left.magnitude_db.clone(),
            right_magnitude_db: right.magnitude_db.clone(),
            left_timed_combined_ir: Some(timed_impulse(left)),
            right_timed_combined_ir: Some(timed_impulse(right)),
        });
    }
    if !positions.iter().any(|position| position.id == "P0") {
        return Err(
            "an accepted baseline L/R pair at P0 is required before filter design".to_string(),
        );
    }
    Ok(MeasuredStereoResponseSet {
        sample_rate_hz: PROJECT_SAMPLE_RATE_HZ,
        frequencies_hz: frequencies
            .ok_or_else(|| "no accepted paired baseline measurements are available".to_string())?,
        positions,
    })
}

impl LiveMeasurementState {
    /// Design the predicted minimum-phase trial filter.
    pub fn design_trial(&self, target: &str) -> Result<LiveDesignSummary, String> {
        let active = self
            .active_capture
            .lock()
            .map_err(|_| "live capture state lock was poisoned".to_string())?;
        if active.is_some() {
            return Err("finish the active microphone capture before filter design".to_string());
        }
        let (
            target_curve,
            selected_custom_target,
            response_set,
            baseline_octave_snrs,
            root,
            artifact_index,
            session_id,
            evidence_generation,
            system_mode,
            subwoofer_setup,
        ) = {
            let mut guard = self
                .session
                .lock()
                .map_err(|_| "live session state lock was poisoned".to_string())?;
            let session = guard
                .as_mut()
                .ok_or_else(|| "start a live project before filter design".to_string())?;
            let target_curve = target_from_name(target, session.custom_target.as_ref())?;
            let selected_custom_target = (target == "custom").then(|| {
                session
                    .custom_target
                    .as_ref()
                    .expect("custom target resolved above")
                    .summary
                    .clone()
            });
            if session.calibration.is_none()
                || ![LiveChannel::Left, LiveChannel::Right]
                    .iter()
                    .all(|channel| session.sweeps.contains_key(channel))
            {
                return Err(
                    "calibration and both measurement sweeps are required before design"
                        .to_string(),
                );
            }
            let response_set = build_response_set(session)?;
            let baseline_octave_snrs: Vec<Option<Vec<Option<f64>>>> = session
                .measurements
                .iter()
                .filter(|((kind, _, _), measurement)| {
                    *kind == LiveCaptureKind::Baseline && measurement.summary.accepted
                })
                .map(|(_, measurement)| measurement.summary.octave_band_snr_db.clone())
                .collect();
            let index = session.next_artifact_index;
            session.next_artifact_index = session
                .next_artifact_index
                .checked_add(1)
                .ok_or_else(|| "live artifact counter overflowed".to_string())?;
            (
                target_curve,
                selected_custom_target,
                response_set,
                baseline_octave_snrs,
                session.root.clone(),
                index,
                session.id.clone(),
                session.evidence_generation,
                session.system_mode,
                session.subwoofer_setup.clone(),
            )
        };
        // In a 2.1 project the shared-low-bass region tracks the confirmed
        // crossover instead of a fixed 100 Hz (2026-07-29 expert review,
        // finding 6): Roon convolution sits upstream of bass management, so
        // the sub feed is the filtered L+R sum, and below the crossover an
        // L!=R correction would make the sub response depend on the program's
        // channel correlation. The common band therefore extends to
        // max(100 Hz, XO) with the blend finishing at 1.4x that frequency
        // (the stereo default stays 100 -> 140 Hz).
        let stereo_blend = match subwoofer_setup
            .as_ref()
            .filter(|setup| setup.confirmed_on_hardware)
        {
            Some(setup) => {
                let common_below_hz = setup.crossover_hz.max(100.0);
                StereoBlendSettings {
                    common_below_hz,
                    channel_specific_above_hz: common_below_hz * 1.4,
                }
            }
            None => StereoBlendSettings::default(),
        };
        // Boost SNR requirement (finding 10): a band may only receive boost
        // when every accepted baseline capture resolves the sweep at least
        // LIVE_MINIMUM_BOOST_BAND_SNR_DB above its own noise floor there.
        // Captures from caches that predate the per-octave diagnostic count
        // as insufficient evidence, which only withholds boost - never cuts.
        let mut boost_disallowed_bands = Vec::new();
        for (band_index, &(low_hz, high_hz)) in OCTAVE_SNR_BANDS_HZ.iter().enumerate() {
            let mut worst: Option<f64> = None;
            let mut missing = baseline_octave_snrs.is_empty();
            for capture_bands in &baseline_octave_snrs {
                match capture_bands
                    .as_ref()
                    .and_then(|bands| bands.get(band_index).copied().flatten())
                {
                    Some(snr) => worst = Some(worst.map_or(snr, |value: f64| value.min(snr))),
                    None => missing = true,
                }
            }
            if missing || worst.is_none_or(|value| value < LIVE_MINIMUM_BOOST_BAND_SNR_DB) {
                boost_disallowed_bands.push((low_hz, high_hz));
            }
        }
        let config = Phase4OfflineConfig {
            target: target_curve,
            stereo_blend,
            correction: eqforbeginner_dsp_core::correction::CorrectionSettings {
                boost_disallowed_bands,
                ..eqforbeginner_dsp_core::correction::CorrectionSettings::default()
            },
            // The live trial must be the exact physical 48 kHz member of the
            // native-rate duration family so the filter that is remeasured can
            // be response-bound to the final package.
            fir_fft_size: native_fft_size(PROJECT_SAMPLE_RATE_HZ)
                .ok_or_else(|| "Phase 6 has no 48 kHz native grid".to_string())?,
            ..Phase4OfflineConfig::default()
        };
        let result = run_phase4_offline(&response_set, &config)
            .map_err(|error| format!("existing Phase 4 design failed: {error}"))?;
        if !result.numerical_passed {
            return Err(
                "Phase 4 numerical prediction did not pass; trial convolution was not emitted"
                    .to_string(),
            );
        }
        let full_unaligned_target_db = config
            .target
            .evaluate(&response_set.frequencies_hz)
            .map_err(|error| format!("could not evaluate the full-band result target: {error}"))?;
        let full_left_aligned_target_db = full_unaligned_target_db
            .iter()
            .map(|value| value + result.left_design.target_alignment_db)
            .collect::<Vec<_>>();
        let full_right_aligned_target_db = full_unaligned_target_db
            .iter()
            .map(|value| value + result.right_design.target_alignment_db)
            .collect::<Vec<_>>();
        let trial_wav_path = root
            .join("trial")
            .join(format!("trial-{artifact_index:06}-48000-stereo.wav"));
        let trial_zip_path = root
            .join("trial")
            .join(format!("trial-{artifact_index:06}-roon.zip"));
        let trial_fir = StereoFir {
            sample_rate: PROJECT_SAMPLE_RATE_HZ,
            left: result.left_fir.taps.iter().map(|tap| *tap as f32).collect(),
            right: result
                .right_fir
                .taps
                .iter()
                .map(|tap| *tap as f32)
                .collect(),
        };
        write_stereo_wav(&trial_wav_path, &trial_fir)
            .map_err(|error| format!("could not write 48 kHz trial convolution: {error}"))?;
        let trial_wav_sha256 = sha256_hex(
            &fs::read(&trial_wav_path)
                .map_err(|error| format!("could not hash 48 kHz trial convolution: {error}"))?,
        );
        let readme = format!(
            "EQforBeginner developer live trial\n\
             State: predicted-only; not a final package\n\
             Load this ZIP in one Roon convolution slot, keep volume and microphone gain fixed,\n\
             then perform the required P0 L/R verification measurements.\n\
             Target: {} ({})\n\
             Correction: minimum phase, 20-500 Hz, unity taper through 650 Hz;\n\
             broad spatially repeated shallow dips may receive at most +3 dB,\n\
             while deep/narrow dips remain protected.\n\
             Algorithm: {PHASE4_OFFLINE_ALGORITHM_VERSION}\n",
            config.target.name(),
            config.target.version(),
        );
        create_roon_zip(
            &trial_zip_path,
            std::slice::from_ref(&trial_wav_path),
            PHASE4_OFFLINE_ALGORITHM_VERSION,
            &readme,
        )
        .map_err(|error| format!("could not create trial Roon ZIP: {error}"))?;
        let maximum_attenuation_db = result
            .left_validation
            .metrics
            .maximum_correction_attenuation_db
            .max(
                result
                    .right_validation
                    .metrics
                    .maximum_correction_attenuation_db,
            );
        let maximum_boost_db = result
            .left_validation
            .metrics
            .maximum_correction_gain_db
            .max(result.right_validation.metrics.maximum_correction_gain_db)
            .max(0.0);
        let protected_dips_passed = result.protected_dip_validation.left_passed
            && result.protected_dip_validation.right_passed;
        let summary = LiveDesignSummary {
            algorithm_version: PHASE4_OFFLINE_ALGORITHM_VERSION.to_string(),
            numerical_passed: result.numerical_passed,
            position_count: response_set.positions.len(),
            trial_wav_path: trial_wav_path.display().to_string(),
            trial_zip_path: trial_zip_path.display().to_string(),
            left_raw_rmse_db: result.left_validation.metrics.raw_rmse_db,
            left_predicted_rmse_db: result.left_validation.metrics.predicted_rmse_db,
            right_raw_rmse_db: result.right_validation.metrics.raw_rmse_db,
            right_predicted_rmse_db: result.right_validation.metrics.predicted_rmse_db,
            maximum_attenuation_db,
            maximum_boost_db,
            protected_dips_passed,
            warning: "Predicted-only trial. Final export stays locked until a new P0 L/R capture is made with this exact filter active in Roon.".to_string(),
        };
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "live session state lock was poisoned".to_string())?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "live project was closed during design".to_string())?;
        if session.id != session_id
            || session.evidence_generation != evidence_generation
            || session.system_mode != system_mode
            || session.subwoofer_setup != subwoofer_setup
            || (target == "custom"
                && session
                    .custom_target
                    .as_ref()
                    .map(|entry| entry.summary.clone())
                    != selected_custom_target)
        {
            return Err(
                "live measurement evidence changed during filter design; discard the stale trial and design again"
                    .to_string(),
            );
        }
        session
            .measurements
            .retain(|(kind, _, _), _| *kind != LiveCaptureKind::Verification);
        session.design = Some(LiveDesign {
            summary: summary.clone(),
            target_name: config.target.name().to_string(),
            target_version: config.target.version().to_string(),
            custom_target: selected_custom_target,
            response_set,
            result,
            full_left_aligned_target_db,
            full_right_aligned_target_db,
            evidence_sha256: trial_wav_sha256,
            user_declared_active_at_unix_ms: None,
        });
        advance_evidence_generation(session)?;
        Ok(summary)
    }
}
