import { describe, expect, it } from "vitest";
import {
  createDeveloperRunId,
  initialDeveloperStages,
  isFinalExportUnlocked,
  type DeveloperStageView,
} from "./developerBeta";

describe("developer beta safety state", () => {
  it("creates a filesystem-safe deterministic run id", () => {
    expect(createDeveloperRunId(new Date("2026-07-21T12:34:56.789Z"))).toBe(
      "run-2026-07-21T12-34-56-789Z",
    );
  });

  it("starts all fixture stages idle", () => {
    expect(initialDeveloperStages()).toEqual([
      { id: "phase3", state: "idle" },
      { id: "phase4", state: "idle" },
      { id: "phase6", state: "idle" },
    ]);
  });

  it("never unlocks export from a numerical preview", () => {
    const stages: DeveloperStageView[] = initialDeveloperStages();
    stages[2] = {
      id: "phase6",
      state: "blocked",
      result: {
        id: "phase6",
        outputDirectory: "/tmp/phase6",
        verificationState: "predicted-only-measured",
        artifacts: ["/tmp/phase6/project.json"],
        logs: [],
        numericalPassed: true,
        exportEligible: false,
        exportBlockReason: "No post-FIR playback-path measurement",
      },
    };
    expect(isFinalExportUnlocked(stages)).toBe(false);
  });

  it("requires both a passed Phase 6 state and explicit eligibility", () => {
    const stages: DeveloperStageView[] = initialDeveloperStages();
    stages[2] = {
      id: "phase6",
      state: "passed",
      result: {
        id: "phase6",
        outputDirectory: "/tmp/phase6",
        verificationState: "hardware-verified",
        artifacts: ["/tmp/phase6/package.zip"],
        logs: [],
        numericalPassed: true,
        exportEligible: true,
        exportBlockReason: "",
      },
    };
    expect(isFinalExportUnlocked(stages)).toBe(true);
  });
});
