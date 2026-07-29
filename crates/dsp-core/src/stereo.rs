//! Stereo policy for systems whose low bass shares a subwoofer signal path.

use crate::correction::validate_correction_invariant;
use crate::{DspError, DspResult};

#[derive(Debug, Clone, PartialEq)]
pub struct StereoBlendSettings {
    pub common_below_hz: f64,
    pub channel_specific_above_hz: f64,
}

impl Default for StereoBlendSettings {
    fn default() -> Self {
        Self {
            common_below_hz: 100.0,
            channel_specific_above_hz: 140.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StereoGainDesign {
    pub left_gain_db: Vec<f64>,
    pub right_gain_db: Vec<f64>,
    pub common_gain_db: Vec<f64>,
    pub channel_specific_mix: Vec<f64>,
}

fn smoothstep(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

/// Below the lower boundary both channels receive the same conservative
/// correction: the lesser attenuation when both request cuts, the lesser boost
/// when both request boosts, and unity when their requested signs conflict.
/// Above the upper boundary each channel retains its own correction. A
/// log-frequency smoothstep connects the regions with zero slope at both
/// boundaries.
pub fn blend_stereo_correction(
    frequencies_hz: &[f64],
    left_gain_db: &[f64],
    right_gain_db: &[f64],
    settings: &StereoBlendSettings,
) -> DspResult<StereoGainDesign> {
    if frequencies_hz.len() != left_gain_db.len()
        || left_gain_db.len() != right_gain_db.len()
        || frequencies_hz.is_empty()
    {
        return Err(DspError::ShapeMismatch(
            "stereo frequency, left-gain and right-gain grids must be nonempty and equal".into(),
        ));
    }
    if !settings.common_below_hz.is_finite()
        || !settings.channel_specific_above_hz.is_finite()
        || settings.common_below_hz <= 0.0
        || settings.channel_specific_above_hz <= settings.common_below_hz
    {
        return Err(DspError::InvalidArgument(
            "stereo boundaries must be finite, positive and increasing".into(),
        ));
    }
    validate_correction_invariant(left_gain_db)?;
    validate_correction_invariant(right_gain_db)?;

    let mut left = Vec::with_capacity(frequencies_hz.len());
    let mut right = Vec::with_capacity(frequencies_hz.len());
    let mut common = Vec::with_capacity(frequencies_hz.len());
    let mut mix = Vec::with_capacity(frequencies_hz.len());
    let log_span = (settings.channel_specific_above_hz / settings.common_below_hz).ln();

    for ((&frequency, &left_specific), &right_specific) in
        frequencies_hz.iter().zip(left_gain_db).zip(right_gain_db)
    {
        // Never impose one channel's boost or cut on the shared sub path when
        // the other channel asks for the opposite sign.
        let common_gain = if left_specific <= 0.0 && right_specific <= 0.0 {
            left_specific.max(right_specific)
        } else if left_specific >= 0.0 && right_specific >= 0.0 {
            left_specific.min(right_specific)
        } else {
            0.0
        };
        let channel_mix = if frequency <= settings.common_below_hz {
            0.0
        } else if frequency >= settings.channel_specific_above_hz {
            1.0
        } else {
            let log_fraction = (frequency / settings.common_below_hz).ln() / log_span;
            smoothstep(log_fraction)
        };
        common.push(common_gain);
        mix.push(channel_mix);
        left.push(common_gain + channel_mix * (left_specific - common_gain));
        right.push(common_gain + channel_mix * (right_specific - common_gain));
    }
    validate_correction_invariant(&left)?;
    validate_correction_invariant(&right)?;
    Ok(StereoGainDesign {
        left_gain_db: left,
        right_gain_db: right,
        common_gain_db: common,
        channel_specific_mix: mix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_then_crossfade_then_channel_specific_is_continuous() {
        let frequencies: Vec<f64> = (1..=250).map(f64::from).collect();
        let left = vec![-8.0; frequencies.len()];
        let right = vec![-3.0; frequencies.len()];
        let result =
            blend_stereo_correction(&frequencies, &left, &right, &StereoBlendSettings::default())
                .unwrap();
        assert_eq!(result.left_gain_db[99], -3.0); // 100 Hz
        assert_eq!(result.right_gain_db[99], -3.0);
        assert_eq!(result.left_gain_db[139], -8.0); // 140 Hz
        assert_eq!(result.right_gain_db[139], -3.0);
        assert!((result.left_gain_db[100] - result.left_gain_db[99]).abs() < 0.02);
        assert!((result.left_gain_db[139] - result.left_gain_db[138]).abs() < 0.02);
        assert!(result.left_gain_db.iter().all(|gain| *gain <= 0.0));
        assert!(result.right_gain_db.iter().all(|gain| *gain <= 0.0));
    }

    #[test]
    fn crossover_boundaries_are_configurable() {
        let frequencies = vec![70.0, 80.0, 100.0, 120.0];
        let result = blend_stereo_correction(
            &frequencies,
            &[-6.0; 4],
            &[-2.0; 4],
            &StereoBlendSettings {
                common_below_hz: 80.0,
                channel_specific_above_hz: 120.0,
            },
        )
        .unwrap();
        assert_eq!(result.left_gain_db[1], -2.0);
        assert_eq!(result.left_gain_db[3], -6.0);
    }

    #[test]
    fn shared_bass_uses_only_mutually_supported_boost() {
        let frequencies = vec![60.0, 80.0, 100.0, 140.0];
        let result = blend_stereo_correction(
            &frequencies,
            &[3.0, 3.0, 3.0, 3.0],
            &[1.5, -2.0, 1.5, 1.5],
            &StereoBlendSettings::default(),
        )
        .unwrap();
        assert_eq!(result.common_gain_db[0], 1.5);
        assert_eq!(result.left_gain_db[0], 1.5);
        assert_eq!(result.right_gain_db[0], 1.5);
        assert_eq!(result.common_gain_db[1], 0.0);
        assert_eq!(result.left_gain_db[1], 0.0);
        assert_eq!(result.right_gain_db[1], 0.0);
        assert_eq!(result.left_gain_db[3], 3.0);
        assert_eq!(result.right_gain_db[3], 1.5);
    }
}
