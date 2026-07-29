//! Reusable display and diagnostic smoothing on a logarithmic frequency axis.

use crate::{DspError, DspResult};

const GAUSSIAN_RADIUS_SIGMA: f64 = 4.0;

pub(crate) fn log_frequency_cell_weights(frequencies_hz: &[f64]) -> DspResult<Vec<f64>> {
    if frequencies_hz.len() < 2 {
        return Err(DspError::EmptyInput(
            "log-frequency smoothing frequency grid",
        ));
    }
    let mut logs = Vec::with_capacity(frequencies_hz.len());
    for (index, frequency) in frequencies_hz.iter().copied().enumerate() {
        if !frequency.is_finite()
            || frequency <= 0.0
            || (index > 0 && frequency <= frequencies_hz[index - 1])
        {
            return Err(DspError::InvalidArgument(
                "log-frequency smoothing frequencies must be finite, positive, and increasing"
                    .into(),
            ));
        }
        logs.push(frequency.ln());
    }
    let mut weights = Vec::with_capacity(logs.len());
    for index in 0..logs.len() {
        let weight = if index == 0 {
            (logs[1] - logs[0]) / 2.0
        } else if index + 1 == logs.len() {
            (logs[index] - logs[index - 1]) / 2.0
        } else {
            (logs[index + 1] - logs[index - 1]) / 2.0
        };
        if !weight.is_finite() || weight <= 0.0 {
            return Err(DspError::InvalidArgument(
                "log-frequency smoothing grid has an invalid cell width".into(),
            ));
        }
        weights.push(weight);
    }
    Ok(weights)
}

/// Gaussian-smooth dB values on a logarithmic frequency axis and evaluate the
/// result at the requested frequencies.
///
/// This is presentation/diagnostic processing only. It does not change filter
/// design arrays or verification thresholds.
pub fn gaussian_log_frequency_smooth_at_db(
    frequencies_hz: &[f64],
    values_db: &[f64],
    target_frequencies_hz: &[f64],
    fwhm_octaves: f64,
) -> DspResult<Vec<f64>> {
    if frequencies_hz.len() != values_db.len() {
        return Err(DspError::ShapeMismatch(
            "log-frequency smoothing frequency and value grids must match".into(),
        ));
    }
    if !fwhm_octaves.is_finite() || fwhm_octaves <= 0.0 {
        return Err(DspError::InvalidArgument(
            "log-frequency smoothing width must be finite and positive".into(),
        ));
    }
    if let Some(index) = values_db.iter().position(|value| !value.is_finite()) {
        return Err(DspError::NonFinite {
            context: "log-frequency smoothing values",
            index,
        });
    }
    for (index, target) in target_frequencies_hz.iter().copied().enumerate() {
        if !target.is_finite()
            || target <= 0.0
            || (index > 0 && target <= target_frequencies_hz[index - 1])
        {
            return Err(DspError::InvalidArgument(
                "log-frequency smoothing targets must be finite, positive, and increasing".into(),
            ));
        }
    }

    let cell_weights = log_frequency_cell_weights(frequencies_hz)?;
    let sigma_octaves = fwhm_octaves / (2.0 * (2.0 * std::f64::consts::LN_2).sqrt());
    let radius_ratio = 2.0_f64.powf(GAUSSIAN_RADIUS_SIGMA * sigma_octaves);
    let mut smoothed = Vec::with_capacity(target_frequencies_hz.len());
    for (target_index, target_hz) in target_frequencies_hz.iter().copied().enumerate() {
        let first =
            frequencies_hz.partition_point(|frequency| *frequency < target_hz / radius_ratio);
        let end =
            frequencies_hz.partition_point(|frequency| *frequency <= target_hz * radius_ratio);
        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;
        for index in first..end {
            let distance = (frequencies_hz[index] / target_hz).log2();
            let gaussian = (-0.5 * (distance / sigma_octaves).powi(2)).exp();
            let weight = gaussian * cell_weights[index];
            weighted_sum += weight * values_db[index];
            weight_sum += weight;
        }
        let value = weighted_sum / weight_sum;
        if !value.is_finite() {
            return Err(DspError::NonFinite {
                context: "log-frequency smoothing output",
                index: target_index,
            });
        }
        smoothed.push(value);
    }
    Ok(smoothed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_response_remains_constant_on_a_different_target_grid() {
        let frequencies = (0..1_000)
            .map(|index| 20.0 * 1_000.0_f64.powf(index as f64 / 999.0))
            .collect::<Vec<_>>();
        let values = vec![72.5; frequencies.len()];
        let targets = [20.0, 100.0, 1_000.0, 10_000.0, 20_000.0];
        let smoothed =
            gaussian_log_frequency_smooth_at_db(&frequencies, &values, &targets, 1.0 / 12.0)
                .unwrap();
        assert!(smoothed.iter().all(|value| (*value - 72.5).abs() < 1.0e-12));
    }

    #[test]
    fn one_twelfth_octave_suppresses_narrow_display_ripple() {
        let frequencies = (0..2_000)
            .map(|index| 20.0 * 1_000.0_f64.powf(index as f64 / 1_999.0))
            .collect::<Vec<_>>();
        let values = frequencies
            .iter()
            .map(|frequency| 70.0 + 5.0 * (240.0 * (frequency / 1_000.0).log2()).sin())
            .collect::<Vec<_>>();
        let smoothed =
            gaussian_log_frequency_smooth_at_db(&frequencies, &values, &frequencies, 1.0 / 12.0)
                .unwrap();
        let raw_span = values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - values.iter().copied().fold(f64::INFINITY, f64::min);
        let smoothed_span = smoothed.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - smoothed.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(smoothed_span < raw_span * 0.25);
    }

    #[test]
    fn invalid_grids_fail_closed() {
        assert!(gaussian_log_frequency_smooth_at_db(&[], &[], &[100.0], 1.0 / 12.0).is_err());
        assert!(
            gaussian_log_frequency_smooth_at_db(&[20.0, 20.0], &[0.0; 2], &[20.0], 1.0 / 12.0)
                .is_err()
        );
        assert!(
            gaussian_log_frequency_smooth_at_db(&[20.0, 30.0], &[0.0, f64::NAN], &[20.0], 0.1)
                .is_err()
        );
    }
}
