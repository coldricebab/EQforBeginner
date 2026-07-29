//! Deterministic synthetic room impulse responses used by numerical tests.
//!
//! The fixture contains broad, repeated modal peaks, seat-dependent narrow
//! dips, one isolated-seat anomaly, channel-specific upper-bass peaks, and
//! realistic per-position propagation delays. It is generated analytically in
//! the frequency domain and transformed to an actual real impulse response.

use crate::target::{TargetCurve, TargetPreset};
use crate::{DspError, DspResult};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticRoomFixture {
    pub sample_rate_hz: u32,
    pub fft_size: usize,
    pub position_labels: Vec<String>,
    pub left_impulses: Vec<Vec<f64>>,
    pub right_impulses: Vec<Vec<f64>>,
}

fn log_gaussian_db(frequency_hz: f64, center_hz: f64, fwhm_octaves: f64, level_db: f64) -> f64 {
    if frequency_hz <= 0.0 {
        return 0.0;
    }
    let distance = (frequency_hz / center_hz).log2();
    level_db * (-4.0 * std::f64::consts::LN_2 * (distance / fwhm_octaves).powi(2)).exp()
}

fn room_level_db(frequency_hz: f64, position: usize, is_right: bool) -> f64 {
    let target = TargetCurve::preset(TargetPreset::BkStyle);
    let safe_frequency = frequency_hz.max(1.0);
    let position_offset = position as f64 - 2.5;
    let mut level = target
        .level_at(safe_frequency)
        .expect("bundled target and finite fixture frequency are valid");

    // Repeated broad structures: deliberately similar, not identical, at all
    // six locations so the spatial lower-quantile detector admits them.
    level += log_gaussian_db(safe_frequency, 40.0, 0.24, 8.5 + 0.12 * position_offset);
    level += log_gaussian_db(safe_frequency, 55.0, 0.21, 7.5 - 0.10 * position_offset);
    level += log_gaussian_db(safe_frequency, 66.0, 0.19, 6.5 + 0.08 * position_offset);

    // Seat-dependent narrow cancellations. Each pair of seats has a different
    // center, making these poor candidates for spatial EQ and explicit dip
    // protection candidates.
    let dip_center = match position / 2 {
        0 => 83.0,
        1 => 103.0,
        _ => 119.0,
    } + if is_right { 0.8 } else { 0.0 };
    level += log_gaussian_db(safe_frequency, dip_center, 0.065, -15.0);

    // A single-location narrow peak must not drive the shared result.
    if position == 5 {
        level += log_gaussian_db(safe_frequency, 174.0, 0.055, 10.0);
    }

    // Above the common-sub region, the two main channels need distinct broad
    // cuts. Small deterministic ripples avoid an unrealistically exact model.
    if is_right {
        level += log_gaussian_db(safe_frequency, 330.0, 0.42, 5.0);
        // One explicit broad, shallow, spatially shared deficit so the demo
        // exercises the bounded-boost path deliberately instead of by
        // accident of anchor placement.
        level += log_gaussian_db(safe_frequency, 180.0, 0.35, -2.2);
    } else {
        level += log_gaussian_db(safe_frequency, 255.0, 0.40, 4.5);
    }
    level + 0.18 * (safe_frequency.log2() * 3.1 + position as f64 * 0.7).sin()
}

fn synthesize_impulse(
    sample_rate_hz: u32,
    fft_size: usize,
    position: usize,
    is_right: bool,
) -> Vec<f64> {
    let mut spectrum = vec![Complex::new(0.0, 0.0); fft_size];
    let delay_samples = 56 + position * 2 + usize::from(is_right) * 3;
    let bin_hz = f64::from(sample_rate_hz) / fft_size as f64;

    for (index, bin) in spectrum.iter_mut().enumerate().take(fft_size / 2 + 1) {
        let frequency = index as f64 * bin_hz;
        let amplitude = 10.0_f64.powf(room_level_db(frequency, position, is_right) / 20.0);
        let phase =
            -2.0 * std::f64::consts::PI * index as f64 * delay_samples as f64 / fft_size as f64;
        *bin = Complex::new(amplitude * phase.cos(), amplitude * phase.sin());
    }
    for index in (fft_size / 2 + 1)..fft_size {
        spectrum[index] = spectrum[fft_size - index].conj();
    }
    FftPlanner::<f64>::new()
        .plan_fft_inverse(fft_size)
        .process(&mut spectrum);
    let scale = 1.0 / fft_size as f64;
    spectrum.iter().map(|value| value.re * scale).collect()
}

impl SyntheticRoomFixture {
    /// The Phase 1 end-to-end fixture: P0 plus five surrounding positions at
    /// the product's default 48 kHz sample rate.
    pub fn phase1_48k() -> DspResult<Self> {
        let sample_rate_hz = 48_000;
        let fft_size = 16_384;
        let position_labels = vec![
            "P0 center".into(),
            "P1 left".into(),
            "P2 right".into(),
            "P3 forward".into(),
            "P4 back".into(),
            "P5 raised".into(),
        ];
        let left_impulses: Vec<Vec<f64>> = (0..position_labels.len())
            .map(|position| synthesize_impulse(sample_rate_hz, fft_size, position, false))
            .collect();
        let right_impulses: Vec<Vec<f64>> = (0..position_labels.len())
            .map(|position| synthesize_impulse(sample_rate_hz, fft_size, position, true))
            .collect();
        if left_impulses
            .iter()
            .chain(&right_impulses)
            .flatten()
            .any(|sample| !sample.is_finite())
        {
            return Err(DspError::InvalidArgument(
                "synthetic room generation produced a non-finite sample".into(),
            ));
        }
        Ok(Self {
            sample_rate_hz,
            fft_size,
            position_labels,
            left_impulses,
            right_impulses,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::frequency_response;

    fn bin_for(frequency_hz: f64, fixture: &SyntheticRoomFixture) -> usize {
        (frequency_hz * fixture.fft_size as f64 / f64::from(fixture.sample_rate_hz)).round()
            as usize
    }

    #[test]
    fn fixture_is_deterministic_and_contains_known_room_structures() {
        let first = SyntheticRoomFixture::phase1_48k().unwrap();
        let second = SyntheticRoomFixture::phase1_48k().unwrap();
        assert_eq!(first.left_impulses, second.left_impulses);
        assert_eq!(first.left_impulses.len(), 6);

        let p0 = frequency_response(&first.left_impulses[0], 48_000, first.fft_size).unwrap();
        let at_40 = p0.magnitude_db[bin_for(40.0, &first)];
        let at_30 = p0.magnitude_db[bin_for(30.0, &first)];
        let at_83 = p0.magnitude_db[bin_for(83.0, &first)];
        let at_75 = p0.magnitude_db[bin_for(75.0, &first)];
        assert!(at_40 > at_30 + 5.0, "40 Hz modal peak missing");
        assert!(at_83 < at_75 - 8.0, "83 Hz protected dip missing");
    }
}
