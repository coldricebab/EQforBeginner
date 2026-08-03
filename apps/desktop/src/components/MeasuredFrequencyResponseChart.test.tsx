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

  it("keeps the zoomed view at 500 Hz when the algorithm corrects the full band", () => {
    // SECS reports 20 kHz as its correction range, which used to make the
    // zoomed view identical to the full-band one. The 500 Hz gridline must
    // land on the plot's right edge (x = 906) in the default zoomed view.
    const secsPlot: LiveFrequencyResponsePlot = {
      ...measuredPlotFixture,
      correctionHighHz: 20_000,
      taperEndHz: 20_000,
    };
    const html = renderToStaticMarkup(
      <MeasuredFrequencyResponseChart copy={messages.ko.chart} data={secsPlot} />,
    );

    expect(html).toContain('class="grid-line" x1="906" x2="906"');
    expect(html).not.toContain(">20k<");
  });

  it("aligns the displayed curve levels and reports the applied offsets", () => {
    // A mixed-phase filter attenuates broadly: predicted and verified sit
    // 6 dB below raw here. The traces are lifted onto the raw level for
    // shape comparison and the offset is stated under the chart.
    const attenuated: LiveFrequencyResponsePlot = {
      ...measuredPlotFixture,
      predictedLeftDb: shifted(3.5 - 6),
      predictedRightDb: shifted(3.5 - 6),
      predictedAverageDb: shifted(3.5 - 6),
      verifiedLeftDb: shifted(3.5 - 6),
      verifiedRightDb: shifted(3.5 - 6),
      verifiedAverageDb: shifted(3.5 - 6),
    };
    const html = renderToStaticMarkup(
      <MeasuredFrequencyResponseChart
        copy={messages.ko.chart}
        data={attenuated}
      />,
    );

    expect(html).toContain("표시 레벨 정렬(200–500 Hz 중앙값 기준");
    expect(html).toContain("Predicted +6.0 dB");
    expect(html).toContain("Verified +6.0 dB");
    // Raw is the reference, so it is never moved and never listed.
    expect(html).not.toContain("Raw +");
    // The aligned predicted trace now coincides with raw: same path data.
    const paths = [
      ...html.matchAll(
        /class="response-line trace--(raw|predicted) channel--spatial" d="([^"]+)"/g,
      ),
    ];
    expect(paths).toHaveLength(2);
    expect(paths[0][2]).toBe(paths[1][2]);
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
