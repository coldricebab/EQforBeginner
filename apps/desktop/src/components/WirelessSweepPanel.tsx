import { invoke, isTauri } from "@tauri-apps/api/core";
import { useRef, useState } from "react";
import type { Messages } from "../i18n/types";
import {
  formatDbfs,
  formatPercent,
  formatPpm,
  inputChannelIndexFromChoice,
  inputDeviceIdFromChoice,
  MAX_WIRELESS_SWEEP_BYTES,
  type WirelessSweepCapture,
  type WirelessSweepImport,
  type WirelessSweepUiState,
} from "../lib/wirelessSweep";

type WirelessSweepPanelProps = {
  copy: Messages["wirelessSweep"];
  selectedInput: string;
  selectedInputName: string | null;
};

export function WirelessSweepPanel({
  copy,
  selectedInput,
  selectedInputName,
}: WirelessSweepPanelProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const cancelRequestedRef = useRef(false);
  const [reference, setReference] = useState<WirelessSweepImport | null>(null);
  const [fileName, setFileName] = useState("");
  const [state, setState] = useState<WirelessSweepUiState>("empty");
  const [capture, setCapture] = useState<WirelessSweepCapture | null>(null);
  const [error, setError] = useState("");
  const inputDeviceId = inputDeviceIdFromChoice(selectedInput);
  const inputChannelIndex = inputChannelIndexFromChoice(selectedInput);

  const importFile = async (file: File | undefined) => {
    setCapture(null);
    setError("");
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".wav")) {
      setReference(null);
      setState("error");
      setError(copy.errors.wavOnly);
      return;
    }
    if (file.size > MAX_WIRELESS_SWEEP_BYTES) {
      setReference(null);
      setState("error");
      setError(copy.errors.fileTooLarge);
      return;
    }
    if (!isTauri()) {
      setReference(null);
      setState("error");
      setError(copy.errors.nativeOnly);
      return;
    }

    setState("importing");
    try {
      // Keep the file as a top-level ArrayBuffer so Tauri uses its binary IPC
      // transport. The Rust command also accepts Tauri's JSON byte-array
      // fallback for webviews where the custom protocol is unavailable.
      const bytes = await file.arrayBuffer();
      const imported = await invoke<WirelessSweepImport>(
        "import_wireless_sweep",
        bytes,
      );
      setFileName(file.name);
      setReference(imported);
      setState("ready");
    } catch (caught) {
      setReference(null);
      setState("error");
      setError(String(caught));
    }
  };

  const beginCapture = async () => {
    if (
      !reference ||
      !inputDeviceId ||
      inputChannelIndex === null ||
      state === "listening"
    ) return;
    setCapture(null);
    setError("");
    cancelRequestedRef.current = false;
    setState("listening");
    try {
      const result = await invoke<WirelessSweepCapture>(
        "capture_wireless_sweep",
        {
          sweepId: reference.sweepId,
          inputDeviceId,
          inputChannelIndex,
          waitSeconds: 20,
        },
      );
      if (cancelRequestedRef.current) return;
      setCapture(result);
      setState(result.status);
    } catch (caught) {
      if (cancelRequestedRef.current) {
        setState("ready");
      } else {
        setState("error");
        setError(String(caught));
      }
    }
  };

  const cancelCapture = async () => {
    cancelRequestedRef.current = true;
    try {
      await invoke<boolean>("cancel_wireless_sweep_capture");
      setState("ready");
    } catch (caught) {
      cancelRequestedRef.current = false;
      setState("error");
      setError(String(caught));
    }
  };

  const statusLabel = copy.status[state];
  const referenceChannel = reference
    ? copy.referenceChannels[reference.referenceChannel]
    : "";
  const canListen =
    reference !== null &&
    inputDeviceId !== null &&
    inputChannelIndex !== null &&
    state !== "importing" &&
    state !== "listening";

  return (
    <section className="wireless-sweep" aria-labelledby="wireless-sweep-title">
      <div className="wireless-sweep__heading">
        <div>
          <p className="eyebrow">{copy.eyebrow}</p>
          <h3 id="wireless-sweep-title">{copy.title}</h3>
          <p>{copy.body}</p>
        </div>
        <span className={`wireless-sweep__status wireless-sweep__status--${state}`}>
          <i aria-hidden="true" />
          {statusLabel}
        </span>
      </div>

      <div className="wireless-sweep__flow">
        <article className="wireless-sweep__step">
          <span className="wireless-sweep__number" aria-hidden="true">1</span>
          <div>
            <strong>{copy.chooseTitle}</strong>
            <p>{copy.chooseBody}</p>
            <input
              ref={inputRef}
              className="visually-hidden"
              type="file"
              accept=".wav,audio/wav,audio/x-wav"
              onChange={(event) => void importFile(event.currentTarget.files?.[0])}
            />
            <button
              className="button button--secondary"
              type="button"
              disabled={state === "importing" || state === "listening"}
              onClick={() => inputRef.current?.click()}
            >
              {state === "importing" ? copy.importing : copy.chooseButton}
            </button>
          </div>
        </article>

        <article className="wireless-sweep__step">
          <span className="wireless-sweep__number" aria-hidden="true">2</span>
          <div>
            <strong>{copy.armTitle}</strong>
            <p>{copy.armBody}</p>
            <div className="wireless-sweep__microphone">
              <span>{copy.microphone}</span>
              <strong>{selectedInputName ?? copy.noMicrophone}</strong>
            </div>
            <button
              className="button button--primary"
              type="button"
              disabled={!canListen}
              onClick={() => void beginCapture()}
            >
              {state === "listening" ? copy.listeningButton : copy.armButton}
            </button>
          </div>
        </article>

        <article className="wireless-sweep__step">
          <span className="wireless-sweep__number" aria-hidden="true">3</span>
          <div>
            <strong>{copy.playTitle}</strong>
            <p>{copy.playBody}</p>
            <ol className="wireless-sweep__checklist">
              {copy.playChecklist.map((item) => <li key={item}>{item}</li>)}
            </ol>
          </div>
        </article>
      </div>

      {reference && (
        <dl className="wireless-sweep__reference" aria-label={copy.referenceSummary}>
          <div><dt>{copy.file}</dt><dd>{fileName}</dd></div>
          <div><dt>{copy.format}</dt><dd>{reference.sampleRateHz / 1000} kHz · {reference.channels} ch</dd></div>
          <div><dt>{copy.duration}</dt><dd>{reference.durationSeconds.toFixed(2)} s</dd></div>
          <div><dt>{copy.referenceChannel}</dt><dd>{referenceChannel}</dd></div>
          <div><dt>{copy.filePeak}</dt><dd>{formatDbfs(reference.peakDbfs)}</dd></div>
          <div>
            <dt>{copy.sweepCheck}</dt>
            <dd className={reference.sweepLike ? "is-safe" : "is-warning"}>
              {reference.sweepLike ? copy.sweepLike : copy.sweepUncertain}
            </dd>
          </div>
        </dl>
      )}

      {state === "listening" && (
        <div className="wireless-sweep__listening" role="status" aria-live="polite">
          <span className="wireless-sweep__pulse" aria-hidden="true" />
          <div><strong>{copy.listeningTitle}</strong><p>{copy.listeningBody}</p></div>
          <button
            className="button button--secondary"
            type="button"
            onClick={() => void cancelCapture()}
          >
            {copy.cancelButton}
          </button>
        </div>
      )}

      {capture && (
        <div
          className={`wireless-sweep__result wireless-sweep__result--${capture.status}`}
          role="status"
          aria-live="polite"
        >
          <div className="wireless-sweep__result-heading">
            <div>
              <strong>{copy.resultTitles[capture.status]}</strong>
              <p>{copy.resultBodies[capture.status]}</p>
            </div>
            {capture.status === "detected" && (
              <span>{formatPercent(capture.correlation)}</span>
            )}
          </div>
          <dl>
            <div><dt>{copy.detectedStart}</dt><dd>{capture.detectedStartSeconds === null ? "—" : `${capture.detectedStartSeconds.toFixed(3)} s`}</dd></div>
            <div><dt>{copy.confidence}</dt><dd>{formatPercent(capture.correlation)}</dd></div>
            <div><dt>{copy.confidenceMargin}</dt><dd>{formatPercent(capture.confidenceMargin)}</dd></div>
            <div><dt>{copy.clockDrift}</dt><dd>{formatPpm(capture.clockDriftPpm)}</dd></div>
            <div><dt>{copy.inputPeak}</dt><dd>{formatDbfs(capture.inputPeakDbfs)}</dd></div>
            <div><dt>{copy.clippedSamples}</dt><dd>{capture.clippedSamples.toLocaleString()}</dd></div>
          </dl>
          {(capture.dropoutSuspected || capture.streamErrorCount > 0) && (
            <p className="wireless-sweep__warning">
              {copy.streamWarning
                .replace("{errors}", capture.streamErrorCount.toString())}
            </p>
          )}
          {!capture.qualityAccepted && (
            <p className="wireless-sweep__warning">
              {copy.qualityWarning
                .replace("{clipped}", capture.clippedSamples.toLocaleString())}
            </p>
          )}
        </div>
      )}

      {error && (
        <p className="wireless-sweep__error" role="alert">{error}</p>
      )}

      <p className="wireless-sweep__boundary">
        <span aria-hidden="true">i</span>
        {copy.boundary}
      </p>
    </section>
  );
}
