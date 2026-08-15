import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { messages } from "../i18n";
import {
  CaptureProgressPanel,
  LiveMeasurementPanel,
  SUBWOOFER_DELAY_DEFAULTS,
  SweepChannelAnalysis,
  SweepPlaybackToggle,
  TargetCurveSelector,
  WIDE_BAND_SEARCH_DEFAULTS,
  ZipDownloadControl,
} from "./LiveMeasurementPanel";
import { parseCrossoverCandidates } from "../lib/liveMeasurement";

describe("LiveMeasurementPanel", () => {
  it("starts the sub delay search wide enough for a beginner to get a real answer", () => {
    const minimum = Number(SUBWOOFER_DELAY_DEFAULTS.minimumMs);
    const maximum = Number(SUBWOOFER_DELAY_DEFAULTS.maximumMs);
    const step = Number(SUBWOOFER_DELAY_DEFAULTS.stepMs);

    // Both locales tell the user a negative recommendation means the delay
    // belongs on the subwoofer. A minimum of 0 would make that answer
    // unreachable no matter what the room does.
    expect(minimum).toBeLessThan(0);
    for (const locale of [messages.en, messages.ko]) {
      expect(locale.liveMeasurement.subwooferDelayAxisNote).toMatch(/sub|서브/i);
    }

    // The search looks one crossover half-period either side of the measured
    // arrival. At 80 Hz that is 6.25 ms each way, so a range narrower than
    // 12.5 ms is clipped before the room is even measured.
    expect(maximum - minimum).toBeGreaterThanOrEqual(1_000 / 80);
    // A DSP subwoofer's processing latency plus placement offset commonly
    // exceeds the old 5 ms ceiling.
    expect(maximum).toBeGreaterThan(5);

    // A step finer than this cannot be entered on real amplifiers.
    expect(step).toBeGreaterThanOrEqual(0.1);

    // Backend limits: at most 1,001 delay values, and 12 crossovers times both
    // polarities must stay inside the 10,000-candidate cap.
    const delayValues = Math.ceil((maximum - minimum) / step) + 1;
    expect(delayValues).toBeLessThanOrEqual(1_001);
    expect(delayValues * 2 * 12).toBeLessThanOrEqual(10_000);

    // And they must satisfy the adapter's own admission bounds.
    expect(minimum).toBeGreaterThanOrEqual(-20);
    expect(maximum).toBeLessThanOrEqual(50);
    expect(step).toBeLessThanOrEqual(5);
  });

  it("ships wide-band defaults the backend accepts and hardware can dial", () => {
    const candidates = parseCrossoverCandidates(
      WIDE_BAND_SEARCH_DEFAULTS.crossoverCandidates,
    );
    // 2-12 ascending 30-200 Hz values, all multiples of the 10 Hz steps
    // consumer bass management offers.
    expect(candidates).not.toBeNull();
    expect(candidates!.length).toBeGreaterThanOrEqual(2);
    expect(candidates!.length).toBeLessThanOrEqual(12);
    for (const candidate of candidates!) {
      expect(candidate % 10).toBe(0);
    }
    // Every default candidate must sit at or below the default measured dial,
    // or the backend rejects the plan (the low-pass replacement ratio would
    // amplify noise above the dial).
    const dial = Number(WIDE_BAND_SEARCH_DEFAULTS.subMeasuredLowPassHz);
    expect(dial).toBeGreaterThanOrEqual(120);
    expect(dial).toBeLessThanOrEqual(500);
    expect(Math.max(...candidates!)).toBeLessThanOrEqual(dial);
    // LR4 is the declared default slope on both branches.
    expect(WIDE_BAND_SEARCH_DEFAULTS.slope).toBe("lr4");
  });

  it("requires an explicit 2.0 or 2.1 choice before starting the live flow", () => {
    const inputChoice = {
      value: JSON.stringify(["coreaudio:umik", 1, "f32", 64, 512]),
      deviceId: "coreaudio:umik",
      name: "UMIK-1",
      isDefault: true,
      inputChannelIndex: 0,
      configuration: {
        sampleRateHz: 48_000,
        channels: 1,
        sampleFormat: "f32",
        minimumBufferSizeFrames: 64,
        maximumBufferSizeFrames: 512,
      },
      currentSampleRateHz: 48_000,
    };
    const html = renderToStaticMarkup(
      <LiveMeasurementPanel
        copy={messages.en.liveMeasurement}
        chartCopy={messages.en.chart}
        inputChoices={[inputChoice]}
        selectedInput={inputChoice.value}
        selectedInputName="UMIK-1"
        audioScanState="ready"
        audioScanError={null}
        onScanInputDevices={() => undefined}
        onSelectedInputChange={() => undefined}
        outputChoices={[]}
        selectedOutput=""
        outputScanState="idle"
        outputScanError={null}
        onScanOutputDevices={() => undefined}
        onSelectedOutputChange={() => undefined}
        target="harman_6db"
        onTargetChange={() => undefined}
      />,
    );

    expect(html).toContain("Live correction session");
    expect(html).toContain("System-dependent live correction stages");
    expect(html).toContain("Start a project");
    expect(html).toContain("Stereo (L/R)");
    expect(html).toContain("2.1 (L/R + one sub)");
    expect(html).toContain("Choose either stereo or a 2.1 system");
    expect(html).toContain("Raw multipoint measurement");
    expect(html).toContain("Final Roon convolution");
    expect(html).toContain("Start new live project");
    // In-app sweep playback is strictly opt-in: with no speaker chosen, the
    // app still never scans or opens an output device.
    expect(html).toContain(
      "leave that empty and no output device is ever scanned or opened",
    );
    expect(html).not.toContain("P0 center");
    expect(html).not.toContain("Manual hardware-setting stage");
    expect(html).not.toContain("UMIK-1");
    expect(messages.en.liveMeasurement.trialDeclaration).toContain(
      "this exact trial ZIP",
    );
    expect(html).not.toContain("Final package ready");
  });

  it("shows marker completion and an accessible live sweep level meter", () => {
    const html = renderToStaticMarkup(
      <CaptureProgressPanel
        copy={messages.ko.liveMeasurement}
        progress={{
          algorithmVersion: "known-marker-capture-endpoint-v4",
          phase: "measuring_sweep",
          elapsedSeconds: 8.5,
          peakDbfs: -14.2,
          rmsDbfs: -31.4,
          estimatedSplDb: 89.0,
          levelStatus: "good",
          startMarkerDetected: true,
          endMarkerDetected: false,
          automaticCompletionArmed: true,
        }}
      />,
    );

    expect(html).toContain("시작 신호");
    expect(html).toContain("종료 신호");
    expect(html).toContain("확인됨");
    expect(html).toContain("측정 음압·입력 레벨");
    expect(html).toContain("-14.2 dBFS");
    expect(html).toContain("89.0 dB SPL");
    expect(html).toContain('role="meter"');
    expect(html).toContain('aria-valuenow="-14.2"');
  });

  it("shows microphone activity before the start marker is recognized", () => {
    const html = renderToStaticMarkup(
      <CaptureProgressPanel
        copy={messages.en.liveMeasurement}
        progress={{
          algorithmVersion: "known-marker-capture-endpoint-v4",
          phase: "waiting_for_start",
          elapsedSeconds: 2,
          peakDbfs: -37.5,
          rmsDbfs: null,
          estimatedSplDb: null,
          levelStatus: "waiting",
          startMarkerDetected: false,
          endMarkerDetected: false,
          automaticCompletionArmed: true,
        }}
      />,
    );

    expect(html).toContain("Input/sweep peak");
    expect(html).toContain("-37.5 dBFS");
    expect(html).toContain("microphone input is arriving");
    expect(html).toContain('aria-valuenow="-37.5"');
  });

  it("reports which uploaded WAV channel carries each marker", () => {
    const html = renderToStaticMarkup(
      <SweepChannelAnalysis
        copy={messages.ko.liveMeasurement}
        summary={{
          channel: "left",
          sha256: "abc",
          sourceChannels: 2,
          sampleRateHz: 48_000,
          sourceDurationSeconds: 13,
          measurementStartSeconds: 1,
          measurementEndSeconds: 12,
          measurementDurationSeconds: 11,
          measurementPeakDbfs: -12,
          sourceReferenceChannel: "left",
          timingMarkerCount: 2,
          markerChannelAnalysisVersion:
            "uploaded-wav-marker-channel-analysis-v1",
          startMarkerChannel: "right",
          endMarkerChannel: "right",
          startMarkerChannelSeparationDb: 42.5,
          endMarkerChannelSeparationDb: 41.2,
        }}
      />,
    );

    expect(html).toContain("업로드 WAV 신호 채널 분석");
    expect(html).toContain("<dt>본 스윕</dt><dd>L 채널</dd>");
    expect(html).toContain("<dt>시작 신호</dt><dd>R 채널");
    expect(html).toContain("42.5 dB 우세");
    expect(html).toContain("<dt>종료 신호</dt><dd>R 채널");
  });

  it("keeps custom TXT targets available in the live design UI", () => {
    const html = renderToStaticMarkup(
      <TargetCurveSelector
        copy={messages.ko.liveMeasurement}
        target="custom"
        customTarget={{
          fileName: "Harman-6dB_REW.txt",
          sha256: "abc",
          parserVersion: "target-txt-parser-v1",
          pointCount: 26,
          minimumFrequencyHz: 9.98753,
          maximumFrequencyHz: 20_000,
          correctionBandCovered: true,
          alignmentLowerHz: 200,
          alignmentUpperHz: 500,
          storedPath: "/tmp/Harman-6dB_REW.txt",
        }}
        disabled={false}
        importDisabled={false}
        importing={false}
        onTargetChange={() => undefined}
        onImportTarget={() => undefined}
      />,
    );

    expect(html).toContain("사용자 TXT");
    expect(html).toContain("사용자 타겟 TXT 불러오기");
    expect(html).toContain("Harman-6dB_REW.txt");
    expect(html).toContain("26개 점");
    expect(html).toContain("200–500 Hz");
    expect(html).toContain('accept=".txt,text/plain"');
    // The only built-in option is the Dirac-sourced default; the removed
    // B&K/Harman style presets must not resurface, and the source link must
    // point at Dirac's official page.
    expect(html).toContain("Harman-6dB (Dirac)");
    expect(html).toContain("https://www.dirac.com/resources/target-curve");
    expect(html).not.toContain("B&amp;K");
    expect(html).not.toContain("Harman 스타일");
  });

  it("offers in-app sweep playback only once a speaker has been chosen", () => {
    const withoutSpeaker = renderToStaticMarkup(
      <SweepPlaybackToggle
        copy={messages.ko.liveMeasurement}
        checked
        speakerSelected={false}
        deviceSampleRateHz={48_000}
        disabled={false}
        onChange={() => undefined}
      />,
    );

    // No speaker means no playback, whatever the remembered checkbox says.
    expect(withoutSpeaker).toContain("disabled");
    expect(withoutSpeaker).not.toContain("checked");
    expect(withoutSpeaker).toContain("2단계에서 출력 장치를 선택하면");

    const withSpeaker = renderToStaticMarkup(
      <SweepPlaybackToggle
        copy={messages.ko.liveMeasurement}
        checked
        speakerSelected
        deviceSampleRateHz={48_000}
        disabled={false}
        onChange={() => undefined}
      />,
    );

    expect(withSpeaker).toContain("checked");
    expect(withSpeaker).not.toContain("disabled");
    expect(withSpeaker).toContain("앱에서 스윕 재생");
    // The note must keep the head start separate from recognition.
    expect(withSpeaker).toContain("3초");
    expect(withSpeaker).toContain("타이밍 마커");
  });

  it("says the sweep meets a device that is not on the project rate", () => {
    const converted = renderToStaticMarkup(
      <SweepPlaybackToggle
        copy={messages.ko.liveMeasurement}
        checked
        speakerSelected
        deviceSampleRateHz={44_100}
        disabled={false}
        onChange={() => undefined}
      />,
    );

    // Naming the rate is what lets the user tie the note to their device.
    expect(converted).toContain("44100 Hz");
    expect(converted).toContain("변환해");
    // The promise that matters: nothing else on that device is disturbed.
    expect(converted).toContain("장치 설정은 바뀌지 않고");
    expect(
      renderToStaticMarkup(
        <SweepPlaybackToggle
          copy={messages.en.liveMeasurement}
          checked
          speakerSelected
          deviceSampleRateHz={44_100}
          disabled={false}
          onChange={() => undefined}
        />,
      ),
    ).toContain("44100 Hz");

    // A device already on the project rate needs no conversion, so nothing is
    // announced.
    const native = renderToStaticMarkup(
      <SweepPlaybackToggle
        copy={messages.ko.liveMeasurement}
        checked
        speakerSelected
        deviceSampleRateHz={48_000}
        disabled={false}
        onChange={() => undefined}
      />,
    );
    expect(native).not.toContain("변환해");

    // Neither is one the user plays externally.
    const external = renderToStaticMarkup(
      <SweepPlaybackToggle
        copy={messages.ko.liveMeasurement}
        checked={false}
        speakerSelected
        deviceSampleRateHz={44_100}
        disabled={false}
        onChange={() => undefined}
      />,
    );
    expect(external).not.toContain("변환해");
  });

  it("renders a clickable ZIP save control and its completed destination", () => {
    const html = renderToStaticMarkup(
      <ZipDownloadControl
        label={messages.ko.liveMeasurement.downloadTrialZip}
        savingLabel={messages.ko.liveMeasurement.savingZip}
        savedTemplate={messages.ko.liveMeasurement.zipSaved}
        saving={false}
        disabled={false}
        saved={{
          artifactKind: "trial",
          fileName: "trial.zip",
          savedPath: "/Users/test/Downloads/trial.zip",
          byteCount: 1234,
          sha256: "abc",
        }}
        onDownload={() => undefined}
      />,
    );

    expect(html).toContain(">시험 ZIP 다운로드…</button>");
    expect(html).toContain('role="status"');
    expect(html).toContain("ZIP 저장 완료: /Users/test/Downloads/trial.zip");
    expect(html).not.toContain("disabled");
  });
});
