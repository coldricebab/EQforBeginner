import { describe, expect, it } from "vitest";
import {
  formatDbfs,
  formatPercent,
  formatPpm,
  inputChannelIndexFromChoice,
  inputDeviceIdFromChoice,
} from "./wirelessSweep";

describe("wireless sweep presentation", () => {
  it("extracts only a valid host-qualified input ID", () => {
    expect(
      inputDeviceIdFromChoice(
        JSON.stringify(["coreaudio:umic", 1, "f32", 64, 512]),
      ),
    ).toBe("coreaudio:umic");
    expect(inputDeviceIdFromChoice("not-json")).toBeNull();
    expect(inputDeviceIdFromChoice(JSON.stringify([42]))).toBeNull();
    expect(
      inputChannelIndexFromChoice(
        JSON.stringify(["coreaudio:umic", 2, "f32", 64, 512, 1]),
      ),
    ).toBe(1);
    expect(
      inputChannelIndexFromChoice(
        JSON.stringify(["coreaudio:legacy", 1, "f32", 64, 512]),
      ),
    ).toBe(0);
    expect(inputChannelIndexFromChoice(JSON.stringify(["x", 2, "f32", 1, 2, -1])))
      .toBeNull();
  });

  it("formats measured values without inventing unavailable numbers", () => {
    expect(formatDbfs(-12.345)).toBe("-12.3 dBFS");
    expect(formatPpm(18.25)).toBe("+18.3 ppm");
    expect(formatPpm(null)).toBe("—");
    expect(formatPercent(0.8125)).toBe("81.3%");
    expect(formatPercent(null)).toBe("—");
  });
});
