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

An experimental **SECS advanced option** (an open-source single-point full-band
algorithm ported with its author's permission, `secs-port-v1`) can design a
full-band mixed-phase trial from the accepted central P0 pair, deliberately
bypassing the multi-seat safety design. The SECS.py control set (boost
ceiling, tilt, bass shelf, resolution, curtain, latency mode, fixed or
automatic delay) is exposed in the advanced options, range-validated by the
backend, and the exact settings are stored and reused by the final export.
A multi-position option (on by default, an extension over SECS.py) feeds the
magnitude side a seat-weighted RMS average of every accepted baseline seat -
the same spatial statistic as the Phase 4 design - while the excess-phase
correction and its confidence guard stay strictly on the P0 pair; with only
P0 accepted it falls back to the plain single-point path.
A target-curve option (another extension; on by default in the UI) overlays
the app's selected target curve - B&K-style, Harman-style, or the imported
custom curve, re-anchored to 0 dB at 500 Hz - onto the SECS adaptive target,
so the correction steers toward the same house curve the Phase 4 design uses
instead of in-room flat; unchecked reproduces the SECS-native flat target,
and the final export reuses the stored copy of the resolved curve.
2.1 projects are supported: bass management is linear, so correcting each
channel's combined main+sub capture independently is exact at the measured
seat. Below the confirmed crossover - where one shared subwoofer reproduces
both channels and a measured L/R difference is noise on the same path (an
11.8 dB spurious split was measured at 37 Hz on the first live 2.1 package) -
the magnitude AND excess-phase corrections are commonized across L/R, fading
out half an octave above the crossover; this is the SECS analog of Phase 4's
common-low-bass constraint and also keeps the filters phase-matched where
bass management sums the channels into the subwoofer. It follows the same
closed-loop discipline as the main path: the trial is predicted-only until the
user declares it active in Roon and a new P0 L/R capture passes the smoothed
20-650 Hz judgment (improvement or delivered prediction, 3 dB agreement,
applied-scale [0.6, 1.4], session-gain gate; Phase 4's 12.05 dB attenuation
ceiling is documented as not applied because SECS cuts are unbounded by
design, while the +3 dB positive-gain gate stays live to guard the peak
normalization); high frequencies are excluded
from the judgment because centimeter-scale microphone repositioning dominates
them. Final export then re-runs SECS per native rate on the P0 impulse
resampled with a scipy-`resample_poly`-parity resampler, locks the verified
delay, byte-binds the 48 kHz member to the verified trial WAV, level-aligns
the other rates to it (recorded per rate, charged to headroom), and records
per-rate smoothed agreement diagnostics. The SECS headroom adds a
program-material peak-growth basis to v3 (full-scale square/kick/clipped-noise
proxies convolved through every rate member), because the sweep basis
under-recommended by ~5 dB on the first real package and playback clipped. A
passing verification renders the same measured before/after chart as the
Phase 4 path; before any verification the design already renders a
predicted-only chart (raw/target/predicted, with the verified curve hidden
rather than faked). The user may explicitly skip verification and export a
predicted-only package; it is labeled as such in the file name, README, and
project record, and is never presented as verified. Numerical parity with the
Python reference is pinned by `crates/dsp-core/tests/secs_parity.rs` against
`testdata/secs-parity.json`. The first real microphone/Roon run passed the
SECS closed loop and produced a verified package on 2026-07-30 (one room, one
system); broader listening evaluation is ongoing.

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
