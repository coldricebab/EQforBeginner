export type WirelessSweepReferenceChannel =
  | "mono"
  | "left"
  | "right"
  | "identical_stereo";

export type WirelessSweepImport = {
  sweepId: string;
  sha256: string;
  sampleRateHz: number;
  channels: number;
  frameCount: number;
  durationSeconds: number;
  referenceChannel: WirelessSweepReferenceChannel;
  peakDbfs: number;
  rmsDbfs: number;
  sweepLike: boolean;
};

export type WirelessSweepDetectionStatus =
  | "detected"
  | "not_detected"
  | "ambiguous";

export type WirelessSweepCapture = {
  sweepId: string;
  status: WirelessSweepDetectionStatus;
  sampleRateHz: number;
  capturedFrames: number;
  detectedStartSeconds: number | null;
  detectedEndSeconds: number | null;
  correlation: number | null;
  secondBestCorrelation: number | null;
  confidenceMargin: number | null;
  clockDriftPpm: number | null;
  inputPeakDbfs: number | null;
  clippedSamples: number;
  streamErrorCount: number;
  dropoutSuspected: boolean;
  qualityAccepted: boolean;
  timingEligible: boolean;
};

export type WirelessSweepUiState =
  | "empty"
  | "importing"
  | "ready"
  | "listening"
  | "detected"
  | "not_detected"
  | "ambiguous"
  | "error";

export const MAX_WIRELESS_SWEEP_BYTES = 32 * 1024 * 1024;

export function inputDeviceIdFromChoice(value: string): string | null {
  if (!value) return null;
  try {
    const parsed: unknown = JSON.parse(value);
    if (
      Array.isArray(parsed) &&
      typeof parsed[0] === "string" &&
      parsed[0].trim() !== ""
    ) {
      return parsed[0];
    }
  } catch {
    return null;
  }
  return null;
}

export function inputChannelIndexFromChoice(value: string): number | null {
  if (!value) return null;
  try {
    const parsed: unknown = JSON.parse(value);
    if (!Array.isArray(parsed)) return null;
    // Five-field choices predate explicit multichannel extraction and mean
    // the first input channel.
    if (parsed.length === 5) return 0;
    const channelIndex = parsed[5];
    return typeof channelIndex === "number" &&
      Number.isSafeInteger(channelIndex) &&
      channelIndex >= 0 &&
      channelIndex <= 65_535
      ? channelIndex
      : null;
  } catch {
    return null;
  }
}

export function formatDbfs(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "—";
  return `${value.toFixed(1)} dBFS`;
}

export function formatPpm(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "—";
  const prefix = value > 0 ? "+" : "";
  return `${prefix}${value.toFixed(1)} ppm`;
}

export function formatPercent(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "—";
  return `${(value * 100).toFixed(1)}%`;
}
