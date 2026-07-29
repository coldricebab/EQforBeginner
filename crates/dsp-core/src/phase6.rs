//! Phase 6 native-rate redesign and deterministic cross-rate validation.
//!
//! This module consumes the physical-frequency correction intent, not a 48 kHz
//! impulse response. Each output rate is synthesized on its own FFT grid.  It
//! cannot establish hardware verification, clipping safety, or export
//! eligibility; those are evidence decisions owned by the application layer.

use crate::correction::validate_correction_invariant;
use crate::fir::{synthesize_minimum_phase, FirFilter};
use crate::phase4::{native_frequency_grid, resample_correction_to_native_grid};
use crate::target::interpolate_log_frequency_grid;
use crate::{DspError, DspResult};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

pub const PHASE6_ALGORITHM_VERSION: &str = "phase6-native-six-rate-v2";
pub const ROON_NATIVE_SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
pub const DEFAULT_CROSS_RATE_MAGNITUDE_TOLERANCE_DB: f64 = 0.05;
pub const DEFAULT_CROSS_RATE_RELATIVE_GD_TOLERANCE_MS: f64 = 0.02;
const RESPONSE_OVERSAMPLE_FACTOR: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub struct Phase6DesignIntent {
    pub frequencies_hz: Vec<f64>,
    pub left_gain_db: Vec<f64>,
    pub right_gain_db: Vec<f64>,
    pub correction_low_hz: f64,
    pub taper_end_hz: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phase6Config {
    pub magnitude_compare_low_hz: f64,
    pub magnitude_compare_high_hz: f64,
    pub group_delay_compare_low_hz: f64,
    pub group_delay_compare_high_hz: f64,
    pub maximum_magnitude_difference_db: f64,
    pub maximum_relative_group_delay_difference_ms: f64,
    pub comparison_points: usize,
}

impl Default for Phase6Config {
    fn default() -> Self {
        Self {
            magnitude_compare_low_hz: 20.0,
            magnitude_compare_high_hz: 20_000.0,
            group_delay_compare_low_hz: 20.0,
            group_delay_compare_high_hz: 650.0,
            maximum_magnitude_difference_db: DEFAULT_CROSS_RATE_MAGNITUDE_TOLERANCE_DB,
            maximum_relative_group_delay_difference_ms: DEFAULT_CROSS_RATE_RELATIVE_GD_TOLERANCE_MS,
            comparison_points: 2_048,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeRateFilter {
    pub sample_rate_hz: u32,
    pub fft_size: usize,
    pub native_frequencies_hz: Vec<f64>,
    pub left_fir: FirFilter,
    pub right_fir: FirFilter,
    pub left_additional_common_safety_attenuation_db: f64,
    pub right_additional_common_safety_attenuation_db: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CrossRateChannelMetrics {
    pub maximum_magnitude_difference_db: f64,
    pub maximum_magnitude_difference_frequency_hz: f64,
    pub maximum_relative_group_delay_difference_ms: f64,
    pub maximum_relative_group_delay_difference_frequency_hz: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CrossRateMetrics {
    pub sample_rate_hz: u32,
    pub reference_sample_rate_hz: u32,
    pub left: CrossRateChannelMetrics,
    pub right: CrossRateChannelMetrics,
    pub passed: bool,
}

/// Physical-response binding between the exact stereo FIR that was verified
/// at 48 kHz and the 48 kHz member of a native-rate export.
#[derive(Debug, Clone, PartialEq)]
pub struct StereoFilterBindingMetrics {
    pub sample_rate_hz: u32,
    pub left: CrossRateChannelMetrics,
    pub right: CrossRateChannelMetrics,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phase6NativeResult {
    pub algorithm_version: &'static str,
    pub filters: Vec<NativeRateFilter>,
    /// The most conservative realization normalization found across every
    /// channel and rate. All FIRs receive attenuation-only alignment to this
    /// value so sample-rate switching cannot change playback level.
    pub common_safety_normalization_db: f64,
    pub comparisons: Vec<CrossRateMetrics>,
    pub cross_rate_passed: bool,
}

fn validate_intent(intent: &Phase6DesignIntent) -> DspResult<()> {
    let length = intent.frequencies_hz.len();
    if length < 3 || intent.left_gain_db.len() != length || intent.right_gain_db.len() != length {
        return Err(DspError::ShapeMismatch(
            "Phase 6 frequency and L/R design grids must have equal lengths of at least three"
                .into(),
        ));
    }
    if !intent.correction_low_hz.is_finite()
        || !intent.taper_end_hz.is_finite()
        || intent.correction_low_hz <= 0.0
        || intent.taper_end_hz <= intent.correction_low_hz
    {
        return Err(DspError::InvalidArgument(
            "Phase 6 correction limits must be finite and ordered".into(),
        ));
    }
    for (index, &frequency) in intent.frequencies_hz.iter().enumerate() {
        if !frequency.is_finite() || frequency <= 0.0 {
            return Err(DspError::InvalidArgument(format!(
                "Phase 6 frequency bin {index} must be finite and positive"
            )));
        }
        if index > 0 && frequency <= intent.frequencies_hz[index - 1] {
            return Err(DspError::InvalidArgument(
                "Phase 6 frequency grid must be strictly increasing".into(),
            ));
        }
    }
    // A measured grid need not land exactly on the nominal 650 Hz endpoint;
    // accept only the sub-bin remainder that is already effectively unity.
    if intent.frequencies_hz[0] > intent.correction_low_hz * 1.02
        || *intent.frequencies_hz.last().unwrap_or(&0.0) < intent.taper_end_hz * 0.999
    {
        return Err(DspError::InvalidArgument(
            "Phase 6 design does not cover the declared correction/taper band".into(),
        ));
    }
    validate_correction_invariant(&intent.left_gain_db)?;
    validate_correction_invariant(&intent.right_gain_db)?;
    Ok(())
}

fn validate_config(config: &Phase6Config) -> DspResult<()> {
    let magnitude_band = config.magnitude_compare_low_hz.is_finite()
        && config.magnitude_compare_high_hz.is_finite()
        && config.magnitude_compare_low_hz > 0.0
        && config.magnitude_compare_high_hz > config.magnitude_compare_low_hz
        && config.magnitude_compare_high_hz <= 20_000.0;
    let gd_band = config.group_delay_compare_low_hz.is_finite()
        && config.group_delay_compare_high_hz.is_finite()
        && config.group_delay_compare_low_hz > 0.0
        && config.group_delay_compare_high_hz > config.group_delay_compare_low_hz;
    if !magnitude_band || !gd_band || config.comparison_points < 128 {
        return Err(DspError::InvalidArgument(
            "Phase 6 comparison bands or grid size are invalid".into(),
        ));
    }
    if !config.maximum_magnitude_difference_db.is_finite()
        || config.maximum_magnitude_difference_db <= 0.0
        || config.maximum_magnitude_difference_db > DEFAULT_CROSS_RATE_MAGNITUDE_TOLERANCE_DB
        || !config
            .maximum_relative_group_delay_difference_ms
            .is_finite()
        || config.maximum_relative_group_delay_difference_ms <= 0.0
        || config.maximum_relative_group_delay_difference_ms
            > DEFAULT_CROSS_RATE_RELATIVE_GD_TOLERANCE_MS
    {
        return Err(DspError::InvalidArgument(
            "Phase 6 comparison tolerances exceed product safety limits".into(),
        ));
    }
    Ok(())
}

#[must_use]
pub const fn native_fft_size(sample_rate_hz: u32) -> Option<usize> {
    match sample_rate_hz {
        44_100 => Some(14_994),
        48_000 => Some(16_320),
        88_200 => Some(29_988),
        96_000 => Some(32_640),
        176_400 => Some(59_976),
        192_000 => Some(65_280),
        _ => None,
    }
}

fn log_grid(low_hz: f64, high_hz: f64, points: usize) -> Vec<f64> {
    let ratio = high_hz / low_hz;
    (0..points)
        .map(|index| low_hz * ratio.powf(index as f64 / (points - 1) as f64))
        .collect()
}

struct DenseResponse {
    frequencies_hz: Vec<f64>,
    magnitude_db: Vec<f64>,
    group_delay_ms: Vec<f64>,
}

fn dense_response(fir: &FirFilter) -> DspResult<DenseResponse> {
    let fft_size = fir
        .taps
        .len()
        .checked_mul(RESPONSE_OVERSAMPLE_FACTOR)
        .ok_or_else(|| DspError::InvalidArgument("Phase 6 response FFT overflow".into()))?;
    let mut spectrum = vec![Complex::new(0.0, 0.0); fft_size];
    for (bin, &tap) in spectrum.iter_mut().zip(&fir.taps) {
        bin.re = tap;
    }
    FftPlanner::<f64>::new()
        .plan_fft_forward(fft_size)
        .process(&mut spectrum);
    let one_sided = fft_size / 2 + 1;
    let mut phase: Vec<f64> = spectrum[..one_sided]
        .iter()
        .map(|value| value.arg())
        .collect();
    for index in 1..phase.len() {
        let mut delta = phase[index] - phase[index - 1];
        while delta > std::f64::consts::PI {
            phase[index] -= 2.0 * std::f64::consts::PI;
            delta -= 2.0 * std::f64::consts::PI;
        }
        while delta < -std::f64::consts::PI {
            phase[index] += 2.0 * std::f64::consts::PI;
            delta += 2.0 * std::f64::consts::PI;
        }
    }
    let delta_omega = 2.0 * std::f64::consts::PI / fft_size as f64;
    let mut group_delay_ms = vec![0.0; one_sided];
    for index in 1..one_sided - 1 {
        let samples = -(phase[index + 1] - phase[index - 1]) / (2.0 * delta_omega);
        group_delay_ms[index] = samples * 1_000.0 / f64::from(fir.sample_rate_hz);
    }
    group_delay_ms[0] = group_delay_ms[1];
    group_delay_ms[one_sided - 1] = group_delay_ms[one_sided - 2];
    if group_delay_ms.iter().any(|value| !value.is_finite()) {
        return Err(DspError::InvalidArgument(
            "Phase 6 group-delay response is non-finite".into(),
        ));
    }
    Ok(DenseResponse {
        frequencies_hz: native_frequency_grid(fir.sample_rate_hz, fft_size),
        magnitude_db: spectrum[..one_sided]
            .iter()
            .map(|value| 20.0 * value.norm().max(1.0e-300).log10())
            .collect(),
        group_delay_ms,
    })
}

fn channel_metrics(
    reference: &DenseResponse,
    candidate: &DenseResponse,
    magnitude_queries: &[f64],
    gd_queries: &[f64],
) -> DspResult<CrossRateChannelMetrics> {
    let reference_magnitude = interpolate_log_frequency_grid(
        &reference.frequencies_hz,
        &reference.magnitude_db,
        magnitude_queries,
    )?;
    let candidate_magnitude = interpolate_log_frequency_grid(
        &candidate.frequencies_hz,
        &candidate.magnitude_db,
        magnitude_queries,
    )?;
    let reference_gd = interpolate_log_frequency_grid(
        &reference.frequencies_hz,
        &reference.group_delay_ms,
        gd_queries,
    )?;
    let candidate_gd = interpolate_log_frequency_grid(
        &candidate.frequencies_hz,
        &candidate.group_delay_ms,
        gd_queries,
    )?;
    let (magnitude_index, maximum_magnitude_difference_db) = reference_magnitude
        .iter()
        .zip(candidate_magnitude)
        .enumerate()
        .map(|(index, (reference, candidate))| (index, (reference - candidate).abs()))
        .max_by(|(_, left), (_, right)| left.total_cmp(right))
        .unwrap_or((0, 0.0));
    let (gd_index, maximum_relative_group_delay_difference_ms) = reference_gd
        .iter()
        .zip(candidate_gd)
        .enumerate()
        .map(|(index, (reference, candidate))| (index, (reference - candidate).abs()))
        .max_by(|(_, left), (_, right)| left.total_cmp(right))
        .unwrap_or((0, 0.0));
    Ok(CrossRateChannelMetrics {
        maximum_magnitude_difference_db,
        maximum_magnitude_difference_frequency_hz: magnitude_queries[magnitude_index],
        maximum_relative_group_delay_difference_ms,
        maximum_relative_group_delay_difference_frequency_hz: gd_queries[gd_index],
    })
}

/// Compare two stereo FIR pairs on physical frequency and relative group-delay
/// grids. This is used to keep a native-rate export bound to the exact 48 kHz
/// FIR that passed closed-loop hardware verification.
pub fn compare_stereo_filter_responses(
    verified_left: &FirFilter,
    verified_right: &FirFilter,
    export_left: &FirFilter,
    export_right: &FirFilter,
    config: &Phase6Config,
) -> DspResult<StereoFilterBindingMetrics> {
    validate_config(config)?;
    let sample_rate_hz = verified_left.sample_rate_hz;
    if [
        verified_right.sample_rate_hz,
        export_left.sample_rate_hz,
        export_right.sample_rate_hz,
    ]
    .iter()
    .any(|rate| *rate != sample_rate_hz)
    {
        return Err(DspError::ShapeMismatch(
            "verified/export stereo FIRs must use one sample rate".into(),
        ));
    }
    let verified_left_response = dense_response(verified_left)?;
    let verified_right_response = dense_response(verified_right)?;
    let export_left_response = dense_response(export_left)?;
    let export_right_response = dense_response(export_right)?;
    let magnitude_queries = log_grid(
        config.magnitude_compare_low_hz,
        config.magnitude_compare_high_hz,
        config.comparison_points,
    );
    let gd_queries = log_grid(
        config.group_delay_compare_low_hz,
        config.group_delay_compare_high_hz,
        config.comparison_points,
    );
    let left = channel_metrics(
        &verified_left_response,
        &export_left_response,
        &magnitude_queries,
        &gd_queries,
    )?;
    let right = channel_metrics(
        &verified_right_response,
        &export_right_response,
        &magnitude_queries,
        &gd_queries,
    )?;
    let passed = left.maximum_magnitude_difference_db <= config.maximum_magnitude_difference_db
        && right.maximum_magnitude_difference_db <= config.maximum_magnitude_difference_db
        && left.maximum_relative_group_delay_difference_ms
            <= config.maximum_relative_group_delay_difference_ms
        && right.maximum_relative_group_delay_difference_ms
            <= config.maximum_relative_group_delay_difference_ms;
    Ok(StereoFilterBindingMetrics {
        sample_rate_hz,
        left,
        right,
        passed,
    })
}

/// Synthesize the six native-rate minimum-phase filters.
pub fn design_native_rate_filters(
    intent: &Phase6DesignIntent,
    config: &Phase6Config,
) -> DspResult<Phase6NativeResult> {
    validate_intent(intent)?;
    validate_config(config)?;
    let mut filters = Vec::with_capacity(ROON_NATIVE_SAMPLE_RATES.len());
    for sample_rate_hz in ROON_NATIVE_SAMPLE_RATES {
        let fft_size = native_fft_size(sample_rate_hz).ok_or_else(|| {
            DspError::InvalidArgument(format!("unsupported Phase 6 rate {sample_rate_hz}"))
        })?;
        let frequencies = native_frequency_grid(sample_rate_hz, fft_size);
        let left_gain = resample_correction_to_native_grid(
            &intent.frequencies_hz,
            &intent.left_gain_db,
            &frequencies,
            intent.correction_low_hz,
            intent.taper_end_hz,
        )?;
        let right_gain = resample_correction_to_native_grid(
            &intent.frequencies_hz,
            &intent.right_gain_db,
            &frequencies,
            intent.correction_low_hz,
            intent.taper_end_hz,
        )?;
        filters.push(NativeRateFilter {
            sample_rate_hz,
            fft_size,
            native_frequencies_hz: frequencies,
            left_fir: synthesize_minimum_phase(&left_gain, sample_rate_hz)?,
            right_fir: synthesize_minimum_phase(&right_gain, sample_rate_hz)?,
            left_additional_common_safety_attenuation_db: 0.0,
            right_additional_common_safety_attenuation_db: 0.0,
        });
    }
    let common_safety_normalization_db = filters
        .iter()
        .flat_map(|filter| {
            [
                filter.left_fir.safety_normalization_db,
                filter.right_fir.safety_normalization_db,
            ]
        })
        .fold(0.0, f64::min);
    for filter in &mut filters {
        for (fir, additional) in [
            (
                &mut filter.left_fir,
                &mut filter.left_additional_common_safety_attenuation_db,
            ),
            (
                &mut filter.right_fir,
                &mut filter.right_additional_common_safety_attenuation_db,
            ),
        ] {
            *additional = common_safety_normalization_db - fir.safety_normalization_db;
            let amplitude = 10.0_f64.powf(*additional / 20.0);
            for tap in &mut fir.taps {
                *tap *= amplitude;
            }
            fir.safety_normalization_db = common_safety_normalization_db;
        }
    }
    let reference_index = filters
        .iter()
        .position(|filter| filter.sample_rate_hz == 48_000)
        .ok_or_else(|| DspError::InvalidArgument("Phase 6 reference rate is absent".into()))?;
    let dense_responses: Vec<(DenseResponse, DenseResponse)> = filters
        .iter()
        .map(|filter| {
            Ok((
                dense_response(&filter.left_fir)?,
                dense_response(&filter.right_fir)?,
            ))
        })
        .collect::<DspResult<_>>()?;
    let reference = &dense_responses[reference_index];
    let magnitude_queries = log_grid(
        config.magnitude_compare_low_hz,
        config.magnitude_compare_high_hz,
        config.comparison_points,
    );
    let gd_queries = log_grid(
        config.group_delay_compare_low_hz,
        config.group_delay_compare_high_hz,
        config.comparison_points,
    );
    let mut comparisons = Vec::with_capacity(filters.len());
    for (filter, response) in filters.iter().zip(&dense_responses) {
        let left = channel_metrics(&reference.0, &response.0, &magnitude_queries, &gd_queries)?;
        let right = channel_metrics(&reference.1, &response.1, &magnitude_queries, &gd_queries)?;
        let passed = left.maximum_magnitude_difference_db <= config.maximum_magnitude_difference_db
            && right.maximum_magnitude_difference_db <= config.maximum_magnitude_difference_db
            && left.maximum_relative_group_delay_difference_ms
                <= config.maximum_relative_group_delay_difference_ms
            && right.maximum_relative_group_delay_difference_ms
                <= config.maximum_relative_group_delay_difference_ms;
        comparisons.push(CrossRateMetrics {
            sample_rate_hz: filter.sample_rate_hz,
            reference_sample_rate_hz: 48_000,
            left,
            right,
            passed,
        });
    }
    let cross_rate_passed = comparisons.iter().all(|comparison| comparison.passed);
    Ok(Phase6NativeResult {
        algorithm_version: PHASE6_ALGORITHM_VERSION,
        filters,
        common_safety_normalization_db,
        comparisons,
        cross_rate_passed,
    })
}

/// Rebuild the legacy Phase 4 48 kHz source grid from the same physical design
/// intent. This is an evidence-binding operation only; Phase 6 outputs must use
/// [`design_native_rate_filters`] and its common-duration native grids.
pub fn reproduce_phase4_48k_source(
    intent: &Phase6DesignIntent,
) -> DspResult<(FirFilter, FirFilter)> {
    validate_intent(intent)?;
    let fft_size = crate::phase4::DEFAULT_PHASE4_FIR_FFT_SIZE;
    let frequencies = native_frequency_grid(48_000, fft_size);
    let left_gain = resample_correction_to_native_grid(
        &intent.frequencies_hz,
        &intent.left_gain_db,
        &frequencies,
        intent.correction_low_hz,
        intent.taper_end_hz,
    )?;
    let right_gain = resample_correction_to_native_grid(
        &intent.frequencies_hz,
        &intent.right_gain_db,
        &frequencies,
        intent.correction_low_hz,
        intent.taper_end_hz,
    )?;
    Ok((
        synthesize_minimum_phase(&left_gain, 48_000)?,
        synthesize_minimum_phase(&right_gain, 48_000)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent() -> Phase6DesignIntent {
        let frequencies_hz: Vec<f64> = (0..512)
            .map(|index| 20.0 * (650.0_f64 / 20.0).powf(index as f64 / 511.0))
            .collect();
        let gain = frequencies_hz
            .iter()
            .map(|frequency| {
                let peak = -6.0 * (-0.5 * ((*frequency / 55.0).log2() / 0.18).powi(2)).exp();
                let boost = 2.5 * (-0.5 * ((*frequency / 120.0).log2() / 0.28).powi(2)).exp();
                let taper = if *frequency <= 500.0 {
                    1.0
                } else {
                    0.5 * (1.0 + (std::f64::consts::PI * (*frequency - 500.0) / 150.0).cos())
                };
                (peak + boost) * taper
            })
            .collect::<Vec<_>>();
        Phase6DesignIntent {
            frequencies_hz,
            left_gain_db: gain.clone(),
            right_gain_db: gain,
            correction_low_hz: 20.0,
            taper_end_hz: 650.0,
        }
    }

    #[test]
    fn six_rates_are_redesigned_on_native_grids_and_match() {
        let result = design_native_rate_filters(&intent(), &Phase6Config::default()).unwrap();
        assert!(result.cross_rate_passed, "{:?}", result.comparisons);
        assert_eq!(result.filters.len(), 6);
        for (filter, expected_rate) in result.filters.iter().zip(ROON_NATIVE_SAMPLE_RATES) {
            assert_eq!(filter.sample_rate_hz, expected_rate);
            assert_eq!(filter.fft_size, native_fft_size(expected_rate).unwrap());
            assert_eq!(expected_rate as usize * 17, filter.fft_size * 50);
            assert_eq!(filter.left_fir.taps.len(), filter.fft_size);
            assert_eq!(
                filter.left_fir.safety_normalization_db,
                result.common_safety_normalization_db
            );
            assert!(filter.left_additional_common_safety_attenuation_db <= 0.0);
            assert!(filter.right_additional_common_safety_attenuation_db <= 0.0);
            assert!(filter
                .left_fir
                .response_db(filter.fft_size * 2)
                .unwrap()
                .iter()
                .all(|gain| { *gain <= crate::correction::MAXIMUM_SUPPORTED_BOOST_DB + 1.0e-9 }));
            assert!(filter
                .left_fir
                .response_db(filter.fft_size * 2)
                .unwrap()
                .iter()
                .any(|gain| *gain > 2.0));
        }
    }

    #[test]
    fn invalid_or_over_limit_design_is_rejected() {
        let mut invalid = intent();
        invalid.left_gain_db[10] = crate::correction::MAXIMUM_SUPPORTED_BOOST_DB + 0.1;
        assert!(design_native_rate_filters(&invalid, &Phase6Config::default()).is_err());
        let mut invalid = intent();
        invalid.frequencies_hz.swap(10, 11);
        assert!(design_native_rate_filters(&invalid, &Phase6Config::default()).is_err());
    }

    #[test]
    fn product_tolerances_cannot_be_relaxed() {
        let config = Phase6Config {
            maximum_magnitude_difference_db: 0.051,
            ..Phase6Config::default()
        };
        assert!(design_native_rate_filters(&intent(), &config).is_err());
        let config = Phase6Config {
            maximum_relative_group_delay_difference_ms: 0.021,
            ..Phase6Config::default()
        };
        assert!(design_native_rate_filters(&intent(), &config).is_err());
    }

    #[test]
    fn native_48k_export_remains_bound_to_the_phase4_verified_response() {
        let intent = intent();
        let (verified_left, verified_right) = reproduce_phase4_48k_source(&intent).unwrap();
        let native = design_native_rate_filters(&intent, &Phase6Config::default()).unwrap();
        let export = native
            .filters
            .iter()
            .find(|filter| filter.sample_rate_hz == 48_000)
            .unwrap();
        let binding = compare_stereo_filter_responses(
            &verified_left,
            &verified_right,
            &export.left_fir,
            &export.right_fir,
            &Phase6Config::default(),
        )
        .unwrap();
        assert!(binding.passed, "{binding:?}");

        let mut changed_left = export.left_fir.clone();
        for tap in &mut changed_left.taps {
            *tap *= 0.9;
        }
        let rejected = compare_stereo_filter_responses(
            &verified_left,
            &verified_right,
            &changed_left,
            &export.right_fir,
            &Phase6Config::default(),
        )
        .unwrap();
        assert!(!rejected.passed);
        assert!(rejected.left.maximum_magnitude_difference_db > 0.05);
    }
}
