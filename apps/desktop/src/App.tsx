import { invoke, isTauri } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import {
  LiveMeasurementPanel,
  type LiveTargetKind,
} from "./components/LiveMeasurementPanel";
import { messages, type Locale } from "./i18n";
import {
  buildInputDeviceChoices,
  type AudioDeviceInventory,
  type AudioScanError,
  type AudioScanState,
} from "./lib/audio";

function App() {
  const [locale, setLocale] = useState<Locale>("ko");
  const [target, setTarget] = useState<LiveTargetKind>("bk");
  const [audioInventory, setAudioInventory] =
    useState<AudioDeviceInventory | null>(null);
  const [audioScanState, setAudioScanState] =
    useState<AudioScanState>("idle");
  const [audioScanError, setAudioScanError] =
    useState<AudioScanError>(null);
  const [selectedInput, setSelectedInput] = useState("");
  const copy = messages[locale];
  const inputChoices = buildInputDeviceChoices(audioInventory?.inputs ?? []);
  const selectedInputChoice = inputChoices.find(
    (choice) => choice.value === selectedInput,
  );
  const selectedInputName = selectedInputChoice
    ? `${selectedInputChoice.name} · ${copy.liveMeasurement.inputChannel.replace(
        "{channel}",
        (selectedInputChoice.inputChannelIndex + 1).toString(),
      )}`
    : null;

  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  const scanInputDevices = async () => {
    if (!isTauri()) {
      setAudioScanState("error");
      setAudioScanError({ kind: "native_shell_required" });
      return;
    }

    setAudioScanState("scanning");
    setAudioScanError(null);
    try {
      const inventory = await invoke<AudioDeviceInventory>(
        "scan_input_devices",
      );
      const choices = buildInputDeviceChoices(inventory.inputs);
      setAudioInventory(inventory);
      setSelectedInput((current) =>
        choices.some((choice) => choice.value === current)
          ? current
          : (choices[0]?.value ?? ""),
      );
      setAudioScanState("ready");
    } catch (error) {
      setAudioInventory(null);
      setSelectedInput("");
      setAudioScanState("error");
      setAudioScanError({ kind: "runtime", detail: String(error) });
    }
  };

  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">
        {copy.meta.skipToContent}
      </a>
      <header className="topbar">
        <div className="brand-lockup">
          <span className="brand-mark" aria-hidden="true">
            <i /><i /><i /><i />
          </span>
          <div>
            <strong>{copy.brand.name}</strong>
            <span>{copy.brand.tagline}</span>
          </div>
        </div>
        <div className="topbar__right">
          <p className="privacy-copy">
            <span aria-hidden="true">◇</span>
            {copy.brand.privacy}
          </p>
          <span className="shell-badge">{copy.status.shell}</span>
          <div
            className="language-switcher"
            role="group"
            aria-label={copy.meta.language}
          >
            <button
              type="button"
              className={locale === "ko" ? "is-active" : ""}
              aria-pressed={locale === "ko"}
              onClick={() => setLocale("ko")}
            >
              한국어
            </button>
            <button
              type="button"
              className={locale === "en" ? "is-active" : ""}
              aria-pressed={locale === "en"}
              onClick={() => setLocale("en")}
            >
              EN
            </button>
          </div>
        </div>
      </header>

      <main id="main-content" className="main-content main-content--wizard">
        <LiveMeasurementPanel
          copy={copy.liveMeasurement}
          chartCopy={copy.chart}
          inputChoices={inputChoices}
          selectedInput={selectedInput}
          selectedInputName={selectedInputName}
          audioScanState={audioScanState}
          audioScanError={audioScanError}
          onScanInputDevices={() => void scanInputDevices()}
          onSelectedInputChange={setSelectedInput}
          target={target}
          onTargetChange={setTarget}
        />
      </main>
    </div>
  );
}

export default App;
