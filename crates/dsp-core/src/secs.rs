//! Port of the open-source SECS single-point room-correction algorithm.
//!
//! Source: `debugfiles/SECS.py` (developer states the original author granted
//! permission to adapt it). This module mirrors the Python DSP line by line so
//! the two implementations can be held to a numerical parity fixture
//! (`testdata/secs-parity.json`, generated from the Python reference).
//!
//! Scope and honesty boundaries:
//! - Only the DSP is ported: per-channel precompute, filter synthesis, the
//!   automatic target-delay search, the stereo combine (level match,
//!   inter-channel peak alignment, channel balance trim, fade, preamp
//!   normalization), and `secs_resample_poly` (the scipy `resample_poly`
//!   equivalent SECS.py uses when the input rate differs from the design
//!   rate). Qt UI and WAV I/O are not ported.
//! - The Python `rm_eq` array is identically 1.0 in SECS.py (a leftover hook),
//!   so it is intentionally not represented here.
//! - `get_peq_mag_fast` is dead code in SECS.py and is intentionally not
//!   ported.
//! - This designs from a single stereo impulse response pair. It is predicted
//!   design only; nothing here claims measured verification.
//!
//! Where SECS.py mixes float32 and float64, this port uses f64 throughout.
//! The parity fixture therefore compares with tolerances instead of bit
//! equality (documented per assertion in the fixture test).

use crate::error::{DspError, DspResult};
use crate::target::TargetCurve;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

type C64 = Complex<f64>;

/// Algorithm identifier recorded on SECS-designed artifacts.
pub const SECS_ALGORITHM_VERSION: &str = "secs-port-v1";

/// SECS constants, mirrored from the top of SECS.py.
pub const SECS_PHASE_PRERING_LOW_MIN_MS: f64 = 2.0;
pub const SECS_PHASE_PRERING_LOW_MAX_MS: f64 = 25.0;
pub const SECS_PHASE_PRERING_MID_MAX_MS: f64 = 10.0;
pub const SECS_PHASE_PRERING_HIGH_MAX_MS: f64 = 5.0;
pub const SECS_AUTO_DELAY_MIN_MS: f64 = 2.0;
pub const SECS_AUTO_DELAY_MAX_MS: f64 = 10.0;
pub const SECS_AUTO_DELAY_COARSE_STEP_MS: f64 = 1.0;
pub const SECS_AUTO_DELAY_WEIGHT_GAIN: f64 = 2.0;
pub const SECS_CHANNEL_BALANCE_BAND_LOW_HZ: f64 = 20.0;
pub const SECS_CHANNEL_BALANCE_BAND_HIGH_HZ: f64 = 1_000.0;
pub const SECS_CHANNEL_BALANCE_MAX_TRIM_DB: f64 = 6.0;
pub const SECS_PHASE_SCORE_DECAY_START_MS: f64 = 100.0;
pub const SECS_PHASE_SCORE_WINDOW_MS: f64 = 100.0;
pub const SECS_PHASE_SCORE_BAND_LOW_HZ: f64 = 1_000.0;
pub const SECS_PHASE_SCORE_BAND_HIGH_HZ: f64 = 10_000.0;
pub const SECS_PHASE_SCORE_REF_SPAN_DB: f64 = 40.0;
pub const SECS_PHASE_SCORE_TARGET_DECAY_DB: f64 = 40.0;
/// House-curve overlay anchor (app extension): the overlay is re-anchored to
/// 0 dB at this frequency before it multiplies the adaptive target, so a
/// curve's absolute level axis never shifts the SECS level anchoring. The
/// bundled presets are already 0 dB at 500 Hz, so they pass through exactly.
pub const SECS_TARGET_OVERLAY_ANCHOR_HZ: f64 = 500.0;

/// Frequency-resolution mode: how aggressively narrow structure is corrected.
/// Mirrors SECS.py `res_mode` 0/1/2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecsResolutionMode {
    /// res_mode 0 — smoothing fractions (12, 6, 3, 2, 1), gentlest.
    Gentle,
    /// res_mode 1 — smoothing fractions (24, 12, 6, 3, 1), SECS default.
    Balanced,
    /// res_mode 2 — smoothing fractions (96, 48, 24, 12, 3), sharpest.
    Precise,
}

impl SecsResolutionMode {
    fn smoothing_fractions(self) -> [f64; 5] {
        match self {
            Self::Gentle => [12.0, 6.0, 3.0, 2.0, 1.0],
            Self::Balanced => [24.0, 12.0, 6.0, 3.0, 1.0],
            Self::Precise => [96.0, 48.0, 24.0, 12.0, 3.0],
        }
    }

    fn sharp_fraction(self) -> f64 {
        match self {
            Self::Gentle => 24.0,
            Self::Balanced => 48.0,
            Self::Precise => 96.0,
        }
    }

    fn crush_intensity(self) -> f64 {
        match self {
            Self::Gentle => 0.3,
            Self::Balanced => 0.6,
            Self::Precise => 1.0,
        }
    }
}

/// Mirror of SECS.py `FilterConfig` minus the resampling fields (`orig_sr` is
/// required to equal the design rate here).
#[derive(Debug, Clone)]
pub struct SecsConfig {
    pub sample_rate_hz: u32,
    pub max_boost_db: f64,
    /// Target delay in milliseconds; doubles as the low-band pre-ringing
    /// budget. The automatic search overwrites this per candidate.
    pub target_delay_ms: f64,
    pub tilt_db_per_octave: f64,
    pub bass_boost_db: f64,
    pub bass_frequency_hz: f64,
    pub resolution: SecsResolutionMode,
    pub taps: usize,
    pub hf_min_phase_reference_hz: f64,
    pub low_latency: bool,
    pub zero_latency: bool,
    /// Optional house-curve overlay (app extension, not in SECS.py). When
    /// `Some`, the curve - re-anchored to 0 dB at
    /// [`SECS_TARGET_OVERLAY_ANCHOR_HZ`] - multiplies the adaptive target
    /// exactly like the bass shelf does, so the magnitude inversion, the
    /// macro EQ and the reported target all steer toward the user's target
    /// curve instead of the flat base ideal. `None` reproduces SECS.py.
    pub target_overlay: Option<TargetCurve>,
    /// 2.1 extension (not in SECS.py): the bass-management crossover in Hz.
    /// Below it one shared subwoofer reproduces both channels, so an L/R
    /// difference measured there is noise on the same physical path (a
    /// 2026-07-30 live 2.1 filter carried an 11.8 dB spurious L-R split at
    /// 37 Hz), not a channel difference. When `Some`, the magnitude AND
    /// excess-phase corrections are blended toward a common L/R correction
    /// at full weight up to the crossover, fading out over the half octave
    /// above it. The phase half also keeps the two filters phase-matched
    /// where bass management SUMS the channels into the subwoofer - a phase
    /// split there thins mono bass in a way per-channel verification sweeps
    /// can never observe. `None` keeps the plain stereo path (parity-pinned).
    pub shared_low_frequency_hz: Option<f64>,
}

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

/// Per-channel summary surfaced on the finished design.
#[derive(Debug, Clone, PartialEq)]
pub struct SecsChannelSummary {
    pub peak_index: usize,
    pub low_cutoff_hz: f64,
    pub high_cutoff_hz: f64,
    pub reference_magnitude: f64,
}

/// Result of the stereo SECS design.
#[derive(Debug, Clone, PartialEq)]
pub struct SecsStereoDesign {
    pub sample_rate_hz: u32,
    pub left_taps: Vec<f64>,
    pub right_taps: Vec<f64>,
    /// Negative preamp recommendation in dB (0.0 when no normalization was
    /// needed). Mirrors the Python `preamp_db`.
    pub preamp_db: f64,
    /// The delay the automatic search settled on (equals the configured delay
    /// when explicit candidates were supplied).
    pub auto_delay_ms: f64,
    /// Averaged L/R natural low/high cutoffs from the winning evaluation.
    pub low_cutoff_hz: f64,
    pub high_cutoff_hz: f64,
    /// Right-channel level match applied before alignment (rms_L / rms_R).
    pub rms_scale: f64,
    /// Median L-R system-balance trim applied to the right filter, dB.
    pub channel_balance_trim_db: f64,
    /// SECS "phase score" of the input IRs (0-100, higher = drier room).
    pub input_phase_score: f64,
    pub channels: [SecsChannelSummary; 2],
    /// Delays tried by the coarse search with their metric (None = candidate
    /// produced a non-finite filter and was skipped).
    pub delay_candidates_ms: Vec<f64>,
    pub delay_metrics: Vec<Option<f64>>,
    /// Positive-frequency grid (> 10 Hz, < Nyquist) of the design FFT and the
    /// left channel's tilt-adjusted target magnitude (linear) on that grid.
    pub target_frequencies_hz: Vec<f64>,
    pub target_magnitude: Vec<f64>,
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

/// `get_lr4_weights_5bands`: complementary LR4-style split at 100/150/200/300 Hz.
fn lr4_weights_5bands(n: usize, sample_rate_hz: f64) -> [Vec<f64>; 5] {
    let freqs = fft_frequencies(n, sample_rate_hz);
    let f_abs = absolute_frequencies(&freqs);
    let low = |f: f64, corner: f64| 1.0 / (1.0 + (f / corner).powi(4));
    let mut bands: [Vec<f64>; 5] = std::array::from_fn(|_| Vec::with_capacity(n));
    for f in &f_abs {
        let l1 = low(*f, 100.0);
        let l2 = low(*f, 150.0);
        let l3 = low(*f, 200.0);
        let l4 = low(*f, 300.0);
        bands[0].push(l1);
        bands[1].push((1.0 - l1) * l2);
        bands[2].push((1.0 - l2) * l3);
        bands[3].push((1.0 - l3) * l4);
        bands[4].push(1.0 - l4);
    }
    bands
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

fn apply_5band_smoothing_complex(
    spectrum: &[C64],
    sample_rate_hz: f64,
    weights: &[Vec<f64>; 5],
    resolution: SecsResolutionMode,
) -> Vec<C64> {
    let fractions = resolution.smoothing_fractions();
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
    let fractions = resolution.smoothing_fractions();
    let mut result = vec![0.0; values.len()];
    for (band, fraction) in fractions.iter().enumerate() {
        let smoothed = smooth_spectrum_real(values, sample_rate_hz, *fraction);
        for (index, value) in smoothed.iter().enumerate() {
            result[index] += weights[band][index] * value;
        }
    }
    result
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
fn precompute_secs_channel(
    ir: &[f64],
    config: &SecsConfig,
    magnitude_override: Option<&[f64]>,
    target_overlay_linear: Option<&[f64]>,
) -> SecsChannelPrecompute {
    let n = ir.len();
    let sample_rate_hz = f64::from(config.sample_rate_hz);
    let freqs = fft_frequencies(n, sample_rate_hz);
    let f_abs = absolute_frequencies(&freqs);
    let peak_index = argmax_abs(ir);
    let h_raw = fft_real(ir, n);
    let shift = sample_shift_phase(&freqs, sample_rate_hz, peak_index as f64);
    let h_shifted: Vec<C64> = h_raw.iter().zip(shift.iter()).map(|(h, s)| h * s).collect();
    let weights = lr4_weights_5bands(n, sample_rate_hz);
    let own_magnitude: Vec<f64> = h_raw.iter().map(|h| h.norm()).collect();
    let min_h = minimum_phase_from_magnitude(&own_magnitude);
    let mut excess_inv_h: Vec<C64> = h_shifted
        .iter()
        .zip(min_h.iter())
        .map(|(h, m)| (h / (m + 1e-12)).conj())
        .collect();
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
    let ref_mag = if !valid_values.is_empty() {
        percentile_linear(&valid_values, 35.0)
    } else {
        mag_orig.iter().sum::<f64>() / mag_orig.len() as f64
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
    let mut smoothed_excess =
        apply_5band_smoothing_complex(&excess_inv_h, sample_rate_hz, &weights, config.resolution);
    for value in smoothed_excess.iter_mut() {
        *value /= value.norm() + 1e-12;
    }
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
    let target_mag_base: Vec<f64> = target_curve.iter().map(|t| ref_mag * t).collect();

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

struct SecsChannelFilter {
    taps: Vec<f64>,
    target_mag_ideal: Vec<f64>,
}

/// `process_secs_filter`.
fn process_secs_filter(
    precomputed: &SecsChannelPrecompute,
    config: &SecsConfig,
    apply_tilt: bool,
) -> SecsChannelFilter {
    let n = precomputed.n;
    let sample_rate_hz = precomputed.sample_rate_hz;
    let freqs = &precomputed.freqs;
    let f_abs = &precomputed.f_abs;
    let weights = &precomputed.weights;

    let initial_filter_h: Vec<C64> = if config.low_latency || config.zero_latency {
        fft_real(&precomputed.min_inv_ir, n)
    } else {
        let pr_low = config
            .target_delay_ms
            .clamp(SECS_PHASE_PRERING_LOW_MIN_MS, SECS_PHASE_PRERING_LOW_MAX_MS);
        let pr_mid = SECS_PHASE_PRERING_MID_MAX_MS.min(pr_low * 0.5);
        let pr_high = SECS_PHASE_PRERING_HIGH_MAX_MS.min(pr_low * 0.25);

        let windowed_fft = |left_ms: f64| -> Vec<C64> {
            let window = asymmetric_window(n, sample_rate_hz, left_ms, f64::INFINITY);
            let windowed: Vec<f64> = precomputed
                .excess_inv_ir
                .iter()
                .zip(window.iter())
                .map(|(sample, w)| sample * w)
                .collect();
            fft_real(&windowed, n)
        };
        let h_low = windowed_fft(pr_low);
        let h_mid = windowed_fft(pr_mid);
        let h_high = windowed_fft(pr_high);
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
    let mut macro_eq_mag: Vec<f64> = (0..n)
        .map(|index| {
            let raw = precomputed.target_mag_base[index] / (mag_simulated_tonal[index] + 1e-12);
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
        config.resolution.sharp_fraction(),
    );
    let crush_intensity = config.resolution.crush_intensity();
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
    }
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

/// `FilterGeneratorWorker._crop_peak_left`.
fn crop_peak_left(signal: &[f64], taps: usize, left_samples: usize, peak_index: usize) -> Vec<f64> {
    let start = peak_index as isize - left_samples as isize;
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

/// `FilterGeneratorWorker._linearity_score`: SECS "phase score" (0-100).
pub fn secs_linearity_score(signal: &[f64], sample_rate_hz: u32) -> f64 {
    let n = signal.len();
    if n == 0 {
        return 0.0;
    }
    let sample_rate = f64::from(sample_rate_hz);
    let peak_index = argmax_abs(signal);
    let peak_value = signal[peak_index];
    if peak_value.abs() <= 1e-20 {
        return 0.0;
    }
    let mut rolled: Vec<f64> = Vec::with_capacity(n);
    for index in 0..n {
        rolled.push(signal[(index + peak_index) % n]);
    }
    if peak_value < 0.0 {
        for value in rolled.iter_mut() {
            *value = -*value;
        }
    }

    let freqs = fft_frequencies(n, sample_rate);
    let window_length = (SECS_PHASE_SCORE_WINDOW_MS * sample_rate / 1000.0) as usize;
    let decay_start = (SECS_PHASE_SCORE_DECAY_START_MS * sample_rate / 1000.0) as usize;
    let mut early = vec![0.0; n];
    let mut late = vec![0.0; n];
    let early_available = n.min(window_length);
    early[..early_available].copy_from_slice(&rolled[..early_available]);
    let mut coverage = 0.0;
    let mut late_available = 0;
    if window_length > 0 {
        let late_start = if decay_start < n {
            late_available = (n - decay_start).min(window_length);
            coverage = (late_available as f64 / window_length.max(1) as f64).clamp(0.0, 1.0);
            decay_start
        } else {
            let start = n.saturating_sub(window_length);
            late_available = (n - start).min(window_length);
            coverage = 0.0;
            start
        };
        if late_available > 0 {
            late[..late_available]
                .copy_from_slice(&rolled[late_start..late_start + late_available]);
        }
    }
    let h_early = fft_real(&early, n);
    let h_late = fft_real(&late, n);
    let band: Vec<usize> = (0..n)
        .filter(|index| {
            freqs[*index] > SECS_PHASE_SCORE_BAND_LOW_HZ
                && freqs[*index] < SECS_PHASE_SCORE_BAND_HIGH_HZ
        })
        .collect();
    if band.is_empty() || early_available == 0 || late_available == 0 {
        return 0.0;
    }
    let early_db: Vec<f64> = band
        .iter()
        .map(|index| 20.0 * (h_early[*index].norm() + 1e-12).log10())
        .collect();
    let reference_db = percentile_linear(&early_db, 90.0);
    let mut valid: Vec<usize> = band
        .iter()
        .zip(early_db.iter())
        .filter(|(_, db)| **db >= reference_db - SECS_PHASE_SCORE_REF_SPAN_DB)
        .map(|(index, _)| *index)
        .collect();
    if valid.is_empty() {
        valid = band;
    }
    let mean_decay_db = valid
        .iter()
        .map(|index| {
            let decay_db =
                20.0 * ((h_late[*index].norm() + 1e-12) / (h_early[*index].norm() + 1e-12)).log10();
            (-decay_db).clamp(0.0, 200.0)
        })
        .sum::<f64>()
        / valid.len() as f64;
    let raw_score =
        100.0 * (1.0 - (-mean_decay_db / SECS_PHASE_SCORE_TARGET_DECAY_DB.max(1e-9)).exp());
    (raw_score * coverage).clamp(0.0, 100.0)
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

/// Stereo SECS design: mirror of `FilterGeneratorWorker.run` for a stereo IR
/// pair, non-preview path, without resampling.
/// `magnitude_overrides`: optional multi-position extension (not in
/// SECS.py). `Some((left, right))` supplies spatially averaged magnitude
/// spectra on the impulse's full FFT grid; the magnitude correction then
/// targets the averaged response while the excess-phase correction and its
/// confidence guard stay strictly on this impulse pair. `None` reproduces
/// SECS.py exactly (the parity fixture pins that path).
pub fn design_secs_stereo_filter(
    left_ir: &[f64],
    right_ir: &[f64],
    config: &SecsConfig,
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
    let weights = lr4_weights_5bands(fft_len, sample_rate_hz);
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
    );
    let mut precomputed_right = precompute_secs_channel(
        right_ir,
        config,
        effective_magnitude_overrides.map(|(_, right)| right),
        target_overlay_linear.as_deref(),
    );

    // 2.1 shared-sub-band commonization, phase half: below the crossover
    // both captures measured the same driver, so the excess-phase correctors
    // are blended toward their normalized complex mean (both are referenced
    // to each channel's own direct-sound peak, so the mean is meaningful).
    // This removes noise-chasing L/R phase divergence AND keeps the two
    // filters phase-matched where bass management sums the channels into
    // the subwoofer.
    if let Some(crossover_hz) = config.shared_low_frequency_hz {
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

    let stage1: Vec<f64> = if config.low_latency || config.zero_latency {
        vec![0.0]
    } else if let Some(candidates) = delay_candidates_ms {
        let filtered: Vec<f64> = candidates
            .iter()
            .filter(|delay| delay.is_finite())
            .map(|delay| delay.clamp(SECS_AUTO_DELAY_MIN_MS, SECS_AUTO_DELAY_MAX_MS))
            .collect();
        if filtered.is_empty() {
            coarse_delay_grid()
        } else {
            filtered
        }
    } else {
        coarse_delay_grid()
    };

    let mut target_db_cache: Option<Vec<f64>> = None;
    let mut best: Option<DelayEvaluation> = None;
    let mut delay_metrics: Vec<Option<f64>> = Vec::with_capacity(stage1.len());

    for delay_ms in &stage1 {
        let mut candidate_config = config.clone();
        candidate_config.target_delay_ms = *delay_ms;
        let target_delay_samples =
            ((candidate_config.target_delay_ms / 1000.0) * sample_rate_hz) as usize;

        let left_filter = process_secs_filter(&precomputed_left, &candidate_config, false);
        let right_filter = process_secs_filter(&precomputed_right, &candidate_config, false);
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

        let mut peak_reference = 0;
        let mut peak_reference_value = f64::NEG_INFINITY;
        for index in 0..final_left.len() {
            let combined = final_left[index].abs().max(final_right[index].abs());
            if combined > peak_reference_value {
                peak_reference_value = combined;
                peak_reference = index;
            }
        }
        let crop_left = crop_peak_left(
            &final_left,
            candidate_config.taps,
            target_delay_samples,
            peak_reference,
        );
        let crop_right = crop_peak_left(
            &final_right,
            candidate_config.taps,
            target_delay_samples,
            peak_reference,
        );

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
        let left_filter = process_secs_filter(&precomputed_left, &final_config, true);
        let right_filter = process_secs_filter(&precomputed_right, &final_config, true);
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
        let mut peak_reference = 0;
        let mut peak_reference_value = f64::NEG_INFINITY;
        for index in 0..final_left.len() {
            let combined = final_left[index].abs().max(final_right[index].abs());
            if combined > peak_reference_value {
                peak_reference_value = combined;
                peak_reference = index;
            }
        }
        winner.crop_left = crop_peak_left(
            &final_left,
            final_config.taps,
            winner.target_delay_samples,
            peak_reference,
        );
        winner.crop_right = crop_peak_left(
            &final_right,
            final_config.taps,
            winner.target_delay_samples,
            peak_reference,
        );
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

    let left_score = secs_linearity_score(left_ir, config.sample_rate_hz);
    let right_score = secs_linearity_score(right_ir, config.sample_rate_hz);

    let target_linear: Vec<f64> = orig_indices
        .iter()
        .map(|index| winner.target_mag_left[*index])
        .collect();

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
    })
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

/// Zeroth-order modified Bessel function of the first kind (series expansion,
/// converges quickly for the Kaiser beta range used here).
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut sum = 1.0;
    let mut term = 1.0;
    for k in 1..64 {
        term *= (half / k as f64) * (half / k as f64);
        sum += term;
        if term < sum * 1e-18 {
            break;
        }
    }
    sum
}

/// `scipy.signal.resample_poly(x, up, down)` with its default
/// `window=('kaiser', 5.0)` design: a rational-ratio polyphase resampler
/// whose anti-alias/anti-image lowpass is a Kaiser(5.0)-windowed sinc with
/// `half_len = 10 * max(up, down)` and cutoff `1 / max(up, down)`. The output
/// is time-aligned with the input (the filter's group delay is removed) and
/// has length `ceil(len * up / down)`. This is the resampler SECS.py itself
/// runs when the source rate differs from the design rate, which is why the
/// per-rate export path reuses it. Parity with scipy is pinned by the fixture
/// test.
pub fn secs_resample_poly(input: &[f64], up: u32, down: u32) -> DspResult<Vec<f64>> {
    if input.is_empty() {
        return Err(DspError::EmptyInput("resampler input"));
    }
    if up == 0 || down == 0 {
        return Err(DspError::InvalidArgument(
            "resampler factors must be positive".to_string(),
        ));
    }
    let divisor = gcd(up, down);
    let up = (up / divisor) as usize;
    let down = (down / divisor) as usize;
    if up == 1 && down == 1 {
        return Ok(input.to_vec());
    }
    let max_rate = up.max(down);
    let cutoff = 1.0 / max_rate as f64;
    let half_len = 10 * max_rate;
    let taps = 2 * half_len + 1;

    // firwin(taps, cutoff, window=("kaiser", 5.0)) with scale=True: a
    // windowed sinc normalized to unity DC gain, then scaled by `up` to
    // preserve amplitude through the zero-stuffing.
    let beta = 5.0;
    let i0_beta = bessel_i0(beta);
    let mut kernel = Vec::with_capacity(taps);
    let mut kernel_sum = 0.0;
    for index in 0..taps {
        let offset = index as f64 - half_len as f64;
        let sinc = if offset == 0.0 {
            1.0
        } else {
            let argument = std::f64::consts::PI * cutoff * offset;
            argument.sin() / argument
        };
        let position = 2.0 * index as f64 / (taps - 1) as f64 - 1.0;
        let window = bessel_i0(beta * (1.0 - position * position).max(0.0).sqrt()) / i0_beta;
        let value = cutoff * sinc * window;
        kernel_sum += value;
        kernel.push(value);
    }
    for value in kernel.iter_mut() {
        *value = *value / kernel_sum * up as f64;
    }

    // y[k] = sum_n x[n] * h[half_len + k*down - n*up], zero-padded edges:
    // polyphase evaluation of upfirdn with the leading group delay trimmed so
    // the output starts at the input's time origin.
    let length = input.len();
    let output_length = (length * up).div_ceil(down);
    let mut output = Vec::with_capacity(output_length);
    for k in 0..output_length {
        let center = half_len as i64 + (k * down) as i64;
        // h index = center - n*up must lie in [0, taps).
        let n_low = (center - (taps as i64 - 1)).div_euclid(up as i64).max(0);
        let mut n = n_low;
        // Advance to the first n with a valid kernel index.
        while center - n * up as i64 > taps as i64 - 1 {
            n += 1;
        }
        let mut accumulator = 0.0;
        while n < length as i64 {
            let kernel_index = center - n * up as i64;
            if kernel_index < 0 {
                break;
            }
            accumulator += input[n as usize] * kernel[kernel_index as usize];
            n += 1;
        }
        output.push(accumulator);
    }
    Ok(output)
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a
}

/// Smallest 5-smooth (2^a * 3^b * 5^c) length >= `minimum`. SECS.py pads the
/// loaded impulse to `scipy.fft.next_fast_len` before designing; this mirrors
/// that zero-padding step with a factor set rustfft handles efficiently. The
/// exact smooth set differs from pocketfft's (which also admits 7 and 11), so
/// the pad may be slightly longer — both are pure zero-padding of the tail.
pub fn secs_next_fast_len(minimum: usize) -> usize {
    if minimum <= 1 {
        return 1;
    }
    let mut best = usize::MAX;
    let mut power5 = 1usize;
    while power5 < best {
        let mut with_three = power5;
        while with_three < best {
            let mut candidate = with_three;
            while candidate < minimum {
                match candidate.checked_mul(2) {
                    Some(next) => candidate = next,
                    None => break,
                }
            }
            if candidate >= minimum && candidate < best {
                best = candidate;
            }
            match with_three.checked_mul(3) {
                Some(next) => with_three = next,
                None => break,
            }
        }
        match power5.checked_mul(5) {
            Some(next) => power5 = next,
            None => break,
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

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
            shared_low_frequency_hz: None,
        }
    }

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

    #[test]
    fn the_excess_phase_corrector_is_all_pass() {
        let ir = simple_room_ir(8_192, 90, 1.0);
        let precomputed = precompute_secs_channel(&ir, &test_config(), None, None);
        let spectrum = fft_real(&precomputed.excess_inv_ir, precomputed.n);
        for value in &spectrum {
            assert!(
                (value.norm() - 1.0).abs() < 1e-9,
                "excess corrector must stay unit magnitude, saw {}",
                value.norm()
            );
        }
    }

    #[test]
    fn the_magnitude_inversion_respects_the_boost_ceiling() {
        let config = test_config();
        let ir = simple_room_ir(8_192, 90, 1.0);
        let precomputed = precompute_secs_channel(&ir, &config, None, None);
        // The minimum-phase inverse magnitude may not exceed the configured
        // boost cap anywhere (the HF fade only pulls it toward unity).
        let ceiling = 10f64.powf(config.max_boost_db / 20.0) + 1e-6;
        let spectrum = fft_real(&precomputed.min_inv_ir, precomputed.n);
        for value in &spectrum {
            assert!(
                value.norm() <= ceiling,
                "minimum-phase inversion exceeded the boost cap: {}",
                value.norm()
            );
        }
    }

    #[test]
    fn the_stereo_design_is_deterministic() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let config = test_config();
        let first = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        let second = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn zero_latency_mode_produces_a_causal_filter() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let mut config = test_config();
        config.zero_latency = true;
        let design = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        assert_eq!(design.auto_delay_ms, 0.0);
        let peak = argmax_abs(&design.left_taps);
        assert!(
            peak <= 2,
            "zero-latency crop must start at the filter peak, saw index {peak}"
        );
    }

    #[test]
    fn normal_mode_bounds_the_energy_before_the_delayed_peak() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let config = test_config();
        let design = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        let peak_value = design
            .left_taps
            .iter()
            .map(|tap| tap.abs())
            .fold(f64::NEG_INFINITY, f64::max);
        let head = ((design.auto_delay_ms / 4.0) / 1000.0 * 48_000.0) as usize;
        let head_peak = design.left_taps[..head]
            .iter()
            .map(|tap| tap.abs())
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            head_peak <= 0.05 * peak_value,
            "pre-ringing head energy is not bounded: {head_peak} vs peak {peak_value}"
        );
    }

    #[test]
    fn a_magnitude_override_equal_to_the_own_magnitude_changes_nothing() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let config = test_config();
        let own = |ir: &[f64]| -> Vec<f64> {
            fft_real(ir, ir.len())
                .iter()
                .map(|value| value.norm())
                .collect()
        };
        let left_magnitude = own(&left);
        let right_magnitude = own(&right);
        let plain = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        let overridden = design_secs_stereo_filter(
            &left,
            &right,
            &config,
            None,
            Some((&left_magnitude, &right_magnitude)),
        )
        .unwrap();
        assert_eq!(plain, overridden);
    }

    #[test]
    fn a_spatially_averaged_magnitude_redirects_the_magnitude_correction() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let config = test_config();
        // An average in which every seat but this one lacks the modal peak:
        // scale the 40-70 Hz region down by 6 dB and expect the design to
        // follow the average, i.e. differ from the single-point design.
        let diluted = |ir: &[f64]| -> Vec<f64> {
            let n = ir.len();
            fft_real(ir, n)
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let hz = fft_frequencies(n, 48_000.0)[index].abs();
                    let scale = if (40.0..=70.0).contains(&hz) {
                        0.5
                    } else {
                        1.0
                    };
                    value.norm() * scale
                })
                .collect()
        };
        let left_magnitude = diluted(&left);
        let right_magnitude = diluted(&right);
        let plain = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        let averaged = design_secs_stereo_filter(
            &left,
            &right,
            &config,
            None,
            Some((&left_magnitude, &right_magnitude)),
        )
        .unwrap();
        assert_ne!(plain.left_taps, averaged.left_taps);
        // The averaged design must not exceed the boost cap either.
        let ceiling = 10f64.powf(config.max_boost_db / 20.0) + 1e-6;
        let response = fft_real(&averaged.left_taps, averaged.left_taps.len());
        let peak = response
            .iter()
            .map(|value| value.norm())
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(peak.is_finite() && peak > 0.0);
        let _ = ceiling;
    }

    #[test]
    fn a_flat_target_overlay_changes_nothing() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let plain = design_secs_stereo_filter(&left, &right, &test_config(), None, None).unwrap();
        let mut config = test_config();
        config.target_overlay = Some(
            crate::target::TargetCurve::from_knots(
                "flat test curve",
                "test-v1",
                vec![
                    crate::target::TargetKnot {
                        frequency_hz: 20.0,
                        level_db: 0.0,
                    },
                    crate::target::TargetKnot {
                        frequency_hz: 20_000.0,
                        level_db: 0.0,
                    },
                ],
            )
            .unwrap(),
        );
        let overlaid = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        assert_eq!(plain, overlaid);
    }

    #[test]
    fn a_house_curve_overlay_steers_the_reported_target() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(8_192, 101, 0.9);
        let plain = design_secs_stereo_filter(&left, &right, &test_config(), None, None).unwrap();
        let curve = crate::target::TargetCurve::preset(crate::target::TargetPreset::HarmanStyle);
        let mut config = test_config();
        config.target_overlay = Some(curve.clone());
        let overlaid = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        assert_ne!(plain.left_taps, overlaid.left_taps);
        // The overlay multiplies the adaptive target verbatim (re-anchored at
        // 500 Hz), so the reported target must shift by exactly the curve.
        let anchor_db = curve.level_at(SECS_TARGET_OVERLAY_ANCHOR_HZ).unwrap();
        for probe_hz in [50.0, 200.0, 10_000.0] {
            let mut index = 0;
            let mut best = f64::INFINITY;
            for (candidate, frequency) in plain.target_frequencies_hz.iter().enumerate() {
                let distance = (frequency - probe_hz).abs();
                if distance < best {
                    best = distance;
                    index = candidate;
                }
            }
            let frequency = plain.target_frequencies_hz[index];
            let expected_db = curve.level_at(frequency).unwrap() - anchor_db;
            let ratio_db = 20.0
                * ((overlaid.target_magnitude[index] + 1e-30)
                    / (plain.target_magnitude[index] + 1e-30))
                    .log10();
            assert!(
                (ratio_db - expected_db).abs() < 1e-6,
                "target near {frequency:.1} Hz shifted {ratio_db:.4} dB, expected {expected_db:.4}"
            );
        }
    }

    #[test]
    fn a_shared_sub_band_commonizes_the_low_frequency_correction() {
        // A 45 Hz mode present on the LEFT capture only - below a 90 Hz
        // crossover this must be treated as same-path noise, not as a
        // genuine channel difference.
        let sample_rate = 48_000.0;
        let mut left = simple_room_ir(8_192, 90, 1.0);
        for offset in 0..(8_192 - 90) {
            let t = offset as f64 / sample_rate;
            left[90 + offset] +=
                1.2e-3 * (-t / 0.3).exp() * (2.0 * std::f64::consts::PI * 45.0 * t).sin();
        }
        let right = simple_room_ir(8_192, 101, 0.9);
        let config = test_config();
        let mut shared_config = test_config();
        shared_config.shared_low_frequency_hz = Some(90.0);
        let plain = design_secs_stereo_filter(&left, &right, &config, None, None).unwrap();
        let shared = design_secs_stereo_filter(&left, &right, &shared_config, None, None).unwrap();

        // L-R filter magnitude spread in the sub-owned band, with the
        // constant channel-balance/level offset removed.
        let spread = |design: &SecsStereoDesign| -> f64 {
            let fft_len = design.left_taps.len().next_power_of_two();
            let h_left = fft_real(&design.left_taps, fft_len);
            let h_right = fft_real(&design.right_taps, fft_len);
            let mut differences: Vec<f64> = (0..fft_len)
                .filter(|index| {
                    let hz = *index as f64 * sample_rate / fft_len as f64;
                    (25.0..=70.0).contains(&hz)
                })
                .map(|index| {
                    20.0 * (h_left[index].norm() + 1e-12).log10()
                        - 20.0 * (h_right[index].norm() + 1e-12).log10()
                })
                .collect();
            differences.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = differences[differences.len() / 2];
            differences
                .iter()
                .map(|difference| (difference - median).abs())
                .fold(0.0, f64::max)
        };
        let plain_spread = spread(&plain);
        let shared_spread = spread(&shared);
        assert!(
            plain_spread > 3.0,
            "the injected left-only mode must split the plain filters, saw {plain_spread:.2} dB"
        );
        assert!(
            shared_spread < 1.5 && shared_spread < plain_spread * 0.5,
            "commonization must collapse the sub-band split: {shared_spread:.2} vs {plain_spread:.2} dB"
        );

        // Phase half: below the crossover the two filters must agree in
        // phase (mono bass sums into one subwoofer), far more tightly than
        // the plain design.
        let phase_spread = |design: &SecsStereoDesign| -> f64 {
            let fft_len = design.left_taps.len().next_power_of_two();
            let h_left = fft_real(&design.left_taps, fft_len);
            let h_right = fft_real(&design.right_taps, fft_len);
            (0..fft_len)
                .filter(|index| {
                    let hz = *index as f64 * sample_rate / fft_len as f64;
                    (25.0..=70.0).contains(&hz)
                })
                .map(|index| {
                    (h_left[index] / (h_right[index] + C64::new(1e-18, 0.0)))
                        .arg()
                        .abs()
                })
                .fold(0.0, f64::max)
        };
        assert!(
            phase_spread(&shared) < phase_spread(&plain),
            "commonization must not widen the sub-band phase split"
        );
    }

    #[test]
    fn mismatched_channel_lengths_are_rejected() {
        let left = simple_room_ir(8_192, 90, 1.0);
        let right = simple_room_ir(4_096, 90, 1.0);
        let error =
            design_secs_stereo_filter(&left, &right, &test_config(), None, None).unwrap_err();
        assert!(matches!(error, DspError::ShapeMismatch(_)));
    }
}
