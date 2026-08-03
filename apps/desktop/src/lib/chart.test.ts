import { describe, expect, it } from "vitest";
import {
  bandMedianDb,
  buildPath,
  geometricFrequencies,
  linearY,
  logX,
  syntheticLevel,
  type ChartBounds,
} from "./chart";

const bounds: ChartBounds = {
  left: 50,
  right: 850,
  top: 20,
  bottom: 280,
  minFrequency: 20,
  maxFrequency: 500,
  minLevel: -14,
  maxLevel: 10,
};

describe("chart geometry", () => {
  it("maps logarithmic and linear endpoints exactly", () => {
    expect(logX(20, bounds)).toBe(50);
    expect(logX(500, bounds)).toBe(850);
    expect(linearY(-14, bounds)).toBe(280);
    expect(linearY(10, bounds)).toBe(20);
  });

  it("creates a sorted path and drops invalid points", () => {
    const path = buildPath(
      [
        { frequency: 100, level: 1 },
        { frequency: Number.NaN, level: 2 },
        { frequency: 20, level: 0 },
      ],
      bounds,
    );
    expect(path).toMatch(/^M50\.00,/);
    expect(path.match(/[ML]/g)).toHaveLength(2);
  });

  it("takes the band median only from finite in-band samples", () => {
    const frequencies = [20, 100, 200, 300, 400, 500, 1_000];
    const levels = [99, 99, 4, 2, 6, 8, -99];
    // In-band values are 4, 2, 6, 8 -> median 5.
    expect(bandMedianDb(frequencies, levels, 200, 500)).toBe(5);
    // Odd count picks the middle sample.
    expect(bandMedianDb(frequencies, levels, 200, 400)).toBe(4);
    // A non-finite sample is skipped rather than poisoning the median.
    expect(
      bandMedianDb([200, 300, 400], [2, Number.NaN, 6], 200, 500),
    ).toBe(4);
    // An empty band reports no reference instead of guessing one.
    expect(bandMedianDb(frequencies, levels, 600, 900)).toBeNull();
  });

  it("keeps deterministic synthetic traces finite", () => {
    const frequencies = geometricFrequencies(20, 20_000, 192);
    for (const frequency of frequencies) {
      expect(Number.isFinite(syntheticLevel(frequency, "raw", "left"))).toBe(true);
      expect(Number.isFinite(syntheticLevel(frequency, "predicted", "spatial"))).toBe(
        true,
      );
    }
  });
});
