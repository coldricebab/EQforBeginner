import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { messages } from "../i18n";
import { MeasurementPositionGuide } from "./MeasurementPositionGuide";

describe("MeasurementPositionGuide", () => {
  it("shows distinct written and visual guidance for P1 through P5", () => {
    const html = renderToStaticMarkup(
      <MeasurementPositionGuide copy={messages.en.liveMeasurement} />,
    );

    expect(html).toContain("Top view of P0 through P4");
    expect(html).toContain("25–30 cm left of P0");
    expect(html).toContain("25–30 cm right of P0");
    expect(html).toContain("toward the speakers");
    expect(html).toContain("behind P0");
    expect(html).toContain("10–15 cm directly above P0");
    for (const position of ["P1", "P2", "P3", "P4", "P5"]) {
      expect(html).toContain(`>${position}<`);
    }
  });
});
