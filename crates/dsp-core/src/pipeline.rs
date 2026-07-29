//! The smallest complete Phase 1 vertical slice.

use crate::analysis::frequency_response;
use crate::correction::{
    design_limited_correction, CorrectionSettings, CorrectionWarning, RoomCorrectionDesign,
};
use crate::fir::{synthesize_minimum_phase, FirFilter};
use crate::fixture::SyntheticRoomFixture;
use crate::spatial::default_position_weights;
use crate::stereo::{blend_stereo_correction, StereoBlendSettings, StereoGainDesign};
use crate::target::{TargetCurve, TargetPreset};
use crate::validation::{validate_prediction, ValidationReport, ValidationThresholds};
use crate::{DspError, DspResult};

#[derive(Debug, Clone, PartialEq)]
pub struct Phase1Config {
    pub target: TargetCurve,
    pub correction: CorrectionSettings,
    pub stereo_blend: StereoBlendSettings,
    pub validation: ValidationThresholds,
}

impl Default for Phase1Config {
    fn default() -> Self {
        Self {
            target: TargetCurve::preset(TargetPreset::BkStyle),
            correction: CorrectionSettings::default(),
            stereo_blend: StereoBlendSettings::default(),
            validation: ValidationThresholds::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phase1Result {
    pub sample_rate_hz: u32,
    pub frequencies_hz: Vec<f64>,
    pub position_weights: Vec<f64>,
    pub left_design: RoomCorrectionDesign,
    pub right_design: RoomCorrectionDesign,
    pub stereo_design: StereoGainDesign,
    pub left_fir: FirFilter,
    pub right_fir: FirFilter,
    pub left_validation: ValidationReport,
    pub right_validation: ValidationReport,
    pub passed: bool,
}

/// IR input -> FFT FR -> P0-weighted energy statistics -> target alignment ->
/// spatially robust bounded stereo design -> minimum-phase FIR -> predicted
/// convolution and automatic validation.
pub fn run_phase1(
    fixture: &SyntheticRoomFixture,
    config: &Phase1Config,
) -> DspResult<Phase1Result> {
    if fixture.sample_rate_hz != 48_000 {
        return Err(DspError::InvalidArgument(format!(
            "Phase 1 verification requires the 48 kHz trial path, got {} Hz",
            fixture.sample_rate_hz
        )));
    }
    if fixture.left_impulses.is_empty()
        || fixture.left_impulses.len() != fixture.right_impulses.len()
        || fixture.left_impulses.len() != fixture.position_labels.len()
    {
        return Err(DspError::ShapeMismatch(
            "fixture must contain matching nonempty left/right positions and labels".into(),
        ));
    }
    let position_weights = default_position_weights(fixture.left_impulses.len())?;
    let left_responses = fixture
        .left_impulses
        .iter()
        .map(|impulse| frequency_response(impulse, fixture.sample_rate_hz, fixture.fft_size))
        .collect::<DspResult<Vec<_>>>()?;
    let right_responses = fixture
        .right_impulses
        .iter()
        .map(|impulse| frequency_response(impulse, fixture.sample_rate_hz, fixture.fft_size))
        .collect::<DspResult<Vec<_>>>()?;
    let frequencies_hz = left_responses[0].frequencies_hz.clone();
    if right_responses
        .iter()
        .chain(&left_responses)
        .any(|response| response.frequencies_hz != frequencies_hz)
    {
        return Err(DspError::ShapeMismatch(
            "all impulse responses must resolve to the same FFT grid".into(),
        ));
    }
    let target_db = config.target.evaluate(&frequencies_hz)?;
    let left_levels: Vec<Vec<f64>> = left_responses
        .iter()
        .map(|response| response.magnitude_db.clone())
        .collect();
    let right_levels: Vec<Vec<f64>> = right_responses
        .iter()
        .map(|response| response.magnitude_db.clone())
        .collect();
    let left_design = design_limited_correction(
        &frequencies_hz,
        &left_levels,
        &position_weights,
        &target_db,
        &config.correction,
    )?;
    let right_design = design_limited_correction(
        &frequencies_hz,
        &right_levels,
        &position_weights,
        &target_db,
        &config.correction,
    )?;
    let stereo_design = blend_stereo_correction(
        &frequencies_hz,
        &left_design.gain_db,
        &right_design.gain_db,
        &config.stereo_blend,
    )?;
    let left_fir = synthesize_minimum_phase(&stereo_design.left_gain_db, fixture.sample_rate_hz)?;
    let right_fir = synthesize_minimum_phase(&stereo_design.right_gain_db, fixture.sample_rate_hz)?;
    let left_validation = validate_prediction(
        &fixture.left_impulses,
        &position_weights,
        &left_fir,
        &frequencies_hz,
        &left_design.aligned_target_db,
        &config.validation,
    )?;
    let right_validation = validate_prediction(
        &fixture.right_impulses,
        &position_weights,
        &right_fir,
        &frequencies_hz,
        &right_design.aligned_target_db,
        &config.validation,
    )?;
    let attenuation_limit_reached = left_design
        .warnings
        .iter()
        .chain(&right_design.warnings)
        .any(|warning| matches!(warning, CorrectionWarning::AttenuationLimitReached { .. }));
    let passed = left_validation.passed && right_validation.passed && !attenuation_limit_reached;

    Ok(Phase1Result {
        sample_rate_hz: fixture.sample_rate_hz,
        frequencies_hz,
        position_weights,
        left_design,
        right_design,
        stereo_design,
        left_fir,
        right_fir,
        left_validation,
        right_validation,
        passed,
    })
}
