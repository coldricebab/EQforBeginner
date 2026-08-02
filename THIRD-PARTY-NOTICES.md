# Third-party notices

## SECS — single-point full-band room correction

The **SECS advanced option** in this app is a port of the SECS room-correction
program.

- **Original author:** 한플 (Hanpeul)
- **Original work:** SECS (Python / PyQt), published on the DCInside speaker
  gallery
- **Source link:**
  <https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1>

### License

The original author licensed the work under the **MIT License** for use in
this project, and asked that the original be credited with a link. That credit
appears in this file, in the app's SECS advanced option, in the README, and in
the header of every ported source file.

Because the upstream grant is MIT, the ported code in this repository carries
no restriction beyond MIT: anyone may reuse `crates/dsp-core/src/secs.rs` and
its fixtures under the same terms, keeping the copyright and permission notice
below. The upstream program's own distribution ships no license file, so this
notice is the record of the grant.

```
MIT License

Copyright (c) 한플 (Hanpeul)          — original SECS algorithm and implementation
Copyright (c) 2026 coldricebab        — Rust port and the additions listed below

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

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
author; the MIT grant covers the code, not an endorsement of what this project
built on top of it. `crates/dsp-core/tests/secs_parity.rs` pins the ported DSP against the
original program's own output so the two can be compared numerically.
