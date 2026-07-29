import { describe, expect, it } from "vitest";
import { messages } from ".";

describe("localized wizard copy", () => {
  it("keeps a complete ten-step flow in every locale", () => {
    for (const locale of Object.values(messages)) {
      expect(locale.wizard.steps).toHaveLength(10);
      for (const step of locale.wizard.steps) {
        expect(step.short.trim()).not.toBe("");
        expect(step.title.trim()).not.toBe("");
        expect(step.description.trim()).not.toBe("");
      }
    }
  });

  it("labels the live beta, preview shell, and synthetic chart without ambiguity", () => {
    expect(messages.ko.status.liveReady).toContain("사용 가능");
    expect(messages.ko.status.notConnected).toContain("미리보기");
    expect(messages.ko.status.synthetic).toContain("합성");
    expect(messages.en.status.liveReady.toLowerCase()).toContain("available");
    expect(messages.en.status.notConnected.toLowerCase()).toContain("preview");
    expect(messages.en.status.synthetic.toLowerCase()).toContain("synthetic");
  });

  it("keeps the 2.0/2.1 choice and manual-sub boundary localized", () => {
    for (const locale of Object.values(messages)) {
      expect(locale.liveMeasurement.systemLabels.stereo_2_0.trim()).not.toBe(
        "",
      );
      expect(
        locale.liveMeasurement.systemLabels.single_sub_2_1.trim(),
      ).not.toBe("");
      expect(locale.liveMeasurement.subwooferScopeBody.trim()).not.toBe("");
      expect(locale.liveMeasurement.subwooferChecklist).toHaveLength(3);
      expect(
        locale.liveMeasurement.subwooferSearch.safetyChecklist,
      ).toHaveLength(3);
      expect(
        locale.liveMeasurement.subwooferSearch.predictionBody.trim(),
      ).not.toBe("");
    }
    expect(messages.ko.liveMeasurement.subwooferScopeBody).toContain(
      "직접 바꾸지",
    );
    expect(
      messages.en.liveMeasurement.subwooferScopeBody.toLowerCase(),
    ).toContain("does not change");
    expect(messages.ko.liveMeasurement.subwooferSearch.predictionBody).toContain(
      "예측",
    );
    expect(
      messages.en.liveMeasurement.subwooferSearch.predictionBody.toLowerCase(),
    ).toContain("prediction");
  });

  it("keeps the redesign-and-reverify loop honest in both locales", () => {
    for (const locale of [messages.en, messages.ko]) {
      const copy = locale.liveMeasurement;
      // Retrying is a real loop: it must tell the user a regenerated filter
      // is verified again by remeasurement, never carried over from a failed
      // attempt.
      expect(copy.verificationRetrySteps.length).toBeGreaterThanOrEqual(3);
      const steps = copy.verificationRetrySteps.join(" ");
      expect(steps).toMatch(/재측정|measure.*again|remeasure/i);
      expect(steps).toMatch(/Roon/);
      // The small-improvement advisory must state the deterministic fact that
      // the same measurements produce the same filter, and must not call a
      // near-target room a failure.
      expect(copy.smallImprovementAdvisory).toMatch(/같은 필터|same filter/i);
      expect(copy.smallImprovementAdvisory).toMatch(/실패가 아니|not a failure/i);
      // The gate tiles must say what they show: the smoothed, octave-weighted
      // judgment values, so nobody mistakes them for the unsmoothed RMSEs.
      expect(copy.leftImprovementGate).toMatch(/스무딩|smooth/i);
      expect(copy.rightImprovementGate).toMatch(/옥타브|octave/i);
    }
  });
});
