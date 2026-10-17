//! SECS bug-fix edition of the [`crate::secs`] port.
//!
//! Original author: 한플 (Hanpeul). Original work: SECS, published on the
//! DCInside speaker gallery:
//! <https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1>
//!
//! Copyright (c) 한플 (Hanpeul) for the original algorithm, licensed by the
//! author under the MIT License, with a request that the original be credited
//! with a link. See `THIRD-PARTY-NOTICES.md` for the full notice and for which
//! parts of the SECS port are the original design versus this project's
//! additions (multi-position averaging, the house-curve overlay, and the 2.1
//! shared-sub-band commonization are ours, not the original author's).
//!
//! This edition fixes the defects listed in the developer-local bug report
//! `debugfiles/secs_bugfix_prompt.txt` (written against SECS.py). `secs.rs`
//! stays the untouched original port, pinned to SECS.py by the parity
//! fixture; this module forks its private pipeline and changes nothing but
//! the fixes below. Every fix sits behind an internal switch, and a unit test
//! proves that with every switch off this edition reproduces
//! [`crate::secs::design_secs_stereo_filter`] bit for bit.
//!
//! - [1] mandatory: the 5-band LR4 smoothing weights are normalized to sum to
//!   one (they summed to up to 1.257 between 100 and 300 Hz, which cut a
//!   -2 dB dip at ~174 Hz into a perfectly flat speaker).
//! - [2] mandatory: the macro EQ only cuts (it could add up to +6 dB on top of
//!   the boost ceiling, filling reflection nulls).
//! - [3] option (default on, 500 Hz): the level anchor moves to the band
//!   just above the correction upper limit, and above it the macro EQ either
//!   fades out or (default, `hf_target_follow`) keeps following the target
//!   cut-only, so a house curve's HF rolloff still applies while the
//!   reverberant treble can never be lifted again.
//! - [4] option (default on): no magnitude-only delay search; the design runs
//!   at the caller's delay budget, which [`secs_edited_auto_delay_ceiling_ms`]
//!   resolves with taper headroom above the measured low-band requirement.
//! - [5] mandatory: the stereo crop starts from the earliest main tap, so the
//!   later-arriving channel (the one the arrival alignment leaves unshifted)
//!   keeps its main tap.
//! - [6] no core change: the app's multi-position path already feeds a
//!   seat-weighted power average as `magnitude_overrides` and keeps the
//!   excess phase on the reference seat's impulse.
//! - [7] no change here: microphone calibration is divided out of the impulse
//!   upstream (`measurement.rs`) before SECS sees it.
//! - [8] option (default 0.3): the peak-crush exponent is its own setting
//!   instead of being tied to the resolution mode.
//! - [9] [`secs_decay_score`] is the honest name of the "phase score".
//!
//! Scope and honesty boundaries: this is predicted design only. Nothing here
//! claims measured verification; the bug-fix tests judge synthetic models.

use crate::error::{DspError, DspResult};
use crate::secs::{
    secs_linearity_score, secs_required_bulk_advance_ms, SecsAutoDelayCeiling, SecsChannelSummary,
    SecsConfig, SecsExcessPhaseExport, SecsResolutionMode, SecsStereoDesign,
    SECS_AUTO_CEILING_HEADROOM_MS, SECS_AUTO_CEILING_STEP_MS, SECS_AUTO_DELAY_COARSE_STEP_MS,
    SECS_AUTO_DELAY_MAX_MS, SECS_AUTO_DELAY_MIN_MS, SECS_AUTO_DELAY_WEIGHT_GAIN,
    SECS_CHANNEL_BALANCE_BAND_HIGH_HZ, SECS_CHANNEL_BALANCE_BAND_LOW_HZ,
    SECS_CHANNEL_BALANCE_MAX_TRIM_DB, SECS_GROUP_DELAY_GATE_BANDS, SECS_MAXIMUM_DELAY_CEILING_MS,
    SECS_PHASE_GUARD_BAND_BUDGET_RATIO, SECS_PHASE_GUARD_TOLERANCE_MS,
    SECS_PHASE_PRERING_HIGH_MAX_MS, SECS_PHASE_PRERING_LOW_MAX_MS, SECS_PHASE_PRERING_LOW_MIN_MS,
    SECS_PHASE_PRERING_MID_MAX_MS, SECS_TARGET_OVERLAY_ANCHOR_HZ,
};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

type C64 = Complex<f64>;

/// Algorithm identifier recorded on artifacts designed by this edition.
pub const SECS_EDITED_VERSION: &str = "secs-bugfix-v1";
/// Default correction upper limit of fix [3], Hz.
pub const SECS_EDITED_CORRECTION_UPPER_DEFAULT_HZ: f64 = 500.0;
/// Lowest accepted correction upper limit, Hz.
pub const SECS_EDITED_CORRECTION_UPPER_MIN_HZ: f64 = 200.0;
/// At or above this the [3] limit is off: legacy full-band macro EQ AND the
/// legacy 35th-percentile level anchor, exactly (the bug report: "20000이면
/// 기존 동작").
pub const SECS_EDITED_CORRECTION_UPPER_OFF_HZ: f64 = 20_000.0;
/// Default peak-crush exponent of fix [8] (SECS.py's Gentle value).
pub const SECS_EDITED_CRUSH_INTENSITY_DEFAULT: f64 = 0.3;

/// How the edition picks the target delay in normal latency mode (fix [4]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecsEditedDelayChoice {
    /// Fix [4] (default): no magnitude-only search. In normal latency mode
    /// with no explicit candidates the design runs at
    /// `config.maximum_delay_ms`; the caller resolves that budget with
    /// [`secs_edited_auto_delay_ceiling_ms`], which keeps taper headroom above
    /// the measured low-band requirement.
    ///
    /// Why: the delay decides how much low-band excess phase the corrector
    /// can undo, but SECS.py scored candidates on L+R magnitude alone, so it
    /// picked 8-9 ms on an anechoic bass-managed model and left +5.8 ms of
    /// 40 Hz excess group delay. Simply fixing the original full-extent
    /// cosine pre-ring window at 10 ms is no cure either: it was measured to
    /// cost -7.7 dB at 25 Hz in the realized filter (the truncated corrector
    /// spreads anti-causal energy past the budget and the crop drops it).
    /// The extended machinery - flat-left low-band window plus bulk split -
    /// with a budget at least the requirement plus the 10 ms taper was
    /// measured clean on the same model (LF within 0.1 dB, 40 Hz excess
    /// +0.34 ms). That machinery only engages with `phase_guard == true` and
    /// `maximum_delay_ms > 10`, so callers should run this edition with the
    /// phase guard on.
    Ceiling,
    /// SECS.py behavior: the coarse 2..=10 ms magnitude-only search.
    MagnitudeSearch,
}

/// User-facing options of the bug-fix edition. Fixes [1], [2] and [5] are
/// mandatory and have no switch here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SecsEditedOptions {
    /// Fix [3]: the macro EQ fades to unity between this frequency and twice
    /// it, and the level anchor is the 1/3-octave-smoothed RMS between this
    /// frequency and four times it. At or above
    /// [`SECS_EDITED_CORRECTION_UPPER_OFF_HZ`] it is disabled exactly.
    pub correction_upper_hz: f64,
    /// Fix [4].
    pub delay_choice: SecsEditedDelayChoice,
    /// Fix [8]: peak-crush exponent in `[0, 1]` (SECS.py tied it to the
    /// resolution mode: 0.3/0.6/1.0). 0 disables the crush.
    pub crush_intensity: f64,
    /// Fix [3] companion (default on): above the correction upper limit the
    /// macro EQ keeps following the target, but - being cut-only after fix
    /// [2] - it can only bring treble that sits ABOVE the target down to it,
    /// so a house curve's high-frequency rolloff still shapes the result.
    /// The defect [3] fixed was the macro EQ *lifting* a reverberant treble
    /// that falls below the target, and a cut-only EQ cannot lift. `false`
    /// fades the macro EQ to unity between U and 2U (treble untouched).
    /// Ignored when the upper limit is off (the macro EQ is full band then).
    pub hf_target_follow: bool,
}

impl Default for SecsEditedOptions {
    fn default() -> Self {
        Self {
            correction_upper_hz: SECS_EDITED_CORRECTION_UPPER_DEFAULT_HZ,
            delay_choice: SecsEditedDelayChoice::Ceiling,
            crush_intensity: SECS_EDITED_CRUSH_INTENSITY_DEFAULT,
            hf_target_follow: true,
        }
    }
}

/// Internal fix switches. All off reproduces `secs.rs` bit for bit (pinned by
/// a unit test); the public entry point always turns [1], [2] and [5] on.
#[derive(Debug, Clone, Copy)]
struct EditedFixes {
    /// Fix [1].
    normalized_weights: bool,
    /// Fix [2].
    cut_only_macro: bool,
    /// Fix [3]; `None` = legacy full-band macro EQ and percentile anchor.
    correction_upper_hz: Option<f64>,
    /// Fix [4].
    delay_choice: SecsEditedDelayChoice,
    /// Fix [8]; `None` = legacy per-resolution exponent.
    crush_intensity: Option<f64>,
    /// Fix [5].
    earliest_tap_crop: bool,
    /// Fix [3] companion: keep the cut-only macro EQ active above the upper
    /// limit instead of fading it out. Only honoured with `cut_only_macro`.
    hf_target_follow: bool,
}

impl EditedFixes {
    fn from_options(options: &SecsEditedOptions) -> Self {
        Self {
            normalized_weights: true,
            cut_only_macro: true,
            correction_upper_hz: (options.correction_upper_hz
                < SECS_EDITED_CORRECTION_UPPER_OFF_HZ)
                .then_some(options.correction_upper_hz),
            delay_choice: options.delay_choice,
            crush_intensity: Some(options.crush_intensity),
            earliest_tap_crop: true,
            hf_target_follow: options.hf_target_follow,
        }
    }
}

/// Bug-fixed stereo SECS design. Same inputs and validation as
/// [`crate::secs::design_secs_stereo_filter`] plus `options`; see the module
/// documentation for what each fix changes. Predicted design only.
pub fn design_secs_stereo_filter_edited(
    left_ir: &[f64],
    right_ir: &[f64],
    config: &SecsConfig,
    options: &SecsEditedOptions,
    delay_candidates_ms: Option<&[f64]>,
    magnitude_overrides: Option<(&[f64], &[f64])>,
) -> DspResult<SecsStereoDesign> {
    if !options.correction_upper_hz.is_finite()
        || options.correction_upper_hz < SECS_EDITED_CORRECTION_UPPER_MIN_HZ
    {
        return Err(DspError::InvalidArgument(format!(
            "SECS correction upper limit must be finite and at least {SECS_EDITED_CORRECTION_UPPER_MIN_HZ} Hz"
        )));
    }
    if !options.crush_intensity.is_finite() || !(0.0..=1.0).contains(&options.crush_intensity) {
        return Err(DspError::InvalidArgument(
            "SECS peak-crush intensity must be between 0 and 1".to_string(),
        ));
    }
    design_with_fixes(
        left_ir,
        right_ir,
        config,
        &EditedFixes::from_options(options),
        delay_candidates_ms,
        magnitude_overrides,
    )
}

/// Fix [4] budget for a stereo pair: requirement = the larger channel's
/// [`secs_required_bulk_advance_ms`]; ceiling = 10 ms when the requirement is
/// zero, else the requirement plus [`SECS_AUTO_CEILING_HEADROOM_MS`] rounded
/// up to the [`SECS_AUTO_CEILING_STEP_MS`] grid, clamped to
/// 10..=[`SECS_MAXIMUM_DELAY_CEILING_MS`] ms.
///
/// The original [`crate::secs::secs_auto_delay_ceiling_ms`] returns exactly
/// 10 ms for any requirement up to 10 ms - the measured pathological budget:
/// content advanced by the requirement then sits inside the cosine taper of
/// the low-band window instead of its flat part (an anechoic bass-managed
/// model needing 8 ms lost 7.7 dB at 25 Hz at 10 ms and was clean at 20 ms).
/// Headroom is therefore added whenever the room asks for any advance.
pub fn secs_edited_auto_delay_ceiling_ms(
    left_ir: &[f64],
    right_ir: &[f64],
    sample_rate_hz: u32,
) -> DspResult<SecsAutoDelayCeiling> {
    let left = secs_required_bulk_advance_ms(left_ir, sample_rate_hz)?;
    let right = secs_required_bulk_advance_ms(right_ir, sample_rate_hz)?;
    let requirement_ms = left.max(right);
    if requirement_ms <= 0.0 {
        return Ok(SecsAutoDelayCeiling {
            ceiling_ms: SECS_AUTO_DELAY_MAX_MS,
            requirement_ms,
        });
    }
    let stepped = ((requirement_ms + SECS_AUTO_CEILING_HEADROOM_MS) / SECS_AUTO_CEILING_STEP_MS)
        .ceil()
        * SECS_AUTO_CEILING_STEP_MS;
    Ok(SecsAutoDelayCeiling {
        ceiling_ms: stepped.clamp(SECS_AUTO_DELAY_MAX_MS, SECS_MAXIMUM_DELAY_CEILING_MS),
        requirement_ms,
    })
}

/// Fix [9]: the SECS "phase score" is the 1-10 kHz late/early decay of the
/// impulse (how much the 100-200 ms window has decayed against the first
/// 100 ms), not a phase-linearity measure. Same number as
/// [`secs_linearity_score`], under an honest name.
pub fn secs_decay_score(signal: &[f64], sample_rate_hz: u32) -> f64 {
    secs_linearity_score(signal, sample_rate_hz)
}

/// `SecsResolutionMode::smoothing_fractions` (private in `secs.rs`).
fn smoothing_fractions(mode: SecsResolutionMode) -> [f64; 5] {
    match mode {
        SecsResolutionMode::Gentle => [12.0, 6.0, 3.0, 2.0, 1.0],
        SecsResolutionMode::Balanced => [24.0, 12.0, 6.0, 3.0, 1.0],
        SecsResolutionMode::Precise => [96.0, 48.0, 24.0, 12.0, 3.0],
    }
}

/// `SecsResolutionMode::sharp_fraction` (private in `secs.rs`).
fn sharp_fraction(mode: SecsResolutionMode) -> f64 {
    match mode {
        SecsResolutionMode::Gentle => 24.0,
        SecsResolutionMode::Balanced => 48.0,
        SecsResolutionMode::Precise => 96.0,
    }
}

/// `SecsResolutionMode::crush_intensity` (private in `secs.rs`): the legacy
/// exponent the `None` crush switch reproduces.
fn legacy_crush_intensity(mode: SecsResolutionMode) -> f64 {
    match mode {
        SecsResolutionMode::Gentle => 0.3,
        SecsResolutionMode::Balanced => 0.6,
        SecsResolutionMode::Precise => 1.0,
    }
}

// ---------------------------------------------------------------------------
// Copied unchanged from secs.rs (its private helpers cannot be reached
// without editing it, and secs.rs must stay the untouched original).
// ---------------------------------------------------------------------------

/// One channel's reusable analysis, mirror of `precompute_secs_channel`.
struct SecsChannelPrecompute {
    n: usize,
    sample_rate_hz: f64,
    freqs: Vec<f64>,
    f_abs: Vec<f64>,
    weights: [Vec<f64>; 5],
    mag_orig: Vec<f64>,
    excess_inv_ir: Vec<f64>,
    min_inv_ir: Vec<f64>,
    target_mag_base: Vec<f64>,
    ref_mag: f64,
    track_weight: Vec<f64>,
    low_cutoff_hz: f64,
    high_cutoff_hz: f64,
}

fn fft_in_place(buffer: &mut [C64]) {
    FftPlanner::<f64>::new()
        .plan_fft_forward(buffer.len())
        .process(buffer);
}

fn ifft_in_place(buffer: &mut [C64]) {
    let scale = 1.0 / buffer.len() as f64;
    FftPlanner::<f64>::new()
        .plan_fft_inverse(buffer.len())
        .process(buffer);
    for value in buffer.iter_mut() {
        *value *= scale;
    }
}

fn fft_real(samples: &[f64], fft_size: usize) -> Vec<C64> {
    let mut buffer = vec![C64::new(0.0, 0.0); fft_size];
    for (slot, sample) in buffer.iter_mut().zip(samples.iter()) {
        slot.re = *sample;
    }
    fft_in_place(&mut buffer);
    buffer
}

/// `scipy.fft.fftfreq(n, 1/sr)` in Hz.
fn fft_frequencies(n: usize, sample_rate_hz: f64) -> Vec<f64> {
    let half = n.div_ceil(2);
    (0..n)
        .map(|index| {
            let signed = if index < half {
                index as isize
            } else {
                index as isize - n as isize
            };
            signed as f64 * sample_rate_hz / n as f64
        })
        .collect()
}

fn absolute_frequencies(freqs: &[f64]) -> Vec<f64> {
    freqs.iter().map(|f| f.abs() + 1e-12).collect()
}

/// `get_centered_distance`: signed FFT-ordered distance from bin 0.
fn centered_distance(n: usize) -> Vec<isize> {
    let half = (n / 2) as isize;
    (0..n as isize)
        .map(|i| (i + half).rem_euclid(n as isize) - half)
        .collect()
}

/// `get_sample_shift_phase`: exp(+j 2π f shift / sr).
fn sample_shift_phase(freqs: &[f64], sample_rate_hz: f64, shift_samples: f64) -> Vec<C64> {
    freqs
        .iter()
        .map(|f| {
            C64::from_polar(
                1.0,
                2.0 * std::f64::consts::PI * f * shift_samples / sample_rate_hz,
            )
        })
        .collect()
}

/// `get_asymmetric_window`: raised-cosine tapers of `left_ms` before t=0 and
/// `right_ms` after (`f64::INFINITY` keeps the entire right side).
/// Flat-topped variant for the extended-delay low band: unity out to
/// `left_ms - taper_ms`, cosine only over the last `taper_ms`. The original
/// window tapers across its whole left extent, which erased a restored
/// 51 ms bulk advance sitting at 85% of a 60 ms window (gain 0.056) - the
/// advanced low-band energy IS the correction and must survive its window.
/// Only the extended improved path uses this; every original path keeps the
/// SECS.py window bit for bit.
fn asymmetric_window_flat_left(
    n: usize,
    sample_rate_hz: f64,
    left_ms: f64,
    taper_ms: f64,
) -> Vec<f64> {
    let mut window = vec![0.0; n];
    let left_samples = (left_ms * sample_rate_hz / 1000.0) as isize;
    let taper_samples = ((taper_ms * sample_rate_hz / 1000.0) as isize).max(1);
    let flat_samples = (left_samples - taper_samples).max(0);
    let distance = centered_distance(n);
    for (slot, dist) in window.iter_mut().zip(distance.iter()) {
        let dist = *dist;
        // The whole right side and the flat part of the left side are unity.
        if dist >= -flat_samples {
            *slot = 1.0;
        } else if dist >= -left_samples {
            let into_taper = (-dist - flat_samples) as f64 / taper_samples as f64;
            *slot = 0.5 * (1.0 + (std::f64::consts::PI * into_taper).cos());
        }
    }
    window
}

fn asymmetric_window(n: usize, sample_rate_hz: f64, left_ms: f64, right_ms: f64) -> Vec<f64> {
    let mut window = vec![0.0; n];
    let left_samples = (left_ms * sample_rate_hz / 1000.0) as isize;
    let right_infinite = !right_ms.is_finite();
    let right_samples = if right_infinite {
        0
    } else {
        (right_ms * sample_rate_hz / 1000.0) as isize
    };
    let distance = centered_distance(n);
    for (slot, dist) in window.iter_mut().zip(distance.iter()) {
        let dist = *dist;
        if dist == 0 {
            *slot = 1.0;
        } else if dist < 0 {
            if left_samples > 0 && dist >= -left_samples {
                *slot =
                    0.5 * (1.0 + (std::f64::consts::PI * dist as f64 / left_samples as f64).cos());
            }
        } else if right_infinite {
            *slot = 1.0;
        } else if right_samples > 0 && dist <= right_samples {
            *slot = 0.5 * (1.0 + (std::f64::consts::PI * dist as f64 / right_samples as f64).cos());
        }
    }
    window
}

/// `get_base_ideal_no_tilt`: flat 20 Hz - 20 kHz shape with 4th-power edges.
fn base_ideal_no_tilt(f_abs: &[f64]) -> Vec<f64> {
    f_abs
        .iter()
        .map(|f| {
            let f4 = f.powi(4);
            (f4 / (f4 + 1.0)) * (20_000.0f64.powi(4) / (f4 + 20_000.0f64.powi(4)))
        })
        .collect()
}

fn bass_kernel(f_abs: &[f64], bass_frequency_hz: f64) -> Vec<f64> {
    f_abs
        .iter()
        .map(|f| bass_frequency_hz.powi(2) / (f * f + bass_frequency_hz.powi(2)))
        .collect()
}

/// Shared-sub-band weight: 1 at and below the crossover, fading to 0 half an
/// octave above it. LR4 bass management leaves the subwoofer roughly 12 dB
/// down half an octave above the crossover, so genuine per-channel main
/// corrections regain full authority quickly while the band one driver owns
/// is corrected identically on both channels.
fn shared_band_weight(frequency_hz: f64, crossover_hz: f64) -> f64 {
    if frequency_hz <= crossover_hz {
        1.0
    } else {
        (1.0 - (frequency_hz / crossover_hz).log2() / 0.5).clamp(0.0, 1.0)
    }
}

fn tilt_log_profile(f_abs: &[f64]) -> Vec<f64> {
    f_abs
        .iter()
        .map(|f| (f.max(20.0) / 1000.0).log2())
        .collect()
}

/// `get_minimum_phase_from_mag`: real-cepstrum spectral factorization of a
/// full (conjugate-symmetric) magnitude spectrum.
fn minimum_phase_from_magnitude(magnitude: &[f64]) -> Vec<C64> {
    let n = magnitude.len();
    let mut cepstrum: Vec<C64> = magnitude
        .iter()
        .map(|m| C64::new((m + 1e-12).ln(), 0.0))
        .collect();
    ifft_in_place(&mut cepstrum);
    let mut lifted = vec![C64::new(0.0, 0.0); n];
    lifted[0].re = cepstrum[0].re;
    if n % 2 == 0 {
        for index in 1..n / 2 {
            lifted[index].re = 2.0 * cepstrum[index].re;
        }
        lifted[n / 2].re = cepstrum[n / 2].re;
    } else {
        for index in 1..n.div_ceil(2) {
            lifted[index].re = 2.0 * cepstrum[index].re;
        }
    }
    fft_in_place(&mut lifted);
    lifted.iter().map(|value| value.exp()).collect()
}

/// `get_smoothing_indices`: boxcar bounds for fractional-octave smoothing.
fn smoothing_indices(
    n: usize,
    sample_rate_hz: f64,
    fraction: f64,
) -> (usize, Vec<usize>, Vec<usize>) {
    let num_pos = if n % 2 == 0 { n / 2 + 1 } else { n.div_ceil(2) };
    let df = sample_rate_hz / n as f64;
    let width_factor = 2.0f64.powf(1.0 / (2.0 * fraction)) - 2.0f64.powf(-1.0 / (2.0 * fraction));
    let mut starts = Vec::with_capacity(num_pos.saturating_sub(1));
    let mut ends = Vec::with_capacity(num_pos.saturating_sub(1));
    for index in 1..num_pos {
        let width_hz = df * index as f64 * width_factor;
        let bins = (width_hz / df / 2.0).max(1.0) as usize;
        let start = index.saturating_sub(bins).max(1);
        let end = (index + bins + 1).min(num_pos);
        starts.push(start);
        ends.push(end);
    }
    (num_pos, starts, ends)
}

fn mirror_conjugate(spectrum: &mut [C64]) {
    let n = spectrum.len();
    if n % 2 == 0 {
        spectrum[n / 2].im = 0.0;
        for index in n / 2 + 1..n {
            spectrum[index] = spectrum[n - index].conj();
        }
    } else {
        for index in n.div_ceil(2)..n {
            spectrum[index] = spectrum[n - index].conj();
        }
    }
}

/// `smooth_spectrum` for complex spectra (variable-width boxcar via cumsum,
/// conjugate-mirrored onto the negative frequencies).
fn smooth_spectrum_complex(spectrum: &[C64], sample_rate_hz: f64, fraction: f64) -> Vec<C64> {
    if fraction <= 0.0 {
        return spectrum.to_vec();
    }
    let n = spectrum.len();
    let (num_pos, starts, ends) = smoothing_indices(n, sample_rate_hz, fraction);
    let mut cumulative = vec![C64::new(0.0, 0.0); num_pos + 1];
    for index in 0..num_pos {
        cumulative[index + 1] = cumulative[index] + spectrum[index];
    }
    let mut smoothed = vec![C64::new(0.0, 0.0); n];
    for (offset, index) in (1..num_pos).enumerate() {
        let start = starts[offset];
        let end = ends[offset];
        smoothed[index] = (cumulative[end] - cumulative[start]) / (end - start) as f64;
    }
    smoothed[0] = C64::new(spectrum[0].re, 0.0);
    mirror_conjugate(&mut smoothed);
    smoothed
}

/// `smooth_spectrum` for real (magnitude) spectra.
fn smooth_spectrum_real(values: &[f64], sample_rate_hz: f64, fraction: f64) -> Vec<f64> {
    if fraction <= 0.0 {
        return values.to_vec();
    }
    let n = values.len();
    let (num_pos, starts, ends) = smoothing_indices(n, sample_rate_hz, fraction);
    let mut cumulative = vec![0.0; num_pos + 1];
    for index in 0..num_pos {
        cumulative[index + 1] = cumulative[index] + values[index];
    }
    let mut smoothed = vec![0.0; n];
    for (offset, index) in (1..num_pos).enumerate() {
        let start = starts[offset];
        let end = ends[offset];
        smoothed[index] = (cumulative[end] - cumulative[start]) / (end - start) as f64;
    }
    smoothed[0] = values[0];
    if n % 2 == 0 {
        for index in n / 2 + 1..n {
            smoothed[index] = smoothed[n - index];
        }
    } else {
        for index in n.div_ceil(2)..n {
            smoothed[index] = smoothed[n - index];
        }
    }
    smoothed
}

/// `np.percentile(..., q)` with the default linear interpolation.
fn percentile_linear(values: &[f64], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let rank = (n - 1) as f64 * q / 100.0;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    sorted[lower] + (sorted[upper] - sorted[lower]) * (rank - lower as f64)
}

/// `np.median` (average of the two central values for even counts).
fn median_value(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

/// `np.interp`: linear interpolation clamped to the endpoints.
fn interp_clamped(x: f64, xs: &[f64], ys: &[f64]) -> f64 {
    if x <= xs[0] {
        return ys[0];
    }
    if x >= xs[xs.len() - 1] {
        return ys[ys.len() - 1];
    }
    let mut upper = xs.partition_point(|value| *value < x);
    if upper == 0 {
        upper = 1;
    }
    let lower = upper - 1;
    let span = xs[upper] - xs[lower];
    if span <= 0.0 {
        return ys[lower];
    }
    ys[lower] + (ys[upper] - ys[lower]) * (x - xs[lower]) / span
}

fn argmax_abs(values: &[f64]) -> usize {
    let mut best_index = 0;
    let mut best_value = f64::NEG_INFINITY;
    for (index, value) in values.iter().enumerate() {
        let magnitude = value.abs();
        if magnitude > best_value {
            best_value = magnitude;
            best_index = index;
        }
    }
    best_index
}

/// `find_natural_low_cutoff`.
fn find_natural_low_cutoff(f_pos: &[f64], mag_smoothed: &[f64]) -> f64 {
    if f_pos.is_empty() || mag_smoothed.is_empty() {
        return 50.0;
    }
    let mag_db: Vec<f64> = mag_smoothed
        .iter()
        .map(|m| 20.0 * m.max(1e-12).log10())
        .collect();
    let reference: Vec<f64> = f_pos
        .iter()
        .zip(mag_db.iter())
        .filter(|(f, _)| **f >= 1_000.0 && **f <= 10_000.0)
        .map(|(_, db)| *db)
        .collect();
    let reference_db = if !reference.is_empty() {
        reference.iter().sum::<f64>() / reference.len() as f64
    } else {
        let max_f = f_pos.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let alt: Vec<f64> = f_pos
            .iter()
            .zip(mag_db.iter())
            .filter(|(f, _)| **f >= 300.0 && **f <= 300.0f64.max(max_f * 0.45))
            .map(|(_, db)| *db)
            .collect();
        if !alt.is_empty() {
            alt.iter().sum::<f64>() / alt.len() as f64
        } else {
            mag_db.iter().sum::<f64>() / mag_db.len() as f64
        }
    };
    let threshold_db = reference_db - 6.0;
    let mut searched_any = false;
    for (f, db) in f_pos.iter().zip(mag_db.iter()) {
        if *f >= 10.0 && *f <= 1_000.0 {
            searched_any = true;
            if *db >= threshold_db {
                return f.clamp(10.0, 300.0);
            }
        }
    }
    if searched_any {
        300.0
    } else {
        50.0
    }
}

/// `find_natural_high_cutoff`.
fn find_natural_high_cutoff(f_pos: &[f64], mag_smoothed: &[f64]) -> f64 {
    let band: Vec<(f64, f64)> = f_pos
        .iter()
        .zip(mag_smoothed.iter())
        .filter(|(f, _)| **f >= 2_000.0 && **f <= 22_050.0)
        .map(|(f, m)| (*f, *m))
        .collect();
    if band.is_empty() {
        return 16_000.0;
    }
    let magnitudes: Vec<f64> = band.iter().map(|(_, m)| *m).collect();
    let top_magnitude = percentile_linear(&magnitudes, 95.0);
    let reference_db = 20.0 * (top_magnitude + 1e-12).log10();
    let threshold_db = reference_db - 10.0;
    let mut last_hit = None;
    for (f, magnitude) in &band {
        let db = 20.0 * (magnitude + 1e-12).log10();
        if db >= threshold_db {
            last_hit = Some(*f);
        }
    }
    match last_hit {
        Some(f) => f.clamp(6_000.0, 20_000.0),
        None => 16_000.0,
    }
}

/// Peak-aligned excess-phase inverse of one IR on its own FFT grid - the
/// exact object the mixed-phase corrector inverts (conj(H/H_min)). Shared by
/// `precompute_secs_channel` and the delay-requirement probe so the probe
/// judges precisely what the design will be asked to correct.
struct SecsExcessInverse {
    freqs: Vec<f64>,
    own_magnitude: Vec<f64>,
    excess_inv_h: Vec<C64>,
}

fn secs_excess_phase_inverse(ir: &[f64], sample_rate_hz: f64) -> SecsExcessInverse {
    let n = ir.len();
    let freqs = fft_frequencies(n, sample_rate_hz);
    let peak_index = argmax_abs(ir);
    let h_raw = fft_real(ir, n);
    let shift = sample_shift_phase(&freqs, sample_rate_hz, peak_index as f64);
    let h_shifted: Vec<C64> = h_raw.iter().zip(shift.iter()).map(|(h, s)| h * s).collect();
    let own_magnitude: Vec<f64> = h_raw.iter().map(|h| h.norm()).collect();
    let min_h = minimum_phase_from_magnitude(&own_magnitude);
    let excess_inv_h: Vec<C64> = h_shifted
        .iter()
        .zip(min_h.iter())
        .map(|(h, m)| (h / (m + 1e-12)).conj())
        .collect();
    SecsExcessInverse {
        freqs,
        own_magnitude,
        excess_inv_h,
    }
}

/// Matched-filter scan for the common advance the excess-phase inverse asks
/// for within one frequency band: the bulk delay whose removal maximizes the
/// coherent sum over the band's bins, on a 0.5 ms grid. Returns the best
/// delay and its coherence (peak coherent sum over the incoherent sum, 1 =
/// the whole band is one clean delay).
/// Unwrapped phase of a spectrum's positive-frequency bins (indices
/// `1..=n/2` for even `n`), radians. The smoothed excess corrector's
/// adjacent-bin phase steps are well under pi, so simple unwrapping is exact.
fn unwrap_positive_phase(spectrum: &[C64]) -> Vec<f64> {
    let n = spectrum.len();
    let half = n / 2;
    let mut phases = Vec::with_capacity(half);
    let mut previous = 0.0f64;
    let mut offset = 0.0f64;
    for value in spectrum.iter().take(half + 1).skip(1) {
        let mut candidate = value.arg() + offset;
        while candidate - previous > std::f64::consts::PI {
            offset -= std::f64::consts::TAU;
            candidate -= std::f64::consts::TAU;
        }
        while previous - candidate > std::f64::consts::PI {
            offset += std::f64::consts::TAU;
            candidate += std::f64::consts::TAU;
        }
        previous = candidate;
        phases.push(candidate);
    }
    phases
}

/// Rebuild an excess-corrector IR on this design's grid from an exported
/// phase curve: linear interpolation of unwrapped phase in frequency, unit
/// magnitude, held at the last exported value above the source's top bin
/// (constant phase = zero group delay there - the mid/high pre-ring windows
/// flatten the excess in that region anyway).
fn excess_ir_from_phase_export(
    frequencies_hz: &[f64],
    phase_rad: &[f64],
    n: usize,
    sample_rate_hz: f64,
) -> Vec<f64> {
    let interpolate = |frequency: f64| -> f64 {
        match frequencies_hz.binary_search_by(|probe| probe.total_cmp(&frequency)) {
            Ok(index) => phase_rad[index],
            Err(0) => phase_rad[0],
            Err(index) if index >= frequencies_hz.len() => phase_rad[phase_rad.len() - 1],
            Err(index) => {
                let f0 = frequencies_hz[index - 1];
                let f1 = frequencies_hz[index];
                let fraction = (frequency - f0) / (f1 - f0);
                phase_rad[index - 1] + fraction * (phase_rad[index] - phase_rad[index - 1])
            }
        }
    };
    let half = n / 2;
    let bin_hz = sample_rate_hz / n as f64;
    let mut spectrum = vec![C64::new(1.0, 0.0); n];
    for (index, slot) in spectrum.iter_mut().enumerate().take(half + 1).skip(1) {
        *slot = C64::from_polar(1.0, interpolate(index as f64 * bin_hz));
    }
    // A real IR needs a real Nyquist bin and a conjugate-mirrored tail.
    if n % 2 == 0 {
        spectrum[half] = C64::new(1.0, 0.0);
    }
    mirror_conjugate(&mut spectrum);
    ifft_in_place(&mut spectrum);
    spectrum.iter().map(|value| value.re).collect()
}

/// `SECS_GUARD_DEBUG` diagnostic: median per-bin group delay of a spectrum
/// over one band, the same statistic the guard's stage 2 uses. Lets the
/// excess-corrector pipeline be watched stage by stage when the env var is
/// set; compiled in but inert otherwise.
fn debug_band_gd_ms(label: &str, spectrum: &[C64], sample_rate_hz: f64, low_hz: f64, high_hz: f64) {
    if std::env::var_os("SECS_GUARD_DEBUG").is_none() {
        return;
    }
    let n = spectrum.len();
    let bin_hz = sample_rate_hz / n as f64;
    let mut values = Vec::new();
    for index in 1..n / 2 {
        let frequency = index as f64 * bin_hz;
        if frequency < low_hz || frequency >= high_hz {
            continue;
        }
        let here = spectrum[index] / (spectrum[index].norm() + 1e-30);
        let ahead = spectrum[index + 1] / (spectrum[index + 1].norm() + 1e-30);
        let delta = (ahead * here.conj()).arg();
        values.push(-1_000.0 * delta / (2.0 * std::f64::consts::PI * bin_hz));
    }
    if values.is_empty() {
        return;
    }
    values.sort_by(f64::total_cmp);
    eprintln!(
        "stage {label}: {low_hz:.0}-{high_hz:.0} Hz median gd {:+.1} ms ({} bins)",
        values[values.len() / 2],
        values.len()
    );
}

fn band_advance_scan_ms(
    freqs: &[f64],
    excess_inv_h: &[C64],
    low_hz: f64,
    high_hz: f64,
    maximum_ms: f64,
) -> (f64, f64) {
    let band: Vec<usize> = (0..freqs.len())
        .filter(|index| {
            let frequency = freqs[*index];
            frequency > low_hz && frequency < high_hz
        })
        .collect();
    let incoherent: f64 = band.iter().map(|&index| excess_inv_h[index].norm()).sum();
    if incoherent <= 0.0 {
        return (0.0, 0.0);
    }
    let mut best_ms = 0.0_f64;
    let mut best_score = f64::NEG_INFINITY;
    let mut candidate_ms = 0.0;
    while candidate_ms <= maximum_ms + 1e-9 {
        let score = band
            .iter()
            .map(|&index| {
                let rotation = C64::from_polar(
                    1.0,
                    -2.0 * std::f64::consts::PI * freqs[index] * candidate_ms / 1000.0,
                );
                excess_inv_h[index] * rotation
            })
            .sum::<C64>()
            .norm();
        if score > best_score {
            best_score = score;
            best_ms = candidate_ms;
        }
        candidate_ms += 0.5;
    }
    (best_ms, (best_score / incoherent).clamp(0.0, 1.0))
}

struct SecsChannelFilter {
    taps: Vec<f64>,
    target_mag_ideal: Vec<f64>,
    /// Whether the phase guard removed excess correction anywhere inside the
    /// shared sub band. Below the bass-management crossover one subwoofer
    /// reproduces both channels, so the two filters must agree in phase
    /// there; the caller re-runs both with the removal forced when the
    /// channels disagree.
    shared_band_guard_removed: bool,
}

/// Phase guard (app extension; see `SecsConfig::phase_guard`): blend the
/// windowed excess corrector back to unity wherever it fails its own job.
///
/// `windowed` is the pre-ring-windowed, unit-magnitude corrector about to be
/// applied; `excess_inv_ir` is the unwindowed corrector it was cut from,
/// whose conjugate spectrum is the room's measured excess phase. The
/// corrected residual is `excess x windowed`; without correction it is
/// `excess` alone. Comparing the two per-bin group delays asks the only
/// question that matters - does applying the corrector here bring the timing
/// closer to zero than leaving it alone? - and covers every failure mode of
/// the causal truncation at once, without modeling the window itself. The
/// keep/drop verdict is smoothed over frequency so the blend stays a smooth
/// all-pass, and the result is renormalized to unit magnitude.
///
/// `shared_low_frequency_hz` marks the band where one subwoofer reproduces
/// both channels. The verdict there must be identical for L and R - the two
/// signals are summed into one driver, so a phase split cancels mono bass at
/// the sub input, and a per-channel verification sweep cannot see it. The
/// function reports whether it removed anything inside that band, and
/// `force_shared_removal` lets the caller re-run the other channel with the
/// same decision. (Without this the guard broke the 2.1 commonization: on a
/// real 2026-08-04 filter L spent 27.3 ms of its 18 ms band budget and was
/// stripped while R spent 7.9 ms and kept its correction, leaving 84 degrees
/// of L/R phase split and -1.95 dB of mono bass at 20-30 Hz.)
/// `advance_budget_ms` is the low-band pre-ring window actually applied: a
/// corrector may legitimately *advance* a band by up to that much (that is
/// the extended-delay feature), so the band budget bound is asymmetric -
/// delay is capped by the gate share alone, advance by the budget plus it.
fn guard_unrealizable_excess(
    windowed: &mut [C64],
    excess_inv_ir: &[f64],
    sample_rate_hz: f64,
    shared_low_frequency_hz: Option<f64>,
    force_shared_removal: bool,
    advance_budget_ms: f64,
) -> bool {
    let n = windowed.len();
    let half = n / 2;
    if half < 4 {
        return false;
    }
    let mut shared_band_removed = false;
    let ideal = fft_real(excess_inv_ir, n);
    let bin_hz = sample_rate_hz / n as f64;
    let normalized = |value: C64| -> C64 {
        let norm = value.norm();
        if norm > 1e-12 {
            value / norm
        } else {
            C64::new(1.0, 0.0)
        }
    };
    // The room's excess phase is the conjugate of the ideal inverse
    // corrector; magnitudes are normalized out because only phase slopes
    // matter here.
    let residual_with: Vec<C64> = (0..=half)
        .map(|index| normalized(ideal[index].conj() * windowed[index]))
        .collect();
    let residual_without: Vec<C64> = (0..=half)
        .map(|index| normalized(ideal[index].conj()))
        .collect();
    let group_delay_ms = |curve: &[C64], index: usize| -> f64 {
        let delta = (curve[index + 1] * curve[index].conj()).arg();
        -1_000.0 * delta / (2.0 * std::f64::consts::PI * bin_hz)
    };
    let mut keep = vec![1.0_f64; n];
    for (index, slot) in keep.iter_mut().enumerate().take(half) {
        let with_correction = group_delay_ms(&residual_with, index).abs();
        let without_correction = group_delay_ms(&residual_without, index).abs();
        // Stage 1 is a smooth per-bin blend on both channels alike; its small
        // divergence is bounded (the windowing alone already leaves a few
        // degrees). The channel-splitting decision is the binary band kill
        // in stage 2, so only that one is reconciled across channels.
        if with_correction > without_correction + SECS_PHASE_GUARD_TOLERANCE_MS {
            *slot = 0.0;
        }
    }
    // Mirror the verdict onto the conjugate half so the blended spectrum
    // remains the spectrum of a real impulse response.
    for index in half + 1..n {
        keep[index] = keep[n - index];
    }
    let smoothed = smooth_spectrum_real(&keep, sample_rate_hz, 3.0);
    for (value, weight) in windowed.iter_mut().zip(smoothed.iter()) {
        let blend = weight.clamp(0.0, 1.0);
        *value = *value * blend + C64::new(1.0 - blend, 0.0);
        *value = normalized(*value);
    }

    // Stage 2 (added after a real 2026-08-04 session tripped the design gate
    // with stage 1 alone): on real modal excess the per-bin verdict
    // degenerates. The baseline's own per-bin group delay is noise with
    // swings far beyond any tolerance, so the comparison above turns into a
    // variance contest, and a corrector that adds a consistent *bias* while
    // slightly reducing variance wins most bins and survives - the field
    // filter kept +37 to +55 ms of low-band bias that way. Band medians see
    // the bias straight through the noise: for each gate band, if the
    // corrector as blended still worsens the residual's median group delay,
    // the correction is removed across the whole band (half-octave
    // crossfades), which is exact where partial per-bin blends are not.
    let residual_gd_ms = |windowed: &[C64], index: usize| -> f64 {
        let here = normalized(ideal[index].conj() * windowed[index]);
        let ahead = normalized(ideal[index + 1].conj() * windowed[index + 1]);
        let delta = (ahead * here.conj()).arg();
        -1_000.0 * delta / (2.0 * std::f64::consts::PI * bin_hz)
    };
    let median = |mut values: Vec<f64>| -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        values.sort_by(f64::total_cmp);
        Some(values[values.len() / 2])
    };
    let corrector_gd_ms = |windowed: &[C64], index: usize| -> f64 {
        let delta = (windowed[index + 1] * windowed[index].conj()).arg();
        -1_000.0 * delta / (2.0 * std::f64::consts::PI * bin_hz)
    };
    for (band_low_hz, band_high_hz, limit_ms) in SECS_GROUP_DELAY_GATE_BANDS {
        let mut with_correction = Vec::new();
        let mut without_correction = Vec::new();
        let mut corrector_alone = Vec::new();
        for index in 1..half.saturating_sub(1) {
            let frequency = index as f64 * bin_hz;
            if frequency < band_low_hz || frequency >= band_high_hz {
                continue;
            }
            with_correction.push(residual_gd_ms(windowed, index));
            without_correction.push(group_delay_ms(&residual_without, index));
            corrector_alone.push(corrector_gd_ms(windowed, index));
        }
        let (Some(median_with), Some(median_without), Some(median_corrector)) = (
            median(with_correction),
            median(without_correction),
            median(corrector_alone),
        ) else {
            continue;
        };
        if std::env::var_os("SECS_GUARD_DEBUG").is_some() {
            eprintln!(
                "guard band {band_low_hz:.0}-{band_high_hz:.0}: with {median_with:+.1} ms, \
                 without {median_without:+.1} ms, corrector {median_corrector:+.1} ms"
            );
        }
        // Two kill criteria. The comparative one catches a corrector that
        // worsens the system; the budget one catches a corrector that
        // "helps" some residual statistic while spending more of the gate
        // band's group-delay budget than the whole filter is allowed to -
        // the field failure mode: an unweighted residual median looked
        // improved while the corrector alone carried +34 ms into the taps.
        let band_touches_shared = shared_low_frequency_hz.is_some_and(|crossover_hz| {
            band_low_hz < 2.0 * crossover_hz && band_high_hz > 0.5 * crossover_hz
        });
        if median_with.abs() > median_without.abs() + SECS_PHASE_GUARD_TOLERANCE_MS
            || median_corrector > SECS_PHASE_GUARD_BAND_BUDGET_RATIO * limit_ms
            || median_corrector
                < -(advance_budget_ms + SECS_PHASE_GUARD_BAND_BUDGET_RATIO * limit_ms)
            || (band_touches_shared && force_shared_removal)
        {
            if band_touches_shared {
                shared_band_removed = true;
            }
            let fade_low_hz = band_low_hz * 2.0_f64.powf(-0.5);
            let fade_high_hz = band_high_hz * 2.0_f64.powf(0.5);
            for (index, value) in windowed.iter_mut().enumerate() {
                // Symmetric in |f| so the spectrum stays that of a real IR.
                let frequency = (index.min(n - index)) as f64 * bin_hz;
                let blend = if frequency <= fade_low_hz || frequency >= fade_high_hz {
                    1.0
                } else if frequency < band_low_hz {
                    let ratio =
                        (frequency / fade_low_hz).log2() / (band_low_hz / fade_low_hz).log2();
                    0.5 + 0.5 * (std::f64::consts::PI * ratio).cos()
                } else if frequency >= band_high_hz {
                    let ratio =
                        (frequency / band_high_hz).log2() / (fade_high_hz / band_high_hz).log2();
                    0.5 - 0.5 * (std::f64::consts::PI * ratio).cos()
                } else {
                    0.0
                };
                *value = *value * blend + C64::new(1.0 - blend, 0.0);
                *value = normalized(*value);
            }
        }
    }
    shared_band_removed
}

/// `FilterGeneratorWorker._shift_right_zero`.
fn shift_right_zero(signal: &[f64], shift_samples: usize) -> Vec<f64> {
    if shift_samples == 0 {
        return signal.to_vec();
    }
    let mut shifted = vec![0.0; signal.len()];
    if shift_samples < signal.len() {
        shifted[shift_samples..].copy_from_slice(&signal[..signal.len() - shift_samples]);
    }
    shifted
}

struct DelayEvaluation {
    metric: f64,
    crop_left: Vec<f64>,
    crop_right: Vec<f64>,
    target_mag_left: Vec<f64>,
    low_cutoff_hz: f64,
    high_cutoff_hz: f64,
    target_delay_samples: usize,
    delay_ms: f64,
}

fn coarse_delay_grid() -> Vec<f64> {
    let mut grid = Vec::new();
    let mut delay = SECS_AUTO_DELAY_MIN_MS;
    while delay <= SECS_AUTO_DELAY_MAX_MS + 1e-9 {
        grid.push(delay);
        delay += SECS_AUTO_DELAY_COARSE_STEP_MS;
    }
    grid
}

// ---------------------------------------------------------------------------
// Forked from secs.rs with the fix switches threaded through. With every
// switch off these are the secs.rs functions bit for bit.
// ---------------------------------------------------------------------------

/// `get_lr4_weights_5bands`: complementary LR4-style split at 100/150/200/300 Hz.
///
/// Fix [1] (`normalized`): the five products `l1, (1-l1)l2, (1-l2)l3,
/// (1-l3)l4, 1-l4` are not a partition of unity - their sum peaks at 1.257
/// (+1.99 dB) near 173 Hz. Every 5-band smoothing therefore inflated the
/// 100-300 Hz magnitude, and the inversion cut a -2 dB dip at ~174 Hz into a
/// perfectly flat speaker (measured on the original port in every resolution
/// mode), while the SECS graph - smoothed with the same weights - showed a
/// perfect target match. Normalizing per bin makes every consumer a true
/// weighted average: magnitude smoothing, the complex excess smoothing
/// (renormalized afterwards anyway), the window mix, and the bulk-split
/// share `w0 + w1`, which becomes the low band's true share. No other
/// compensation is applied.
fn lr4_weights_5bands(n: usize, sample_rate_hz: f64, normalized: bool) -> [Vec<f64>; 5] {
    let freqs = fft_frequencies(n, sample_rate_hz);
    let f_abs = absolute_frequencies(&freqs);
    let low = |f: f64, corner: f64| 1.0 / (1.0 + (f / corner).powi(4));
    let mut bands: [Vec<f64>; 5] = std::array::from_fn(|_| Vec::with_capacity(n));
    for f in &f_abs {
        let l1 = low(*f, 100.0);
        let l2 = low(*f, 150.0);
        let l3 = low(*f, 200.0);
        let l4 = low(*f, 300.0);
        let mut weights = [
            l1,
            (1.0 - l1) * l2,
            (1.0 - l2) * l3,
            (1.0 - l3) * l4,
            1.0 - l4,
        ];
        if normalized {
            let sum: f64 = weights.iter().sum();
            for weight in weights.iter_mut() {
                *weight /= sum;
            }
        }
        for (band, weight) in bands.iter_mut().zip(weights) {
            band.push(weight);
        }
    }
    bands
}

fn apply_5band_smoothing_complex(
    spectrum: &[C64],
    sample_rate_hz: f64,
    weights: &[Vec<f64>; 5],
    resolution: SecsResolutionMode,
) -> Vec<C64> {
    let fractions = smoothing_fractions(resolution);
    let mut result = vec![C64::new(0.0, 0.0); spectrum.len()];
    for (band, fraction) in fractions.iter().enumerate() {
        let smoothed = smooth_spectrum_complex(spectrum, sample_rate_hz, *fraction);
        for (index, value) in smoothed.iter().enumerate() {
            result[index] += weights[band][index] * value;
        }
    }
    result
}

fn apply_5band_smoothing_real(
    values: &[f64],
    sample_rate_hz: f64,
    weights: &[Vec<f64>; 5],
    resolution: SecsResolutionMode,
) -> Vec<f64> {
    let fractions = smoothing_fractions(resolution);
    let mut result = vec![0.0; values.len()];
    for (band, fraction) in fractions.iter().enumerate() {
        let smoothed = smooth_spectrum_real(values, sample_rate_hz, *fraction);
        for (index, value) in smoothed.iter().enumerate() {
            result[index] += weights[band][index] * value;
        }
    }
    result
}

/// `precompute_secs_channel`.
/// `magnitude_override` (multi-position extension, not in SECS.py): when
/// `Some`, this full-FFT-grid magnitude replaces the channel's own magnitude
/// for everything on the magnitude side - smoothing, target construction,
/// reference anchoring, inversion, and the macro-EQ simulation. The phase
/// side stays strictly single-point: the minimum-phase split that yields the
/// excess-phase corrector and the `phase_reg` confidence weight both keep the
/// IR's own magnitude, because an averaged magnitude has no phase meaning and
/// would un-guard the excess inversion exactly where this IR has a null.
/// `target_overlay_linear` (house-curve extension, not in SECS.py): when
/// `Some`, this linear gain on the full FFT grid - the user's target curve
/// re-anchored to 0 dB at [`SECS_TARGET_OVERLAY_ANCHOR_HZ`], evaluated by the
/// caller - multiplies the adaptive target after the ideal-envelope cap,
/// exactly like the bass shelf.
/// Edition changes: fix [1] (normalized weights) and the fix [3] level
/// anchor; see the comments at each site.
fn precompute_secs_channel(
    ir: &[f64],
    config: &SecsConfig,
    magnitude_override: Option<&[f64]>,
    target_overlay_linear: Option<&[f64]>,
    fixes: &EditedFixes,
) -> SecsChannelPrecompute {
    let n = ir.len();
    let sample_rate_hz = f64::from(config.sample_rate_hz);
    let weights = lr4_weights_5bands(n, sample_rate_hz, fixes.normalized_weights);
    let inverse = secs_excess_phase_inverse(ir, sample_rate_hz);
    let freqs = inverse.freqs;
    let f_abs = absolute_frequencies(&freqs);
    let own_magnitude = inverse.own_magnitude;
    let mut excess_inv_h = inverse.excess_inv_h;
    // Extended-delay bulk split (improved path only): the complex band
    // smoothing below was designed around the SECS.py <=10 ms budget. A room
    // whose bass trails by 40-60 ms puts a phase ramp of several full turns
    // per smoothing window into the corrector, and averaging unit vectors
    // through full turns cancels them - the intended advance never survives
    // into the smoothed corrector (measured on real captures: a 60 ms budget
    // still produced a +26 to +41 ms *delaying* corrector). So the common
    // low-band bulk advance is estimated first, divided out so the smoothing
    // only sees the shallow residual structure it was built for, and
    // restored afterwards. The guard and the group-delay gate still judge
    // the outcome; this only changes what the smoothing is asked to survive.
    // A band-wise rotation profile was tried here and measured worse on
    // real captures in every variant (see `secs_low_band_advance_profile_ms`
    // and docs/dsp.md): the 20-40 Hz excess of the 2026-08-03 room has no
    // coherent arrival (coherence at the noise floor), so per-band deviations
    // either inject junk delays or flip the guard's finely balanced L/R
    // verdicts (a few ms of rotation change swung the L 20-40 Hz seat
    // residual from +12.8 to +34.5 ms, and one divergent verdict made the
    // 2.1 reconciliation strip both channels entirely). The scalar wide-band
    // estimate is what the guard and the field results were verified with.
    let bulk_advance_ms = if config.phase_guard && config.maximum_delay_ms > SECS_AUTO_DELAY_MAX_MS
    {
        let (best_ms, coherence) =
            band_advance_scan_ms(&freqs, &excess_inv_h, 20.0, 90.0, config.maximum_delay_ms);
        if std::env::var_os("SECS_GUARD_DEBUG").is_some() {
            eprintln!("bulk advance estimate: {best_ms:.1} ms (coherence {coherence:.2})");
        }
        best_ms
    } else {
        0.0
    };
    // The bulk ramp is confined to the low split (the same complementary
    // LR4 partition whose pre-ring window is pr_low), fading out with it:
    // above the split the pre-ring caps are 10/5 ms and a large advance
    // ramp would only be truncated into corruption there.
    let bulk_weight: Vec<f64> = (0..n)
        .map(|index| weights[0][index] + weights[1][index])
        .collect();
    if bulk_advance_ms > 0.0 {
        for index in 0..n {
            excess_inv_h[index] *= C64::from_polar(
                1.0,
                -2.0 * std::f64::consts::PI * freqs[index] * bulk_advance_ms * bulk_weight[index]
                    / 1000.0,
            );
        }
    }
    let mag_orig: Vec<f64> = match magnitude_override {
        Some(magnitude) => magnitude.to_vec(),
        None => own_magnitude.clone(),
    };

    let mut smoothed_mag_orig =
        apply_5band_smoothing_real(&mag_orig, sample_rate_hz, &weights, config.resolution);

    let band_15_20: Vec<f64> = f_abs
        .iter()
        .zip(smoothed_mag_orig.iter())
        .filter(|(f, _)| **f >= 15.0 && **f <= 20.0)
        .map(|(_, m)| *m)
        .collect();
    let mag_15 = if !band_15_20.is_empty() {
        band_15_20.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    } else {
        smoothed_mag_orig
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
    };
    for (f, value) in f_abs.iter().zip(smoothed_mag_orig.iter_mut()) {
        if *f < 15.0 {
            *value = value.min(mag_15);
        }
    }

    // Phase confidence stays anchored to this IR's own magnitude (see the
    // function comment): where THIS capture has a null, its measured phase is
    // untrustworthy no matter what the spatial average says.
    let smoothed_mag_simple = smooth_spectrum_real(&own_magnitude, sample_rate_hz, 3.0);
    let valid_mask: Vec<bool> = f_abs.iter().map(|f| *f > 100.0 && *f < 10_000.0).collect();
    let valid_values: Vec<f64> = mag_orig
        .iter()
        .zip(valid_mask.iter())
        .filter(|(_, keep)| **keep)
        .map(|(m, _)| *m)
        .collect();
    let legacy_ref_mag = || {
        if !valid_values.is_empty() {
            percentile_linear(&valid_values, 35.0)
        } else {
            mag_orig.iter().sum::<f64>() / mag_orig.len() as f64
        }
    };
    // Fix [3], level anchor half. The legacy anchor is the 35th percentile of
    // the raw 100 Hz - 10 kHz linear bins, i.e. in practice the 1-10 kHz
    // level. Once the macro EQ stops correcting above the upper limit U, the
    // level must be anchored where nothing is corrected, or the corrected
    // band lands several dB off the untouched band. So the anchor becomes the
    // RMS of the 1/3-octave-smoothed magnitude between U and 4U - of
    // `mag_orig`, the magnitude the correction targets (the multi-position
    // average when one is supplied), not of the P0-only phase-confidence
    // magnitude above. Both halves of fix [3] switch together.
    let ref_mag = match fixes.correction_upper_hz {
        Some(upper_hz) => {
            let smoothed = smooth_spectrum_real(&mag_orig, sample_rate_hz, 3.0);
            let band: Vec<f64> = f_abs
                .iter()
                .zip(smoothed.iter())
                .filter(|(f, _)| **f > upper_hz && **f < 4.0 * upper_hz)
                .map(|(_, m)| m * m)
                .collect();
            if band.is_empty() {
                legacy_ref_mag()
            } else {
                (band.iter().sum::<f64>() / band.len() as f64).sqrt()
            }
        }
        None => legacy_ref_mag(),
    };

    let simple_valid: Vec<f64> = smoothed_mag_simple
        .iter()
        .zip(valid_mask.iter())
        .filter(|(_, keep)| **keep)
        .map(|(m, _)| *m)
        .collect();
    let simple_mean = if simple_valid.is_empty() {
        0.0
    } else {
        simple_valid.iter().sum::<f64>() / simple_valid.len() as f64
    };
    for (index, value) in excess_inv_h.iter_mut().enumerate() {
        let regularizer =
            ((smoothed_mag_simple[index] / (simple_mean + 1e-12) - 0.177) / 0.5).clamp(0.0, 1.0);
        *value = *value * regularizer + C64::new(1.0 - regularizer, 0.0);
    }
    debug_band_gd_ms(
        "1 excess (bulk removed)",
        &excess_inv_h,
        sample_rate_hz,
        20.0,
        100.0,
    );
    let mut smoothed_excess =
        apply_5band_smoothing_complex(&excess_inv_h, sample_rate_hz, &weights, config.resolution);
    for value in smoothed_excess.iter_mut() {
        *value /= value.norm() + 1e-12;
    }
    debug_band_gd_ms("2 smoothed", &smoothed_excess, sample_rate_hz, 20.0, 100.0);
    // Restore the bulk advance the smoothing never saw.
    if bulk_advance_ms > 0.0 {
        for index in 0..n {
            smoothed_excess[index] *= C64::from_polar(
                1.0,
                2.0 * std::f64::consts::PI * freqs[index] * bulk_advance_ms * bulk_weight[index]
                    / 1000.0,
            );
        }
    }
    debug_band_gd_ms("3 restored", &smoothed_excess, sample_rate_hz, 20.0, 100.0);
    let mut excess_time = smoothed_excess;
    ifft_in_place(&mut excess_time);
    let excess_inv_ir: Vec<f64> = excess_time.iter().map(|value| value.re).collect();

    let base_ideal = base_ideal_no_tilt(&f_abs);
    let bass_boost_linear: Vec<f64> = if config.bass_boost_db > 0.0 {
        bass_kernel(&f_abs, config.bass_frequency_hz)
            .iter()
            .map(|kernel| 10f64.powf(config.bass_boost_db * kernel / 20.0))
            .collect()
    } else {
        vec![1.0; n]
    };

    let num_pos = if n % 2 == 0 { n / 2 + 1 } else { n.div_ceil(2) };
    let f_pos = &f_abs[..num_pos];
    let mag_pos = &smoothed_mag_orig[..num_pos];
    let low_cutoff_hz = find_natural_low_cutoff(f_pos, mag_pos);
    let high_cutoff_hz = find_natural_high_cutoff(f_pos, mag_pos);

    let mid_values: Vec<f64> = f_abs
        .iter()
        .zip(smoothed_mag_orig.iter())
        .filter(|(f, _)| **f >= 200.0 && **f <= 1_000.0)
        .map(|(_, m)| *m)
        .collect();
    let ref_mid = if !mid_values.is_empty() {
        mid_values.iter().sum::<f64>() / mid_values.len() as f64
    } else {
        ref_mag
    };
    let noise_threshold = ref_mid * 10f64.powf(-30.0 / 20.0);

    let mut track_weight = Vec::with_capacity(n);
    let mut rolloff_env = Vec::with_capacity(n);
    let mut target_curve = Vec::with_capacity(n);
    for index in 0..n {
        let f = f_abs[index];
        let blend_low =
            ((low_cutoff_hz * 1.15 - f) / (low_cutoff_hz * 0.15 + 1e-12)).clamp(0.0, 1.0);
        let blend_high =
            ((f - high_cutoff_hz * 0.8) / (high_cutoff_hz * 0.2 + 1e-12)).clamp(0.0, 1.0);
        let blend_noise = if f >= 200.0 {
            0.0
        } else {
            ((noise_threshold * 2.0 - smoothed_mag_orig[index]) / (noise_threshold + 1e-12))
                .clamp(0.0, 1.0)
        };
        let track = (blend_low + blend_high + blend_noise).clamp(0.0, 1.0);
        let natural = smoothed_mag_orig[index] / (ref_mag + 1e-12);
        let envelope = natural.min(1.0);
        let mut target = envelope.powf(track) * base_ideal[index].powf(1.0 - track);
        if f > 1_000.0 {
            target = target.min(base_ideal[index]);
        }
        target *= bass_boost_linear[index];
        if let Some(overlay) = target_overlay_linear {
            target *= overlay[index];
        }
        track_weight.push(track);
        rolloff_env.push(envelope);
        target_curve.push(target);
    }

    let boost_ceiling = 10f64.powf(config.max_boost_db / 20.0);
    let mut mag_inv: Vec<f64> = (0..n)
        .map(|index| {
            let natural = smoothed_mag_orig[index] / (ref_mag + 1e-12);
            (target_curve[index] / (natural + 1e-12)).clamp(0.0, boost_ceiling)
        })
        .collect();
    mag_inv[0] = mag_inv[1];

    let nyquist = sample_rate_hz * 0.5;
    let hf_start = config
        .hf_min_phase_reference_hz
        .clamp(20.0, (nyquist - 1.0).max(20.0));
    let hf_end = (hf_start * 2.0).min(nyquist - 1.0);
    for (index, value) in mag_inv.iter_mut().enumerate() {
        let fade = if hf_end <= hf_start + 1e-9 {
            if f_abs[index] >= hf_start {
                1.0
            } else {
                0.0
            }
        } else {
            ((f_abs[index] - hf_start) / (hf_end - hf_start)).clamp(0.0, 1.0)
        };
        *value = *value * (1.0 - fade) + fade;
    }

    let final_min_h = minimum_phase_from_magnitude(&mag_inv);
    let mut min_time = final_min_h;
    ifft_in_place(&mut min_time);
    let min_inv_ir: Vec<f64> = min_time.iter().map(|value| value.re).collect();
    // HF-follow companion (macro-EQ target only; the minimum-phase inversion
    // above keeps `target_curve`). Above 1 kHz the SECS target carries two
    // shapes that a following, cut-only macro EQ turned into extra treble
    // cuts on top of the house curve: the 20 kHz fourth-power band edge of
    // `base_ideal_no_tilt` (-3 dB at 16 kHz, -6 dB at 20 kHz) and, in the
    // natural-rolloff zone (track -> 1 from 0.8x the high cutoff), the
    // overlay multiplied onto the measured envelope - so 16-20 kHz was cut
    // a second time below a house curve it already met. Following, the
    // ideal above 1 kHz is the overlay alone (times the bass kernel, ~1
    // there) and the zone's target is the lower of the envelope and that
    // ideal: treble above the curve is still cut to it, treble already
    // below it is left alone. Bins at or below 1 kHz keep `target_curve`
    // exactly, so the bass correction is untouched.
    // Same gate as the macro-EQ fade skip in `process_secs_filter`.
    let follow_hf_target = fixes.hf_target_follow && fixes.cut_only_macro;
    let target_mag_base: Vec<f64> = if follow_hf_target {
        (0..n)
            .map(|index| {
                if f_abs[index] <= 1_000.0 {
                    return ref_mag * target_curve[index];
                }
                let overlay = target_overlay_linear.map_or(1.0, |overlay| overlay[index]);
                let ideal = bass_boost_linear[index] * overlay;
                let track = track_weight[index];
                ref_mag * ideal.powf(1.0 - track) * rolloff_env[index].min(ideal).powf(track)
            })
            .collect()
    } else {
        target_curve.iter().map(|t| ref_mag * t).collect()
    };

    SecsChannelPrecompute {
        n,
        sample_rate_hz,
        freqs,
        f_abs,
        weights,
        mag_orig,
        excess_inv_ir,
        min_inv_ir,
        target_mag_base,
        ref_mag,
        track_weight,
        low_cutoff_hz,
        high_cutoff_hz,
    }
}

/// `process_secs_filter`. Edition changes: fix [2] (cut-only macro EQ), the
/// fix [3] macro-EQ band limit and fix [8] (crush exponent).
fn process_secs_filter(
    precomputed: &SecsChannelPrecompute,
    config: &SecsConfig,
    apply_tilt: bool,
    force_shared_removal: bool,
    fixes: &EditedFixes,
) -> SecsChannelFilter {
    let mut shared_band_guard_removed = false;
    let n = precomputed.n;
    let sample_rate_hz = precomputed.sample_rate_hz;
    let freqs = &precomputed.freqs;
    let f_abs = &precomputed.f_abs;
    let weights = &precomputed.weights;

    let initial_filter_h: Vec<C64> = if config.low_latency || config.zero_latency {
        fft_real(&precomputed.min_inv_ir, n)
    } else {
        // The low-band pre-ring cap follows the extended-delay ceiling: for
        // the SECS.py maximum (10 ms) `max(25, 10) = 25` reproduces the
        // original clamp bit for bit, and an extended ceiling passes its
        // full budget to the low band. Mid/high keep their own hard caps.
        let pr_low = config.target_delay_ms.clamp(
            SECS_PHASE_PRERING_LOW_MIN_MS,
            SECS_PHASE_PRERING_LOW_MAX_MS.max(config.maximum_delay_ms),
        );
        let pr_mid = SECS_PHASE_PRERING_MID_MAX_MS.min(pr_low * 0.5);
        let pr_high = SECS_PHASE_PRERING_HIGH_MAX_MS.min(pr_low * 0.25);

        let extended_budget =
            config.phase_guard && config.maximum_delay_ms > SECS_AUTO_DELAY_MAX_MS;
        let windowed_fft = |left_ms: f64, flat_left: bool| -> Vec<C64> {
            let window = if flat_left && extended_budget {
                // See `asymmetric_window_flat_left`: the restored bulk
                // advance sits deep inside the left extent and must not be
                // tapered away. Only the low band asks for this; mid/high
                // keep the original full-extent taper.
                asymmetric_window_flat_left(
                    n,
                    sample_rate_hz,
                    left_ms,
                    SECS_PHASE_PRERING_MID_MAX_MS,
                )
            } else {
                asymmetric_window(n, sample_rate_hz, left_ms, f64::INFINITY)
            };
            let windowed: Vec<f64> = precomputed
                .excess_inv_ir
                .iter()
                .zip(window.iter())
                .map(|(sample, w)| sample * w)
                .collect();
            fft_real(&windowed, n)
        };
        let h_low = windowed_fft(pr_low, true);
        let h_mid = windowed_fft(pr_mid, false);
        let h_high = windowed_fft(pr_high, false);
        debug_band_gd_ms("4 windowed low", &h_low, sample_rate_hz, 20.0, 100.0);
        let mut h_ex_win: Vec<C64> = (0..n)
            .map(|index| {
                (weights[0][index] + weights[1][index]) * h_low[index]
                    + (weights[2][index] + weights[3][index]) * h_mid[index]
                    + weights[4][index] * h_high[index]
            })
            .collect();
        for value in h_ex_win.iter_mut() {
            *value /= value.norm() + 1e-12;
        }
        debug_band_gd_ms("5 mixed pre-guard", &h_ex_win, sample_rate_hz, 20.0, 100.0);
        if config.phase_guard {
            shared_band_guard_removed = guard_unrealizable_excess(
                &mut h_ex_win,
                &precomputed.excess_inv_ir,
                sample_rate_hz,
                config.shared_low_frequency_hz,
                force_shared_removal,
                pr_low,
            );
        }

        let window_long = asymmetric_window(n, sample_rate_hz, pr_low, 500.0);
        let window_short = asymmetric_window(n, sample_rate_hz, pr_low, 10.0);
        let long_windowed: Vec<f64> = precomputed
            .min_inv_ir
            .iter()
            .zip(window_long.iter())
            .map(|(sample, w)| sample * w)
            .collect();
        let short_windowed: Vec<f64> = precomputed
            .min_inv_ir
            .iter()
            .zip(window_short.iter())
            .map(|(sample, w)| sample * w)
            .collect();
        let h_long = fft_real(&long_windowed, n);
        let h_short = fft_real(&short_windowed, n);

        let hf_start = config
            .hf_min_phase_reference_hz
            .clamp(20.0, sample_rate_hz * 0.5 - 1.0);
        let hf_end = (hf_start * 2.0).min(sample_rate_hz * 0.5 - 1.0);
        (0..n)
            .map(|index| {
                let fade = if hf_end <= hf_start + 1e-9 {
                    if f_abs[index] >= hf_start {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    ((f_abs[index] - hf_start) / (hf_end - hf_start)).clamp(0.0, 1.0)
                };
                let windowed_min = h_long[index] * (1.0 - fade) + h_short[index] * fade;
                let delay_phase = C64::from_polar(
                    1.0,
                    -2.0 * std::f64::consts::PI * freqs[index] * config.target_delay_ms / 1000.0,
                );
                h_ex_win[index] * windowed_min * delay_phase
            })
            .collect()
    };

    let mag_simulated: Vec<f64> = precomputed
        .mag_orig
        .iter()
        .zip(initial_filter_h.iter())
        .map(|(mag, h)| mag * h.norm())
        .collect();
    let mag_simulated_tonal = apply_5band_smoothing_real(
        &mag_simulated,
        sample_rate_hz,
        weights,
        SecsResolutionMode::Gentle,
    );
    // Fix [2]: the boost ceiling only bounded the minimum-phase inversion;
    // the macro EQ could add up to +6 dB on top (clamp 0.5..2.0) and so
    // tried to fill reflection nulls - a 100 Hz SBIR null got +7.9 dB at
    // Boost 3 and +9.9 dB at Boost 6 on the original port. Cut-only
    // (0.5..1.0) makes the boost ceiling the true maximum gain and keeps the
    // macro EQ smooth (the 1-octave smoothing below is unchanged). The
    // HF-fade branch only ever shaped boosts, so it is dead and dropped.
    // Consequence: a target shelf (bass boost or house-curve overlay) larger
    // than `max_boost_db` is no longer completed by the macro EQ - the boost
    // ceiling now bounds it. Tilt is still applied after the EQ, as in the
    // original, because it is an explicit user target slope.
    let mut macro_eq_mag: Vec<f64> = (0..n)
        .map(|index| {
            let raw = precomputed.target_mag_base[index] / (mag_simulated_tonal[index] + 1e-12);
            if fixes.cut_only_macro {
                return raw.clamp(0.5, 1.0);
            }
            let clipped = raw.clamp(0.5, 2.0);
            let hf_fade = ((20_000.0 - f_abs[index]) / 10_000.0).clamp(0.0, 1.0);
            if clipped > 1.0 {
                1.0 + (clipped - 1.0) * hf_fade
            } else {
                clipped
            }
        })
        .collect();
    macro_eq_mag = smooth_spectrum_real(&macro_eq_mag, sample_rate_hz, 1.0);
    // Fix [3], band-limit half: the curtain only bounds the minimum-phase
    // stage, while the macro EQ pulled the reverberant steady state toward
    // the target across the whole band - in a room whose steady state falls
    // ~1.2 dB/oct it brightened the direct sound by +2.1 dB at 4 kHz and
    // +2.9 dB at 8 kHz. Above U the steady state is mostly room, not speaker,
    // so the macro EQ fades linearly to unity between U and 2U.
    // With `hf_target_follow` the fade is skipped: the macro EQ is cut-only
    // (fix [2]), so above U it can only lower treble that exceeds the target
    // - which is exactly what a house curve's HF rolloff asks for - and can
    // never lift the reverberant treble again. Bins below U are untouched
    // either way (the fade is identity there).
    let follow_hf_target = fixes.hf_target_follow && fixes.cut_only_macro;
    if let Some(upper_hz) = fixes.correction_upper_hz.filter(|_| !follow_hf_target) {
        for (value, f) in macro_eq_mag.iter_mut().zip(f_abs.iter()) {
            let limit = ((f - upper_hz) / upper_hz).clamp(0.0, 1.0);
            *value = *value * (1.0 - limit) + limit;
        }
    }

    let peaks_only: Vec<f64> = (0..n)
        .map(|index| {
            let error_ratio = (mag_simulated[index] * macro_eq_mag[index])
                / (precomputed.target_mag_base[index] + 1e-12);
            error_ratio.max(1.0)
        })
        .collect();
    let sharp_peaks = smooth_spectrum_real(
        &peaks_only,
        sample_rate_hz,
        sharp_fraction(config.resolution),
    );
    // Fix [8]: SECS.py tied the crush exponent to the resolution mode
    // (0.3/0.6/1.0). The crush only cuts narrow peaks, never fills dips, so
    // a strong exponent lowers the effective 100-400 Hz level; the edition
    // makes it a setting of its own (default 0.3).
    let crush_intensity = fixes
        .crush_intensity
        .unwrap_or_else(|| legacy_crush_intensity(config.resolution));
    let peak_crush_mag: Vec<f64> = (0..n)
        .map(|index| {
            let crushed = (1.0 / (sharp_peaks[index] + 1e-12)).powf(crush_intensity);
            let crush_weight = ((500.0 - f_abs[index]) / (500.0 - 300.0)).clamp(0.0, 1.0);
            (1.0 - crush_weight) + crushed * crush_weight
        })
        .collect();

    let tilt_enabled = apply_tilt && config.tilt_db_per_octave != 0.0;
    let tilt_profile = if tilt_enabled {
        Some(tilt_log_profile(f_abs))
    } else {
        None
    };
    let mut tilt_blend_mag = vec![1.0; n];
    let mut final_post_eq_mag: Vec<f64> = (0..n)
        .map(|index| macro_eq_mag[index] * peak_crush_mag[index])
        .collect();
    if let Some(profile) = &tilt_profile {
        for index in 0..n {
            let tilt_mag = 10f64.powf(config.tilt_db_per_octave * profile[index] / 20.0);
            tilt_blend_mag[index] = tilt_mag.powf(1.0 - precomputed.track_weight[index]);
            final_post_eq_mag[index] *= tilt_blend_mag[index];
        }
    }
    final_post_eq_mag[0] = final_post_eq_mag[1];
    let final_post_eq_h = minimum_phase_from_magnitude(&final_post_eq_mag);

    let mut product: Vec<C64> = initial_filter_h
        .iter()
        .zip(final_post_eq_h.iter())
        .map(|(a, b)| a * b)
        .collect();
    ifft_in_place(&mut product);
    let taps: Vec<f64> = product.iter().map(|value| value.re).collect();
    let target_mag_ideal: Vec<f64> = precomputed
        .target_mag_base
        .iter()
        .zip(tilt_blend_mag.iter())
        .map(|(base, blend)| base * blend)
        .collect();

    SecsChannelFilter {
        taps,
        target_mag_ideal,
        shared_band_guard_removed,
    }
}

/// Both channels' filters for one configuration, with the phase guard's
/// shared-sub-band verdict reconciled: below the bass-management crossover
/// one driver reproduces L+R, so if the guard stripped the excess correction
/// for either channel there, both are re-run with it stripped. Leaving them
/// to disagree splits the two filters' phase in exactly the band where the
/// amplifier sums them, which cancels mono bass at the subwoofer input and
/// is invisible to per-channel verification sweeps.
fn process_secs_pair(
    precomputed_left: &SecsChannelPrecompute,
    precomputed_right: &SecsChannelPrecompute,
    config: &SecsConfig,
    apply_tilt: bool,
    fixes: &EditedFixes,
) -> (SecsChannelFilter, SecsChannelFilter) {
    let left = process_secs_filter(precomputed_left, config, apply_tilt, false, fixes);
    let right = process_secs_filter(precomputed_right, config, apply_tilt, false, fixes);
    if !config.phase_guard
        || config.shared_low_frequency_hz.is_none()
        || left.shared_band_guard_removed == right.shared_band_guard_removed
    {
        return (left, right);
    }
    (
        process_secs_filter(precomputed_left, config, apply_tilt, true, fixes),
        process_secs_filter(precomputed_right, config, apply_tilt, true, fixes),
    )
}

/// Stereo SECS design: mirror of `FilterGeneratorWorker.run` for a stereo IR
/// pair, non-preview path, without resampling.
/// `magnitude_overrides`: optional multi-position extension (not in
/// SECS.py). `Some((left, right))` supplies spatially averaged magnitude
/// spectra on the impulse's full FFT grid; the magnitude correction then
/// targets the averaged response while the excess-phase correction and its
/// confidence guard stay strictly on this impulse pair.
/// Edition changes: every fix switch in `fixes` (the stereo-level ones are
/// [1] for the evaluation weights, [4] for the candidate list and [5] for
/// the crop). With every switch off this is
/// [`crate::secs::design_secs_stereo_filter`] bit for bit.
fn design_with_fixes(
    left_ir: &[f64],
    right_ir: &[f64],
    config: &SecsConfig,
    fixes: &EditedFixes,
    delay_candidates_ms: Option<&[f64]>,
    magnitude_overrides: Option<(&[f64], &[f64])>,
) -> DspResult<SecsStereoDesign> {
    if left_ir.is_empty() || right_ir.is_empty() {
        return Err(DspError::EmptyInput("SECS impulse response"));
    }
    if left_ir.len() != right_ir.len() {
        return Err(DspError::ShapeMismatch(format!(
            "SECS stereo impulse responses differ in length: {} vs {}",
            left_ir.len(),
            right_ir.len()
        )));
    }
    if config.taps == 0 {
        return Err(DspError::InvalidArgument(
            "SECS taps must be positive".to_string(),
        ));
    }
    if !config.maximum_delay_ms.is_finite()
        || !(SECS_AUTO_DELAY_MIN_MS..=SECS_MAXIMUM_DELAY_CEILING_MS)
            .contains(&config.maximum_delay_ms)
    {
        return Err(DspError::InvalidArgument(format!(
            "SECS maximum delay must be between {SECS_AUTO_DELAY_MIN_MS} and {SECS_MAXIMUM_DELAY_CEILING_MS} ms"
        )));
    }
    for (index, value) in left_ir.iter().chain(right_ir.iter()).enumerate() {
        if !value.is_finite() {
            return Err(DspError::NonFinite {
                context: "SECS impulse response",
                index,
            });
        }
    }
    if let Some((left_magnitude, right_magnitude)) = magnitude_overrides {
        if left_magnitude.len() != left_ir.len() || right_magnitude.len() != right_ir.len() {
            return Err(DspError::ShapeMismatch(format!(
                "SECS magnitude overrides must match the impulse FFT grid ({} bins): {} / {}",
                left_ir.len(),
                left_magnitude.len(),
                right_magnitude.len()
            )));
        }
        for (index, value) in left_magnitude
            .iter()
            .chain(right_magnitude.iter())
            .enumerate()
        {
            if !value.is_finite() || *value < 0.0 {
                return Err(DspError::NonFinite {
                    context: "SECS magnitude override",
                    index,
                });
            }
        }
    }

    let n = left_ir.len();
    let sample_rate_hz = f64::from(config.sample_rate_hz);
    let fft_len = n.max(config.taps);
    let freqs = fft_frequencies(fft_len, sample_rate_hz);
    let eval_indices: Vec<usize> = (0..fft_len)
        .filter(|index| freqs[*index] > 10.0 && freqs[*index] < sample_rate_hz / 2.0)
        .collect();
    let f_eval: Vec<f64> = eval_indices.iter().map(|index| freqs[*index]).collect();
    let weights = lr4_weights_5bands(fft_len, sample_rate_hz, fixes.normalized_weights);
    let orig_freqs = fft_frequencies(n, sample_rate_hz);
    let orig_indices: Vec<usize> = (0..n)
        .filter(|index| orig_freqs[*index] > 10.0 && orig_freqs[*index] < sample_rate_hz / 2.0)
        .collect();
    let f_orig: Vec<f64> = orig_indices
        .iter()
        .map(|index| orig_freqs[*index])
        .collect();

    // House-curve overlay, evaluated once on the impulse FFT grid that
    // `precompute_secs_channel` works on (this also validates the curve) and
    // re-anchored to 0 dB at the overlay anchor so an absolute-level custom
    // curve cannot shift the SECS level anchoring.
    let f_abs_orig = absolute_frequencies(&orig_freqs);
    let target_overlay_linear: Option<Vec<f64>> = match &config.target_overlay {
        Some(curve) => {
            let anchor_db = curve.level_at(SECS_TARGET_OVERLAY_ANCHOR_HZ)?;
            Some(
                curve
                    .evaluate(&f_abs_orig)?
                    .into_iter()
                    .map(|level_db| 10f64.powf((level_db - anchor_db) / 20.0))
                    .collect(),
            )
        }
        None => None,
    };

    // 2.1 shared-sub-band commonization, magnitude half (see the config
    // field comment): blend the effective L/R magnitude inputs toward their
    // energy mean below the crossover. Routing the result through the
    // existing magnitude-override path keeps the phase-confidence guard on
    // each channel's OWN magnitude, exactly like the multi-position average.
    let shared_magnitudes: Option<(Vec<f64>, Vec<f64>)> = match config.shared_low_frequency_hz {
        Some(crossover_hz) => {
            if !crossover_hz.is_finite() || crossover_hz <= 0.0 {
                return Err(DspError::InvalidArgument(
                    "SECS shared low-frequency crossover must be finite and positive".into(),
                ));
            }
            let own = |ir: &[f64]| -> Vec<f64> {
                fft_real(ir, n).iter().map(|value| value.norm()).collect()
            };
            let (base_left, base_right) = match magnitude_overrides {
                Some((left, right)) => (left.to_vec(), right.to_vec()),
                None => (own(left_ir), own(right_ir)),
            };
            let mut out_left = base_left.clone();
            let mut out_right = base_right.clone();
            for index in 0..n {
                let weight = shared_band_weight(f_abs_orig[index], crossover_hz);
                if weight > 0.0 {
                    let common = ((base_left[index] * base_left[index]
                        + base_right[index] * base_right[index])
                        * 0.5)
                        .sqrt();
                    out_left[index] = base_left[index] * (1.0 - weight) + common * weight;
                    out_right[index] = base_right[index] * (1.0 - weight) + common * weight;
                }
            }
            Some((out_left, out_right))
        }
        None => None,
    };
    let effective_magnitude_overrides: Option<(&[f64], &[f64])> = match &shared_magnitudes {
        Some((left, right)) => Some((left.as_slice(), right.as_slice())),
        None => magnitude_overrides,
    };

    let peak_left = argmax_abs(left_ir);
    let peak_right = argmax_abs(right_ir);
    let max_peak = peak_left.max(peak_right);
    let rms_window = (sample_rate_hz * 0.005) as usize;
    let windowed_rms = |signal: &[f64], peak: usize| -> f64 {
        let start = peak.saturating_sub(rms_window);
        let end = (peak + rms_window).min(signal.len());
        let slice = &signal[start..end];
        let mean_square = slice.iter().map(|v| v * v).sum::<f64>() / slice.len() as f64;
        (mean_square + 1e-12).sqrt()
    };
    let rms_scale = windowed_rms(left_ir, peak_left) / windowed_rms(right_ir, peak_right);

    let h_ir_left = fft_real(left_ir, fft_len);
    let h_ir_right = fft_real(right_ir, fft_len);
    let mut precomputed_left = precompute_secs_channel(
        left_ir,
        config,
        effective_magnitude_overrides.map(|(left, _)| left),
        target_overlay_linear.as_deref(),
        fixes,
    );
    let mut precomputed_right = precompute_secs_channel(
        right_ir,
        config,
        effective_magnitude_overrides.map(|(_, right)| right),
        target_overlay_linear.as_deref(),
        fixes,
    );

    // Transplanted excess (see the config field): the reference design's
    // corrector, interpolated onto this grid, replaces both channels' own
    // excess analysis. It is post-commonization on the reference, so the
    // commonization below is skipped - re-blending would double-apply the
    // crossfade region.
    if let Some(transplant) = config.excess_phase_transplant.as_ref() {
        if !config.low_latency && !config.zero_latency {
            precomputed_left.excess_inv_ir = excess_ir_from_phase_export(
                &transplant.frequencies_hz,
                &transplant.left_phase_rad,
                n,
                sample_rate_hz,
            );
            precomputed_right.excess_inv_ir = excess_ir_from_phase_export(
                &transplant.frequencies_hz,
                &transplant.right_phase_rad,
                n,
                sample_rate_hz,
            );
            debug_band_gd_ms(
                "3T transplanted L",
                &fft_real(&precomputed_left.excess_inv_ir, n),
                sample_rate_hz,
                20.0,
                100.0,
            );
        }
    }
    // 2.1 shared-sub-band commonization, phase half: below the crossover
    // both captures measured the same driver, so the excess-phase correctors
    // are blended toward their normalized complex mean (both are referenced
    // to each channel's own direct-sound peak, so the mean is meaningful).
    // This removes noise-chasing L/R phase divergence AND keeps the two
    // filters phase-matched where bass management sums the channels into
    // the subwoofer.
    if let Some(crossover_hz) = config
        .shared_low_frequency_hz
        .filter(|_| config.excess_phase_transplant.is_none())
    {
        let mut spectrum_left = fft_real(&precomputed_left.excess_inv_ir, n);
        let mut spectrum_right = fft_real(&precomputed_right.excess_inv_ir, n);
        for index in 0..n {
            let weight = shared_band_weight(f_abs_orig[index], crossover_hz);
            if weight > 0.0 {
                let mean = (spectrum_left[index] + spectrum_right[index]) * 0.5;
                let common = mean / (mean.norm() + 1e-12);
                let blend = |value: C64| -> C64 {
                    let mixed = value * (1.0 - weight) + common * weight;
                    mixed / (mixed.norm() + 1e-12)
                };
                spectrum_left[index] = blend(spectrum_left[index]);
                spectrum_right[index] = blend(spectrum_right[index]);
            }
        }
        ifft_in_place(&mut spectrum_left);
        ifft_in_place(&mut spectrum_right);
        precomputed_left.excess_inv_ir = spectrum_left.iter().map(|value| value.re).collect();
        precomputed_right.excess_inv_ir = spectrum_right.iter().map(|value| value.re).collect();
    }

    // Fix [4]: without explicit candidates the original searched 2..=10 ms
    // on L+R magnitude alone, blind to the low-band phase the delay exists
    // to correct. `Ceiling` runs the design at the caller's budget instead
    // (see `SecsEditedDelayChoice`). Explicit candidates and the low/zero
    // latency paths are unchanged.
    let automatic_candidates = || -> Vec<f64> {
        match fixes.delay_choice {
            SecsEditedDelayChoice::Ceiling => vec![config.maximum_delay_ms],
            SecsEditedDelayChoice::MagnitudeSearch => coarse_delay_grid(),
        }
    };
    let stage1: Vec<f64> = if config.low_latency || config.zero_latency {
        vec![0.0]
    } else if let Some(candidates) = delay_candidates_ms {
        let filtered: Vec<f64> = candidates
            .iter()
            .filter(|delay| delay.is_finite())
            .map(|delay| delay.clamp(SECS_AUTO_DELAY_MIN_MS, config.maximum_delay_ms))
            .collect();
        if filtered.is_empty() {
            automatic_candidates()
        } else {
            filtered
        }
    } else {
        automatic_candidates()
    };

    let mut target_db_cache: Option<Vec<f64>> = None;
    let mut best: Option<DelayEvaluation> = None;
    let mut delay_metrics: Vec<Option<f64>> = Vec::with_capacity(stage1.len());

    for delay_ms in &stage1 {
        let mut candidate_config = config.clone();
        candidate_config.target_delay_ms = *delay_ms;
        let target_delay_samples =
            ((candidate_config.target_delay_ms / 1000.0) * sample_rate_hz) as usize;

        let (left_filter, right_filter) = process_secs_pair(
            &precomputed_left,
            &precomputed_right,
            &candidate_config,
            false,
            fixes,
        );
        let mut final_left = left_filter.taps;
        let mut final_right: Vec<f64> = right_filter
            .taps
            .iter()
            .map(|tap| tap * rms_scale)
            .collect();
        if !candidate_config.zero_latency {
            if max_peak > peak_left {
                final_left = shift_right_zero(&final_left, max_peak - peak_left);
            }
            if max_peak > peak_right {
                final_right = shift_right_zero(&final_right, max_peak - peak_right);
            }
        }
        if final_left
            .iter()
            .chain(final_right.iter())
            .any(|value| !value.is_finite())
        {
            delay_metrics.push(None);
            continue;
        }
        let low_cutoff_metric =
            0.5 * (precomputed_left.low_cutoff_hz + precomputed_right.low_cutoff_hz);
        let high_cutoff_metric =
            0.5 * (precomputed_left.high_cutoff_hz + precomputed_right.high_cutoff_hz);

        let start = crop_start(
            &final_left,
            &final_right,
            target_delay_samples,
            fixes.earliest_tap_crop,
        );
        let crop_left = crop_from(&final_left, candidate_config.taps, start);
        let crop_right = crop_from(&final_right, candidate_config.taps, start);

        let target_db = target_db_cache.get_or_insert_with(|| {
            let target_linear: Vec<f64> = orig_indices
                .iter()
                .map(|index| left_filter.target_mag_ideal[*index])
                .collect();
            f_eval
                .iter()
                .map(|f| 20.0 * (interp_clamped(*f, &f_orig, &target_linear) + 1e-12).log10())
                .collect()
        });

        let low_match_hz = low_cutoff_metric.max(10.0);
        let match_positions: Vec<usize> = f_eval
            .iter()
            .enumerate()
            .filter(|(_, f)| **f <= 300.0 && **f >= low_match_hz)
            .map(|(position, _)| position)
            .collect();
        if match_positions.is_empty() {
            delay_metrics.push(None);
            continue;
        }
        let lf_span = (300.0 - low_match_hz).max(1e-9);
        let lf_weights: Vec<f64> = match_positions
            .iter()
            .map(|position| {
                let proximity = ((300.0 - f_eval[*position]) / lf_span).clamp(0.0, 1.0);
                1.0 + (SECS_AUTO_DELAY_WEIGHT_GAIN - 1.0) * proximity
            })
            .collect();
        let lf_weight_sum = lf_weights.iter().sum::<f64>() + 1e-12;

        let shift = sample_shift_phase(
            &freqs,
            sample_rate_hz,
            (max_peak + target_delay_samples) as f64,
        );
        let h_left = fft_real(&crop_left, fft_len);
        let h_right = fft_real(&crop_right, fft_len);
        let system_sum_mag: Vec<f64> = (0..fft_len)
            .map(|index| {
                let left = h_ir_left[index] * h_left[index] * shift[index];
                let right = h_ir_right[index] * h_right[index] * shift[index];
                (left + right).norm()
            })
            .collect();
        let sum_smoothed = apply_5band_smoothing_real(
            &system_sum_mag,
            sample_rate_hz,
            &weights,
            candidate_config.resolution,
        );
        let errors: Vec<f64> = match_positions
            .iter()
            .map(|position| {
                let after_db = 20.0 * (sum_smoothed[eval_indices[*position]] + 1e-12).log10();
                after_db - (target_db[*position] + 6.0)
            })
            .collect();
        let absolute_mae = lf_weights
            .iter()
            .zip(errors.iter())
            .map(|(weight, error)| weight * error.abs())
            .sum::<f64>()
            / lf_weight_sum;
        let weighted_mean = lf_weights
            .iter()
            .zip(errors.iter())
            .map(|(weight, error)| weight * error)
            .sum::<f64>()
            / lf_weight_sum;
        let shape_mae = lf_weights
            .iter()
            .zip(errors.iter())
            .map(|(weight, error)| weight * (error - weighted_mean).abs())
            .sum::<f64>()
            / lf_weight_sum;
        let metric = absolute_mae + 0.35 * shape_mae;
        delay_metrics.push(Some(metric));

        let is_better = best
            .as_ref()
            .map(|current| metric < current.metric)
            .unwrap_or(true);
        if is_better {
            best = Some(DelayEvaluation {
                metric,
                crop_left,
                crop_right,
                target_mag_left: left_filter.target_mag_ideal,
                low_cutoff_hz: low_cutoff_metric,
                high_cutoff_hz: high_cutoff_metric,
                target_delay_samples,
                delay_ms: *delay_ms,
            });
        }
    }

    let mut winner = best.ok_or_else(|| {
        DspError::InvalidArgument("SECS automatic delay search failed".to_string())
    })?;

    // Finalize: the search ran without tilt; regenerate with tilt when set.
    let tilt_enabled = config.tilt_db_per_octave.abs() > 1e-12;
    if tilt_enabled {
        let mut final_config = config.clone();
        final_config.target_delay_ms = winner.delay_ms;
        let (left_filter, right_filter) = process_secs_pair(
            &precomputed_left,
            &precomputed_right,
            &final_config,
            true,
            fixes,
        );
        winner.low_cutoff_hz =
            0.5 * (precomputed_left.low_cutoff_hz + precomputed_right.low_cutoff_hz);
        winner.high_cutoff_hz =
            0.5 * (precomputed_left.high_cutoff_hz + precomputed_right.high_cutoff_hz);
        let mut final_left = left_filter.taps;
        let mut final_right: Vec<f64> = right_filter
            .taps
            .iter()
            .map(|tap| tap * rms_scale)
            .collect();
        if !final_config.zero_latency {
            if max_peak > peak_left {
                final_left = shift_right_zero(&final_left, max_peak - peak_left);
            }
            if max_peak > peak_right {
                final_right = shift_right_zero(&final_right, max_peak - peak_right);
            }
        }
        let start = crop_start(
            &final_left,
            &final_right,
            winner.target_delay_samples,
            fixes.earliest_tap_crop,
        );
        winner.crop_left = crop_from(&final_left, final_config.taps, start);
        winner.crop_right = crop_from(&final_right, final_config.taps, start);
        winner.target_mag_left = left_filter.target_mag_ideal;
    }

    let shift_common = sample_shift_phase(
        &freqs,
        sample_rate_hz,
        (max_peak + winner.target_delay_samples) as f64,
    );
    let shift_left_only = sample_shift_phase(&freqs, sample_rate_hz, peak_left as f64);
    let shift_right_only = sample_shift_phase(&freqs, sample_rate_hz, peak_right as f64);
    let (shift_left, shift_right) = if config.zero_latency {
        (&shift_left_only, &shift_right_only)
    } else {
        (&shift_common, &shift_common)
    };

    let mut crop_left = winner.crop_left.clone();
    let mut crop_right = winner.crop_right.clone();

    // Channel balance trim on the smoothed 20 Hz - 1 kHz system responses.
    let h_left = fft_real(&crop_left, fft_len);
    let h_right = fft_real(&crop_right, fft_len);
    let balance_indices: Vec<usize> = (0..fft_len)
        .filter(|index| {
            freqs[*index] > SECS_CHANNEL_BALANCE_BAND_LOW_HZ
                && freqs[*index] < SECS_CHANNEL_BALANCE_BAND_HIGH_HZ.min(sample_rate_hz / 2.0 - 1.0)
        })
        .collect();
    let mut channel_balance_trim_db = 0.0;
    if !balance_indices.is_empty() {
        let system_mag = |h_ir: &[C64], h_fil: &[C64], shift: &[C64]| -> Vec<f64> {
            (0..fft_len)
                .map(|index| (h_ir[index] * h_fil[index] * shift[index]).norm())
                .collect()
        };
        let left_smoothed = apply_5band_smoothing_real(
            &system_mag(&h_ir_left, &h_left, shift_left),
            sample_rate_hz,
            &weights,
            config.resolution,
        );
        let right_smoothed = apply_5band_smoothing_real(
            &system_mag(&h_ir_right, &h_right, shift_right),
            sample_rate_hz,
            &weights,
            config.resolution,
        );
        let differences: Vec<f64> = balance_indices
            .iter()
            .map(|index| {
                20.0 * (left_smoothed[*index] + 1e-12).log10()
                    - 20.0 * (right_smoothed[*index] + 1e-12).log10()
            })
            .collect();
        channel_balance_trim_db = median_value(&differences).clamp(
            -SECS_CHANNEL_BALANCE_MAX_TRIM_DB,
            SECS_CHANNEL_BALANCE_MAX_TRIM_DB,
        );
        if channel_balance_trim_db.abs() > 0.01 {
            let gain = 10f64.powf(channel_balance_trim_db / 20.0);
            for tap in crop_right.iter_mut() {
                *tap *= gain;
            }
        }
    }

    // Squared-linspace fade on the last 5 ms.
    let fade_length = ((sample_rate_hz * 0.005) as usize).min(config.taps);
    if fade_length > 1 {
        for offset in 0..fade_length {
            let fade = 1.0 - offset as f64 / (fade_length - 1) as f64;
            let gain = fade * fade;
            let index = config.taps - fade_length + offset;
            crop_left[index] *= gain;
            crop_right[index] *= gain;
        }
    }

    // Normalize so the peak spectral magnitude across both channels is <= 1.
    let fft_eval_len = config.taps.max(8192);
    let mut preamp_db = 0.0;
    let max_magnitude = fft_real(&crop_left, fft_eval_len)
        .iter()
        .chain(fft_real(&crop_right, fft_eval_len).iter())
        .map(|value| value.norm())
        .fold(f64::NEG_INFINITY, f64::max);
    if max_magnitude > 1.0 {
        for tap in crop_left.iter_mut().chain(crop_right.iter_mut()) {
            *tap /= max_magnitude;
        }
        preamp_db = -20.0 * max_magnitude.log10();
    }

    // Fix [9]: same number, honest name.
    let left_score = secs_decay_score(left_ir, config.sample_rate_hz);
    let right_score = secs_decay_score(right_ir, config.sample_rate_hz);

    let target_linear: Vec<f64> = orig_indices
        .iter()
        .map(|index| winner.target_mag_left[*index])
        .collect();

    // Export the applied excess corrector for sibling-rate transplantation.
    // On the minimum-phase-only paths there is none to export.
    let excess_phase = (!config.low_latency && !config.zero_latency).then(|| {
        let bin_hz = sample_rate_hz / n as f64;
        SecsExcessPhaseExport {
            frequencies_hz: (1..=n / 2).map(|index| index as f64 * bin_hz).collect(),
            left_phase_rad: unwrap_positive_phase(&fft_real(&precomputed_left.excess_inv_ir, n)),
            right_phase_rad: unwrap_positive_phase(&fft_real(&precomputed_right.excess_inv_ir, n)),
        }
    });

    Ok(SecsStereoDesign {
        sample_rate_hz: config.sample_rate_hz,
        left_taps: crop_left,
        right_taps: crop_right,
        preamp_db,
        auto_delay_ms: winner.delay_ms,
        low_cutoff_hz: winner.low_cutoff_hz,
        high_cutoff_hz: winner.high_cutoff_hz,
        rms_scale,
        channel_balance_trim_db,
        input_phase_score: 0.5 * (left_score + right_score),
        channels: [
            SecsChannelSummary {
                peak_index: peak_left,
                low_cutoff_hz: precomputed_left.low_cutoff_hz,
                high_cutoff_hz: precomputed_left.high_cutoff_hz,
                reference_magnitude: precomputed_left.ref_mag,
            },
            SecsChannelSummary {
                peak_index: peak_right,
                low_cutoff_hz: precomputed_right.low_cutoff_hz,
                high_cutoff_hz: precomputed_right.high_cutoff_hz,
                reference_magnitude: precomputed_right.ref_mag,
            },
        ],
        delay_candidates_ms: stage1,
        delay_metrics,
        target_frequencies_hz: f_orig,
        target_magnitude: target_linear,
        excess_phase,
    })
}

/// First filter sample both channels are cropped from, so that the main tap
/// lands `target_delay_samples` into the crop.
///
/// Fix [5] (`earliest`): the original anchored the crop on the larger of
/// the two channels' peaks after the arrival-alignment shift. The alignment
/// delays the channel that arrived first and leaves the later-arriving one
/// unshifted, so the later channel's main tap sits *earlier* in its buffer
/// than the shifted channel's and was cut off whenever the arrival difference
/// exceeded the target delay (measured: 3 ms arrival difference at 2 ms
/// delay left the right filter at -31.8 dB energy; in low-latency mode the
/// target delay is 0, so any arrival difference lost a main tap). Anchoring
/// on the earliest main tap keeps both, with their relative delay intact; a
/// too-short tap budget then trims the end, never the front.
fn crop_start(
    final_left: &[f64],
    final_right: &[f64],
    target_delay_samples: usize,
    earliest: bool,
) -> isize {
    let reference = if earliest {
        argmax_abs(final_left).min(argmax_abs(final_right))
    } else {
        let mut peak_reference = 0;
        let mut peak_reference_value = f64::NEG_INFINITY;
        for index in 0..final_left.len() {
            let combined = final_left[index].abs().max(final_right[index].abs());
            if combined > peak_reference_value {
                peak_reference_value = combined;
                peak_reference = index;
            }
        }
        peak_reference
    };
    reference as isize - target_delay_samples as isize
}

/// `taps` samples of `signal` starting at `start` (zero-filled where `start`
/// is negative or the window runs past the end). Same arithmetic as the
/// original `_crop_peak_left`, with the start computed by the caller.
fn crop_from(signal: &[f64], taps: usize, start: isize) -> Vec<f64> {
    let mut cropped = vec![0.0; taps];
    let source_start = start.max(0) as usize;
    let source_end = ((start + taps as isize).min(signal.len() as isize)).max(0) as usize;
    if source_end > source_start {
        let destination_start = (source_start as isize - start) as usize;
        let destination_end = destination_start + (source_end - source_start);
        cropped[destination_start..destination_end]
            .copy_from_slice(&signal[source_start..source_end]);
    }
    cropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secs::design_secs_stereo_filter;
    use crate::target::{TargetCurve, TargetKnot, TargetPreset};

    /// Every fix off: the original port.
    const ORIGINAL: EditedFixes = EditedFixes {
        normalized_weights: false,
        cut_only_macro: false,
        correction_upper_hz: None,
        delay_choice: SecsEditedDelayChoice::MagnitudeSearch,
        crush_intensity: None,
        earliest_tap_crop: false,
        hf_target_follow: false,
    };

    fn test_config() -> SecsConfig {
        SecsConfig {
            sample_rate_hz: 48_000,
            max_boost_db: 6.0,
            target_delay_ms: SECS_AUTO_DELAY_MIN_MS,
            tilt_db_per_octave: 0.0,
            bass_boost_db: 0.0,
            bass_frequency_hz: 60.0,
            resolution: SecsResolutionMode::Balanced,
            taps: 4_096,
            hf_min_phase_reference_hz: 300.0,
            low_latency: false,
            zero_latency: false,
            target_overlay: None,
            excess_phase_transplant: None,
            shared_low_frequency_hz: None,
            phase_guard: false,
            maximum_delay_ms: SECS_AUTO_DELAY_MAX_MS,
        }
    }

    /// Same synthetic room as the secs.rs tests: direct sound, two early
    /// reflections and a decaying 52 Hz mode.
    fn simple_room_ir(n: usize, direct_index: usize, gain: f64) -> Vec<f64> {
        let mut ir = vec![0.0; n];
        ir[direct_index] = gain;
        ir[direct_index + 120] = -0.35 * gain;
        ir[direct_index + 310] = 0.2 * gain;
        let sample_rate = 48_000.0;
        for offset in 0..(n - direct_index) {
            let t = offset as f64 / sample_rate;
            ir[direct_index + offset] +=
                gain * 9.0e-4 * (-t / 0.2).exp() * (2.0 * std::f64::consts::PI * 52.0 * t).sin();
        }
        ir
    }

    fn delta(n: usize, at: usize, gain: f64) -> Vec<f64> {
        let mut ir = vec![0.0; n];
        ir[at] = gain;
        ir
    }

    fn own_magnitude(ir: &[f64]) -> Vec<f64> {
        fft_real(ir, ir.len()).iter().map(|v| v.norm()).collect()
    }

    /// Bit-exact comparison (f64 `==`, no tolerance) of the fields the
    /// equivalence guard pins, then of the whole design.
    fn assert_bit_exact(label: &str, edited: &SecsStereoDesign, original: &SecsStereoDesign) {
        assert!(edited.left_taps == original.left_taps, "{label}: left_taps");
        assert!(
            edited.right_taps == original.right_taps,
            "{label}: right_taps"
        );
        assert!(edited.preamp_db == original.preamp_db, "{label}: preamp_db");
        assert!(
            edited.auto_delay_ms == original.auto_delay_ms,
            "{label}: auto_delay_ms"
        );
        assert!(
            edited.delay_metrics == original.delay_metrics,
            "{label}: delay_metrics"
        );
        assert!(
            edited.target_magnitude == original.target_magnitude,
            "{label}: target_magnitude"
        );
        assert!(
            edited.channel_balance_trim_db == original.channel_balance_trim_db,
            "{label}: channel_balance_trim_db"
        );
        assert!(edited.rms_scale == original.rms_scale, "{label}: rms_scale");
        assert!(edited == original, "{label}: remaining design fields");
    }

    fn check_equivalence(
        label: &str,
        left: &[f64],
        right: &[f64],
        config: &SecsConfig,
        candidates: Option<&[f64]>,
        overrides: Option<(&[f64], &[f64])>,
    ) {
        let original = design_secs_stereo_filter(left, right, config, candidates, overrides)
            .unwrap_or_else(|error| panic!("{label}: original failed: {error}"));
        let edited = design_with_fixes(left, right, config, &ORIGINAL, candidates, overrides)
            .unwrap_or_else(|error| panic!("{label}: edition failed: {error}"));
        assert_bit_exact(label, &edited, &original);
    }

    #[test]
    fn with_every_fix_off_the_edition_is_the_original_bit_for_bit() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let base = test_config();

        check_equivalence("normal auto search", &left, &right, &base, None, None);

        let mut guarded = base.clone();
        guarded.phase_guard = true;
        check_equivalence("phase guard on", &left, &right, &guarded, None, None);

        let mut tilted = guarded.clone();
        tilted.tilt_db_per_octave = -0.8;
        tilted.bass_boost_db = 4.0;
        check_equivalence("tilt + bass shelf", &left, &right, &tilted, None, None);

        let mut shared = guarded.clone();
        shared.shared_low_frequency_hz = Some(80.0);
        check_equivalence("2.1 shared sub band", &left, &right, &shared, None, None);

        let diluted = |ir: &[f64]| -> Vec<f64> {
            let n = ir.len();
            let freqs = fft_frequencies(n, 48_000.0);
            fft_real(ir, n)
                .iter()
                .zip(freqs.iter())
                .map(|(value, hz)| {
                    let scale = if (40.0..=70.0).contains(&hz.abs()) {
                        0.5
                    } else {
                        1.0
                    };
                    value.norm() * scale
                })
                .collect()
        };
        let (left_magnitude, right_magnitude) = (diluted(&left), diluted(&right));
        check_equivalence(
            "magnitude override",
            &left,
            &right,
            &base,
            None,
            Some((&left_magnitude, &right_magnitude)),
        );

        let mut overlaid = base.clone();
        overlaid.target_overlay = Some(TargetCurve::preset(TargetPreset::HarmanStyle));
        check_equivalence("target overlay", &left, &right, &overlaid, None, None);

        let mut low_latency = base.clone();
        low_latency.low_latency = true;
        check_equivalence("low latency", &left, &right, &low_latency, None, None);

        let mut zero_latency = base.clone();
        zero_latency.zero_latency = true;
        check_equivalence("zero latency", &left, &right, &zero_latency, None, None);

        let mut extended = guarded.clone();
        extended.maximum_delay_ms = 60.0;
        check_equivalence(
            "extended 60 ms budget",
            &left,
            &right,
            &extended,
            Some(&[60.0]),
            None,
        );

        // A transplanted excess from a sibling design.
        let donor = design_secs_stereo_filter(&left, &right, &extended, Some(&[60.0]), None)
            .expect("donor design");
        let mut transplanted = extended.clone();
        transplanted.excess_phase_transplant = donor.excess_phase;
        check_equivalence(
            "excess transplant",
            &left,
            &right,
            &transplanted,
            Some(&[60.0]),
            None,
        );

        // Arrival difference larger than the delay: the original crop cuts
        // the early channel; the edition must reproduce even that.
        let late_right = simple_room_ir(8_192, 400, 0.9);
        check_equivalence(
            "L/R peaks differ",
            &left,
            &late_right,
            &base,
            Some(&[2.0]),
            None,
        );
        check_equivalence(
            "L/R peaks differ, auto",
            &left,
            &late_right,
            &base,
            None,
            None,
        );
    }

    #[test]
    fn single_fixes_alone_match_the_original_where_they_must() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let config = test_config();
        let original = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();

        // [8] alone at the Balanced legacy exponent is the original.
        let crush = EditedFixes {
            crush_intensity: Some(0.6),
            ..ORIGINAL
        };
        let edited = design_with_fixes(&left, &right, &config, &crush, None, None).unwrap();
        assert_bit_exact("[8] at 0.6 on Balanced", &edited, &original);
        // ... and any other exponent is not.
        let crush = EditedFixes {
            crush_intensity: Some(0.3),
            ..ORIGINAL
        };
        let edited = design_with_fixes(&left, &right, &config, &crush, None, None).unwrap();
        assert_ne!(edited.left_taps, original.left_taps);

        // [4] alone at a 10 ms budget is the original pinned to 10 ms.
        let ceiling = EditedFixes {
            delay_choice: SecsEditedDelayChoice::Ceiling,
            ..ORIGINAL
        };
        let pinned =
            design_secs_stereo_filter(&left, &right, &config, Some(&[10.0]), None).unwrap();
        let edited = design_with_fixes(&left, &right, &config, &ceiling, None, None).unwrap();
        assert_bit_exact("[4] Ceiling at 10 ms", &edited, &pinned);
        assert_eq!(edited.delay_candidates_ms, vec![10.0]);

        // [5] alone on identical channels (one common peak) is the original.
        let earliest = EditedFixes {
            earliest_tap_crop: true,
            ..ORIGINAL
        };
        let same = design_secs_stereo_filter(&left, &left, &config, None, None).unwrap();
        let edited = design_with_fixes(&left, &left, &config, &earliest, None, None).unwrap();
        assert_bit_exact("[5] identical channels", &edited, &same);

        // [1], [2] and [3] alone each change the design.
        for (label, fixes) in [
            (
                "[1]",
                EditedFixes {
                    normalized_weights: true,
                    ..ORIGINAL
                },
            ),
            (
                "[2]",
                EditedFixes {
                    cut_only_macro: true,
                    ..ORIGINAL
                },
            ),
            (
                "[3]",
                EditedFixes {
                    correction_upper_hz: Some(500.0),
                    ..ORIGINAL
                },
            ),
        ] {
            let edited = design_with_fixes(&left, &right, &config, &fixes, None, None).unwrap();
            assert_ne!(
                edited.left_taps, original.left_taps,
                "{label} changed nothing"
            );
        }
    }

    #[test]
    fn the_correction_upper_limit_off_value_is_the_legacy_behavior() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let config = test_config();
        let options = SecsEditedOptions {
            correction_upper_hz: SECS_EDITED_CORRECTION_UPPER_OFF_HZ,
            ..SecsEditedOptions::default()
        };
        let public =
            design_secs_stereo_filter_edited(&left, &right, &config, &options, None, None).unwrap();
        let internal = design_with_fixes(
            &left,
            &right,
            &config,
            &EditedFixes {
                correction_upper_hz: None,
                ..EditedFixes::from_options(&SecsEditedOptions::default())
            },
            None,
            None,
        )
        .unwrap();
        assert_bit_exact("upper limit off", &public, &internal);
    }

    #[test]
    fn the_default_options_are_the_documented_values() {
        let options = SecsEditedOptions::default();
        assert_eq!(options.correction_upper_hz, 500.0);
        assert_eq!(options.delay_choice, SecsEditedDelayChoice::Ceiling);
        assert_eq!(options.crush_intensity, 0.3);
        let fixes = EditedFixes::from_options(&options);
        assert!(fixes.normalized_weights && fixes.cut_only_macro && fixes.earliest_tap_crop);
        assert_eq!(fixes.correction_upper_hz, Some(500.0));
        assert_eq!(fixes.crush_intensity, Some(0.3));
        assert!(options.hf_target_follow && fixes.hf_target_follow);
    }

    /// Magnitude of a filter in dB on a dense grid at `hz`, as a 1/3-octave
    /// power average (independent of the SECS smoothing).
    fn third_octave_db(taps: &[f64], hz: f64) -> f64 {
        let n = 131_072;
        let spectrum = fft_real(taps, n);
        let bin_hz = 48_000.0 / n as f64;
        let edge = 2f64.powf(1.0 / 6.0);
        let (mut sum, mut count) = (0.0, 0usize);
        for (index, value) in spectrum.iter().enumerate().take(n / 2).skip(1) {
            let frequency = index as f64 * bin_hz;
            if frequency >= hz / edge && frequency <= hz * edge {
                sum += value.norm_sqr();
                count += 1;
            }
        }
        10.0 * (sum / count as f64).log10()
    }

    #[test]
    fn hf_target_follow_applies_a_house_curve_rolloff_without_touching_the_bass() {
        // A flat in-room treble (delta direct sound plus bass room gain) and a
        // house curve falling 1 dB/oct above 500 Hz: following the target
        // must cut the treble along the curve; fading the macro EQ out must
        // leave it flat; neither may change anything below the upper limit.
        let mut ir = vec![0.0; 16_384];
        ir[200] = 1.0;
        for index in 0..16_384 - 200 {
            let t = index as f64 / 48_000.0;
            ir[200 + index] +=
                0.02 * (2.0 * std::f64::consts::PI * 55.0 * t).sin() * (-t / 0.15).exp();
        }
        let mut config = test_config();
        config.target_overlay = Some(
            TargetCurve::from_knots(
                "rolloff test curve",
                "test-v1",
                vec![
                    TargetKnot {
                        frequency_hz: 20.0,
                        level_db: 0.0,
                    },
                    TargetKnot {
                        frequency_hz: 500.0,
                        level_db: 0.0,
                    },
                    TargetKnot {
                        frequency_hz: 8_000.0,
                        level_db: -4.0,
                    },
                    TargetKnot {
                        frequency_hz: 20_000.0,
                        level_db: -4.0,
                    },
                ],
            )
            .unwrap(),
        );
        let follow = SecsEditedOptions::default();
        let fade = SecsEditedOptions {
            hf_target_follow: false,
            ..follow
        };
        let unnormalized = |options: &SecsEditedOptions| -> Vec<f64> {
            let design =
                design_secs_stereo_filter_edited(&ir, &ir, &config, options, None, None).unwrap();
            let gain = 10f64.powf(-design.preamp_db / 20.0);
            design.left_taps.iter().map(|tap| tap * gain).collect()
        };
        let followed = unnormalized(&follow);
        let faded = unnormalized(&fade);
        let rel = |taps: &[f64], hz: f64| third_octave_db(taps, hz) - third_octave_db(taps, 500.0);
        // Following: 8 kHz sits about 4 dB below 500 Hz, as the curve asks.
        assert!(
            (rel(&followed, 8_000.0) + 4.0).abs() < 1.0,
            "followed 8 kHz {:.2} dB",
            rel(&followed, 8_000.0)
        );
        assert!(rel(&followed, 2_000.0) < -1.0);
        // Fading: the treble is left alone.
        assert!(
            rel(&faded, 8_000.0).abs() < 0.3,
            "faded 8 kHz {:.2}",
            rel(&faded, 8_000.0)
        );
        // Below the upper limit both are the same correction.
        for hz in [40.0, 55.0, 80.0, 125.0, 200.0, 315.0] {
            let difference = third_octave_db(&followed, hz) - third_octave_db(&faded, hz);
            assert!(
                difference.abs() < 0.05,
                "{hz} Hz differs by {difference:.3} dB"
            );
        }
        // It only ever cuts: no band above 500 Hz rises over its faded twin.
        for hz in [630.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0] {
            assert!(third_octave_db(&followed, hz) <= third_octave_db(&faded, hz) + 0.05);
        }
    }

    #[test]
    fn hf_target_follow_does_not_cut_treble_already_below_the_target_twice() {
        // The direct sound rolls off above ~14 kHz (2nd-order low-pass), so
        // the top octave falls inside SECS's natural-rolloff zone and sits
        // below a house curve that ends 4 dB down. Following must still cut
        // 8 kHz to the curve, but must not stack the curve's rolloff on top
        // of the already-falling 16-20 kHz (it used to remove ~4 dB more).
        let n = 16_384;
        let (b, a) = {
            let w0 = 2.0 * std::f64::consts::PI * 14_000.0 / 48_000.0;
            let alpha = w0.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
            let a0 = 1.0 + alpha;
            (
                [
                    (1.0 - w0.cos()) / 2.0 / a0,
                    (1.0 - w0.cos()) / a0,
                    (1.0 - w0.cos()) / 2.0 / a0,
                ],
                [-2.0 * w0.cos() / a0, (1.0 - alpha) / a0],
            )
        };
        let mut ir = vec![0.0; n];
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for (index, slot) in ir.iter_mut().enumerate().skip(200) {
            let x = if index == 200 { 1.0 } else { 0.0 };
            let y = b[0] * x + b[1] * x1 + b[2] * x2 - a[0] * y1 - a[1] * y2;
            (x2, x1, y2, y1) = (x1, x, y1, y);
            let t = (index - 200) as f64 / 48_000.0;
            *slot = y + 0.02 * (2.0 * std::f64::consts::PI * 55.0 * t).sin() * (-t / 0.15).exp();
        }
        let mut config = test_config();
        config.target_overlay = Some(
            TargetCurve::from_knots(
                "rolloff test curve",
                "test-v1",
                vec![
                    TargetKnot {
                        frequency_hz: 20.0,
                        level_db: 0.0,
                    },
                    TargetKnot {
                        frequency_hz: 500.0,
                        level_db: 0.0,
                    },
                    TargetKnot {
                        frequency_hz: 8_000.0,
                        level_db: -4.0,
                    },
                    TargetKnot {
                        frequency_hz: 20_000.0,
                        level_db: -4.0,
                    },
                ],
            )
            .unwrap(),
        );
        let design = design_secs_stereo_filter_edited(
            &ir,
            &ir,
            &config,
            &SecsEditedOptions::default(),
            None,
            None,
        )
        .unwrap();
        let fft_n = 131_072;
        let room = fft_real(&ir, fft_n);
        let filter = fft_real(&design.left_taps, fft_n);
        let band_db = |weighting: Option<&[C64]>, hz: f64| -> f64 {
            let bin_hz = 48_000.0 / fft_n as f64;
            let edge = 2f64.powf(1.0 / 6.0);
            let (mut sum, mut count) = (0.0, 0usize);
            for index in 1..fft_n / 2 {
                let frequency = index as f64 * bin_hz;
                if frequency >= hz / edge && frequency <= hz * edge {
                    let gain = weighting.map_or(1.0, |w| w[index].norm_sqr());
                    sum += room[index].norm_sqr() * gain;
                    count += 1;
                }
            }
            10.0 * (sum / count as f64).log10()
        };
        let rel =
            |weighting: Option<&[C64]>, hz: f64| band_db(weighting, hz) - band_db(weighting, 500.0);
        let realized_8k = rel(Some(&filter), 8_000.0);
        assert!(
            (realized_8k + 4.0).abs() < 1.0,
            "realized 8 kHz {realized_8k:.2} dB"
        );
        for hz in [16_000.0, 20_000.0] {
            let natural = rel(None, hz);
            let realized = rel(Some(&filter), hz);
            assert!(natural < -4.0, "{hz} Hz natural {natural:.2} dB");
            assert!(
                realized - natural > -2.0,
                "{hz} Hz natural {natural:.2} dB cut to {realized:.2} dB"
            );
        }
    }

    #[test]
    fn invalid_options_are_rejected() {
        let ir = simple_room_ir(8_192, 90, 1.0);
        let config = test_config();
        let invalid = [
            SecsEditedOptions {
                correction_upper_hz: 199.0,
                ..SecsEditedOptions::default()
            },
            SecsEditedOptions {
                correction_upper_hz: f64::NAN,
                ..SecsEditedOptions::default()
            },
            SecsEditedOptions {
                correction_upper_hz: f64::INFINITY,
                ..SecsEditedOptions::default()
            },
            SecsEditedOptions {
                crush_intensity: -0.1,
                ..SecsEditedOptions::default()
            },
            SecsEditedOptions {
                crush_intensity: 1.5,
                ..SecsEditedOptions::default()
            },
            SecsEditedOptions {
                crush_intensity: f64::NAN,
                ..SecsEditedOptions::default()
            },
        ];
        for options in invalid {
            let result = design_secs_stereo_filter_edited(&ir, &ir, &config, &options, None, None);
            assert!(
                matches!(result, Err(DspError::InvalidArgument(_))),
                "{options:?} accepted"
            );
        }
        // The original input validation is kept.
        let short = simple_room_ir(4_096, 90, 1.0);
        assert!(matches!(
            design_secs_stereo_filter_edited(
                &ir,
                &short,
                &config,
                &SecsEditedOptions::default(),
                None,
                None
            ),
            Err(DspError::ShapeMismatch(_))
        ));
        let mut bad_budget = config.clone();
        bad_budget.maximum_delay_ms = 300.0;
        assert!(matches!(
            design_secs_stereo_filter_edited(
                &ir,
                &ir,
                &bad_budget,
                &SecsEditedOptions::default(),
                None,
                None
            ),
            Err(DspError::InvalidArgument(_))
        ));
    }

    #[test]
    fn the_auto_ceiling_keeps_taper_headroom() {
        // A pure delay asks for no advance: the original 10 ms budget.
        let flat = delta(32_768, 3_000, 1.0);
        let ceiling = secs_edited_auto_delay_ceiling_ms(&flat, &flat, 48_000).unwrap();
        assert_eq!(ceiling.requirement_ms, 0.0);
        assert_eq!(ceiling.ceiling_ms, SECS_AUTO_DELAY_MAX_MS);
        // Any requirement gets the 10 ms taper headroom on the 5 ms grid,
        // also below 10 ms where the original stays at exactly 10 ms. (The
        // anechoic bass-managed model of the integration test reads 8 ms ->
        // 20 ms; it is asserted there.)
        let room = simple_room_ir(32_768, 3_000, 1.0);
        let edited = secs_edited_auto_delay_ceiling_ms(&room, &room, 48_000).unwrap();
        let expected = if edited.requirement_ms <= 0.0 {
            SECS_AUTO_DELAY_MAX_MS
        } else {
            (((edited.requirement_ms + 10.0) / 5.0).ceil() * 5.0).clamp(10.0, 250.0)
        };
        assert_eq!(edited.ceiling_ms, expected);
        assert!(edited.ceiling_ms >= edited.requirement_ms + SECS_AUTO_CEILING_HEADROOM_MS);
    }

    #[test]
    fn the_edited_design_is_deterministic() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let mut config = test_config();
        config.phase_guard = true;
        let options = SecsEditedOptions::default();
        let first =
            design_secs_stereo_filter_edited(&left, &right, &config, &options, None, None).unwrap();
        let second =
            design_secs_stereo_filter_edited(&left, &right, &config, &options, None, None).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn the_decay_score_is_the_linearity_score() {
        let ir = simple_room_ir(8_192, 90, 1.0);
        assert!(secs_decay_score(&ir, 48_000) == secs_linearity_score(&ir, 48_000));
    }

    #[test]
    fn a_flat_overlay_and_own_magnitude_override_change_nothing_in_the_edition() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let config = test_config();
        let options = SecsEditedOptions::default();
        let plain =
            design_secs_stereo_filter_edited(&left, &right, &config, &options, None, None).unwrap();
        let (left_magnitude, right_magnitude) = (own_magnitude(&left), own_magnitude(&right));
        let overridden = design_secs_stereo_filter_edited(
            &left,
            &right,
            &config,
            &options,
            None,
            Some((&left_magnitude, &right_magnitude)),
        )
        .unwrap();
        assert_eq!(plain, overridden);
        let mut flat = config.clone();
        flat.target_overlay = Some(
            TargetCurve::from_knots(
                "flat test curve",
                "test-v1",
                vec![
                    TargetKnot {
                        frequency_hz: 20.0,
                        level_db: 0.0,
                    },
                    TargetKnot {
                        frequency_hz: 20_000.0,
                        level_db: 0.0,
                    },
                ],
            )
            .unwrap(),
        );
        let overlaid =
            design_secs_stereo_filter_edited(&left, &right, &flat, &options, None, None).unwrap();
        assert_eq!(plain, overlaid);
    }
}
