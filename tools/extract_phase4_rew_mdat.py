#!/usr/bin/env python3
"""Build the trusted Phase 4 offline-replay fixture from six REW MDAT files.

This development-only converter never ships with EQforBeginner. The product runtime
consumes the versioned JSON fixture, verifies all original source hashes, and does
not parse private REW serialization or require Python.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any

from extract_rew_mdat import (
    MAX_MDAT_BYTES,
    class_name,
    extract_response,
    read_single_measurement,
    wrapped_value,
)


# Historic on-disk format id, predates the product rename; not user-visible branding
# - do not rename. The Rust CLI compares the emitted fixture against this exact value.
SCHEMA_VERSION = "similarrew-phase4-offline-measurements-v1"
EXTRACTOR_VERSION = "rew-mdat-phase4-javaobj-v2-band-grid-origin"
MIN_FREQUENCY_HZ = 20.0
# Retain a guard above the 2 kHz scoring edge so 1/3-octave smoothing
# remains two-sided at every evaluated bin.
MAX_ANALYSIS_FREQUENCY_HZ = 3_000.0
SOURCE_SWEEP_MAX_FREQUENCY_HZ = 20_000.0
MAX_IR_SAMPLES = 262_144

SOURCE_FILES = {
    "combined-left-xo90": "L_C90_D083.mdat",
    "combined-right-xo90": "R_C90_D083.mdat",
    "main-left-xo90": "L_M90.mdat",
    "main-right-xo90": "R_M90.mdat",
    "sub-xo90-a": "S90_A.mdat",
    "sub-xo90-b": "S90_B.mdat",
}


def extract_impulse(path: Path, response_id: str) -> dict[str, Any]:
    measurement = read_single_measurement(path)
    sampled = measurement.irData.ir
    if class_name(sampled) != "roomeqwizard.SampledData":
        raise ValueError(f"{path}: unsupported stored impulse representation")
    samples = [float(value) for value in sampled.data]
    sample_interval_s = float(sampled.T)
    start_time_s = float(sampled.startTime)
    if not samples or len(samples) > MAX_IR_SAMPLES:
        raise ValueError(f"{path}: unexpected impulse length {len(samples)}")
    if not all(math.isfinite(value) for value in samples):
        raise ValueError(f"{path}: non-finite impulse sample")
    if (
        not math.isfinite(sample_interval_s)
        or abs(sample_interval_s - 1.0 / 48_000.0) > 1.0e-12
        or not math.isfinite(start_time_s)
    ):
        raise ValueError(f"{path}: invalid 48 kHz impulse timeline")
    return {
        "response_id": response_id,
        "sample_interval_s": sample_interval_s,
        "start_time_s": start_time_s,
        "timeline_shift_applied": False,
        "samples": samples,
    }


def build_fixture(measurements_root: Path) -> dict[str, Any]:
    responses = {
        response_id: extract_response(
            measurements_root / relative_path,
            relative_path,
            MIN_FREQUENCY_HZ,
            MAX_ANALYSIS_FREQUENCY_HZ,
        )
        for response_id, relative_path in SOURCE_FILES.items()
    }
    for response_id, response in responses.items():
        measurement = read_single_measurement(
            measurements_root / SOURCE_FILES[response_id]
        )
        source_valid_range_hz = [
            float(measurement.validStartFreq),
            float(measurement.validEndFreq),
        ]
        if (
            source_valid_range_hz[0] > 20.2
            or source_valid_range_hz[1] < 19_999.0
        ):
            raise ValueError(
                f"{SOURCE_FILES[response_id]}: source sweep does not cover 20 Hz-20 kHz"
            )
        response["source_valid_range_hz"] = source_valid_range_hz
        response["filter_set_when_measured_present"] = (
            measurement.filterSetWhenMeasured is not None
        )
        calibration_frequencies = [float(value) for value in measurement.meterCal.freqArray]
        if (
            not calibration_frequencies
            or any(not math.isfinite(value) for value in calibration_frequencies)
            or any(
                right <= left
                for left, right in zip(
                    calibration_frequencies, calibration_frequencies[1:]
                )
            )
        ):
            raise ValueError(
                f"{SOURCE_FILES[response_id]}: invalid calibration frequency data"
            )
        response["calibration"]["frequency_range_hz"] = [
            calibration_frequencies[0],
            calibration_frequencies[-1],
        ]
        response["role"] = (
            "combined"
            if response_id.startswith("combined-")
            else "main-only"
            if response_id.startswith("main-")
            else "sub-only"
        )
        response["quality"]["known_warnings"] = []
        signal_to_distortion = response["quality"]["signal_to_distortion_db"]
        if signal_to_distortion is not None and signal_to_distortion < 20.0:
            response["quality"]["known_warnings"].append(
                "Sub-only signal-to-distortion evidence is below 20 dB; use this response for path-consistency diagnostics only"
            )

    grid_reference = None
    for response_id, response in responses.items():
        grid = (
            response["frequency_start_hz"],
            response["frequency_step_hz"],
            len(response["magnitude_db_spl"]),
        )
        if grid_reference is None:
            grid_reference = grid
        elif any(
            abs(float(left) - float(right)) > 1.0e-8
            for left, right in zip(grid, grid_reference)
        ):
            raise ValueError(f"{response_id}: response grid does not match")

    assert grid_reference is not None
    frequency_start_hz, frequency_step_hz, length = grid_reference
    frequency_grid_hz = [
        frequency_start_hz + index * frequency_step_hz for index in range(int(length))
    ]
    for response in responses.values():
        del response["frequency_start_hz"]
        del response["frequency_step_hz"]

    impulses = {
        response_id: extract_impulse(
            measurements_root / SOURCE_FILES[response_id], response_id
        )
        for response_id in ("combined-left-xo90", "combined-right-xo90")
    }

    return {
        "schema_version": SCHEMA_VERSION,
        "extraction": {
            "extractor_version": EXTRACTOR_VERSION,
            "dependency": "javaobj-py3==0.5.0 (development only)",
            "source_rew_build": "5.31.3",
            "preferred_future_path": "REW 5.40+ local REST API",
            "extracted_analysis_range_hz": [
                MIN_FREQUENCY_HZ,
                MAX_ANALYSIS_FREQUENCY_HZ,
            ],
            "source_sweep_range_hz": [
                MIN_FREQUENCY_HZ,
                SOURCE_SWEEP_MAX_FREQUENCY_HZ,
            ],
            "level_alignment_applied": False,
            "timeline_shift_applied": False,
            "maximum_source_bytes": MAX_MDAT_BYTES,
        },
        "assumptions": {
            "declaration_date": "2026-07-19",
            "source": "user-declared",
            "verified": False,
            "system": "2.1 single subwoofer",
            "crossover_hz": 90.0,
            "main_delay_ms": 0.83,
            "main_delay_status": "assumed-optimal-not-app-optimized",
            "polarity": "unchanged-value-not-recorded",
            "sub_level": "unchanged-value-not-recorded",
            "playback_volume": "unchanged-value-not-recorded",
            "microphone_gain": "unchanged-value-not-recorded",
            "post_fir_measurement_deferred_until": "first real-use test after Developer Beta 1",
        },
        "frequency_grid_hz": frequency_grid_hz,
        "responses": responses,
        "combined_impulses": impulses,
        "design_inputs": {
            "position_id": "P0",
            "position_weight": 2.0,
            "left_combined_response_id": "combined-left-xo90",
            "right_combined_response_id": "combined-right-xo90",
        },
        "separated_references": [
            {
                "id": f"xo90-{channel}-sub-{repeat}",
                "crossover_hz": 90.0,
                "channel": channel,
                "main_response_id": f"main-{channel}-xo90",
                "sub_response_id": f"sub-xo90-{repeat}",
                "combined_response_id": f"combined-{channel}-xo90",
            }
            for channel in ("left", "right")
            for repeat in ("a", "b")
        ],
        "post_fir_measurements": [],
        "limitations": [
            "The combined L/R files are the existing XO90 Phase 3 candidate captures, not new repeats or post-FIR measurements.",
            "Only central position P0 is available; spatial verification is unavailable.",
            "L and R combined responses have no same-setting repeat.",
            "Sub-only A/B are admitted for path-consistency diagnostics, not as filter-design responses.",
        ],
        "required_evidence_missing": [
            "actual 48 kHz FIR-applied L+sub and R+sub remeasurement",
            "actual-path clipping, sample-drop and channel-map checks",
            "same-setting combined-response repeats and timing residual",
            "multi-position spatial verification",
            "verification-signal true peak for recommended headroom",
        ],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--measurements-root",
        type=Path,
        default=Path("measurments/phase4"),
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("measurments/derived/phase4-offline-measurements.json"),
    )
    arguments = parser.parse_args()
    fixture = build_fixture(arguments.measurements_root.resolve())
    payload = (json.dumps(fixture, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_bytes(payload)
    print(f"Wrote {arguments.output} ({len(payload)} bytes, {len(SOURCE_FILES)} sources)")


if __name__ == "__main__":
    main()
