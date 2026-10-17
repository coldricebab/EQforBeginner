//! Bug-fix verification for the SECS bug-fix edition
//! (`eqforbeginner_dsp_core::secs_edited`) against the defects listed in the
//! developer-local `debugfiles/secs_bugfix_prompt.txt`.
//!
//! Every number is measured with independent analysis only - raw |H| on a
//! 131072-point FFT, or a single linear-bin boxcar 1/3-octave power average -
//! never with the SECS smoothing, which shares defect [1] and hides it. The
//! models, seeds and thresholds were fixed before the edition was run (the
//! thresholds are the bug report's). Each test also shows that the original
//! port (`secs::design_secs_stereo_filter`) fails the same criterion, so the
//! test can see the bug - except [6] and [8], which have no defect in the port.
//!
//! These are synthetic models: predicted design only, not a measured
//! (microphone) verification.

use eqforbeginner_dsp_core::secs::{
    design_secs_stereo_filter, secs_linearity_score, SecsConfig, SecsResolutionMode,
    SecsStereoDesign,
};
use eqforbeginner_dsp_core::secs_edited::{
    design_secs_stereo_filter_edited, secs_decay_score, secs_edited_auto_delay_ceiling_ms,
    SecsEditedOptions,
};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

const FS: f64 = 48_000.0;
const N: usize = 32_768;
const ONSET: usize = 3_000;
const FFT_N: usize = 131_072;

type C64 = Complex<f64>;

// --- Models and independent analysis (from the Step-1 reproduction harness) ---

fn fft(signal: &[f64], n: usize) -> Vec<C64> {
    let mut buffer = vec![C64::new(0.0, 0.0); n];
    for (slot, value) in buffer.iter_mut().zip(signal.iter()) {
        slot.re = *value;
    }
    FftPlanner::<f64>::new()
        .plan_fft_forward(n)
        .process(&mut buffer);
    buffer
}

fn ifft(mut spectrum: Vec<C64>) -> Vec<f64> {
    let n = spectrum.len();
    FftPlanner::<f64>::new()
        .plan_fft_inverse(n)
        .process(&mut spectrum);
    spectrum.iter().map(|v| v.re / n as f64).collect()
}

fn delta(n: usize, at: usize, gain: f64) -> Vec<f64> {
    let mut ir = vec![0.0; n];
    ir[at] = gain;
    ir
}

/// RBJ biquad, direct form I.
fn biquad(signal: &[f64], b: [f64; 3], a: [f64; 3]) -> Vec<f64> {
    let (b0, b1, b2) = (b[0] / a[0], b[1] / a[0], b[2] / a[0]);
    let (a1, a2) = (a[1] / a[0], a[2] / a[0]);
    let mut out = vec![0.0; signal.len()];
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    for (slot, x0) in out.iter_mut().zip(signal.iter()) {
        let y0 = b0 * x0 + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        *slot = y0;
        x2 = x1;
        x1 = *x0;
        y2 = y1;
        y1 = y0;
    }
    out
}

fn butter2(signal: &[f64], corner: f64, high: bool) -> Vec<f64> {
    let w0 = 2.0 * std::f64::consts::PI * corner / FS;
    let (c, s) = (w0.cos(), w0.sin());
    let alpha = s / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let a = [1.0 + alpha, -2.0 * c, 1.0 - alpha];
    let b = if high {
        [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0]
    } else {
        [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0]
    };
    biquad(signal, b, a)
}

fn lr4_low(signal: &[f64], corner: f64) -> Vec<f64> {
    butter2(&butter2(signal, corner, false), corner, false)
}

fn lr4_high(signal: &[f64], corner: f64) -> Vec<f64> {
    butter2(&butter2(signal, corner, true), corner, true)
}

fn lr4_allpass(signal: &[f64], corner: f64) -> Vec<f64> {
    let low = lr4_low(signal, corner);
    let high = lr4_high(signal, corner);
    low.iter().zip(high.iter()).map(|(a, b)| a + b).collect()
}

fn shift(signal: &[f64], samples: usize) -> Vec<f64> {
    let mut out = vec![0.0; signal.len()];
    out[samples..].copy_from_slice(&signal[..signal.len() - samples]);
    out
}

/// Anechoic model of item [4]: 80 Hz LR4 bass management with the sub path
/// 2 ms late, then 400 Hz and 2.2 kHz LR4 all-passes.
fn anechoic_bass_managed(n: usize, onset: usize) -> Vec<f64> {
    let x = delta(n, onset, 1.0);
    let main = lr4_high(&x, 80.0);
    let sub = shift(&lr4_low(&x, 80.0), (0.002 * FS).round() as usize);
    let summed: Vec<f64> = main.iter().zip(sub.iter()).map(|(a, b)| a + b).collect();
    lr4_allpass(&lr4_allpass(&summed, 400.0), 2_200.0)
}

/// Deterministic xorshift64* + Box-Muller normal source.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn normal(&mut self) -> f64 {
        let u1 = self.uniform();
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

/// Item [3] room: flat direct sound plus a diffuse tail whose power makes the
/// steady-state (direct + reverb) power follow `slope_db_per_octave` around
/// 1 kHz, with reverb `reverb_to_direct_db` above the direct sound at 1 kHz.
fn sloped_room(
    n: usize,
    onset: usize,
    slope_db_per_octave: f64,
    reverb_to_direct_db: f64,
    rt60_s: f64,
    seed: u64,
) -> Vec<f64> {
    let alpha = -slope_db_per_octave / (10.0 * 2f64.log10()); // power ~ f^-alpha
    let p1k = 1.0 + 10f64.powf(reverb_to_direct_db / 10.0);
    let target_reverb_power = |f: f64| -> f64 {
        let f = f.max(10.0);
        (p1k * (f / 1000.0).powf(-alpha) - 1.0).max(0.0)
    };
    let mut rng = Rng::new(seed);
    let noise: Vec<f64> = (0..n).map(|_| rng.normal()).collect();
    let mut spec = fft(&noise, n);
    for (k, value) in spec.iter_mut().enumerate() {
        let f = (k.min(n - k)) as f64 * FS / n as f64;
        *value *= target_reverb_power(f).sqrt();
    }
    let colored = ifft(spec);
    let start = onset + (0.005 * FS) as usize;
    let mut tail = vec![0.0; n];
    for t in start..n {
        let seconds = (t - start) as f64 / FS;
        tail[t] = colored[t] * (-6.907_755 * seconds / rt60_s).exp();
    }
    // Normalize: tail power at 1 kHz (1/3 octave) must equal p1k - 1.
    let tail_spec = fft(&tail, n);
    let measured = band_power(
        &tail_spec,
        n,
        1000.0 / 2f64.powf(1.0 / 6.0),
        1000.0 * 2f64.powf(1.0 / 6.0),
    );
    let gain = ((p1k - 1.0) / measured).sqrt();
    let mut ir = vec![0.0; n];
    ir[onset] = 1.0;
    for t in 0..n {
        ir[t] += tail[t] * gain;
    }
    ir
}

/// Exponentially decaying sine starting at `onset` with quality factor `q`
/// (decay time constant q / (pi f)) whose resonance peak adds `peak_gain` to
/// the spectrum magnitude (continuous-time approximation a * tau * fs / 2).
fn decaying_mode(n: usize, onset: usize, frequency_hz: f64, q: f64, peak_gain: f64) -> Vec<f64> {
    let tau = q / (std::f64::consts::PI * frequency_hz);
    let amplitude = peak_gain / (tau * FS / 2.0);
    let mut out = vec![0.0; n];
    for (t, slot) in out.iter_mut().enumerate().skip(onset) {
        let seconds = (t - onset) as f64 / FS;
        *slot = amplitude
            * (-seconds / tau).exp()
            * (2.0 * std::f64::consts::PI * frequency_hz * seconds).sin();
    }
    out
}

/// Mean |X|^2 over the linear bins of [lo, hi] (a single unbiased boxcar).
fn band_power(spectrum: &[C64], n: usize, lo: f64, hi: f64) -> f64 {
    let df = FS / n as f64;
    let (mut sum, mut count) = (0.0, 0usize);
    for (k, value) in spectrum.iter().enumerate().take(n / 2).skip(1) {
        let f = k as f64 * df;
        if f >= lo && f <= hi {
            sum += value.norm_sqr();
            count += 1;
        }
    }
    sum / count.max(1) as f64
}

fn third_octave_db(spectrum: &[C64], n: usize, center: f64) -> f64 {
    let edge = 2f64.powf(1.0 / 6.0);
    10.0 * band_power(spectrum, n, center / edge, center * edge).log10()
}

/// Base-2 1/3-octave centers (1000 * 2^(k/3)) whose nominal band lies in
/// [lo, hi]: a 2% slack admits the exact centers of nominal 50, 5000 and
/// 16000 Hz (49.6, 5040, 16127 Hz).
fn third_octave_centers(lo: f64, hi: f64) -> Vec<f64> {
    (-40..=20)
        .map(|k| 1000.0 * 2f64.powf(f64::from(k) / 3.0))
        .filter(|fc| *fc >= lo * 0.98 && *fc <= hi * 1.02)
        .collect()
}

/// Excess group delay (total - minimum phase) of a spectrum, ms, per bin.
fn excess_group_delay_ms(spectrum: &[C64]) -> Vec<f64> {
    let n = spectrum.len();
    let df = FS / n as f64;
    let mut cep: Vec<C64> = spectrum
        .iter()
        .map(|v| C64::new(v.norm().max(1e-12).ln(), 0.0))
        .collect();
    FftPlanner::<f64>::new()
        .plan_fft_inverse(n)
        .process(&mut cep);
    for v in cep.iter_mut() {
        *v /= n as f64;
    }
    let mut folded = vec![C64::new(0.0, 0.0); n];
    folded[0] = C64::new(cep[0].re, 0.0);
    for k in 1..n / 2 {
        folded[k] = C64::new(2.0 * cep[k].re, 0.0);
    }
    folded[n / 2] = C64::new(cep[n / 2].re, 0.0);
    FftPlanner::<f64>::new()
        .plan_fft_forward(n)
        .process(&mut folded);
    let min_phase: Vec<C64> = folded.iter().map(|v| v.exp()).collect();
    let gd = |s: &[C64], k: usize| -> f64 {
        -(s[k + 1] * s[k].conj()).arg() / (2.0 * std::f64::consts::PI * df) * 1000.0
    };
    (0..n / 2 - 1)
        .map(|k| gd(spectrum, k) - gd(&min_phase, k))
        .collect()
}

fn convolve_spectrum(a: &[f64], b: &[f64], n: usize) -> Vec<C64> {
    let sa = fft(a, n);
    let sb = fft(b, n);
    sa.iter().zip(sb.iter()).map(|(x, y)| x * y).collect()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    if v.len() % 2 == 0 {
        0.5 * (v[m - 1] + v[m])
    } else {
        v[m]
    }
}

/// Pre-ringing energy share of a time signal: energy strictly before
/// (peak - 0.5 ms) over the total, dB.
fn pre_ring_db(signal: &[f64]) -> f64 {
    let peak = argmax_abs(signal);
    let guard = (0.0005 * FS) as usize;
    let total: f64 = signal.iter().map(|v| v * v).sum();
    let pre: f64 = signal[..peak.saturating_sub(guard)]
        .iter()
        .map(|v| v * v)
        .sum();
    10.0 * (pre.max(1e-300) / total).log10()
}

fn argmax_abs(signal: &[f64]) -> usize {
    let mut best = (0, f64::NEG_INFINITY);
    for (index, value) in signal.iter().enumerate() {
        if value.abs() > best.1 {
            best = (index, value.abs());
        }
    }
    best.0
}

fn energy(signal: &[f64]) -> f64 {
    signal.iter().map(|v| v * v).sum()
}

/// The design's reported target (dB) at `frequency_hz`, linear in dB between
/// the reported grid points.
fn target_db_at(design: &SecsStereoDesign, frequency_hz: f64) -> f64 {
    let xs = &design.target_frequencies_hz;
    let upper = xs
        .partition_point(|x| *x < frequency_hz)
        .clamp(1, xs.len() - 1);
    let lower = upper - 1;
    let db = |index: usize| 20.0 * design.target_magnitude[index].max(1e-300).log10();
    let fraction = (frequency_hz - xs[lower]) / (xs[upper] - xs[lower]);
    db(lower) + (db(upper) - db(lower)) * fraction
}

/// The left filter with the design's preamp normalization undone, so the
/// system level is comparable to the design's reported target.
fn unnormalized_left(design: &SecsStereoDesign) -> Vec<f64> {
    let gain = 10f64.powf(-design.preamp_db / 20.0);
    design.left_taps.iter().map(|tap| tap * gain).collect()
}

// --- Designs ---

/// Base configuration of the spec: taps scale with the budget,
/// (maximum_delay_ms + 500) ms.
fn base_config(maximum_delay_ms: f64) -> SecsConfig {
    SecsConfig {
        sample_rate_hz: 48_000,
        max_boost_db: 6.0,
        target_delay_ms: 2.0,
        tilt_db_per_octave: 0.0,
        bass_boost_db: 0.0,
        bass_frequency_hz: 80.0,
        resolution: SecsResolutionMode::Balanced,
        taps: ((maximum_delay_ms + 500.0) * 48.0).round() as usize,
        hf_min_phase_reference_hz: 300.0,
        low_latency: false,
        zero_latency: false,
        target_overlay: None,
        shared_low_frequency_hz: None,
        excess_phase_transplant: None,
        phase_guard: true,
        maximum_delay_ms,
    }
}

fn edited(
    left: &[f64],
    right: &[f64],
    config: &SecsConfig,
    options: &SecsEditedOptions,
    candidates: Option<&[f64]>,
) -> SecsStereoDesign {
    design_secs_stereo_filter_edited(left, right, config, options, candidates, None)
        .expect("edited design")
}

fn original(
    left: &[f64],
    right: &[f64],
    config: &SecsConfig,
    candidates: Option<&[f64]>,
) -> SecsStereoDesign {
    design_secs_stereo_filter(left, right, config, candidates, None).expect("original design")
}

// --- [1] ---

/// Worst deviation (dB, signed) of the 50-600 Hz 1/3-octave bands of a
/// filter from the mean of its 1-5 kHz 1/3-octave bands, and where.
fn flatness_50_600(taps: &[f64]) -> (f64, f64) {
    let spectrum = fft(taps, FFT_N);
    let reference_bands = third_octave_centers(1000.0, 5000.0);
    let reference = reference_bands
        .iter()
        .map(|fc| third_octave_db(&spectrum, FFT_N, *fc))
        .sum::<f64>()
        / reference_bands.len() as f64;
    let mut worst = (0.0f64, 0.0f64);
    for fc in third_octave_centers(50.0, 600.0) {
        let deviation = third_octave_db(&spectrum, FFT_N, fc) - reference;
        if deviation.abs() > worst.1.abs() {
            worst = (fc, deviation);
        }
    }
    worst
}

#[test]
fn fix1_a_flat_speaker_gets_a_flat_filter_in_every_resolution_mode() {
    let ir = delta(N, ONSET, 1.0);
    for resolution in [
        SecsResolutionMode::Gentle,
        SecsResolutionMode::Balanced,
        SecsResolutionMode::Precise,
    ] {
        let mut config = base_config(10.0);
        config.resolution = resolution;
        let fixed = edited(&ir, &ir, &config, &SecsEditedOptions::default(), None);
        let legacy = original(&ir, &ir, &config, None);
        let (fixed_at, fixed_dev) = flatness_50_600(&fixed.left_taps);
        let (legacy_at, legacy_dev) = flatness_50_600(&legacy.left_taps);
        println!(
            "[1] {resolution:?}: edited worst {fixed_dev:+.3} dB @ {fixed_at:.0} Hz | original worst {legacy_dev:+.3} dB @ {legacy_at:.0} Hz (limit +-0.2)"
        );
        assert!(
            fixed_dev.abs() <= 0.2,
            "[1] {resolution:?}: edited filter deviates {fixed_dev:+.3} dB at {fixed_at:.0} Hz"
        );
        assert!(
            legacy_dev.abs() > 0.2,
            "[1] {resolution:?}: the original no longer shows the defect ({legacy_dev:+.3} dB)"
        );
    }
}

// --- [2] ---

/// Filter max raw |H| over 30-400 Hz minus the median raw |H| over
/// 600-1200 Hz, dB.
fn boost_over_mid(taps: &[f64]) -> f64 {
    let spectrum = fft(taps, FFT_N);
    let df = FS / FFT_N as f64;
    let mut max_low = f64::NEG_INFINITY;
    let mut mid = Vec::new();
    for (k, value) in spectrum.iter().enumerate().take(FFT_N / 2).skip(1) {
        let f = k as f64 * df;
        let db = 20.0 * value.norm().max(1e-300).log10();
        if (30.0..=400.0).contains(&f) {
            max_low = max_low.max(db);
        }
        if (600.0..=1200.0).contains(&f) {
            mid.push(db);
        }
    }
    max_low - median(mid)
}

#[test]
fn fix2_the_boost_ceiling_bounds_the_filter_gain_in_a_reflection_null() {
    let mut ir = delta(N, ONSET, 1.0);
    ir[ONSET + 240] += 0.85; // 5 ms reflection: -18 dB null at 100 Hz.
    let legacy_anchor = SecsEditedOptions {
        correction_upper_hz: 20_000.0,
        ..SecsEditedOptions::default()
    };
    for (boost, limit) in [(0.0, 0.5), (3.0, 3.5)] {
        let mut config = base_config(10.0);
        config.max_boost_db = boost;
        let fixed = boost_over_mid(
            &edited(&ir, &ir, &config, &SecsEditedOptions::default(), None).left_taps,
        );
        let legacy = boost_over_mid(&original(&ir, &ir, &config, None).left_taps);
        let reported = boost_over_mid(&edited(&ir, &ir, &config, &legacy_anchor, None).left_taps);
        println!(
            "[2] Boost {boost}: edited {fixed:+.2} dB | edited with upper=20000 (report only) {reported:+.2} dB | original {legacy:+.2} dB (limit {limit:+.1})"
        );
        assert!(
            fixed <= limit,
            "[2] Boost {boost}: edited filter gain {fixed:+.2} dB"
        );
        assert!(
            legacy > limit,
            "[2] Boost {boost}: the original no longer shows the defect ({legacy:+.2} dB)"
        );
    }
}

// --- [3] ---

/// (worst 1-16 kHz 1/3-octave deviation from the 1 kHz band with its
/// center, 50-400 Hz power average re the 1 kHz band), dB.
fn treble_and_bass_re_1k(taps: &[f64]) -> ((f64, f64), f64) {
    let spectrum = fft(taps, FFT_N);
    let at_1k = third_octave_db(&spectrum, FFT_N, 1000.0);
    let mut worst = (0.0f64, 0.0f64);
    for fc in third_octave_centers(1000.0, 16_000.0) {
        let deviation = third_octave_db(&spectrum, FFT_N, fc) - at_1k;
        if deviation.abs() > worst.1.abs() {
            worst = (fc, deviation);
        }
    }
    let bass = 10.0 * band_power(&spectrum, FFT_N, 50.0, 400.0).log10() - at_1k;
    (worst, bass)
}

#[test]
fn fix3_the_macro_eq_leaves_the_reverberant_treble_alone() {
    let ir = sloped_room(N, ONSET, -1.2, 6.0, 0.4, 7);
    let ceiling = secs_edited_auto_delay_ceiling_ms(&ir, &ir, 48_000).unwrap();
    let config = base_config(ceiling.ceiling_ms);
    let fixed = edited(&ir, &ir, &config, &SecsEditedOptions::default(), None);
    let legacy = original(&ir, &ir, &base_config(10.0), None);
    let ((fixed_at, fixed_hf), fixed_bass) = treble_and_bass_re_1k(&fixed.left_taps);
    let ((legacy_at, legacy_hf), legacy_bass) = treble_and_bass_re_1k(&legacy.left_taps);
    println!(
        "[3] ceiling {} ms (requirement {} ms): edited HF worst {fixed_hf:+.2} dB @ {fixed_at:.0} Hz, 50-400 Hz {fixed_bass:+.2} dB | original HF worst {legacy_hf:+.2} dB @ {legacy_at:.0} Hz, 50-400 Hz {legacy_bass:+.2} dB (limits +-0.7 / +-3)",
        ceiling.ceiling_ms, ceiling.requirement_ms
    );
    assert!(
        fixed_hf.abs() <= 0.7,
        "[3] edited treble deviates {fixed_hf:+.2} dB at {fixed_at:.0} Hz"
    );
    assert!(
        fixed_bass.abs() <= 3.0,
        "[3] edited 50-400 Hz level {fixed_bass:+.2} dB re 1 kHz"
    );
    assert!(
        legacy_hf.abs() > 0.7 || legacy_bass.abs() > 3.0,
        "[3] the original no longer shows the defect"
    );
}

// --- [4] ---

/// (40 Hz excess group delay re the 1-3 kHz median, ms; pre-ring energy
/// share of the corrected system, dB) of `ir` convolved with `taps`.
fn timing(ir: &[f64], taps: &[f64]) -> (f64, f64) {
    let spectrum = convolve_spectrum(ir, taps, FFT_N);
    let excess = excess_group_delay_ms(&spectrum);
    let df = FS / FFT_N as f64;
    let pick = |lo: f64, hi: f64| -> Vec<f64> {
        (0..excess.len())
            .filter(|k| {
                let f = *k as f64 * df;
                f >= lo && f <= hi
            })
            .map(|k| excess[k])
            .collect()
    };
    let reference = median(pick(1000.0, 3000.0));
    let at_40 = pick(39.0, 41.0);
    let gd_40 = at_40.iter().sum::<f64>() / at_40.len() as f64 - reference;
    let system = ifft(spectrum);
    (gd_40, pre_ring_db(&system[..ir.len() + taps.len()]))
}

#[test]
fn fix4_the_delay_budget_corrects_the_low_band_timing() {
    let ir = anechoic_bass_managed(N, ONSET);
    let ceiling = secs_edited_auto_delay_ceiling_ms(&ir, &ir, 48_000).unwrap();
    println!(
        "[4] requirement {} ms -> edited ceiling {} ms",
        ceiling.requirement_ms, ceiling.ceiling_ms
    );
    assert_eq!(ceiling.ceiling_ms, 20.0, "[4] expected a 20 ms budget");
    let config = base_config(ceiling.ceiling_ms);
    let fixed = edited(&ir, &ir, &config, &SecsEditedOptions::default(), None);

    let mut shipped = base_config(10.0);
    shipped.phase_guard = false;
    let legacy = original(&ir, &ir, &shipped, None);

    let mut minimum = config.clone();
    minimum.low_latency = true;
    let minimum_phase = edited(&ir, &ir, &minimum, &SecsEditedOptions::default(), None);

    let (fixed_gd, fixed_pre) = timing(&ir, &fixed.left_taps);
    let (legacy_gd, legacy_pre) = timing(&ir, &legacy.left_taps);
    let (minimum_gd, minimum_pre) = timing(&ir, &minimum_phase.left_taps);
    let (raw_gd, _) = timing(&ir, &delta(1, 0, 1.0));

    let spectrum = fft(&fixed.left_taps, FFT_N);
    let at_1k = third_octave_db(&spectrum, FFT_N, 1000.0);
    let lf: Vec<(f64, f64)> = third_octave_centers(25.0, 63.0)
        .into_iter()
        .map(|fc| (fc, third_octave_db(&spectrum, FFT_N, fc) - at_1k))
        .collect();
    let legacy_spectrum = fft(&legacy.left_taps, FFT_N);
    let legacy_1k = third_octave_db(&legacy_spectrum, FFT_N, 1000.0);
    let legacy_lf: Vec<String> = third_octave_centers(25.0, 63.0)
        .into_iter()
        .map(|fc| {
            format!(
                "{fc:.1}:{:+.2}",
                third_octave_db(&legacy_spectrum, FFT_N, fc) - legacy_1k
            )
        })
        .collect();
    println!(
        "[4] uncorrected GD40 {raw_gd:+.2} ms | edited (delay {} ms) GD40 {fixed_gd:+.2} ms, pre-ring {fixed_pre:.1} dB | original auto (picked {} ms) GD40 {legacy_gd:+.2} ms, pre-ring {legacy_pre:.1} dB | min-phase GD40 {minimum_gd:+.2} ms, pre-ring {minimum_pre:.1} dB",
        fixed.auto_delay_ms, legacy.auto_delay_ms
    );
    println!(
        "[4] edited LF re 1 kHz: {:?} | original LF: [{}] (limit +-1.0)",
        lf.iter()
            .map(|(fc, db)| format!("{fc:.1}:{db:+.2}"))
            .collect::<Vec<_>>(),
        legacy_lf.join(" ")
    );
    assert!(
        fixed_gd <= 1.5,
        "[4] edited 40 Hz excess GD {fixed_gd:+.2} ms"
    );
    assert!(
        fixed_pre <= legacy_pre + 1.0,
        "[4] edited pre-ring {fixed_pre:.1} dB vs original {legacy_pre:.1} dB"
    );
    for (fc, db) in &lf {
        assert!(
            db.abs() <= 1.0,
            "[4] edited filter {fc:.1} Hz band {db:+.2} dB re 1 kHz"
        );
    }
    assert!(
        legacy_gd > 1.5,
        "[4] the original no longer shows the defect (GD40 {legacy_gd:+.2} ms)"
    );
}

// --- [5] ---

struct CropCheck {
    main_taps_present: bool,
    offset_error_samples: i64,
    energy_ratio_db: f64,
    rms_scale: f64,
    trim_db: f64,
}

impl CropCheck {
    fn of(design: &SecsStereoDesign, expected_offset_samples: i64) -> Self {
        let (left_peak, right_peak) = (
            argmax_abs(&design.left_taps),
            argmax_abs(&design.right_taps),
        );
        let (left_max, right_max) = (
            design.left_taps[left_peak].abs(),
            design.right_taps[right_peak].abs(),
        );
        Self {
            main_taps_present: left_max >= 0.5 * right_max && right_max >= 0.5 * left_max,
            offset_error_samples: (left_peak as i64 - right_peak as i64) - expected_offset_samples,
            energy_ratio_db: 10.0
                * (energy(&design.right_taps) / energy(&design.left_taps)).log10(),
            rms_scale: design.rms_scale,
            trim_db: design.channel_balance_trim_db,
        }
    }

    fn level_ok(&self) -> bool {
        let expected = 20.0 * (1.0f64 / 1.25).log10();
        (self.energy_ratio_db - expected).abs() <= 0.5
            && (self.rms_scale - 0.8).abs() <= 1e-6
            && self.trim_db.abs() <= 0.1
    }

    fn all_ok(&self) -> bool {
        self.main_taps_present && self.offset_error_samples.abs() <= 1 && self.level_ok()
    }

    fn describe(&self) -> String {
        format!(
            "main taps {}, offset error {} samples, R/L energy {:+.2} dB (want -1.94 +-0.5), rms_scale {:.4}, trim {:+.2} dB",
            self.main_taps_present,
            self.offset_error_samples,
            self.energy_ratio_db,
            self.rms_scale,
            self.trim_db
        )
    }
}

#[test]
fn fix5_the_stereo_crop_keeps_both_main_taps() {
    let left = delta(N, ONSET, 1.0);
    let mut original_failures = Vec::new();
    for d_ms in [0.5, 3.0, 8.0] {
        let d_samples = (d_ms * 48.0f64).round() as usize;
        let right = delta(N, ONSET + d_samples, 1.25);
        for delay_ms in [2.0, 10.0] {
            let config = base_config(10.0);
            let candidates = [delay_ms];
            let fixed = CropCheck::of(
                &edited(
                    &left,
                    &right,
                    &config,
                    &SecsEditedOptions::default(),
                    Some(&candidates),
                ),
                d_samples as i64,
            );
            let legacy = CropCheck::of(
                &original(&left, &right, &config, Some(&candidates)),
                d_samples as i64,
            );
            println!(
                "[5] D {d_ms} ms, delay {delay_ms} ms\n    edited:   {}\n    original: {}",
                fixed.describe(),
                legacy.describe()
            );
            assert!(
                fixed.all_ok(),
                "[5] D {d_ms} ms delay {delay_ms} ms: {}",
                fixed.describe()
            );
            if !legacy.all_ok() {
                original_failures.push(format!("D {d_ms} / delay {delay_ms}"));
            }
            // The measured Step-1 failure: arrival difference beyond the delay.
            if d_ms == 3.0 && delay_ms == 2.0 {
                assert!(
                    !legacy.all_ok(),
                    "[5] the original no longer shows the defect"
                );
            }
        }
    }
    let right = delta(N, ONSET + 144, 1.25);
    for (label, low, zero) in [("low-latency", true, false), ("zero-latency", false, true)] {
        let mut config = base_config(10.0);
        config.low_latency = low;
        config.zero_latency = zero;
        let fixed = edited(&left, &right, &config, &SecsEditedOptions::default(), None);
        let legacy = original(&left, &right, &config, None);
        // Low latency aligns the arrivals (left shifted by D); zero latency
        // leaves both filters at their own onset.
        let offset = if low { 144 } else { 0 };
        let (fixed_check, legacy_check) = (
            CropCheck::of(&fixed, offset),
            CropCheck::of(&legacy, offset),
        );
        println!(
            "[5] {label} D 3 ms\n    edited:   {}\n    original: {}",
            fixed_check.describe(),
            legacy_check.describe()
        );
        assert!(
            fixed_check.level_ok(),
            "[5] {label}: {}",
            fixed_check.describe()
        );
        if !legacy_check.level_ok() {
            original_failures.push(label.to_string());
        }
        if low {
            assert!(
                !legacy_check.level_ok(),
                "[5] low-latency: the original no longer shows the defect"
            );
        }
    }
    println!("[5] original fails in: {original_failures:?}");
}

// --- [6] ---

/// Nine seats: direct sound, a floor reflection at 2.5 ms with seat jitter
/// N(0, 0.25 ms) (seat 0 unjittered), an independent diffuse tail per seat
/// and one shared decaying 45 Hz room mode. Weights: P0 = 2, others 1.
fn nine_seats() -> (Vec<Vec<f64>>, Vec<f64>) {
    let mut rng = Rng::new(42);
    let mode = decaying_mode(N, ONSET, 45.0, 0.25 * std::f64::consts::PI * 45.0, 3.6);
    let seats = (0..9u64)
        .map(|seat| {
            let jitter_ms = if seat == 0 { 0.0 } else { 0.25 * rng.normal() };
            let mut ir = sloped_room(N, ONSET, 0.0, 0.0, 0.3, 1_000 + seat);
            let reflection = ONSET as f64 + (2.5 + jitter_ms) * 48.0;
            ir[reflection.round() as usize] += 0.5;
            for (sample, add) in ir.iter_mut().zip(mode.iter()) {
                *sample += add;
            }
            ir
        })
        .collect();
    let weights = (0..9)
        .map(|seat| if seat == 0 { 2.0 } else { 1.0 })
        .collect();
    (seats, weights)
}

/// Seat-weighted 1/3-octave power average of |H_i F|^2 at `center`, dB.
fn seat_average_db(seat_spectra: &[Vec<C64>], weights: &[f64], filter: &[C64], center: f64) -> f64 {
    let total: f64 = weights.iter().sum();
    let averaged: Vec<C64> = (0..FFT_N)
        .map(|k| {
            let power: f64 = seat_spectra
                .iter()
                .zip(weights.iter())
                .map(|(spectrum, weight)| weight * (spectrum[k] * filter[k]).norm_sqr())
                .sum::<f64>()
                / total;
            C64::new(power.sqrt(), 0.0)
        })
        .collect();
    third_octave_db(&averaged, FFT_N, center)
}

#[test]
fn fix6_the_power_averaged_design_lands_on_target_at_every_seat_on_average() {
    let (seats, weights) = nine_seats();
    let total: f64 = weights.iter().sum();
    let own: Vec<Vec<C64>> = seats.iter().map(|ir| fft(ir, N)).collect();
    let power_average: Vec<f64> = (0..N)
        .map(|k| {
            (own.iter()
                .zip(weights.iter())
                .map(|(spectrum, weight)| weight * spectrum[k].norm_sqr())
                .sum::<f64>()
                / total)
                .sqrt()
        })
        .collect();
    let vector_average: Vec<f64> = (0..N)
        .map(|t| {
            seats
                .iter()
                .zip(weights.iter())
                .map(|(ir, weight)| weight * ir[t])
                .sum::<f64>()
                / total
        })
        .collect();
    let config = base_config(10.0);
    let options = SecsEditedOptions::default();
    let power_design = design_secs_stereo_filter_edited(
        &seats[0],
        &seats[0],
        &config,
        &options,
        None,
        Some((&power_average, &power_average)),
    )
    .expect("power-average design");
    let vector_design = edited(&vector_average, &vector_average, &config, &options, None);

    let seat_spectra: Vec<Vec<C64>> = seats.iter().map(|ir| fft(ir, FFT_N)).collect();
    let power_filter = fft(&unnormalized_left(&power_design), FFT_N);
    let vector_filter = fft(&unnormalized_left(&vector_design), FFT_N);
    let centers: Vec<f64> = (-14..=-10)
        .map(|k| 1000.0 * 2f64.powf(f64::from(k) / 3.0))
        .collect();
    let mut worst_power = 0.0f64;
    let mut worst_vector = 0.0f64;
    for center in &centers {
        // One target for both designs: the power-average design's, i.e. the
        // target anchored to what the seats really hear. (The vector
        // design's own target is anchored to a level the seats never hear.)
        let target = target_db_at(&power_design, *center);
        let power = seat_average_db(&seat_spectra, &weights, &power_filter, *center) - target;
        let vector = seat_average_db(&seat_spectra, &weights, &vector_filter, *center) - target;
        let vector_own = seat_average_db(&seat_spectra, &weights, &vector_filter, *center)
            - target_db_at(&vector_design, *center);
        println!(
            "[6] {center:6.1} Hz: power-average design {power:+.2} dB | vector-average design {vector:+.2} dB (vs its own target {vector_own:+.2} dB) (limit +-2.5)"
        );
        assert!(
            power.abs() <= 2.5,
            "[6] {center:.1} Hz: power-average design off target by {power:+.2} dB"
        );
        worst_power = worst_power.max(power.abs());
        worst_vector = worst_vector.max(vector.abs());
    }
    println!("[6] worst |deviation|: power average {worst_power:.2} dB, vector average {worst_vector:.2} dB");
    assert!(
        worst_vector > worst_power,
        "[6] negative control: the vector average is not worse ({worst_vector:.2} vs {worst_power:.2} dB)"
    );
}

// --- [8] ---

#[test]
fn fix8_the_crush_exponent_sets_how_far_narrow_peaks_are_pushed_down() {
    let mut ir = delta(N, ONSET, 1.0);
    for frequency_hz in [87.0, 143.0, 231.0] {
        for (sample, add) in ir
            .iter_mut()
            .zip(decaying_mode(N, ONSET, frequency_hz, 20.0, 3.0).iter())
        {
            *sample += add;
        }
    }
    let config = base_config(10.0);
    let df = FS / FFT_N as f64;
    let ir_spectrum = fft(&ir, FFT_N);
    let level = |crush_intensity: f64| -> f64 {
        let options = SecsEditedOptions {
            crush_intensity,
            ..SecsEditedOptions::default()
        };
        let design = edited(&ir, &ir, &config, &options, None);
        let filter = fft(&unnormalized_left(&design), FFT_N);
        let (mut system, mut target) = (0.0, 0.0);
        for k in 1..FFT_N / 2 {
            let f = k as f64 * df;
            if (100.0..=400.0).contains(&f) {
                system += (ir_spectrum[k] * filter[k]).norm_sqr();
                target += 10f64.powf(target_db_at(&design, f) / 10.0);
            }
        }
        10.0 * (system / target).log10()
    };
    let (none, gentle, balanced) = (level(0.0), level(0.3), level(0.6));
    println!(
        "[8] 100-400 Hz power average re target: crush 0.0 {none:+.3} dB, 0.3 {gentle:+.3} dB, 0.6 {balanced:+.3} dB"
    );
    assert!(
        none > gentle && gentle > balanced,
        "[8] not monotonic: {none:+.3} / {gentle:+.3} / {balanced:+.3} dB"
    );
    assert!(
        gentle != balanced,
        "[8] the default equals the legacy Balanced crush"
    );
}

// --- [9] ---

#[test]
fn fix9_the_decay_score_is_the_linearity_score_under_its_honest_name() {
    for ir in [
        sloped_room(N, ONSET, -1.2, 6.0, 0.4, 7),
        anechoic_bass_managed(N, ONSET),
        delta(N, ONSET, 1.0),
    ] {
        let decay = secs_decay_score(&ir, 48_000);
        assert!(decay == secs_linearity_score(&ir, 48_000));
    }
}
