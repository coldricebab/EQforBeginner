"""Generate the SECS parity fixture (`testdata/secs-parity.json`).

The reference program is SECS by 한플 (Hanpeul), published on the DCInside
speaker gallery:
https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1
Used under the MIT License granted by the original author; see
THIRD-PARTY-NOTICES.md. This script only reads a developer-local copy of that
program - no upstream source is redistributed in this repository.

Developer-local prerequisites (matches the other tools/ extractors):
- `debugfiles/SECS.py` must exist (the upstream SECS snapshot; gitignored).
- numpy + scipy, e.g. `python3 -m venv .venv && .venv/bin/pip install numpy scipy`.

Run from the repository root:
    python tools/generate_secs_parity_fixture.py

The script extracts the pure-DSP slice of SECS.py by content markers (from the
PHASE_PRERING constants up to the Qt worker class), executes it without any Qt
dependency, rebuilds the analytic synthetic stereo IR that
`crates/dsp-core/tests/secs_parity.rs` also rebuilds, and mirrors
FilterGeneratorWorker.run's stereo non-preview path line by line.
"""

import json
import math
import sys
from dataclasses import replace
from pathlib import Path

import numpy as np
from scipy.fft import fft

REPO_ROOT = Path(__file__).resolve().parent.parent
SECS_SOURCE = REPO_ROOT / "debugfiles" / "SECS.py"


def load_secs_core():
    if not SECS_SOURCE.exists():
        sys.exit("SKIPPED: debugfiles/SECS.py is developer-local and absent here")
    lines = SECS_SOURCE.read_text().splitlines()
    start = next(i for i, l in enumerate(lines) if l.startswith("PHASE_PRERING_LOW_MIN_MS"))
    end = next(i for i, l in enumerate(lines) if l.startswith("class FilterGeneratorWorker"))
    source = "\n".join(lines[start:end])
    namespace = {}
    prelude = (
        "import math\n"
        "import numpy as np\n"
        "from functools import lru_cache\n"
        "from dataclasses import dataclass, replace\n"
        "from scipy.fft import fft, ifft, fftfreq, next_fast_len\n"
    )
    exec(prelude + source, namespace)  # noqa: S102 - trusted local file
    return namespace


_CORE = load_secs_core()
AUTO_DELAY_MIN_MS = _CORE["AUTO_DELAY_MIN_MS"]
FilterConfig = _CORE["FilterConfig"]
get_frequency_axes = _CORE["get_frequency_axes"]
get_lr4_weights_5bands = _CORE["get_lr4_weights_5bands"]
apply_5band_smoothing = _CORE["apply_5band_smoothing"]
precompute_secs_channel = _CORE["precompute_secs_channel"]
process_secs_filter = _CORE["process_secs_filter"]

SR = 48_000
N = 16_384


def biquad_hp(x, fc, q, sr=SR):
    w0 = 2.0 * math.pi * fc / sr
    cw = math.cos(w0)
    alpha = math.sin(w0) / (2.0 * q)
    b0 = (1.0 + cw) / 2.0
    b1 = -(1.0 + cw)
    b2 = (1.0 + cw) / 2.0
    a0 = 1.0 + alpha
    a1 = -2.0 * cw
    a2 = 1.0 - alpha
    return df1(x, b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0)


def biquad_lp(x, fc, q, sr=SR):
    w0 = 2.0 * math.pi * fc / sr
    cw = math.cos(w0)
    alpha = math.sin(w0) / (2.0 * q)
    b0 = (1.0 - cw) / 2.0
    b1 = 1.0 - cw
    b2 = (1.0 - cw) / 2.0
    a0 = 1.0 + alpha
    a1 = -2.0 * cw
    a2 = 1.0 - alpha
    return df1(x, b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0)


def df1(x, b0, b1, b2, a1, a2):
    y = np.zeros_like(x)
    x1 = x2 = y1 = y2 = 0.0
    for i in range(len(x)):
        xi = float(x[i])
        yi = b0 * xi + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2
        y[i] = yi
        x2, x1 = x1, xi
        y2, y1 = y1, yi
    return y


Q = 0.7071067811865476


def synth_channel(direct_idx, direct_amp, reflections, modes, hp_fc, lp_fc, post_gain):
    x = np.zeros(N, dtype=np.float64)
    x[direct_idx] = direct_amp
    for offset, amp in reflections:
        x[direct_idx + offset] += amp
    tail = N - direct_idx
    t = np.arange(tail, dtype=np.float64) / SR
    for freq, amp, tau, phase in modes:
        x[direct_idx:] += amp * np.exp(-t / tau) * np.sin(
            2.0 * math.pi * freq * t + phase
        )
    x = biquad_hp(x, hp_fc, Q)
    x = biquad_lp(x, lp_fc, Q)
    x = biquad_lp(x, lp_fc, Q)
    return x * post_gain


def synth_ir():
    # Modal tail amplitudes are chosen so the DFT resonance peaks sit a
    # realistic +6..+14 dB above the direct sound's flat spectral floor
    # (peak |X| ~ amp * tau * sr / 2 against the delta's 1.0).
    left = synth_channel(
        100,
        1.0,
        [(143, 0.55), (331, -0.4), (557, 0.3)],
        [
            (46.7, 8.3e-4, 0.25, 0.0),
            (71.3, -1.16e-3, 0.09, 1.1),
            (113.0, 1.39e-3, 0.06, 2.3),
        ],
        52.0,
        13_000.0,
        1.0,
    )
    right = synth_channel(
        112,
        0.92,
        [(161, 0.5), (389, -0.35)],
        [
            (44.1, 8.5e-4, 0.22, 0.7),
            (76.5, -1.15e-3, 0.08, 0.0),
            (108.0, 1.5e-3, 0.055, 1.9),
        ],
        58.0,
        12_800.0,
        0.9,
    )
    return np.column_stack((left, right))


def shift_right_zero(signal, shift_samples):
    if shift_samples <= 0:
        return signal
    out = np.zeros_like(signal)
    if shift_samples < len(signal):
        out[shift_samples:] = signal[:-shift_samples]
    return out


def crop_peak_left(signal, taps, left_samples, peak_idx=None):
    peak = int(np.argmax(np.abs(signal))) if peak_idx is None else int(peak_idx)
    start = peak - int(left_samples)
    if start >= 0 and start + taps <= len(signal):
        return signal[start : start + taps].astype(np.float32, copy=False)
    out = np.zeros(taps, dtype=np.float32)
    src_start = max(0, start)
    src_end = min(len(signal), start + taps)
    if src_end > src_start:
        dst_start = src_start - start
        dst_end = dst_start + (src_end - src_start)
        out[dst_start:dst_end] = signal[src_start:src_end]
    return out


def design_stereo(proc_ir, base_cfg, delay_candidates=None):
    """FilterGeneratorWorker.run, stereo, preview_mode=False, no resample."""
    preamp_db = 0.0
    n = len(proc_ir)
    fft_len = max(n, base_cfg.taps)
    freqs, _ = get_frequency_axes(fft_len, base_cfg.target_sr)
    v_idx = (freqs > 10) & (freqs < base_cfg.target_sr / 2)
    f_eval = freqs[v_idx]
    lf_upper_idx = f_eval <= 300.0
    weights = get_lr4_weights_5bands(fft_len, base_cfg.target_sr)
    orig_freqs, _ = get_frequency_axes(n, base_cfg.target_sr)
    orig_v_idx = (orig_freqs > 10) & (orig_freqs < base_cfg.target_sr / 2)
    f_orig = orig_freqs[orig_v_idx]

    l_ir, r_ir = proc_ir[:, 0], proc_ir[:, 1]
    ir_channels = [l_ir, r_ir]
    p_l, p_r = int(np.argmax(np.abs(l_ir))), int(np.argmax(np.abs(r_ir)))
    peak_positions = [p_l, p_r]
    max_p = max(p_l, p_r)
    win = int(base_cfg.target_sr * 0.005)
    rms_l = np.sqrt(np.mean(l_ir[max(0, p_l - win) : min(len(l_ir), p_l + win)] ** 2) + 1e-12)
    rms_r = np.sqrt(np.mean(r_ir[max(0, p_r - win) : min(len(r_ir), p_r + win)] ** 2) + 1e-12)
    rms_scale = rms_l / rms_r

    h_ir_channels = [fft(ch, n=fft_len) for ch in ir_channels]
    precomputed = [precompute_secs_channel(ch, base_cfg) for ch in ir_channels]

    best_metric = np.inf
    best_bundle = None
    target_db_cache = None
    phase_omega = 1j * 2 * np.pi * freqs / base_cfg.target_sr

    def evaluate(delay_ms):
        nonlocal target_db_cache
        cfg = replace(base_cfg, target_delay=float(delay_ms))
        target_delay_samples = int((cfg.target_delay / 1000.0) * cfg.target_sr)

        final_l, h_orig_l, target_mag_l, rm_eq_l, ref_mag_l, rolloff_l, track_l, low_l, high_l = (
            process_secs_filter(l_ir, cfg, apply_tilt=False, precomputed=precomputed[0])
        )
        final_r, h_orig_r, _, _, _, _, _, low_r, high_r = process_secs_filter(
            r_ir, cfg, apply_tilt=False, precomputed=precomputed[1]
        )
        final_r = final_r * rms_scale
        if not cfg.zero_latency:
            if max_p - p_l > 0:
                final_l = shift_right_zero(final_l, max_p - p_l)
            if max_p - p_r > 0:
                final_r = shift_right_zero(final_r, max_p - p_r)
        final_filter = np.column_stack((final_l, final_r))
        low_cutoff_metric = 0.5 * (float(low_l) + float(low_r))
        high_cutoff_metric = 0.5 * (float(high_l) + float(high_r))

        if np.any(np.isnan(final_filter)) or np.any(np.isinf(final_filter)):
            return None

        peak_ref = int(np.argmax(np.max(np.abs(final_filter), axis=1)))
        crop_l = crop_peak_left(final_filter[:, 0], cfg.taps, target_delay_samples, peak_ref)
        crop_r = crop_peak_left(final_filter[:, 1], cfg.taps, target_delay_samples, peak_ref)
        candidate_filter = np.column_stack((crop_l, crop_r))

        if target_db_cache is None:
            target_db_cache = 20 * np.log10(
                np.interp(f_eval, f_orig, target_mag_l[orig_v_idx]) + 1e-12
            )
        target_db = target_db_cache
        low_match_hz = max(10.0, float(low_cutoff_metric))
        lf_match_idx = lf_upper_idx & (f_eval >= low_match_hz)
        if not np.any(lf_match_idx):
            return None
        lf_match_freqs = f_eval[lf_match_idx]
        lf_span = max(300.0 - low_match_hz, 1e-9)
        proximity = np.clip((300.0 - lf_match_freqs) / lf_span, 0.0, 1.0)
        lf_weights = 1.0 + (2.0 - 1.0) * proximity
        lf_wsum = float(np.sum(lf_weights) + 1e-12)

        candidate_shift_phase = np.exp(phase_omega * (max_p + target_delay_samples))
        h_fil_metric = fft(candidate_filter, n=fft_len, axis=0)
        h_sys_l = h_ir_channels[0] * h_fil_metric[:, 0] * candidate_shift_phase
        h_sys_r = h_ir_channels[1] * h_fil_metric[:, 1] * candidate_shift_phase
        h_sys_sum = h_sys_l + h_sys_r
        sum_smoothed = apply_5band_smoothing(
            np.abs(h_sys_sum), fft_len, cfg.target_sr, weights, False, cfg.res_mode
        )[v_idx]
        after_db = 20 * np.log10(sum_smoothed + 1e-12)
        lf_err = after_db[lf_match_idx] - (target_db[lf_match_idx] + 6.0)
        abs_mae = float(np.sum(lf_weights * np.abs(lf_err)) / lf_wsum)
        lf_err_mean = float(np.sum(lf_weights * lf_err) / lf_wsum)
        shape_mae = float(np.sum(lf_weights * np.abs(lf_err - lf_err_mean)) / lf_wsum)
        metric = abs_mae + 0.35 * shape_mae

        bundle = (
            cfg,
            candidate_filter,
            target_mag_l,
            low_cutoff_metric,
            high_cutoff_metric,
            max_p,
            target_delay_samples,
        )
        return metric, bundle

    if base_cfg.low_latency or base_cfg.zero_latency:
        stage1 = np.array([0.0])
    elif delay_candidates is not None:
        stage1 = np.clip(np.array(delay_candidates, dtype=float), 2.0, 10.0)
    else:
        stage1 = np.arange(2.0, 10.0 + 1e-9, 1.0)

    metrics = []
    for delay_ms in stage1:
        result = evaluate(float(delay_ms))
        if result is None:
            metrics.append(None)
            continue
        metric, bundle = result
        metrics.append(metric)
        if metric < best_metric:
            best_metric = metric
            best_bundle = bundle

    assert best_bundle is not None
    cfg, crop_filter, target_mag_l, low_l, high_l, max_p, target_delay_samples = best_bundle

    tilt_post_enabled = abs(cfg.tilt_db_oct) > 1e-12
    if tilt_post_enabled:
        final_l, h_orig_l, target_mag_l, rm_eq_l, ref_mag_l, rolloff_l, track_l, low_l2, high_l2 = (
            process_secs_filter(l_ir, cfg, apply_tilt=True, precomputed=precomputed[0])
        )
        final_r, h_orig_r, _, _, _, _, _, low_r2, high_r2 = process_secs_filter(
            r_ir, cfg, apply_tilt=True, precomputed=precomputed[1]
        )
        low_l = 0.5 * (float(low_l2) + float(low_r2))
        high_l = 0.5 * (float(high_l2) + float(high_r2))
        final_r = final_r * rms_scale
        if not cfg.zero_latency:
            if max_p - p_l > 0:
                final_l = shift_right_zero(final_l, max_p - p_l)
            if max_p - p_r > 0:
                final_r = shift_right_zero(final_r, max_p - p_r)
        final_filter = np.column_stack((final_l, final_r))
        peak_ref = int(np.argmax(np.max(np.abs(final_filter), axis=1)))
        crop_l = crop_peak_left(final_filter[:, 0], cfg.taps, target_delay_samples, peak_ref)
        crop_r = crop_peak_left(final_filter[:, 1], cfg.taps, target_delay_samples, peak_ref)
        crop_filter = np.column_stack((crop_l, crop_r))

    if cfg.zero_latency:
        best_shift_phases = [np.exp(phase_omega * peak_positions[ch]) for ch in range(2)]
    else:
        common = np.exp(phase_omega * (max_p + target_delay_samples))
        best_shift_phases = [common, common]

    h_bal = fft(crop_filter, n=fft_len, axis=0)
    h_sys_l = h_ir_channels[0] * h_bal[:, 0] * best_shift_phases[0]
    h_sys_r = h_ir_channels[1] * h_bal[:, 1] * best_shift_phases[1]
    bal_idx = (freqs > 20.0) & (freqs < min(1000.0, cfg.target_sr / 2 - 1.0))
    lr_offset_db = 0.0
    if np.any(bal_idx):
        mag_l_db = 20 * np.log10(
            apply_5band_smoothing(np.abs(h_sys_l), fft_len, cfg.target_sr, weights, False, cfg.res_mode)[bal_idx]
            + 1e-12
        )
        mag_r_db = 20 * np.log10(
            apply_5band_smoothing(np.abs(h_sys_r), fft_len, cfg.target_sr, weights, False, cfg.res_mode)[bal_idx]
            + 1e-12
        )
        lr_offset_db = float(np.median(mag_l_db - mag_r_db))
        lr_offset_db = float(np.clip(lr_offset_db, -6.0, 6.0))
        if abs(lr_offset_db) > 0.01:
            crop_filter[:, 1] *= 10 ** (lr_offset_db / 20.0)

    flen = min(int(cfg.target_sr * 0.005), cfg.taps)
    fade = np.linspace(1, 0, flen) ** 2
    crop_filter[-flen:, 0] *= fade
    crop_filter[-flen:, 1] *= fade

    fft_eval_len = max(len(crop_filter), 8192)
    max_mag = np.max(np.abs(fft(crop_filter, n=fft_eval_len, axis=0)))
    if max_mag > 1.0:
        crop_filter = crop_filter / max_mag
        preamp_db = -20 * np.log10(max_mag)

    return {
        "cfg": cfg,
        "crop_filter": crop_filter,
        "target_mag_l": target_mag_l,
        "low_cutoff": float(low_l),
        "high_cutoff": float(high_l),
        "preamp_db": float(preamp_db),
        "rms_scale": float(rms_scale),
        "lr_offset_db": float(lr_offset_db),
        "auto_delay_ms": float(cfg.target_delay),
        "metrics": metrics,
        "stage1": [float(d) for d in stage1],
        "fft_len": fft_len,
        "f_eval": f_eval,
        "f_orig": f_orig,
        "orig_v_idx": orig_v_idx,
        "v_idx": v_idx,
        "h_ir_channels": h_ir_channels,
        "best_shift_phases": best_shift_phases,
        "peaks": peak_positions,
    }


def curves(result, query):
    out = {}
    fft_len = result["fft_len"]
    f_eval = result["f_eval"]
    v_idx = result["v_idx"]
    crop = result["crop_filter"]
    h_fil = fft(crop, n=fft_len, axis=0)
    for ch, name in ((0, "left"), (1, "right")):
        fil_db = 20 * np.log10(np.abs(h_fil[:, ch])[v_idx] + 1e-12)
        sys_db = 20 * np.log10(
            np.abs(result["h_ir_channels"][ch] * h_fil[:, ch] * result["best_shift_phases"][ch])[v_idx]
            + 1e-12
        )
        out[f"filter_db_{name}"] = np.interp(query, f_eval, fil_db).tolist()
        out[f"system_db_{name}"] = np.interp(query, f_eval, sys_db).tolist()
    target_db = 20 * np.log10(
        np.interp(query, result["f_orig"], result["target_mag_l"][result["orig_v_idx"]]) + 1e-12
    )
    out["target_db"] = target_db.tolist()
    return out


def tap_slices(crop, peak_window=256, head=128):
    out = {}
    for ch, name in ((0, "left"), (1, "right")):
        taps = crop[:, ch].astype(np.float64)
        peak = int(np.argmax(np.abs(taps)))
        start = max(0, peak - peak_window // 2)
        out[f"{name}_head"] = taps[:head].tolist()
        out[f"{name}_peak_start"] = start
        out[f"{name}_peak"] = taps[start : start + peak_window].tolist()
        out[f"{name}_abs_sum"] = float(np.sum(np.abs(taps)))
        out[f"{name}_peak_index"] = peak
    return out


def main():
    ir = synth_ir()
    checks = {
        "left_sum_sq": float(np.sum(ir[:, 0] ** 2)),
        "right_sum_sq": float(np.sum(ir[:, 1] ** 2)),
        "left_abs_sum": float(np.sum(np.abs(ir[:, 0]))),
        "right_abs_sum": float(np.sum(np.abs(ir[:, 1]))),
        "left_peak_index": int(np.argmax(np.abs(ir[:, 0]))),
        "right_peak_index": int(np.argmax(np.abs(ir[:, 1]))),
    }
    query = np.exp(
        np.linspace(math.log(20.0), math.log(20_000.0), 128)
    )

    def cfg_dict(cfg):
        return {
            "orig_sr": cfg.orig_sr,
            "target_sr": cfg.target_sr,
            "max_boost": cfg.max_boost,
            "target_delay": cfg.target_delay,
            "tilt_db_oct": cfg.tilt_db_oct,
            "bass_boost_db": cfg.bass_boost_db,
            "bass_freq": cfg.bass_freq,
            "res_mode": cfg.res_mode,
            "taps": cfg.taps,
            "hf_min_phase_ref_hz": cfg.hf_min_phase_ref_hz,
            "low_latency": cfg.low_latency,
            "zero_latency": cfg.zero_latency,
        }

    cases = []
    normal = FilterConfig(
        orig_sr=SR,
        target_sr=SR,
        max_boost=6.0,
        target_delay=AUTO_DELAY_MIN_MS,
        tilt_db_oct=-0.3,
        bass_boost_db=3.0,
        bass_freq=60.0,
        res_mode=1,
        taps=8192,
        hf_min_phase_ref_hz=300.0,
    )
    result = design_stereo(ir, normal, delay_candidates=None)
    pre_l = precompute_secs_channel(ir[:, 0], normal)
    pre_r = precompute_secs_channel(ir[:, 1], normal)
    cases.append(
        {
            "name": "normal_auto",
            "config": cfg_dict(normal),
            "expected": {
                "auto_delay_ms": result["auto_delay_ms"],
                "stage1_metrics": result["metrics"],
                "low_cutoff_hz": result["low_cutoff"],
                "high_cutoff_hz": result["high_cutoff"],
                "channel_low_cutoffs": [float(pre_l["low_cutoff"]), float(pre_r["low_cutoff"])],
                "channel_high_cutoffs": [float(pre_l["high_cutoff"]), float(pre_r["high_cutoff"])],
                "channel_ref_mags": [float(pre_l["ref_mag"]), float(pre_r["ref_mag"])],
                "preamp_db": result["preamp_db"],
                "rms_scale": result["rms_scale"],
                "lr_offset_db": result["lr_offset_db"],
                "peaks": result["peaks"],
                **curves(result, query),
                **tap_slices(result["crop_filter"]),
            },
        }
    )

    zero = replace(normal, zero_latency=True, tilt_db_oct=0.0)
    result_zero = design_stereo(ir, zero)
    cases.append(
        {
            "name": "zero_latency",
            "config": cfg_dict(zero),
            "expected": {
                "auto_delay_ms": result_zero["auto_delay_ms"],
                "low_cutoff_hz": result_zero["low_cutoff"],
                "high_cutoff_hz": result_zero["high_cutoff"],
                "preamp_db": result_zero["preamp_db"],
                "rms_scale": result_zero["rms_scale"],
                "lr_offset_db": result_zero["lr_offset_db"],
                "peaks": result_zero["peaks"],
                **curves(result_zero, query),
                **tap_slices(result_zero["crop_filter"]),
            },
        }
    )

    # scipy resample_poly references for the Rust `secs_resample_poly` port
    # (48 kHz left channel -> 44.1 kHz and 96 kHz, the rational families the
    # per-rate export path exercises).
    from scipy.signal import resample_poly

    resample_cases = []
    for name, up, down in (("to_44100", 147, 160), ("to_96000", 2, 1)):
        resampled = resample_poly(ir[:, 0], up, down)
        mid = len(resampled) // 2
        resample_cases.append(
            {
                "name": name,
                "up": up,
                "down": down,
                "length": len(resampled),
                "sum_sq": float(np.sum(resampled**2)),
                "abs_sum": float(np.sum(np.abs(resampled))),
                "head": resampled[:64].tolist(),
                "mid_start": mid,
                "mid": resampled[mid : mid + 64].tolist(),
            }
        )

    fixture = {
        "schema": "eqforbeginner-secs-parity-v1",
        "sample_rate_hz": SR,
        "ir_length": N,
        "ir_checksums": checks,
        "query_frequencies_hz": query.tolist(),
        "resample_cases": resample_cases,
        "cases": cases,
    }
    out_path = REPO_ROOT / "testdata" / "secs-parity.json"
    with open(out_path, "w") as fh:
        json.dump(fixture, fh)
    print(f"wrote {out_path}")
    print("auto delay:", result["auto_delay_ms"], "metrics:", [None if m is None else round(m, 4) for m in result["metrics"]])
    print("low/high cutoff:", result["low_cutoff"], result["high_cutoff"])
    print("preamp:", result["preamp_db"], "rms_scale:", result["rms_scale"], "lr_offset:", result["lr_offset_db"])
    print("zero preamp:", result_zero["preamp_db"])
    print("checksums:", checks)


if __name__ == "__main__":
    main()
