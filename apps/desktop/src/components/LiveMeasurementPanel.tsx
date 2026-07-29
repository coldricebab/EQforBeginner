import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import type { Messages } from "../i18n/types";
import type {
  AudioDeviceChoice,
  AudioScanError,
  AudioScanState,
} from "../lib/audio";
import {
  LIVE_BASELINE_POSITIONS,
  LIVE_CHANNELS,
  MAX_LIVE_CALIBRATION_BYTES,
  MAX_LIVE_TARGET_BYTES,
  MAX_LIVE_SWEEP_BYTES,
  acceptedSubTripletCount,
  acceptedPairCount,
  captureKey,
  classifyLiveCaptureIssues,
  formatMetric,
  hasAcceptedP0,
  liveWizardStages,
  parseCrossoverCandidates,
  type CalibrationImportSummary,
  type LiveCaptureProgress,
  type LiveCaptureKind,
  type LiveCaptureSummary,
  type LiveChannel,
  type LiveDesignSummary,
  type LiveExportSummary,
  type LiveMeasurementCacheRestoreSummary,
  type LiveReferenceChannel,
  type LiveSessionSummary,
  type LiveSubwooferSetupRequest,
  type LiveSubwooferSetupSummary,
  type LiveSubwooferSearchRequest,
  type LiveSubwooferSearchSummary,
  type LiveSubwooferOptimizationSummary,
  type LiveSweepImportSummary,
  type LiveSystemMode,
  type LiveVerificationSummary,
  type LiveWizardStage,
  type LiveZipArtifactKind,
  type LiveZipDownloadSummary,
  type TargetImportSummary,
} from "../lib/liveMeasurement";
import {
  inputChannelIndexFromChoice,
  inputDeviceIdFromChoice,
} from "../lib/wirelessSweep";
import { MeasurementPositionGuide } from "./MeasurementPositionGuide";
import { FinalMeasurementResults } from "./FinalMeasurementResults";

export type LiveTargetKind = "bk" | "harman" | "custom";

type Props = {
  copy: Messages["liveMeasurement"];
  chartCopy: Messages["chart"];
  inputChoices: AudioDeviceChoice[];
  selectedInput: string;
  selectedInputName: string | null;
  audioScanState: AudioScanState;
  audioScanError: AudioScanError;
  onScanInputDevices: () => void;
  onSelectedInputChange: (value: string) => void;
  target: LiveTargetKind;
  onTargetChange: (target: LiveTargetKind) => void;
};

type BusyOperation =
  | "session"
  | "calibration"
  | "target"
  | "left_sweep"
  | "right_sweep"
  | "subwoofer"
  | "sub_search"
  | "sub_optimize"
  | "cache"
  | "design"
  | "declaration"
  | "validate"
  | "export"
  | "trial_download"
  | "final_download"
  | `capture:${string}`
  | null;

/**
 * Starting values for the separated-path delay search.
 *
 * These bound the *amplifier's* capability, not the search. Since the
 * separated-path v2 change the search centres one crossover half-period on the
 * measured sub arrival and snaps candidates to step multiples, so widening the
 * range adds no candidates and only stops that window being clipped.
 *
 * The previous 0 / 5 / 0.05 defaults fought that on all three axes. A minimum
 * of 0 made the "put the delay on the sub instead" answer unreachable even
 * though the result card explains it; 5 ms is under one half-period below
 * 80 Hz and cannot cover a DSP subwoofer's processing latency plus placement
 * offset; and a 0.05 ms step is finer than an amplifier can actually be set to,
 * so it produced recommendations nobody could dial in.
 */
export const SUBWOOFER_DELAY_DEFAULTS = {
  minimumMs: "-10",
  maximumMs: "25",
  stepMs: "0.1",
} as const;

export function LiveMeasurementPanel({
  copy,
  chartCopy,
  inputChoices,
  selectedInput,
  selectedInputName,
  audioScanState,
  audioScanError,
  onScanInputDevices,
  onSelectedInputChange,
  target,
  onTargetChange,
}: Props) {
  const calibrationInputRef = useRef<HTMLInputElement>(null);
  const leftSweepInputRef = useRef<HTMLInputElement>(null);
  const rightSweepInputRef = useRef<HTMLInputElement>(null);
  const cancelRequestedRef = useRef(false);
  const stageHeadingRef = useRef<HTMLHeadingElement>(null);
  const [currentStage, setCurrentStage] = useState(0);
  const [systemMode, setSystemMode] = useState<LiveSystemMode | null>(null);
  const [session, setSession] = useState<LiveSessionSummary | null>(null);
  const [subwooferSetup, setSubwooferSetup] =
    useState<LiveSubwooferSetupSummary | null>(null);
  const [subwooferSearch, setSubwooferSearch] =
    useState<LiveSubwooferSearchSummary | null>(null);
  const [subwooferOptimization, setSubwooferOptimization] =
    useState<LiveSubwooferOptimizationSummary | null>(null);
  const [crossoverCandidates, setCrossoverCandidates] =
    useState("70, 80, 90");
  const [measuredMainDelayMs, setMeasuredMainDelayMs] = useState("0");
  const [measuredPolarityDegrees, setMeasuredPolarityDegrees] =
    useState<"0" | "180">("0");
  const [fixedSubLevelDb, setFixedSubLevelDb] = useState("0");
  const [delayMinimumMs, setDelayMinimumMs] = useState<string>(
    SUBWOOFER_DELAY_DEFAULTS.minimumMs,
  );
  const [delayMaximumMs, setDelayMaximumMs] = useState<string>(
    SUBWOOFER_DELAY_DEFAULTS.maximumMs,
  );
  const [delayStepMs, setDelayStepMs] = useState<string>(
    SUBWOOFER_DELAY_DEFAULTS.stepMs,
  );
  const [crossoverHz, setCrossoverHz] = useState("");
  const [mainDelayMs, setMainDelayMs] = useState("");
  const [polarityDegrees, setPolarityDegrees] = useState<"0" | "180">("0");
  const [subLevelDb, setSubLevelDb] = useState("");
  const [hardwareConfirmed, setHardwareConfirmed] = useState(false);
  const [calibration, setCalibration] =
    useState<CalibrationImportSummary | null>(null);
  const [customTarget, setCustomTarget] =
    useState<TargetImportSummary | null>(null);
  const [sweeps, setSweeps] = useState<
    Partial<Record<LiveChannel, LiveSweepImportSummary>>
  >({});
  const [captures, setCaptures] = useState<
    Record<string, LiveCaptureSummary>
  >({});
  const [cacheRestore, setCacheRestore] =
    useState<LiveMeasurementCacheRestoreSummary | null>(null);
  const [design, setDesign] = useState<LiveDesignSummary | null>(null);
  const [trialDeclared, setTrialDeclared] = useState(false);
  const [verification, setVerification] =
    useState<LiveVerificationSummary | null>(null);
  const [exported, setExported] = useState<LiveExportSummary | null>(null);
  const [trialDownload, setTrialDownload] =
    useState<LiveZipDownloadSummary | null>(null);
  const [finalDownload, setFinalDownload] =
    useState<LiveZipDownloadSummary | null>(null);
  const [busy, setBusy] = useState<BusyOperation>(null);
  const [captureProgress, setCaptureProgress] =
    useState<LiveCaptureProgress | null>(null);
  const [error, setError] = useState("");

  const inputDeviceId = inputDeviceIdFromChoice(selectedInput);
  const inputChannelIndex = inputChannelIndexFromChoice(selectedInput);
  const baselinePairs = acceptedPairCount(captures);
  const baselineP0Ready = hasAcceptedP0(captures, "baseline");
  const verificationP0Ready = hasAcceptedP0(captures, "verification");
  const filesReady = calibration !== null && Boolean(sweeps.left && sweeps.right);
  const parsedCrossovers = parseCrossoverCandidates(crossoverCandidates);
  const subwooferSearchFieldsValid =
    parsedCrossovers !== null &&
    [
      measuredMainDelayMs,
      fixedSubLevelDb,
      delayMinimumMs,
      delayMaximumMs,
      delayStepMs,
    ].every((value) => value.trim() !== "" && Number.isFinite(Number(value))) &&
    Number(measuredMainDelayMs) >= -20 &&
    Number(measuredMainDelayMs) <= 50 &&
    Number(fixedSubLevelDb) >= -30 &&
    Number(fixedSubLevelDb) <= 12 &&
    Number(delayMinimumMs) >= -20 &&
    Number(delayMaximumMs) <= 50 &&
    Number(delayMaximumMs) >= Number(delayMinimumMs) &&
    Number(delayStepMs) >= 0.01 &&
    Number(delayStepMs) <= 5;
  const completedSubTriplets = acceptedSubTripletCount(
    captures,
    subwooferSearch,
  );
  const subwooferFieldsValid =
    crossoverHz.trim() !== "" &&
    mainDelayMs.trim() !== "" &&
    subLevelDb.trim() !== "" &&
    Number.isFinite(Number(crossoverHz)) &&
    Number.isFinite(Number(mainDelayMs)) &&
    Number.isFinite(Number(subLevelDb)) &&
    Number(crossoverHz) >= 30 &&
    Number(crossoverHz) <= 200 &&
    Number(mainDelayMs) >= -20 &&
    Number(mainDelayMs) <= 50 &&
    Number(subLevelDb) >= -30 &&
    Number(subLevelDb) <= 12;
  const stageIds = liveWizardStages(systemMode);
  const stageTitleById: Record<LiveWizardStage, string> = {
    session: copy.sessionTitle,
    inputs: copy.inputsTitle,
    subwoofer: copy.subwooferTitle,
    baseline: copy.baselineTitle,
    design: copy.designTitle,
    verify: copy.verifyTitle,
    export: copy.exportTitle,
  };
  const stageTitles = stageIds.map((stage) => stageTitleById[stage]);
  const currentStageId = stageIds[currentStage] ?? "session";
  const stageNumber = (stage: LiveWizardStage) =>
    stageIds.indexOf(stage) + 1;

  const canEnterStage = (stage: number) => {
    if (stage <= currentStage) return true;
    switch (stageIds[stage]) {
      case "inputs":
        return session !== null;
      case "subwoofer":
        return session !== null &&
          filesReady &&
          inputDeviceId !== null &&
          inputChannelIndex !== null;
      case "baseline":
        return session !== null &&
          filesReady &&
          inputDeviceId !== null &&
          inputChannelIndex !== null &&
          (systemMode !== "single_sub_2_1" || subwooferSetup !== null);
      case "design":
        return baselineP0Ready;
      case "verify":
        return design !== null;
      case "export":
        return verification?.passed === true;
      default:
        return false;
    }
  };

  const navigateToStage = (stage: number) => {
    const destination = Math.max(0, Math.min(stageTitles.length - 1, stage));
    if (!canEnterStage(destination)) return;
    setCurrentStage(destination);
    requestAnimationFrame(() => stageHeadingRef.current?.focus());
  };

  useEffect(() => {
    setDesign(null);
    setTrialDeclared(false);
    setVerification(null);
    setExported(null);
    setTrialDownload(null);
    setFinalDownload(null);
  }, [target]);

  const fail = (caught: unknown) => {
    setError(String(caught));
    setBusy(null);
  };

  const invalidateSubwooferSearch = () => {
    setSubwooferSearch(null);
    setSubwooferOptimization(null);
    setSubwooferSetup(null);
    setHardwareConfirmed(false);
    setCaptures({});
    setCacheRestore(null);
    setDesign(null);
    setTrialDeclared(false);
    setVerification(null);
    setExported(null);
    setTrialDownload(null);
    setFinalDownload(null);
  };

  const startSession = async () => {
    if (systemMode === null) {
      setError(copy.systemTypeRequired);
      return;
    }
    if (!isTauri()) {
      setError(copy.errors.nativeOnly);
      return;
    }
    setBusy("session");
    setError("");
    try {
      const created = await invoke<LiveSessionSummary>(
        "start_live_measurement_session",
        { systemMode },
      );
      setSession(created);
      setSystemMode(created.systemMode);
      setSubwooferSetup(null);
      setSubwooferSearch(null);
      setSubwooferOptimization(null);
      setCrossoverCandidates("70, 80, 90");
      setMeasuredMainDelayMs("0");
      setMeasuredPolarityDegrees("0");
      setFixedSubLevelDb("0");
      setDelayMinimumMs(SUBWOOFER_DELAY_DEFAULTS.minimumMs);
      setDelayMaximumMs(SUBWOOFER_DELAY_DEFAULTS.maximumMs);
      setDelayStepMs(SUBWOOFER_DELAY_DEFAULTS.stepMs);
      setCrossoverHz("");
      setMainDelayMs("");
      setPolarityDegrees("0");
      setSubLevelDb("");
      setHardwareConfirmed(false);
      setCalibration(null);
      setCustomTarget(null);
      onTargetChange("bk");
      setSweeps({});
      setCaptures({});
      setCacheRestore(null);
      setDesign(null);
      setTrialDeclared(false);
      setVerification(null);
      setExported(null);
      setTrialDownload(null);
      setFinalDownload(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const recordSubwooferSetup = async () => {
    if (!session || systemMode !== "single_sub_2_1" || busy !== null) return;
    const request: LiveSubwooferSetupRequest = {
      crossoverHz: Number(crossoverHz),
      mainDelayMs: Number(mainDelayMs),
      polarityDegrees: Number(polarityDegrees) as 0 | 180,
      subLevelDb: Number(subLevelDb),
      confirmedOnHardware: hardwareConfirmed,
    };
    setBusy("subwoofer");
    setError("");
    try {
      const result = await invoke<LiveSubwooferSetupSummary>(
        "record_live_subwoofer_setup",
        { setup: request },
      );
      setSubwooferSetup(result);
      setCacheRestore(null);
      setCaptures((current) =>
        Object.fromEntries(
          Object.entries(current).filter(
            ([key]) =>
              key.startsWith("sub_main_only:") || key.startsWith("sub_only:"),
          ),
        ),
      );
      setDesign(null);
      setTrialDeclared(false);
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const configureSubwooferSearch = async () => {
    if (
      !session ||
      systemMode !== "single_sub_2_1" ||
      !subwooferSearchFieldsValid ||
      !parsedCrossovers ||
      busy !== null
    ) return;
    const request: LiveSubwooferSearchRequest = {
      crossoverHz: parsedCrossovers,
      measuredMainDelayMs: Number(measuredMainDelayMs),
      measuredPolarityDegrees: Number(measuredPolarityDegrees) as 0 | 180,
      fixedSubLevelDb: Number(fixedSubLevelDb),
      delayMinimumMs: Number(delayMinimumMs),
      delayMaximumMs: Number(delayMaximumMs),
      delayStepMs: Number(delayStepMs),
    };
    setBusy("sub_search");
    setError("");
    try {
      const result = await invoke<LiveSubwooferSearchSummary>(
        "configure_live_subwoofer_search",
        { search: request },
      );
      setSubwooferSearch(result);
      setSubwooferOptimization(null);
      setSubwooferSetup(null);
      setHardwareConfirmed(false);
      setCaptures({});
      setCacheRestore(null);
      setDesign(null);
      setTrialDeclared(false);
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const optimizeSubwooferPaths = async () => {
    if (
      !subwooferSearch ||
      completedSubTriplets !== subwooferSearch.candidates.length ||
      busy !== null
    ) return;
    setBusy("sub_optimize");
    setError("");
    try {
      const result = await invoke<LiveSubwooferOptimizationSummary>(
        "optimize_live_subwoofer_paths",
      );
      setSubwooferOptimization(result);
      setCrossoverHz(result.best.crossoverHz.toString());
      setMainDelayMs(result.best.mainDelayMs.toString());
      setPolarityDegrees(result.best.polarityDegrees.toString() as "0" | "180");
      setSubLevelDb(result.fixedSubLevelDb.toString());
      setHardwareConfirmed(false);
      setSubwooferSetup(null);
      setDesign(null);
      setTrialDeclared(false);
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const importCalibration = async (file: File | undefined) => {
    if (!file) return;
    if (!session) {
      setError(copy.errors.startFirst);
      return;
    }
    if (file.size > MAX_LIVE_CALIBRATION_BYTES) {
      setError(copy.errors.calibrationTooLarge);
      return;
    }
    setBusy("calibration");
    setError("");
    try {
      const summary = await invoke<CalibrationImportSummary>(
        "import_live_microphone_calibration",
        { fileName: file.name, contents: await file.text() },
      );
      setCalibration(summary);
      setCaptures({});
      setCacheRestore(null);
      setSubwooferOptimization(null);
      setSubwooferSetup(null);
      setDesign(null);
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const importTarget = async (file: File | undefined) => {
    if (!file) return;
    if (!session) {
      setError(copy.errors.startFirst);
      return;
    }
    if (!file.name.toLowerCase().endsWith(".txt")) {
      setError(copy.errors.targetTxtOnly);
      return;
    }
    if (file.size > MAX_LIVE_TARGET_BYTES) {
      setError(copy.errors.targetTooLarge);
      return;
    }
    setBusy("target");
    setError("");
    try {
      const summary = await invoke<TargetImportSummary>(
        "import_live_target_curve",
        { fileName: file.name, contents: await file.text() },
      );
      setCustomTarget(summary);
      onTargetChange("custom");
      setDesign(null);
      setTrialDeclared(false);
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const importSweep = async (
    channel: LiveChannel,
    file: File | undefined,
  ) => {
    if (!file) return;
    if (!session) {
      setError(copy.errors.startFirst);
      return;
    }
    if (!file.name.toLowerCase().endsWith(".wav")) {
      setError(copy.errors.wavOnly);
      return;
    }
    if (file.size > MAX_LIVE_SWEEP_BYTES) {
      setError(copy.errors.sweepTooLarge);
      return;
    }
    const operation: BusyOperation = `${channel}_sweep`;
    setBusy(operation);
    setError("");
    try {
      const summary = await invoke<LiveSweepImportSummary>(
        channel === "left"
          ? "import_live_left_sweep"
          : "import_live_right_sweep",
        await file.arrayBuffer(),
      );
      setSweeps((current) => ({ ...current, [channel]: summary }));
      setCaptures({});
      setCacheRestore(null);
      setSubwooferSearch(null);
      setSubwooferOptimization(null);
      setSubwooferSetup(null);
      setDesign(null);
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const capture = async (
    kind: LiveCaptureKind,
    positionId: string,
    channel: LiveChannel,
  ) => {
    if (
      !session ||
      !filesReady ||
      !inputDeviceId ||
      inputChannelIndex === null ||
      busy !== null
    ) return;
    const key = captureKey(kind, positionId, channel);
    cancelRequestedRef.current = false;
    setBusy(`capture:${key}`);
    setCaptureProgress(null);
    setError("");
    try {
      const onProgress = new Channel<LiveCaptureProgress>();
      onProgress.onmessage = (progress) => {
        if (!cancelRequestedRef.current) setCaptureProgress(progress);
      };
      const result = await invoke<LiveCaptureSummary>(
        "capture_live_measurement",
        {
          kind,
          channel,
          positionId,
          inputDeviceId,
          inputChannelIndex,
          waitSeconds: 20,
          onProgress,
        },
      );
      if (cancelRequestedRef.current) {
        setCaptureProgress(null);
        setBusy(null);
        return;
      }
      if (kind === "sub_main_only" || kind === "sub_only") {
        setCaptures((current) => ({
          ...Object.fromEntries(
            Object.entries(current).filter(
              ([storedKey]) =>
                storedKey.startsWith("sub_main_only:") ||
                storedKey.startsWith("sub_only:"),
            ),
          ),
          [key]:
            current[key]?.accepted && !result.accepted ? current[key] : result,
        }));
        setSubwooferOptimization(null);
        setSubwooferSetup(null);
        setHardwareConfirmed(false);
        setDesign(null);
        setTrialDeclared(false);
        setVerification(null);
        setExported(null);
      } else {
        setCaptures((current) => ({
          ...current,
          [key]:
            current[key]?.accepted && !result.accepted ? current[key] : result,
        }));
      }
      if (kind === "baseline") {
        setDesign(null);
        setTrialDeclared(false);
        setVerification(null);
        setExported(null);
      } else if (kind === "verification") {
        setVerification(null);
        setExported(null);
      }
      setCaptureProgress(null);
      setBusy(null);
    } catch (caught) {
      if (cancelRequestedRef.current) {
        setCaptureProgress(null);
        setBusy(null);
      } else {
        setCaptureProgress(null);
        fail(caught);
      }
    }
  };

  const restoreAcceptedMeasurements = async () => {
    if (
      !session ||
      !filesReady ||
      !inputDeviceId ||
      inputChannelIndex === null ||
      busy !== null
    ) return;
    setBusy("cache");
    setError("");
    try {
      const result = await invoke<LiveMeasurementCacheRestoreSummary>(
        "restore_live_accepted_measurements",
        { inputDeviceId, inputChannelIndex, scope: "general" },
      );
      setCaptures((current) => {
        const restored = { ...current };
        for (const capture of result.restoredCaptures) {
          restored[captureKey(capture.kind, capture.positionId, capture.channel)] =
            capture;
        }
        return restored;
      });
      setCacheRestore(result);
      setSubwooferOptimization(null);
      setDesign(null);
      setTrialDeclared(false);
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const cancelCapture = async () => {
    cancelRequestedRef.current = true;
    setCaptureProgress(null);
    try {
      await invoke<boolean>("cancel_live_measurement_capture");
    } catch (caught) {
      cancelRequestedRef.current = false;
      fail(caught);
    }
  };

  const designTrial = async () => {
    if (
      !baselineP0Ready ||
      busy !== null ||
      (target === "custom" && customTarget === null)
    ) return;
    setBusy("design");
    setError("");
    setTrialDownload(null);
    setFinalDownload(null);
    try {
      const result = await invoke<LiveDesignSummary>(
        "design_live_trial_filter",
        { target },
      );
      setDesign(result);
      setTrialDeclared(false);
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const setTrialActivation = async (activeInRoon: boolean) => {
    if (!design || busy !== null) return;
    setBusy("declaration");
    setError("");
    try {
      const declared = await invoke<boolean>("set_live_trial_activation", {
        activeInRoon,
      });
      setTrialDeclared(declared);
      setCaptures((current) =>
        Object.fromEntries(
          Object.entries(current).filter(
            ([key]) => !key.startsWith("verification:"),
          ),
        ),
      );
      setVerification(null);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const validate = async () => {
    if (!verificationP0Ready || busy !== null) return;
    setBusy("validate");
    setError("");
    try {
      const result = await invoke<LiveVerificationSummary>(
        "validate_live_closed_loop",
      );
      setVerification(result);
      setExported(null);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const exportFinal = async () => {
    if (!verification?.passed || busy !== null) return;
    setBusy("export");
    setError("");
    setFinalDownload(null);
    try {
      const result = await invoke<LiveExportSummary>(
        "export_live_roon_convolution",
      );
      setExported(result);
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const downloadZip = async (artifactKind: LiveZipArtifactKind) => {
    if (
      busy !== null ||
      (artifactKind === "trial" && design === null) ||
      (artifactKind === "final" && exported === null)
    ) return;
    setBusy(artifactKind === "trial" ? "trial_download" : "final_download");
    setError("");
    try {
      const result = await invoke<LiveZipDownloadSummary | null>(
        "download_live_roon_zip",
        { artifactKind },
      );
      if (result) {
        if (artifactKind === "trial") {
          setTrialDownload(result);
        } else {
          setFinalDownload(result);
        }
      }
      setBusy(null);
    } catch (caught) {
      fail(caught);
    }
  };

  const captureBusy = busy?.startsWith("capture:") === true;

  return (
    <section
      className="live-measurement"
      aria-labelledby="live-measurement-title"
    >
      <header className="live-measurement__header">
        <div>
          <p className="eyebrow">{copy.eyebrow}</p>
          <h2 id="live-measurement-title">{copy.title}</h2>
          <p>{copy.body}</p>
        </div>
        <span className="live-measurement__badge">{copy.badge}</span>
      </header>

      <nav
        className={`live-wizard-nav live-wizard-nav--${stageTitles.length}`}
        aria-label={copy.stageNavigation}
      >
        <ol>
          {stageTitles.map((title, index) => {
            const enabled = canEnterStage(index);
            const current = currentStage === index;
            return (
              <li key={title}>
                <button
                  type="button"
                  className={[
                    current ? "is-current" : "",
                    index < currentStage ? "is-visited" : "",
                  ].filter(Boolean).join(" ")}
                  aria-current={current ? "step" : undefined}
                  disabled={!enabled}
                  onClick={() => navigateToStage(index)}
                >
                  <span aria-hidden="true">
                    {index < currentStage ? "✓" : index + 1}
                  </span>
                  <strong>{title}</strong>
                </button>
              </li>
            );
          })}
        </ol>
      </nav>

      <p className="live-stage-progress" aria-live="polite">
        {copy.stageProgress
          .replace("{current}", (currentStage + 1).toString())
          .replace("{total}", stageTitles.length.toString())
          .replace("{title}", stageTitles[currentStage])}
      </p>

      <div className="live-measurement__notice">
        <span aria-hidden="true">!</span>
        <div>
          <strong>{copy.manualTitle}</strong>
          <p>{copy.manualBody}</p>
        </div>
      </div>

      {currentStageId === "session" && (
      <section className="live-stage" aria-labelledby="live-session-title">
        <div className="live-stage__heading">
          <span aria-hidden="true">{stageNumber("session")}</span>
          <div>
            <h3 id="live-session-title" ref={stageHeadingRef} tabIndex={-1}>{copy.sessionTitle}</h3>
            <p>{copy.sessionBody}</p>
          </div>
        </div>
        <fieldset className="live-system-options">
          <legend>{copy.systemTypeTitle}</legend>
          {(["stereo_2_0", "single_sub_2_1"] as const).map((mode) => (
            <label
              className={systemMode === mode ? "is-selected" : ""}
              key={mode}
            >
              <input
                type="radio"
                name="live-system-mode"
                value={mode}
                checked={systemMode === mode}
                disabled={session !== null || busy !== null}
                onChange={() => {
                  setSystemMode(mode);
                  setError("");
                  setCurrentStage(0);
                }}
              />
              <span>
                <strong>{copy.systemLabels[mode]}</strong>
                <small>{copy.systemBodies[mode]}</small>
              </span>
            </label>
          ))}
        </fieldset>
        {systemMode === null && (
          <p className="live-warning">{copy.systemTypeRequired}</p>
        )}
        <button
          className="button button--primary"
          type="button"
          disabled={systemMode === null || busy !== null}
          onClick={() => void startSession()}
        >
          {busy === "session" ? copy.startingSession : copy.startSession}
        </button>
        {session && (
          <dl className="live-summary">
            <div>
              <dt>{copy.selectedSystem}</dt>
              <dd>{copy.systemLabels[session.systemMode]}</dd>
            </div>
            <div><dt>{copy.sessionId}</dt><dd>{session.sessionId}</dd></div>
            <div><dt>{copy.outputDirectory}</dt><dd>{session.outputDirectory}</dd></div>
          </dl>
        )}
      </section>
      )}

      {currentStageId === "inputs" && (
      <section className="live-stage" aria-labelledby="live-inputs-title">
        <div className="live-stage__heading">
          <span aria-hidden="true">{stageNumber("inputs")}</span>
          <div>
            <h3 id="live-inputs-title" ref={stageHeadingRef} tabIndex={-1}>{copy.inputsTitle}</h3>
            <p>{copy.inputsBody}</p>
          </div>
        </div>
        <div className="live-inputs">
          <article className="live-inputs__microphone">
            <strong>{copy.microphoneTitle}</strong>
            <p>{copy.microphoneBody}</p>
            <button
              className="button button--secondary"
              type="button"
              disabled={audioScanState === "scanning" || busy !== null}
              onClick={onScanInputDevices}
            >
              {audioScanState === "scanning"
                ? copy.scanningMicrophones
                : copy.scanMicrophones}
            </button>
            <label className="live-input-select">
              <span>{copy.selectMicrophone}</span>
              <select
                value={selectedInput}
                disabled={inputChoices.length === 0 || busy !== null}
                onChange={(event) =>
                  onSelectedInputChange(event.currentTarget.value)
                }
              >
                {inputChoices.length === 0 && (
                  <option value="">{copy.noMicrophone}</option>
                )}
                {inputChoices.map((choice) => (
                  <option value={choice.value} key={choice.value}>
                    {choice.name}
                    {choice.isDefault ? ` · ${copy.defaultMicrophone}` : ""}
                    {` · ${copy.inputChannel.replace(
                      "{channel}",
                      (choice.inputChannelIndex + 1).toString(),
                    )}`}
                    {` · ${choice.configuration.channels}ch ${choice.configuration.sampleFormat}`}
                  </option>
                ))}
              </select>
            </label>
            <small className="live-input-only-notice">
              {copy.inputOnlyNotice}
            </small>
            {audioScanError && (
              <small className="live-input-scan-error" role="alert">
                {audioScanError.kind === "native_shell_required"
                  ? copy.nativeScanOnly
                  : copy.scanFailed.replace(
                      "{detail}",
                      audioScanError.detail,
                    )}
              </small>
            )}
          </article>
          <article>
            <strong>{copy.calibrationTitle}</strong>
            <p>{copy.calibrationBody}</p>
            <input
              ref={calibrationInputRef}
              className="visually-hidden"
              type="file"
              accept=".txt,.cal,text/plain"
              onChange={(event) =>
                void importCalibration(event.currentTarget.files?.[0])
              }
            />
            <button
              className="button button--secondary"
              type="button"
              disabled={!session || busy !== null}
              onClick={() => {
                if (calibrationInputRef.current) {
                  calibrationInputRef.current.value = "";
                  calibrationInputRef.current.click();
                }
              }}
            >
              {busy === "calibration"
                ? copy.importingCalibration
                : copy.chooseCalibration}
            </button>
            {calibration && (
              <p className="live-inputs__accepted">
                {copy.calibrationReady
                  .replace("{file}", calibration.fileName)
                  .replace("{points}", calibration.pointCount.toString())}
                {calibration.serialNumber
                  ? ` · ${copy.calibrationSerial} ${calibration.serialNumber}`
                  : ""}
              </p>
            )}
          </article>
          {LIVE_CHANNELS.map((channel) => {
            const imported = sweeps[channel];
            const ref =
              channel === "left" ? leftSweepInputRef : rightSweepInputRef;
            return (
              <article key={channel}>
                <strong>{copy.sweepTitles[channel]}</strong>
                <p>{copy.sweepBody}</p>
                <input
                  ref={ref}
                  className="visually-hidden"
                  type="file"
                  accept=".wav,audio/wav,audio/x-wav"
                  onChange={(event) =>
                    void importSweep(channel, event.currentTarget.files?.[0])
                  }
                />
                <button
                  className="button button--secondary"
                  type="button"
                  disabled={!session || busy !== null}
                  onClick={() => {
                    if (ref.current) {
                      ref.current.value = "";
                      ref.current.click();
                    }
                  }}
                >
                  {busy === `${channel}_sweep`
                    ? copy.importingSweep
                    : copy.chooseSweep}
                </button>
                {imported && (
                  <>
                    <p className="live-inputs__accepted">
                      {copy.sweepReady
                        .replace(
                          "{duration}",
                          imported.measurementDurationSeconds.toFixed(2),
                        )
                        .replace(
                          "{peak}",
                          imported.measurementPeakDbfs.toFixed(1),
                        )
                        .replace(
                          "{markers}",
                          imported.timingMarkerCount.toString(),
                        )}
                    </p>
                    <SweepChannelAnalysis copy={copy} summary={imported} />
                  </>
                )}
                {imported && imported.timingMarkerCount < 2 && (
                  <p className="live-warning">{copy.sweepMarkerWarning}</p>
                )}
              </article>
            );
          })}
        </div>
      </section>
      )}

      {currentStageId === "subwoofer" && (
      <section className="live-stage" aria-labelledby="live-subwoofer-title">
        <div className="live-stage__heading">
          <span aria-hidden="true">{stageNumber("subwoofer")}</span>
          <div>
            <h3 id="live-subwoofer-title" ref={stageHeadingRef} tabIndex={-1}>
              {copy.subwooferTitle}
            </h3>
            <p>{copy.subwooferBody}</p>
          </div>
        </div>
        <div className="live-subwoofer-scope">
          <strong>{copy.subwooferScopeTitle}</strong>
          <p>{copy.subwooferScopeBody}</p>
          <p>{copy.subwooferCandidateCountHint}</p>
          <p>{copy.subwooferDelayAxisNote}</p>
        </div>
        <ol className="live-roon-checklist">
          {copy.subwooferChecklist.map((item) => <li key={item}>{item}</li>)}
        </ol>
        <div className="live-sub-search-section">
          <div className="live-sub-search-section__heading">
            <strong>{copy.subwooferSearch.title}</strong>
            <p>{copy.subwooferSearch.body}</p>
          </div>
          <label className="live-sub-search-crossovers">
            <span>{copy.subwooferSearch.crossoverCandidates}</span>
            <input
              type="text"
              inputMode="decimal"
              value={crossoverCandidates}
              disabled={busy !== null}
              aria-invalid={parsedCrossovers === null}
              onChange={(event) => {
                setCrossoverCandidates(event.currentTarget.value);
                invalidateSubwooferSearch();
              }}
            />
            <small>{copy.subwooferSearch.crossoverCandidatesHint}</small>
          </label>
          <div className="live-sub-search-section__heading">
            <strong>{copy.subwooferSearch.measuredSettings}</strong>
            <p>{copy.subwooferSearch.measuredSettingsHint}</p>
          </div>
          <div className="live-subwoofer-fields">
            <label>
              <span>{copy.mainDelayMs}</span>
              <input
                type="number"
                min="-20"
                max="50"
                step="0.01"
                inputMode="decimal"
                value={measuredMainDelayMs}
                disabled={busy !== null}
                onChange={(event) => {
                  setMeasuredMainDelayMs(event.currentTarget.value);
                  invalidateSubwooferSearch();
                }}
              />
            </label>
            <label>
              <span>{copy.polarityDegrees}</span>
              <select
                value={measuredPolarityDegrees}
                disabled={busy !== null}
                onChange={(event) => {
                  setMeasuredPolarityDegrees(
                    event.currentTarget.value as "0" | "180",
                  );
                  invalidateSubwooferSearch();
                }}
              >
                <option value="0">0°</option>
                <option value="180">180°</option>
              </select>
            </label>
            <label>
              <span>{copy.subLevelDb}</span>
              <input
                type="number"
                min="-30"
                max="12"
                step="0.1"
                inputMode="decimal"
                value={fixedSubLevelDb}
                disabled={busy !== null}
                onChange={(event) => {
                  setFixedSubLevelDb(event.currentTarget.value);
                  invalidateSubwooferSearch();
                }}
              />
            </label>
            <label>
              <span>{copy.subwooferSearch.delayMinimum}</span>
              <input
                type="number"
                min="-20"
                max="50"
                step="0.01"
                inputMode="decimal"
                value={delayMinimumMs}
                disabled={busy !== null}
                onChange={(event) => {
                  setDelayMinimumMs(event.currentTarget.value);
                  invalidateSubwooferSearch();
                }}
              />
            </label>
            <label>
              <span>{copy.subwooferSearch.delayMaximum}</span>
              <input
                type="number"
                min="-20"
                max="50"
                step="0.01"
                inputMode="decimal"
                value={delayMaximumMs}
                disabled={busy !== null}
                onChange={(event) => {
                  setDelayMaximumMs(event.currentTarget.value);
                  invalidateSubwooferSearch();
                }}
              />
            </label>
            <label>
              <span>{copy.subwooferSearch.delayStep}</span>
              <input
                type="number"
                min="0.01"
                max="5"
                step="0.01"
                inputMode="decimal"
                value={delayStepMs}
                disabled={busy !== null}
                onChange={(event) => {
                  setDelayStepMs(event.currentTarget.value);
                  invalidateSubwooferSearch();
                }}
              />
            </label>
          </div>
          <small className="live-subwoofer-limits">
            {copy.subwooferSetupLimits}
          </small>
          <button
            className="button button--primary"
            type="button"
            disabled={
              !session ||
              !filesReady ||
              !subwooferSearchFieldsValid ||
              subwooferSearch !== null ||
              busy !== null
            }
            onClick={() => void configureSubwooferSearch()}
          >
            {busy === "sub_search"
              ? copy.subwooferSearch.savingPlan
              : copy.subwooferSearch.savePlan}
          </button>
        </div>

        {subwooferSearch && (
          <>
            <div className="live-result live-result--passed" role="status">
              <strong>
                {copy.subwooferSearch.planSaved.replace(
                  "{count}",
                  subwooferSearch.candidates.length.toString(),
                )}
              </strong>
              <dl className="live-metrics">
                <div>
                  <dt>{copy.subwooferSearch.timingReference}</dt>
                  <dd>
                    {
                      copy.sweepChannelLabels[
                        subwooferSearch.fixedTimingReferenceChannel
                      ]
                    }
                  </dd>
                </div>
                <div>
                  <dt>{copy.subwooferSearch.subSweep}</dt>
                  <dd>{copy.sweepTitles[subwooferSearch.subSweepChannel]}</dd>
                </div>
              </dl>
              <dl className="live-paths">
                <div>
                  <dt>{copy.subwooferSearch.planPath}</dt>
                  <dd>{subwooferSearch.planPath}</dd>
                </div>
              </dl>
            </div>
            <MeasurementCacheControls
              copy={copy}
              result={cacheRestore}
              busy={busy === "cache"}
              disabled={
                !filesReady ||
                !inputDeviceId ||
                inputChannelIndex === null ||
                busy !== null
              }
              onRestore={() => void restoreAcceptedMeasurements()}
            />
            <div className="live-sub-search-section">
              <div className="live-sub-search-section__heading">
                <strong>{copy.subwooferSearch.measurementsTitle}</strong>
                <p>{copy.subwooferSearch.measurementsBody}</p>
              </div>
              <ul className="live-sub-safety">
                {copy.subwooferSearch.safetyChecklist.map((item) => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
              <p className="live-warning">
                {copy.subwooferSearch.subOnlyRoute
                  .replace(
                    "{sweep}",
                    copy.channelLabels[subwooferSearch.subSweepChannel],
                  )
                  .replace(
                    "{reference}",
                    copy.sweepChannelLabels[
                      subwooferSearch.fixedTimingReferenceChannel
                    ],
                  )}
              </p>
              <div className="live-sub-candidate-list">
                {subwooferSearch.candidates.map((candidate) => (
                  <article className="live-sub-candidate" key={candidate.id}>
                    <header>
                      <strong>
                        {copy.subwooferSearch.candidateTitle.replace(
                          "{crossover}",
                          candidate.crossoverHz.toFixed(1),
                        )}
                      </strong>
                      <span>{candidate.id}</span>
                    </header>
                    <div className="live-sub-candidate__captures">
                      {[
                        {
                          kind: "sub_main_only" as const,
                          channel: "left" as const,
                          label: copy.subwooferSearch.mainOnlyLeft,
                        },
                        {
                          kind: "sub_main_only" as const,
                          channel: "right" as const,
                          label: copy.subwooferSearch.mainOnlyRight,
                        },
                        {
                          kind: "sub_only" as const,
                          channel: subwooferSearch.subSweepChannel,
                          label: copy.subwooferSearch.subOnly,
                        },
                      ].map(({ kind, channel, label }) => {
                        const key = captureKey(kind, candidate.id, channel);
                        return (
                          <CaptureCell
                            key={key}
                            copy={copy}
                            label={label}
                            summary={captures[key]}
                            busy={busy === `capture:${key}`}
                            disabled={
                              !filesReady ||
                              !inputDeviceId ||
                              inputChannelIndex === null ||
                              (busy !== null && busy !== `capture:${key}`)
                            }
                            onCapture={() =>
                              void capture(kind, candidate.id, channel)
                            }
                            onCancel={() => void cancelCapture()}
                          />
                        );
                      })}
                    </div>
                  </article>
                ))}
              </div>
              <p className="live-stage__footnote">
                {copy.subwooferSearch.tripletProgress
                  .replace("{complete}", completedSubTriplets.toString())
                  .replace(
                    "{total}",
                    subwooferSearch.candidates.length.toString(),
                  )}
              </p>
              <button
                className="button button--primary"
                type="button"
                disabled={
                  completedSubTriplets !== subwooferSearch.candidates.length ||
                  busy !== null
                }
                onClick={() => void optimizeSubwooferPaths()}
              >
                {busy === "sub_optimize"
                  ? copy.subwooferSearch.optimizing
                  : copy.subwooferSearch.optimize}
              </button>
            </div>
          </>
        )}

        {subwooferOptimization && (
          <div className="live-sub-prediction">
            <div className="live-sub-search-section__heading">
              <strong>{copy.subwooferSearch.predictionTitle}</strong>
              <p>{copy.subwooferSearch.predictionBody}</p>
            </div>
            <dl className="live-metrics">
              <div><dt>{copy.crossoverHz}</dt><dd>{subwooferOptimization.best.crossoverHz.toFixed(1)} Hz</dd></div>
              <div><dt>{copy.mainDelayMs}</dt><dd>{subwooferOptimization.best.mainDelayMs.toFixed(3)} ms</dd></div>
              <div><dt>{copy.polarityDegrees}</dt><dd>{subwooferOptimization.best.polarityDegrees}°</dd></div>
              <div><dt>{copy.subLevelDb}</dt><dd>{subwooferOptimization.fixedSubLevelDb.toFixed(1)} dB</dd></div>
              <div>
                <dt>{copy.subwooferSearch.scoringBand}</dt>
                <dd>{subwooferOptimization.scoringLowerHz.toFixed(1)}–{subwooferOptimization.scoringUpperHz.toFixed(1)} Hz</dd>
              </div>
              <div>
                <dt>{copy.subwooferSearch.candidateCount}</dt>
                <dd>{subwooferOptimization.synthesizedCandidateCount}</dd>
              </div>
            </dl>
            {subwooferOptimization.subLevelAdvisory && (
              <p>
                {copy.subwooferSearch.subLevelAdvisory
                  .replace("{gain}", subwooferOptimization.subLevelAdvisory.bestGainDb.toFixed(0))
                  .replace(
                    "{best}",
                    subwooferOptimization.subLevelAdvisory.deficitRmsAtBestDb.toFixed(2),
                  )
                  .replace(
                    "{zero}",
                    subwooferOptimization.subLevelAdvisory.deficitRmsAtZeroDb.toFixed(2),
                  )}
              </p>
            )}
            <p>{copy.subwooferSearch.lowerScoreBetter}</p>
            <div className="live-sub-ranking-scroll">
              <table className="live-sub-ranking">
                <thead>
                  <tr>
                    <th>{copy.subwooferSearch.rank}</th>
                    <th>{copy.crossoverHz}</th>
                    <th>{copy.mainDelayMs}</th>
                    <th>{copy.polarityDegrees}</th>
                    <th>{copy.subwooferSearch.score}</th>
                    <th>{copy.subwooferSearch.rmsDeficit}</th>
                    <th>{copy.subwooferSearch.p95Deficit}</th>
                    <th>{copy.subwooferSearch.worstDeficit}</th>
                  </tr>
                </thead>
                <tbody>
                  {subwooferOptimization.rankings.slice(0, 5).map((ranking) => (
                    <tr key={`${ranking.rank}-${ranking.crossoverHz}-${ranking.mainDelayMs}-${ranking.polarityDegrees}`}>
                      <td>{ranking.rank}</td>
                      <td>{ranking.crossoverHz.toFixed(1)} Hz</td>
                      <td>{ranking.mainDelayMs.toFixed(3)} ms</td>
                      <td>{ranking.polarityDegrees}°</td>
                      <td>{ranking.totalScore.toFixed(3)}</td>
                      <td>{ranking.deficitRmsDb.toFixed(2)} dB</td>
                      <td>{ranking.deficitP95Db.toFixed(2)} dB</td>
                      <td>{ranking.worstDeficitDb.toFixed(2)} dB</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <dl className="live-paths">
              <div>
                <dt>{copy.subwooferSearch.reportPath}</dt>
                <dd>{subwooferOptimization.reportPath}</dd>
              </div>
            </dl>
            <div className="live-sub-apply">
              <strong>{copy.subwooferSearch.applyTitle}</strong>
              <p>{copy.subwooferSearch.applyBody}</p>
              <label className="live-declaration live-declaration--hardware">
                <input
                  type="checkbox"
                  checked={hardwareConfirmed}
                  disabled={busy !== null}
                  onChange={(event) => {
                    setHardwareConfirmed(event.currentTarget.checked);
                    setSubwooferSetup(null);
                  }}
                />
                <span>{copy.hardwareConfirmation}</span>
              </label>
              <button
                className="button button--primary"
                type="button"
                disabled={
                  !session ||
                  !subwooferFieldsValid ||
                  !hardwareConfirmed ||
                  busy !== null
                }
                onClick={() => void recordSubwooferSetup()}
              >
                {busy === "subwoofer"
                  ? copy.savingSubwooferSetup
                  : copy.saveSubwooferSetup}
              </button>
            </div>
          </div>
        )}
        {subwooferSetup && (
          <div className="live-result live-result--passed" role="status">
            <strong>{copy.subwooferSetupSaved}</strong>
            <dl className="live-metrics">
              <div><dt>{copy.crossoverHz}</dt><dd>{subwooferSetup.crossoverHz.toFixed(1)} Hz</dd></div>
              <div><dt>{copy.mainDelayMs}</dt><dd>{subwooferSetup.mainDelayMs.toFixed(2)} ms</dd></div>
              <div><dt>{copy.polarityDegrees}</dt><dd>{subwooferSetup.polarityDegrees}°</dd></div>
              <div><dt>{copy.subLevelDb}</dt><dd>{subwooferSetup.subLevelDb.toFixed(1)} dB</dd></div>
            </dl>
            <dl className="live-paths">
              <div>
                <dt>{copy.subwooferSettingsPath}</dt>
                <dd>{subwooferSetup.settingsPath}</dd>
              </div>
            </dl>
          </div>
        )}
      </section>
      )}

      {currentStageId === "baseline" && (
      <section className="live-stage" aria-labelledby="live-baseline-title">
        <div className="live-stage__heading">
          <span aria-hidden="true">{stageNumber("baseline")}</span>
          <div>
            <h3 id="live-baseline-title" ref={stageHeadingRef} tabIndex={-1}>{copy.baselineTitle}</h3>
            <p>
              {systemMode === "single_sub_2_1"
                ? copy.baselineBody21
                : copy.baselineBody}
            </p>
          </div>
        </div>
        <div className="live-selected-mic">
          <div>
            <span>{copy.selectedMicrophone}</span>
            <strong>{selectedInputName ?? copy.noMicrophone}</strong>
          </div>
          <button
            className="button button--secondary"
            type="button"
            disabled={audioScanState === "scanning" || busy !== null}
            onClick={onScanInputDevices}
          >
            {audioScanState === "scanning"
              ? copy.scanningMicrophones
              : copy.scanMicrophones}
          </button>
        </div>
        <MeasurementCacheControls
          copy={copy}
          result={cacheRestore}
          busy={busy === "cache"}
          disabled={
            !filesReady ||
            !inputDeviceId ||
            inputChannelIndex === null ||
            busy !== null
          }
          onRestore={() => void restoreAcceptedMeasurements()}
        />
        <ol className="live-roon-checklist">
          {copy.rawRoonChecklist.map((item) => <li key={item}>{item}</li>)}
          {systemMode === "single_sub_2_1" &&
            copy.rawRoonChecklist21.map((item) => <li key={item}>{item}</li>)}
        </ol>
        <p className="live-auto-save-notice">
          <span aria-hidden="true">✓</span>
          {copy.automaticSaveNotice}
        </p>
        <MeasurementPositionGuide copy={copy} />
        <div className="live-capture-table" role="table" aria-label={copy.baselineTitle}>
          <div className="live-capture-table__head" role="row">
            <span role="columnheader">{copy.position}</span>
            <span role="columnheader">{copy.channelLabels.left}</span>
            <span role="columnheader">{copy.channelLabels.right}</span>
          </div>
          {LIVE_BASELINE_POSITIONS.map((positionId) => (
            <div className="live-capture-table__row" role="row" key={positionId}>
              <strong role="rowheader">
                <span>{copy.positionLabels[positionId]}</span>
                <small>{copy.positionDetails[positionId]}</small>
              </strong>
              {LIVE_CHANNELS.map((channel) => (
                <CaptureCell
                  key={channel}
                  copy={copy}
                  tableCell
                  summary={
                    captures[captureKey("baseline", positionId, channel)]
                  }
                  busy={
                    busy ===
                    `capture:${captureKey("baseline", positionId, channel)}`
                  }
                  disabled={
                    !filesReady ||
                    !inputDeviceId ||
                    inputChannelIndex === null ||
                    (busy !== null &&
                      busy !==
                        `capture:${captureKey("baseline", positionId, channel)}`)
                  }
                  onCapture={() =>
                    void capture("baseline", positionId, channel)
                  }
                  onCancel={() => void cancelCapture()}
                />
              ))}
            </div>
          ))}
        </div>
        <p className="live-stage__footnote">
          {copy.pairProgress
            .replace("{complete}", baselinePairs.toString())
            .replace("{total}", LIVE_BASELINE_POSITIONS.length.toString())}
        </p>
      </section>
      )}

      {currentStageId === "design" && (
      <section className="live-stage" aria-labelledby="live-design-title">
        <div className="live-stage__heading">
          <span aria-hidden="true">{stageNumber("design")}</span>
          <div>
            <h3 id="live-design-title" ref={stageHeadingRef} tabIndex={-1}>{copy.designTitle}</h3>
            <p>{copy.designBody}</p>
          </div>
        </div>
        <TargetCurveSelector
          copy={copy}
          target={target}
          customTarget={customTarget}
          disabled={busy !== null}
          importDisabled={!session || busy !== null}
          importing={busy === "target"}
          onTargetChange={onTargetChange}
          onImportTarget={(file) => void importTarget(file)}
        />
        {baselinePairs < LIVE_BASELINE_POSITIONS.length && (
          <p className="live-warning">{copy.incompleteBaselineWarning}</p>
        )}
        <button
          className="button button--primary"
          type="button"
          disabled={
            !baselineP0Ready ||
            !filesReady ||
            busy !== null ||
            (target === "custom" && customTarget === null)
          }
          onClick={() => void designTrial()}
        >
          {busy === "design" ? copy.designing : copy.designButton}
        </button>
        {design && (
          <div className="live-result live-result--trial">
            <div className="live-result__heading">
              <strong>{copy.trialReady}</strong>
              <span>{design.algorithmVersion}</span>
            </div>
            <dl className="live-metrics">
              <div><dt>{copy.positionsUsed}</dt><dd>{design.positionCount}</dd></div>
              <div><dt>{copy.leftRmse}</dt><dd>{formatMetric(design.leftPredictedRmseDb, "dB")}</dd></div>
              <div><dt>{copy.rightRmse}</dt><dd>{formatMetric(design.rightPredictedRmseDb, "dB")}</dd></div>
              <div><dt>{copy.maximumCut}</dt><dd>{formatMetric(design.maximumAttenuationDb, "dB")}</dd></div>
              <div><dt>{copy.maximumBoost}</dt><dd>{formatMetric(design.maximumBoostDb, "dB")}</dd></div>
              <div>
                <dt>{copy.predictedImprovement}</dt>
                <dd>
                  {`L ${formatMetric(design.leftRawRmseDb - design.leftPredictedRmseDb, "dB")} · R ${formatMetric(design.rightRawRmseDb - design.rightPredictedRmseDb, "dB")}`}
                </dd>
              </div>
            </dl>
            {Math.max(
              design.leftRawRmseDb - design.leftPredictedRmseDb,
              design.rightRawRmseDb - design.rightPredictedRmseDb,
            ) < 0.5 && (
              <p className="live-note">{copy.smallImprovementAdvisory}</p>
            )}
            <dl className="live-paths">
              <div><dt>{copy.trialZip}</dt><dd>{design.trialZipPath}</dd></div>
              <div><dt>{copy.trialWav}</dt><dd>{design.trialWavPath}</dd></div>
            </dl>
            <ZipDownloadControl
              label={copy.downloadTrialZip}
              savingLabel={copy.savingZip}
              savedTemplate={copy.zipSaved}
              saving={busy === "trial_download"}
              disabled={busy !== null}
              saved={trialDownload}
              onDownload={() => void downloadZip("trial")}
            />
            <p className="live-warning">{copy.predictedOnly}</p>
            <ol className="live-roon-checklist">
              {copy.trialChecklist.map((item) => <li key={item}>{item}</li>)}
            </ol>
            <label className="live-declaration">
              <input
                type="checkbox"
                checked={trialDeclared}
                disabled={busy !== null}
                onChange={(event) =>
                  void setTrialActivation(event.target.checked)
                }
              />
              <span>{copy.trialDeclaration}</span>
            </label>
          </div>
        )}
      </section>
      )}

      {currentStageId === "verify" && (
      <section className="live-stage" aria-labelledby="live-verify-title">
        <div className="live-stage__heading">
          <span aria-hidden="true">{stageNumber("verify")}</span>
          <div>
            <h3 id="live-verify-title" ref={stageHeadingRef} tabIndex={-1}>{copy.verifyTitle}</h3>
            <p>{copy.verifyBody}</p>
          </div>
        </div>
        <div className="live-verification-captures">
          {LIVE_CHANNELS.map((channel) => (
            <CaptureCell
              key={channel}
              copy={copy}
              label={copy.channelLabels[channel]}
              summary={captures[captureKey("verification", "P0", channel)]}
              busy={
                busy ===
                `capture:${captureKey("verification", "P0", channel)}`
              }
              disabled={
                !design ||
                !trialDeclared ||
                !inputDeviceId ||
                inputChannelIndex === null ||
                (busy !== null &&
                  busy !==
                    `capture:${captureKey("verification", "P0", channel)}`)
              }
              onCapture={() => void capture("verification", "P0", channel)}
              onCancel={() => void cancelCapture()}
            />
          ))}
        </div>
        <button
          className="button button--primary"
          type="button"
          disabled={!verificationP0Ready || busy !== null}
          onClick={() => void validate()}
        >
          {busy === "validate" ? copy.validating : copy.validateButton}
        </button>
        {verification && (
          <div
            className={`live-result live-result--${
              verification.passed ? "passed" : "failed"
            }`}
            role="status"
          >
            <strong>
              {verification.passed
                ? copy.verificationPassed
                : copy.verificationFailed}
            </strong>
            <dl className="live-metrics">
              <div><dt>{copy.leftVerifiedRmse}</dt><dd>{formatMetric(verification.leftVerifiedRmseDb, "dB")}</dd></div>
              <div><dt>{copy.rightVerifiedRmse}</dt><dd>{formatMetric(verification.rightVerifiedRmseDb, "dB")}</dd></div>
              <div><dt>{copy.leftPredictionGap}</dt><dd>{formatMetric(verification.leftPredictedVerifiedRmseDb, "dB")}</dd></div>
              <div><dt>{copy.rightPredictionGap}</dt><dd>{formatMetric(verification.rightPredictedVerifiedRmseDb, "dB")}</dd></div>
              {verification.leftGateRawRmseDb != null &&
                verification.leftGateVerifiedRmseDb != null && (
                  <div>
                    <dt>{copy.leftImprovementGate}</dt>
                    <dd>{`${formatMetric(verification.leftGateRawRmseDb, "dB")} → ${formatMetric(verification.leftGateVerifiedRmseDb, "dB")}`}</dd>
                  </div>
                )}
              {verification.rightGateRawRmseDb != null &&
                verification.rightGateVerifiedRmseDb != null && (
                  <div>
                    <dt>{copy.rightImprovementGate}</dt>
                    <dd>{`${formatMetric(verification.rightGateRawRmseDb, "dB")} → ${formatMetric(verification.rightGateVerifiedRmseDb, "dB")}`}</dd>
                  </div>
                )}
            </dl>
            {verification.issues.length > 0 && (
              <ul className="live-issues">
                {verification.issues.map((issue) => <li key={issue}>{issue}</li>)}
              </ul>
            )}
            {!verification.passed && (
              <div className="live-note live-verification-retry">
                <strong>{copy.verificationRetryTitle}</strong>
                <ol>
                  {copy.verificationRetrySteps.map((step) => (
                    <li key={step}>{step}</li>
                  ))}
                </ol>
              </div>
            )}
          </div>
        )}
      </section>
      )}

      {currentStageId === "export" && (
      <section className="live-stage" aria-labelledby="live-export-title">
        <div className="live-stage__heading">
          <span aria-hidden="true">{stageNumber("export")}</span>
          <div>
            <h3 id="live-export-title" ref={stageHeadingRef} tabIndex={-1}>{copy.exportTitle}</h3>
            <p>{copy.exportBody}</p>
          </div>
        </div>
        {design && verification?.passed && (
          <FinalMeasurementResults
            copy={copy}
            chartCopy={chartCopy}
            design={design}
            verification={verification}
            exported={exported}
          />
        )}
        <button
          className="button button--primary"
          type="button"
          disabled={!verification?.passed || busy !== null}
          onClick={() => void exportFinal()}
        >
          {busy === "export" ? copy.exporting : copy.exportButton}
        </button>
        {exported && (
          <div className="live-result live-result--passed" role="status">
            <strong>{copy.exportReady}</strong>
            <dl className="live-paths">
              <div><dt>{copy.finalZip}</dt><dd>{exported.zipPath}</dd></div>
              <div><dt>{copy.projectFile}</dt><dd>{exported.projectPath}</dd></div>
            </dl>
            <ZipDownloadControl
              label={copy.downloadFinalZip}
              savingLabel={copy.savingZip}
              savedTemplate={copy.zipSaved}
              saving={busy === "final_download"}
              disabled={busy !== null}
              saved={finalDownload}
              onDownload={() => void downloadZip("final")}
            />
            <dl className="live-metrics">
              <div><dt>{copy.headroom}</dt><dd>{formatMetric(exported.recommendedHeadroomDb, "dB")}</dd></div>
              <div><dt>{copy.firPeakBound}</dt><dd>{formatMetric(exported.firWorstCasePeakBoundDb, "dB")}</dd></div>
              <div><dt>{copy.finalBindingMagnitude}</dt><dd>{formatMetric(exported.final48kBindingMaximumMagnitudeDifferenceDb, "dB", 4)}</dd></div>
              <div><dt>{copy.finalBindingGroupDelay}</dt><dd>{formatMetric(exported.final48kBindingMaximumRelativeGroupDelayDifferenceMs, "ms", 4)}</dd></div>
              <div><dt>{copy.nativeRates}</dt><dd>{exported.nativeRateCount}</dd></div>
            </dl>
          </div>
        )}
      </section>
      )}

      <div className="live-wizard-actions">
        <button
          className="button button--secondary"
          type="button"
          disabled={currentStage === 0 || busy !== null}
          onClick={() => navigateToStage(currentStage - 1)}
        >
          <span aria-hidden="true">←</span>
          {copy.previousStage}
        </button>
        <button
          className="button button--primary"
          type="button"
          disabled={
            currentStage === stageTitles.length - 1 ||
            !canEnterStage(currentStage + 1) ||
            busy !== null
          }
          onClick={() => navigateToStage(currentStage + 1)}
        >
          {copy.nextStage}
          <span aria-hidden="true">→</span>
        </button>
      </div>

      {captureBusy && (
        <div className="live-capture-active" role="status" aria-live="polite">
          <span aria-hidden="true" />
          <div className="live-capture-active__content">
            <strong>
              {captureProgress
                ? copy.capturePhases[captureProgress.phase]
                : copy.captureActive}
            </strong>
            <p>{copy.captureActiveBody}</p>
            <CaptureProgressPanel copy={copy} progress={captureProgress} />
          </div>
          <button
            className="button button--secondary"
            type="button"
            onClick={() => void cancelCapture()}
          >
            {copy.cancelCapture}
          </button>
        </div>
      )}

      {error && (
        <p className="live-error" role="alert">
          <strong>{copy.errorTitle}</strong> {error}
        </p>
      )}

      <p className="live-boundary">
        <span aria-hidden="true">i</span>
        {copy.boundary}
      </p>
    </section>
  );
}

type ZipDownloadControlProps = {
  label: string;
  savingLabel: string;
  savedTemplate: string;
  saving: boolean;
  disabled: boolean;
  saved: LiveZipDownloadSummary | null;
  onDownload: () => void;
};

export function ZipDownloadControl({
  label,
  savingLabel,
  savedTemplate,
  saving,
  disabled,
  saved,
  onDownload,
}: ZipDownloadControlProps) {
  return (
    <div className="live-download">
      <button
        className="button button--secondary"
        type="button"
        disabled={disabled}
        onClick={onDownload}
      >
        {saving ? savingLabel : label}
      </button>
      {saved && (
        <p role="status">
          {savedTemplate.replace("{path}", saved.savedPath)}
        </p>
      )}
    </div>
  );
}

export function TargetCurveSelector({
  copy,
  target,
  customTarget,
  disabled,
  importDisabled,
  importing,
  onTargetChange,
  onImportTarget,
}: {
  copy: Messages["liveMeasurement"];
  target: LiveTargetKind;
  customTarget: TargetImportSummary | null;
  disabled: boolean;
  importDisabled: boolean;
  importing: boolean;
  onTargetChange: (target: LiveTargetKind) => void;
  onImportTarget: (file: File | undefined) => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  return (
    <>
      <fieldset className="live-target-options">
        <legend>{copy.selectedTarget}</legend>
        {(["bk", "harman", "custom"] as const).map((candidate) => (
          <label
            className={target === candidate ? "is-selected" : ""}
            key={candidate}
          >
            <input
              type="radio"
              name="live-target"
              value={candidate}
              checked={target === candidate}
              disabled={disabled}
              onChange={() => onTargetChange(candidate)}
            />
            <span>{copy.targetLabels[candidate]}</span>
          </label>
        ))}
      </fieldset>
      <div className="live-custom-target">
        <input
          ref={inputRef}
          className="visually-hidden"
          type="file"
          accept=".txt,text/plain"
          onChange={(event) =>
            onImportTarget(event.currentTarget.files?.[0])
          }
        />
        <button
          className="button button--secondary"
          type="button"
          disabled={importDisabled}
          onClick={() => {
            if (inputRef.current) {
              inputRef.current.value = "";
              inputRef.current.click();
            }
          }}
        >
          {importing ? copy.importingTarget : copy.importCustomTarget}
        </button>
        {customTarget ? (
          <>
            <p className="live-inputs__accepted">
              {copy.customTargetReady
                .replace("{file}", customTarget.fileName)
                .replace("{points}", customTarget.pointCount.toString())
                .replace(
                  "{minimum}",
                  customTarget.minimumFrequencyHz.toFixed(1),
                )
                .replace(
                  "{maximum}",
                  customTarget.maximumFrequencyHz.toFixed(1),
                )}
            </p>
            {!customTarget.correctionBandCovered && (
              <p className="live-warning">
                {copy.customTargetCoverageWarning}
              </p>
            )}
          </>
        ) : (
          target === "custom" && (
            <p className="live-warning">{copy.customTargetRequired}</p>
          )
        )}
        <small>{copy.customTargetFormat}</small>
      </div>
    </>
  );
}

export function SweepChannelAnalysis({
  copy,
  summary,
}: {
  copy: Messages["liveMeasurement"];
  summary: LiveSweepImportSummary;
}) {
  const channelValue = (
    channel: LiveReferenceChannel | null,
    separationDb: number | null,
  ) => {
    if (channel === null) return copy.sweepChannelUnknown;
    const label = copy.sweepChannelLabels[channel];
    if (
      separationDb === null ||
      channel === "mono" ||
      channel === "identical_stereo"
    ) {
      return label;
    }
    return `${label} · ${copy.sweepChannelDominance.replace(
      "{db}",
      separationDb.toFixed(1),
    )}`;
  };
  return (
    <div className="live-sweep-channel-analysis">
      <strong>{copy.sweepChannelAnalysisTitle}</strong>
      <dl>
        <div>
          <dt>{copy.sweepSignalLabels.measurement}</dt>
          <dd>{copy.sweepChannelLabels[summary.sourceReferenceChannel]}</dd>
        </div>
        <div>
          <dt>{copy.sweepSignalLabels.start}</dt>
          <dd>
            {channelValue(
              summary.startMarkerChannel,
              summary.startMarkerChannelSeparationDb,
            )}
          </dd>
        </div>
        <div>
          <dt>{copy.sweepSignalLabels.end}</dt>
          <dd>
            {channelValue(
              summary.endMarkerChannel,
              summary.endMarkerChannelSeparationDb,
            )}
          </dd>
        </div>
      </dl>
    </div>
  );
}

export function CaptureProgressPanel({
  copy,
  progress,
}: {
  copy: Messages["liveMeasurement"];
  progress: LiveCaptureProgress | null;
}) {
  const status = progress?.levelStatus ?? "waiting";
  const peakDbfs = progress?.peakDbfs ?? null;
  const meterPercent = peakDbfs === null
    ? 0
    : Math.max(0, Math.min(100, ((peakDbfs + 60) / 60) * 100));
  return (
    <div className={`live-level-monitor live-level-monitor--${status}`}>
      <div className="live-marker-progress">
        <span className={progress?.startMarkerDetected ? "is-detected" : ""}>
          <i aria-hidden="true">
            {progress?.startMarkerDetected ? "✓" : "1"}
          </i>
          {copy.startMarker}
          <small>
            {progress?.startMarkerDetected
              ? copy.markerDetected
              : copy.markerWaiting}
          </small>
        </span>
        <span className={progress?.endMarkerDetected ? "is-detected" : ""}>
          <i aria-hidden="true">
            {progress?.endMarkerDetected ? "✓" : "2"}
          </i>
          {copy.endMarker}
          <small>
            {progress?.endMarkerDetected
              ? copy.markerDetected
              : copy.markerWaiting}
          </small>
        </span>
      </div>
      <div className="live-level-monitor__heading">
        <strong>{copy.levelMeterTitle}</strong>
        <span>{copy.levelStatuses[status]}</span>
      </div>
      <div
        className="live-level-monitor__meter"
        role="meter"
        aria-label={copy.levelPeak}
        aria-valuemin={-60}
        aria-valuemax={0}
        aria-valuenow={peakDbfs ?? -60}
        aria-valuetext={formatMetric(peakDbfs, "dBFS", 1)}
      >
        <span style={{ width: `${meterPercent}%` }} />
        <i />
        <i />
      </div>
      <dl className="live-level-monitor__metrics">
        <div>
          <dt>{copy.levelPeak}</dt>
          <dd>{formatMetric(peakDbfs, "dBFS", 1)}</dd>
        </div>
        <div>
          <dt>{copy.levelRms}</dt>
          <dd>{formatMetric(progress?.rmsDbfs ?? null, "dBFS", 1)}</dd>
        </div>
        <div>
          <dt>{copy.estimatedSpl}</dt>
          <dd>
            {progress?.estimatedSplDb == null
              ? copy.estimatedSplUnavailable
              : formatMetric(progress.estimatedSplDb, "dB SPL", 1)}
          </dd>
        </div>
      </dl>
      <p>{copy.levelGuidance[status]}</p>
      <small>{copy.recommendedLevel}</small>
      <small>{copy.estimatedSplNote}</small>
    </div>
  );
}

function MeasurementCacheControls({
  copy,
  result,
  busy,
  disabled,
  onRestore,
}: {
  copy: Messages["liveMeasurement"];
  result: LiveMeasurementCacheRestoreSummary | null;
  busy: boolean;
  disabled: boolean;
  onRestore: () => void;
}) {
  return (
    <div className="live-cache-controls">
      <button
        className="button button--secondary"
        type="button"
        disabled={disabled}
        onClick={onRestore}
      >
        {busy
          ? copy.restoringAcceptedMeasurements
          : copy.restoreAcceptedMeasurements}
      </button>
      {result && (
        <div className="live-result" role="status">
          <strong>
            {result.restoredCaptures.length > 0
              ? copy.restoredAcceptedMeasurements.replace(
                  "{count}",
                  result.restoredCaptures.length.toString(),
                )
              : copy.noAcceptedMeasurementsToRestore}
          </strong>
          {result.sourceSessionIds.length > 0 && (
            <small>
              {copy.restoredMeasurementSources}:{" "}
              {result.sourceSessionIds.join(", ")}
            </small>
          )}
        </div>
      )}
    </div>
  );
}

function CaptureCell({
  copy,
  tableCell = false,
  label,
  summary,
  busy,
  disabled,
  onCapture,
  onCancel,
}: {
  copy: Messages["liveMeasurement"];
  tableCell?: boolean;
  label?: string;
  summary?: LiveCaptureSummary;
  busy: boolean;
  disabled: boolean;
  onCapture: () => void;
  onCancel: () => void;
}) {
  const issueGuidance = summary
    ? captureIssueGuidance(summary.issueCodes, copy)
    : "";
  return (
    <div
      className={`live-capture-cell ${
        summary
          ? summary.accepted
            ? "live-capture-cell--accepted"
            : "live-capture-cell--rejected"
          : ""
      }`}
      role={tableCell ? "cell" : undefined}
    >
      {label && <strong>{label}</strong>}
      <span>
        {busy
          ? copy.listening
          : summary
            ? summary.accepted
              ? copy.accepted
              : copy.rejected
            : copy.pending}
      </span>
      {summary && (
        <small>
          {summary.restoredFromCache && (
            <>
              {copy.cachedMeasurementBadge}
              {" · "}
            </>
          )}
          {formatMetric(summary.capturePeakDbfs, "dBFS", 1)}
          {" · "}
          {formatMetric(summary.captureSnrDb, "dB SNR", 1)}
          {" · "}
          {copy.reconstructionFit}{" "}
          {formatMetric(summary.reconstructionFitDb, "dB", 1)}
          {!summary.reconstructionFitRequired &&
            ` (${copy.reconstructionFitAdvisory})`}
        </small>
      )}
      {summary && (
        <>
          <div className="live-capture-cell__markers">
            <span className={summary.startMarkerDetected ? "is-detected" : ""}>
              {copy.startMarker}:{" "}
              {summary.startMarkerDetected
                ? copy.markerDetected
                : copy.markerWaiting}
            </span>
            <span className={summary.endMarkerDetected ? "is-detected" : ""}>
              {copy.endMarker}:{" "}
              {summary.endMarkerDetected
                ? copy.markerDetected
                : copy.markerWaiting}
            </span>
          </div>
          <div
            className={`live-capture-cell__level live-capture-cell__level--${summary.levelAssessment.status}`}
          >
            <strong>
              {copy.levelStatuses[summary.levelAssessment.status]}
            </strong>
            <span>
              {formatMetric(
                summary.levelAssessment.measurementPeakDbfs,
                "dBFS peak",
                1,
              )}
              {" · "}
              {summary.levelAssessment.estimatedSplDb == null
                ? copy.estimatedSplUnavailable
                : formatMetric(
                    summary.levelAssessment.estimatedSplDb,
                    "dB SPL",
                    1,
                  )}
            </span>
          </div>
        </>
      )}
      {summary && !summary.accepted && summary.issueCodes.length > 0 && (
        <small className="live-capture-cell__issue">
          {issueGuidance}
        </small>
      )}
      {summary &&
        summary.accepted &&
        (summary.audioStreamDiagnostics.timestampGapFrames > 0 ||
          summary.audioStreamDiagnostics.timestampDiscontinuityCount > 0) && (
          <small className="live-capture-cell__diagnostic">
            {copy.timestampDiagnostic.replace(
              "{frames}",
              summary.audioStreamDiagnostics.timestampGapFrames.toString(),
            )}
          </small>
        )}
      <button
        className="button button--secondary"
        type="button"
        disabled={disabled}
        onClick={busy ? onCancel : onCapture}
      >
        {busy
          ? copy.cancelCapture
          : summary
            ? copy.retryCapture
            : copy.startCapture}
      </button>
    </div>
  );
}

function captureIssueGuidance(
  issueCodes: readonly string[],
  copy: Messages["liveMeasurement"],
): string {
  const guidance = classifyLiveCaptureIssues(issueCodes);
  if (guidance === "too_low") {
    return copy.levelGuidance.too_low;
  }
  if (guidance === "high") return copy.levelGuidance.high;
  return copy.captureIssueGuidance[guidance];
}
