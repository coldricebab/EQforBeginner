# Validation and fallback policy

Validation is a sequence of numerical gates. A plausible graph is not evidence of a
safe filter.

## Advanced wireless sweep-recognition gates

The live advanced path is a file-recognition gate, not yet a calibrated measurement
gate. The imported reference must be at most 32 MiB, exactly 48 kHz, 1-60 seconds,
mono or stereo PCM 8/16/24/32-bit or IEEE float32, finite, normalized, and non-silent.
Stereo admission requires one channel at least 60 dB below the other or L/R normalized
correlation of at least 0.999 with no more than approximately 1 dB level difference.
It also admits a dominant channel whose RMS energy is at least three times the other
channel (approximately 9.5 dB), which covers a measurement sweep accompanied by
lower-energy acoustic timing-reference markers. Similarly active unrelated stereo
signals are rejected before an input stream opens. The supplied left/right
`assets/sweeps/*_refR.wav` files are regression fixtures for this rule.

`wireless-sweep-recognition-v1` applies zero-mean normalized cross-correlation to a
4,096-sample mid-file probe using bounded block FFTs. A default detection needs
absolute probe correlation at least `0.45`, at least four of seven independent
1,024-sample segment matches at correlation `0.35`, an effective slope within
`±2,000 ppm`, timing-fit RMS no greater than 24 samples, and complete inferred
start/end containment. A well-separated second playback at least 90% as strong as the
best candidate returns `ambiguous`; an isolated strong probe whose independent
segments fail returns likely false positive and is shown as not detected.

The CPAL capture must also be complete and free from these conditions before
`quality_accepted=true`:

- sample peak at or above the capture clipping threshold;
- non-finite data or a raw callback-format contract violation;
- xrun, callback loss, or missing tail;
- runtime stream error or timeout.

Recognition and capture quality are separate. A damaged capture may still identify the
file so the UI can diagnose the path, but it must be excluded from later measurement.
Backend timestamp gaps/regressions are recorded separately. They do not reject an
otherwise complete PCM capture because CoreAudio callback timestamps can vary without
proving a missing sample; repeated markers and deconvolution still enforce the
measurement extent and effective-rate checks.

Accepted-cache restore is fail closed. It scans prior projects newest-first and selects
the first accepted snapshot for each measurement kind/position/channel key, allowing
older compatible projects only to fill keys missing from newer ones. Every contributing
project must have exact system mode, microphone device/channel, calibration SHA-256,
channel-specific sweep SHA-256, sample rate, project format, and deconvolution version.
Separated-path plan compatibility is capture-relevant rather than whole-plan: the
hardware state held during the isolated captures (measured delay, polarity, sub level)
and the marker/sweep routing must match exactly, a wide-band sub capture additionally
binds to the low-pass dial it was made through, and a measured-mode capture binds to
its own candidate's physical crossover - while the candidate list (wide-band), delay
search range/step, and slope models are pure search/synthesis parameters that may
change freely over the same captures. Baseline restore binds to the confirmed hardware
setup, not to the search plan. It rejects missing raw WAVs,
non-finite/inconsistent arrays, failed snapshots, verification snapshots, and paths
outside their source project.
The legacy isolated-recognizer result (no longer mounted as a product panel) is fixed
to `timing_eligible=false`: its
`intra_sweep_segment_fit` ppm is not repeated-marker evidence and that recognition
result alone cannot unlock channel delay, closed-loop verification, headroom, or
export.

The full live adapter analyzes each uploaded stereo WAV before capture. It separates
the longest main-sweep region from the before/after marker regions, calculates L/R
energy and correlation per marker, selects the dominant/identical channel, and retains
that channel's exact marker waveform. The bundled `*_refR.wav` regression files must
report both markers on R.

During capture the marker recognizer uses minimum absolute correlation 0.20 and
minimum independent-marker-segment correlation 0.16. Candidate generation permits
25,000 ppm of room-biased slope within one short marker and retains a separated second
peak at least 50% as strong as the first. Those relaxed candidate values are not clock
evidence. `known-marker-capture-endpoint-v4` additionally retains a candidate at
correlation >=0.30 when all five marker segments match and only the intra-marker drift
or timing fit is rejected; room excess delay can bias that single short fit. Such a
candidate cannot complete alone: acceptance still requires a valid chronological pair
whose separation error is at most 5,000 ppm. Low-level/reverberant, unequal-marker, and
preserved P4 L/R capture replays cover these paths. A valid pair maps the exact
main-sweep source boundary into the capture and
supplies the deconvolution clock ratio. Capture ends on the first 250 ms monitor poll
that contains the complete end marker, with no fixed additional post-marker samples;
final deconvolution still requires the WAV's built-in spacing to retain the
32,768-sample IR tail. If a
pair is unavailable, the full-sweep recognizer remains the fallback with a 96-sample
magnitude-only fit ceiling and ratio 1.0 rather than its room-delay-biased intra-sweep
slope. Neither branch changes `timing_eligible=false`.

## Live calibration, deconvolution, and evidence gates

The live project rejects calibration text larger than 2 MiB, malformed/non-finite
rows, non-increasing or duplicate frequencies, fewer than two points, or coverage that
does not span 20 Hz-20 kHz. It rejects sweep WAVs larger than 32 MiB, non-48-kHz or
non-mono/stereo data, unsupported encoding, out-of-range/non-finite samples, wrong
dominant L/R channel, or a longest active measurement region shorter than one second.
Identical reimports are idempotent; changed inputs invalidate dependent in-memory
measurements/design.

The first physical microphone input opened by a project becomes its locked device ID.
Every active capture token binds the session ID, monotonic evidence generation, input
device, calibration hash, and channel-sweep hash; verification additionally binds the
exact trial SHA-256. Calibration/sweep mutation, design/export, validation, and project
replacement are blocked during active capture. A stale completing token is rejected
rather than admitted against newer evidence.

Raw capture is always written before recognition/deconvolution so failure evidence is
not lost. A measurement enters a spatial/design set only when all of these are true:

- either a complete chronological marker pair has each correlation >=0.20, or one
  complete, unambiguous fallback sweep is recognized with correlation >=0.45;
- the calibrated known-sweep deconvolver retains enough pre-roll and post-roll;
- marker-referenced deconvolution retains a fixed 4,800-sample pre-zero window and
  records its -100 ms origin without moving the impulse peak;
- the calibration profile in force covers 20 Hz-20 kHz and is applied without
  extrapolation. A session may deliberately run with no TXT - for a microphone that
  applies its correction internally - and then measures through an identity profile
  that is exactly 0 dB across that band, so the calibrated response is bit-identical to
  the raw one. Such a session binds to its own evidence identity
  (`none:uncalibrated-microphone-v1`), which is not a hex digest and therefore cannot
  collide with a real file's SHA-256, and every manifest carries
  `calibration.uncalibrated: true`;
- no sample reaches the 0.999 clipping threshold;
- recognized main-sweep peak is within the hard -48 to -6 dBFS range; -30 dBFS is the
  preferred lower boundary;
- at least 1,024 pre-sweep noise samples remain after the 256-sample guard. The
  noise floor is read from at most the 4,800 samples immediately preceding that
  guard, not from the whole retained pre-roll: the retained region reaches back
  toward the start marker, whose decaying reverberation is not ambient noise, so
  an unbounded window would inflate the estimate and reject good captures
  whenever the pre-zero window grows;
- capture SNR is >=20 dB; reconstruction fit is recorded as a diagnostic when a
  complete repeated-marker pair independently qualifies extent/clock, while the
  markerless fallback still requires >=12 dB;
- effective/marker rate mismatch remains within 2,000 ppm and the live
  magnitude-only timing fit is <=96 samples;
- CPAL reports a complete capture with no format error, stream/runtime error, timeout,
  or suspected sample drop.

The live meter reports microphone input peak before marker recognition, so a moving
meter distinguishes input routing from pattern recognition. The measurement snapshot
records every issue and accepted/excluded state, both marker
flags, automatic-completion evidence, sweep peak/RMS, and optional estimated SPL. The
estimate uses `94 + 24 - SensFactor + RMS_dBFS`, assumes operating-system input gain
0 dB, and presents 65-85 dB SPL as guidance. Because the host gain relationship is not
independently verified, it is advisory and does not decide admission; digital
peak/clipping, SNR, recognition, and stream gates do. Reconstruction fit is a hard
gate only for the markerless fallback. Automatic output
clipping, acoustic L/R swap, transient contamination, and
microphone-motion classification are not implemented; they remain human beta-test
checks.

At design time only accepted common-grid L/R pairs are retained. Baseline P0 L/R is
mandatory. P1-P8 and P0_END are optional; missing pairs are not invented. P0 has weight
2 unless P0_END exists, in which case both center measurements have weight 1.
When both central pairs exist, each channel must keep its absolute 200-500 Hz median
level shift within 1.0 dB. Its level-removed 20-500 Hz shape RMSE must remain within a
loose 6.0 dB gross-change bound. This accommodates hand repositioning after P1-P8 while
still rejecting a clearly different signal path or listening region. Failure blocks
design; this gate does not qualify timing or exact P0 replacement.

## Phase 1 offline gates

- Every input, spectrum, correction bin, tap, and metric is finite.
- Weighted spatial energy mean agrees with the analytic formula.
- Requested magnitude correction remains within -12 to +3 dB at design, stereo blend,
  and FIR input boundaries.
- Positive correction requires at least two positions, a one-third-octave-smoothed
  upper spatial quartile at least 1 dB below target, and a contiguous width of at
  least 0.25 octave.
- Protected deep/narrow dips receive no positive gain and are not chased by adjacent bins.
- Correction is unity outside the transition band; realized FIR gain is no greater than
  `+3 dB` and realized attenuation must remain within the `12.05` dB numerical gate.
- Common-to-independent stereo transition is continuous.
- FIR peak and energy are finite; stereo WAV readback preserves declared L/R order.
- Above the 0.01 dB no-op floor, 20-500 Hz target RMSE must be strictly lower than raw.
- On 1/12-octave-smoothed raw peak bins at least 1 dB above target, peak RMSE must
  improve by at least 10% and the worst peak by at least 5%.
- Every validation threshold is finite and range-checked before any comparison; a
  caller may make a gate stricter but cannot weaken the product safety minima/ceilings.
  Empty peak sets are a valid “nothing to correct” case with zero peak metrics.
- Reaching the selected attenuation limit is a redesign warning and blocks the overall
  `passed` state even when the remaining numerical metrics look acceptable.
- A request beyond +3 dB is capped and recorded; it cannot relax the spatial or
  protected-dip gates.

If any gate fails, no filter is labeled valid and the harness produces no artifact.
The checked-in ZIP is admitted only by the synthetic offline gates and is still labeled
`predicted-only-synthetic`; it is not an end-user export.

### Effective smoothing widths (2026-07-28 release review)

The design/gate Gaussian smoothers in `correction.rs` and `validation.rs`
convert FWHM to sigma with a deliberate `FWHM/(2·√ln2)` convention, so the
**realized** width is √2× the configured value: the nominal 1/12-octave cut
and gate smoothing acts as ≈0.118 octave (~1/8.5), and the nominal 1/3-octave
boost smoothing acts as ≈0.471 octave. Every product gate — most critically
the ≤0.5 dB realized protected-dip contract — plus the checked-in fixtures
and the first real-room verified session are calibrated against these
effective widths; converting to the textbook formula makes the realized FIR
leak more than 0.5 dB into protected dips beside legitimate cuts (measured
0.55/0.61 dB in the phase4 regression scenario). The convention is therefore
kept, the code comments state it explicitly, and the evidence record
(`predictionVerificationSmoothingFwhmOctaves`) now stores the effective
0.118-octave value instead of the nominal constant. The display-only result
plot uses the separate `smoothing.rs` smoother, whose 1/12-octave label is
exact. UI copy says "octave smoothing" without a fraction.

## Phase 3 offline ranking gates

The `phase3-single-sub-ranking-v2` Rust path fails closed on an empty candidate set,
duplicate IDs, non-finite or out-of-range settings/data, non-increasing grids, missing
channels, or any difference in position IDs, weights, ordering, or frequency grids
between candidates. At least three bins must remain in the common comparison band.
Configuration smoothing widths and the anchor threshold must be finite and positive;
all score weights must be finite and nonnegative.

The following evidence rules are also enforced:

- Candidate SPL is never automatically aligned and the original timeline is not
  shifted. The 200-500 Hz anchor is diagnostic only and warns above 1.5 dB spread.
- Phase irregularity is scored only when every candidate/channel/position has phase.
  Same-setting timing reliability is scored only when every response has repeat timing.
  Otherwise the whole unavailable term is omitted and the report contains a warning.
- Worst-seat and spatial-spread penalties are used only with more than one position.
  A P0-only run is labeled `limited-single-position`.
- The result always requires a newly measured hardware confirmation. Ranking first is
  never itself a pass or “integration complete” state.
- The measured-fixture CLI verifies every referenced `.mdat` source SHA-256 and size,
  requires 48 kHz and the declared 20-500 Hz extraction range, and refuses converted
  input marked as level-aligned or timeline-shifted.
- The extracted linear grid must preserve REW's recorded non-zero frequency origin;
  treating response index zero as 0 Hz is a fixture-generation failure.
- Every available XO80 separated main/sub pair must predict its measured complex sum
  over 40-160 Hz with magnitude RMSE <= 1.0 dB. This is a fixture/model-admission gate,
  not part of the 70/80/90 candidate score.

Synthetic unit tests cover measured responses representing correct-versus-wrong
polarity, wide combination dips, declared delay/level regularization, a one-position-only
improvement losing to a robust multi-position candidate, optional phase/timing
evidence, deterministic tie breaking, invalid grid/range rejection, exact
separated-path complex summation, cancellation loss, and the no-auto-alignment anchor
warning.

### Live separated-path search gates

The live `phase3-separated-path-delay-polarity-search-v5` adapter additionally fails
closed unless all of the following hold:

- 2–12 unique, strictly increasing real crossover settings are declared in the
  30–200 Hz UI/hardware range.
- Both uploaded sweep WAVs contain start and end markers on the same single L or R
  acoustic-reference speaker. Mono, identical-stereo, missing, split, or different
  marker routing is rejected.
- Every crossover has accepted P0 L-main-only, R-main-only, and sub-only captures.
  The sub-only role must use the channel opposite the fixed marker speaker.
- An isolated capture whose spectrum contradicts its declared role is rejected:
  a sub-only capture whose 2.2-10x-crossover band comes within 12 dB of its own
  passband, or a main-only capture whose 0.3-0.5x-crossover band comes within 6 dB
  of its passband, means the other path was live during the capture. The rejection
  additionally requires the flagged band to be measurable above the room's own
  noise: its capture-domain SNR (sweep-span band power against pre-sweep band
  power) must reach 20 dB. Below that the reading is noise-limited - on the first
  real session, ambient 20-40 Hz noise at ~10 dB band SNR filled the sub band with
  the subwoofer physically powered off, while a genuinely live sub measures
  32-33 dB - so the capture is admitted and the finding is recorded as a
  `*_noise_limited` diagnostic instead of a rejection.
- Every stored capture remains bound to the current session mode/search plan,
  calibration hash, sweep hash, microphone device, and input channel.
- Every response has finite magnitude and phase on one exact common grid with at least
  three admitted bins.
- Delay bounds and step are finite and increasing. Each candidate's arrival-centred
  half-period window holds at most 1,001 step-snapped values and the complete
  crossover/delay/polarity search at most 10,000 candidates. A step too fine for
  those caps is not refused: the run-time grid coarsens the stage-one scan to the
  finest step multiple that fits and then refines at the full requested resolution
  around each crossover's stage-one optimum, reporting the two-stage run as a
  warning.

In the wide-band search mode (`phase3-wide-band-crossover-synthesis-v1`) the same
adapter instead enforces:

- The plan must declare both filter-slope models (LR4/LR2/BW2 per branch); a
  measured-states plan with model fields, or a wide plan without them, is rejected.
- The dial value entered for the sub capture must be finite in 120–500 Hz and every
  candidate crossover must sit at or below it, because the low-pass replacement
  ratio `LP(candidate)/LP(dial)` is bounded by unity only in that direction; above
  the dial it would amplify measurement noise.
- Exactly three capture roles exist: `FULL` L/R mains (kind `sub_main_only`) and one
  `WIDE` sub (kind `sub_only`, opposite the marker speaker). The measured-mode
  `XO##` vocabulary is rejected while a wide plan is active, and vice versa.
- The sub-only leakage check is judged against the dial instead of a candidate
  crossover; with the dial at 250 Hz its 2.2x checked band exceeds the 500 Hz
  analysis cap and the check honestly skips (protocol muting is the remaining
  guard), and a declared bypass has no corner to judge against.
- A full-range main legitimately contains deep bass, so the main-only sub-band
  leakage test does not apply (the sub output being off also means no signal can
  reach the sub). Instead, a main whose 40–100 Hz mean sits more than 30 dB below
  its own 200–500 Hz anchor is flagged `main_full_range_low_band_missing` as a
  non-rejecting diagnostic - it usually means bass management was left on, but a
  small speaker's natural rolloff can approach the same shape, so it never rejects.
- The cross-capture start-marker level gate widens from 0.3 to 0.5 dB, because the
  wide dial's high-pass attenuates the reference speaker's >=650 Hz marker band by
  a bounded ~0.2 dB on the sub capture only; a real volume change still trips it.

Changing the plan, sweep, calibration, or any isolated capture invalidates the ranking
and applied-setting evidence. When a ranking exists, the backend accepts only its exact
crossover, delay, and polarity, and a sub level within 0.5 dB of the recommended one
(the measured level plus the winner's deployment trim; the tolerance covers level-dial
granularity) as the user-declared hardware state.
It then preserves the isolated evidence but still requires new combined Raw captures.
The live UI labels this a predicted recommendation because a numerical
predicted-versus-combined confirmation gate is not yet implemented; in wide-band mode
the report additionally warns that the recommendation inherits the declared slope
models rather than per-state measurements.

### Current measured-fixture regression

The checked-in P0 replay compares only crossover 70/80/90 Hz while the declared main
delay remains 0.83 ms; it does not vary or optimize delay, polarity, or sub level. On
the 35-180 Hz common band, lower-is-better scores are currently:

| Rank | Measured candidate | Total score | Deficit RMS | Deficit p95 | Worst deficit |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | 90 Hz | 2.3417 | 0.0000 dB | 0.0000 dB | 0.0000 dB |
| 2 | 80 Hz | 3.8165 | 1.0930 dB | 1.9246 dB | 2.1688 dB |
| 3 | 70 Hz | 5.7033 | 2.2108 dB | 4.0554 dB | 4.3375 dB |

These replace the earlier shifted-grid regression values. REW stores the first
response bin at `startFreq=20.1416015625 Hz`; the corrected converter associates array
index zero with that origin instead of 0 Hz. Candidate anchor spread is 0.1331 dB,
with no alignment applied. Phase is present;
multi-position evidence and same-setting crossover timing repeats are absent. Four
XO80 separated-path checks pass the 1.0 dB gate: measured-state complex-sum RMSE is
0.5601/0.5024 dB for L with sub A/B and 0.3756/0.4354 dB for R. The 180-degree
sub-phase counterfactual is much worse (4.0375-5.8000 dB), but it is not an actually
measured inverted combined state and therefore does not establish the hardware's
polarity setting.

Both sub-only source captures retain REW's recorded “High measurement distortion
11.4%” warning. The ranking is therefore a useful deterministic regression and a
provisional preference for the measured 90 Hz candidate, not a release-quality
multi-position integration result. A fresh selected-setting confirmation capture and
spatial measurements remain mandatory for product completion.

## Phase 4 offline measured-response gates

`phase4-response-replay-v2` is an offline response replay, not the hardware release
gate described below. Its admission path fails closed unless all of the following are
true:

- the fixture schema and grid-origin extractor version match; no SPL alignment or
  timeline shift is declared, and the source/analysis ranges are 20-20,000 Hz and
  20-3,000 Hz respectively;
- exactly six declared XO90 roles exist: L/R combined, L/R main-only, and sub-only
  A/B, each with a verified source SHA-256 and byte size;
- every response is 48 kHz on the same finite, increasing REW grid, shares one embedded
  UMIK calibration identity and SPL offset, and retains calibration coverage through
  20 kHz;
- offline quality metadata is finite, every response has at least 20 dB SNR, each
  combined response has at least 30 dB SNR, and known sub-only distortion limitations
  remain explicit;
- every response retains an acoustic-reference timeline with finite timing values and
  plausible clock adjustment; exactly the two combined raw IRs retain an unshifted
  48 kHz start time and finite samples;
- the user-declared state remains 90 Hz crossover and 0.83 ms main delay assumed
  optimal, with polarity, sub level, playback volume, and microphone gain declared
  unchanged but unrecorded; these assumptions must not be marked verified;
- the post-FIR measurement list is empty and all missing-evidence/limitation fields
  remain present;
- all four L/R x sub-A/B separated-path predictions match measured combined magnitude
  over 45-180 Hz with RMSE <= 1.0 dB.

After admission, numerical pass requires the finite `[-12,+3] dB` constraints,
realized gain <= `+3 dB`, realized attenuation <= 12.05 dB, target RMSE improvement,
broad-peak RMSE improvement of at least 10%, worst broad-peak error improvement of at
least 5%, protected-dip attenuation <= 0.5 dB, protected-dip boost <= 0.05 dB, and a
resolved safe redesign for every initial over-limit cut request. The FIR artifact must
read back as 48 kHz, stereo IEEE float32, 16,384 frames. Staging validation also
rejects any emitted ZIP.

### Current Phase 4 measured-baseline regression

The developer-local `examples/phase4-offline-measured/project.json` (produced by the
offline replay from measurement sources that are not part of the public repository) is
retained as a legacy `phase4-response-replay-v1` compatibility fixture and reports
`numerical_prediction_passed=true`. These are response-replay metrics on P0 only; the
v2 boost path deliberately requires at least two positions and therefore would not
boost this input:

| Metric | Left raw | Left predicted | Right raw | Right predicted |
| --- | ---: | ---: | ---: | ---: |
| Target RMSE, 20-500 Hz | 6.1995 dB | 5.5168 dB | 6.3414 dB | 5.8003 dB |
| Broad-peak RMSE | 6.0382 dB | 0.8296 dB | 6.0364 dB | 0.7575 dB |
| Worst broad-peak error | 15.3090 dB | 3.9871 dB | 15.1891 dB | 3.8670 dB |

The realized maximum correction attenuation is 11.6071/11.6075 dB for L/R and maximum
gain is approximately -0.000009 dB, so no positive correction is present. Maximum
attenuation beyond the bounded design at protected-dip bins is 0.1118/0.1751 dB
against the 0.5 dB limit.

The initial L/R requests were 16.9146/16.9279 dB and therefore retained the 12 dB
attenuation-limit warnings. The per-bin soft knee (identity below 8 dB,
tanh saturation toward 12 dB) absorbs most of the excess at request time, and the
typed safe redesign then scales against the clamped curve's own maximum (strengths
0.9991/0.9991) so the smoothed maxima land exactly on the 11.5 dB target; both
redesigns are marked resolved. FIR safety normalization is -0.1037/-0.1038 dB. The four 45-180 Hz
separated-path RMSE values are 0.6634, 0.5411, 0.3514, and 0.4365 dB, all below the
1.0 dB admission limit.

The raw combined IR convolution preserves L/R start times
`-1.0000578938/-0.9999591277` seconds. Its maximum absolute samples
`0.00043465/0.00041517` are diagnostic values only; they are not a playback true-peak
or clipping test.

### Phase 4 state boundary

The only successful offline state is `predicted-only-measured`. Even when its numerical
gates pass, the project must retain:

```text
hardware_verification = "unverified"
closed_loop_passed = null
export_eligible = false
recommended_headroom_db = null
```

No post-FIR measurement exists, each combined channel lacks a same-setting repeat, and
only P0 is available. The current plan defers real FIR-applied same-path measurement to
user testing after Phase 6. Until then the trial WAV cannot authorize headroom or final
Roon packaging, and “closed-loop verified” is prohibited.

## Live minimum-phase hardware gate

Device discovery is implemented but is not a hardware readiness gate. A selectable
choice means only that CPAL reported a 48 kHz PCM configuration with at least one input
or two output channels. It does not prove permission, simultaneous stream opening,
channel identity, clock stability, level safety, or successful capture.

The live developer beta implements a magnitude-response subset of the hardware gate.
The 48 kHz trial is always labeled predicted-only. The user must manually load it as
the only active Roon convolution, declare it active, preserve hardware/volume/gain
settings, and make new accepted P0 L/R captures. Raw, predicted, and verified responses
remain separate. The declaration timestamp and exact trial hash are backend evidence,
but are not independent proof of Roon state. Changing or clearing the declaration
invalidates previously captured verification evidence.

The session is locked to the first actual microphone device ID. Active captures carry
the session ID, evidence generation, device, calibration hash, channel-sweep hash and,
for verification, exact trial hash. Changing any of them makes the completing capture
stale. If accepted P0 and P0_END pairs both exist, each channel must pass <=1.0 dB
200-500 Hz median level shift and <=6.0 dB level-removed 20-500 Hz shape RMSE before
trial design. The latter is deliberately only a gross-change guard for a hand-returned
center-area point. The repeat gate does not authorize timing.

`live-closed-loop-validation-v4` runs the existing frequency-response validation for
each baseline/verified channel, with one replacement: the overall target-improvement
judgment is made on 1/12-octave-smoothed curves scored as the RMS of per-octave-cell
RMSE over 20-500 Hz (cells 20-40-80-160-320-500), not on unsmoothed linear-grid RMSE.
The linear judgment failed a working filter on the first real v5 session: 31% of its
bins sat in 300-500 Hz, where a small P0 microphone reposition between the baseline
and verification captures had already shifted the unfiltered response - the session's
own unfiltered repeats read 8.5-8.8 dB RMSE at 300-400 Hz against the baseline's 6.6
before any filter was loaded - while the genuinely corrected 50-100 Hz band held 8%
of the vote. The verified curve must beat the baseline on this metric, or reach the
value the accepted design predicted on the same metric (which covers designs whose
improvement lives in features narrower than the gate smoothing, such as the synthetic
fixture's single-bin modes). An unloaded filter fails both branches - its measurement
equals the baseline while the prediction sits below it - and the applied-scale fit
flags it independently. Both unsmoothed RMSEs remain reported as diagnostics, and
the peak-bin improvement gates are unchanged.

The agreement gate is unchanged: the signed predicted-versus-verified residual over
20-650 Hz, smoothed with the same 1/12-octave Gaussian window used by broad-peak
validation, must have RMSE <=3.0 dB per channel. This makes it insensitive to narrow
comb-filter displacement from a small P0 microphone reposition. It does not subtract
a fitted level offset, so broadband mismatch remains a failure; the unsmoothed RMSE
is retained in the result as a diagnostic. Both channels and every issue list must
pass. Failure returns no final package and does not alter the safe trial evidence.

A failed verification is retryable as a loop: recapturing the baseline P0 invalidates
the design and its trial, a redesign (same or different target) produces a new trial
whose SHA-256 must be re-declared in Roon, and new verification captures re-run this
same gate. Nothing from a failed attempt carries into the retried judgment.

This gate does not qualify arrival timing: all live captures remain timing-ineligible.
Automatic acoustic L/R swap detection is also absent. Therefore a live pass authorizes
only the minimum-phase magnitude package, never L/R delay correction.

`measured-fr-result-plot-v2` is built only after that closed-loop comparison has the
four admitted baseline/verified P0 records. Its Raw and Predicted L/R inputs are the
same weighted spatial energy averages used by Phase 4 validation; Target is evaluated
on the full retained grid with the exact per-channel alignment offsets; Verified is
the newly captured filtered P0 L/R magnitude. Raw, Predicted, and Verified are
Gaussian-smoothed at 1/12-octave FWHM on the bounded output grid for display only;
Target is interpolated without smoothing. The chart additionally applies one scalar
level offset per curve before drawing, so that each curve's 200-500 Hz median sits on
the Raw curve's: a mixed-phase SECS filter attenuates broadly, which otherwise pushes
its Predicted and Verified traces off the readable range even though the difference is
playback gain. The offset is per curve (not per channel), so curve shapes and the L/R
difference inside a curve are unchanged, and the applied values are printed under the
chart. This transformation does not feed any RMSE,
protected-dip, headroom, binding, or export gate. All 12 displayed series must:

- share one finite, strictly increasing log-spaced result grid;
- contain equal finite lengths and cover 20 Hz through the retained value near 20 kHz;
- preserve exact 500 Hz correction and 650 Hz taper-boundary samples;
- keep protected-dip and corrected-peak marker frequencies inside 20-500 Hz.

The plot payload is persisted inside the final closed-loop summary. UI visibility
toggles never change the underlying metrics or export eligibility, and no synthetic
curve may be substituted for a missing series.

## Phase 6 native-rate and packaging gates

For the offline measured preview, admission requires the Phase 4 v2 project to stay
`predicted-only-measured`, retain its missing-evidence list, keep
`export_eligible=false`, and hash-link the exact 48 kHz trial WAV. The adjacent design
CSV is resynthesized on the legacy 48 kHz/16,384 grid and each float32 tap must reproduce
that source within `1e-6`. Changed source bytes, incompatible layout, a design above
+3 dB, a ZIP inside the measured preview, or an attempted eligibility promotion aborts
the run.

Six filters are then redesigned on native 340 ms grids at exactly 44.1, 48, 88.2, 96,
176.4, and 192 kHz. Each output must be finite stereo IEEE-float32, remain within
`[-12,+3] dB` after dense readback, use the expected native length, and share the
common safety normalization.
Against the 48 kHz realization, every rate/channel must meet:

| Cross-rate gate | Limit |
| --- | ---: |
| Maximum magnitude difference, 20 Hz–20 kHz | <= 0.05 dB |
| Maximum relative-GD difference, 20–650 Hz | <= 0.02 ms |

The measured preview writes the six WAVs, `project.json`, and `README.txt`, then parses
their headers back. Its successful state is still an unverified developer preview,
with `recommended_headroom_db=null`, `export_eligible=false`, and no ZIP.

The live path has a separate admission gate: a passing live closed-loop summary and
the exact in-session Phase 4 gain intent. It calls the same native redesign and refuses
export unless `cross_rate_passed=true`. The live trial uses the existing Phase 6 48 kHz
native length of 16,320 samples, while the legacy offline Phase 4 fixture remains
16,384. `verified-trial-native48-response-binding-v1` refuses export unless the final
48 kHz response differs from the declared-and-remeasured trial by no more than 0.05 dB
magnitude over 20 Hz-20 kHz and 0.02 ms relative group delay over 20-650 Hz.

For the 48 kHz native filter and each registered L/R measurement sweep,
`validation-signal-and-response-peak-v3` computes four-times-oversampled input and
convolved-output peaks plus the FIR L1 worst-case sample-peak bound. Recommended
headroom takes the larger result, floors it at 0 dB, adds 1.0 dB, and rounds upward to
0.1 dB. Zero/non-finite input, convolution, peak, or FIR norm aborts export.

The final ZIP must pass the same strict six-rate readback before its SHA-256 and the
`hardware-remeasured-minimum-phase` project snapshot are written. The snapshot records
calibration/sweep hashes, captures and exclusions, Phase 4/closed-loop/Phase 6/headroom
algorithm versions, exact trial activation attestation, verified-trial/native-48k
binding metrics, headroom components, and final path.

User-directed downloads do not bypass those gates. The trial download command resolves
only the current design's 48 kHz one-rate ZIP and reruns the strict package validator.
The final download command resolves only the current session's last verified export,
reruns the exact-six-rate validator, and checks its recorded SHA-256. Both use a native
save destination, reject writes into other internal live-project artifacts, persist
through a same-directory temporary file, and compare the saved bytes with the generated
package. Downloading a trial cannot set hardware verification or export eligibility.

A separate synthetic structural reference must contain exactly the six expected rates,
one README, and one matching machine manifest. The validator rejects missing/duplicate
rates, unsafe or case-colliding names, paths/subdirectories, duplicate central-directory
entries, unexpected files, wrong stereo order/format, NaN/Inf, oversized inputs, and
manifest/header mismatches. Stored timestamps and entry ordering are fixed so repeated
generation is byte-identical. This reference ZIP is never evidence for a user's room.

Still-required production gates are real-hardware execution of the implemented
remeasurement/quality/headroom path, automatic acoustic channel identity, certified
physical output-level/SPL checks, and a Roon import/clipping smoke test on macOS and
Windows. Public Roon
documentation does not define every extra metadata-file behavior, so structural
validation alone cannot promote this developer-beta package to a public release.

## SECS advanced-option gates (experimental)

The SECS path (`secs-port-v1`, ported single-point full-band algorithm) keeps
the same closed-loop discipline as the main path while never claiming its
multi-seat guarantees.

- Trial: predicted-only, designed from the accepted central P0 pair in 2.0
  projects only. Verification captures bind to the trial WAV SHA-256 and
  require the user's declared-active attestation, exactly like Phase 4.
- Group-delay gate (design time, from the designed 48 kHz taps alone): band
  medians of per-bin group delay (magnitude-weighted, re the filter's own
  1-16 kHz baseline, worst of L/R) must stay within 30 ms at 20-100 Hz,
  15 ms at 100-300 Hz, and 8 ms at 300-1000 Hz. Hard gate with the
  default-on improved option (`+phase-guard-v1`); warning-only when the
  original algorithm is selected, since the preserved original must remain
  runnable for comparison. An all-pass timing defect changes no magnitude,
  so no other gate in the pipeline can see it - a real 2026-08-03 filter
  shipped 70-200 ms of low-band delay with every magnitude number green.
- Closed loop (`secs-port-v1+live-closed-loop-validation-v4`): judged on
  1/12-octave gate-smoothed curves as the RMS of per-octave-cell RMSE against
  the SECS target over 20-500 Hz, with the v4 dual branch (verified beats raw,
  or delivers the predicted figure). Smoothed predicted-versus-verified
  agreement stays capped at 3 dB over 20-650 Hz, and the applied-correction
  scale must fit inside [0.6, 1.4]. The session-gain gate is response-based
  rather than marker-based: because the SECS filter is full-band, the trial
  itself shifts the captured >=650 Hz marker level by its own response
  (measured -1.2 dB live), so the marker comparison the Phase 4 loop uses
  would false-positive on every SECS verification. Instead the session level
  shift is the median over 650 Hz - 20 kHz of (verified - baseline - filter),
  which cancels the filter's contribution exactly and stays robust to
  microphone-reposition combing; the +-0.5 dB allowance is unchanged.
  The validator's bounded-correction extrema gates see the FILTER's response
  (the Phase 4 convention; a 2026-07-30 live session found the port passing
  the absolute predicted response there, which fired ExcessiveAttenuation on
  every real room). Of those two gates, ExcessiveAttenuation (12.05 dB) is
  exempted for SECS - it encodes the Phase 4 bounded-gain contract, while
  SECS cuts are unbounded by design and their justification is judged by the
  measured improvement, agreement, and applied-scale gates instead.
  PositiveCorrectionGain (+3 dB) stays live: the SECS peak normalization
  keeps the filter at or below 0 dB, so any positive gain means the
  normalization broke. The e2e fixture is anchored +25 dB outside the
  bounded-correction window so a recurrence of the absolute-response wiring
  trips the non-exempted gate. Frequencies above the judged band are deliberately excluded:
  centimeter-scale microphone repositioning between captures dominates them,
  so a full-band comparison would demand a reproduction the physics cannot
  deliver. Correctness above the judged band rests on the SHA-bound filter,
  the applied-scale fit, and listening.
- Export (`secs-native-rerun-v3-resampled-fir`): only the 48 kHz member is
  designed - it must remain byte-identical to the verified trial WAV - and
  every other member is that finished FIR resampled with the resampler
  pinned to scipy `resample_poly` by the parity fixture, so all six rates
  carry one transfer function by construction. History: v1 let each rate
  re-fit its own excess phase - below ~100 Hz that is modal noise and grids
  7% apart fit it differently, shipping members ~35 ms apart in 20-100 Hz
  group delay. v2 transplanted the 48 kHz corrector by phase interpolation
  but still re-ran the pre-ring windowing, the phase guard, and the
  minimum-phase magnitude EQ per grid; near a razor-sharp modal cut the
  assembled response passes close to zero and the winding count of the
  realized phase proved grid-dependent - a real 2026-08-14 package's 44.1
  kHz family sat 33.6 ms from the verified member with the transplant
  working as designed. Each member's regression 20-100 Hz group delay
  (`secs_band_group_delay_fit_ms`) is still recorded in the manifest, and
  the export still fails closed if any member sits more than 5 ms from the
  48 kHz reference - under v3 that gate verifies resampler fidelity
  (measured 0.06 ms worst on the failing real package) instead of policing
  independent designs. Members need no level alignment - one filter is one level (v2's per-rate
  alignment, applied to identical members, would only chase comparison-grid
  aliasing of narrow modes);
  their smoothed 20-500 Hz agreement with the verified response is recorded
  as a diagnostic with only a 6 dB gross-corruption backstop, because the
  measured gap between legitimate narrow-mode grid variation (1.20 dB on the
  synthetic fixture) and a simulated rate-wiring bug (1.42 dB) is too small
  for any spectral threshold to separate - that class is covered by the
  parity fixtures and structural assertions instead.
- Headroom (`validation-signal-and-response-peak-v3+program-peak-v2`): the
  v3 sweep/response basis structurally under-recommends for a full-band
  mixed-phase filter - a sweep is a single frequency at every instant and
  the peak-normalized filter has a ~0 dB response peak, yet the 2026-07-30
  live package grew real program peaks by +3..+6.4 dB and clipped at the
  v3-recommended 1.3 dB. SECS packages therefore also convolve every rate
  member with deterministic full-scale program proxies and take the worst
  peak growth as an additional basis term. v2 (2026-08-04): the square-wave
  proxy is a SWEPT scan (1/48-octave over 20-320 Hz with 1/192-octave
  refinement around the top readings) instead of a fixed tone grid, because
  the growth-versus-frequency continuum of a real room filter is sharp
  (7.9 dB at 70.0 Hz vs 6.5 dB at 71.3 Hz, high-Q correction structure) and
  the fixed v1 grid (40/60/90/150 Hz) read 7.28 dB on a package whose
  70.5 Hz square measured 8.6 dB - an ordinary clipped bass note past the
  whole recommendation. The scan was validated against a full 1/96-octave
  sweep (it lands at or above it; the peak is onset-transient dominated,
  overshooting steady state by 3-6 dB, so each tone is convolved rather
  than evaluated analytically). Kick bursts and hard-clipped fixed-seed
  noise stay as transient/dense-master proxies. The L1 absolute-safe bound
  is unchanged.
- 2.1 shared sub band: with a confirmed crossover the design commonizes the
  L/R magnitude and excess-phase corrections below it (full weight up to the
  crossover, fading over the half octave above). Rationale: one shared
  subwoofer reproduces both channels there, so a measured L/R split is noise
  on the same path - and a phase split would thin mono bass where bass
  management sums the channels, which per-channel verification sweeps can
  never observe. Pinned by a dsp-core test that injects a left-only 45 Hz
  mode and asserts the commonized filters collapse the sub-band split.
- Verification skip (user opt-in, SECS only): the user may export a final
  package without the closed loop. The package is then labeled
  predicted-only everywhere - file name (`_secs_predicted_`), README,
  `verification_state: predicted-only-verification-skipped-by-user`, and a
  `null` verification record - and is never presented as a verified
  correction. The Phase 4 export has no skip; measured verification is its
  core promise.
