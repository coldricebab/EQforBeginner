# Third-party notices

## SECS — single-point full-band room correction

The **SECS advanced option** in this app is a port of the SECS room-correction
program.

- **Original author:** 한플 (Hanpeul)
- **Original work:** SECS (Python / PyQt), published on the DCInside speaker
  gallery
- **Source link:**
  <https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1>

### Permission and scope

The original author gave this project permission to freely modify and improve
the work, and asked that the original be credited with a link. That credit
appears in this file, in the app's SECS advanced option, in the README, and in
the header of every ported source file.

This is a **permission granted to this project**, not a statement that the
upstream program carries an open-source license. The upstream distribution
carries no license text of its own. Anyone who wants to reuse the ported code
(`crates/dsp-core/src/secs.rs` and its fixtures) beyond what this project's
MIT license can convey should contact the original author directly.

### What was ported, and what is not the original author's work

Ported from the original (`crates/dsp-core/src/secs.rs`):

- cepstral minimum-phase / excess-phase split and the all-pass excess
  inversion with per-band pre-ringing windows,
- five-band LR4-weighted fractional-octave smoothing,
- the adaptive target (natural low/high cutoff tracking, base ideal, bass
  shelf, tilt),
- bounded-boost magnitude inversion, macro EQ, and the sub-500 Hz peak crush,
- the automatic target-delay search,
- the stereo combine (level match, inter-channel peak alignment, channel
  balance trim, fade, peak normalization),
- a `scipy.signal.resample_poly` equivalent.

Added by this project, and therefore **not** the original author's design:

- multi-position magnitude averaging across measured seats,
- the house-curve target overlay,
- the 2.1 shared-sub-band commonization below a confirmed crossover,
- the closed-loop verification, per-native-rate export, and headroom
  calculation that wrap the design,
- the Rust implementation itself and its numerical parity harness.

Defects in any of the above belong to this project, not to the original
author. `crates/dsp-core/tests/secs_parity.rs` pins the ported DSP against the
original program's own output so the two can be compared numerically.
