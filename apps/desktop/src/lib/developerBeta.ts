import { invoke, isTauri } from "@tauri-apps/api/core";

export const developerStageIds = ["phase3", "phase4", "phase6"] as const;

export type DeveloperStageId = (typeof developerStageIds)[number];
export type DeveloperStageState = "idle" | "running" | "passed" | "blocked" | "failed";

export type DeveloperStagePreflight = {
  id: DeveloperStageId;
  available: boolean;
  detail: string;
  requiredPaths: string[];
};

export type DeveloperBetaPreflight = {
  workspaceRoot: string;
  measurementRuntimeConnected: boolean;
  stages: DeveloperStagePreflight[];
  exportEligible: boolean;
  exportBlockReason: string;
};

export type DeveloperStageResult = {
  id: DeveloperStageId;
  outputDirectory: string;
  verificationState: string;
  artifacts: string[];
  logs: string[];
  numericalPassed: boolean | null;
  exportEligible: boolean;
  exportBlockReason: string;
};

export type DeveloperStageView = {
  id: DeveloperStageId;
  state: DeveloperStageState;
  result?: DeveloperStageResult;
  error?: string;
};

export function initialDeveloperStages(): DeveloperStageView[] {
  return developerStageIds.map((id) => ({ id, state: "idle" }));
}

export function createDeveloperRunId(now = new Date()): string {
  return `run-${now.toISOString().replace(/[:.]/g, "-")}`;
}

export function isFinalExportUnlocked(
  stages: readonly DeveloperStageView[],
): boolean {
  const phase6 = stages.find((stage) => stage.id === "phase6");
  return phase6?.state === "passed" && phase6.result?.exportEligible === true;
}

export async function loadDeveloperBetaPreflight(
  workspaceRoot: string,
): Promise<DeveloperBetaPreflight> {
  if (!isTauri()) {
    throw new Error("native-shell-required");
  }
  return invoke<DeveloperBetaPreflight>("developer_beta_preflight", {
    workspaceRoot: workspaceRoot.trim() || null,
  });
}

export async function runDeveloperBetaStage(
  id: DeveloperStageId,
  workspaceRoot: string,
  runId: string,
): Promise<DeveloperStageResult> {
  if (!isTauri()) {
    throw new Error("native-shell-required");
  }
  return invoke<DeveloperStageResult>("run_developer_beta_stage", {
    stage: id,
    workspaceRoot: workspaceRoot.trim() || null,
    runId,
  });
}
