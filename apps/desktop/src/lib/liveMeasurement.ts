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

/**
 * `wide_band` measures the sub once with the bass-management low-pass at its
 * maximum and both mains once full range (sub output off), then synthesizes
 * every candidate crossover from the declared slope models. `measured_states`
 * physically measures every candidate (model-free).
 */
export type LiveSubwooferSearchMode = "measured_states" | "wide_band";
export type LiveCrossoverSlopeModel = "lr4" | "lr2" | "bw2";

/** Wide-band mode's fixed capture roles (position ids). */
export const WIDE_BAND_MAIN_POSITION_ID = "FULL";
export const WIDE_BAND_SUB_POSITION_ID = "WIDE";

export type LiveSubwooferSearchRequest = {
  crossoverHz: number[];
  measuredMainDelayMs: number;
  measuredPolarityDegrees: 0 | 180;
  fixedSubLevelDb: number;
  delayMinimumMs: number;
  delayMaximumMs: number;
  delayStepMs: number;
  mode: LiveSubwooferSearchMode;
  subMeasuredLowPassHz: number | null;
  mainHighPassSlope: LiveCrossoverSlopeModel | null;
  subLowPassSlope: LiveCrossoverSlopeModel | null;
};

export type LiveSubwooferCrossoverCandidate = {
  id: string;
  crossoverHz: number;
};

export type LiveSubwooferSearchSummary = {
  algorithmVersion: string;
  mode: LiveSubwooferSearchMode;
  candidates: LiveSubwooferCrossoverCandidate[];
  measuredMainDelayMs: number;
  measuredPolarityDegrees: 0 | 180;
  fixedSubLevelDb: number;
  delayMinimumMs: number;
  delayMaximumMs: number;
  delayStepMs: number;
  subMeasuredLowPassHz: number | null;
  mainHighPassSlope: LiveCrossoverSlopeModel | null;
  subLowPassSlope: LiveCrossoverSlopeModel | null;
  fixedTimingReferenceChannel: LiveReferenceChannel;
  subSweepChannel: LiveChannel;
  planPath: string;
};

export type LiveSubwooferRankedSetting = {
  rank: number;
  crossoverHz: number;
  /** Negative means the delay belongs on the subwoofer output instead. */
  mainDelayMs: number;
  polarityDegrees: 0 | 180;
  /** Deployment sub-level change (dB re the measured level) this candidate
   * was scored with — part of the recommendation. */
  subTrimDb: number;
  totalScore: number;
  deficitRmsDb: number;
  deficitP95Db: number;
  worstDeficitDb: number;
};

export type LiveSubwooferOptimizationSummary = {
  algorithmVersion: string;
  mode: LiveSubwooferSearchMode;
  subMeasuredLowPassHz: number | null;
  mainHighPassSlope: LiveCrossoverSlopeModel | null;
  subLowPassSlope: LiveCrossoverSlopeModel | null;
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
  /** True only for the identity stand-in used when no TXT was imported. */
  uncalibrated: boolean;
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

/**
 * The sweep pair embedded in the app, imported through the ordinary path.
 *
 * A session loads this the moment it starts, so measuring never waits on a
 * file choice; choosing a sweep WAV replaces it for that channel only.
 */
export type BuiltInSweepImportSummary = {
  leftFileName: string;
  rightFileName: string;
  left: LiveSweepImportSummary;
  right: LiveSweepImportSummary;
};

/** Where a channel's loaded sweep came from. */
export type LiveSweepOrigin = "built_in" | "chosen_file";

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

/**
 * What one in-app sweep playback did to the output device.
 *
 * Playback sits outside the measurement path, so the panel reads only the part
 * that outlives the sweep: a device borrowed at 48 kHz and not handed back.
 */
export type LiveSweepPlaybackReport = {
  deviceRateBeforeHz: number | null;
  deviceRateForced: boolean;
  deviceRateRestored: boolean;
  deviceRateRestoreError: string | null;
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
  /** How many restored measurements were admitted only by the debug
   * relaxation (mismatched subwoofer conditions or microphone). Nonzero
   * means the session's evidence is mixed. */
  debugRelaxedSnapshotCount: number;
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
  /** Adaptive-HF provenance of the built-in default target; null whenever a
   * custom target was designed (the adaptive fit is default-target-only). */
  adaptiveHf: LiveAdaptiveHfSummary | null;
  warning: string;
};

/** Guard codes of the adaptive HF fit, mirrored from dsp-core. */
export type LiveAdaptiveHfFallbackReason =
  | "insufficient_coverage"
  | "too_few_fit_points"
  | "non_finite_measurement"
  | "ill_conditioned_fit";

/** What the measurement-adaptive HF rolloff (`harman-6db-adaptive-hf-v1`)
 * decided for the built-in default target. Derived from the baseline
 * measurement — design provenance, never verification evidence. */
export type LiveAdaptiveHfSummary = {
  algorithmVersion: string;
  applied: boolean;
  fittedSlopeDbPerOctave: number | null;
  breakFrequencyHz: number | null;
  fallbackReason: string | null;
};

/** The built-in default target's bass shelf follows the Harman +6 dB curve
 * published on Dirac's official target-curve resource page. */
export const DIRAC_TARGET_SOURCE_URL =
  "https://www.dirac.com/resources/target-curve";

/** Original SECS by 한플 (Hanpeul); credit link requested by the author. */
export const SECS_ORIGINAL_URL =
  "https://gall.dcinside.com/mgallery/board/view/?id=speakers&no=514096&s_type=search_name&s_keyword=%ED%95%9C%ED%94%8C&page=1";

export type LiveSecsResolution = "low" | "normal" | "high";
export type LiveSecsLatencyMode = "normal" | "low" | "zero";
/** App target curve the SECS correction steers toward; "flat" keeps the
 * SECS-native adaptive flat target, the rest match the main target selector. */
export type LiveSecsTargetCurve = "flat" | "harman_6db" | "custom";

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
  /** Improved SECS: phase guard for unrealizable excess-phase corrections
   * plus a hard group-delay gate. Off = the original algorithm, bit for bit. */
  improvedPhase: boolean;
  /** Delay ceiling in ms. 10 = original SECS; larger values are a music-only
   * trade (more latency) that make late bass genuinely correctable, and the
   * design then uses the ceiling as its target delay. null = automatic: the
   * design measures how late the room's bass arrives and resolves the
   * smallest covering ceiling itself (original 10 when nothing needs more). */
  maximumDelayMs: number | null;
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
  improvedPhase: true,
  maximumDelayMs: null,
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
  /** The delay ceiling the design actually ran with (the automatic setting
   * resolves against the measured pair; export replays exactly this). */
  maximumDelayResolvedMs: number;
  /** Measured low-band advance requirement (worst channel) when the
   * automatic ceiling probe ran; null when the ceiling was manual. */
  delayRequirementMs: number | null;
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
  /** Group delay of the designed filter itself per gate band (worst of L/R,
   * re its own 1–16 kHz baseline) — the timing defect no magnitude chart can
   * show. Hard gate on the improved path, warning on the original. */
  groupDelayReport: LiveSecsGroupDelayBand[];
  /** Predicted-only raw/target/predicted display curves (verified empty). */
  frequencyResponse: LiveFrequencyResponsePlot;
  /** Adaptive-HF provenance when following the built-in default target;
   * null for the flat and custom targets. */
  adaptiveHf: LiveAdaptiveHfSummary | null;
  warning: string;
};

export type LiveSecsGroupDelayBand = {
  lowHz: number;
  highHz: number;
  groupDelayMs: number;
  limitMs: number;
  exceeded: boolean;
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
  if (search.mode === "wide_band") {
    // One shared triplet: both full-range mains plus the wide sub capture.
    return captures[
      captureKey("sub_main_only", WIDE_BAND_MAIN_POSITION_ID, "left")
    ]?.accepted === true &&
      captures[
        captureKey("sub_main_only", WIDE_BAND_MAIN_POSITION_ID, "right")
      ]?.accepted === true &&
      captures[
        captureKey("sub_only", WIDE_BAND_SUB_POSITION_ID, search.subSweepChannel)
      ]?.accepted === true
      ? 1
      : 0;
  }
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

/** Total triplets the current plan expects (wide-band mode has exactly one). */
export function expectedSubTripletCount(
  search: LiveSubwooferSearchSummary | null,
): number {
  if (!search) return 0;
  return search.mode === "wide_band" ? 1 : search.candidates.length;
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
