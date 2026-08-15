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

  it("keeps the wide-band crossover synthesis honest in both locales", () => {
    for (const locale of [messages.en, messages.ko]) {
      const search = locale.liveMeasurement.subwooferSearch;
      expect(search.wideSafetyChecklist).toHaveLength(3);
      // The wide mode is a model-based simulation, and the copy must say so:
      // the recommendation can be wrong when the declared slopes differ from
      // the hardware, and the real combined measurement stays the judge.
      expect(search.modeWideBandHint).toMatch(/시뮬레이션|simulat/i);
      expect(search.wideModelNote).toMatch(/모델|model/i);
      expect(search.wideModelNote).toMatch(/틀릴 수|can be wrong/i);
      expect(search.wideModelNote).toMatch(/최종 판정|remains the judge/i);
      // The measured mode must remain described as model-free.
      expect(search.modeMeasuredStatesHint).toMatch(
        /가정하지 않|no filter model is assumed/i,
      );
    }
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

  it("calls the calibration file optional and warns about skipping it", () => {
    for (const locale of Object.values(messages)) {
      const copy = locale.liveMeasurement;
      // The TXT is optional for microphones that self-correct, and the body
      // must say why supplying one anyway is wrong.
      expect(copy.calibrationTitle).toMatch(/선택|optional/i);
      expect(copy.calibrationBody).toMatch(/이중 보정|apply it twice/i);
      // Skipping it is a real loss of accuracy: the warning must say the
      // microphone's own error stays in the measurement and reaches the
      // filter, and that the package records the session as uncalibrated.
      expect(copy.calibrationOptionalWarning).toMatch(/경고|warning/i);
      expect(copy.calibrationOptionalWarning).toMatch(/필터|filter/i);
      expect(copy.calibrationOptionalWarning).toMatch(/uncalibrated/i);
    }
  });

  it("keeps the SECS advanced option honest in both locales", () => {
    for (const locale of Object.values(messages)) {
      const copy = locale.liveMeasurement;
      // The toggle note must say the trial is predicted-only, that it designs
      // from P0 alone, and that export requires the P0 closed-loop
      // verification to pass first.
      expect(copy.secsToggleNote).toMatch(/예측 전용|predicted-only/i);
      expect(copy.secsToggleNote).toContain("P0");
      expect(copy.secsToggleNote).toMatch(/검증|verification/i);
      expect(copy.secsTrialReady).toMatch(/예측 전용|predicted-only/i);
      expect(copy.secsToggleLabel).toMatch(/실험적|experimental/i);
      // The original author asked for a credit with a link; both locales must
      // name them and must not present this app's additions as their design.
      expect(copy.secsCredit).toContain("한플");
      expect(copy.secsCredit).toMatch(/MIT/);
      expect(copy.secsCreditLink.trim()).not.toBe("");
      // The verification-skip strings must carry the predicted-only state.
      expect(copy.secsSkipVerificationNote).toMatch(/예측 전용|predicted-only/i);
      expect(copy.exportUnverifiedWarning).toMatch(/예측 전용|predicted-only/i);
      expect(copy.exportReadyUnverified).toMatch(/예측 전용|PREDICTED-ONLY/i);
      // Every SECS parameter control must carry a label in both locales.
      for (const key of [
        "secsSettingsTitle",
        "secsSettingsSummary",
        "secsMaxBoost",
        "secsTilt",
        "secsBassBoost",
        "secsBassFrequency",
        "secsResolution",
        "secsResolutionLow",
        "secsResolutionNormal",
        "secsResolutionHigh",
        "secsCurtain",
        "secsLatency",
        "secsLatencyNormal",
        "secsLatencyLow",
        "secsLatencyZero",
        "secsDelayMode",
        "secsDelayAuto",
        "secsDelayFixed",
        "secsMultiPosition",
        "secsFollowTarget",
        "secsTargetCurve",
        "secsTargetFlat",
        "secsCredit",
        "secsCreditLink",
        "secsSharedSubBand",
        "secsSharedSubBandValue",
        "secsSkipVerification",
        "secsSkipVerificationNote",
        "secsSkipVerificationVerifyHint",
        "exportReadyUnverified",
        "exportUnverifiedWarning",
      ] as const) {
        expect(copy[key].trim()).not.toBe("");
      }
    }
  });

  it("keeps the default target attributed to Dirac and its HF rolloff honest", () => {
    for (const locale of Object.values(messages)) {
      const copy = locale.liveMeasurement;
      // The only built-in target is the Dirac-published Harman +6 dB curve;
      // the label and note must name it and its source.
      expect(copy.targetLabels.harman_6db).toContain("Harman-6dB");
      expect(copy.targetLabels.harman_6db).toContain("Dirac");
      expect(copy.defaultTargetNote).toContain("Dirac");
      expect(copy.defaultTargetNote).toContain("+6 dB");
      // The adaptive HF rolloff is default-target-only, and the copy says so.
      expect(copy.defaultTargetNote).toMatch(/기본 타겟을 선택했을 때만|only when the default target/i);
      expect(copy.defaultTargetSourceLink.trim()).not.toBe("");
      // Provenance strings: templates carry their placeholders, and every
      // fallback guard code has a human-readable explanation.
      expect(copy.adaptiveHfTitle.trim()).not.toBe("");
      expect(copy.adaptiveHfPreferred).toContain("{slope}");
      expect(copy.adaptiveHfRolloff).toContain("{slope}");
      expect(copy.adaptiveHfRolloff).toContain("{break}");
      expect(copy.adaptiveHfFallback).toContain("{reason}");
      for (const reason of Object.values(copy.adaptiveHfFallbackReasons)) {
        expect(reason.trim()).not.toBe("");
      }
      // The provenance is measurement-derived prediction context; it must
      // never use completion/verification language.
      expect(copy.adaptiveHfTitle).not.toMatch(/보정 완료|verified|검증됨/i);
    }
  });

  it("tells both locales the sweep is already loaded and can be replaced", () => {
    for (const locale of Object.values(messages)) {
      const copy = locale.liveMeasurement;
      // The built-in notice names the file it loaded, so the user can tell it
      // apart from a sweep they picked themselves.
      expect(copy.sweepBuiltInInUse).toContain("{file}");
      for (const key of [
        "replaceSweep",
        "useBuiltInSweep",
        "chooseSweep",
      ] as const) {
        expect(copy[key].trim()).not.toBe("");
      }
      // A failed built-in load must point at the manual path rather than
      // leaving the stage with no way forward.
      expect(copy.errors.builtInSweepFailed.trim()).not.toBe("");
    }
    expect(messages.ko.liveMeasurement.sweepBody).toContain("기본 스윕");
    expect(messages.en.liveMeasurement.sweepBody.toLowerCase()).toContain(
      "built-in sweep",
    );
  });
});
