import { describe, expect, it } from "vitest";
import {
  buildDeviceChoices,
  buildInputDeviceChoices,
  type AudioDeviceDescriptor,
} from "./audio";

const device = (
  id: string,
  name: string,
  isDefault: boolean,
  configurations: AudioDeviceDescriptor["configurations"],
): AudioDeviceDescriptor => ({
  id,
  name,
  manufacturer: null,
  deviceType: "unknown",
  interfaceType: "unknown",
  isDefault,
  configurations,
});

describe("audio device choices", () => {
  it("admits only 48 kHz PCM configurations with enough channels", () => {
    const choices = buildDeviceChoices(
      [
        device("coreaudio:out", "Output", true, [
          {
            sampleRateHz: 48_000,
            channels: 2,
            sampleFormat: "f32",
            minimumBufferSizeFrames: 64,
            maximumBufferSizeFrames: 512,
          },
          {
            sampleRateHz: 44_100,
            channels: 2,
            sampleFormat: "f32",
            minimumBufferSizeFrames: null,
            maximumBufferSizeFrames: null,
          },
          {
            sampleRateHz: 48_000,
            channels: 1,
            sampleFormat: "i16",
            minimumBufferSizeFrames: null,
            maximumBufferSizeFrames: null,
          },
          {
            sampleRateHz: 48_000,
            channels: 2,
            sampleFormat: "dsd_u8",
            minimumBufferSizeFrames: null,
            maximumBufferSizeFrames: null,
          },
        ]),
      ],
      2,
    );

    expect(choices).toHaveLength(1);
    expect(choices[0].configuration.sampleFormat).toBe("f32");
  });

  it("puts the default exact-channel choice first", () => {
    const configuration = (channels: number) => ({
      sampleRateHz: 48_000,
      channels,
      sampleFormat: "f32",
      minimumBufferSizeFrames: null,
      maximumBufferSizeFrames: null,
    });
    const choices = buildDeviceChoices(
      [
        device("coreaudio:a", "Alpha", false, [configuration(2)]),
        device("coreaudio:z", "Zulu", true, [
          configuration(4),
          configuration(2),
        ]),
      ],
      2,
    );

    expect(choices[0].deviceId).toBe("coreaudio:z");
    expect(choices[0].configuration.channels).toBe(2);
  });

  it("exposes each native input channel for a multichannel CoreAudio microphone", () => {
    const choices = buildInputDeviceChoices([
      device("coreaudio:umik", "UMIK-1", true, [
        {
          sampleRateHz: 48_000,
          channels: 2,
          sampleFormat: "f32",
          minimumBufferSizeFrames: 64,
          maximumBufferSizeFrames: 512,
        },
      ]),
    ]);

    expect(choices).toHaveLength(2);
    expect(choices.map((choice) => choice.inputChannelIndex)).toEqual([0, 1]);
    expect(JSON.parse(choices[1].value)[5]).toBe(1);
  });
});
