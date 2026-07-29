import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import tauriConfig from "../src-tauri/tauri.conf.json";

const styles = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

describe("desktop minimum-window layout", () => {
  it("reflows the six-stage wizard before the configured minimum width", () => {
    expect(styles).toMatch(
      /\.live-wizard-nav ol\s*\{[\s\S]*?grid-template-columns:\s*repeat\(6,/,
    );
    const mediaQueries = [
      ...styles.matchAll(/@media\s*\(\s*max-width\s*:\s*(\d+)px\s*\)/g),
    ];
    const responsiveWizardBreakpoint = mediaQueries.find((query, index) => {
      const start = query.index ?? 0;
      const end = mediaQueries[index + 1]?.index ?? styles.length;
      return /\.live-wizard-nav ol\s*\{\s*grid-template-columns:\s*repeat\(3,/.test(
        styles.slice(start, end),
      );
    });

    expect(responsiveWizardBreakpoint).toBeDefined();
    const breakpoint = Number(responsiveWizardBreakpoint?.[1]);
    const minimumWindowWidth = tauriConfig.app.windows[0].minWidth;
    expect(breakpoint).toBeGreaterThanOrEqual(minimumWindowWidth);
  });
});
