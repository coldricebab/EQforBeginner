//! UMIK-style microphone calibration parsing and log-frequency interpolation.
//!
//! The parser is deliberately independent of filenames and device APIs. It
//! retains the optional sensitivity/serial header and optional phase column,
//! but room-response correction uses only the file's magnitude-correction
//! values. The sensitivity factor is metadata rather than an SPL claim.

use crate::{DspError, DspResult};

pub const UMIK_CALIBRATION_PARSER_VERSION: &str = "umik-calibration-parser-v2";

const MINIMUM_FREQUENCY_HZ: f64 = 0.1;
const MAXIMUM_FREQUENCY_HZ: f64 = 1_000_000.0;
const MAXIMUM_ABSOLUTE_CORRECTION_DB: f64 = 100.0;
const MAXIMUM_ABSOLUTE_PHASE_DEGREES: f64 = 3_600.0;

#[derive(Debug, Clone, PartialEq)]
pub struct MicrophoneCalibrationPoint {
    pub frequency_hz: f64,
    /// The microphone's own magnitude deviation from flat, in dB, exactly as
    /// the file records it. Compensation SUBTRACTS this from a measurement
    /// (`measurement.rs` divides the spectrum by it); it is not an additive
    /// correction, despite the field name the parser keeps for file parity.
    pub correction_db: f64,
    /// Retained for provenance. The v1 measurement path does not apply phase.
    pub phase_degrees: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MicrophoneCalibration {
    pub parser_version: &'static str,
    pub serial_number: Option<String>,
    pub sensitivity_factor_db: Option<f64>,
    pub points: Vec<MicrophoneCalibrationPoint>,
}

impl MicrophoneCalibration {
    #[must_use]
    pub fn frequency_range_hz(&self) -> [f64; 2] {
        [
            self.points
                .first()
                .expect("a parsed calibration always contains points")
                .frequency_hz,
            self.points
                .last()
                .expect("a parsed calibration always contains points")
                .frequency_hz,
        ]
    }

    #[must_use]
    pub fn covers(&self, frequency_range_hz: [f64; 2]) -> bool {
        let actual = self.frequency_range_hz();
        frequency_range_hz[0].is_finite()
            && frequency_range_hz[1].is_finite()
            && frequency_range_hz[0] > 0.0
            && frequency_range_hz[1] >= frequency_range_hz[0]
            && actual[0] <= frequency_range_hz[0]
            && actual[1] >= frequency_range_hz[1]
    }

    /// Interpolate the recorded microphone deviation linearly in dB on a
    /// log-frequency axis. Extrapolation is rejected so callers cannot
    /// silently use a calibration outside its documented range.
    pub fn correction_db_at(&self, frequency_hz: f64) -> DspResult<f64> {
        if !frequency_hz.is_finite() || frequency_hz <= 0.0 {
            return Err(DspError::InvalidArgument(
                "calibration interpolation frequency must be finite and positive".into(),
            ));
        }
        let range = self.frequency_range_hz();
        if frequency_hz < range[0] || frequency_hz > range[1] {
            return Err(DspError::InvalidArgument(format!(
                "calibration frequency {frequency_hz} Hz is outside the file range {}-{} Hz",
                range[0], range[1]
            )));
        }

        match self
            .points
            .binary_search_by(|point| point.frequency_hz.total_cmp(&frequency_hz))
        {
            Ok(index) => Ok(self.points[index].correction_db),
            Err(upper) => {
                let lower = upper.checked_sub(1).ok_or_else(|| {
                    DspError::InvalidArgument("calibration interpolation underflow".into())
                })?;
                let low = &self.points[lower];
                let high = &self.points[upper];
                let position = (frequency_hz / low.frequency_hz).ln()
                    / (high.frequency_hz / low.frequency_hz).ln();
                let correction =
                    low.correction_db + position * (high.correction_db - low.correction_db);
                if !correction.is_finite() {
                    return Err(DspError::InvalidArgument(
                        "calibration interpolation produced a non-finite value".into(),
                    ));
                }
                Ok(correction)
            }
        }
    }
}

fn parse_header_number(line: &str, marker: &str) -> Option<f64> {
    let lowercase = line.to_ascii_lowercase();
    let start = lowercase.find(marker)? + marker.len();
    let tail = line.get(start..)?.trim_start_matches([' ', '\t', '=', ':']);
    let token: String = tail
        .chars()
        .take_while(|character| {
            character.is_ascii_digit() || matches!(character, '+' | '-' | '.' | 'e' | 'E')
        })
        .collect();
    (!token.is_empty())
        .then(|| token.parse::<f64>().ok())
        .flatten()
}

fn parse_serial_number(line: &str) -> Option<String> {
    let lowercase = line.to_ascii_lowercase();
    let marker = lowercase
        .find("serno")
        .map(|index| (index, "serno".len()))
        .or_else(|| {
            lowercase
                .find("serial")
                .map(|index| (index, "serial".len()))
        })?;
    let tail = line
        .get(marker.0 + marker.1..)?
        .trim_start_matches([' ', '\t', '=', ':']);
    let serial: String = tail
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .collect();
    (!serial.is_empty()).then_some(serial)
}

fn calibration_parse_error(line: usize, message: impl Into<String>) -> DspError {
    DspError::CalibrationParse {
        line,
        message: message.into(),
    }
}

fn strip_outer_quotes(line: &str) -> (&str, bool) {
    line.strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map_or((line, false), |value| (value.trim(), true))
}

fn starts_with_numeric_column(line: &str) -> bool {
    line.split(|character: char| character.is_ascii_whitespace() || character == ',')
        .find(|column| !column.is_empty())
        .is_some_and(|column| column.parse::<f64>().is_ok())
}

/// Parse the two- or three-column format distributed for UMIK microphones.
///
/// Data rows accept spaces, tabs, or commas:
/// `frequency_hz correction_db [phase_degrees]`. Blank lines and `#`/`;`
/// comments are ignored. Frequencies must be strictly increasing; duplicate
/// points are rejected rather than being merged invisibly.
pub fn parse_umik_calibration(text: &str) -> DspResult<MicrophoneCalibration> {
    if text
        .trim_matches(['\u{feff}', ' ', '\t', '\r', '\n'])
        .is_empty()
    {
        return Err(DspError::EmptyInput("microphone calibration text"));
    }

    let mut serial_number = None;
    let mut sensitivity_factor_db = None;
    let mut points = Vec::new();

    for (zero_based_line, original) in text.lines().enumerate() {
        let line_number = zero_based_line + 1;
        let without_bom = original.trim_start_matches('\u{feff}');
        let trimmed = without_bom.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let (content, quoted_line) = strip_outer_quotes(trimmed);

        let lowercase = content.to_ascii_lowercase();
        if lowercase.contains("sens factor") || lowercase.contains("sensitivity") {
            let sensitivity = parse_header_number(content, "sens factor")
                .or_else(|| parse_header_number(content, "sensitivity"))
                .ok_or_else(|| {
                    calibration_parse_error(
                        line_number,
                        "sensitivity header must contain a finite dB value",
                    )
                })?;
            if !sensitivity.is_finite() || sensitivity.abs() > MAXIMUM_ABSOLUTE_CORRECTION_DB {
                return Err(calibration_parse_error(
                    line_number,
                    "sensitivity factor must be finite and within +/-100 dB",
                ));
            }
            if sensitivity_factor_db.replace(sensitivity).is_some() {
                return Err(calibration_parse_error(
                    line_number,
                    "duplicate sensitivity header",
                ));
            }
            serial_number = parse_serial_number(content).or(serial_number);
            continue;
        }

        // miniDSP calibration downloads may place descriptive manufacturer
        // metadata in fully quoted lines before the numeric table. A quoted
        // numeric row is still parsed below; only clearly textual metadata is
        // ignored so malformed unquoted data cannot disappear silently.
        if quoted_line && !starts_with_numeric_column(content) {
            serial_number = parse_serial_number(content).or(serial_number);
            continue;
        }

        let data_without_comment = content
            .split(['#', ';'])
            .next()
            .expect("split always yields one item")
            .trim();
        if data_without_comment.is_empty() {
            continue;
        }
        let columns: Vec<&str> = data_without_comment
            .split(|character: char| character.is_ascii_whitespace() || character == ',')
            .filter(|column| !column.is_empty())
            .collect();
        if !(2..=3).contains(&columns.len()) {
            return Err(calibration_parse_error(
                line_number,
                "expected `frequency_hz correction_db [phase_degrees]`",
            ));
        }

        let frequency_hz = columns[0].parse::<f64>().map_err(|_| {
            calibration_parse_error(line_number, "frequency must be a number in Hz")
        })?;
        let correction_db = columns[1].parse::<f64>().map_err(|_| {
            calibration_parse_error(line_number, "correction must be a number in dB")
        })?;
        let phase_degrees = columns
            .get(2)
            .map(|value| {
                value.parse::<f64>().map_err(|_| {
                    calibration_parse_error(line_number, "phase must be a number in degrees")
                })
            })
            .transpose()?;

        if !frequency_hz.is_finite()
            || !(MINIMUM_FREQUENCY_HZ..=MAXIMUM_FREQUENCY_HZ).contains(&frequency_hz)
        {
            return Err(calibration_parse_error(
                line_number,
                "frequency must be finite and between 0.1 and 1000000 Hz",
            ));
        }
        if !correction_db.is_finite() || correction_db.abs() > MAXIMUM_ABSOLUTE_CORRECTION_DB {
            return Err(calibration_parse_error(
                line_number,
                "correction must be finite and within +/-100 dB",
            ));
        }
        if phase_degrees
            .is_some_and(|phase| !phase.is_finite() || phase.abs() > MAXIMUM_ABSOLUTE_PHASE_DEGREES)
        {
            return Err(calibration_parse_error(
                line_number,
                "phase must be finite and within +/-3600 degrees",
            ));
        }
        if let Some(previous) = points.last() {
            let previous: &MicrophoneCalibrationPoint = previous;
            if frequency_hz <= previous.frequency_hz {
                return Err(calibration_parse_error(
                    line_number,
                    "frequencies must be strictly increasing with no duplicates",
                ));
            }
        }
        points.push(MicrophoneCalibrationPoint {
            frequency_hz,
            correction_db,
            phase_degrees,
        });
    }

    if points.len() < 2 {
        return Err(DspError::InvalidArgument(
            "microphone calibration must contain at least two frequency points".into(),
        ));
    }
    Ok(MicrophoneCalibration {
        parser_version: UMIK_CALIBRATION_PARSER_VERSION,
        serial_number,
        sensitivity_factor_db,
        points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_umik_header_and_mixed_delimiters() {
        let calibration = parse_umik_calibration(
            "\u{feff}Sens Factor = -18.435dB, SERNO: 007127277\n\
             # frequency correction phase\n\
             10.0, -2.0, 1.5\n\
             100.0\t0.0\t0.0 ; retained comment\n\
             1000.0 2.0 -1.5\n",
        )
        .unwrap();

        assert_eq!(calibration.parser_version, UMIK_CALIBRATION_PARSER_VERSION);
        assert_eq!(calibration.serial_number.as_deref(), Some("007127277"));
        assert_eq!(calibration.sensitivity_factor_db, Some(-18.435));
        assert_eq!(calibration.points.len(), 3);
        assert_eq!(calibration.points[0].phase_degrees, Some(1.5));
        assert_eq!(calibration.frequency_range_hz(), [10.0, 1000.0]);
    }

    #[test]
    fn parses_the_checked_in_minidsp_quoted_metadata_calibration() {
        let calibration = parse_umik_calibration(include_str!(
            "../../../testdata/umik-quoted-metadata-90deg.txt"
        ))
        .unwrap();

        assert_eq!(calibration.serial_number.as_deref(), Some("0000000"));
        assert_eq!(calibration.sensitivity_factor_db, Some(-2.434));
        assert!(calibration.points.len() > 600);
        assert!(calibration.covers([20.0, 20_000.0]));
        assert_eq!(calibration.points[0].frequency_hz, 10.054);
        assert_eq!(calibration.points.last().unwrap().frequency_hz, 20_016.816);
    }

    #[test]
    fn interpolates_linearly_in_db_on_a_log_frequency_axis() {
        let calibration = parse_umik_calibration("10 -6\n100 0\n1000 6\n").unwrap();
        let geometric_midpoint = (100.0_f64 * 1000.0).sqrt();
        assert!((calibration.correction_db_at(geometric_midpoint).unwrap() - 3.0).abs() < 1.0e-12);
        assert_eq!(calibration.correction_db_at(10.0).unwrap(), -6.0);
        assert!(calibration.covers([20.0, 500.0]));
        assert!(!calibration.covers([5.0, 500.0]));
    }

    #[test]
    fn rejects_extrapolation_and_invalid_interpolation_frequencies() {
        let calibration = parse_umik_calibration("10 0\n1000 0\n").unwrap();
        for frequency in [0.0, -1.0, f64::NAN, 5.0, 1001.0] {
            assert!(calibration.correction_db_at(frequency).is_err());
        }
    }

    #[test]
    fn reports_the_line_for_bad_rows_and_nonascending_points() {
        let error = parse_umik_calibration("10 0\nbad 1\n").unwrap_err();
        assert_eq!(
            error,
            DspError::CalibrationParse {
                line: 2,
                message: "frequency must be a number in Hz".into(),
            }
        );
        assert_eq!(
            parse_umik_calibration("10 0\n10 1\n").unwrap_err(),
            DspError::CalibrationParse {
                line: 2,
                message: "frequencies must be strictly increasing with no duplicates".into(),
            }
        );
    }

    #[test]
    fn rejects_empty_or_incomplete_calibration_text() {
        assert_eq!(
            parse_umik_calibration(" \n").unwrap_err(),
            DspError::EmptyInput("microphone calibration text")
        );
        assert!(parse_umik_calibration("Sens Factor = -18dB, SERNO: 1\n10 0\n").is_err());
    }

    #[test]
    fn rejects_duplicate_or_malformed_sensitivity_headers() {
        assert!(
            parse_umik_calibration("Sens Factor = -18dB\nSensitivity = -18dB\n10 0\n100 0\n")
                .is_err()
        );
        assert!(parse_umik_calibration("Sens Factor = nope\n10 0\n100 0\n")
            .unwrap_err()
            .to_string()
            .contains("line 1"));
    }
}
