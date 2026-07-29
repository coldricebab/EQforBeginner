//! Versioned house-curve presets and logarithmic-frequency interpolation.
//!
//! The bundled values are explicitly "-style" design curves, not claimed to be
//! official curves from either organization.

use crate::{DspError, DspResult};

pub const TARGET_TXT_PARSER_VERSION: &str = "target-txt-parser-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPreset {
    BkStyle,
    HarmanStyle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetKnot {
    pub frequency_hz: f64,
    pub level_db: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetCurve {
    name: String,
    version: String,
    knots: Vec<TargetKnot>,
}

impl TargetCurve {
    #[must_use]
    pub fn preset(preset: TargetPreset) -> Self {
        match preset {
            TargetPreset::BkStyle => Self {
                name: "B&K-style house curve".into(),
                version: "bk-style-v1-log-f-linear-db".into(),
                knots: vec![
                    TargetKnot {
                        frequency_hz: 20.0,
                        level_db: 6.0,
                    },
                    TargetKnot {
                        frequency_hz: 30.0,
                        level_db: 5.3,
                    },
                    TargetKnot {
                        frequency_hz: 50.0,
                        level_db: 4.2,
                    },
                    TargetKnot {
                        frequency_hz: 80.0,
                        level_db: 3.2,
                    },
                    TargetKnot {
                        frequency_hz: 120.0,
                        level_db: 2.4,
                    },
                    TargetKnot {
                        frequency_hz: 200.0,
                        level_db: 1.4,
                    },
                    TargetKnot {
                        frequency_hz: 500.0,
                        level_db: 0.0,
                    },
                    TargetKnot {
                        frequency_hz: 1_000.0,
                        level_db: -0.5,
                    },
                    TargetKnot {
                        frequency_hz: 2_000.0,
                        level_db: -1.0,
                    },
                    TargetKnot {
                        frequency_hz: 5_000.0,
                        level_db: -2.2,
                    },
                    TargetKnot {
                        frequency_hz: 10_000.0,
                        level_db: -4.0,
                    },
                    TargetKnot {
                        frequency_hz: 20_000.0,
                        level_db: -6.0,
                    },
                ],
            },
            TargetPreset::HarmanStyle => Self {
                name: "Harman-style hi-fi house curve".into(),
                version: "harman-style-v1-log-f-linear-db".into(),
                knots: vec![
                    TargetKnot {
                        frequency_hz: 20.0,
                        level_db: 7.0,
                    },
                    TargetKnot {
                        frequency_hz: 30.0,
                        level_db: 6.6,
                    },
                    TargetKnot {
                        frequency_hz: 50.0,
                        level_db: 5.6,
                    },
                    TargetKnot {
                        frequency_hz: 80.0,
                        level_db: 4.4,
                    },
                    TargetKnot {
                        frequency_hz: 120.0,
                        level_db: 3.3,
                    },
                    TargetKnot {
                        frequency_hz: 200.0,
                        level_db: 2.0,
                    },
                    TargetKnot {
                        frequency_hz: 500.0,
                        level_db: 0.0,
                    },
                    TargetKnot {
                        frequency_hz: 1_000.0,
                        level_db: -0.4,
                    },
                    TargetKnot {
                        frequency_hz: 2_000.0,
                        level_db: -0.9,
                    },
                    TargetKnot {
                        frequency_hz: 5_000.0,
                        level_db: -2.0,
                    },
                    TargetKnot {
                        frequency_hz: 10_000.0,
                        level_db: -3.8,
                    },
                    TargetKnot {
                        frequency_hz: 20_000.0,
                        level_db: -6.5,
                    },
                ],
            },
        }
    }

    pub fn from_knots(
        name: impl Into<String>,
        version: impl Into<String>,
        knots: Vec<TargetKnot>,
    ) -> DspResult<Self> {
        validate_knots(&knots)?;
        Ok(Self {
            name: name.into(),
            version: version.into(),
            knots,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn knots(&self) -> &[TargetKnot] {
        &self.knots
    }

    /// Linear interpolation in dB on a logarithmic frequency axis. Values
    /// outside the knot range use the nearest endpoint.
    fn level_at_validated(&self, frequency_hz: f64) -> f64 {
        if frequency_hz <= self.knots[0].frequency_hz {
            return self.knots[0].level_db;
        }
        let last = self
            .knots
            .last()
            .expect("target curves always contain knots");
        if frequency_hz >= last.frequency_hz {
            return last.level_db;
        }
        let upper_index = self
            .knots
            .partition_point(|knot| knot.frequency_hz < frequency_hz);
        let lower = &self.knots[upper_index - 1];
        let upper = &self.knots[upper_index];
        let fraction = (frequency_hz.ln() - lower.frequency_hz.ln())
            / (upper.frequency_hz.ln() - lower.frequency_hz.ln());
        lower.level_db + fraction * (upper.level_db - lower.level_db)
    }

    pub fn level_at(&self, frequency_hz: f64) -> DspResult<f64> {
        validate_knots(&self.knots)?;
        if !frequency_hz.is_finite() || frequency_hz < 0.0 {
            return Err(DspError::InvalidArgument(
                "target query frequency must be finite and nonnegative".into(),
            ));
        }
        Ok(self.level_at_validated(frequency_hz))
    }

    pub fn evaluate(&self, frequencies_hz: &[f64]) -> DspResult<Vec<f64>> {
        validate_knots(&self.knots)?;
        if frequencies_hz
            .iter()
            .any(|frequency| !frequency.is_finite() || *frequency < 0.0)
        {
            return Err(DspError::InvalidArgument(
                "target query frequencies must be finite and nonnegative".into(),
            ));
        }
        Ok(frequencies_hz
            .iter()
            .map(|frequency| self.level_at_validated(*frequency))
            .collect())
    }
}

fn validate_knots(knots: &[TargetKnot]) -> DspResult<()> {
    if knots.len() < 2 {
        return Err(DspError::InvalidArgument(
            "a target curve requires at least two knots".into(),
        ));
    }
    for (index, knot) in knots.iter().enumerate() {
        if !knot.frequency_hz.is_finite()
            || knot.frequency_hz <= 0.0
            || knot.frequency_hz > 1_000_000.0
        {
            return Err(DspError::InvalidArgument(format!(
                "target knot {} must have a finite frequency within (0, 1000000] Hz",
                index + 1
            )));
        }
        if !knot.level_db.is_finite() || !(-200.0..=200.0).contains(&knot.level_db) {
            return Err(DspError::InvalidArgument(format!(
                "target knot {} must have a finite level within [-200, 200] dB",
                index + 1
            )));
        }
        if index > 0 && knot.frequency_hz <= knots[index - 1].frequency_hz {
            return Err(DspError::InvalidArgument(format!(
                "target frequencies must be strictly increasing (knot {})",
                index + 1
            )));
        }
    }
    Ok(())
}

/// Parse `frequency_hz level_db` text. Commas, spaces and tabs are accepted;
/// blank lines and lines beginning with `#` or `;` are ignored. Duplicate or
/// descending frequencies are rejected with a line number.
pub fn parse_target_txt(input: &str) -> DspResult<TargetCurve> {
    let mut knots = Vec::new();
    for (line_index, original_line) in input.lines().enumerate() {
        let line_number = line_index + 1;
        let comment_start = original_line
            .char_indices()
            .find_map(|(index, character)| (character == '#' || character == ';').then_some(index));
        let content = &original_line[..comment_start.unwrap_or(original_line.len())];
        let fields: Vec<&str> = content
            .split(|character: char| character == ',' || character.is_whitespace())
            .filter(|field| !field.is_empty())
            .collect();
        if fields.is_empty() {
            continue;
        }
        if fields.len() != 2 {
            return Err(DspError::TargetParse {
                line: line_number,
                message: "expected `frequency_hz level_db`, for example `100 2.5`".into(),
            });
        }
        let frequency_hz = fields[0]
            .parse::<f64>()
            .map_err(|_| DspError::TargetParse {
                line: line_number,
                message: format!("invalid frequency `{}`", fields[0]),
            })?;
        let level_db = fields[1]
            .parse::<f64>()
            .map_err(|_| DspError::TargetParse {
                line: line_number,
                message: format!("invalid level `{}`", fields[1]),
            })?;
        if !frequency_hz.is_finite() || frequency_hz <= 0.0 || frequency_hz > 1_000_000.0 {
            return Err(DspError::TargetParse {
                line: line_number,
                message: "frequency must be finite and within (0, 1000000] Hz".into(),
            });
        }
        if !level_db.is_finite() || !(-200.0..=200.0).contains(&level_db) {
            return Err(DspError::TargetParse {
                line: line_number,
                message: "level must be finite and within [-200, 200] dB".into(),
            });
        }
        if let Some(previous) = knots.last() {
            let previous: &TargetKnot = previous;
            if frequency_hz <= previous.frequency_hz {
                let kind = if frequency_hz == previous.frequency_hz {
                    "duplicate"
                } else {
                    "descending"
                };
                return Err(DspError::TargetParse {
                    line: line_number,
                    message: format!("{kind} frequency {frequency_hz}; frequencies must increase"),
                });
            }
        }
        knots.push(TargetKnot {
            frequency_hz,
            level_db,
        });
    }

    if knots.len() < 2 {
        return Err(DspError::TargetParse {
            line: 0,
            message: "at least two target rows are required".into(),
        });
    }
    TargetCurve::from_knots("User target", TARGET_TXT_PARSER_VERSION, knots)
}

/// Median vertical offset between a target and a measured response over a
/// trusted reference band.
pub fn vertical_alignment_db(
    frequencies_hz: &[f64],
    target_db: &[f64],
    response_db: &[f64],
    reference_low_hz: f64,
    reference_high_hz: f64,
) -> DspResult<f64> {
    if frequencies_hz.len() != target_db.len() || target_db.len() != response_db.len() {
        return Err(DspError::ShapeMismatch(
            "frequency, target and response grids must have equal lengths".into(),
        ));
    }
    if !reference_low_hz.is_finite()
        || !reference_high_hz.is_finite()
        || reference_low_hz <= 0.0
        || reference_high_hz <= reference_low_hz
    {
        return Err(DspError::InvalidArgument(
            "target reference band must be finite, positive and increasing".into(),
        ));
    }
    if frequencies_hz
        .iter()
        .chain(target_db)
        .chain(response_db)
        .any(|value| !value.is_finite())
    {
        return Err(DspError::InvalidArgument(
            "target alignment grids must contain only finite values".into(),
        ));
    }
    // The median runs over log-spaced queries instead of the raw linear FFT
    // bins (2026-07-29 expert review, item C): on a linear grid the 200-500 Hz
    // band holds three times as many bins above 300 Hz as below it, so the
    // "robust anchor" was really a 300-500 Hz anchor, tilting how the cut and
    // boost budget is distributed. Ninety-six log-spaced points give every
    // octave equal say; each query interpolates both curves on the same grid.
    const ALIGNMENT_LOG_QUERY_POINTS: usize = 96;
    let in_band: Vec<usize> = frequencies_hz
        .iter()
        .enumerate()
        .filter_map(|(index, frequency)| {
            (*frequency >= reference_low_hz && *frequency <= reference_high_hz).then_some(index)
        })
        .collect();
    let mut offsets: Vec<f64> = if in_band.len() >= 4 {
        let query_low_hz = frequencies_hz[in_band[0]].max(reference_low_hz);
        let query_high_hz =
            frequencies_hz[*in_band.last().expect("nonempty")].min(reference_high_hz);
        let ratio = query_high_hz / query_low_hz;
        let queries: Vec<f64> = (0..ALIGNMENT_LOG_QUERY_POINTS)
            .map(|index| {
                query_low_hz * ratio.powf(index as f64 / (ALIGNMENT_LOG_QUERY_POINTS - 1) as f64)
            })
            .collect();
        let target_at = interpolate_log_frequency_grid(frequencies_hz, target_db, &queries)?;
        let response_at = interpolate_log_frequency_grid(frequencies_hz, response_db, &queries)?;
        response_at
            .iter()
            .zip(&target_at)
            .map(|(response, target)| response - target)
            .collect()
    } else {
        frequencies_hz
            .iter()
            .zip(target_db)
            .zip(response_db)
            .filter_map(|((&frequency, &target), &response)| {
                (frequency >= reference_low_hz && frequency <= reference_high_hz)
                    .then_some(response - target)
            })
            .collect()
    };
    if offsets.iter().any(|value| !value.is_finite()) {
        return Err(DspError::InvalidArgument(
            "target alignment overflowed to a non-finite offset".into(),
        ));
    }
    if offsets.is_empty() {
        return Err(DspError::InvalidArgument(
            "target reference band contains no FFT bins".into(),
        ));
    }
    offsets.sort_by(f64::total_cmp);
    let middle = offsets.len() / 2;
    let alignment = if offsets.len() % 2 == 0 {
        offsets[middle - 1] * 0.5 + offsets[middle] * 0.5
    } else {
        offsets[middle]
    };
    if !alignment.is_finite() {
        return Err(DspError::InvalidArgument(
            "target alignment produced a non-finite median".into(),
        ));
    }
    Ok(alignment)
}

fn validate_interpolation_inputs(frequencies_hz: &[f64], values: &[f64]) -> DspResult<()> {
    if frequencies_hz.len() != values.len() || frequencies_hz.len() < 2 {
        return Err(DspError::ShapeMismatch(
            "interpolation needs equal frequency/value grids with at least two bins".into(),
        ));
    }
    for (index, (&frequency, &value)) in frequencies_hz.iter().zip(values).enumerate() {
        if !frequency.is_finite() || frequency < 0.0 || !value.is_finite() {
            return Err(DspError::InvalidArgument(format!(
                "interpolation bin {index} must contain a finite nonnegative frequency and finite value"
            )));
        }
        if index > 0 && frequency <= frequencies_hz[index - 1] {
            return Err(DspError::InvalidArgument(
                "interpolation frequencies must be strictly increasing".into(),
            ));
        }
    }
    Ok(())
}

fn interpolate_log_frequency_validated(
    frequencies_hz: &[f64],
    values: &[f64],
    frequency_hz: f64,
) -> f64 {
    if frequency_hz <= frequencies_hz[0] {
        return values[0];
    }
    let last = frequencies_hz.len() - 1;
    if frequency_hz >= frequencies_hz[last] {
        return values[last];
    }
    let upper = frequencies_hz.partition_point(|frequency| *frequency < frequency_hz);
    let lower = upper - 1;
    let low_f = frequencies_hz[lower].max(f64::MIN_POSITIVE);
    let high_f = frequencies_hz[upper].max(f64::MIN_POSITIVE);
    let query = frequency_hz.max(f64::MIN_POSITIVE);
    let fraction = (query.ln() - low_f.ln()) / (high_f.ln() - low_f.ln());
    values[lower] + fraction * (values[upper] - values[lower])
}

/// Interpolate one arbitrary response value on a logarithmic frequency axis.
pub fn interpolate_log_frequency(
    frequencies_hz: &[f64],
    values: &[f64],
    frequency_hz: f64,
) -> DspResult<f64> {
    validate_interpolation_inputs(frequencies_hz, values)?;
    if !frequency_hz.is_finite() || frequency_hz < 0.0 {
        return Err(DspError::InvalidArgument(
            "interpolation query must be finite and nonnegative".into(),
        ));
    }
    let interpolated = interpolate_log_frequency_validated(frequencies_hz, values, frequency_hz);
    if !interpolated.is_finite() {
        return Err(DspError::InvalidArgument(
            "interpolation produced a non-finite value".into(),
        ));
    }
    Ok(interpolated)
}

/// Interpolate a complete query grid while validating the source only once.
pub fn interpolate_log_frequency_grid(
    frequencies_hz: &[f64],
    values: &[f64],
    queries_hz: &[f64],
) -> DspResult<Vec<f64>> {
    validate_interpolation_inputs(frequencies_hz, values)?;
    if queries_hz
        .iter()
        .any(|frequency| !frequency.is_finite() || *frequency < 0.0)
    {
        return Err(DspError::InvalidArgument(
            "interpolation queries must be finite and nonnegative".into(),
        ));
    }
    let interpolated: Vec<f64> = queries_hz
        .iter()
        .map(|frequency| interpolate_log_frequency_validated(frequencies_hz, values, *frequency))
        .collect();
    if interpolated.iter().any(|value| !value.is_finite()) {
        return Err(DspError::InvalidArgument(
            "interpolation produced a non-finite value".into(),
        ));
    }
    Ok(interpolated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_interpolates_halfway_on_log_axis() {
        let curve = TargetCurve::from_knots(
            "test",
            "v1",
            vec![
                TargetKnot {
                    frequency_hz: 100.0,
                    level_db: 0.0,
                },
                TargetKnot {
                    frequency_hz: 400.0,
                    level_db: 8.0,
                },
            ],
        )
        .unwrap();
        assert!((curve.level_at(200.0).unwrap() - 4.0).abs() < 1.0e-12);
    }

    #[test]
    fn parser_accepts_separators_comments_and_reports_bad_line() {
        let curve = parse_target_txt("# custom\n20, 6\n100\t2 ; comment\n500 0\n").unwrap();
        assert_eq!(curve.knots().len(), 3);
        let error = parse_target_txt("20 6\n10 2\n").unwrap_err();
        assert!(matches!(error, DspError::TargetParse { line: 2, .. }));
    }

    #[test]
    fn parser_rejects_duplicate_frequency() {
        let error = parse_target_txt("20 6\n20 5\n").unwrap_err();
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn invalid_queries_and_unvalidated_curve_state_cannot_panic() {
        let curve = TargetCurve::preset(TargetPreset::BkStyle);
        assert!(curve.level_at(f64::NAN).is_err());
        assert!(curve.level_at(f64::NEG_INFINITY).is_err());
        assert!(interpolate_log_frequency(&[0.0, 1.0], &[0.0, 0.0], f64::NAN).is_err());
        assert!(
            interpolate_log_frequency(&[100.0, 400.0], &[-f64::MAX, f64::MAX], 200.0,).is_err()
        );
    }
}
