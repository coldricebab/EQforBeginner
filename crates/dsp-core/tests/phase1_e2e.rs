use eqforbeginner_dsp_core::correction::MAXIMUM_SUPPORTED_BOOST_DB;
use eqforbeginner_dsp_core::fixture::SyntheticRoomFixture;
use eqforbeginner_dsp_core::pipeline::{run_phase1, Phase1Config};

fn nearest_bin(frequencies_hz: &[f64], frequency_hz: f64) -> usize {
    frequencies_hz
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            (*left - frequency_hz)
                .abs()
                .total_cmp(&(*right - frequency_hz).abs())
        })
        .expect("frequency grid is nonempty")
        .0
}

#[test]
fn deterministic_48k_room_to_validated_stereo_fir() {
    let fixture = SyntheticRoomFixture::phase1_48k().unwrap();
    let config = Phase1Config::default();
    let result = run_phase1(&fixture, &config).unwrap();

    println!(
        "48 kHz E2E: L RMSE {:.3}->{:.3} dB, peak RMSE {:.3}->{:.3} dB, max gain {:.6} dB; \
         R RMSE {:.3}->{:.3} dB, peak RMSE {:.3}->{:.3} dB, max gain {:.6} dB",
        result.left_validation.metrics.raw_rmse_db,
        result.left_validation.metrics.predicted_rmse_db,
        result.left_validation.metrics.raw_peak_rmse_db,
        result.left_validation.metrics.predicted_peak_rmse_db,
        result.left_validation.metrics.maximum_correction_gain_db,
        result.right_validation.metrics.raw_rmse_db,
        result.right_validation.metrics.predicted_rmse_db,
        result.right_validation.metrics.raw_peak_rmse_db,
        result.right_validation.metrics.predicted_peak_rmse_db,
        result.right_validation.metrics.maximum_correction_gain_db,
    );

    assert_eq!(result.sample_rate_hz, 48_000);
    assert_eq!(result.position_weights, vec![2.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
    assert!(
        result.passed,
        "automatic validation issues: L={:?}, R={:?}",
        result.left_validation.issues, result.right_validation.issues
    );

    // The core safety invariant is checked at design, stereo blend, FIR
    // realization, and predicted-validation layers.
    assert!(result
        .left_design
        .gain_db
        .iter()
        .all(|gain| *gain <= MAXIMUM_SUPPORTED_BOOST_DB));
    assert!(result
        .right_design
        .gain_db
        .iter()
        .all(|gain| *gain <= MAXIMUM_SUPPORTED_BOOST_DB));
    assert!(result
        .stereo_design
        .left_gain_db
        .iter()
        .all(|gain| *gain <= MAXIMUM_SUPPORTED_BOOST_DB));
    assert!(result
        .stereo_design
        .right_gain_db
        .iter()
        .all(|gain| *gain <= MAXIMUM_SUPPORTED_BOOST_DB));
    assert!(
        result.left_validation.metrics.maximum_correction_gain_db <= MAXIMUM_SUPPORTED_BOOST_DB
    );
    assert!(
        result.right_validation.metrics.maximum_correction_gain_db <= MAXIMUM_SUPPORTED_BOOST_DB
    );
    assert!(result.right_design.gain_db.iter().any(|gain| *gain > 0.5));

    // Repeated broad modal peaks are attenuated.
    for center_hz in [40.0, 55.0, 66.0] {
        let bin = nearest_bin(&result.frequencies_hz, center_hz);
        assert!(
            result.stereo_design.left_gain_db[bin] < -1.0,
            "expected a cut at {center_hz} Hz, got {} dB",
            result.stereo_design.left_gain_db[bin]
        );
    }

    // A seat-local dip is explicitly protected and never boosted.
    let dip_bin = nearest_bin(&result.frequencies_hz, 83.0);
    assert!(result.left_design.protected_dip[dip_bin]);
    assert_eq!(result.left_design.gain_db[dip_bin], 0.0);

    // One isolated-seat peak has insufficient spatial support.
    let isolated_bin = nearest_bin(&result.frequencies_hz, 174.0);
    assert!(result.left_design.spatial_support[isolated_bin] < 0.5);
    assert!(result.left_design.gain_db[isolated_bin].abs() < 0.05);

    // Shared bass below 100 Hz and channel-specific correction above 140 Hz.
    for (frequency, (&left, &right)) in result.frequencies_hz.iter().zip(
        result
            .stereo_design
            .left_gain_db
            .iter()
            .zip(&result.stereo_design.right_gain_db),
    ) {
        if *frequency <= 100.0 {
            assert!((left - right).abs() < 1.0e-12);
        }
    }
    let left_specific_bin = nearest_bin(&result.frequencies_hz, 255.0);
    let right_specific_bin = nearest_bin(&result.frequencies_hz, 330.0);
    assert!(
        result.stereo_design.left_gain_db[left_specific_bin]
            < result.stereo_design.right_gain_db[left_specific_bin] - 0.5
    );
    assert!(
        result.stereo_design.right_gain_db[right_specific_bin]
            < result.stereo_design.left_gain_db[right_specific_bin] - 0.5
    );

    assert!(
        result.left_validation.metrics.predicted_peak_rmse_db
            < result.left_validation.metrics.raw_peak_rmse_db
    );
    assert!(
        result.right_validation.metrics.predicted_peak_rmse_db
            < result.right_validation.metrics.raw_peak_rmse_db
    );
    assert!(
        result.left_validation.metrics.predicted_rmse_db
            < result.left_validation.metrics.raw_rmse_db
    );
    assert!(
        result.right_validation.metrics.predicted_rmse_db
            < result.right_validation.metrics.raw_rmse_db
    );
    assert!(result.left_fir.taps.iter().all(|tap| tap.is_finite()));
    assert!(result.right_fir.taps.iter().all(|tap| tap.is_finite()));

    // Re-running the complete slice is bit-for-bit deterministic.
    let repeated = run_phase1(&fixture, &config).unwrap();
    assert_eq!(result.left_fir.taps, repeated.left_fir.taps);
    assert_eq!(result.right_fir.taps, repeated.right_fir.taps);
    assert_eq!(
        result.left_validation.metrics,
        repeated.left_validation.metrics
    );

    // Reaching a deliberately restrictive cap requires redesign and cannot be
    // silently promoted to a passing export state.
    let mut restrictive = Phase1Config::default();
    restrictive.correction.maximum_attenuation_db = 1.0;
    let restricted = run_phase1(&fixture, &restrictive).unwrap();
    assert!(!restricted.passed);
    assert!(restricted
        .left_design
        .warnings
        .iter()
        .chain(&restricted.right_design.warnings)
        .any(|warning| matches!(
            warning,
            eqforbeginner_dsp_core::correction::CorrectionWarning::AttenuationLimitReached { .. }
        )));
}
