export type LiveChannel = "left" | "right";
export type LiveCaptureKind =
  | "sub_main_only"
  | "sub_only"
  | "baseline"
  | "verification";
export type LiveSystemMode = "stereo_2_0" | "single_sub_2_1";
export type LiveWizardStage =
  | "session"
  | "inputs"
  | "subwoofer"
  | "baseline"
  | "design"
  | "verify"
  | "export";
export type LiveReferenceChannel =
  | "mono"
  | "left"
  | "right"
  | "identical_stereo";

export type LiveSessionSummary = {
  sessionId: string;
  outputDirectory: string;
  projectVersion: string;
  systemMode: LiveSystemMode;
  systemDeclarationPath: string;
};

export type LiveSubwooferSetupRequest = {
  crossoverHz: number;
  mainDelayMs: number;
  polarityDegrees: 0 | 180;
  subLevelDb: number;
  confirmedOnHardware: boolean;
};

export type LiveSubwooferSetupSummary = LiveSubwooferSetupRequest & {
  algorithmVersion: string;
  settingsPath: string;
};

export type LiveSubwooferSearchRequest = {
  crossoverHz: number[];
  measuredMainDelayMs: number;
  measuredPolarityDegrees: 0 | 180;
  fixedSubLevelDb: number;
  delayMinimumMs: number;
  delayMaximumMs: number;
  delayStepMs: number;
};

export type LiveSubwooferCrossoverCandidate = {
  id: string;
  crossoverHz: number;
};

export type LiveSubwooferSearchSummary = {
  algorithmVersion: string;
  candidates: LiveSubwooferCrossoverCandidate[];
  measuredMainDelayMs: number;
  measuredPolarityDegrees: 0 | 180;
  fixedSubLevelDb: number;
  delayMinimumMs: number;
  delayMaximumMs: number;
  delayStepMs: number;
  fixedTimingReferenceChannel: LiveReferenceChannel;
  subSweepChannel: LiveChannel;
  planPath: string;
};

export type LiveSubwooferRankedSetting = {
  rank: number;
  crossoverHz: number;
  mainDelayMs: number;
  polarityDegrees: 0 | 180;
  totalScore: number;
  deficitRmsDb: number;
  deficitP95Db: number;
  worstDeficitDb: number;
};

export type LiveSubwooferOptimizationSummary = {
  algorithmVersion: string;
  synthesizedCandidateCount: number;
  best: LiveSubwooferRankedSetting;
  rankings: LiveSubwooferRankedSetting[];
  scoringLowerHz: number;
  scoringUpperHz: number;
  fixedSubLevelDb: number;
  needsCombinedConfirmation: boolean;
  arrivalEstimates: {
    crossoverHz: number;
    centerMs: number;
    leftRightSpreadMs: number;
    windowLowMs: number;
    windowHighMs: number;
    rangeLimited: boolean;
  }[];
  subLevelAdvisory: {
    bestGainDb: number;
    deficitRmsAtBestDb: number;
    deficitRmsAtZeroDb: number;
  } | null;
  warnings: string[];
  reportPath: string;
};

export type CalibrationImportSummary = {
  fileName: string;
  sha256: string;
  parserVersion: string;
  serialNumber: string | null;
  sensitivityFactorDb: number | null;
  pointCount: number;
  minimumFrequencyHz: number;
  maximumFrequencyHz: number;
  correctionBandCovered: boolean;
};

export type TargetImportSummary = {
  fileName: string;
  sha256: string;
  parserVersion: string;
  pointCount: number;
  minimumFrequencyHz: number;
  maximumFrequencyHz: number;
  correctionBandCovered: boolean;
  alignmentLowerHz: number;
  alignmentUpperHz: number;
  storedPath: string;
};

export type LiveSweepImportSummary = {
  channel: LiveChannel;
  sha256: string;
  sourceChannels: number;
  sampleRateHz: number;
  sourceDurationSeconds: number;
  measurementStartSeconds: number;
  measurementEndSeconds: number;
  measurementDurationSeconds: number;
  measurementPeakDbfs: number;
  sourceReferenceChannel: LiveReferenceChannel;
  timingMarkerCount: number;
  markerChannelAnalysisVersion: string;
  startMarkerChannel: LiveReferenceChannel | null;
  endMarkerChannel: LiveReferenceChannel | null;
  startMarkerChannelSeparationDb: number | null;
  endMarkerChannelSeparationDb: number | null;
};

export type LiveCaptureProgressPhase =
  | "waiting_for_start"
  | "start_marker_detected"
  | "measuring_sweep"
  | "end_marker_detected"
  | "saving_measurement";

export type LiveMeasurementLevelStatus =
  | "waiting"
  | "too_low"
  | "good"
  | "high"
  | "clipping";

export type LiveCaptureProgress = {
  algorithmVersion: string;
  phase: LiveCaptureProgressPhase;
  elapsedSeconds: number;
  peakDbfs: number | null;
  rmsDbfs: number | null;
  estimatedSplDb: number | null;
  levelStatus: LiveMeasurementLevelStatus;
  startMarkerDetected: boolean;
  endMarkerDetected: boolean;
  automaticCompletionArmed: boolean;
};

export type LiveMeasurementLevelAssessment = {
  algorithmVersion: string;
  status: LiveMeasurementLevelStatus;
  acceptableForMeasurement: boolean;
  measurementPeakDbfs: number;
  measurementRmsDbfs: number;
  estimatedSplDb: number | null;
  estimatedSplAssumption: string | null;
  minimumAcceptedPeakDbfs: number;
  recommendedPeakMinimumDbfs: number;
  recommendedPeakMaximumDbfs: number;
  recommendedSplMinimumDb: number;
  recommendedSplMaximumDb: number;
};

export type LiveCaptureSummary = {
  kind: LiveCaptureKind;
  channel: LiveChannel;
  positionId: string;
  inputDeviceId: string;
  inputDeviceName: string | null;
  inputChannelIndex: number;
  sourceChannelCount: number;
  accepted: boolean;
  issueCodes: string[];
  diagnosticCodes: string[];
  audioStreamDiagnostics: {
    xrunCount: number;
    callbackLockDropFrames: number;
    timestampGapFrames: number;
    timestampDiscontinuityCount: number;
    missingSamplesAtEnd: number;
    streamErrorCount: number;
  };
  capturePeakDbfs: number | null;
  captureSnrDb: number | null;
  reconstructionFitDb: number | null;
  reconstructionFitRequired: boolean;
  correlation: number | null;
  clockDriftPpm: number | null;
  startMarkerDetected: boolean;
  endMarkerDetected: boolean;
  automaticCompletionDetected: boolean;
  levelAssessment: LiveMeasurementLevelAssessment;
  capturedFrames: number;
  rawWavPath: string;
  measurementSnapshotPath: string | null;
  frequencyBinCount: number;
  timingEligible: boolean;
  restoredFromCache: boolean;
  cacheSourceSessionId: string | null;
};

export type LiveMeasurementCacheRestoreSummary = {
  algorithmVersion: string;
  sourceSessionId: string | null;
  sourceSessionIds: string[];
  restoredCaptures: LiveCaptureSummary[];
  scannedSnapshotCount: number;
  compatibleSnapshotCount: number;
};

export type LiveDesignSummary = {
  algorithmVersion: string;
  numericalPassed: boolean;
  positionCount: number;
  trialWavPath: string;
  trialZipPath: string;
  leftRawRmseDb: number;
  leftPredictedRmseDb: number;
  rightRawRmseDb: number;
  rightPredictedRmseDb: number;
  maximumAttenuationDb: number;
  maximumBoostDb: number;
  protectedDipsPassed: boolean;
  warning: string;
};

export type LiveSecsResolution = "low" | "normal" | "high";
export type LiveSecsLatencyMode = "normal" | "low" | "zero";
/** App target curve the SECS correction steers toward; "flat" keeps the
 * SECS-native adaptive flat target, the rest match the main target selector. */
export type LiveSecsTargetCurve = "flat" | "bk" | "harman" | "custom";

export type LiveSecsDesignSettings = {
  maxBoostDb: number;
  tiltDbPerOctave: number;
  bassBoostDb: number;
  bassFrequencyHz: number;
  resolution: LiveSecsResolution;
  curtainHz: number;
  latencyMode: LiveSecsLatencyMode;
  fixedDelayMs: number | null;
  multiPosition: boolean;
  targetCurve: LiveSecsTargetCurve;
};

/// SECS.py "Flat" preset — the backend rejects out-of-range values instead
/// of clamping, so the UI constrains inputs to the same limits.
export const SECS_DEFAULT_SETTINGS: LiveSecsDesignSettings = {
  maxBoostDb: 6,
  tiltDbPerOctave: 0,
  bassBoostDb: 0,
  bassFrequencyHz: 80,
  resolution: "normal",
  curtainHz: 300,
  latencyMode: "normal",
  fixedDelayMs: null,
  multiPosition: true,
  targetCurve: "flat",
};

export type LiveSecsDesignSummary = {
  settings: LiveSecsDesignSettings;
  algorithmVersion: string;
  positionId: string;
  positionCount: number;
  multiPositionApplied: boolean;
  /** 2.1: crossover below which the L/R correction is commonized; null on 2.0. */
  sharedSubBandHz: number | null;
  sampleRateHz: number;
  taps: number;
  autoDelayMs: number;
  lowCutoffHz: number;
  highCutoffHz: number;
  preampDb: number;
  channelBalanceTrimDb: number;
  inputPhaseScore: number;
  leftRawRmseDb: number;
  leftPredictedRmseDb: number;
  rightRawRmseDb: number;
  rightPredictedRmseDb: number;
  trialWavPath: string;
  trialZipPath: string;
  /** Predicted-only raw/target/predicted display curves (verified empty). */
  frequencyResponse: LiveFrequencyResponsePlot;
  warning: string;
};

export type LiveFrequencyResponsePlot = {
  algorithmVersion: string;
  displaySmoothingFwhmOctaves: number;
  frequenciesHz: number[];
  rawLeftDb: number[];
  rawRightDb: number[];
  rawAverageDb: number[];
  targetLeftDb: number[];
  targetRightDb: number[];
  targetAverageDb: number[];
  predictedLeftDb: number[];
  predictedRightDb: number[];
  predictedAverageDb: number[];
  verifiedLeftDb: number[];
  verifiedRightDb: number[];
  verifiedAverageDb: number[];
  correctionLowHz: number;
  correctionHighHz: number;
  taperEndHz: number;
  protectedDipFrequenciesHz: number[];
  correctedPeakFrequenciesHz: number[];
};

export type LiveVerificationSummary = {
  algorithmVersion: string;
  passed: boolean;
  leftPassed: boolean;
  rightPassed: boolean;
  leftRawRmseDb: number;
  leftVerifiedRmseDb: number;
  rightRawRmseDb: number;
  rightVerifiedRmseDb: number;
  leftPredictedVerifiedRmseDb: number;
  rightPredictedVerifiedRmseDb: number;
  leftUnsmoothedPredictedVerifiedRmseDb: number;
  rightUnsmoothedPredictedVerifiedRmseDb: number;
  leftGateRawRmseDb?: number | null;
  leftGateVerifiedRmseDb?: number | null;
  rightGateRawRmseDb?: number | null;
  rightGateVerifiedRmseDb?: number | null;
  predictionVerificationSmoothingFwhmOctaves: number;
  maximumAllowedPredictedVerifiedRmseDb: number;
  issues: string[];
  frequencyResponse: LiveFrequencyResponsePlot;
};

export type LiveExportSummary = {
  zipPath: string;
  projectPath: string;
  zipSha256: string;
  algorithmVersion: string;
  recommendedHeadroomDb: number;
  measuredTruePeakRatioDb: number;
  /** Worst program-material peak growth through any rate member (SECS). */
  programMaterialPeakGrowthDb: number;
  firWorstCasePeakBoundDb: number;
  final48kBindingMaximumMagnitudeDifferenceDb: number;
  final48kBindingMaximumRelativeGroupDelayDifferenceMs: number;
  nativeRateCount: number;
  crossRatePassed: boolean;
  /** null when the user explicitly skipped verification (predicted-only). */
  verification: LiveVerificationSummary | null;
};

export type LiveZipArtifactKind = "trial" | "final";

export type LiveZipDownloadSummary = {
  artifactKind: LiveZipArtifactKind;
  fileName: string;
  savedPath: string;
  byteCount: number;
  sha256: string;
};

export const LIVE_BASELINE_POSITIONS = [
  "P0",
  "P1",
  "P2",
  "P3",
  "P4",
  "P5",
  "P6",
  "P7",
  "P8",
  "P0_END",
] as const;

export type LiveBaselinePosition = (typeof LIVE_BASELINE_POSITIONS)[number];

export const LIVE_CHANNELS: readonly LiveChannel[] = ["left", "right"];
export const MAX_LIVE_CALIBRATION_BYTES = 2 * 1024 * 1024;
export const MAX_LIVE_TARGET_BYTES = 2 * 1024 * 1024;
export const MAX_LIVE_SWEEP_BYTES = 32 * 1024 * 1024;

export type LiveCaptureIssueGuidance =
  | "too_low"
  | "high"
  | "clipping"
  | "noise"
  | "interrupted"
  | "incomplete"
  | "reconstruction"
  | "unstable"
  | "generic";

export function classifyLiveCaptureIssues(
  issueCodes: readonly string[],
): LiveCaptureIssueGuidance {
  const joined = issueCodes.join(" ").toLowerCase();
  if (joined.includes("measurement_level_too_low")) return "too_low";
  if (
    joined.includes("measurement_level_too_high") ||
    joined.includes("measurement_level_clipping")
  ) {
    return "high";
  }
  if (joined.includes("clipping") || joined.includes("clipped")) {
    return "clipping";
  }
  if (joined.includes("snr") || joined.includes("noise")) return "noise";
  if (
    joined.includes("sample_drop") ||
    joined.includes("stream_") ||
    joined.includes("callback")
  ) {
    return "interrupted";
  }
  if (joined.includes("incomplete") || joined.includes("timed_out")) {
    return "incomplete";
  }
  if (joined.includes("reconstruction")) return "reconstruction";
  if (
    joined.includes("timing") ||
    joined.includes("drift") ||
    joined.includes("correlation")
  ) {
    return "unstable";
  }
  return "generic";
}

export function liveWizardStages(
  systemMode: LiveSystemMode | null,
): readonly LiveWizardStage[] {
  const common: LiveWizardStage[] = ["session", "inputs"];
  if (systemMode === "single_sub_2_1") common.push("subwoofer");
  return [...common, "baseline", "design", "verify", "export"];
}

export function captureKey(
  kind: LiveCaptureKind,
  positionId: string,
  channel: LiveChannel,
): string {
  return `${kind}:${positionId}:${channel}`;
}

export function acceptedPairCount(
  captures: Readonly<Record<string, LiveCaptureSummary>>,
): number {
  return LIVE_BASELINE_POSITIONS.filter((positionId) =>
    LIVE_CHANNELS.every(
      (channel) =>
        captures[captureKey("baseline", positionId, channel)]?.accepted === true,
    ),
  ).length;
}

export function hasAcceptedP0(
  captures: Readonly<Record<string, LiveCaptureSummary>>,
  kind: LiveCaptureKind,
): boolean {
  return hasAcceptedPair(captures, kind, "P0");
}

export function hasAcceptedPair(
  captures: Readonly<Record<string, LiveCaptureSummary>>,
  kind: LiveCaptureKind,
  positionId: string,
): boolean {
  return LIVE_CHANNELS.every(
    (channel) =>
      captures[captureKey(kind, positionId, channel)]?.accepted === true,
  );
}

export function parseCrossoverCandidates(value: string): number[] | null {
  const tokens = value
    .split(/[\s,]+/)
    .map((token) => token.trim())
    .filter(Boolean);
  if (tokens.length < 2 || tokens.length > 12) return null;
  const crossovers = tokens.map(Number);
  if (
    crossovers.some(
      (crossover, index) =>
        !Number.isFinite(crossover) ||
        crossover < 30 ||
        crossover > 200 ||
        (index > 0 && crossover <= crossovers[index - 1]),
    )
  ) {
    return null;
  }
  return crossovers;
}

export function acceptedSubTripletCount(
  captures: Readonly<Record<string, LiveCaptureSummary>>,
  search: LiveSubwooferSearchSummary | null,
): number {
  if (!search) return 0;
  return search.candidates.filter(
    (candidate) =>
      captures[
        captureKey("sub_main_only", candidate.id, "left")
      ]?.accepted === true &&
      captures[
        captureKey("sub_main_only", candidate.id, "right")
      ]?.accepted === true &&
      captures[
        captureKey("sub_only", candidate.id, search.subSweepChannel)
      ]?.accepted === true,
  ).length;
}

export function formatMetric(
  value: number | null | undefined,
  suffix: string,
  digits = 2,
): string {
  return value === null || value === undefined || !Number.isFinite(value)
    ? "—"
    : `${value.toFixed(digits)} ${suffix}`.trim();
}
