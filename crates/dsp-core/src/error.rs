use std::error::Error;
use std::fmt::{Display, Formatter};

/// Errors caused by malformed DSP inputs or failed safety constraints.
#[derive(Debug, Clone, PartialEq)]
pub enum DspError {
    EmptyInput(&'static str),
    InvalidArgument(String),
    ShapeMismatch(String),
    NonFinite { context: &'static str, index: usize },
    TargetParse { line: usize, message: String },
    CalibrationParse { line: usize, message: String },
}

impl Display for DspError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInput(context) => write!(f, "{context} must not be empty"),
            Self::InvalidArgument(message) | Self::ShapeMismatch(message) => f.write_str(message),
            Self::NonFinite { context, index } => {
                write!(f, "{context} contains a non-finite value at index {index}")
            }
            Self::TargetParse { line, message } => {
                write!(f, "target line {line}: {message}")
            }
            Self::CalibrationParse { line, message } => {
                write!(f, "microphone calibration line {line}: {message}")
            }
        }
    }
}

impl Error for DspError {}

pub type DspResult<T> = Result<T, DspError>;
