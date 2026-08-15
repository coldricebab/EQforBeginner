//! Versioned house-curve presets and logarithmic-frequency interpolation.
//!
//! The `-style` values are explicitly design curves, not claimed to be
//! official curves from either organization. The `Harman6db` preset follows
//! the Harman +6 dB target curve that Dirac publishes on its official site
//! (see [`HARMAN_6DB_TARGET_SOURCE_URL`]).

use crate::smoothing::gaussian_log_frequency_smooth_at_db;
use crate::{DspError, DspResult};

pub const TARGET_TXT_PARSER_VERSION: &str = "target-txt-parser-v1";

pub const HARMAN_6DB_TARGET_NAME: &str = "Harman-6dB (Dirac) house curve";
pub const HARMAN_6DB_TARGET_VERSION: &str = "dirac-harman-6db-v1-log-f-linear-db";
pub const HARMAN_6DB_ADAPTIVE_HF_TARGET_NAME: &str =
    "Harman-6dB (Dirac) house curve + measured HF rolloff";
pub const HARMAN_6DB_ADAPTIVE_HF_VERSION: &str = "harman-6db-adaptive-hf-v1";
/// Where the bass shelf of the `Harman6db` preset comes from: the Harman
/// +6 dB target curve published (with +4/+8 dB siblings) on Dirac's official
/// target-curve resource page. The page states the Harman curves carry no
/// high-frequency rolloff of their own because that part should be set per
/// room and speaker - which is exactly what the adaptive HF extension below
/// does.
pub const HARMAN_6DB_TARGET_SOURCE_URL: &str = "https://www.dirac.com/resources/target-curve";

/// Preferred downward in-room slope above the 500 Hz anchor.
///
/// This is the gently declining steady-state in-room response reported as
/// preferred in the Toole/Olive listening research (roughly -1 dB per octave
/// through the mid/treble). It is a literature constant, not a value derived
/// from any particular speaker or room measured with this app.
pub const PREFERRED_IN_ROOM_SLOPE_DB_PER_OCTAVE: f64 = -1.0;

/// Largest droop the adaptive HF target may request relative to the 500 Hz
/// anchor. Chosen to equal the correction engine's own
/// `MAXIMUM_SUPPORTED_ATTENUATION_DB` (12 dB): a measured trend that would
/// pull the target further down than the corrector could ever attenuate is
/// evidence of a broken measurement, not of a speaker to be followed.
pub const ADAPTIVE_HF_MAXIMUM_DROOP_DB: f64 = 12.0;

/// The 500 Hz anchor is measured over a band, not a single bin: one bin of a
/// smoothed spatial average is still noisy enough to tilt the whole fitted
/// trend line. 400-630 Hz is the third-octave neighborhood of the anchor.
const ADAPTIVE_HF_ANCHOR_LOW_HZ: f64 = 400.0;
const ADAPTIVE_HF_ANCHOR_HIGH_HZ: f64 = 630.0;
const ADAPTIVE_HF_ANCHOR_QUERY_POINTS: usize = 9;

/// Trend-fit band. 2 kHz keeps the fit clear of the 500 Hz anchor region and
/// of midrange room modes so only the treble trend enters; 20 kHz is the
/// audible top. A least-squares line over this band deliberately ignores
/// local peaks and dips - those remain ordinary correction targets - and
/// captures only the broad driver/room HF trend.
const ADAPTIVE_HF_FIT_LOW_HZ: f64 = 2_000.0;
const ADAPTIVE_HF_FIT_HIGH_HZ: f64 = 20_000.0;
/// Without measured evidence to at least 10 kHz the top-octave trend would be
/// pure extrapolation, so the adaptive path refuses and the static curve is
/// used instead.
const ADAPTIVE_HF_MINIMUM_COVERAGE_HZ: f64 = 10_000.0;
const ADAPTIVE_HF_MINIMUM_FIT_POINTS: usize = 6;
/// 1/3 octave: wide enough to erase individual room reflections, narrow
/// enough that a genuine driver rolloff is not smoothed away.
const ADAPTIVE_HF_SMOOTHING_FWHM_OCTAVES: f64 = 1.0 / 3.0;

/// Sub-500 Hz knots of the bundled Harman +6 dB curve, byte-identical to
/// `assets/targets/Harman-6dB_REW.txt` (pinned by a test against that file).
/// The file's final `20000 0` row is intentionally absent: everything at and
/// above 500 Hz is produced by the preferred-slope / adaptive logic below.
const HARMAN_6DB_BASS_KNOTS: [(f64, f64); 25] = [
    (9.98753, 5.99947),
    (19.9501, 5.99156),
    (29.8697, 5.95785),
    (39.8505, 5.86876),
    (50.1876, 5.6819),
    (59.6648, 5.40044),
    (70.1184, 4.96129),
    (79.6016, 4.46667),
    (90.3672, 3.84383),
    (100.25, 3.26654),
    (109.938, 2.73537),
    (120.563, 2.21982),
    (130.699, 1.80376),
    (140.062, 1.4842),
    (150.096, 1.20398),
    (160.849, 0.964491),
    (180.509, 0.651208),
    (200.25, 0.448594),
    (219.602, 0.318503),
    (240.825, 0.224409),
    (261.071, 0.164427),
    (279.774, 0.125629),
    (299.817, 0.0958133),
    (321.296, 0.0729731),
    (400.0, 0.0306274),
];

/// The frequency where the bass shelf ends and the HF slope begins; also the
/// 0 dB anchor of the whole curve family.
const HARMAN_6DB_ANCHOR_HZ: f64 = 500.0;

/// Third-octave emit grid above the anchor, matching the granularity of the
/// reference target files, extended to 24 kHz so a 48 kHz design grid never
/// extrapolates.
const HARMAN_6DB_HF_KNOT_FREQUENCIES_HZ: [f64; 17] = [
    630.0, 800.0, 1_000.0, 1_250.0, 1_600.0, 2_000.0, 2_500.0, 3_150.0, 4_000.0, 5_000.0, 6_300.0,
    8_000.0, 10_000.0, 12_500.0, 16_000.0, 20_000.0, 24_000.0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPreset {
    BkStyle,
    HarmanStyle,
    /// The app's built-in default target. Below 500 Hz it is the Harman
    /// +6 dB bass shelf as published on Dirac's official site
    /// ([`HARMAN_6DB_TARGET_SOURCE_URL`]); from 500 Hz upward it follows the
    /// preferred in-room slope of [`PREFERRED_IN_ROOM_SLOPE_DB_PER_OCTAVE`].
    /// This static form is the fallback; the live design normally adapts the
    /// HF part to the measurement via [`harman_6db_adaptive_hf_target`].
    Harman6db,
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
            TargetPreset::Harman6db => Self {
                name: HARMAN_6DB_TARGET_NAME.into(),
                version: HARMAN_6DB_TARGET_VERSION.into(),
                knots: harman_6db_static_knots(),
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

/// Level of the preferred in-room slope line at `frequency_hz`, relative to
/// the 0 dB anchor at 500 Hz.
fn preferred_slope_level_db(frequency_hz: f64) -> f64 {
    PREFERRED_IN_ROOM_SLOPE_DB_PER_OCTAVE * (frequency_hz / HARMAN_6DB_ANCHOR_HZ).log2()
}

/// The Dirac-published bass shelf plus the explicit 500 Hz / 0 dB anchor.
fn harman_6db_bass_knots() -> Vec<TargetKnot> {
    let mut knots: Vec<TargetKnot> = HARMAN_6DB_BASS_KNOTS
        .iter()
        .map(|&(frequency_hz, level_db)| TargetKnot {
            frequency_hz,
            level_db,
        })
        .collect();
    knots.push(TargetKnot {
        frequency_hz: HARMAN_6DB_ANCHOR_HZ,
        level_db: 0.0,
    });
    knots
}

/// Static default: bass shelf, then a single preferred-slope line to 24 kHz.
fn harman_6db_static_knots() -> Vec<TargetKnot> {
    let mut knots = harman_6db_bass_knots();
    knots.extend(
        HARMAN_6DB_HF_KNOT_FREQUENCIES_HZ
            .iter()
            .map(|&frequency_hz| TargetKnot {
                frequency_hz,
                level_db: preferred_slope_level_db(frequency_hz),
            }),
    );
    knots
}

/// Why the adaptive HF fit could not run. Every reason falls back to the
/// static [`TargetPreset::Harman6db`] curve and is recorded, never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptiveHfFallbackReason {
    /// The measurement grid does not reach the 400 Hz anchor band or does not
    /// extend to at least 10 kHz.
    InsufficientCoverage,
    /// Fewer than [`ADAPTIVE_HF_MINIMUM_FIT_POINTS`] third-octave centres are
    /// usable inside the 2-20 kHz fit band.
    TooFewFitPoints,
    /// The spatial average contains non-finite values or smoothing produced
    /// a non-finite result.
    NonFiniteMeasurement,
    /// The least-squares system was degenerate.
    IllConditionedFit,
}

impl AdaptiveHfFallbackReason {
    /// Stable machine-readable code carried through design provenance.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InsufficientCoverage => "insufficient_coverage",
            Self::TooFewFitPoints => "too_few_fit_points",
            Self::NonFiniteMeasurement => "non_finite_measurement",
            Self::IllConditionedFit => "ill_conditioned_fit",
        }
    }
}

/// What [`harman_6db_adaptive_hf_target`] decided, for design provenance.
#[derive(Debug, Clone, PartialEq)]
pub enum AdaptiveHfOutcome {
    /// The fit ran and the measured treble trend sits at or above the
    /// preferred slope everywhere: the target is the full preferred-slope
    /// line (identical to the static preset).
    PreferredSlope { fitted_slope_db_per_octave: f64 },
    /// The fit ran and the measured trend crosses below the preferred slope:
    /// above `break_frequency_hz` the target follows the speaker's own
    /// declining trend, so no futile top-octave boost is demanded.
    MeasuredRolloff {
        fitted_slope_db_per_octave: f64,
        break_frequency_hz: f64,
    },
    /// A guard tripped; the static curve is used.
    Fallback { reason: AdaptiveHfFallbackReason },
}

/// Build the app-default target with a measurement-adaptive high-frequency
/// rolloff (`harman-6db-adaptive-hf-v1`).
///
/// Input is the weighted spatial-average magnitude the correction stage
/// consumes, in dB on its measured frequency grid. The algorithm:
///
/// 1. smooths the average at 1/3 octave,
/// 2. anchors it so the 400-630 Hz band mean is 0 dB,
/// 3. least-squares fits the anchored level against `log2(f / 500)` over
///    2-20 kHz, giving the broad measured treble trend `meas(f)`,
/// 4. sets the target above 500 Hz to
///    `max(min(pref(f), meas(f)), -12 dB)` on the third-octave grid, where
///    `pref(f)` is the preferred -1 dB/octave line, then enforces a
///    monotone non-increasing shape.
///
/// A speaker with extended treble therefore receives the full preferred
/// slope; a speaker that genuinely rolls off receives a target that follows
/// its own decline instead of demanding boost the driver cannot deliver.
/// Every speaker-specific number (break frequency, droop depth) emerges from
/// the run-time fit; nothing here encodes any particular measured system.
///
/// Errors are structural misuse only (mismatched grids, invalid
/// frequencies). Measurement-quality problems never error: they return the
/// static curve with a recorded [`AdaptiveHfOutcome::Fallback`].
pub fn harman_6db_adaptive_hf_target(
    frequencies_hz: &[f64],
    spatial_average_db: &[f64],
) -> DspResult<(TargetCurve, AdaptiveHfOutcome)> {
    if frequencies_hz.len() != spatial_average_db.len() || frequencies_hz.len() < 2 {
        return Err(DspError::ShapeMismatch(
            "adaptive HF target needs equal frequency/level grids with at least two bins".into(),
        ));
    }
    for (index, &frequency) in frequencies_hz.iter().enumerate() {
        if !frequency.is_finite()
            || frequency <= 0.0
            || (index > 0 && frequency <= frequencies_hz[index - 1])
        {
            return Err(DspError::InvalidArgument(
                "adaptive HF target frequencies must be finite, positive, and increasing".into(),
            ));
        }
    }
    let fallback = |reason: AdaptiveHfFallbackReason| {
        (
            TargetCurve::preset(TargetPreset::Harman6db),
            AdaptiveHfOutcome::Fallback { reason },
        )
    };
    if spatial_average_db.iter().any(|value| !value.is_finite()) {
        return Ok(fallback(AdaptiveHfFallbackReason::NonFiniteMeasurement));
    }
    let low = frequencies_hz[0];
    let high = *frequencies_hz.last().expect("length checked above");
    if low > ADAPTIVE_HF_ANCHOR_LOW_HZ || high < ADAPTIVE_HF_MINIMUM_COVERAGE_HZ {
        return Ok(fallback(AdaptiveHfFallbackReason::InsufficientCoverage));
    }

    // Smoothing queries: a small log-spaced anchor band plus the usable
    // third-octave fit centres.
    let anchor_ratio = ADAPTIVE_HF_ANCHOR_HIGH_HZ / ADAPTIVE_HF_ANCHOR_LOW_HZ;
    let mut queries: Vec<f64> = (0..ADAPTIVE_HF_ANCHOR_QUERY_POINTS)
        .map(|index| {
            ADAPTIVE_HF_ANCHOR_LOW_HZ
                * anchor_ratio.powf(index as f64 / (ADAPTIVE_HF_ANCHOR_QUERY_POINTS - 1) as f64)
        })
        .collect();
    let fit_centres: Vec<f64> = HARMAN_6DB_HF_KNOT_FREQUENCIES_HZ
        .iter()
        .copied()
        .filter(|&centre| {
            (ADAPTIVE_HF_FIT_LOW_HZ..=ADAPTIVE_HF_FIT_HIGH_HZ).contains(&centre) && centre <= high
        })
        .collect();
    if fit_centres.len() < ADAPTIVE_HF_MINIMUM_FIT_POINTS {
        return Ok(fallback(AdaptiveHfFallbackReason::TooFewFitPoints));
    }
    queries.extend(&fit_centres);
    let smoothed = match gaussian_log_frequency_smooth_at_db(
        frequencies_hz,
        spatial_average_db,
        &queries,
        ADAPTIVE_HF_SMOOTHING_FWHM_OCTAVES,
    ) {
        Ok(values) => values,
        Err(_) => return Ok(fallback(AdaptiveHfFallbackReason::NonFiniteMeasurement)),
    };
    let anchor_db = smoothed[..ADAPTIVE_HF_ANCHOR_QUERY_POINTS]
        .iter()
        .sum::<f64>()
        / ADAPTIVE_HF_ANCHOR_QUERY_POINTS as f64;

    // Least squares of the anchored level against octaves above 500 Hz.
    let points: Vec<(f64, f64)> = fit_centres
        .iter()
        .zip(&smoothed[ADAPTIVE_HF_ANCHOR_QUERY_POINTS..])
        .map(|(&centre, &level)| ((centre / HARMAN_6DB_ANCHOR_HZ).log2(), level - anchor_db))
        .collect();
    let count = points.len() as f64;
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / count;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / count;
    let denominator = points
        .iter()
        .map(|(x, _)| (x - mean_x).powi(2))
        .sum::<f64>();
    if denominator < 1.0e-9 {
        return Ok(fallback(AdaptiveHfFallbackReason::IllConditionedFit));
    }
    let slope = points
        .iter()
        .map(|(x, y)| (x - mean_x) * (y - mean_y))
        .sum::<f64>()
        / denominator;
    let intercept = mean_y - slope * mean_x;
    if !slope.is_finite() || !intercept.is_finite() {
        return Ok(fallback(AdaptiveHfFallbackReason::IllConditionedFit));
    }

    // A rising (or flat) treble trend never bends the target: the preferred
    // slope already asks for less treble than such a speaker delivers.
    if slope >= 0.0 {
        return Ok((
            TargetCurve::preset(TargetPreset::Harman6db),
            AdaptiveHfOutcome::PreferredSlope {
                fitted_slope_db_per_octave: slope,
            },
        ));
    }

    let measured_trend_db =
        |frequency_hz: f64| intercept + slope * (frequency_hz / HARMAN_6DB_ANCHOR_HZ).log2();
    let mut bends = false;
    let mut previous_level = 0.0_f64;
    let mut first_bend_hz = None;
    let mut knots = harman_6db_bass_knots();
    for &frequency_hz in &HARMAN_6DB_HF_KNOT_FREQUENCIES_HZ {
        let preferred = preferred_slope_level_db(frequency_hz);
        let measured = measured_trend_db(frequency_hz);
        if measured < preferred - 1.0e-9 && first_bend_hz.is_none() {
            first_bend_hz = Some(frequency_hz);
        }
        let level = preferred
            .min(measured)
            .max(-ADAPTIVE_HF_MAXIMUM_DROOP_DB)
            // Monotone non-increasing above the anchor by construction; the
            // running minimum only defends the invariant against future
            // edits to the level formula.
            .min(previous_level);
        if (level - preferred).abs() > 1.0e-9 {
            bends = true;
        }
        previous_level = level;
        knots.push(TargetKnot {
            frequency_hz,
            level_db: level,
        });
    }
    if !bends {
        return Ok((
            TargetCurve::preset(TargetPreset::Harman6db),
            AdaptiveHfOutcome::PreferredSlope {
                fitted_slope_db_per_octave: slope,
            },
        ));
    }

    // Analytic crossing of the two lines, clamped to the emitted range; the
    // first bent knot is the fallback when the division degenerates.
    let slope_difference = PREFERRED_IN_ROOM_SLOPE_DB_PER_OCTAVE - slope;
    let break_frequency_hz = if slope_difference.abs() > 1.0e-12 {
        let octaves = intercept / slope_difference;
        let crossing = HARMAN_6DB_ANCHOR_HZ * octaves.exp2();
        if crossing.is_finite() {
            crossing.clamp(
                HARMAN_6DB_ANCHOR_HZ,
                *HARMAN_6DB_HF_KNOT_FREQUENCIES_HZ
                    .last()
                    .expect("nonempty grid"),
            )
        } else {
            first_bend_hz.unwrap_or(HARMAN_6DB_ANCHOR_HZ)
        }
    } else {
        first_bend_hz.unwrap_or(HARMAN_6DB_ANCHOR_HZ)
    };
    let curve = TargetCurve::from_knots(
        HARMAN_6DB_ADAPTIVE_HF_TARGET_NAME,
        format!("{HARMAN_6DB_TARGET_VERSION}+{HARMAN_6DB_ADAPTIVE_HF_VERSION}"),
        knots,
    )?;
    Ok((
        curve,
        AdaptiveHfOutcome::MeasuredRolloff {
            fitted_slope_db_per_octave: slope,
            break_frequency_hz,
        },
    ))
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

    /// The built-in default's bass shelf is pinned to the repository copy of
    /// the Dirac-published Harman +6 dB file: if either side drifts, this
    /// fails and the attribution would be a lie.
    #[test]
    fn the_harman_6db_bass_shelf_is_the_repository_dirac_file_verbatim() {
        let asset_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/targets/Harman-6dB_REW.txt");
        let text = std::fs::read_to_string(&asset_path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", asset_path.display()));
        let file_curve = parse_target_txt(&text).unwrap();
        let file_bass: Vec<&TargetKnot> = file_curve
            .knots()
            .iter()
            .filter(|knot| knot.frequency_hz < 500.0)
            .collect();
        let preset = TargetCurve::preset(TargetPreset::Harman6db);
        let preset_bass: Vec<&TargetKnot> = preset
            .knots()
            .iter()
            .filter(|knot| knot.frequency_hz < 500.0)
            .collect();
        assert_eq!(file_bass.len(), 25);
        assert_eq!(preset_bass.len(), file_bass.len());
        for (preset_knot, file_knot) in preset_bass.iter().zip(&file_bass) {
            assert_eq!(preset_knot.frequency_hz, file_knot.frequency_hz);
            assert_eq!(preset_knot.level_db, file_knot.level_db);
        }
    }

    #[test]
    fn the_static_harman_6db_curve_is_anchored_and_slopes_at_minus_one_db_per_octave() {
        let preset = TargetCurve::preset(TargetPreset::Harman6db);
        assert_eq!(preset.version(), HARMAN_6DB_TARGET_VERSION);
        assert!((preset.level_at(500.0).unwrap()).abs() < 1.0e-12);
        assert!((preset.level_at(1_000.0).unwrap() + 1.0).abs() < 1.0e-9);
        assert!((preset.level_at(4_000.0).unwrap() + 3.0).abs() < 1.0e-9);
        assert!((preset.level_at(16_000.0).unwrap() + 5.0).abs() < 1.0e-9);
        let levels: Vec<f64> = preset
            .knots()
            .iter()
            .filter(|knot| knot.frequency_hz >= 500.0)
            .map(|knot| knot.level_db)
            .collect();
        assert!(levels.windows(2).all(|pair| pair[1] <= pair[0] + 1.0e-12));
    }

    /// Synthetic log grid covering the full measured band.
    fn synthetic_grid() -> Vec<f64> {
        (0..1_200)
            .map(|index| 20.0 * (24_000.0_f64 / 20.0).powf(index as f64 / 1_199.0))
            .collect()
    }

    #[test]
    fn a_well_extended_measurement_keeps_the_full_preferred_slope() {
        let frequencies = synthetic_grid();
        // -0.5 dB/oct above 500 Hz: shallower than the preferred -1, so the
        // preferred line is the lower of the two everywhere.
        let levels: Vec<f64> = frequencies
            .iter()
            .map(|&frequency| {
                if frequency <= 500.0 {
                    70.0
                } else {
                    70.0 - 0.5 * (frequency / 500.0).log2()
                }
            })
            .collect();
        let (curve, outcome) = harman_6db_adaptive_hf_target(&frequencies, &levels).unwrap();
        assert_eq!(curve, TargetCurve::preset(TargetPreset::Harman6db));
        match outcome {
            AdaptiveHfOutcome::PreferredSlope {
                fitted_slope_db_per_octave,
            } => assert!((fitted_slope_db_per_octave + 0.5).abs() < 0.1),
            other => panic!("expected the preferred slope, got {other:?}"),
        }
    }

    #[test]
    fn a_rolled_off_measurement_bends_the_target_and_demands_no_top_octave_boost() {
        let frequencies = synthetic_grid();
        // Flat to 4 kHz, then a steep -9 dB/oct driver rolloff.
        let levels: Vec<f64> = frequencies
            .iter()
            .map(|&frequency| {
                if frequency <= 4_000.0 {
                    70.0
                } else {
                    70.0 - 9.0 * (frequency / 4_000.0).log2()
                }
            })
            .collect();
        let (curve, outcome) = harman_6db_adaptive_hf_target(&frequencies, &levels).unwrap();
        let (fitted, break_hz) = match outcome {
            AdaptiveHfOutcome::MeasuredRolloff {
                fitted_slope_db_per_octave,
                break_frequency_hz,
            } => (fitted_slope_db_per_octave, break_frequency_hz),
            other => panic!("expected a measured rolloff, got {other:?}"),
        };
        assert!(fitted < PREFERRED_IN_ROOM_SLOPE_DB_PER_OCTAVE);
        assert!((500.0..=24_000.0).contains(&break_hz));
        // Above the break the target sits below the static preferred line,
        // so correcting toward it never boosts the collapsed top octave.
        let static_curve = TargetCurve::preset(TargetPreset::Harman6db);
        for frequency in [12_500.0, 16_000.0, 20_000.0] {
            let adaptive_level = curve.level_at(frequency).unwrap();
            assert!(adaptive_level < static_curve.level_at(frequency).unwrap());
            assert!(adaptive_level >= -ADAPTIVE_HF_MAXIMUM_DROOP_DB - 1.0e-9);
        }
        // Monotone non-increasing above the anchor.
        let hf_levels: Vec<f64> = curve
            .knots()
            .iter()
            .filter(|knot| knot.frequency_hz >= 500.0)
            .map(|knot| knot.level_db)
            .collect();
        assert!(hf_levels
            .windows(2)
            .all(|pair| pair[1] <= pair[0] + 1.0e-12));
        assert!(curve.version().contains(HARMAN_6DB_ADAPTIVE_HF_VERSION));
    }

    #[test]
    fn a_rising_or_broken_measurement_falls_back_to_the_static_curve() {
        let frequencies = synthetic_grid();
        // Rising treble: the fit runs but the preferred slope is kept.
        let rising: Vec<f64> = frequencies
            .iter()
            .map(|&frequency| 60.0 + 2.0 * (frequency / 500.0).log2().max(0.0))
            .collect();
        let (curve, outcome) = harman_6db_adaptive_hf_target(&frequencies, &rising).unwrap();
        assert_eq!(curve, TargetCurve::preset(TargetPreset::Harman6db));
        assert!(matches!(outcome, AdaptiveHfOutcome::PreferredSlope { .. }));

        // A grid that stops at 5 kHz cannot justify a top-octave trend.
        let short_grid: Vec<f64> = (0..200)
            .map(|index| 20.0 * (5_000.0_f64 / 20.0).powf(index as f64 / 199.0))
            .collect();
        let short_levels = vec![70.0; short_grid.len()];
        let (curve, outcome) = harman_6db_adaptive_hf_target(&short_grid, &short_levels).unwrap();
        assert_eq!(curve, TargetCurve::preset(TargetPreset::Harman6db));
        assert_eq!(
            outcome,
            AdaptiveHfOutcome::Fallback {
                reason: AdaptiveHfFallbackReason::InsufficientCoverage
            }
        );

        // Non-finite values are a broken measurement, not an error path.
        let mut broken = vec![70.0; frequencies.len()];
        broken[600] = f64::NAN;
        let (curve, outcome) = harman_6db_adaptive_hf_target(&frequencies, &broken).unwrap();
        assert_eq!(curve, TargetCurve::preset(TargetPreset::Harman6db));
        assert_eq!(
            outcome,
            AdaptiveHfOutcome::Fallback {
                reason: AdaptiveHfFallbackReason::NonFiniteMeasurement
            }
        );

        // Structural misuse is still an error.
        assert!(harman_6db_adaptive_hf_target(&frequencies, &[0.0]).is_err());
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
