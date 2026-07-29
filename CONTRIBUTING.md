# Contributing

This project is one hobbyist's attempt at a room-correction tool that refuses to
overclaim. It is published to be corrected. Blunt technical criticism is welcome and
more useful than encouragement.

## What helps most

**Acoustics and DSP review.** [docs/dsp.md](docs/dsp.md) states every design rule and
why it exists. If a bound is wrong, arbitrary, or hiding an assumption, saying so — with
the reasoning — is the single most valuable contribution.

**Challenging the thresholds.** [docs/validation.md](docs/validation.md) lists each gate
and the evidence behind it. Several constants were calibrated on exactly one room. They
are hypotheses. Numbers that fail elsewhere are what this beta is for.

**Running it in your room.** A failed session is a useful report. Everything needed to
diagnose one is in the session directory (see below).

**Code review.** The safety invariants below are enforced by structure, not by
convention. Code that can violate one is a bug even when the tests pass.

## Safety invariants

These are deliberate product rules, not implementation accidents:

1. A predicted result is never presented as a verified one. Only a real remeasurement
   through the real signal path can set a verified state.
2. Nothing is called "correction complete" without a post-filter remeasurement.
3. Correction is cut-first: boost is bounded at +3 dB, requires multi-position support,
   and is never applied to deep or narrow dips.
4. Attenuation never exceeds 12 dB.
5. The design path is minimum-phase only.
6. A failed capture is retained for diagnosis but can never replace the last accepted
   value for that measurement cell.
7. Export stays locked until the closed loop passes on the exact filter that was
   declared active.

## Reporting a measurement problem

Open an issue with:

- What you measured (system, room, microphone, seat positions).
- What the app decided, and what you expected instead.
- The per-capture JSON and project record from
  `live-projects/<session-id>/` in the platform application-data directory.

Raw capture WAVs are large; attach them only when the problem is in capture or
deconvolution. Never attach anything you would not want public.

Issues in Korean are welcome — the maintainer is a Korean speaker, and the application
ships in Korean and English.

## Working on the code

```sh
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
npm ci --prefix apps/desktop
npm test --prefix apps/desktop
```

Notes:

- Some tests need large local measurement fixtures that are not in this repository.
  They print a `SKIPPED:` line and return, so a clean clone runs green.
- Every user-facing string exists in Korean and English under
  `apps/desktop/src/i18n/`. Changing one locale without the other fails the tests, and
  that is deliberate.
- New behaviour that touches a validation gate should come with the evidence that
  justifies the number, not only a passing test.

## Building installers

Each platform builds on its own machine — Tauri's Windows bundle needs the MSVC
linker, WebView2 and WiX/NSIS, none of which cross-compile from macOS.

```sh
npm run tauri --prefix apps/desktop -- build --bundles dmg
```

```sh
npm run tauri --prefix apps/desktop -- build --bundles msi,nsis
```

The first command is macOS, the second Windows. To get both without owning both
machines, run the `release` workflow from the Actions tab, or push a `v*` tag; it
builds each platform on its own runner and uploads the installers.

**Nothing produced by any of these is signed.** The macOS app is ad-hoc signed at best,
so Gatekeeper blocks it after a download, and Windows SmartScreen warns about the
installer. That is accurate — this is an unsigned beta — and it is the state until
Developer ID signing and notarization are in place.

## The one rule

Never let a predicted result be presented as a verified one. The entire structure of
this project — separate types, separate states, separate gates — exists to make that
mistake hard. A change that makes it easier will be rejected regardless of how well it
performs.
