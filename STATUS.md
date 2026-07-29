*English · [한국어](STATUS_ko.md)*

# Implementation status

Last updated: 2026-07-29.

Legend: **implemented** means real code with a passing deterministic check; **partial**
means a usable subset with the missing boundary stated; **not implemented** means
intentionally absent rather than hidden behind a mock.

## Stage matrix

| Stage | State | What exists, and where it stops |
| --- | --- | --- |
| 0 — skeleton | Implemented | Tauri 2 / React / TypeScript shell, Rust workspace, DSP library, CLI harness, CI definition. A macOS `.app` developer bundle builds locally. No hosted CI run has happened yet. |
| 1 — offline DSP core | Partial | Deterministic synthetic six-seat fixture → analysis → target → spatially gated `[-12,+3] dB` minimum-phase stereo FIR → predicted validation → float32 WAV/ZIP. Arbitrary user WAV/CSV project import is not implemented. |
| 2 — measurement wizard | Partial, capture path works | CPAL bounded 48 kHz input-only capture with explicit native channel selection; the app never opens an output device. The wizard imports a covering UMIK calibration and separate L/R sweep WAVs, reports main/start/end channel routing, meters the sweep, and persists immutable raw WAV/JSON evidence. `known-marker-capture-endpoint-v4` tolerates unequal reverberant marker candidates while still validating strict pair spacing. `known-sweep-deconvolution-v5` keeps a fixed signed 4,800-sample pre-zero window without peak alignment and retains 32,768 samples (0.68 s) analyzed on a 65,536-point FFT (0.73 Hz grid), so a narrow low-frequency mode is no longer underestimated between bins. Caches from v3 and earlier do not restore. Certified SPL, hardware volume control, acoustic channel-swap detection, and transient/microphone-movement classification are absent. |
| 3 — single-sub integration | Partial, measured search implemented | A 2.1 project records 2–12 physically configured crossover states and guides accepted P0 main-only and sub-only captures. The core synthesizes a bounded main-delay grid and both polarities from calibrated complex paths; it does not invent crossover transfer functions or search sub level. The ranking is explicitly predicted-only and the winner must be entered on real hardware before new combined captures unlock the next stage. Sub-level search, phase-knob search, and multi-seat candidate scoring are absent. |
| 4 — closed-loop verification | Partial, live gate implemented | The live adapter reuses the bounded-gain / minimum-phase / protected-dip / spatial design with B&K-style, Harman-style, or imported custom targets, emits a predicted-only trial ZIP, and requires new accepted P0 L/R captures made after the user declares that exact trial active in Roon. `live-closed-loop-validation-v4` judges improvement on 1/12-octave-smoothed curves scored as the RMS of per-octave-cell RMSE, keeps a ≤3 dB smoothed predicted-versus-verified agreement gate over 20–650 Hz, fits an applied-correction scale to catch an unloaded or doubled filter, and retains the unsmoothed residual as a diagnostic. A failure guides redesign and reverification instead of shipping. Automatic L/R swap detection is absent. |
| 5 — Roon package | Partial, verified-live export implemented | A project whose closed loop passes can run the native-rate redesign, response-bind its final 48 kHz member to the verified trial (0.05 dB / 0.02 ms), write and read back the six-rate minimum-phase Roon ZIP, persist a final project record and hash, and calculate bounded headroom. Trial and final stages expose native save dialogs that revalidate and byte-check only the current session's package. Windows execution, installer validation, Developer ID signing, and notarization are absent. |

## Verification actually run

Deterministic suites, on macOS, at the commit that opens this beta:

- `cargo test --workspace` — 178 passed, 3 hardware-dependent tests ignored, 0 failed.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all --check` — clean.
- `npm test --prefix apps/desktop` — 13 files, 36 tests passed.
- `tsc --noEmit` — clean.

Tests that need the developer's local measurement fixtures print `SKIPPED:` and return
when those files are absent, so a clean clone runs green while a local run exercises
more.

The synthetic 48 kHz regression currently reports:

| Metric | Raw | Predicted |
| --- | ---: | ---: |
| Left 20–500 Hz RMSE | 1.714 dB | 1.214 dB |
| Left broad-peak RMSE | 2.225 dB | 0.518 dB |
| Right 20–500 Hz RMSE | 1.852 dB | 1.609 dB |
| Right broad-peak RMSE | 2.187 dB | 0.271 dB |

Maximum realized filter gain is about `-0.0 dB` left and `+2.166 dB` right; the latter
is the synthetic fixture's deliberate broad shallow shared deficit exercising the
admitted-boost path, below the +3 dB ceiling. These are synthetic regression figures,
not expected performance in a real room.

## Real-hardware evidence

One system, one developer: KEF R3 Meta + KC62, WiiM Amp Ultra, UMIK-1, Roon. That
system has completed real capture, separated-path subwoofer search, trial generation,
Roon convolution, and closed-loop verification. Several constants are calibrated on
that single room:

- The isolated-capture leakage gate's 20 dB band-SNR qualification (measured 9.9/10.9 dB
  with the subwoofer physically switched off versus 32.1/33.1 dB with it live).
- The 4,800-sample deconvolution pre-zero window (central repeat agreement moved from
  1.3814 ms to 0.2394 ms against an unchanged 0.5 ms criterion).
- The per-octave-cell improvement gate, adopted after a filter that measurably worked
  failed the previous whole-band unsmoothed judgment.

A threshold justified by one room is a hypothesis, not a constant. These are the
numbers most worth challenging.

## Current risks

- The two target presets are independent v1 product curves with no listening
  validation. They are deliberately named `-style`, not official curves.
- Synthetic impulse responses validate deterministic DSP behaviour, not loudspeaker
  nonlinearity, noise, microphone clock drift, or real-room repeatability.
- The asynchronous single-sweep path has no repeated L/R arrival evidence. Marker clock
  mapping and the intra-sweep ppm diagnostic must never drive inter-channel timing, and
  the code is structured so that they cannot.
- The subwoofer separated-path search is predicted-only. Its arrival estimator has a
  stated uncertainty (roughly ±2.75 ms on the one real run) and its recommendation is
  not proven until a new combined measurement confirms it.
- Recommended headroom exists only for a passing live session; no fixed guess is
  substituted anywhere.
- One common attenuation-only safety normalization is recorded across channels and
  rates. Real clipping behaviour under Roon playback has not been stress-tested.
- Roon's public documentation describes 32-bit WAVs in a ZIP but does not separately
  promise IEEE-float WAVs or extra `README.txt` / `manifest.json` entries. The package
  parser is strict, and a real Roon import on someone else's system is still a gate.
- Only macOS has ever run this. CI defines macOS and Windows; no Windows host has
  executed it.
- The macOS `.app` is ad-hoc signed only — not Developer ID signed, not notarized.
- Dependency licenses are all present in Cargo metadata, but a packaged third-party
  notice bundle and a real distribution review have not been done.
- Device-discovery and capture smoke tests stay ignored in unattended CI because they
  need a physical microphone and OS permission. Hot-unplug handling is unverified.

## Next concrete goals

1. Get the app run end to end in a room that is not the author's, on hardware that is
   not the author's, and find out which gates are wrong.
2. A real Roon import test on another machine, including Windows.
3. Replace single-room threshold calibration with evidence from several rooms.
4. Developer ID signing and notarization before anything resembling a public release.
