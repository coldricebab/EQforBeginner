import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { messages } from "../i18n";
import { MeasurementPositionGuide } from "./MeasurementPositionGuide";

describe("MeasurementPositionGuide", () => {
  it("shows distinct written and visual guidance for the box corners P1 through P8", () => {
    const html = renderToStaticMarkup(
      <MeasurementPositionGuide copy={messages.en.liveMeasurement} />,
    );

    // Every corner of the box is described with all three offsets.
    // (The apostrophe in "box's" renders HTML-escaped, so match around it.)
    expect(html).toContain("four corner columns (P1·P5, P2·P6, P3·P7, P4·P8)");
    expect(html).toContain("Upper front-left corner");
    expect(html).toContain("Upper front-right corner");
    expect(html).toContain("Upper back-right corner");
    expect(html).toContain("Upper back-left corner");
    expect(html).toContain("Lower front-left corner");
    expect(html).toContain("Lower front-right corner");
    expect(html).toContain("Lower back-right corner");
    expect(html).toContain("Lower back-left corner");
    expect(html).toContain("25–30 cm left");
    expect(html).toContain("20–25 cm toward the speakers");
    expect(html).toContain("10–15 cm up");
    expect(html).toContain("10–15 cm down");
    // The written list still enumerates every position id.
    for (const position of ["P1", "P2", "P3", "P4", "P5", "P6", "P7", "P8"]) {
      expect(html).toContain(`>${position}<`);
    }
    // The diagram shows the four corner columns and the two side-view levels.
    for (const column of ["P1·P5", "P2·P6", "P3·P7", "P4·P8"]) {
      expect(html).toContain(`>${column}<`);
    }
    expect(html).toContain(">P1–P4<");
    expect(html).toContain(">P5–P8<");
  });
});
