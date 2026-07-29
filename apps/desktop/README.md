# EQforBeginner desktop — Developer Beta 1

Tauri 2, React, and TypeScript desktop UI for the guided 2.0 / 2.1 room-correction workflow.

The former native fixture-only Developer Beta and global Advanced settings panels are
not mounted. The product surface is the guided live wizard; fixture pipelines remain
available through the Rust CLI and regression tests.

This is an offline developer beta, not a completed measurement build. CPAL
device/configuration discovery is real. The live wizard opens only the selected native
48 kHz microphone input, deconvolves user-started Roon sweeps, persists admitted
evidence, creates a trial, requires filtered P0 remeasurement, and can export the
verified six-rate ZIP. It emits no audio and does not control Roon.

All user-facing strings exist in Korean and English (`src/i18n/`), and both locales are
checked by the test suite. A change to one without the other fails.

## Native development run

From the repository root:

```sh
npm ci --prefix apps/desktop
npm run tauri --prefix apps/desktop -- dev
```

In the Tauri window, start a live 2.0 or 2.1 project and follow the prerequisite-gated
stages. The global isolated wireless-recognition page has been removed.

Browser-only UI iteration is available with:

```sh
npm run dev --prefix apps/desktop
```

Open `http://localhost:1420`. Native device discovery and the fixture pipeline intentionally report that Tauri is required.

## Verification and developer bundle

```sh
npm test --prefix apps/desktop
npm run build --prefix apps/desktop
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml
npm run tauri --prefix apps/desktop -- build --no-bundle
```

On macOS, create an unsigned local `.app` with:

```sh
npm run tauri --prefix apps/desktop -- build --bundles app
```

On Windows, run the equivalent Tauri build on a Windows host to produce the configured local installer formats. Signing/notarization credentials are not included in this repository.

The original transparent source icon is `src-tauri/app-icon.png`; Tauri-generated macOS, Windows, and PNG bundle assets are under `src-tauri/icons/` and are referenced by `tauri.conf.json`.
