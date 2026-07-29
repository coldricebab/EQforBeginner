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

**Nothing produced by any of these is signed.** Gatekeeper blocks the macOS app after a
download and Windows SmartScreen warns about the installer. That is accurate — this is
an unsigned beta.

A plain macOS build leaves only the linker's automatic ad-hoc signature, which carries
the linker's identifier instead of the bundle's and does not pass
`codesign --verify --deep --strict`. Asking for an explicit ad-hoc identity fixes both
and embeds the entitlements:

```sh
APPLE_SIGNING_IDENTITY="-" npm run tauri --prefix apps/desktop -- build --bundles dmg
```

This is still not a distributable signature — an ad-hoc signature changes on every
rebuild, so macOS may ask for microphone permission again after each one. If a build
signed this way is ever refused the microphone, build without the variable and grant
permission to that copy instead.

### Turning signing on

Signing needs a certificate that only the project owner can obtain; there is nothing to
configure until one exists. Everything else is already in place: the build reads the
standard Tauri variables, and `src-tauri/Entitlements.plist` already requests
`com.apple.security.device.audio-input`, which the hardened runtime requires or a signed
build is refused the microphone and every capture records silence.

For macOS you need an Apple Developer Program membership and a **Developer ID
Application** certificate. A free Apple ID only issues *Apple Development*
certificates, which work on your own machines and are not accepted for distribution.
With the certificate in the login keychain:

```sh
security find-identity -v -p codesigning
```

```sh
APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
  npm run tauri --prefix apps/desktop -- build --bundles dmg
```

Notarization additionally needs `APPLE_ID`, `APPLE_TEAM_ID` and `APPLE_PASSWORD` — an
app-specific password, not the Apple ID password — or an App Store Connect API key via
`APPLE_API_KEY`, `APPLE_API_ISSUER` and `APPLE_API_KEY_PATH`. Keep all of these in your
shell or in repository secrets; never commit them.

In CI the same variables come from repository secrets (`APPLE_SIGNING_IDENTITY`,
`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD`,
`APPLE_TEAM_ID`). When they are absent the release workflow still builds and simply
produces an unsigned bundle, and it prints the resulting signature so a release is never
assumed to be signed when it is not.

Windows signing is a separate certificate — an OV or EV code-signing certificate from a
commercial CA — and is not wired up yet.

## The one rule

Never let a predicted result be presented as a verified one. The entire structure of
this project — separate types, separate states, separate gates — exists to make that
mistake hard. A change that makes it easier will be rejected regardless of how well it
performs.
