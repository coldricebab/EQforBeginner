//! Minimum-phase FIR synthesis from a one-sided magnitude response.

use crate::correction::{validate_correction_invariant, MAXIMUM_SUPPORTED_BOOST_DB};
use crate::{DspError, DspResult};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

const LOG_TEN_OVER_TWENTY: f64 = std::f64::consts::LN_10 / 20.0;
const UNITY_SAFETY_AMPLITUDE: f64 = 0.999_999;
const OVERSAMPLE_FACTOR: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub struct FirFilter {
    pub sample_rate_hz: u32,
    pub taps: Vec<f64>,
    /// Magnitude requested on the native design grid before the tiny bounded
    /// gain safety normalization.
    pub design_gain_db: Vec<f64>,
    pub safety_normalization_db: f64,
}

fn one_sided_fft_db(taps: &[f64], fft_size: usize) -> DspResult<Vec<f64>> {
    if fft_size < taps.len() || fft_size < 4 || fft_size % 2 != 0 {
        return Err(DspError::InvalidArgument(
            "response FFT must be even, at least four samples, and at least as long as the FIR"
                .into(),
        ));
    }
    let mut spectrum = vec![Complex::new(0.0, 0.0); fft_size];
    for (value, &tap) in spectrum.iter_mut().zip(taps) {
        value.re = tap;
    }
    FftPlanner::<f64>::new()
        .plan_fft_forward(fft_size)
        .process(&mut spectrum);
    Ok(spectrum
        .iter()
        .take(fft_size / 2 + 1)
        .map(|value| 20.0 * value.norm().max(1.0e-300).log10())
        .collect())
}

/// Real-cepstrum spectral factorization. The returned FIR has the same length
/// as the implied two-sided design FFT and is causal/minimum phase, with its
/// main energy at the beginning rather than shifted to the center.
pub fn synthesize_minimum_phase(gain_db: &[f64], sample_rate_hz: u32) -> DspResult<FirFilter> {
    validate_correction_invariant(gain_db)?;
    if sample_rate_hz == 0 {
        return Err(DspError::InvalidArgument(
            "sample rate must be greater than zero".into(),
        ));
    }
    if gain_db.len() < 3 {
        return Err(DspError::InvalidArgument(
            "minimum-phase design needs DC, at least one interior bin, and Nyquist".into(),
        ));
    }
    let fft_size = 2 * (gain_db.len() - 1);
    if fft_size < 4 || fft_size % 2 != 0 {
        return Err(DspError::InvalidArgument(format!(
            "one-sided gain length {} implies invalid odd or undersized FFT size {fft_size}",
            gain_db.len()
        )));
    }

    let mut log_spectrum = vec![Complex::new(0.0, 0.0); fft_size];
    for (index, &level_db) in gain_db.iter().enumerate() {
        log_spectrum[index].re = level_db * LOG_TEN_OVER_TWENTY;
    }
    for index in (fft_size / 2 + 1)..fft_size {
        log_spectrum[index].re = log_spectrum[fft_size - index].re;
    }

    FftPlanner::<f64>::new()
        .plan_fft_inverse(fft_size)
        .process(&mut log_spectrum);
    let scale = 1.0 / fft_size as f64;
    for value in &mut log_spectrum {
        *value *= scale;
    }

    let mut minimum_phase_cepstrum = vec![Complex::new(0.0, 0.0); fft_size];
    minimum_phase_cepstrum[0].re = log_spectrum[0].re;
    for index in 1..(fft_size / 2) {
        minimum_phase_cepstrum[index].re = 2.0 * log_spectrum[index].re;
    }
    minimum_phase_cepstrum[fft_size / 2].re = log_spectrum[fft_size / 2].re;

    FftPlanner::<f64>::new()
        .plan_fft_forward(fft_size)
        .process(&mut minimum_phase_cepstrum);
    for value in &mut minimum_phase_cepstrum {
        let amplitude = value.re.exp();
        *value = Complex::new(amplitude * value.im.cos(), amplitude * value.im.sin());
    }
    FftPlanner::<f64>::new()
        .plan_fft_inverse(fft_size)
        .process(&mut minimum_phase_cepstrum);
    let mut taps: Vec<f64> = minimum_phase_cepstrum
        .iter()
        .map(|value| value.re * scale)
        .collect();
    if let Some(index) = taps.iter().position(|tap| !tap.is_finite()) {
        return Err(DspError::NonFinite {
            context: "minimum-phase FIR",
            index,
        });
    }

    // Trigonometric interpolation can peak between native design bins. A dense
    // check followed by attenuation-only normalization preserves the bounded
    // gain invariant without ever lifting the requested response. All-cut
    // designs retain the original unity ceiling; boost designs use +3 dB.
    let check_fft_size = fft_size
        .checked_mul(OVERSAMPLE_FACTOR)
        .ok_or_else(|| DspError::InvalidArgument("FIR oversampling size overflow".into()))?;
    let response_db = one_sided_fft_db(&taps, check_fft_size)?;
    let maximum_db = response_db
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let requested_ceiling_db = if gain_db.iter().any(|gain| *gain > 0.0) {
        MAXIMUM_SUPPORTED_BOOST_DB
    } else {
        0.0
    };
    let safety_ceiling_db = requested_ceiling_db + 20.0 * UNITY_SAFETY_AMPLITUDE.log10();
    let safety_normalization_db = (safety_ceiling_db - maximum_db).min(0.0);
    if safety_normalization_db < 0.0 {
        let normalization = 10.0_f64.powf(safety_normalization_db / 20.0);
        for tap in &mut taps {
            *tap *= normalization;
        }
    }

    let verified = one_sided_fft_db(&taps, check_fft_size)?;
    if let Some((index, gain)) = verified
        .iter()
        .enumerate()
        .find(|(_, gain)| **gain > MAXIMUM_SUPPORTED_BOOST_DB + 1.0e-9)
    {
        return Err(DspError::InvalidArgument(format!(
            "realized FIR exceeds the +{MAXIMUM_SUPPORTED_BOOST_DB:.1} dB safety ceiling at oversampled bin {index}: {gain:.9} dB"
        )));
    }

    Ok(FirFilter {
        sample_rate_hz,
        taps,
        design_gain_db: gain_db.to_vec(),
        safety_normalization_db,
    })
}

impl FirFilter {
    pub fn response_db(&self, fft_size: usize) -> DspResult<Vec<f64>> {
        one_sided_fft_db(&self.taps, fft_size)
    }

    #[must_use]
    pub fn energy_centroid_samples(&self) -> f64 {
        let total: f64 = self.taps.iter().map(|tap| tap * tap).sum();
        if total == 0.0 {
            return 0.0;
        }
        self.taps
            .iter()
            .enumerate()
            .map(|(index, tap)| index as f64 * tap * tap)
            .sum::<f64>()
            / total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_design_is_a_front_loaded_normalized_impulse() {
        let filter = synthesize_minimum_phase(&vec![0.0; 513], 48_000).unwrap();
        assert!(filter.taps[0] > 0.999);
        assert!(filter.taps[1..].iter().all(|tap| tap.abs() < 1.0e-12));
        assert_eq!(filter.energy_centroid_samples(), 0.0);
        let response = filter.response_db(16_384).unwrap();
        assert!(response.iter().all(|gain| *gain <= 1.0e-9));
    }

    #[test]
    fn all_cut_shaped_design_remains_below_unity_and_minimum_phase() {
        let mut gain = vec![0.0; 513];
        for value in &mut gain[3..30] {
            *value = -8.0;
        }
        let filter = synthesize_minimum_phase(&gain, 48_000).unwrap();
        let response = filter.response_db(16_384).unwrap();
        assert!(response.iter().all(|value| *value <= 1.0e-9));
        assert!(filter.energy_centroid_samples() < 8.0);
        assert!(filter.taps.iter().all(|tap| tap.is_finite()));

        let native_response = filter.response_db(1_024).unwrap();
        for ((realized, requested), index) in native_response.iter().zip(&gain).zip(0..) {
            let expected = requested + filter.safety_normalization_db;
            assert!(
                (realized - expected).abs() < 1.0e-8,
                "native bin {index}: requested {requested} dB, normalization {} dB, realized {realized} dB",
                filter.safety_normalization_db
            );
        }
    }

    #[test]
    fn bounded_positive_requested_gain_is_preserved() {
        let mut gain = vec![0.0; 513];
        gain[10..30].fill(3.0);
        let filter = synthesize_minimum_phase(&gain, 48_000).unwrap();
        let response = filter.response_db(16_384).unwrap();
        assert!(response
            .iter()
            .all(|value| *value <= MAXIMUM_SUPPORTED_BOOST_DB + 1.0e-9));
        assert!(response.iter().any(|value| *value > 2.0));
    }

    #[test]
    fn gain_above_product_safety_ceiling_is_rejected() {
        let mut gain = vec![0.0; 513];
        gain[10] = MAXIMUM_SUPPORTED_BOOST_DB + 0.01;
        assert!(synthesize_minimum_phase(&gain, 48_000).is_err());
    }

    #[test]
    fn arbitrary_even_native_grid_is_supported() {
        let filter = synthesize_minimum_phase(&vec![0.0; 8_161], 48_000).unwrap();
        assert_eq!(filter.taps.len(), 16_320);
        let response = filter.response_db(32_640).unwrap();
        assert_eq!(response.len(), 16_321);
        assert!(response.iter().all(|gain| *gain <= 1.0e-9));
    }

    #[test]
    fn attenuation_beyond_product_safety_ceiling_is_rejected() {
        assert!(synthesize_minimum_phase(&vec![-100.0; 513], 48_000).is_err());
    }
}
