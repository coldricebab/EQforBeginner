# DSP specification

Implemented algorithm identifiers:

- `phase1-bounded-boost-v2`: deterministic offline magnitude correction with
  spatially gated boost capped at +3 dB.
- `phase3-single-sub-ranking-v2`: deterministic ranking of measured manual
  single-subwoofer settings.
- `phase3-separated-path-delay-polarity-search-v5`: bounded complex synthesis of
  main-delay and 0/180-degree polarity alternatives across physically measured
  crossover states, scored with each candidate's sub level-matched to the mains
  (the reported deployment trim). v5 makes every candidate's evaluation
  independent of which other crossovers were listed (raw-capture arrival anchor,
  fixed 20-500 Hz scoring band, per-window coarse grids).
- `phase3-wide-band-crossover-synthesis-v1`: virtual crossover states synthesized
  from one wide-band sub capture and full-range main captures using declared
  LR4/LR2/BW2 slope models, with the measurement-state low-pass replaced rather
  than stacked; feeds the same delay/polarity optimizer.
- `phase4-response-replay-v2`: deterministic 48 kHz minimum-phase design and
  predicted-only validation from a measured combined-response baseline.
- `phase6-native-six-rate-v2`: independent native-grid synthesis at all six target
  rates, common safety normalization, and cross-rate magnitude/group-delay checks.
- `wireless-sweep-recognition-v1`: known-WAV recognition on an asynchronous
  microphone capture; this is file/offset evidence, not absolute channel timing.
- `umik-calibration-parser-v2`: strict UMIK-style TXT parsing, quoted miniDSP metadata
  compatibility, and log-frequency/linear-dB magnitude-correction interpolation.
- `known-sweep-deconvolution-v6`: recognized-capture resampling, fixed signed
  pre-zero retention for marker-referenced paths, regularized known-sweep
  deconvolution, magnitude calibration, and measurement-quality gates. v6 divides
  the microphone deviation recorded in the calibration file out of the measurement;
  v5 multiplied it in, doubling the microphone error, so a v5 snapshot's stored
  arrays are never trusted - it restores only by re-analysing its retained raw
  WAV through the current pipeline, and only when that result passes the same
  gates a fresh capture does. The re-analysis is cached beside its source as a
  current-version sidecar snapshot, so it runs once per capture. It retains
  32,768 samples (0.68 s) on a 65,536-point/0.73 Hz analysis grid so narrow
  low-frequency modes keep their measured peak level, and the signed pre-zero
  window is 4,800 samples (100 ms) so a path arriving before the acoustic marker
  reference is retained in full rather than truncated at sample zero. The
  retained tail stays inside the
  bundled sweeps' clean pre-end-marker gap, the noise-floor estimate is bounded
  to the 4,800 samples before the guard so it cannot follow the pre-roll into the
  start marker's reverberation, and caches from v3 and earlier are refused on
  restore rather than mixed with the finer grid.
- `uploaded-wav-marker-channel-analysis-v1`: main/start/end region separation and
  per-marker L/R playback-channel classification.
- `known-marker-capture-endpoint-v4`: acoustically relaxed start/end marker recognition,
  unequal repeated-candidate retention, narrowly admitted all-five-segment candidates
  whose single-marker timing fit is room-biased, strict pair-spacing validation,
  bounded live progress monitoring, pre-recognition input activity, and completion on
  the full end marker without a fixed post-marker delay.
- `umik-sweep-level-assessment-v2`: preferred and hard main-sweep digital peak/RMS
  boundaries plus an optional,
  explicitly assumed Sens-Factor SPL estimate.
- `secs-port-v1`: port of 한플 (Hanpeul)'s SECS full-band single-point mixed-phase
  correction (MIT-licensed by the author); in 2.1 it corrects both channels in common
  below the confirmed crossover. With the default-on "improved SECS" option the label
  gains `+phase-guard-v1`: unrealizable excess-phase corrections are neutralized and
  the designed filter must pass a group-delay gate.
- `live-closed-loop-validation-v4`: post-trial P0 magnitude-response and
  predicted-versus-verified admission gate.
- `verified-trial-native48-response-binding-v1`: final native 48 kHz response binding
  to the exact physically remeasured trial.
- `validation-signal-and-response-peak-v3`: four-times-oversampled registered-sweep
  input/output peak ratio plus FIR L1 worst-case sample-peak bound.

Symbols are level in dB unless noted otherwise. Phase 3 ranks only supplied
measurements; it does not synthesize an unmeasured hardware setting or claim to have
changed the amplifier or subwoofer.

## Frequency response

For an impulse response `x[n]`, the native one-sided response is

```text
X[k] = FFT_N{x[n]}
L[k] = 20 log10(max(|X[k]|, 1e-15))
f[k] = k Fs / N,  k = 0..N/2
```

Input IRs shorter than `N` are zero padded. Phase 1 uses a declared synthetic
timeline; real arrival-time estimation is deferred and is never replaced by moving
the largest sample to zero.

For the Phase 4 REW replay, this FFT equation is not used to reconstruct the design
magnitude from the saved room IR. REW's stored, calibrated magnitude samples are the
authoritative frequency-response input because a plain IR FFT does not reproduce its
window and calibration semantics. The separately retained raw IR is used only for the
timeline-preserving convolution diagnostic defined below.

## Known-WAV recognition over an asynchronous wireless path

Let `r[n]` be the decoded mono reference and `x[n]` the unchanged microphone capture.
The global search takes a distinctive mid-reference probe `p[n]`, removes the mean
from both probe and each same-length capture window, and evaluates

```text
rho[l] =
  sum_n (p[n]-mean(p)) (x[l+n]-mean(x_l))
  ------------------------------------------------
  sqrt(sum_n (p[n]-mean(p))^2 sum_n (x[l+n]-mean(x_l))^2)
```

The implementation computes numerators by bounded overlap/block FFT while updating
capture-window mean and energy directly. It searches only lags that can contain almost
the complete reference under the configured maximum rate mismatch. Absolute
correlation is used for recognition so inverted acoustic polarity remains detectable
and is reported rather than silently corrected.

One correlation peak is insufficient. Seven segments distributed away from the
reference edges are matched within bounded local windows. Accepted pairs
`(r_j, c_j)` fit

```text
c_j = a + b r_j + epsilon_j
drift_ppm = (b - 1) * 10^6
fit_rms_samples = sqrt(mean_j(epsilon_j^2))
```

`a` is the capture-relative playback/acoustic offset and `b` is an effective
capture/reference sample ratio. The inferred file end is `a + b N_reference`; both
start and end must remain in the original capture. A second well-separated complete
candidate close to the first peak returns ambiguous. The algorithm never shifts an IR
maximum to zero.

For a single log sweep, `b` is explicitly tagged
`intra_sweep_segment_fit`: segment frequency changes and room group delay can bias it.
It is suitable for gross mismatch rejection and a future resampling candidate, but is
not proof of inter-channel clock or arrival timing. Only repeated equivalent markers
or repeated identical excitations may qualify the stronger timing workflow.

For an uploaded stereo WAV and marker interval `I`, channel energies are

```text
E_L = sum_(n in I) L[n]^2
E_R = sum_(n in I) R[n]^2
D_marker = 10 log10(max(E_L,E_R) / max(min(E_L,E_R), 1e-30))
```

Normalized L/R correlation at least 0.999 with level difference no greater than 1 dB
is classified as identical stereo; otherwise the larger-energy side is reported as
the marker playback channel and its waveform becomes the recognition template. Start
and end are classified independently because the measurement sweep can occupy a
different channel.

The live endpoint monitor searches the first known marker in bounded overlapping
capture snapshots. Once found, its source coordinate predicts the measurement interval
and the monitor accumulates peak and RMS only over that interval. It then searches the
last marker near its predicted coordinate. Short-marker internal slope may be biased by
room group delay, so candidate generation allows 25,000 ppm internally and retains a
second peak down to half the strongest; neither value is accepted as clock evidence.
Only the chronological start/end pair spacing is checked at 5,000 ppm. Recognition of
the complete last marker ends capture on that monitor poll without another fixed tail.
The imported WAV must already place the end marker far enough after the main sweep for
the 32,768-sample deconvolution tail; final deconvolution remains fail-closed.
Cancellation remains a different, rejected terminal state.

For calibration Sens Factor `S`, the developer-beta display estimates

```text
L_SPL_est = 94 dB + 24 dB - S + L_RMS,dBFS
```

The 24 dB term converts the known UMIK maximum-input-volume convention to the stated
operating-system input-gain assumption of 0 dB. This estimate is informational:
main-sweep peak must be between -30 and -6 dBFS and existing clip/SNR gates must pass.
A physical SPL calibrator or independently verified host-gain mapping is required
before treating the result as certified absolute SPL.

## Recognized-sweep measurement extraction

The generic measurement function retains the original capture segment and uses the
recognized start `a` and supplied slope `b` to sample it on the reference grid. The
live adapter distinguishes two evidence types first. With recognized before/after
timing markers at source coordinates `s_0,s_1` and capture coordinates `c_0,c_1`,

```text
b_marker = (c_1 - c_0) / (s_1 - s_0)
a_marker = c_0 - b_marker s_0
```

is used for deconvolution clock mapping. If both equivalent markers are unavailable,
the adapter retains the recognizer's room-biased intra-sweep slope as diagnostics but
passes `b=1`; it does not warp the magnitude response using non-qualifying evidence.

For a requested `M`-sample IR, the aligned capture contains
`N_reference + M - 1` samples. Marker-referenced live captures use fixed
`P=4800` pre-zero samples at 48 kHz, so retained IR index `n` has declared time

```text
t_IR[n] = (n - P) / Fs
```

and the aligned capture begins `P/Fs = 100 ms` before the marker-mapped main
sweep boundary. Fallback full-sweep recognition uses `P=0`. This is a fixed coordinate
choice, not peak alignment: the maximum IR sample is never searched for or moved.
It allows a measured left-main or sub path to arrive before the fixed right-marker
speaker without being truncated at sample zero.
On the next power-of-two FFT grid,

```text
lambda = max_k |R[k]|^2 10^(regularization_db / 10)
H_raw[k] = Y[k] conj(R[k]) / (|R[k]|^2 + lambda)
```

The default regularization is -100 dB. If the imported calibration covers the complete
20 Hz-20 kHz required band, the microphone deviation `C(f)` it records is
log-frequency-interpolated and divided out of the measurement:

```text
H_cal[k] = H_raw[k] 10^(-C(f_k) / 20)
```

The file states how the microphone itself deviates from flat, so compensation
subtracts it; v5 added it instead, doubling the microphone error in every
calibrated result.

The optional phase column is retained but not applied. Sensitivity does not alter the
IR/FR; it is used only for the explicitly assumed SPL display above. The raw and
magnitude-calibrated IRs are both retained. The existing
frequency-response analyzer evaluates the calibrated IR, and the existing FFT
convolver reconstructs the capture from the raw IR. The resulting fit is not a
coherence estimate: late room decay, loudspeaker harmonic distortion, and a finite IR
window can lower it even when REW-style sweep recovery is usable. A complete repeated
marker pair therefore makes fit diagnostic-only because marker spacing independently
establishes extent and clock ratio; calibration, clipping, SNR, level, and stream
integrity remain hard gates. The markerless fallback still requires 12 dB fit because
it lacks that independent evidence. Other default live thresholds are 20 dB SNR, 0.45
full-sweep correlation or 0.20 per repeated endpoint marker, 2,000 ppm maximum rate
mismatch, and 96 samples maximum
magnitude-only segment fit (the generic core default is 24 samples). A single
asynchronous playback always reports
`absolute_timing_eligible = false`.

The calibration parser accepts
`frequency_hz correction_db [phase_degrees]` with whitespace/comma separators and
`#`/`;` comments. Frequencies must be strictly increasing. A typical `Sens Factor` or
`Sensitivity` header and serial number are retained. V1 requires 20 Hz-20 kHz coverage
and interpolates correction in dB on log frequency; it deliberately does not apply the
optional phase column, and Sens Factor does not modify the IR/FR.

## Multi-position statistics

P0 has weight 2 and P1-P5 weight 1. For linear energy `E_i = 10^(L_i/10)`,

```text
L_energy(f) = 10 log10( sum_i w_i E_i(f) / sum_i w_i )
```

The implementation evaluates this with normalized weights and a log-sum-exp form,
so even very large finite weights cannot overflow their sum. It also retains weighted
standard deviation in dB and weighted quantiles. It does not substitute arithmetic dB
averaging for the energy mean.

## Targets

All curves use linear interpolation in dB on `ln(f)` between knots and hold the
nearest endpoint outside the knot range.

The app's target selector offers exactly two choices: the built-in default and a
user-imported TXT. (The older `-style` B&K/Harman presets remain in `dsp-core`
solely as the basis of the fixture/pipeline regression path; the app no longer
offers them.)

### Built-in default: Harman-6dB (Dirac) — `dirac-harman-6db-v1-log-f-linear-db`

Below 500 Hz the default is, knot for knot, the Harman +6 dB bass shelf that
Dirac publishes on its official target-curve resource page
(<https://www.dirac.com/resources/target-curve>; the page offers +4/+6/+8 dB
variants and states the Harman curves deliberately carry no HF rolloff of their
own, because that part should be set per room and speaker). A test pins the
preset's sub-500 Hz knots to the repository copy
`assets/targets/Harman-6dB_REW.txt` so the attribution cannot silently drift.
The shelf is ~+6 dB at 20 Hz decaying to 0 dB at the 500 Hz anchor.

From 500 Hz upward the static form is a single preferred in-room slope line,
`t(f) = -1.0 · log2(f / 500)` dB out to 24 kHz. The -1 dB/octave constant is the
gently declining steady-state in-room response preferred in the Toole/Olive
listening research — a literature constant, never a value derived from any
particular system measured with this app.

### Measurement-adaptive HF rolloff — `harman-6db-adaptive-hf-v1`

A fixed -1 dB/oct line to 20 kHz is right for a speaker with extended treble,
but for a speaker that genuinely rolls off it demands boost the driver cannot
deliver, costing headroom against the cut-only design policy. When (and only
when) the built-in default target is selected, the design therefore adapts the
HF section to the measurement; a custom TXT is always used verbatim and the
SECS flat target is unaffected.

Input: the weighted spatial energy average the correction stage consumes
(all accepted positions, both channels, P0 at weight 2). Then:

1. Smooth at 1/3 octave (Gaussian on the log-f axis, the existing `smoothing`
   module).
2. Anchor: subtract the mean smoothed level over 400-630 Hz so
   `measured(500 Hz) ≈ 0 dB` (a single bin is too noisy).
3. Preferred line: `pref(f) = -1.0 · log2(f / 500)`.
4. Measured trend: least-squares fit `meas(f) = a + b · log2(f / 500)` over the
   third-octave centres in 2-20 kHz (≥ 6 points; 2 kHz keeps the fit clear of
   the anchor region and midrange modes, so only the broad treble trend enters —
   local peaks/dips deliberately stay ordinary correction targets).
5. Target above 500 Hz: `t(f) = max(min(pref(f), meas(f)), -12 dB)` on the
   third-octave grid, then enforced monotone non-increasing. The -12 dB droop
   clamp equals the corrector's own `MAXIMUM_SUPPORTED_ATTENUATION_DB`: a trend
   deeper than anything the corrector could implement indicates a broken
   measurement.

A well-extended speaker (`meas ≥ pref`) receives the full preferred slope; a
rolled-off speaker receives a target that bends at the measured break frequency
onto its own decline, so no futile top-octave boost is demanded. Every
speaker-specific number (break frequency, droop) emerges from the run-time fit.

Guards — no measured coverage to ≥ 10 kHz or past the 400 Hz anchor band, too
few fit points, non-finite values, an ill-conditioned fit — fall back to the
static curve, and `b ≥ 0` (rising treble) keeps the preferred line. Every
outcome (fitted slope in dB/oct, break frequency or its absence, or the named
fallback reason) is recorded in the design summary, shown in the UI, and
written into the trial README. This provenance is derived from the baseline
measurement: it is prediction context, never verification evidence.

In the bounded Phase 4 design only 20-500 Hz is corrected, so there the
adaptive HF section shapes display/diagnostics; in the full-band SECS path
(overlay re-anchored at 500 Hz) it directly shapes the correction.

A user target accepts exactly two numeric columns (`frequency_hz level_db`) separated
by spaces, tabs, or commas. Blank lines and `#`/`;` comments are ignored. Frequencies
must be strictly increasing and finite within `(0, 1,000,000]`; level is finite within
`[-200, 200]`. Errors carry the source line and a valid example.

The live wizard passes the parsed `TargetCurve` directly into the same Phase 4
configuration used by the presets. It stores the original TXT, SHA-256,
`target-txt-parser-v1`, point/range metadata, selected target name/version, and the
200-500 Hz alignment band in the live/final project evidence. Values outside the
provided knot range use the nearest endpoint and the UI warns when the file does not
cover the full 20-500 Hz correction band.

The target is vertically aligned using the median of `L_energy(f)-T(f)` over the
trusted 200-500 Hz reference band. The recorded offset is added to every target bin.

## Robust bounded-gain objective

At each frequency define residual per position and weighted lower quantile:

```text
r_i(f) = L_i(f) - T_aligned(f)
q(f)   = weighted_quantile_0.25({r_i}, {w_i})
e(f)   = L_energy(f) - T_aligned(f)
```

A candidate cut exists only when `q(f) >= 0.75 dB`, meaning the excess survives a
lower-envelope spatial test. Before smoothing, requested attenuation is

```text
a_raw(f) = -clamp(max(0, 0.7 q(f) + 0.3 e(f)), 0, 12 dB)
```

The cut side retains a 12 dB attenuation bound and a lower-quantile spatial
regularizer. A one-seat peak cannot pass the lower-quantile gate.

Positive correction is a separate, stricter path. Let the per-position residual be
smoothed with a one-third-octave Gaussian and take its weighted upper quartile:

```text
b_i(f) = smooth_1/3oct(r_i(f))
u(f)   = weighted_quantile_0.75({b_i}, {w_i})
```

A boost request exists only when at least two positions are retained, `u(f) <= -1 dB`,
and the condition remains contiguous for at least `0.25 octave`. The upper quartile
means that approximately three quarters of the weighted positions must still be below
target; one low seat cannot authorize boost. Before the final smoothing/window:

```text
b_raw(f) = clamp(-u(f), 0, 3 dB)
```

Any bin where a retained raw position is at least 8 dB below target is protected and
cannot enter the boost path. Cut takes priority when the robust peak gate is active.
The +3 dB ceiling is a product constant, not a user-relaxable target.

`a_raw` is Gaussian-smoothed on log frequency with 1/12-octave FWHM. A bin is a
protected dip when any retained position is at least 8 dB below the aligned target.
Attenuation smoothly returns to zero within 0.10 octave of each protected dip. The
correction-band window is unity from 20 through 500 Hz and a raised-cosine return to
zero from 500 to 650 Hz; it is zero below 20 and at/above 650 Hz.

That unity statement describes the designed correction shape. FIR realization may add
one reported, frequency-independent attenuation-only safety normalization to keep
inter-bin gain below unity; the current synthetic example applies about -0.3035 dB.
This uniform safety trim is not high-frequency tonal correction.

Every requested design and stereo-blend stage reasserts

```text
-12 dB <= a(f) <= +3 dB
```

and rejects NaN/Inf. A configured maximum above the 12 dB product ceiling is rejected.
If the unconstrained request exceeds the chosen limit, an attenuation-limit warning is
recorded. Phase 1 treats that warning as blocking. Phase 4 may resolve it only through
the separately reported conservative redesign below; the original request and warning
remain in the project record. A request above +3 dB is clipped to the hard ceiling and
recorded as `boost-limit-reached`; it never relaxes protected-dip or spatial-width gates.

## Shared low-bass stereo rule

Let `a_L` and `a_R` be independently designed bounded requests. Below 100 Hz the
common request is conservative for both signs:

```text
a_common(f) = max(a_L, a_R), if both are cuts
              min(a_L, a_R), if both are boosts
              0,             if their signs conflict
```

Between configurable boundaries `f0=100 Hz` and `f1=140 Hz`,

```text
u = clamp( ln(f/f0) / ln(f1/f0), 0, 1 )
s = u^2 (3 - 2u)
a'_c = a_common + s (a_c - a_common), c in {L,R}
```

This has zero slope at both boundaries. Above 140 Hz each channel uses its independent
request. The boundaries are settings, not system-specific constants.

## Manual single-sub candidate ranking

The implemented Phase 3 slice compares combined L+sub and R+sub responses captured
at the same declared positions for each candidate hardware setting. All candidates
must have identical position IDs, positive position weights, channels, and exact
frequency grids. Let `theta` identify a measured candidate and `fc_theta` its declared
crossover. The common comparison band is

```text
B = [0.5 min_theta(fc_theta), 2 max_theta(fc_theta)] intersect measured grid
```

For the current measured 70/80/90 Hz set this is 35-180 Hz (396 bins). A common band
prevents each crossover from receiving a different scoring range. The captured
magnitude `L_theta,p,c(f)` is Gaussian-smoothed on a log2-frequency axis with default
1/3-octave FWHM:

```text
S_theta,p,c(f) = smooth_1/3_oct(L_theta,p,c(f))
E_p,c(f)       = max_theta S_theta,p,c(f)
d_theta,p,c(f) = max(0, E_p,c(f) - S_theta,p,c(f))
```

Thus the magnitude term measures a candidate's deficit from the best response that
was actually measured at each position, channel, and frequency. It is a relative
candidate-comparison objective, not a target-curve RMSE and not permission to boost a
dip. Frequency aggregation uses trapezoid-like quadrature weights on `ln(f)` so the
dense linear-frequency source grid does not over-weight its high-frequency end:

```text
w_f[0]   = (ln f[1] - ln f[0]) / 2
w_f[i]   = (ln f[i+1] - ln f[i-1]) / 2
w_f[end] = (ln f[end] - ln f[end-1]) / 2
```

Across the retained positions, both channels, and frequencies, the scorer records
weighted deficit RMS `D_rms`, weighted 95th percentile `D_95`, and maximum `D_max`.
Position weights are applied to the aggregate; P0=2 and surrounding positions=1 are
the product defaults. For more than one position it also records the worst per-seat
deficit RMS `D_seat` and weighted standard deviation of per-seat RMS `D_spread`.

When every response contains phase, phase unwrap and finite-difference group delay are
computed on the preserved frequency data. Group delay is smoothed with default
1/2-octave FWHM, its weighted median is removed, and the remaining weighted RMS is
`G_phase` in ms. When every response has same-setting repeated timing evidence, the
weighted RMS of its repeatability spread is `T_repeat` in ms. If either evidence class
is incomplete, its term is omitted for all candidates and a warning is recorded; a
missing value is never replaced with zero-confidence evidence.

With the default `RankingConfig`, lower score is better:

```text
score(theta) = D_rms + 0.20 D_95 + 0.05 D_max
             + [0.25 D_seat + 0.15 D_spread, only with multiple positions]
             + [0.10 G_phase, only with complete phase evidence]
             + [0.10 T_repeat, only with complete repeat timing evidence]
             + 0.02 |main_delay_ms| + 0.02 |sub_level_db|
```

The bracketed coefficients and smoothing widths are versioned configuration defaults,
not universal acoustical constants. Polarity itself has no regularization cost. An
unknown delay, level, polarity, phase trace, or timing repeat is preserved as unknown;
the scorer does not infer it. Ties are broken deterministically by candidate ID, and
every report sets `needs_confirmation=true`.

Candidate level is deliberately not aligned. For diagnostics only, each candidate's
weighted median level over 200-500 Hz is computed across positions and channels; the
maximum-minus-minimum candidate spread triggers a warning above the default 1.5 dB.
This anchor check never changes response values or the ranking inputs.

### Candidate-set invariance (v5)

A candidate's evaluation must not depend on which OTHER crossovers the user happened
to list. v4 violated this in three places, and the field data that exposed it
(2026-08-09, identical wide-band captures) flipped the winner from 70 Hz / 0.38 ms to
80 Hz / 11.35 ms merely because the candidate list started at 50 Hz instead of 40 Hz:

1. **Arrival anchor.** v4 estimated the shared sub-minus-main arrival from the
   lowest LISTED crossover's filtered path, over a band up to twice that crossover.
   Without the 40 Hz candidate the band widened to 20-100 Hz, a correlation alias one
   bass cycle away (+11.15 ms vs the true -1.2 ms; spacing 12.4 ms = one period at
   ~81 Hz) won the peak pick, and every candidate's delay window - including 70's own -
   shifted a cycle. v5 anchors the arrival on the RAW wide-band captures as the MEDIAN
   over a fixed band family (20-45 / 20-60 / 20-80 Hz, both channels) with
   edge-degenerate members rejected: no single band is trustworthy in every room
   (measured, the same captures read -20 ms pinned at 20-45 Hz on the left - the
   room's low band is phase-incoherent there - and +20 ms pinned at 30-90 Hz on the
   right), while the family median reads -0.4 ms with the channels agreeing to under
   1 ms. Every family member's top edge stays at or below 80 Hz, keeping each
   member's cycle ambiguity out of the median's reach, and the anchor cannot see the
   candidate list. Measured mode has no
   unfiltered capture, keeps the lowest-path fallback, and the report now warns when
   the lowest crossover is 50 Hz or higher (ambiguity fits the scan there).
2. **Scoring band.** v4 scored over 0.5x the minimum to 2x the maximum LISTED
   crossover, so the same candidate was scored over 20-240 Hz in one run and
   25-240 Hz in another. v5 fixes the band at the product's validated 20-500 Hz;
   above twice any offered crossover both branches are main-dominated and identical
   across candidates, so the widening adds the same contribution to every candidate
   and preserves order while removing the list dependence.
3. **Coarse-grid granularity.** v4 chose one stage-one multiplier from the widest
   window in the SET; v5 chooses it per window, so a candidate's grid depends on its
   own window alone.

Pinned by `the_arrival_anchor_and_winner_are_candidate_set_invariant_in_wide_mode`
(same captures, lists with and without 40 Hz: identical anchor and identical
per-crossover best settings) and by the fallback-ambiguity warning test.

### Live separated-path delay and polarity search

`phase3-separated-path-delay-polarity-search-v5` scores with its own level-neutral
splice objective (below), not the measured-candidate envelope objective above.
Its input is not one generic main/sub measurement plus an assumed crossover model.
For every declared crossover `x`, the user physically configures the hardware and
measures complex P0 responses `M_x,L(f)`, `M_x,R(f)`, and mono `U_x(f)`. Therefore the
amplifier's actual high-pass/low-pass behavior is already part of each measured state.
At least two and at most twelve unique ascending crossover states are required.

Let `tau_0` and `p_0` be the main delay and 0/180-degree polarity active during those
captures. For a candidate main delay `tau` and polarity `p`, the synthesized combined
path for channel `c` is

```text
M'_x,c(f; tau) = M_x,c(f) exp(-j 2 pi f (tau - tau_0) / 1000)
U'_x(f; p)     = U_x(f) exp(j pi I[p != p_0])
C_x,c(f)       = M'_x,c(f; tau) + U'_x(f; p)
```

The bounded delay range must lie within -20 to 50 ms with a 0.01–5 ms step. The grid
is not the full range: each crossover searches one half-period window centred on the
shared sub-arrival anchor, clipped to the range and snapped to absolute step multiples.
Each window holds at most 1,001 values. When the requested step is too fine for that
cap (a 0.01 ms step across a 40 Hz candidate's 25 ms window would need 2,501 values),
the search runs in two stages instead of refusing: stage one scans every window at the
smallest integer multiple of the step that fits its own window's cap (per window, v5),
and stage two re-ranks at the full requested resolution within one coarse step either
side of each crossover's stage-one optimum. Both stages
contain only hardware-step multiples, each fine window contains its own coarse
winner, and the score's variation scale over delay (about one period of the highest
scored frequency, >= ~2 ms) is hundreds of times any coarse step chosen here, so the
coarse scan cannot skip the optimum's basin. A configuration the caps admit directly
produces the identical single-stage v2 grid. Synthesis retains
positive bins from `max(20 Hz, 0.4 min(x))` through 500 Hz and the scorer judges the
common `[0.5 min(x), 2 max(x)]` band.

One arrival anchors every delay window, estimated by band-limited complex
cross-correlation on the lowest-crossover path over 20 Hz to twice its crossover.
The true sub-minus-main offset is a property of the drivers and placement, not the
crossover dial, and only the lowest path's correlation is alias-free inside the
+/-20 ms scan: a band centred on `f` repeats every ~`1/f`, so a high-crossover
path's own estimate can lock a full bass cycle away (observed as 12-15 ms
recommendations on real data whose true offset was about -1 ms). A plan whose
lowest candidate sits above 55 Hz carries a warning that the anchor itself is
ambiguous.

Before the window search, each path's sub is level-matched into the mains
(new in v4): the smoothed weighted-median of `|U_x|` over the
crossover-adjacent octave `[x/2, x]` is shifted onto the mains' own raw
200-500 Hz anchor (mean of both channels), and this per-candidate trim `g_x` -
the calibration a user performs after choosing that crossover - is applied to
`U_x` before arrival estimation, synthesis, and scoring, clamped to +/-24 dB
with a warning. The trim is part of the recommendation (reported with every
ranked entry and echoed by the sub-level advisory), never a free search
dimension, and it is what makes the objective below level-neutral end to end:
a pure gain change on the captured sub cancels out of `g_x` exactly, so the
ranking is invariant to the captured sub level. Scoring at capture level
instead let a sub captured ~10 dB hotter than the mains clear the anchor
everywhere it played - its own dips hidden behind the surplus while the mains'
room holes between the crossover and the midband were charged in full - so the
anchor term silently rewarded handing the widest band to the loudest branch
and the recommendation tracked the top of whatever candidate list was offered
(110 of 40-120, 100 of 40-100, 90 of 40-90 on the same real session). A path
that cannot be matched (no 200-500 Hz mains coverage, or too few bins in
`[x/2, x]`) keeps its captured level with a warning, matching the scorer's own
anchor fallback.

Because every candidate derives from the same captures, the measured-candidate
envelope objective is replaced by a per-candidate splice objective on
gate-smoothed curves:

```text
deficit_x,c(f) = max(0, max(smooth(max(|M'_x,c|, |U'_x|))(f), A_x,c) - smooth(|C_x,c|)(f))
```

The first reference term is the pointwise-louder branch, smoothed after the max: an
aligned sum is never below the louder branch bin by bin, so this term charges
destructive interference exactly where the branches overlap and cannot be masked by
turning either branch up (the v2 cross-candidate envelope let a sub running hotter
than the mains hand the win to whichever candidate gave it the widest band - the
highest crossover in every list, confirmed on real data). The second term `A` is the
candidate's own raw 200-500 Hz median: a splice whose sum falls below the midband is
a hole the cut-only EQ cannot legally fill. The anchor comparison is meaningful
because the sub enters it at deployment level (the `g_x` trim above). Excess above
the anchor is deliberately uncharged (cuts remove it and headroom is charged at
export). Group-delay smoothness is computed and reported but not
scored: an LR crossover's own group-delay bump scales as `1/fc`, so a
millisecond-unit smoothness term structurally favors high crossovers and can even
prefer an accidentally-smoother misaligned delay. The score is the weighted deficit
RMS + 0.20 p95 + 0.05 worst, plus `0.02 dB/ms * |tau - tau_arrival|` (regularizing
toward the measured arrival, which is the physically privileged delay - never toward
zero) and `0.1 dB/octave * log2(x / 40 Hz)` (a documented localization tie-break far
below any real splice defect, so it only decides between equivalent splices).

Beyond the reported deployment trim `g_x`, no candidate receives any level
alignment, and the trim itself is never varied to improve a score. The one
sub-only response is reused for both L and R, so symmetric bass-management routing is
an explicit limitation. All captures must use the same uploaded sweep hashes,
calibration, microphone device/channel, gain state, P0 position, and fixed acoustic
marker speaker. This fixed marker authorizes relative complex-path comparison only;
it does not change the global `timing_eligible=false` state and cannot authorize L/R
speaker delay correction.

The report always sets `needs_confirmation=true`. Its first-ranked setting is a
complex-sum prediction, not measured L+sub/R+sub evidence. The user must apply it to
the hardware and take the next combined P0 captures before the correction workflow
continues.

### Wide-band single-capture crossover synthesis

`phase3-wide-band-crossover-synthesis-v1` feeds the same optimizer from three captures
instead of three per crossover. The sub is measured once through the bass-management
low-pass dialed to its maximum `f_meas` (or genuinely bypassed), and both mains are
measured once with the sub output off, which removes the speaker high-pass. For each
candidate crossover `x` a virtual `SeparatedCrossoverPaths` state is synthesized:

```text
M'_x,c(f) = M_c(f) * HP_model(x, f)
U'_x(f)   = U_meas(f) * LP_model(x, f) / LP_model(f_meas, f)    (dial at f_meas)
U'_x(f)   = U_meas(f) * LP_model(x, f)                          (declared bypass)
```

The division *replaces* the measurement-state filter instead of stacking a second
one: an LR4 low-pass at 250 Hz carries about 1.8 ms of low-frequency group delay that
is absent at deployment, and leaving it in would bias the delay recommendation by
about that much (52 degrees at an 80 Hz crossover). With the same alignment and
`x <= f_meas` the replacement ratio `|LP(x)/LP(f_meas)| <= 1` at every frequency, so
measurement noise is never amplified; candidates above the measured dial are rejected.
The available alignment models are LR4 (squared 2nd-order Butterworth, branches in
phase at the crossover), LR2 (two cascaded first-order sections, branches antiphase at
the crossover), and 2nd-order Butterworth, selectable per branch because THX-style
bass management pairs a 12 dB/oct satellite high-pass with a 24 dB/oct sub low-pass.

This mode explicitly trades the measured-states mode's model-freedom for a
three-sweep session and a dialable candidate list of any size up to twelve. The room,
drivers, and placement remain measured complex data on the shared marker timeline;
only the bass-management filters are modeled. The model dependence is honest and
consequential: if the hardware's true slopes differ from the declared ones -
especially 12 versus 24 dB/oct, which flips the relative branch phase at the
crossover - the delay and polarity recommendation can be wrong, and the report
carries that warning instead of the measured-mode "no crossover transfer function was
synthesized" line. Marker-timeline algebra keeps cross-state combination exact: each
capture's markers absorb that state's processing latency, so the sub-minus-main
arrival assembled from the two hardware states equals the deployed one. The
cross-capture marker-level gate widens from 0.3 to 0.5 dB because the wide dial's
high-pass attenuates the reference speaker's >=650 Hz marker band by a bounded
~0.2 dB on the sub capture only. The recommendation stays predicted-only; the
mandatory combined L+sub/R+sub captures that follow are the first measurement of the
hardware's real filters at the chosen crossover.

### Separated main/sub consistency diagnostic

Where main-only, sub-only, and their actual combined response exist with phase, the
complex prediction is

```text
M(f) = 10^(L_main(f)/20) exp(j phi_main(f))
U(f) = 10^(L_sub(f)/20)  exp(j phi_sub(f))
L_sum_pred(f) = 20 log10(max(|M(f) + U(f)|, 1e-15))
```

The diagnostic reports log-frequency-weighted RMSE between `L_sum_pred` and the
measured combined magnitude over `[0.5 fc, 2 fc]`. It separately records cancellation
loss `max(0, max(L_main,L_sub)-L_combined)`. The current CLI requires complex-sum RMSE
no greater than 1.0 dB before publishing its measured ranking. These diagnostics are
not added to the candidate score. Adding 180 degrees to the stored sub phase is only a
counterfactual complex-sum check; without an actually measured inverted combined run,
its cancellation-loss metrics are not evidence.

## Minimum-phase FIR

The one-sided cut request is mirrored into a real even log-magnitude spectrum. An
inverse FFT forms the real cepstrum. The zero-time and Nyquist terms are retained,
positive quefrencies are doubled, and negative quefrencies are zeroed. FFT followed
by complex exponentiation and inverse FFT yields a causal minimum-phase FIR.

The full implied FFT-length impulse is retained in Phase 1, avoiding an unvalidated
truncation window. Its response is checked on a 16x denser grid. An all-cut design
retains the `0.999999`-amplitude unity ceiling. A design containing boost instead uses
the same ratio immediately below +3 dB. If interpolation crosses the applicable
ceiling, the complete filter is attenuated; it is never lifted. The normalization dB
is reported per channel in `project.json`. Native-grid tests verify the realized
magnitude equals the request plus that reported normalization.

## Phase 4 measured-response replay

The Phase 4 input is a measured 48 kHz magnitude grid and at least one L/R combined
position. In the current fixture it is central P0 with weight 2. The response values
retain their absolute SPL and REW frequency origin; the path performs no candidate
level alignment and no timeline shift. The target is aligned independently per channel
with the existing 200-500 Hz robust reference rule, then the bounded-gain objective and
100-140 Hz common-to-channel-specific stereo rule above are applied.

The magnitude design uses the measured bins through 650 Hz. Requested correction is
active from 20 through 500 Hz and follows the raised-cosine return to 0 dB from 500 to
650 Hz. Resampling onto the native FIR grid explicitly writes 0 dB below 20 Hz and at
or above 650 Hz. The default FIR has `N=16384` taps at 48 kHz and therefore 8,193
one-sided design bins. FIR realization is inspected on a `16N` response FFT before its
response is interpolated back onto the measured grid.

### Conservative attenuation-limit redesign

The first design run is preserved, including an `attenuation-limit-reached` warning
when its unconstrained request `A_req` exceeds the configured 12 dB limit. Phase 4 then
uses the versioned 11.5 dB safe target and applies one uniform strength to the
smoothed and unsmoothed requests:

```text
s = min(1, 11.5 dB / A_req)
a_safe(f) = s a_initial(f)
```

This redesign only scales an already-admitted correction toward unity; it cannot turn
a protected dip into boost or alter the target alignment. It is resolved only when
every resulting request remains finite, within +3 dB, and no more attenuated than
11.5 dB. The legacy measured fixture recorded initial
requests of 16.9146/16.9279 dB for L/R, strengths 0.679885/0.679353, and smoothed
redesigned maxima 8.1163/8.1065 dB. The original 12 dB warnings stay visible even
though the typed redesign gate is resolved.

### Prediction and IR diagnostic

Let `H_c,real(f)` be the dense-FFT realized FIR magnitude for channel `c`. The response
replay is purely a frequency-domain prediction:

```text
L_c,pred(f) = L_c,measured(f) + H_c,real(f)
```

It is checked against the aligned target on 20-500 Hz. For a retained raw combined IR
`x_c[n]`, the separate diagnostic computes finite linear convolution

```text
y_c[n] = x_c[n] * h_c[n]
t_start,y = t_start,x
```

because the causal FIR begins at relative time zero. The original REW `startTime` is
therefore preserved; neither input nor result is peak-aligned. `max(abs(y_c))` is a
room-IR diagnostic only. It is not the true peak of a FIR-filtered playback signal and
cannot determine clipping or recommended headroom.

### Offline numerical gates and state

In addition to the general finite/bounded-gain/realized-response gates, Phase 4 requires:

- all four L/R x sub-A/B separated-path predictions to have 45-180 Hz complex-sum
  magnitude RMSE no greater than 1.0 dB before design;
- realized attenuation at every protected-dip bin to be no greater than 0.5 dB;
- realized boost at every protected-dip bin to be no greater than 0.05 dB;
- target RMSE to improve, broad-peak RMSE to improve by at least 10%, and worst broad
  peak error to improve by at least 5% in each channel;
- every required safe redesign to resolve without NaN/Inf, gain above +3 dB, or
  excessive attenuation.

Passing these gates sets only `numerical_prediction_passed=true`. The type-level state
remains `predicted-only-measured`; hardware verification, closed-loop pass, final
export eligibility, and recommended headroom cannot be inferred.

### Live adapter, closed-loop comparison, and headroom

The live developer beta reuses `run_phase4_offline` rather than reimplementing the
objective above. Accepted L/R capture pairs on the same frequency grid become measured
positions. P0 has weight 2 by default; if P0_END is present, P0 and P0_END each have
weight 1. P1-P8 each have weight 1. A trial requires at least an accepted P0 pair.
When both central pairs exist, let `m_c,p` be the median level over 200-500 Hz and
remove it from each channel response. Trial design additionally requires

```text
|m_c,P0_END - m_c,P0| <= 1.0 dB
RMSE_20-500((L_c,P0_END - m_c,P0_END) - (L_c,P0 - m_c,P0)) <= 6.0 dB
```

for both channels. The absolute level bound remains the session/gain stability check.
The shape bound is intentionally loose because P0_END is returned by hand after P1-P8;
it rejects only a gross path/location change and does not demand exact modal
reproduction. This checks repeat level and coarse shape, not arrival-time repeatability.
The live Phase 4 trial uses the existing native 48 kHz duration, 16,320 samples; the
offline Phase 4 default remains 16,384.

The trial WAV/ZIP is still predicted-only. After the user manually enables that exact
filter in Roon and records a backend-timestamped user attestation, new accepted
verification P0 L/R captures are compared against their baseline P0 responses with the
existing `validate_frequency_prediction` gates. The attestation does not inspect or
control Roon. Both channels must pass and no validation issue may remain. The v2
agreement score first forms the signed residual and applies the same
1/12-octave Gaussian log-frequency smoothing used for broad-peak scoring:

```text
r_c(f) = smooth_1/12oct(L_verified,c(f) - L_pred,c(f))
E_pv,c = sqrt(mean_20-650Hz(r_c(f)^2)) <= 3.0 dB
```

The unsmoothed RMSE is retained as a diagnostic. No fitted broadband level offset is
removed, and the independent target-improvement gates above remain mandatory. This is
`live-closed-loop-validation-v4`. It verifies minimum-phase magnitude correction, not
L/R arrival or a Roon transport state that the app controls.

After that gate, the existing Phase 6 engine realizes the same physical gain intent at
all six native rates. Before export, `verified-trial-native48-response-binding-v1`
compares the exact trial associated with the post-attestation verification evidence
`h_trial,c` with the final native 48 kHz member `h_final,c`:

```text
max_20-20000 |M_trial,c(f) - M_final,c(f)| <= 0.05 dB
max_20-650 |GDrel_trial,c(f) - GDrel_final,c(f)| <= 0.02 ms
```

For the registered left/right measurement references `r_c` and native FIR `h_c`,
`validation-signal-and-response-peak-v3` calculates

```text
P_in,c  = true_peak_4x(r_c)
P_out,c = true_peak_4x(r_c * h_c)
G_peak  = max_c 20 log10(P_out,c / P_in,c)
G_fir   = max_c 20 log10(sum_n |h_c[n]|)
headroom_db = ceil_0.1(max(0, G_peak, G_fir) + 1.0 dB)
```

The true-peak estimate uses FFT zero-padding at four-times temporal resolution. The
FIR L1 norm bounds the output sample peak for any input whose sample peak is at most
one, although it can be much more conservative than typical music. The result is a
starting attenuation, not proof of the analog playback chain. The strict exporter
writes and reads back the six-rate ZIP before the final project stores its path and
SHA-256. The serialized field `measuredTruePeakRatioDb` is retained for compatibility;
it contains the registered digital sweep ratio, not an acoustically measured true peak.

## Predicted validation metrics

For the Phase 1 synthetic path, every position and channel is convolved with its raw
synthetic IR and analyzed on the same grid. For Phase 4, prediction instead adds the
realized FIR magnitude to the authoritative stored REW magnitude as specified above.
Both paths compute raw and predicted 20-500 Hz target RMSE. Modal-peak scoring first
smooths the residual with a 1/12-octave Gaussian, then evaluates bins whose raw
positive error is at least 1 dB; both their RMSE and worst error must improve. Invalid
thresholds, non-finite intermediate values, positive realized gain, and excessive
realized attenuation fail closed.

Channel order is structurally verified by the stereo WAV interleave/readback test and
manifest metadata. Acoustic channel-swap detection needs tagged real playback and is
an eventual closed-loop gate, not a claim of the Phase 4 offline predictor.

Neither a synthetic prediction nor measured-baseline response replay is a real-room
verification. The UI and status report must not label either output “correction
complete.”

## Phase 6 native sample-rate redesign

Phase 6 starts from the measured-frequency bounded L/R gain intent produced by Phase 4.
It never resamples the 48 kHz impulse. For rate `F_s`, the native FIR length is chosen
for one common rational duration:

```text
T = 17 / 50 s = 0.34 s
N(F_s) = F_s T
Delta_f = F_s / N(F_s) = 50 / 17 Hz
```

All required rates make `N` an even integer:

| Sample rate | Native samples |
| ---: | ---: |
| 44,100 Hz | 14,994 |
| 48,000 Hz | 16,320 |
| 88,200 Hz | 29,988 |
| 96,000 Hz | 32,640 |
| 176,400 Hz | 59,976 |
| 192,000 Hz | 65,280 |

The shared physical bin spacing prevents a hard 20 Hz unity-to-correction boundary
from landing at a different frequency in the 44.1 and 48 kHz families. Each rate still
has its native Nyquist extent; bins from 650 Hz upward remain unity rather than being
unnecessarily limited to the 48 kHz Nyquist range. The one-sided intent is converted
to a causal minimum-phase FIR by the same real-cepstrum spectral factorization used in
Phase 4. The FFT implementation accepts these even non-power-of-two sizes.

Dense trigonometric interpolation can produce a tiny above-unity peak between design
bins. Let `s_c,r <= 0` be the attenuation-only safety normalization found for channel
`c` and rate `r`. Phase 6 uses one common value

```text
s_common = min_(c,r) s_c,r
```

and applies `s_common - s_c,r <= 0` as additional attenuation to each FIR. This cannot
create or increase boost, preserves the +3 dB ceiling, and removes
channel/rate-dependent broadband offsets before cross-rate comparison.

For log-spaced query set `Q`, realized response at each rate is compared with the 48 kHz
reference:

```text
Delta_M,c,r = max_(f in Q_M) |M_c,r(f) - M_c,48(f)|
Delta_GD,c,r = max_(f in Q_GD) |GD_c,r(f) - GD_c,48(f)|
```

`Q_M` covers 20 Hz–20 kHz and `Q_GD` covers 20–650 Hz. Product limits are 0.05 dB and
0.02 ms respectively and callers may only tighten them. Source provenance is checked
separately: the Phase 4 CSV is first resynthesized on its original 48 kHz/16,384 grid
and must reproduce every hash-linked float32 trial tap within `1e-6`. The same admitted intent
then enters the six new native grids. Thus a new grid is not incorrectly required to
have the same sample sequence as the legacy grid.

These are numerical sample-rate-consistency gates, not hardware verification. The
offline Phase 6 command does not calculate playback true peak, recommended headroom,
or `export_eligible`. Only the separate live adapter may add its already-passed
closed-loop evidence and signal/FIR-bound headroom before calling the same native-rate
engine and exporter.

## SECS full-band correction (advanced option)

Original work by **한플 (Hanpeul)** — SECS, published on the DCInside speaker gallery
(<https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1>),
ported under the MIT License granted by the author.

This section describes **what happens to the signal, in order**, rather than the code,
and marks where the original design ends and this project's additions begin.

### Input

One left/right impulse-response pair from the central seat (P0), 48 kHz, 32,768 samples
(0.68 s). Where the Phase 4 path uses only the **magnitude** of several seats, SECS uses
the full **complex response (magnitude and phase)** of one seat. That is what lets it
correct the time domain, and it is also why it can overfit that one seat.

### Stage 1 — split the response into magnitude and time (original)

A room transfer function factors uniquely into a minimum-phase part times an all-pass part.

- **Minimum-phase part** — determined by the magnitude curve. EQ can fix this.
- **All-pass (excess-phase) part** — unity magnitude, but arrival time varies with
  frequency. EQ cannot touch it in principle; only an all-pass filter cancels it.

SECS performs the split cepstrally (inverse transform of the log magnitude). Aligning the
measured impulse on its peak and dividing by the minimum-phase reconstruction leaves the
pure excess phase.

This split is the decisive difference from ordinary room EQ. A PEQ or a minimum-phase FIR
fixes magnitude only, so it can produce the same magnitude curve while leaving the
low-frequency arrival-time scatter untouched.

### Stage 2 — the magnitude side (original)

**(a) Five-band fractional-octave smoothing.** The response is split into five LR4 bands,
each smoothed at a different resolution, then recombined. The default (Normal) is
1/24 → 1/12 → 1/6 → 1/3 → 1/1 octave: fine at the bottom, coarse at the top. The reason is
physical. Low-frequency modes are real and repeatable; the dense ripple up high is an
interference pattern that changes when the microphone moves a few centimetres, so it is
not something to chase.

**(b) Adaptive target.** No fixed curve.

- The natural low and high cutoffs the speakers and room actually produce are located, and
  outside them the target follows the natural rolloff instead of forcing level.
- The reference level is anchored at the **35th percentile** of the 100 Hz–10 kHz
  magnitude. Anchoring low rather than at the mean is the conservative choice behind the
  whole algorithm: cut peaks to reach the target rather than boost dips to reach it. The
  price is that roughly 65 % of the response sits above target and becomes a cut, so the
  overall level drops after correction.
- The bass shelf and tilt multiply this.

**(c) Magnitude inversion.** The smoothed magnitude is divided into the target. **Cuts are
unbounded in depth; only boost is capped** (default +6 dB) — the same philosophy again.

**(d) Macro EQ.** The result of applying that filter is simulated, smoothed at one octave,
compared to the target, and the remaining broadband error is corrected again within ±6 dB.
This sets the large-scale tonal balance.

**(e) Peak crush — below 500 Hz only.** Whatever still stands above the target is pressed
down once more, at strength 0.3 / 0.6 / 1.0 by resolution setting. Full weight to 300 Hz,
fading to zero by 500 Hz. This is where "a peak rings, so it is worse than a dip" is
written into the algorithm.

**(f) Curtain — default 300 Hz.** Above the curtain the narrow-band correction fades out
and only the one-octave-scale tonal correction remains, fading linearly from the curtain to
twice the curtain.

### Stage 3 — the time side (original)

Conjugating the excess-phase part gives an all-pass that cancels the room's phase
distortion. The catch is that such an all-pass **sounds before its cause** (pre-ringing),
so the budget differs by band.

| Band | Pre-ringing allowed |
| --- | --- |
| Low | up to the target delay (10 ms by default) |
| Mid | half of it (≤5 ms) |
| High | a quarter of it (≤2.5 ms) |

Long wavelengths hide pre-ringing well and phase error costs the most there; at high
frequencies it is the other way round. Each band is windowed separately, recombined with
the LR4 weights, and renormalised to unity magnitude so the result stays all-pass.

**Phase-confidence guard.** Where the response falls into a deep null, the measured phase
itself cannot be trusted. SECS fades the phase correction back toward unity there; the
threshold is roughly 15 dB below the band average of that channel's own magnitude.

**Realizability guard (`phase-guard-v1`, this project's addition — the "improved SECS"
option, on by default).** The windowing above has a failure mode the original never
checks: when the room's bass arrives *later* than the pre-ringing budget can advance
(common — a sub path's DSP latency plus modal storage was 38-57 ms on a real system,
against a 10 ms budget), the truncated-then-renormalised all-pass does not degrade into
"less correction". Its group delay flips sign: the intended advance becomes extra delay
(a real 2026-08-03 filter delayed 20-120 Hz by 70-200 ms — 7 to 16 cycles — while every
magnitude metric stayed green, because an all-pass changes no magnitude anywhere). With
the guard on, the windowed corrector is compared per frequency bin against applying no
correction: the residual is `excess x corrector` versus `excess` alone, and wherever the
corrector worsens the residual's group delay by more than 5 ms it is blended back to
unity (the verdict is smoothed over 1/3 octave and the result renormalised to unit
magnitude, so it stays a smooth all-pass). A second, band-level stage then verifies the
result per gate band with medians and removes the correction across any band (half-octave
crossfades) where it still either worsens the residual's median or spends more than 60%
of that band's gate limit on its own group delay. In a 2.1 project the band-level verdict is
reconciled across channels: below the bass-management crossover one subwoofer reproduces
both channels, so if the guard strips the correction for either channel there, both are
re-run with it stripped. Letting them disagree splits the two filters' phase in exactly
the band where the amplifier sums them - a real 2026-08-04 filter left 84 degrees of L/R
split and -1.95 dB of mono bass at 20-30 Hz that way, which per-channel verification
sweeps cannot observe because only one channel plays at a time. Both stages exist because real modal
excess defeats the per-bin verdict alone: the baseline's per-bin group delay is noise
with swings far beyond any tolerance, so that comparison degenerates into a variance
contest, and a corrector carrying a consistent +34 ms bias survived it on real captures
while an unweighted residual statistic even looked "improved". Off reproduces the
original algorithm bit for bit, which the SECS.py parity fixture pins.

**Extended delay ceiling (this project's addition, music-only; the default setting is
automatic).** Latency only costs lip-sync, which music playback does not have, so the
ceiling (automatic or 10/30/60/100 ms in the UI) may be raised; when it is, the design
uses the ceiling as its target delay outright (the 2-10 ms automatic search judges
magnitude only and cannot see the phase benefit being opted into). Three mechanisms make the budget
actually usable, all confined to the improved path with an extended ceiling: (1) the
low-band bulk advance is estimated first (complex correlation over 20-90 Hz, 0.5 ms
steps up to the ceiling) and divided out around the band smoothing - the smoothing was
designed for shallow <=10 ms ramps, and averaging unit vectors through the several full
turns a 50 ms ramp makes per window simply cancels them (measured: a 60 ms budget still
produced a +26 to +41 ms *delaying* corrector until this split); (2) the low band's
pre-ring window becomes flat-topped with a 10 ms taper, because the original
full-extent cosine taper left a restored 51 ms advance at 5.6% gain; (3) the guard and
the gate allow intentional advance asymmetrically, up to the budget plus the band
limit, while added delay keeps the plain limit. Mid/high pre-ring windows keep their
10/5 ms caps throughout. Measured on the real 2026-08-03 captures (60 ms ceiling,
seat-referenced band arrivals re 1-4 kHz): L 20-40 Hz +37.8 -> +8.5 ms, L 40-70 Hz
+55.6 -> +4.3 ms, R 40-70 Hz +57.1 -> +1.4 ms, with the R 20-40 Hz band overshooting
to -18.5 ms early (|error| still halved; the per-channel bulk estimate is one number
per channel and that band's lateness differed from its neighbor's). The shared-sub-band
L/R match holds (worst split 3 degrees, mono sum loss 0.00 dB).

**Automatic ceiling resolution (the default).** The optimum ceiling is not "as large as
possible" but "just past what the room needs": a 60 vs 100 ms controlled comparison on
the same captures showed the extra 40 ms bought band-arrival differences of at most a
few ms while deepening the filter's early-energy spread (40-70 Hz content starting 49
-> 64 ms before the peak, 70-120 Hz 18 -> 31 ms) and adding 40 ms of playback lag. So
by default the design measures the requirement itself: the same 20-90 Hz matched-filter
bulk-advance scan is run per channel over the full 250 ms hard ceiling on the exact
excess-phase inverse the design will correct, and the ceiling resolves to the worst
channel's requirement plus the 10 ms flat-window taper (so the advanced content never
lands in the taper), rounded up to a 5 ms grid. A pair needing no more than the
original 10 ms budget resolves to exactly that budget and spends no extended latency at
all. The resolved value and the measured requirement are recorded in the design
summary, and the export path replays the recorded value rather than re-probing.
Manual ceilings remain available; validation refuses an extended ceiling without the
improved path (the original algorithm at an extended fixed delay reproduces the
measured smoothing-cancellation failure). Measured on the 2026-08-03 captures the
probe reads L 51.0 / R 56.0 ms and resolves 70 ms; the 70 ms design passes the gate
and lands L 20-40 Hz +12.8, L 40-70 Hz +1.6, R 40-70 Hz +0.2 ms at the seat -
equivalent to the best hand-picked ceiling without the user choosing anything.

**Band-wise tau decomposition: investigated and rejected (2026-08-04).** The
remaining 20-40 Hz residual invited an obvious refinement - rotate by a per-band
advance profile instead of one scalar per channel, since the room's energy-median
lateness is ~42 ms at 20-40 Hz vs ~57 ms at 40-70 Hz. Implemented and measured on the
real captures, every variant lost to the scalar: taking each octave band's
matched-filter reading raw let a junk band poison the design (the 18-35 Hz excess has
coherence 0.17-0.22 against a 0.21 noise floor - no single arrival exists there - and
its 207 ms "reading" drove the automatic ceiling to 220 ms, a +39 ms gate violation
and a 74 degree mono split); blending each band toward the wide-band anchor by
coherence regressed L 20-40 Hz from +12.8 to +34.5 ms because a few ms of rotation
change flips the phase guard's finely balanced band verdicts; and blending toward
zero collapsed one channel's profile, made the L/R verdicts diverge, and the 2.1
reconciliation then stripped the shared band on both channels - no correction at all.
The scalar rotation stays. What ships from the investigation is the diagnostic
(`secs_low_band_advance_profile_ms`): per-band matched-filter readings with their
coherence, noise floor, and a 0-1 trust weight, so a junk reading is recognizable as
such. Context for the residual itself: the -18 ms peak reading at R 20-40 Hz is partly
an envelope-lobe artifact - the robust energy-median residual is -8.3 ms (R) / +19.5 ms
(L), against low-frequency audibility thresholds commonly cited at 20-40 ms.

**Group-delay gate (this project's addition, judged at design time).** Because an
all-pass defect is invisible to every magnitude metric, the designed 48 kHz taps
themselves are measured: per-bin group delay from wrapped finite phase differences,
magnitude-weighted band medians relative to the filter's own 1-16 kHz baseline, worst of
L/R. Limits: 20-100 Hz 30 ms, 100-300 Hz 15 ms, 300-1000 Hz 8 ms — the shape of
published audibility thresholds (a few ms midband, 20-40 ms cited at low frequencies)
with margin. With the improved option the gate is hard (the design fails); with the
original algorithm it only warns, so the preserved original stays usable for comparison.
The report ships in the design summary either way.

**Automatic target-delay search.** How far the whole filter is pushed back is searched from
2 to 10 ms in 1 ms steps, choosing the value that minimises a low-frequency-weighted error
on the L+R sum. That delay *is* the pre-ringing budget for the phase correction: longer
buys more low-frequency phase repair and costs latency.

### Stage 4 — stereo combine (original)

1. Level-match the channels on the RMS within ±5 ms of each peak.
2. Align both impulse peaks to the same sample.
3. Trim the right channel by the median L−R difference of the smoothed 20 Hz–1 kHz
   responses (±6 dB limit).
4. Apply a squared fade over the last 5 ms to end the tail.
5. Normalise so the peak spectral magnitude across both channels is 0 dB. The attenuation
   this costs is the built-in preamp, which is why no extra convolution preamp is needed.

### Stage 5 — where 2.0 and 2.1 diverge (this project's addition)

**In 2.0** everything above is fully independent per channel. The two speakers are
physically different sources reaching the seat by different paths, so answering each
measurement on its own is correct.

**In 2.1**, below the bass-management crossover **one subwoofer reproduces both channels**.
A difference between the L and R measurement there is not a channel difference; it is the
same path measured twice. Left as the original algorithm has it, SECS believes that
difference and cuts the two channels differently, without depth limit.

Measured on this development system (90 Hz crossover), from its first 2.1 filter:

| 20–90 Hz | Without commonization | With commonization |
| --- | ---: | ---: |
| L/R magnitude split (max) | 11.1 dB | 0.8 dB |
| L/R phase split (max) | 178.7° | 13.2° |
| L/R phase split (RMS) | 40.2° | 5.1° |

At 37 Hz the two channels were being corrected 11.8 dB apart, and the phase was very nearly
**inverted**.

The phase half matters more than the magnitude half. Bass management **sums** L and R into
the subwoofer, so when the two filters disagree in phase, the mono content that carries most
musical bass partially cancels at the subwoofer input. A verification sweep plays one channel
at a time and therefore **cannot observe that cancellation in principle** — the measurement
looks fine while music loses bass.

So in 2.1, below the confirmed crossover, the magnitude and excess-phase corrections are made
**common to both channels**:

- **Magnitude** — the energy mean of the two magnitudes becomes the common input.
- **Phase** — the two excess-phase correctors are blended toward their normalised complex
  mean, which keeps them all-pass.
- **Weighting** — full weight to the crossover, fading to zero over the half octave above it,
  where an LR4 subwoofer is already some 12 dB down and the mains dominate.
- **The phase-confidence guard still watches each channel's own magnitude.** The averaged
  magnitude has that channel's nulls erased, so guarding with it would disarm the guard.

Above the crossover (127 Hz here) the channel difference is left alone: there the mains each
produce their own sound and the difference is real. Measured, the 110–180 Hz L/R difference
went 8.0 → 8.3 dB, i.e. unchanged.

On a 2.0 project, or when no subwoofer setup has been confirmed on hardware, this stage is
inactive and the design follows the original path exactly.

### What this project added (not the original design)

| Addition | What it does |
| --- | --- |
| Multi-position magnitude average | Replaces the magnitude input with a seat-weighted energy average of every measured seat; phase stays on P0 |
| Target-curve overlay | Multiplies the adaptive target by the house curve chosen in the app, re-anchored to 0 dB at 500 Hz |
| 2.1 shared-sub-band commonization | Stage 5 above |
| Closed-loop verification | Plays the filter, remeasures, and compares against the prediction (judged 20–650 Hz) |
| Per-rate members | The 48 kHz member is byte-identical to the verified trial; every other member is that FIR resampled with the scipy-parity resampler, so all six rates carry one transfer function by construction (v3; v2 transplanted the excess corrector but re-ran windowing, the phase guard, and the min-phase EQ per grid, and near a razor-sharp modal cut the realized phase's winding count proved grid-dependent - a real 44.1 kHz family shipped 33.6 ms of 20-100 Hz group delay away from the verified member) |
| Headroom calculation | Measures program-material time-domain peak growth to produce the recommendation |

Defects in these belong to this project, not to the original author.

### What each setting actually changes

| Setting | Effect |
| --- | --- |
| Boost ceiling (default +6 dB) | How far dips may be filled; raising it fills more but loads the amplifier and drivers |
| Tilt | Adds a per-octave slope to the target |
| Bass shelf / corner | Raises the low-frequency target; start here if the corrected bass feels thin |
| Resolution (Low/Normal/High) | Changes the smoothing resolution and the peak-crush strength together |
| Curtain (default 300 Hz) | Upper limit of narrow-band correction |
| Latency mode | Normal = mixed phase; low/zero latency = minimum phase only, giving up phase correction |
| Target delay | The pre-ringing budget available to the phase correction |

### Known characteristics

- Because cuts are unbounded and the sub-500 Hz peak crush stacks on top of them, **a few dB
  of deficit is left beside the largest modal peaks.** Measured: after removing a 15 dB mode
  at 41 Hz, −5.6 dB remained at 37–38 Hz and an average −1.7 dB across 150–250 Hz. The
  correction is broader than the room's own structure there, so the valleys beside a peak get
  cut with it.
- The 35th-percentile anchor and the 0 dB peak normalization together mean **the overall
  level drops after correction.** That is a digital level shift, recovered with volume — but
  the volume must be held fixed for verification measurements.
- A mixed-phase filter **grows broadband program peaks by several dB even at a 0 dB response
  peak** (+2.8 to +6.4 dB measured), which is why the recommended headroom is far larger than
  a sweep-based estimate suggests.
