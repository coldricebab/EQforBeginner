import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { messages } from "../i18n";
import { WirelessSweepPanel } from "./WirelessSweepPanel";

describe("WirelessSweepPanel", () => {
  it("renders an explicit manual-Roon flow and timing boundary", () => {
    const html = renderToStaticMarkup(
      <WirelessSweepPanel
        copy={messages.en.wirelessSweep}
        selectedInput={JSON.stringify(["coreaudio:umic", 1, "f32", 64, 512])}
        selectedInputName="UMIK-1"
      />,
    );

    expect(html).toContain("Recognize a wireless Roon sweep");
    expect(html).toContain("UMIK-1");
    expect(html).toContain("Play manually in Roon");
    expect(html).toContain("does not control the Roon Zone");
    expect(html).toContain("not evidence for L/R delay correction");
    expect(html).not.toContain("The reference sweep matched");
  });
});
