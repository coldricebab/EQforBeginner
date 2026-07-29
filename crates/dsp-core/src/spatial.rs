//! Multi-position statistics. Levels are combined as energy, never as an
//! arithmetic average of decibels.

use crate::{DspError, DspResult};

const DB_TO_NEPERS: f64 = std::f64::consts::LN_10 / 10.0;
const NEPERS_TO_DB: f64 = 10.0 / std::f64::consts::LN_10;

#[derive(Debug, Clone, PartialEq)]
pub struct SpatialSummary {
    pub energy_average_db: Vec<f64>,
    pub weighted_std_dev_db: Vec<f64>,
    pub lower_quartile_db: Vec<f64>,
    pub upper_quartile_db: Vec<f64>,
}

/// P0 receives weight two; every surrounding position receives weight one.
pub fn default_position_weights(position_count: usize) -> DspResult<Vec<f64>> {
    if position_count == 0 {
        return Err(DspError::EmptyInput("measurement positions"));
    }
    let mut weights = vec![1.0; position_count];
    weights[0] = 2.0;
    Ok(weights)
}

fn validate_responses(responses_db: &[Vec<f64>], weights: &[f64]) -> DspResult<usize> {
    if responses_db.is_empty() {
        return Err(DspError::EmptyInput("spatial responses"));
    }
    if responses_db.len() != weights.len() {
        return Err(DspError::ShapeMismatch(format!(
            "{} responses require the same number of weights, got {}",
            responses_db.len(),
            weights.len()
        )));
    }
    let bins = responses_db[0].len();
    if bins == 0 {
        return Err(DspError::EmptyInput("frequency bins"));
    }
    if responses_db.iter().any(|response| response.len() != bins) {
        return Err(DspError::ShapeMismatch(
            "all spatial responses must share one frequency grid".into(),
        ));
    }
    if weights
        .iter()
        .any(|weight| !weight.is_finite() || *weight <= 0.0)
    {
        return Err(DspError::InvalidArgument(
            "spatial weights must be finite and strictly positive".into(),
        ));
    }
    for response in responses_db {
        if let Some(index) = response.iter().position(|value| !value.is_finite()) {
            return Err(DspError::NonFinite {
                context: "spatial response",
                index,
            });
        }
    }
    Ok(bins)
}

/// Scale positive finite weights to `(0, 1]` where representable. The largest
/// weight is exactly one, so neither the normalization sum nor weighted
/// accumulation can overflow for any allocatable slice.
fn normalized_weights(weights: &[f64]) -> Vec<f64> {
    let maximum = weights.iter().copied().fold(0.0_f64, f64::max);
    weights.iter().map(|weight| weight / maximum).collect()
}

fn log_sum_exp(values: &[f64]) -> f64 {
    let maximum = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let relative_sum = values
        .iter()
        .map(|value| (value - maximum).exp())
        .sum::<f64>();
    maximum + relative_sum.ln()
}

/// `10 log10(sum(w * 10^(L/10)) / sum(w))`, evaluated stably.
pub fn weighted_energy_average_db(
    responses_db: &[Vec<f64>],
    weights: &[f64],
) -> DspResult<Vec<f64>> {
    let bins = validate_responses(responses_db, weights)?;
    let maximum_weight = weights.iter().copied().fold(0.0_f64, f64::max);
    let log_weights: Vec<f64> = weights
        .iter()
        .map(|weight| weight.ln() - maximum_weight.ln())
        .collect();
    let log_total_weight = log_sum_exp(&log_weights);
    let mut result = Vec::with_capacity(bins);

    for bin in 0..bins {
        let max_db = responses_db
            .iter()
            .map(|response| response[bin])
            .fold(f64::NEG_INFINITY, f64::max);
        let log_relative_energies: Vec<f64> = responses_db
            .iter()
            .zip(&log_weights)
            .map(|(response, log_weight)| log_weight + (response[bin] - max_db) * DB_TO_NEPERS)
            .collect();
        let log_relative_energy = log_sum_exp(&log_relative_energies);
        // An energy average cannot exceed the largest input level. Clamping
        // removes only a possible positive round-off ulp at that boundary.
        let correction_db = ((log_relative_energy - log_total_weight) * NEPERS_TO_DB).min(0.0);
        let average_db = max_db + correction_db;
        if !average_db.is_finite() {
            return Err(DspError::NonFinite {
                context: "weighted energy average",
                index: bin,
            });
        }
        result.push(average_db);
    }
    Ok(result)
}

/// Weighted nearest-rank percentile. `quantile` is in `[0, 1]`.
pub fn weighted_percentile(values: &[f64], weights: &[f64], quantile: f64) -> DspResult<f64> {
    if values.is_empty() {
        return Err(DspError::EmptyInput("percentile values"));
    }
    if values.len() != weights.len() {
        return Err(DspError::ShapeMismatch(
            "percentile values and weights must have equal lengths".into(),
        ));
    }
    if !(0.0..=1.0).contains(&quantile) || !quantile.is_finite() {
        return Err(DspError::InvalidArgument(
            "percentile quantile must be finite and between zero and one".into(),
        ));
    }
    if values.iter().any(|value| !value.is_finite())
        || weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight <= 0.0)
    {
        return Err(DspError::InvalidArgument(
            "percentile values must be finite and weights strictly positive".into(),
        ));
    }

    let mut pairs: Vec<(f64, f64)> = values
        .iter()
        .copied()
        .zip(weights.iter().copied())
        .collect();
    pairs.sort_by(|left, right| left.0.total_cmp(&right.0));
    let normalized_weights = normalized_weights(weights);
    let total_weight: f64 = normalized_weights.iter().sum();
    let maximum_weight = weights.iter().copied().fold(0.0_f64, f64::max);
    let threshold = quantile * total_weight;
    let mut cumulative = 0.0;
    for (value, weight) in &pairs {
        cumulative += weight / maximum_weight;
        if cumulative >= threshold {
            return Ok(*value);
        }
    }
    Ok(pairs.last().expect("nonempty checked above").0)
}

pub fn summarize_spatial(responses_db: &[Vec<f64>], weights: &[f64]) -> DspResult<SpatialSummary> {
    let bins = validate_responses(responses_db, weights)?;
    let energy_average_db = weighted_energy_average_db(responses_db, weights)?;
    let normalized_weights = normalized_weights(weights);
    let total_weight: f64 = normalized_weights.iter().sum();
    let mut weighted_std_dev_db = Vec::with_capacity(bins);
    let mut lower_quartile_db = Vec::with_capacity(bins);
    let mut upper_quartile_db = Vec::with_capacity(bins);

    for bin in 0..bins {
        let values: Vec<f64> = responses_db.iter().map(|response| response[bin]).collect();
        let value_scale = values.iter().copied().map(f64::abs).fold(0.0, f64::max);
        let scaled_values: Vec<f64> = if value_scale == 0.0 {
            vec![0.0; values.len()]
        } else {
            values.iter().map(|value| value / value_scale).collect()
        };
        let arithmetic_center = scaled_values
            .iter()
            .zip(&normalized_weights)
            .map(|(value, weight)| value * weight)
            .sum::<f64>()
            / total_weight;
        let scaled_variance = scaled_values
            .iter()
            .zip(&normalized_weights)
            .map(|(value, weight)| weight * (value - arithmetic_center).powi(2))
            .sum::<f64>()
            / total_weight;
        // Values scaled by their maximum absolute magnitude lie in [-1, 1],
        // so their population variance is at most one. The clamp removes only
        // positive floating-point accumulation error at that exact bound.
        let std_dev_db = value_scale * scaled_variance.clamp(0.0, 1.0).sqrt();
        if !std_dev_db.is_finite() {
            return Err(DspError::NonFinite {
                context: "weighted spatial standard deviation",
                index: bin,
            });
        }
        weighted_std_dev_db.push(std_dev_db);
        lower_quartile_db.push(weighted_percentile(&values, weights, 0.25)?);
        upper_quartile_db.push(weighted_percentile(&values, weights, 0.75)?);
    }

    Ok(SpatialSummary {
        energy_average_db,
        weighted_std_dev_db,
        lower_quartile_db,
        upper_quartile_db,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p0_weight_is_two() {
        assert_eq!(
            default_position_weights(4).unwrap(),
            vec![2.0, 1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn energy_average_matches_definition_and_not_db_mean() {
        let responses = vec![vec![0.0], vec![10.0]];
        let weights = [2.0, 1.0];
        let average = weighted_energy_average_db(&responses, &weights).unwrap()[0];
        let expected = 10.0 * ((2.0 * 1.0 + 1.0 * 10.0) / 3.0_f64).log10();
        assert!((average - expected).abs() < 1.0e-12);
        assert!((average - (20.0 / 3.0)).abs() > 0.5);

        let summary = summarize_spatial(&responses, &weights).unwrap();
        let expected_std_dev = (200.0_f64 / 9.0).sqrt();
        assert!((summary.weighted_std_dev_db[0] - expected_std_dev).abs() < 1.0e-12);
        assert_eq!(summary.lower_quartile_db[0], 0.0);
        assert_eq!(summary.upper_quartile_db[0], 10.0);
    }

    #[test]
    fn max_weights_are_normalized_without_non_finite_outputs() {
        let responses = vec![vec![0.0, f64::MAX], vec![10.0, f64::MAX]];
        let weights = [f64::MAX, f64::MAX];

        let summary = summarize_spatial(&responses, &weights).unwrap();
        assert!(summary
            .energy_average_db
            .iter()
            .chain(&summary.weighted_std_dev_db)
            .chain(&summary.lower_quartile_db)
            .chain(&summary.upper_quartile_db)
            .all(|value| value.is_finite()));
        let expected = 10.0 * ((1.0 + 10.0) / 2.0_f64).log10();
        assert!((summary.energy_average_db[0] - expected).abs() < 1.0e-12);
        assert_eq!(summary.energy_average_db[1], f64::MAX);
        assert_eq!(summary.weighted_std_dev_db[1], 0.0);
        assert_eq!(summary.lower_quartile_db[0], 0.0);
        assert_eq!(summary.upper_quartile_db[0], 10.0);
    }

    #[test]
    fn extreme_finite_levels_keep_summary_finite() {
        let responses = vec![vec![f64::MAX], vec![-f64::MAX]];
        let summary = summarize_spatial(&responses, &[f64::MAX, f64::MAX]).unwrap();

        assert!(summary.energy_average_db[0].is_finite());
        assert!(summary.weighted_std_dev_db[0].is_finite());
        assert_eq!(summary.weighted_std_dev_db[0], f64::MAX);
        assert!(summary.lower_quartile_db[0].is_finite());
        assert!(summary.upper_quartile_db[0].is_finite());
    }

    #[test]
    fn rejects_non_finite_inputs_and_invalid_quantiles() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(weighted_energy_average_db(&[vec![value]], &[1.0]).is_err());
            assert!(weighted_percentile(&[value], &[1.0], 0.5).is_err());
        }
        for weight in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            assert!(weighted_energy_average_db(&[vec![0.0]], &[weight]).is_err());
            assert!(weighted_percentile(&[0.0], &[weight], 0.5).is_err());
        }
        for quantile in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.1] {
            assert!(weighted_percentile(&[0.0], &[1.0], quantile).is_err());
        }
    }
}
