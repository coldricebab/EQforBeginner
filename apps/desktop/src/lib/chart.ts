export type ChartPoint = {
  frequency: number;
  level: number;
};

export type ChartBounds = {
  left: number;
  right: number;
  top: number;
  bottom: number;
  minFrequency: number;
  maxFrequency: number;
  minLevel: number;
  maxLevel: number;
};

export function logX(frequency: number, bounds: ChartBounds): number {
  const clamped = Math.min(
    bounds.maxFrequency,
    Math.max(bounds.minFrequency, frequency),
  );
  const ratio =
    Math.log(clamped / bounds.minFrequency) /
    Math.log(bounds.maxFrequency / bounds.minFrequency);
  return bounds.left + ratio * (bounds.right - bounds.left);
}

export function linearY(level: number, bounds: ChartBounds): number {
  const clamped = Math.min(bounds.maxLevel, Math.max(bounds.minLevel, level));
  const ratio = (clamped - bounds.minLevel) / (bounds.maxLevel - bounds.minLevel);
  return bounds.bottom - ratio * (bounds.bottom - bounds.top);
}

export function buildPath(
  points: ChartPoint[],
  bounds: ChartBounds,
): string {
  return points
    .filter(
      ({ frequency, level }) =>
        Number.isFinite(frequency) &&
        Number.isFinite(level) &&
        frequency >= bounds.minFrequency &&
        frequency <= bounds.maxFrequency,
    )
    .sort((a, b) => a.frequency - b.frequency)
    .map(({ frequency, level }, index) => {
      const command = index === 0 ? "M" : "L";
      return `${command}${logX(frequency, bounds).toFixed(2)},${linearY(level, bounds).toFixed(2)}`;
    })
    .join(" ");
}

export function geometricFrequencies(
  minFrequency: number,
  maxFrequency: number,
  count = 128,
): number[] {
  return Array.from({ length: count }, (_, index) => {
    const ratio = index / (count - 1);
    return minFrequency * Math.pow(maxFrequency / minFrequency, ratio);
  });
}

function gaussian(frequency: number, center: number, width: number): number {
  const distance = Math.log(frequency / center);
  return Math.exp(-(distance * distance) / (2 * width * width));
}

export type CurveKind = "raw" | "target" | "predicted" | "verified";
export type CurveChannel = "left" | "right" | "spatial";

export function syntheticLevel(
  frequency: number,
  kind: CurveKind,
  channel: CurveChannel,
): number {
  const lowShelf = Math.max(0, Math.min(4.2, 1.8 * Math.log2(500 / frequency)));
  const target = frequency <= 650 ? lowShelf : -0.45 * Math.log2(frequency / 650);
  const channelOffset =
    channel === "left"
      ? 0.65 * Math.sin(Math.log(frequency) * 2.1)
      : channel === "right"
        ? -0.5 * Math.cos(Math.log(frequency) * 2.35)
        : 0;

  if (kind === "target") return target;

  const peaks =
    6.8 * gaussian(frequency, 40, 0.1) +
    5.2 * gaussian(frequency, 55, 0.09) +
    4.4 * gaussian(frequency, 66, 0.085) +
    2.1 * gaussian(frequency, 225, 0.14);
  const dips =
    8.2 * gaussian(frequency, 83, 0.045) +
    6.4 * gaussian(frequency, 103, 0.04) +
    5.5 * gaussian(frequency, 119, 0.045);
  const roomVariation =
    channelOffset + 0.85 * Math.sin(Math.log(frequency) * 4.2);

  if (kind === "raw") return target + peaks - dips + roomVariation;

  const residualPeaks =
    1.15 * gaussian(frequency, 40, 0.12) +
    0.8 * gaussian(frequency, 55, 0.11) +
    0.65 * gaussian(frequency, 66, 0.1);
  const taper = frequency > 500 ? Math.min(1, (frequency - 500) / 150) : 0;
  const corrected =
    target +
    residualPeaks -
    dips * 0.82 +
    roomVariation * (0.38 + taper * 0.62) +
    peaks * taper;

  if (kind === "verified") {
    return corrected + 0.25 * Math.sin(Math.log(frequency) * 5.7);
  }
  return corrected;
}
