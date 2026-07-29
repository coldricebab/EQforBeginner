export type ProjectStreamConfig = {
  sampleRateHz: number;
  channels: number;
  sampleFormat: string;
  minimumBufferSizeFrames: number | null;
  maximumBufferSizeFrames: number | null;
};

export type AudioDeviceDescriptor = {
  id: string;
  name: string;
  manufacturer: string | null;
  deviceType: string;
  interfaceType: string;
  isDefault: boolean;
  configurations: ProjectStreamConfig[];
};

export type AudioScanWarning = {
  deviceId: string | null;
  direction: "input" | "output" | null;
  message: string;
};

export type AudioDeviceInventory = {
  host: string;
  sampleRateHz: number;
  inputs: AudioDeviceDescriptor[];
  outputs: AudioDeviceDescriptor[];
  warnings: AudioScanWarning[];
};

export type AudioDeviceChoice = {
  value: string;
  deviceId: string;
  name: string;
  isDefault: boolean;
  configuration: ProjectStreamConfig;
  inputChannelIndex: number;
};

export type AudioScanState = "idle" | "scanning" | "ready" | "error";

export type AudioScanError =
  | { kind: "native_shell_required" }
  | { kind: "runtime"; detail: string }
  | null;

/**
 * Build selectable PCM choices. Output measurement requires at least L/R;
 * capture requires at least one channel. No stream is opened by this helper.
 */
export function buildDeviceChoices(
  devices: AudioDeviceDescriptor[],
  minimumChannels: number,
): AudioDeviceChoice[] {
  const choices = devices.flatMap((device) =>
    device.configurations
      .filter(
        (configuration) =>
          configuration.sampleRateHz === 48_000 &&
          configuration.channels >= minimumChannels &&
          !configuration.sampleFormat.toLowerCase().startsWith("dsd"),
      )
      .map((configuration) => ({
        value: JSON.stringify([
          device.id,
          configuration.channels,
          configuration.sampleFormat,
          configuration.minimumBufferSizeFrames,
          configuration.maximumBufferSizeFrames,
        ]),
        deviceId: device.id,
        name: device.name,
        isDefault: device.isDefault,
        configuration,
        inputChannelIndex: 0,
      })),
  );

  return choices.sort((left, right) => {
    if (left.isDefault !== right.isDefault) {
      return left.isDefault ? -1 : 1;
    }
    const leftExact = left.configuration.channels === minimumChannels;
    const rightExact = right.configuration.channels === minimumChannels;
    if (leftExact !== rightExact) {
      return leftExact ? -1 : 1;
    }
    const nameOrder = left.name.localeCompare(right.name);
    if (nameOrder !== 0) return nameOrder;
    const channelOrder =
      left.configuration.channels - right.configuration.channels;
    if (channelOrder !== 0) return channelOrder;
    return sampleFormatRank(left.configuration.sampleFormat) -
      sampleFormatRank(right.configuration.sampleFormat);
  });
}

/**
 * Build one explicit selectable microphone channel per device.
 *
 * CoreAudio may expose a physically mono USB microphone through a two-channel
 * PCM stream. The capture backend must therefore open that native stream and
 * extract one user-selected channel instead of requiring a one-channel stream.
 */
export function buildInputDeviceChoices(
  devices: AudioDeviceDescriptor[],
): AudioDeviceChoice[] {
  const choices = buildDeviceChoices(devices, 1);
  const byDeviceAndChannel = new Map<string, AudioDeviceChoice>();

  for (const choice of choices) {
    for (
      let inputChannelIndex = 0;
      inputChannelIndex < choice.configuration.channels;
      inputChannelIndex += 1
    ) {
      const key = JSON.stringify([choice.deviceId, inputChannelIndex]);
      if (byDeviceAndChannel.has(key)) continue;
      byDeviceAndChannel.set(key, {
        ...choice,
        inputChannelIndex,
        value: JSON.stringify([
          choice.deviceId,
          choice.configuration.channels,
          choice.configuration.sampleFormat,
          choice.configuration.minimumBufferSizeFrames,
          choice.configuration.maximumBufferSizeFrames,
          inputChannelIndex,
        ]),
      });
    }
  }

  return [...byDeviceAndChannel.values()];
}

function sampleFormatRank(sampleFormat: string): number {
  const preference = ["f32", "f64", "i32", "i24", "i16"];
  const rank = preference.indexOf(sampleFormat.toLowerCase());
  return rank === -1 ? preference.length : rank;
}
