import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { messages } from "../i18n";
import type {
  LiveDesignSummary,
  LiveFrequencyResponsePlot,
  LiveVerificationSummary,
} from "../lib/liveMeasurement";
import { FinalMeasurementResults } from "./FinalMeasurementResults";

const frequenciesHz = [20, 50, 100, 200, 500, 650, 1_000, 20_000];
const response = (offset: number) =>
  frequenciesHz.map((_, index) => 72 - index * 0.5 + offset);
const measuredPlotFixture: LiveFrequencyResponsePlot = {
  algorithmVersion: "measured-fr-result-plot-v2",
  displaySmoothingFwhmOctaves: 1 / 12,
  frequenciesHz,
  rawLeftDb: response(4),
  rawRightDb: response(3),
  rawAverageDb: response(3.5),
  targetLeftDb: response(0),
  targetRightDb: response(-0.2),
  targetAverageDb: response(-0.1),
  predictedLeftDb: response(0.8),
  predictedRightDb: response(0.6),
  predictedAverageDb: response(0.7),
  verifiedLeftDb: response(1),
  verifiedRightDb: response(0.7),
  verifiedAverageDb: response(0.85),
  correctionLowHz: 20,
  correctionHighHz: 500,
  taperEndHz: 650,
  protectedDipFrequenciesHz: [100],
  correctedPeakFrequenciesHz: [50],
};

const design: LiveDesignSummary = {
  algorithmVersion: "phase4-response-replay-v2",
  numericalPassed: true,
  positionCount: 7,
  trialWavPath: "/tmp/trial.wav",
  trialZipPath: "/tmp/trial.zip",
  leftRawRmseDb: 5.5,
  leftPredictedRmseDb: 2.2,
  rightRawRmseDb: 5,
  rightPredictedRmseDb: 2,
  maximumAttenuationDb: 8.42,
  maximumBoostDb: 2.31,
  protectedDipsPassed: true,
  adaptiveHf: null,
  warning: "predicted only",
};

const verification: LiveVerificationSummary = {
  algorithmVersion: "live-closed-loop-validation-v2",
  passed: true,
  leftPassed: true,
  rightPassed: true,
  leftRawRmseDb: 5.5,
  leftVerifiedRmseDb: 2.1,
  rightRawRmseDb: 5,
  rightVerifiedRmseDb: 2,
  leftPredictedVerifiedRmseDb: 0.62,
  rightPredictedVerifiedRmseDb: 0.71,
  leftUnsmoothedPredictedVerifiedRmseDb: 3.8,
  rightUnsmoothedPredictedVerifiedRmseDb: 4.1,
  predictionVerificationSmoothingFwhmOctaves: 1 / 12,
  maximumAllowedPredictedVerifiedRmseDb: 3,
  issues: [],
  frequencyResponse: measuredPlotFixture,
};

describe("FinalMeasurementResults", () => {
  it("shows the final measured dashboard with FR and exact metrics", () => {
    const html = renderToStaticMarkup(
      <FinalMeasurementResults
        copy={messages.ko.liveMeasurement}
        chartCopy={messages.ko.chart}
        design={design}
        verification={verification}
        exported={null}
      />,
    );

    expect(html).toContain("보정 전·후 결과");
    expect(html).toContain("실측 검증 근거");
    expect(html).toContain("5.500 dB");
    expect(html).toContain("2.100 dB");
    expect(html).toContain("61.8% 개선");
    expect(html).toContain("0.620 dB");
    expect(html).toContain("보호된 딥 유지 통과");
    expect(html).toContain("실측 주파수응답");
  });
});
