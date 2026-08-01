import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { messages } from "../i18n";
import type { LiveFrequencyResponsePlot } from "../lib/liveMeasurement";
import { MeasuredFrequencyResponseChart } from "./MeasuredFrequencyResponseChart";

const frequenciesHz = [20, 50, 100, 200, 500, 650, 1_000, 20_000];
const shifted = (offset: number) =>
  frequenciesHz.map((_, index) => 72 - index * 0.5 + offset);

export const measuredPlotFixture: LiveFrequencyResponsePlot = {
  algorithmVersion: "measured-fr-result-plot-v2",
  displaySmoothingFwhmOctaves: 1 / 12,
  frequenciesHz,
  rawLeftDb: shifted(4),
  rawRightDb: shifted(3),
  rawAverageDb: shifted(3.5),
  targetLeftDb: shifted(0),
  targetRightDb: shifted(-0.2),
  targetAverageDb: shifted(-0.1),
  predictedLeftDb: shifted(0.8),
  predictedRightDb: shifted(0.6),
  predictedAverageDb: shifted(0.7),
  verifiedLeftDb: shifted(1),
  verifiedRightDb: shifted(0.7),
  verifiedAverageDb: shifted(0.85),
  correctionLowHz: 20,
  correctionHighHz: 500,
  taperEndHz: 650,
  protectedDipFrequenciesHz: [100],
  correctedPeakFrequenciesHz: [50],
};

describe("MeasuredFrequencyResponseChart", () => {
  it("renders measured curve controls, actual SVG paths, and result markers", () => {
    const html = renderToStaticMarkup(
      <MeasuredFrequencyResponseChart
        copy={messages.ko.chart}
        data={measuredPlotFixture}
      />,
    );

    expect(html).toContain("실측 주파수응답");
    expect(html).toContain("다지점 Raw·예측");
    expect(html).toContain("measured-fr-result-plot-v2");
    expect(html).toContain("표시 스무딩: 1/12옥타브 Gaussian");
    expect(html).toContain("수치 판정 기준은 변경 없음");
    expect(html).toContain("Raw");
    expect(html).toContain("Verified");
    expect(html).toContain("L/R 에너지 평균");
    expect(html).toContain('class="response-line trace--verified channel--spatial"');
    expect(html).toContain("marker marker--protected");
    expect(html).toContain("marker marker--corrected");
    expect(html).toContain("필터 적용 후 중앙 P0 재측정");
    expect(html).not.toContain("합성 예시");
  });

  it("hides the verified toggle and trace on a predicted-only plot", () => {
    const predictedOnly: LiveFrequencyResponsePlot = {
      ...measuredPlotFixture,
      verifiedLeftDb: [],
      verifiedRightDb: [],
      verifiedAverageDb: [],
    };
    const html = renderToStaticMarkup(
      <MeasuredFrequencyResponseChart
        copy={messages.ko.chart}
        data={predictedOnly}
      />,
    );

    expect(html).toContain("Raw");
    expect(html).toContain("Predicted");
    expect(html).not.toContain(">Verified<");
    expect(html).not.toContain("trace--verified");
  });
});
