# Architecture

Primary dependency/source references were checked on 2026-07-24; the live
implementation boundary was updated on 2026-07-26. This document records product
decisions; `STATUS.md` records which boundaries are implemented today.

## Decision summary

| Concern | Decision | Pinned/current version | Reason |
| --- | --- | --- | --- |
| Desktop shell | Tauri 2 | Rust crate 2.11.5, CLI 2.11.4 | Native system webview, small bundle, Rust command boundary, Windows/macOS support |
| UI | React + TypeScript + Vite | Exact versions in `apps/desktop/package-lock.json` | Accessible component state and a portable SVG response plot without a heavy chart runtime |
| DSP | Pure Rust workspace library | `eqforbeginner-dsp-core` 0.1.0 | Deterministic, offline, testable without UI or audio hardware |
| Test harness | Rust CLI | `eqforbeginner-cli` 0.1.0 | Produces repeatable fixtures, reports, WAV, and export checks |
| Audio I/O (Phase 2) | CPAL | 0.18.1, MSRV 1.85 | Host device IDs/configuration discovery plus bounded native-48-kHz PCM capture with explicit input-channel extraction, CoreAudio and WASAPI defaults; Apache-2.0 |
| Advanced wireless measurement transport | User-started local WAV playback in Roon; microphone-side recognition/deconvolution | `wireless-sweep-recognition-v1` + `known-sweep-deconvolution-v5` | Avoids unsafe remote transport/volume changes; arbitrary network latency becomes a searched capture offset, while a fixed signed pre-zero window preserves paths arriving before the acoustic marker reference |
| Microphone calibration | Strict local UMIK-style TXT | `umik-calibration-parser-v2` | Auditable log-frequency/linear-dB interpolation, including quoted miniDSP metadata, without network or filename assumptions |
| Live session adapter | Tauri-owned immutable local evidence | `similarrew-live-project-v5` (historic on-disk format id, kept through the rename) | Keeps device/state/files outside the pure DSP core while binding every capture to the 2.0/2.1 declaration, separated-path crossover plan, predicted single-sub ranking, confirmed manual hardware setting, sweep/calibration hashes, and selected native input channel; records uploaded-WAV marker routing, automatic completion, and sweep-level evidence |
| Existing REW-data bridge | Development-only conversion to versioned JSON | Source REW 5.31.3; preferred REW 5.40+ local API | Keeps private `.mdat` serialization and Python out of the product runtime |
| Reference sample rate | 48 kHz | Product default | Matches the offline trial and eventual first closed-loop validation format |
| EQforBeginner Roon candidate format | Interleaved stereo IEEE float32 WAV in ZIP | WAV metadata selects layout/rate | Direct stereo needs no `.cfg`; IEEE-float acceptance remains a real Roon smoke-test gate |

Tauri's components have independent release numbers. Do not force the Rust crate,
CLI, and JavaScript API to share a number. The lockfiles are the reproducible source
of truth.

Sources:

- [Tauri release index](https://v2.tauri.app/release/)
- [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)
- [Tauri project structure](https://v2.tauri.app/start/project-structure/)
- [Tauri application icons](https://v2.tauri.app/develop/icons/)
- [Tauri distribution overview](https://v2.tauri.app/distribute/)
- [Tauri macOS native `Info.plist` configuration](https://v2.tauri.app/distribute/macos-application-bundle/)
- [Apple `NSMicrophoneUsageDescription`](https://developer.apple.com/documentation/BundleResources/Information-Property-List/NSMicrophoneUsageDescription)
- [Microsoft Windows microphone permissions](https://support.microsoft.com/en-us/windows/privacy/turn-on-app-permissions-for-your-microphone-in-windows)
- [CPAL 0.18.1 API and platform notes](https://docs.rs/cpal/0.18.1/cpal/)
- [CPAL DeviceTrait IDs and configurations](https://docs.rs/cpal/0.18.1/cpal/traits/trait.DeviceTrait.html)
- [REW local HTTP API](https://www.roomeqwizard.com/help/help_en-GB/html/api.html)
- [REW beta release history](https://www.roomeqwizard.com/beta.html)
- [Roon MUSE Convolution](https://help.roonlabs.com/portal/en/kb/articles/dsp-engine-convolution)
- [Roon Headroom Management](https://help.roonlabs.com/portal/en/kb/articles/dsp-engine-headroom-management)
- [Roon-supported audio formats](https://help.roonlabs.com/portal/en/kb/articles/faq-what-audio-file-formats-does-roon-support)
- [Adding local music to Roon](https://help.roonlabs.com/portal/en/kb/articles/adding-local-music-to-roon)
- [Roon Volume Leveling](https://help.roonlabs.com/portal/en/kb/articles/volume-leveling)
- [Roon Signal Path](https://help.roonlabs.com/portal/en/kb/articles/signal-path)
- [Roon Extension API](https://github.com/RoonLabs/node-roon-api)
- [Gamper 2017 asynchronous impulse-response measurement](https://www.microsoft.com/en-us/research/wp-content/uploads/2017/03/Clock_drift_estimation_HSCMA_2017.pdf)
- [Microsoft asynchronous IR reference implementation (MIT)](https://github.com/microsoft/Asynchronous_impulse_response_measurement)
- [ITU-R BS.1116-3 listening-room and stereo-reference conditions](https://www.itu.int/dms_pubrec/itu-r/rec/bs/R-REC-BS.1116-3-201502-I%21%21PDF-E.pdf)

## Process boundaries

```text
React wizard
  -> narrow typed Tauri commands
    -> project/session orchestration (offline diagnostics + live developer beta)
      -> audio_io          (discovery + selected-channel mono capture; no mock/fallback)
      -> measurement       (known-WAV recognition + calibrated IR/FR extraction)
      -> calibration       (strict UMIK TXT parser/interpolator)
      -> dsp-core          (Phase 1, Phase 3 ranking, Phase 4 replay implemented)
          analysis -> spatial -> target -> correction -> FIR -> validation
          sub_integration -> measured candidate ranking + separated-path check
          phase4 -> measured-response design + predicted-only numerical gates
          phase6 -> six native grids + cross-rate response validation
      -> live adapter      (evidence admission, persistence, closed-loop gate, headroom)
      -> export            (trial/final WAV/ZIP writer + strict package readback)
      -> download adapter  (current-artifact selection + native save dialog + byte check)
```

The DSP library owns no windows, device handles, global state, network clients, or
wall-clock decisions. Identical arrays and settings must yield identical results.
The UI never interprets raw FFT bins itself. The live Tauri session service owns
application-data directories, hash-identified calibration/sweep inputs, no-overwrite
raw capture WAVs, measurement snapshots, trial/final artifacts, and state invalidation.
It is developer-beta persistence rather than a full reopen/migration UI: restarting
the app does not reopen an earlier live session. The wizard can, however, explicitly
restore the newest accepted capture for each measurement kind/position/channel key
across prior projects whose system mode, device/channel, calibration and sweep hashes,
manual subwoofer state, and DSP versions all match. Projects are scanned newest-first,
so an older project only fills a key absent from newer compatible projects.

## Rust module boundaries and current state

- `audio_io`: host-qualified device ID/metadata and supported 48 kHz configuration
  discovery are implemented with CPAL. The selected exact ID can be opened when it
  exposes a native 48 kHz PCM configuration containing the selected input channel.
  Interleaved callbacks are frame-counted and only that channel is extracted into an
  explicitly bounded mono buffer with cancellation, peak/clip, xrun, callback-lock
  loss, timestamp-gap, missing-tail, and runtime-error evidence. The callback waits
  for the monitor's bounded tail memcpy instead of silently dropping a complete PCM
  block on lock overlap. A timestamp gap/regression is retained as a diagnostic but
  is not itself called PCM loss; xrun, missing frames, callback loss, format error,
  and runtime stream errors remain hard failures. A caller may supply a
  data-driven completion detector. It receives only bounded overlapping tail snapshots
  and their absolute offset, so marker correlation runs outside the callback lock
  without cloning the complete growing capture. There is no downmix, fallback, or fake
  provider. Output playback, duplex synchronization, and arbitrary routing or mixing
  maps remain unimplemented.
- `measurement` (partial): an imported 48 kHz WAV is decoded and SHA-256 identified,
  then `wireless-sweep-recognition-v1` searches a real mono capture using block-FFT
  zero-mean correlation and independent segment checks. `known-sweep-deconvolution-v5`
  resamples a recognized segment only from qualifying repeated-marker evidence,
  performs regularized spectral division, applies microphone magnitude calibration,
  retains a 32,768-sample raw/calibrated IR pair with a 65,536-point (0.73 Hz)
  analysis FR, and reports SNR/reconstruction/clip/timing quality.
  Marker-referenced captures use a fixed 4,800-sample pre-zero window rather than
  peak alignment, preserving signed main/sub arrival differences relative to the
  right-speaker timing marker. When both markers supply independent extent and clock
  evidence, waveform reconstruction fit remains recorded but is not an admission gate;
  the markerless fallback keeps the 12 dB gate.
  Upload analysis separates the main region and two marker regions, measures L/R energy
  and correlation in each marker, reports their dominant playback channel and uses
  that channel's waveform as the recognition template. The live endpoint recognizes
  the before marker, meters the main sweep, retains unequal repeated-marker candidates,
  validates their pair spacing, and ends on the complete after marker without a fixed
  extra delay; full-sweep recognition is the fallback. Sweep generation,
  certified absolute SPL, acoustic channel-swap detection, and robust
  transient/microphone-movement classification remain unimplemented. A UMIK Sens
  Factor can provide an explicitly assumed SPL estimate, while dBFS/clipping remain
  the fail-closed gates.
- `calibration`: `umik-calibration-parser-v2` parses two/three-column UMIK-style text,
  retains serial/sensitivity/phase provenance, validates 20 Hz-20 kHz coverage, and
  interpolates magnitude correction linearly in dB on log frequency. V1 does not apply
  the optional phase column; Sens Factor does not alter IR/FR but feeds the
  assumption-labeled level estimate.
- `analysis`: FFT response, phase/group-delay products and smoothing.
- `spatial`: weighted energy mean and robust spread across retained positions.
- `target`: versioned style presets and user text target parsing.
- `correction`: protected-dip and spatially robust bounded magnitude request; only
  broad shallow deficits repeated across positions can receive up to +3 dB.
- `fir`: minimum-phase spectral factorization and causal FIR extraction.
- `stereo`: common low-bass correction with a smooth channel-policy transition.
- `validation`: numerical invariants and response-domain prediction metrics.
- `phase4`: 48 kHz measured-response replay, safe redesign, minimum-phase FIR
  synthesis, protected-dip checks, and timeline-preserving IR convolution diagnostics.
  Its result type can express only `predicted-only-measured`; it cannot accept a caller
  boolean that promotes a prediction to hardware verification. The Tauri live adapter
  separately feeds real accepted response sets into this same function and owns the
  post-trial closed-loop evidence state.
- `phase6`: redesigns one physical-frequency bounded-gain intent at all six Roon rates,
  aligns attenuation-only safety normalization, and validates realized magnitude and
  relative group delay independently from export eligibility.
- `sub_integration`: pure-Rust measured-candidate model, deterministic response score
  for candidates whose declared settings may include crossover/delay/polarity/level,
  plus bounded complex synthesis of main-delay and 0/180-degree polarity alternatives
  from physically measured main-only/sub-only paths at each crossover,
  missing-evidence handling, and a separated main/sub complex-sum diagnostic are
  implemented. It does not generate unmeasured settings. Hardware-capability
  collection, guided one-change-at-a-time capture, multi-position acquisition, and
  final confirmation measurement remain unimplemented.
- `export`: WAV/package generation and structural validation.
- `project` (partial): the CLI/fixture diagnostics persist versioned derived JSON.
  `live_measurement.rs` now persists immutable inputs, raw captures, per-measurement
  snapshots, custom target TXT/hash/parser/range metadata, selected target identity,
  final evidence, and artifact hashes below the platform application-data directory.
  `accepted-measurement-snapshot-cache-v3` can restore the newest compatible accepted
  snapshot independently for every measurement key on explicit user request, and a
  failed retry cannot evict the last accepted value for that measurement key. Full
  session reopen/migration, a user-facing exclusion history editor, and cleanup/export
  management are not implemented.

### Reuse boundary for the live developer beta

The live path intentionally contains no second correction implementation:

- reused correction and export pipeline: CPAL capture and telemetry, wireless
  recognizer, Phase 4 energy statistics/targets/bounded protected-dip objective/stereo
  blend/minimum-phase FIR/prediction, Phase 6 native-rate synthesis/cross-rate
  gates, WAV/ZIP generation/readback, and a native save-dialog download boundary that
  admits only the current session's trial or last verified final ZIP;
- new evidence/adaptation code: UMIK parsing/interpolation, known-sweep
  deconvolution/quality, optional before/after marker clock mapping, immutable session
  persistence, conversion of accepted L/R pairs into the Phase 4 response model,
  microphone/evidence-generation locking, bounded complete-segment marker admission
  followed by strict two-marker spacing validation, hand-repositioned P0 ending-repeat
  gross-stability checking, persisted manual
  trial activation attestation, post-trial P0 closed-loop comparison, verified-trial
  to native-48k response binding, conservative signal/FIR-bound headroom,
  `measured-fr-result-plot-v2` serialization of the admitted Raw/Target/Predicted/
  Verified arrays onto a bounded log grid with display-only 1/12-octave smoothing,
  and the dynamic six-stage stereo/seven-stage 2.1 live GUI. The former global
  Advanced settings page is not mounted.

The new code changes how real evidence enters and authorizes existing DSP; it does not
replace the algorithms previously exercised by the MDAT-derived fixtures. The one
design-grid adaptation is explicit: the live Phase 4 trial uses the existing Phase 6
48 kHz native length of 16,320 samples so the declared-and-remeasured trial can be
response-bound to the final package. The offline Phase 4 default stays at 16,384.

## Signal and timing model

Sample data uses normalized `f32` at device boundaries and `f64` for offline
analysis/design. A measurement retains its original sample timeline. Acoustic
arrival time, optional fixed channel delay, frequency-dependent group delay, and
FIR latency are distinct fields. Phase 1 synthetic data has a declared impulse
origin; it does not claim to validate real acoustic timing.

The live desktop command uses `scan_default_host_inputs`: it asks CPAL only for input
devices and their configurations. It does not query the default output, enumerate
output devices, inspect output configurations, or open an output stream. Input
discovery exposes the host-qualified ID, host API, device metadata, channel count,
sample format, buffer range, and supported project rate explicitly.
CPAL describes device IDs as persistable where the backend permits; saved IDs are
therefore hints and must be revalidated against current metadata before stream opening.
CoreAudio is the default on macOS and WASAPI on Windows. ASIO remains opt-in because
CPAL's ASIO feature adds an SDK/toolchain and licensing/distribution review.

CPAL exposes input and output as separate streams and does not document synchronized
clocks for a playback device plus a USB microphone. Therefore, it is an explicit
product inference that callback timestamps alone are insufficient: the measurement
layer must retain its acoustic marker and estimate drift on the recorded timeline.

The advanced wireless and full live paths deliberately emit no audio. The user imports
the same WAV into EQforBeginner and Roon, explicitly arms the microphone, and starts
playback in Roon.
The app searches the capture, so Roon/RAAT buffering latency is part of the reported
offset rather than mistaken for speaker arrival. Roon has an official Extension API,
but this beta does not pair with it or control transport, Zone, queue, or volume:
EQforBeginner cannot know the physical amplifier level, and automatic sweep playback would
create an avoidable safety risk.

One sweep's changing-frequency segments provide only an effective sample-rate slope.
Room frequency-dependent delay can bias that fit, so the result is always stored as
`intra_sweep_segment_fit`. The supplied stereo sweep files additionally contain
lower-energy before/after marker events. When both are recognized, the live adapter fits

```text
capture_sample = offset + clock_ratio * reference_sample
```

from those equivalent events and uses the ratio for deconvolution clock mapping. If
the marker pair cannot be established, the adapter deliberately uses ratio 1.0 rather
than warp magnitude from the room-biased intra-sweep slope. Neither case establishes
same-position L/R arrival repeatability: every live capture remains
`timing_eligible=false`, no peak is moved to zero, and L/R delay correction stays
disabled. This follows the asynchronous-measurement approach above without treating
CPAL callback time or a single impulse maximum as an acoustic timing reference.

## Existing REW measurement boundary

The application and Rust DSP library do not parse `.mdat`. The measured Phase 3 and
Phase 4 development fixtures were produced by development-only converters using the
pinned `javaobj-py3` 0.5.0 helper in an isolated Python environment. The converters
preserve measured SPL and phase, do not move the timeline or align levels, record
source metadata, and store SHA-256 plus byte size for every source. They also preserve
REW's non-zero linear-grid origin: the first stored response sample is associated with
the recorded `startFreq` rather than an invented 0 Hz origin. The Rust CLI consumes the
versioned JSON fixtures and verifies the original files before analysis. Python and the
private Java-serialization reader are not desktop runtime or distribution dependencies.

These converted fixtures and their `.mdat` sources are one developer's personal room
measurements. They are hundreds of megabytes, are not part of the public repository,
and are not required to build, run, or test the application; the regression tests that
consume them print a `SKIPPED:` line when the directories are absent.

The preferred maintainable bridge is REW 5.40 or later's documented localhost HTTP
API. Official documentation exposes startup with `-api`, optional `-nogui`, loading
measurements, and frequency-response/phase and impulse-response endpoints on
`127.0.0.1` (default port 4735). The installed REW 5.31.3 build used for these files
reported that the API was unsupported when started with `-api`, so it was not used as
an extraction path. As checked on 2026-07-19, the official beta history lists REW
5.40 beta 130 dated 2026-07-12. Future development should prefer that documented API
over expanding the private `.mdat` reader, while the final app remains independent of
REW for its own measurements.

## Phase 4 offline response-replay boundary

`phase4-response-replay-v2` accepts six trusted 48 kHz XO90 sources: measured L+sub and
R+sub baselines, L/R main-only, and sub-only A/B. (They live under a developer-local
`measurments/phase4` directory that is not part of the public repository.) The two
combined responses are the only filter-design responses. The separated paths are an
admission diagnostic, not substitutes for combined playback-path data.

REW's stored, calibrated magnitude response is the authoritative design input. A plain
FFT of the raw REW IR does not reproduce REW's response-window and calibration
semantics, so the raw L/R combined IRs are not used to redesign magnitude. They are
convolved with the candidate FIR only to check finite time-domain behavior while
preserving each original `startTime`; their sample maxima are neither clipping nor
playback true-peak evidence.

The deterministic offline path designs a 16,384-tap stereo minimum-phase FIR on its
native 48 kHz grid. It corrects 20-500 Hz, returns to unity through 650 Hz, caps
spatially supported broad-dip boost at +3 dB, applies a typed safe redesign if the
initial cut request exceeds the attenuation limit, and publishes numerical prediction
artifacts. Admission requires unchanged
level/timeline metadata, source hashes, compatible UMIK calibration and timing
metadata, and all four 45-180 Hz separated-path complex-sum checks at or below 1.0 dB
RMSE.

This boundary is intentionally one-way: its only verification state is
`predicted-only-measured`. There is no post-FIR playback-path capture, same-setting
combined repeat, or multi-position confirmation in the fixture. Consequently
`hardware_verification=unverified`, `closed_loop_passed=null`, `export_eligible=false`,
and `recommended_headroom_db=null`. This offline path emits a trial WAV and reports,
never a Roon ZIP. The separately implemented live session path can collect the missing
post-trial P0 evidence, but it does not mutate or promote this fixture project.
The Phase 4 project schema v2 records the trial WAV SHA-256 and byte size so a
downstream consumer cannot silently substitute a different stereo file.

## Phase 6 native-rate and evidence boundary

`phase6-native-six-rate-v2` consumes the Phase 4 physical-frequency bounded design,
not a resampled copy of its 48 kHz impulse. Before redesign, the CLI resynthesizes that
design on the legacy Phase 4 48 kHz/16,384-sample grid and requires per-tap float32
agreement within `1e-6` with the hash-linked source WAV (the current residual is below
`3e-11`). This binds `filter-design.csv` to the prior
artifact without treating a new native grid as if it should have identical samples.

The admitted intent is then synthesized independently at 44.1, 48, 88.2, 96, 176.4,
and 192 kHz. Every native grid has an exact 340 ms duration, giving the same
`50/17 Hz` bin spacing while preserving each rate's Nyquist range. The even filter
lengths are 14,994, 16,320, 29,988, 32,640, 59,976, and 65,280 samples. RustFFT handles
these non-power-of-two lengths. Phase 4 accepts even lengths >=1,024; its offline
default remains 16,384 while the live path selects the native 16,320-sample 48 kHz grid.
A common attenuation-only safety normalization is applied across L/R and all rates so
no rate gains level or receives a rate-specific broadband offset. Dense magnitude over
20 Hz–20 kHz and relative group delay over 20–650 Hz are compared with the 48 kHz
realization.

Evidence and packaging are separate. The offline measured developer preview writes six
stereo float32 engineering WAVs plus an audit project, but its Phase 4 input has no
post-FIR capture, no verified true peak, and `export_eligible=false`; it therefore
cannot create a Roon ZIP. A conspicuously synthetic reference command exercises the
exact-six-rate ZIP writer and strict parse-back validator. Those offline artifacts are
format/engineering evidence only.

The live adapter has a distinct authorization boundary. It requires accepted baseline
P0 L/R, an existing numerically passed trial, accepted post-trial P0 L/R, all existing
frequency-response validation gates, and 1/12-octave-smoothed
predicted-versus-verified RMSE <=3 dB per channel over 20-650 Hz. The unsmoothed value
is retained for diagnosis and no broadband level offset is removed. Only then does it
pass the admitted gain intent to the same Phase 6 function,
write six native WAVs, create/read back the strict ZIP, and persist its SHA-256. The
manual “exact trial active” declaration is timestamped backend evidence, not Roon
control or proof. `verified-trial-native48-response-binding-v1` also requires the final
native 48 kHz response to match the verified trial within 0.05 dB magnitude and
0.02 ms relative group delay.

`validation-signal-and-response-peak-v3` computes both the worst four-times-oversampled
output/input peak ratio for the registered L/R sweeps convolved with the 48 kHz filter
and the stereo FIR L1 worst-case sample-peak bound. It takes the larger value, floors
at 0 dB, adds 1 dB, and rounds upward by 0.1 dB. It is a conservative Roon starting
headroom, not a guarantee for arbitrary program material or the analog playback path.

## Roon packaging contract

Roon officially accepts a ZIP containing one or more impulse-response files and
selects the closest channel layout and sample rate from file metadata. If an exact
rate is absent, Roon resamples. Therefore the final package will contain native
designs at 44.1, 48, 88.2, 96, 176.4, and 192 kHz rather than resampling the 48 kHz
FIR. Stable names use `EQforBeginner_<rate>_stereo.wav`; names are our convention,
not a Roon requirement.

The product requirement also asks for `README.txt` in the ZIP. Roon's public page
does not explicitly promise how arbitrary extra files are handled or separately state
that its supported 32-bit WAV case is IEEE float. Therefore both details, plus package
loading on Roon for Mac/Windows, remain real import-test gates. A `.cfg` is omitted for
the direct stereo-WAV layout.

The offline Phase 4 trial and measured-preview WAVs are not a final package. A live
trial ZIP is also explicitly predicted-only. Only the live final ZIP created after
new P0 captures made after the exact-trial activation attestation, response binding,
and bounded headroom calculation is a developer-beta listening artifact. The app does
not query or cryptographically verify Roon state, and a real Roon import/clipping
smoke test is still required on macOS and Windows.
The trial and final wizard cards expose native save-dialog buttons. Before copying, the
backend resolves the artifact from live session state, confines it to the internal
project root, reruns the appropriate one-rate or exact-six-rate ZIP validator, and
checks the saved bytes. The frontend cannot nominate an arbitrary internal source path.

The original transparent bundle artwork is retained at
`apps/desktop/src-tauri/app-icon.png`. Tauri-generated PNG, `.icns`, and `.ico` assets
are checked in and referenced explicitly by `tauri.conf.json`; the build script does
not replace them with a placeholder. This beta includes no signing or notarization
credentials, and Windows installers still require a Windows build/test run.

## Development prerequisites

- macOS: Command Line Tools (`xcode-select --install`), Rust >= 1.85, Node LTS and npm.
- Windows 11: Microsoft C++ Build Tools with the Desktop C++ workload, WebView2,
  stable MSVC Rust >= 1.85, Node LTS and npm.

Local Phase 0 verification used macOS 26.5.2, Apple clang 21.0.0, Homebrew Rust
1.97.0, Node 26.5.0, and npm 11.17.0. Windows source validation is delegated to CI
until a Windows runner is available.

## License policy

The SECS advanced option is a port of SECS by 한플 (Hanpeul), published on the
DCInside speaker gallery (https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1).
The original author granted this project permission to modify and improve the
work and asked for a credit link; that permission is not a statement that the
upstream program carries an open-source license, and the upstream ships no
license text. `THIRD-PARTY-NOTICES.md` records the grant, the credit, and the
boundary between the original design and this project's additions. Anyone
reusing `crates/dsp-core/src/secs.rs` beyond what this project's MIT license
can convey should contact the original author.

Project source is MIT. Every runtime dependency must be compatible with commercial
desktop distribution (MIT, BSD, ISC, Zlib, or Apache-2.0 preferred). CPAL and Tauri
are MIT/Apache-family licensed; direct `base64` 0.22 fixture decoding and `sha2`
0.10.9 are MIT/Apache-2.0, and `hound` 3.5.1 WAV decoding is Apache-2.0. A few
transitive dependencies in the Tauri webview stack are MPL-2.0, which is file-scoped
copyleft and imposes obligations only on modifications to those files; none are
modified here. Strong copyleft (GPL/AGPL) libraries and proprietary SDKs are not
introduced without a separate distribution review. The development-only `.mdat`
converter and its Python environment are not included in application packages; any
future decision to distribute them requires a separate dependency and license review.
