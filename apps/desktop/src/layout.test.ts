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

/**
 * Text size is expressed in rem so that one root declaration scales the whole
 * UI. Windows fits fewer physical pixels into a CSS pixel than a Retina Mac at
 * its default scaling, so a stylesheet that merely looks small on macOS becomes
 * unusable there - which is what these two rules exist to prevent regressing.
 */
describe("desktop text size", () => {
  const ROOT_PX = 16;
  /** Below this, Korean glyphs lose stroke detail on a non-HiDPI Windows panel. */
  const MINIMUM_EFFECTIVE_PX = 11.5;

  const scale = Number(
    /--ui-font-scale:\s*([0-9.]+)\s*;/.exec(styles)?.[1] ?? "0",
  );

  it("drives the root font size from a single scale knob", () => {
    expect(scale).toBeGreaterThanOrEqual(1);
    expect(styles).toMatch(
      /html\s*\{[^}]*font-size:\s*calc\(\s*16px\s*\*\s*var\(--ui-font-scale\)\s*\)/,
    );
  });

  it("declares every font size in rem, apart from SVG user-space text", () => {
    // These render inside a viewBox, so their px values are user-space units
    // that scale with the diagram rather than absolute screen sizes.
    const svgTextRules =
      /axis-label|axis-title|position-diagram__(?:point text|view-label|small-label|area-label)/;

    const pixelSized = [...styles.matchAll(/([^{}]*)\{([^{}]*)\}/g)]
      .filter(([, selector, body]) => /font-size:\s*[0-9.]+px/.test(body))
      .map(([, selector]) => selector.trim().replace(/\s+/g, " "))
      .filter((selector) => !svgTextRules.test(selector));

    expect(pixelSized).toEqual([]);
  });

  it("keeps the smallest text above the legibility floor", () => {
    const sizes = [...styles.matchAll(/font-size:\s*([0-9.]+)rem/g)].map(
      (match) => Number(match[1]) * ROOT_PX * scale,
    );

    expect(sizes.length).toBeGreaterThan(200);
    expect(Math.min(...sizes)).toBeGreaterThanOrEqual(MINIMUM_EFFECTIVE_PX);
  });
});
