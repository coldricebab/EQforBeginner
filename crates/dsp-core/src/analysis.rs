//! FFT-based impulse-response analysis.

use crate::{DspError, DspResult};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

const MAGNITUDE_FLOOR: f64 = 1.0e-15;

/// One-sided frequency response including DC and Nyquist.
#[derive(Debug, Clone, PartialEq)]
pub struct FrequencyResponse {
    pub sample_rate_hz: u32,
    pub fft_size: usize,
    pub frequencies_hz: Vec<f64>,
    pub magnitude_db: Vec<f64>,
    pub phase_rad: Vec<f64>,
}

impl FrequencyResponse {
    #[must_use]
    pub fn bin_hz(&self) -> f64 {
        f64::from(self.sample_rate_hz) / self.fft_size as f64
    }
}

/// Analyze a real impulse response on an explicitly selected FFT grid.
pub fn frequency_response(
    impulse: &[f64],
    sample_rate_hz: u32,
    fft_size: usize,
) -> DspResult<FrequencyResponse> {
    if impulse.is_empty() {
        return Err(DspError::EmptyInput("impulse response"));
    }
    if sample_rate_hz == 0 {
        return Err(DspError::InvalidArgument(
            "sample rate must be greater than zero".into(),
        ));
    }
    if !fft_size.is_power_of_two() || fft_size < impulse.len() || fft_size < 2 {
        return Err(DspError::InvalidArgument(format!(
            "FFT size {fft_size} must be a power of two and at least the impulse length {}",
            impulse.len()
        )));
    }
    if let Some(index) = impulse.iter().position(|value| !value.is_finite()) {
        return Err(DspError::NonFinite {
            context: "impulse response",
            index,
        });
    }

    let mut spectrum = vec![Complex::new(0.0, 0.0); fft_size];
    for (bin, &sample) in spectrum.iter_mut().zip(impulse) {
        bin.re = sample;
    }
    FftPlanner::<f64>::new()
        .plan_fft_forward(fft_size)
        .process(&mut spectrum);

    if let Some(index) = spectrum
        .iter()
        .position(|value| !value.re.is_finite() || !value.im.is_finite())
    {
        return Err(DspError::NonFinite {
            context: "FFT spectrum",
            index,
        });
    }

    let one_sided_len = fft_size / 2 + 1;
    let bin_hz = f64::from(sample_rate_hz) / fft_size as f64;
    let mut frequencies_hz = Vec::with_capacity(one_sided_len);
    let mut magnitude_db = Vec::with_capacity(one_sided_len);
    let mut phase_rad = Vec::with_capacity(one_sided_len);

    for (index, value) in spectrum.iter().take(one_sided_len).enumerate() {
        let magnitude = value.norm();
        if !magnitude.is_finite() {
            return Err(DspError::NonFinite {
                context: "FFT magnitude",
                index,
            });
        }
        let level_db = 20.0 * magnitude.max(MAGNITUDE_FLOOR).log10();
        let phase = value.arg();
        if !level_db.is_finite() || !phase.is_finite() {
            return Err(DspError::NonFinite {
                context: "frequency response",
                index,
            });
        }
        frequencies_hz.push(index as f64 * bin_hz);
        magnitude_db.push(level_db);
        phase_rad.push(phase);
    }

    Ok(FrequencyResponse {
        sample_rate_hz,
        fft_size,
        frequencies_hz,
        magnitude_db,
        phase_rad,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impulse_has_flat_fft_response() {
        let mut impulse = vec![0.0; 64];
        impulse[7] = 0.5;
        let response = frequency_response(&impulse, 48_000, 128).unwrap();
        let expected_db = 20.0 * 0.5_f64.log10();
        for level in response.magnitude_db {
            assert!((level - expected_db).abs() < 1.0e-12);
        }
    }

    #[test]
    fn rejects_an_fft_shorter_than_the_ir() {
        let error = frequency_response(&[1.0; 9], 48_000, 8).unwrap_err();
        assert!(error.to_string().contains("at least the impulse length"));
    }

    #[test]
    fn rejects_non_finite_impulse_samples() {
        for sample in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = frequency_response(&[sample], 48_000, 2).unwrap_err();
            assert_eq!(
                error,
                DspError::NonFinite {
                    context: "impulse response",
                    index: 0,
                }
            );
        }
    }

    #[test]
    fn rejects_invalid_fft_and_sample_rate_ranges() {
        assert!(matches!(
            frequency_response(&[1.0], 0, 2),
            Err(DspError::InvalidArgument(_))
        ));
        assert!(matches!(
            frequency_response(&[1.0], 48_000, 3),
            Err(DspError::InvalidArgument(_))
        ));
    }

    #[test]
    fn rejects_fft_overflow_from_finite_max_samples() {
        let error = frequency_response(&[f64::MAX, f64::MAX], 48_000, 2).unwrap_err();
        assert!(matches!(
            error,
            DspError::NonFinite {
                context: "FFT spectrum" | "FFT magnitude",
                ..
            }
        ));
    }

    #[test]
    fn rejects_non_finite_magnitude_from_finite_fft_components() {
        let half_max = f64::MAX / 2.0;
        let error =
            frequency_response(&[half_max, -half_max, -half_max, half_max], 48_000, 4).unwrap_err();
        assert_eq!(
            error,
            DspError::NonFinite {
                context: "FFT magnitude",
                index: 1,
            }
        );
    }
}
