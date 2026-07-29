//! Deterministic, offline DSP primitives for a conservative room-correction pass.
//!
//! The crate deliberately separates analysis, spatial statistics, target curves,
//! tightly bounded correction, FIR synthesis, stereo policy, and validation.
//! The first usable slice accepts impulse responses and produces a validated
//! 48 kHz stereo minimum-phase FIR with at most +3 dB of spatially supported,
//! broad-dip boost.

pub mod analysis;
pub mod calibration;
pub mod correction;
pub mod error;
pub mod fir;
pub mod fixture;
pub mod measurement;
pub mod phase4;
pub mod phase6;
pub mod pipeline;
pub mod smoothing;
pub mod spatial;
pub mod stereo;
pub mod sub_integration;
pub mod target;
pub mod validation;
pub mod wireless_sweep;

pub use error::{DspError, DspResult};

/// Algorithm identifier persisted alongside deterministic results.
pub const ALGORITHM_VERSION: &str = "phase1-bounded-boost-v2";
