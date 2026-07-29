# DSP specification

Implemented algorithm identifiers:

- `phase1-bounded-boost-v2`: deterministic offline magnitude correction with
  spatially gated boost capped at +3 dB.
- `phase3-single-sub-ranking-v2`: deterministic ranking of measured manual
  single-subwoofer settings.
- `phase3-separated-path-delay-polarity-search-v2`: bounded complex synthesis of
  main-delay and 0/180-degree polarity alternatives across physically measured
  crossover states.
- `phase4-response-replay-v2`: deterministic 48 kHz minimum-phase design and
  predicted-only validation from a measured combined-response baseline.
- `phase6-native-six-rate-v2`: independent native-grid synthesis at all six target
  rates, common safety normalization, and cross-rate magnitude/group-delay checks.
- `wireless-sweep-recognition-v1`: known-WAV recognition on an asynchronous
  microphone capture; this is file/offset evidence, not absolute channel timing.
- `umik-calibration-parser-v2`: strict UMIK-style TXT parsing, quoted miniDSP metadata
  compatibility, and log-frequency/linear-dB magnitude-correction interpolation.
- `known-sweep-deconvolution-v5`: recognized-capture resampling, fixed signed
  pre-zero retention for marker-referenced paths, regularized known-sweep
  deconvolution, magnitude calibration, and measurement-quality gates. It retains
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
20 Hz-20 kHz required band, its log-frequency-interpolated additive correction `C(f)`
is applied as

```text
H_cal[k] = H_raw[k] 10^(C(f_k) / 20)
```

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

Bundled targets are independent `-style` product curves, not official B&K or Harman
datasets. Version 1 uses linear interpolation in dB on `ln(f)` between knots and
holds the nearest endpoint outside the knot range.

| Hz | B&K-style dB | Harman-style hi-fi dB |
| ---: | ---: | ---: |
| 20 | 6.0 | 7.0 |
| 30 | 5.3 | 6.6 |
| 50 | 4.2 | 5.6 |
| 80 | 3.2 | 4.4 |
| 120 | 2.4 | 3.3 |
| 200 | 1.4 | 2.0 |
| 500 | 0.0 | 0.0 |
| 1,000 | -0.5 | -0.4 |
| 2,000 | -1.0 | -0.9 |
| 5,000 | -2.2 | -2.0 |
| 10,000 | -4.0 | -3.8 |
| 20,000 | -6.0 | -6.5 |

Design rationale: both curves provide a modest low-frequency shelf and gentle
downward full-band diagnostic trend. Harman-style v1 has the larger bass shelf.
Only 20-500 Hz is fully corrected by default, so high-frequency knots are context
and future-proofing rather than permission for full-band EQ.

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

### Live separated-path delay and polarity search

`phase3-separated-path-delay-polarity-search-v2` reuses the ranking objective above.
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

The bounded delay grid must lie within -20 to 50 ms, use a 0.01–5 ms step, contain at
most 1,001 values, and divide its range exactly. Total
`crossover_count * delay_count * 2` candidates are capped at 10,000. Synthesis retains
positive bins from `max(20 Hz, 0.4 min(x))` through 500 Hz; the reused Phase 3 scorer
then selects its common `[0.5 min(x), 2 max(x)]` comparison band.

Sub level is fixed and no candidate receives automatic level alignment. The one
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
weight 1. P1-P5 each have weight 1. A trial requires at least an accepted P0 pair.
When both central pairs exist, let `m_c,p` be the median level over 200-500 Hz and
remove it from each channel response. Trial design additionally requires

```text
|m_c,P0_END - m_c,P0| <= 1.0 dB
RMSE_20-500((L_c,P0_END - m_c,P0_END) - (L_c,P0 - m_c,P0)) <= 6.0 dB
```

for both channels. The absolute level bound remains the session/gain stability check.
The shape bound is intentionally loose because P0_END is returned by hand after P1-P5;
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
