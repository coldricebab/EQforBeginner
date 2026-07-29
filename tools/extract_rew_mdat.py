#!/usr/bin/env python3
"""Create the Phase 3 internal fixture from trusted, single-measurement REW MDAT files.

This is a development-only converter. EQforBeginner does not load MDAT at runtime and
does not require Python. Prefer the official REW 5.40+ REST API for future fixture
refreshes; the installed REW 5.31.3 build rejects API startup, so this converter
uses javaobj-py3 to read the existing Java serialization stream without loading
REW classes or executing serialized code.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
from typing import Any

try:
    from javaobj.v1.transformers import DefaultObjectTransformer
    from javaobj.v1.unmarshaller import JavaObjectUnmarshaller
except ImportError as error:  # pragma: no cover - exercised by the usage error path
    raise SystemExit(
        "javaobj-py3 0.5.0 is required for this development-only conversion; "
        "install tools/requirements-mdat.txt in an isolated environment"
    ) from error


# Historic on-disk format id, predates the product rename; not user-visible branding
# - do not rename. The Rust CLI compares the emitted fixture against this exact value.
SCHEMA_VERSION = "similarrew-phase3-measurements-v1"
EXTRACTOR_VERSION = "rew-mdat-javaobj-v2-grid-origin"
MAX_MDAT_BYTES = 32 * 1024 * 1024
MIN_FREQUENCY_HZ = 20.0
MAX_FREQUENCY_HZ = 500.0

SOURCE_FILES = {
    "first-left-raw-a": "first_measurments/NEW_L_Center_Raw_48k_A.mdat",
    "first-left-raw-b": "first_measurments/NEW_L_Center_Raw_48k_B.mdat",
    "first-right-raw-a": "first_measurments/NEW_R_Center_Raw_48k_A.mdat",
    "first-right-raw-b": "first_measurments/NEW_R_Center_Raw_48k_B.mdat",
    "first-left-main-xo80": "first_measurments/NEW_L_MainOnly_XO80.mdat",
    "first-right-main-xo80": "first_measurments/NEW_R_MainOnly_XO80.mdat",
    "first-sub-xo80-a": "first_measurments/NEW_SubOnly_XO80_A.mdat",
    "first-sub-xo80-b": "first_measurments/NEW_SubOnly_XO80_B.mdat",
    "first-left-combined-xo80": "first_measurments/NEW_L_Combined_XO80.mdat",
    "first-right-combined-xo80": "first_measurments/NEW_R_Combined_XO80.mdat",
    "candidate-xo70-left": "find_sub_crossover/L_C70_D083.mdat",
    "candidate-xo70-right": "find_sub_crossover/R_C70_D083.mdat",
    "candidate-xo80-left": "find_sub_crossover/L_C80_NOW.mdat",
    "candidate-xo80-right": "find_sub_crossover/R_C80_NOW.mdat",
    "candidate-xo90-left": "find_sub_crossover/L_C90_D083.mdat",
    "candidate-xo90-right": "find_sub_crossover/R_C90_D083.mdat",
}


def wrapped_value(value: Any) -> Any:
    """Return java.lang primitive wrapper content without guessing other objects."""

    return getattr(value, "value", value)


def optional_float(value: Any) -> float | None:
    if value is None:
        return None
    result = float(wrapped_value(value))
    if not math.isfinite(result):
        raise ValueError("non-finite numeric metadata in MDAT")
    return result


def class_name(value: Any) -> str | None:
    return getattr(getattr(value, "classdesc", None), "name", None)


def select_linear_grid_range(
    stored_start_hz: float,
    step_hz: float,
    value_count: int,
    requested_low_hz: float,
    requested_high_hz: float,
) -> tuple[int, int, float]:
    """Return inclusive array indices while preserving a nonzero grid origin."""

    if (
        not math.isfinite(stored_start_hz)
        or stored_start_hz < 0.0
        or not math.isfinite(step_hz)
        or step_hz <= 0.0
        or value_count <= 0
        or not math.isfinite(requested_low_hz)
        or not math.isfinite(requested_high_hz)
        or requested_low_hz < 0.0
        or requested_high_hz < requested_low_hz
    ):
        raise ValueError("invalid stored or requested frequency grid")
    first_index = max(0, math.ceil((requested_low_hz - stored_start_hz) / step_hz))
    last_index = min(
        value_count - 1,
        math.floor((requested_high_hz - stored_start_hz) / step_hz),
    )
    if first_index > last_index:
        raise ValueError("requested extraction range has no stored response data")
    return first_index, last_index, stored_start_hz + first_index * step_hz


def read_single_measurement(path: Path) -> Any:
    size = path.stat().st_size
    if size <= 0 or size > MAX_MDAT_BYTES:
        raise ValueError(f"{path}: unexpected MDAT size {size} bytes")

    measurements = []
    header = None
    with path.open("rb") as stream:
        unmarshaller = JavaObjectUnmarshaller(stream)
        unmarshaller.add_transformer(DefaultObjectTransformer())
        while stream.tell() < size:
            _, value = unmarshaller._read_and_exec_opcode()  # noqa: SLF001
            if header is None and isinstance(value, str):
                header = str(value)
            if class_name(value) == "roomeqwizard.MeasData":
                measurements.append(value)

    if header != "REW Measurement Data File V2":
        raise ValueError(f"{path}: unsupported MDAT header {header!r}")
    if len(measurements) != 1:
        raise ValueError(f"{path}: expected one measurement, found {len(measurements)}")
    return measurements[0]


def rew_version(measurement: Any) -> str:
    major = int(wrapped_value(measurement.rewVersion))
    minor = int(wrapped_value(measurement.rewSubVersion))
    suffix = str(measurement.versionSt)
    return f"{major}.{minor}{suffix}"


def extract_response(
    path: Path,
    relative_path: str,
    minimum_frequency_hz: float = MIN_FREQUENCY_HZ,
    maximum_frequency_hz: float = MAX_FREQUENCY_HZ,
) -> dict[str, Any]:
    if (
        not math.isfinite(minimum_frequency_hz)
        or not math.isfinite(maximum_frequency_hz)
        or minimum_frequency_hz < 0.0
        or maximum_frequency_hz <= minimum_frequency_hz
    ):
        raise ValueError("invalid requested extraction frequency range")
    measurement = read_single_measurement(path)
    if int(measurement.sampleRate) != 48_000:
        raise ValueError(f"{path}: Phase 3 fixture requires 48 kHz")
    if bool(measurement.isLogSpaced):
        raise ValueError(f"{path}: expected the stored linear response grid")

    frequency_start_hz = float(measurement.startFreq)
    frequency_step_hz = float(measurement.freqStep)
    magnitudes = [float(value) for value in measurement.splValues]
    phases = [float(value) for value in measurement.unwPhaseValues]
    if len(magnitudes) != int(measurement.dataLength) or len(phases) != len(magnitudes):
        raise ValueError(f"{path}: inconsistent response array lengths")
    if (
        not math.isfinite(frequency_start_hz)
        or frequency_start_hz < 0.0
        or not math.isfinite(frequency_step_hz)
        or frequency_step_hz <= 0.0
    ):
        raise ValueError(f"{path}: invalid stored frequency grid")

    stored_end_hz = frequency_start_hz + (len(magnitudes) - 1) * frequency_step_hz
    if abs(stored_end_hz - float(measurement.endFreq)) > frequency_step_hz * 0.51:
        raise ValueError(f"{path}: stored response length does not match its frequency grid")

    valid_low = max(minimum_frequency_hz, float(measurement.validStartFreq))
    valid_high = min(maximum_frequency_hz, float(measurement.validEndFreq))
    if valid_high < valid_low:
        raise ValueError(f"{path}: requested extraction range has no valid response data")
    first_index, last_index, selected_start_hz = select_linear_grid_range(
        frequency_start_hz,
        frequency_step_hz,
        len(magnitudes),
        valid_low,
        valid_high,
    )
    selected_magnitude = magnitudes[first_index : last_index + 1]
    selected_phase = phases[first_index : last_index + 1]
    if not selected_magnitude:
        raise ValueError(
            f"{path}: no usable response bins in "
            f"{minimum_frequency_hz:g}-{maximum_frequency_hz:g} Hz"
        )
    if not all(math.isfinite(value) for value in selected_magnitude + selected_phase):
        raise ValueError(f"{path}: non-finite response value")

    ir_data = measurement.irData
    meter_cal = measurement.meterCal
    return {
        "source_path": relative_path,
        "source_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "source_bytes": path.stat().st_size,
        "measurement_title": str(measurement.shortDesc),
        "measurement_notes": str(measurement.measNotes),
        "rew_version": rew_version(measurement),
        "sample_rate_hz": int(measurement.sampleRate),
        "frequency_start_hz": selected_start_hz,
        "frequency_step_hz": frequency_step_hz,
        "magnitude_db_spl": selected_magnitude,
        "unwrapped_phase_degrees": selected_phase,
        "calibration": {
            "embedded_calibration_applied": bool(wrapped_value(measurement.usesEmbeddedCalData)),
            "calibration_limit_applied": bool(wrapped_value(measurement.calDataLimitApplied)),
            "microphone_serial": int(wrapped_value(meter_cal.serialNum)),
            "microphone_calibration_file": Path(str(meter_cal.sourceName)).name,
            "spl_calibration_offset_db": optional_float(measurement.splCalOffset),
        },
        "quality": {
            "signal_to_noise_db": optional_float(ir_data.signalToNoisedB),
            "signal_to_distortion_db": optional_float(ir_data.signalToDistdB),
            "signal_dbfs": optional_float(ir_data.signaldBFS),
            "noise_and_distortion_dbfs": optional_float(ir_data.noiseAndNHDdBFS),
        },
        "timing": {
            "used_acoustic_reference": bool(wrapped_value(ir_data.usedTimingRef)),
            "reference_stimulus": str(measurement.outputName),
            "measurement_delay_ms": float(ir_data.measurementDelay) * 1_000.0,
            "timing_offset_ms": float(ir_data.timingOffset) * 1_000.0,
            "cumulative_start_time_offset_ms": float(ir_data.cumulativeStartTimeOffset)
            * 1_000.0,
            "clock_adjustment_ppm": float(ir_data.cumulativeClockRateAdjust) * 1_000_000.0,
            "original_peak_time_ms": float(ir_data.origPeakTime) * 1_000.0,
            "timeline_policy": "raw REW timeline retained; no peak-to-zero shift applied",
        },
    }


def build_fixture(measurements_root: Path) -> dict[str, Any]:
    responses = {
        response_id: extract_response(measurements_root / relative_path, relative_path)
        for response_id, relative_path in SOURCE_FILES.items()
    }
    for response_id in ("first-sub-xo80-a", "first-sub-xo80-b"):
        responses[response_id]["quality"]["known_warnings"] = [
            "REW rew_output.txt recorded High measurement distortion 11.4% for this sub-only run"
        ]

    grid_reference = None
    for response_id, response in responses.items():
        grid = (
            response["frequency_start_hz"],
            response["frequency_step_hz"],
            len(response["magnitude_db_spl"]),
        )
        if grid_reference is None:
            grid_reference = grid
        elif any(abs(float(left) - float(right)) > 1.0e-8 for left, right in zip(grid, grid_reference)):
            raise ValueError(f"{response_id}: response grid does not match the fixture grid")

    assert grid_reference is not None
    frequency_start_hz, frequency_step_hz, length = grid_reference
    frequencies_hz = [frequency_start_hz + index * frequency_step_hz for index in range(int(length))]
    for response in responses.values():
        del response["frequency_start_hz"]
        del response["frequency_step_hz"]

    candidates = []
    for crossover_hz in (70, 80, 90):
        candidates.append(
            {
                "id": f"xo-{crossover_hz}-main-delay-0.83ms",
                "hardware": {
                    "crossover_hz": float(crossover_hz),
                    "main_delay_ms": 0.83,
                    "sub_level_db": None,
                    "polarity_inverted": None,
                    "source": "user-supplied measurement description, 2026-07-19",
                },
                "positions": [
                    {
                        "id": "P0",
                        "weight": 2.0,
                        "left_response_id": f"candidate-xo{crossover_hz}-left",
                        "right_response_id": f"candidate-xo{crossover_hz}-right",
                    }
                ],
            }
        )

    separated_references = []
    for channel in ("left", "right"):
        for repeat in ("a", "b"):
            separated_references.append(
                {
                    "id": f"xo80-{channel}-sub-{repeat}",
                    "crossover_hz": 80.0,
                    "channel": channel,
                    "main_response_id": f"first-{channel}-main-xo80",
                    "sub_response_id": f"first-sub-xo80-{repeat}",
                    "combined_response_id": f"first-{channel}-combined-xo80",
                }
            )

    return {
        "schema_version": SCHEMA_VERSION,
        "extraction": {
            "extractor_version": EXTRACTOR_VERSION,
            "dependency": "javaobj-py3==0.5.0 (development only)",
            "preferred_future_path": "REW 5.40+ local REST API",
            "source_rew_build": "5.31.3; local API startup rejected by this build",
            "frequency_range_hz": [MIN_FREQUENCY_HZ, MAX_FREQUENCY_HZ],
            "level_alignment_applied": False,
            "timeline_shift_applied": False,
        },
        "frequency_grid_hz": frequencies_hz,
        "responses": responses,
        "candidates": candidates,
        "separated_references": separated_references,
        "repeatability_groups": [
            {
                "id": "raw-left-p0",
                "response_ids": ["first-left-raw-a", "first-left-raw-b"],
            },
            {
                "id": "raw-right-p0",
                "response_ids": ["first-right-raw-a", "first-right-raw-b"],
            },
            {
                "id": "sub-only-p0",
                "response_ids": ["first-sub-xo80-a", "first-sub-xo80-b"],
            },
        ],
        "limitations": [
            "Only crossover 70/80/90 Hz varies in the ranked candidate set.",
            "Sub level and polarity were not supplied for these candidate files and remain unknown.",
            "All candidates are central-position P0 measurements; spatial scoring is unavailable.",
            "A final hardware confirmation measurement is required.",
        ],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--measurements-root",
        type=Path,
        default=Path("measurments"),
        help="directory containing first_measurments and find_sub_crossover",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("measurments/derived/phase3-responses.json"),
    )
    arguments = parser.parse_args()

    fixture = build_fixture(arguments.measurements_root.resolve())
    payload = (json.dumps(fixture, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode()
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_bytes(payload)
    print(f"Wrote {arguments.output} ({len(payload)} bytes, {len(SOURCE_FILES)} sources)")


if __name__ == "__main__":
    main()
