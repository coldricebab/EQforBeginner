import { describe, expect, it } from "vitest";
import {
  acceptedSubTripletCount,
  acceptedPairCount,
  captureKey,
  classifyLiveCaptureIssues,
  expectedSubTripletCount,
  formatMetric,
  hasAcceptedP0,
  liveWizardStages,
  parseCrossoverCandidates,
  WIDE_BAND_MAIN_POSITION_ID,
  WIDE_BAND_SUB_POSITION_ID,
  type LiveCaptureKind,
  type LiveCaptureSummary,
  type LiveSubwooferSearchSummary,
} from "./liveMeasurement";

function capture(
  kind: LiveCaptureKind,
  positionId: string,
  channel: "left" | "right",
  accepted = true,
): LiveCaptureSummary {
  return {
    kind,
    positionId,
    channel,
    inputDeviceId: "test-device",
    inputDeviceName: "Test microphone",
    inputChannelIndex: 0,
    sourceChannelCount: 2,
    accepted,
    issueCodes: [],
    diagnosticCodes: [],
    audioStreamDiagnostics: {
      xrunCount: 0,
      callbackLockDropFrames: 0,
      timestampGapFrames: 0,
      timestampDiscontinuityCount: 0,
      missingSamplesAtEnd: 0,
      streamErrorCount: 0,
    },
    capturePeakDbfs: -12,
    captureSnrDb: 40,
    reconstructionFitDb: -30,
    reconstructionFitRequired: true,
    correlation: 0.99,
    clockDriftPpm: 1,
    startMarkerDetected: true,
    endMarkerDetected: true,
    automaticCompletionDetected: true,
    levelAssessment: {
      algorithmVersion: "test-level-v1",
      status: "good",
      acceptableForMeasurement: true,
      measurementPeakDbfs: -12,
      measurementRmsDbfs: -24,
      estimatedSplDb: 76,
      estimatedSplAssumption: "test assumption",
      minimumAcceptedPeakDbfs: -48,
      recommendedPeakMinimumDbfs: -30,
      recommendedPeakMaximumDbfs: -6,
      recommendedSplMinimumDb: 65,
      recommendedSplMaximumDb: 85,
    },
    capturedFrames: 48_000,
    rawWavPath: "/tmp/capture.wav",
    measurementSnapshotPath: "/tmp/capture.json",
    frequencyBinCount: 100,
    timingEligible: false,
    restoredFromCache: false,
    cacheSourceSessionId: null,
  };
}

describe("live measurement workflow helpers", () => {
  it("requires an accepted L/R pair before admitting P0", () => {
    const captures: Record<string, LiveCaptureSummary> = {};
    captures[captureKey("baseline", "P0", "left")] = capture(
      "baseline",
      "P0",
      "left",
    );
    expect(hasAcceptedP0(captures, "baseline")).toBe(false);

    captures[captureKey("baseline", "P0", "right")] = capture(
      "baseline",
      "P0",
      "right",
    );
    expect(hasAcceptedP0(captures, "baseline")).toBe(true);
    expect(acceptedPairCount(captures)).toBe(1);
  });

  it("does not count a rejected surrounding-position pair", () => {
    const captures: Record<string, LiveCaptureSummary> = {
      [captureKey("baseline", "P1", "left")]: capture(
        "baseline",
        "P1",
        "left",
      ),
      [captureKey("baseline", "P1", "right")]: capture(
        "baseline",
        "P1",
        "right",
        false,
      ),
    };
    expect(acceptedPairCount(captures)).toBe(0);
  });

  it("formats missing and finite metrics safely", () => {
    expect(formatMetric(null, "dB")).toBe("—");
    expect(formatMetric(-12.345, "dBFS", 1)).toBe("-12.3 dBFS");
  });

  it("reports reconstruction failure separately from timing instability", () => {
    expect(
      classifyLiveCaptureIssues([
        "ReconstructionFitTooLow { measured_db: 6.25, required_db: 12.0 }",
      ]),
    ).toBe("reconstruction");
    expect(
      classifyLiveCaptureIssues([
        "TimingFitUnstable { measured_rms_samples: 120.0 }",
      ]),
    ).toBe("unstable");
  });

  it("adds the manual single-sub gate only for a 2.1 project", () => {
    expect(liveWizardStages("stereo_2_0")).toEqual([
      "session",
      "inputs",
      "baseline",
      "design",
      "verify",
      "export",
    ]);
    expect(liveWizardStages("single_sub_2_1")).toEqual([
      "session",
      "inputs",
      "subwoofer",
      "baseline",
      "design",
      "verify",
      "export",
    ]);
  });

  it("parses only bounded, ascending real crossover candidates", () => {
    expect(parseCrossoverCandidates("70, 80  90")).toEqual([70, 80, 90]);
    expect(parseCrossoverCandidates("80")).toBeNull();
    expect(parseCrossoverCandidates("80, 70")).toBeNull();
    expect(parseCrossoverCandidates("70, 70")).toBeNull();
    expect(parseCrossoverCandidates("20, 80")).toBeNull();
  });

  it("counts a crossover only after both mains and the required sub sweep pass", () => {
    const search: LiveSubwooferSearchSummary = {
      algorithmVersion: "test-plan-v1",
      mode: "measured_states",
      candidates: [
        { id: "XO01", crossoverHz: 70 },
        { id: "XO02", crossoverHz: 90 },
      ],
      measuredMainDelayMs: 0,
      measuredPolarityDegrees: 0,
      fixedSubLevelDb: 0,
      delayMinimumMs: 0,
      delayMaximumMs: 5,
      delayStepMs: 0.05,
      subMeasuredLowPassHz: null,
      mainHighPassSlope: null,
      subLowPassSlope: null,
      fixedTimingReferenceChannel: "right",
      subSweepChannel: "left",
      planPath: "/tmp/plan.json",
    };
    const captures: Record<string, LiveCaptureSummary> = {
      [captureKey("sub_main_only", "XO01", "left")]: capture(
        "sub_main_only",
        "XO01",
        "left",
      ),
      [captureKey("sub_main_only", "XO01", "right")]: capture(
        "sub_main_only",
        "XO01",
        "right",
      ),
      [captureKey("sub_only", "XO01", "right")]: capture(
        "sub_only",
        "XO01",
        "right",
      ),
    };
    expect(acceptedSubTripletCount(captures, search)).toBe(0);
    captures[captureKey("sub_only", "XO01", "left")] = capture(
      "sub_only",
      "XO01",
      "left",
    );
    expect(acceptedSubTripletCount(captures, search)).toBe(1);
    expect(expectedSubTripletCount(search)).toBe(2);
  });

  it("treats the wide-band plan as one shared capture triplet", () => {
    const search: LiveSubwooferSearchSummary = {
      algorithmVersion: "test-plan-v2",
      mode: "wide_band",
      candidates: [
        { id: "XO01", crossoverHz: 60 },
        { id: "XO02", crossoverHz: 90 },
      ],
      measuredMainDelayMs: 0,
      measuredPolarityDegrees: 0,
      fixedSubLevelDb: 0,
      delayMinimumMs: -10,
      delayMaximumMs: 25,
      delayStepMs: 0.1,
      subMeasuredLowPassHz: 250,
      mainHighPassSlope: "lr4",
      subLowPassSlope: "lr4",
      fixedTimingReferenceChannel: "right",
      subSweepChannel: "left",
      planPath: "/tmp/plan.json",
    };
    expect(expectedSubTripletCount(search)).toBe(1);
    const captures: Record<string, LiveCaptureSummary> = {
      [captureKey("sub_main_only", WIDE_BAND_MAIN_POSITION_ID, "left")]:
        capture("sub_main_only", WIDE_BAND_MAIN_POSITION_ID, "left"),
      [captureKey("sub_main_only", WIDE_BAND_MAIN_POSITION_ID, "right")]:
        capture("sub_main_only", WIDE_BAND_MAIN_POSITION_ID, "right"),
    };
    expect(acceptedSubTripletCount(captures, search)).toBe(0);
    captures[captureKey("sub_only", WIDE_BAND_SUB_POSITION_ID, "left")] =
      capture("sub_only", WIDE_BAND_SUB_POSITION_ID, "left");
    expect(acceptedSubTripletCount(captures, search)).toBe(1);
  });
});
