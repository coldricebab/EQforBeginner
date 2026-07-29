# Measurement protocol

This document separates the implemented real-hardware developer beta from the remaining
production protocol. The desktop discovers only host-qualified CPAL input-device IDs
and 48 kHz configurations; it does not enumerate, select, or open a computer output
device. Saved input IDs are revalidation hints where the backend cannot guarantee
persistence. It can open the selected native-48-kHz PCM input and extract one chosen
native channel to mono, import UMIK calibration and separate L/R sweep WAVs, recognize
playback started manually in Roon, deconvolve and calibrate IR/FR, persist raw evidence,
design a trial with the existing Phase 4 engine, remeasure P0, and create a final
minimum-phase six-rate ZIP only after its closed-loop gate passes.

The app still has no output playback or Roon control. The product has no direct `.mdat`
reader. It meters the recognized sweep in dBFS and, when the calibration contains Sens
Factor, shows an assumption-labeled SPL estimate. It cannot set amplifier volume or
certify absolute SPL. Acoustic channel-swap/transient/microphone-movement tests,
hardware-qualified L/R timing, session reopen, and production hardware validation
remain incomplete.

## Safety and connection

1. Disable every existing convolution/EQ filter and confirm one active signal path.
2. In EQforBeginner, scan and select only UMIK-1 and the native input channel carrying
   its signal. CoreAudio may expose UMIK-1 as a two-channel device; start with channel
   1. Roon and the physical audio system own playback routing; EQforBeginner must not
   select or occupy an output.
3. Load the serial-matched UMIK calibration file and require 48 kHz support.
4. Keep amplifier volume and microphone gain fixed for the entire session.
5. Begin with a conservative hardware volume and watch the live sweep meter. The
   preferred digital range is peak -30 to -6 dBFS. Quieter captures down to -48 dBFS
   are admitted only when the independent >=20 dB SNR and other quality checks pass;
   values above -6 dBFS or clipping at/above -1 dBFS fail. With Sens Factor, 65-85 dB
   SPL is advisory rather than an admission gate. Stop immediately when the sound is
   uncomfortable: the estimate assumes operating-system input gain 0 dB and does not
   replace a calibrated meter.

## Real measurement developer beta

Run this only in the native Tauri app; the browser preview cannot open the microphone.

1. In **Start a project**, explicitly choose **Stereo (L/R)** or
   **2.1 (L/R + one sub)**, start the live session, and note its application-data
   output directory. The choice is stored in `project.json` and cannot be silently
   changed within that session.
2. Move to **Calibration and L/R reference sweeps**, then scan and select the exact
   UMIK 48 kHz input and native channel. The first device and input channel
   actually opened become locked to that project; start a new project rather than
   changing either one mid-session. If channel 1 is completely silent, start a new
   project and try channel 2.
3. Import the serial-matched UMIK TXT. The parser accepts
   `frequency_hz correction_db [phase_degrees]`, requires strictly increasing points
   and complete 20 Hz-20 kHz coverage, and shows the parsed serial/range. V2 accepts
   quoted miniDSP manufacturer metadata while still rejecting malformed numeric rows.
   It applies magnitude correction only to IR/FR; phase remains provenance, while Sens
   Factor feeds only the assumption-labeled SPL estimate.
4. Import `assets/sweeps/Sweep_L_20-20k_refR.wav` as left and
   `assets/sweeps/Sweep_R_20-20k_refR.wav` as right. These are 48 kHz reference files with
   a dominant measurement sweep and lower-energy timing markers. The corrected Tauri
   IPC accepts the file's raw `ArrayBuffer` and a bounded validated JSON byte-array
   fallback; the former “raw WAV byte payload” error is not an expected result. After
   upload, verify the displayed channel analysis for the main sweep, start signal, and
   end signal. The bundled `*_refR.wav` fixtures carry both markers on R even though
   their main measurement sweep is L or R respectively.
5. Add both WAVs to Roon. Use one ungrouped Zone. Disable Crossfade, Volume Leveling,
   Radio/shuffle/repeat, and clear the queue. For baseline measurement disable MUSE
   Convolution, PEQ, and sample-rate conversion, and confirm 48 kHz in Signal Path.
   Volume Leveling is doubly dangerous for the 2.1 isolated captures: it can apply a
   different LUFS gain to each sweep file, silently breaking the main-to-sub relative
   level that the crossover comparison depends on. The app cross-checks the timing
   markers' RMS across every isolated capture and refuses the search beyond a 0.3 dB
   spread.
6. Pipeline order and chain invariants (2.1): the crossover search (isolated
   captures) must be finished and its winner confirmed on the hardware **before**
   any multi-seat baseline capture, and every baseline/verification sweep is a
   full-chain measurement - playing the L sweep drives the L main plus the
   bass-managed sub together. Because the convolution filter sits upstream of
   bass management and the correction is identical on L and R below the
   crossover, adding the correction filter does not change the main/sub
   alignment chosen in the search. Changing the confirmed crossover, delay,
   polarity, or sub level afterwards invalidates every baseline and
   verification measurement (the app enforces this by dropping them), so plan
   to lock the 2.1 settings first.
7. For a 2.1 project, complete **2.1 sub integration settings** before Raw capture.
   Enter 2–12 ascending crossover values that the hardware can actually apply (3–5 is
   the recommended session size: every candidate costs three sweeps, so 12 candidates
   mean 36 captures at one microphone position), plus
   the main-relative delay, 0/180-degree polarity, and sub level that are active during
   all isolated measurements. The app rejects a one-crossover plan because it cannot
   identify a crossover preference. It also requires both uploaded sweep WAVs to carry
   their start/end markers on the same fixed L or R reference speaker.

   The delay minimum, maximum, and step describe **what the amplifier can do**, not
   what to search. The search measures where the sub actually arrives and then looks
   one crossover half-period either side of it, snapped to the step, so a wider range
   adds no candidates and only stops that window being clipped. The defaults are
   -10 ms, +25 ms, and 0.1 ms: the negative minimum is what makes "put the delay on
   the subwoofer instead" reachable at all, +25 ms covers a DSP subwoofer's processing
   latency plus placement offset (the previous 5 ms ceiling was under one half-period
   at 80 Hz), and 0.1 ms is a step real amplifiers can be set to. Narrow them only to
   match a specific amplifier's limits; if the window is clipped the result reports
   `range_limited` rather than silently searching less.
   At P0, perform L main-only, R main-only, and sub-only captures for every crossover:

   - Power the amplifier off and wait before disconnecting or reconnecting speaker
     cables. Never handle a live amplifier output.
   - For main-only, disable/disconnect the sub path; the opposite main may remain
     connected because it emits only the out-of-sweep timing marker.
   - For sub-only, play the channel opposite the fixed timing-reference speaker.
     Disconnect that channel's main while leaving the fixed-reference main connected
     for the markers. With the bundled `*_refR.wav` files, play the L WAV and retain
     the R main as the marker speaker.
   - Restore the same measured delay, polarity, sub level, amplifier volume, microphone
     gain, and P0 microphone position for every triplet.

   The app then reuses the Phase 3 scorer to rank physically measured crossover states
   and synthesized main-delay/0/180-degree polarity alternatives. It does not synthesize
   an unmeasured crossover filter and does not search sub level. Apply the displayed
   recommendation manually and confirm it. The subsequent Raw P0 L+Sub/R+Sub captures
   are mandatory combined-path evidence, not optional cleanup. Stereo projects skip
   this stage.
8. Keep crossover, delay, polarity/phase, sub level, amplifier volume, and microphone
   gain unchanged. Keep the microphone upright in the same 90-degree orientation.
   In 2.1, each L or R playback must keep the normal bass-managed sub path active, so
   the captures represent L+Sub and R+Sub rather than main-only responses.
   For each table cell, press **Wait for sweep measurement** and play the corresponding
   WAV once from its beginning. The wizard first confirms the start signal, then shows
   main-sweep level, then confirms the end signal. It stops and saves on the first
   monitor poll containing the complete end signal, without a fixed extra wait. The
   supplied WAV's gap and end marker already retain the required room-IR tail. Do not
   press **Cancel measurement** after successful
   playback; that action rejects the capture. If either marker is never confirmed,
   cancel and retry from the exact beginning of the WAV. Follow the diagram in the
   wizard: P1 is 25-30 cm left of P0, P2 is
   25-30 cm right, P3 is 20-25 cm toward the speakers, P4 is 20-25 cm behind P0, and
   P5 is 10-15 cm directly above P0. Measure P0 L/R first, then preferably P1-P5, then
   return approximately to the original listening-center area for P0_END L/R; jig-level
   replacement is not required. Stop immediately if the sweep is unexpectedly loud.
   With both central pairs present, design is blocked if either channel shifts by more
   than 1.0 dB in median 200-500 Hz level or 6.0 dB in level-removed 20-500 Hz shape
   RMSE. The shape bound only catches a grossly different path or location.
9. Retain only cells marked accepted. The app stores the raw mono WAV even when later
   recognition/deconvolution fails; an accepted measurement also receives a JSON
   snapshot with sweep/calibration hashes, IR/FR, metrics, algorithm versions, and
   issue codes. Only complete accepted L/R position pairs enter design.
10. Choose the B&K-style or Harman-style target, or import a custom TXT target with one
   `frequency_hz level_db` pair per line, and create the 48 kHz trial. A custom file is
   parsed with line-numbered errors, stored in the project, and aligned over 200-500 Hz.
   P0-only is allowed but shows a spatial-overfit warning and cannot authorize boost.
   With two or more positions, only a broad shallow deficit repeated across the
   weighted positions may receive positive correction, capped at +3 dB. Deep/narrow
   dips remain protected.
   In a 2.1 project this is stage 5.
   The trial is predicted-only. Click **Download trial ZIP** and choose a local save
   location; this packages the generated 48 kHz stereo WAV for Roon but does not count
   as verification.
11. Disable every old Roon convolution and load only the emitted trial ZIP. Confirm that
   it is active, keep every hardware/volume/gain setting fixed, check the declaration
   box, then capture new verification P0 L/R sweeps. This declaration and exact trial
   hash are persisted, but the app cannot independently inspect the Roon zone.
12. Run closed-loop validation. Both channels must pass the existing target/peak gates
   and remain within 3.0 dB RMSE of their numerical prediction. Failure keeps export
   disabled; do not work around it by copying trial files.
13. If validation passes, create the final export. The app redesigns six native rates,
    response-binds the final 48 kHz member to the verified trial at 0.05 dB/0.02 ms,
    validates the ZIP, records its SHA-256/project JSON, and reports recommended
    headroom from the larger of registered-sweep true-peak growth and the FIR L1
    worst-case sample bound, plus 1 dB.
    The final stage shows a measured result dashboard before and after export:
    20-500 Hz and full-band Raw/Target/Predicted/Verified FR, exact L/R target RMSE,
    1/12-octave-smoothed prediction-to-verification RMSE (with unsmoothed diagnostic),
    filter cut/boost and protected-dip state, position
    count, and headroom. Raw/Predicted are multipoint
    energy averages; Verified is the new filtered P0 measurement. The stage-7
    Raw/Predicted/Verified display curves use 1/12-octave Gaussian smoothing for
    readability, while Target remains exact and numerical gates remain unchanged.
    Click **Download final ZIP** and choose a local save location. The app revalidates
    the exact-six-rate package and checks the saved bytes before reporting success.
    Load only this final ZIP in Roon, apply that headroom, enable the clipping indicator,
    and reduce level further if it turns red.

The complete live session is developer-beta evidence. Human testing must still confirm
microphone permission, actual channel identity, Roon import, audible behavior, and no
clipping. A successful run does not authorize L/R delay correction.

## Legacy isolated wireless recognizer

The former global **Advanced settings** page has been removed from the product UI.
Its bounded isolated recognizer remains in source and regression tests, but it is not a
supported user workflow and no current wizard control invokes it. The full live
measurement path remains the only product measurement surface.

## Capture sequence

1. Capture P0 center L and R separately through the normal bass-management path.
2. Repeat P0 response evidence and reject unstable level or response shape. The current
   live gate does not qualify arrival timing.
3. Capture P1-P5 around the listening region using the on-screen placement guide.
4. Allow inaccessible positions to be skipped, but never invent or mirror data.
5. Repeat P0 at the end. The automatic gate invalidates excessive level or response-shape
   drift; timing and other suspected path changes remain human checks.

P0 has weight 2 and each retained surrounding position has weight 1. When P0_END is
present, P0 and P0_END each have weight 1 instead of double-counting the center. A
single-point project may produce a predicted-only trial with an overfitting warning;
it becomes final-export eligible only after separate accepted verification P0 L/R and
the live closed-loop gate.

The implemented ending-repeat stability gate compares P0 with P0_END independently
for L and R. Median level drift over 200-500 Hz must be no greater than 1.0 dB. Because
the stand is moved through the surrounding points and returned by hand, level-removed
response-shape RMSE over 20-500 Hz uses a loose 6.0 dB gross-change bound. P0_END is a
separate center-area sample, not proof of exact microphone replacement. Arrival timing
is still not qualified by this test.

## Opening the measurements in REW

Every accepted capture is also written to `live-projects/<session-id>/rew/` as a mono
48 kHz IEEE-float WAV of its calibrated impulse response, named after what was measured
and when:

```text
L+Sub P0 2026-07-29 14-30-05.wav
L XO01 2026-07-29 14-22-31.wav
Sub XO01 2026-07-29 14-24-02.wav
L+Sub P0 filtered 2026-07-29 15-10-00.wav
```

`L`/`R` is the speaker alone, `L+Sub`/`R+Sub` is that speaker with the subwoofer, `Sub`
is the subwoofer alone, and ` filtered` marks a closed-loop capture taken with the trial
filter active. In a 2.0 project the baseline is `L`/`R`, because there is no subwoofer
in the path.

Load them with REW's **File → Import Impulse Response**, or drag them onto the REW
window. REW names each imported measurement after its file, so the list reads the same
way. Once they are in REW, its own **File → Save All Measurements** writes a real
`.mdat`.

The app does not write `.mdat` itself. That format is REW's Java-serialized save file
with no documented third-party writer, and a file REW silently misread would be worse
than no file at all — so the app hands REW a format REW documents as importable and lets
REW write its own save file.

Microphone calibration is already applied to these impulses, because they are the same
data the design reads. Do not load a UMIK calibration file in REW on top of them or the
correction is applied twice.

## Timing

The supplied stereo sweeps include lower-energy events around the measurement signal.
When both equivalent events are recognized, the live adapter fits their source/capture
coordinates to estimate clock ratio for deconvolution. Otherwise it uses ratio 1 rather
than the room-biased intra-sweep slope. This does not establish L/R arrival-time
confidence. The largest IR sample is never moved to zero merely for display, and the
current beta always disables left/right delay correction. For marker-driven
deconvolution, the stored IR begins at a fixed -100 ms relative to the
marker-mapped main boundary. This preserves an L-main or sub arrival that precedes
the right-speaker marker arrival; it is not an automatic timing correction.

## Per-capture quality gates

- Calibration file present, parseable, and covering the analysis band.
- No input clip, CPAL stream error, incomplete capture, or suspected sample drop.
- Pre-sweep noise evidence and at least 20 dB capture SNR.
- Reconstruction fit remains visible but is diagnostic when a complete repeated-marker
  pair supplies independent extent/clock evidence. The markerless fallback requires
  at least 12 dB fit.
- Either a chronological start/end marker pair with each correlation at least 0.20,
  or a unique fallback full sweep with at least 0.45 correlation, plus no gross rate
  mismatch.
- Enough post-roll for the 32,768-sample deconvolution IR. This is distinct from the
  16,320-tap live trial FIR. The bundled sweeps leave 58,080 samples between the main
  sweep's end and the end marker's end, and the retained tail ends before the end
  marker even begins.

The capture card shows reconstruction fit separately from SNR and labels it as a
diagnostic for complete marker-pair captures. It does not exclude an otherwise clean
measurement merely because finite-window linear reconstruction cannot model all room
decay or loudspeaker distortion. The immutable WAV remains available for deterministic
reprocessing.

Input clipping and the -48 to -6 dBFS hard main-sweep range are automatic gates; the
preferred lower boundary remains -30 dBFS. Estimated SPL is shown when Sens Factor is
available but is not an admission gate. Output clipping, certified absolute SPL,
acoustic channel identity, transient contamination, suspicious microphone movement,
and repeat-arrival timing remain human checks.

Rejected captures remain in project history with their reason and are excluded from
all averages.

## Offline Phase 3 replay available now

The current measured fixture represents only central position P0. Its candidate set
contains L+sub and R+sub measurements with the main delay declared as 0.83 ms and the
crossover changed to 70, 80, and 90 Hz. Sub level and polarity were not recorded as
candidate variables and remain unknown. No crossover candidate has a repeat at the
same setting, so their stored arrival metadata is retained but is not promoted to a
timing-repeatability score.

The separate 80 Hz captures contain L main-only, R main-only, sub-only A/B, and the
measured L+sub and R+sub responses. They support a complex-sum consistency diagnostic;
they do not turn an unmeasured polarity state into a measured candidate. The raw L/R
A/B captures and sub-only A/B captures are retained as repeatability diagnostics, not
substituted for same-setting crossover repeats.

For a reproducible replay, run from the repository root. The `measurments/` tree is one
developer's personal room recordings; it is hundreds of megabytes and is **not** part
of the public repository, so this command only runs where those files exist locally.
The regression tests that consume them print a `SKIPPED:` line instead of failing when
the directory is absent:

```text
cargo run -p eqforbeginner-cli -- analyze-sub \
  --dataset measurments/derived/phase3-responses.json \
  --source-root measurments \
  --output <new-empty-output-directory>
```

The CLI refuses a fixture with changed source hashes, automatic level alignment, a
shifted timeline, incompatible grids, or a failed separated-path model gate. Output is
a provisional measured-candidate ranking, not a completed integration. Direct `.mdat`
conversion is a development migration step only; future conversion should use the
documented REW 5.40+ localhost API rather than make private `.mdat` parsing a product
feature.

## Offline Phase 4 measured-response replay available now

The Phase 4 fixture `measurments/derived/phase4-offline-measurements.json` is derived
from six 48 kHz XO90 captures in `measurments/phase4` (again, developer-local only):

- `L_C90_D083.mdat` and `R_C90_D083.mdat`: central P0 L+sub/R+sub baselines and
  authoritative filter-design responses;
- `L_M90.mdat` and `R_M90.mdat`: main-only separated-path evidence;
- `S90_A.mdat` and `S90_B.mdat`: repeated sub-only separated-path evidence.

The admitted hardware state is explicitly an assumption: 90 Hz crossover and 0.83 ms
main delay, with that delay treated as optimal for this beta replay. Polarity, sub
level, playback volume, and microphone gain are declared unchanged but their values
were not recorded. The assumptions are retained as `user-declared` and
`verified=false`. Sub-only warnings remain visible and those captures are never
substituted for the combined filter-design paths.

Run the offline replay into a new or empty destination. As above, these source captures
are developer-local and are not shipped in the public repository:

```text
cargo run -p eqforbeginner-cli -- verify-48k-offline \
  --dataset measurments/derived/phase4-offline-measurements.json \
  --source-root measurments/phase4 \
  --output <new-empty-output-directory>
```

Before FIR design, the CLI verifies all six source hashes and sizes, the exact response
roles and common 48 kHz grid, the UMIK calibration identity and coverage, retained
acoustic-reference metadata, unchanged SPL/timeline policy, and original L/R combined
IR timelines. It also requires each main-only + sub-A/B complex prediction to match
the corresponding measured combined response over 45-180 Hz within 1.0 dB RMSE.

The stored REW combined magnitude is the design authority. Raw combined IRs are
convolved only as a finite time-domain diagnostic and retain their original
`startTime`; their maximum sample is not clipping, playback true-peak, or headroom
evidence. Output is a 48 kHz, 16,384-tap stereo float32 trial WAV plus
design/prediction reports. It is always labeled `predicted-only-measured`. The command
deliberately creates no Roon ZIP.

### Evidence absent from the offline fixture

There is no measurement made after applying the trial FIR. There is also only P0, no
same-setting repeat of each combined response, and no actual-path clipping, sample-drop,
or channel-map check. Therefore this CLI output always keeps:

- `hardware_verification` remains `unverified` and `closed_loop_passed` remains null;
- final Roon export remains ineligible;
- recommended headroom remains unavailable rather than using a guessed constant;
- “verified,” “correction complete,” and equivalent UI labels are prohibited.

The live developer-beta panel now implements a separate evidence path for baseline,
trial, and post-trial P0 measurement. Running it does not mutate or promote this
checked-in offline fixture.

## Offline Phase 6 developer preview available now

The native-rate engine can be exercised without claiming new acoustic evidence. Run it
only from a fresh Phase 4 output. The former fixture-only desktop diagnostics panel is
not mounted in the product UI. The `examples/` tree below is a developer-local Phase 4
output directory and is not part of the public repository; substitute the output
directory of your own `verify-48k-offline` run:

```text
cargo run -p eqforbeginner-cli -- prepare-phase6-beta \
  --phase4-project examples/phase4-offline-measured/project.json \
  --design-csv examples/phase4-offline-measured/filter-design.csv \
  --phase4-wav examples/phase4-offline-measured/filter/EQforBeginner_48000_Phase4_Trial.wav \
  --output <new-empty-output-directory>
```

The command validates the legacy Phase 4 design/source binding, redesigns six native
sample rates, and writes inspectable engineering WAVs. It deliberately emits no ZIP:
the input is still predicted-only, has no FIR-applied capture, and has no true-peak
headroom result. Do not copy these preview WAVs into a hand-built Roon ZIP.

For package-parser development only, `generate-phase6-reference` creates a clearly
synthetic six-rate structural ZIP. It is not a listening filter and must not be renamed
or presented as a measured correction.

### Implemented minimum-phase first-use path

Use the real measurement developer-beta steps at the top of this document. Start with
its Phase 4 48 kHz minimum-phase trial. The adapter preserves raw and calibrated
snapshots, requires accepted post-trial P0 L/R, and only then response-binds the native
48 kHz member, calculates signal/FIR-bound headroom, and requests the final six-rate
ZIP. A minimum-phase-only package is the only design path and the only live output.

## Live single-sub optimizer boundary

The implemented live path records an immutable crossover plan, retains every accepted
isolated capture, searches a bounded delay grid and both polarities, writes a ranked
JSON report, and requires the exact recommendation to be user-confirmed on the real
hardware before Raw capture is enabled. A newly accepted isolated capture invalidates
the ranking and downstream design. A failed retry remains as a raw WAV/JSON exclusion
record but does not evict the previous accepted value.

After importing the same calibration and L/R WAVs and saving the same 2.1 search plan,
**Restore accepted measurement cache** may be used instead of repeating already passed
captures. Review the displayed source project IDs. The cache scans compatible projects
newest-first and restores the most recent accepted value for each measurement
kind/position/channel cell. An older passed value may therefore fill a cell missing
from a newer incomplete session. Exact hashes, microphone channel, crossover plan,
delay, polarity, and sub level are still required; changing any of them intentionally
yields no restored measurement.

This is still P0-only prediction. Hardware capability discovery, phase knobs other
than 0/180 degrees, sub-level search, multi-position candidate scoring, and an explicit
predicted-versus-combined acoustic model gate remain future work. The required next
Raw P0 pair proves that a real combined path was measured, but the beta does not yet
numerically compare that pair with the isolated complex-sum prediction. Therefore the
UI says “predicted recommendation,” never “subwoofer integration complete.”
