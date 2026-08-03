import { useId, useMemo, useState } from "react";
import type { Messages } from "../i18n";
import {
  bandMedianDb,
  buildPath,
  DISPLAY_ALIGNMENT_HIGH_HZ,
  DISPLAY_ALIGNMENT_LOW_HZ,
  linearY,
  logX,
  type ChartBounds,
  type ChartPoint,
  type CurveKind,
} from "../lib/chart";
import type { LiveFrequencyResponsePlot } from "../lib/liveMeasurement";

type DisplayChannel = "left" | "right" | "average";

type Props = {
  copy: Messages["chart"];
  data: LiveFrequencyResponsePlot;
};

const CURVES: readonly CurveKind[] = [
  "raw",
  "target",
  "predicted",
  "verified",
];
const CHANNELS: readonly DisplayChannel[] = ["left", "right", "average"];
/**
 * Upper bound of the zoomed view, matching what its button says (20–500 Hz).
 * It cannot be derived from `correctionHighHz`: SECS reports the full 20 kHz
 * band as its correction range, which collapsed the zoomed view onto the
 * full-band one.
 */
const ZOOM_MAXIMUM_HZ = 500;

function frequencyLabel(frequencyHz: number): string {
  if (frequencyHz >= 1_000) {
    const khz = frequencyHz / 1_000;
    return Number.isInteger(khz) ? `${khz}k` : `${khz.toFixed(1)}k`;
  }
  return frequencyHz.toFixed(0);
}

function niceStep(rawStep: number): number {
  if (!Number.isFinite(rawStep) || rawStep <= 0) return 5;
  const magnitude = 10 ** Math.floor(Math.log10(rawStep));
  const normalized = rawStep / magnitude;
  const multiplier =
    normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return multiplier * magnitude;
}

function series(
  data: LiveFrequencyResponsePlot,
  curve: CurveKind,
  channel: DisplayChannel,
): number[] {
  const suffix =
    channel === "left" ? "LeftDb" : channel === "right" ? "RightDb" : "AverageDb";
  const key = `${curve}${suffix[0].toUpperCase()}${suffix.slice(1)}` as
    | "rawLeftDb"
    | "rawRightDb"
    | "rawAverageDb"
    | "targetLeftDb"
    | "targetRightDb"
    | "targetAverageDb"
    | "predictedLeftDb"
    | "predictedRightDb"
    | "predictedAverageDb"
    | "verifiedLeftDb"
    | "verifiedRightDb"
    | "verifiedAverageDb";
  return data[key];
}

function nearestLevel(
  frequenciesHz: readonly number[],
  levelsDb: readonly number[],
  frequencyHz: number,
): number | null {
  if (frequenciesHz.length !== levelsDb.length || frequenciesHz.length === 0) {
    return null;
  }
  let closestIndex = 0;
  let closestDistance = Math.abs(frequenciesHz[0] - frequencyHz);
  for (let index = 1; index < frequenciesHz.length; index += 1) {
    const distance = Math.abs(frequenciesHz[index] - frequencyHz);
    if (distance < closestDistance) {
      closestIndex = index;
      closestDistance = distance;
    }
  }
  const value = levelsDb[closestIndex];
  return Number.isFinite(value) ? value : null;
}

export function MeasuredFrequencyResponseChart({ copy, data }: Props) {
  const headingId = useId();
  const clipId = useId().replaceAll(":", "");
  const [fullBand, setFullBand] = useState(false);
  // Predicted-only plots (SECS design before any remeasurement) carry empty
  // verified curves; the verified toggle disappears rather than drawing an
  // empty trace that could be mistaken for a measured result.
  const hasVerified = data.verifiedLeftDb.length > 0;
  const availableCurves = hasVerified
    ? CURVES
    : CURVES.filter((curve) => curve !== "verified");
  const [visibleCurves, setVisibleCurves] = useState<Record<CurveKind, boolean>>({
    raw: true,
    target: true,
    predicted: true,
    verified: true,
  });
  const [visibleChannels, setVisibleChannels] = useState<
    Record<DisplayChannel, boolean>
  >({
    left: false,
    right: false,
    average: true,
  });

  const minimumFrequency = Math.max(
    data.correctionLowHz,
    data.frequenciesHz[0] ?? data.correctionLowHz,
  );
  const maximumFrequency = fullBand
    ? (data.frequenciesHz.at(-1) ?? data.taperEndHz)
    : Math.min(data.correctionHighHz, ZOOM_MAXIMUM_HZ);
  // Display-only level alignment. A mixed-phase SECS filter attenuates
  // broadly, so its predicted and verified traces sit well below the raw
  // measurement and the shapes become hard to compare even though the level
  // difference is just playback gain. Each curve gets ONE scalar offset,
  // taken from its own spatial-average trace over the shared 200-500 Hz
  // reference band, so curve shapes and L/R differences within a curve are
  // untouched. The applied offsets are printed under the chart - the level
  // difference is real, it is only moved out of the way, and the numerical
  // gates never see this.
  const alignmentOffsetsDb = useMemo(() => {
    const offsets: Record<CurveKind, number> = {
      raw: 0,
      target: 0,
      predicted: 0,
      verified: 0,
    };
    const reference = bandMedianDb(
      data.frequenciesHz,
      data.rawAverageDb,
      DISPLAY_ALIGNMENT_LOW_HZ,
      DISPLAY_ALIGNMENT_HIGH_HZ,
    );
    if (reference === null) return offsets;
    for (const curve of CURVES) {
      const median = bandMedianDb(
        data.frequenciesHz,
        series(data, curve, "average"),
        DISPLAY_ALIGNMENT_LOW_HZ,
        DISPLAY_ALIGNMENT_HIGH_HZ,
      );
      offsets[curve] = median === null ? 0 : reference - median;
    }
    return offsets;
  }, [data]);
  const activeSeries = useMemo(
    () =>
      availableCurves.flatMap((curve) =>
        CHANNELS.filter(
          (channel) => visibleCurves[curve] && visibleChannels[channel],
        ).map((channel) => ({
          curve,
          channel,
          values: series(data, curve, channel).map(
            (value) => value + alignmentOffsetsDb[curve],
          ),
        })),
      ),
    [alignmentOffsetsDb, availableCurves, data, visibleChannels, visibleCurves],
  );
  const visibleLevels = activeSeries.flatMap(({ values }) =>
    values.filter(
      (value, index) =>
        Number.isFinite(value) &&
        data.frequenciesHz[index] >= minimumFrequency &&
        data.frequenciesHz[index] <= maximumFrequency,
    ),
  );
  const rawMinimum = visibleLevels.length > 0 ? Math.min(...visibleLevels) : -10;
  const rawMaximum = visibleLevels.length > 0 ? Math.max(...visibleLevels) : 10;
  let minimumLevel = Math.floor((rawMinimum - 3) / 5) * 5;
  let maximumLevel = Math.ceil((rawMaximum + 3) / 5) * 5;
  if (maximumLevel - minimumLevel < 20) {
    const center = (maximumLevel + minimumLevel) / 2;
    minimumLevel = Math.floor((center - 10) / 5) * 5;
    maximumLevel = minimumLevel + 20;
  }

  const bounds: ChartBounds = {
    left: 66,
    right: 906,
    top: 24,
    bottom: 348,
    minFrequency: minimumFrequency,
    maxFrequency: maximumFrequency,
    minLevel: minimumLevel,
    maxLevel: maximumLevel,
  };
  const xTicks = (fullBand
    ? [20, 50, 100, 200, 500, 1_000, 2_000, 5_000, 10_000, 20_000]
    : [20, 30, 50, 80, 100, 200, 300, 500]
  ).filter(
    (frequency) =>
      frequency >= minimumFrequency && frequency <= maximumFrequency,
  );
  const yStep = niceStep((maximumLevel - minimumLevel) / 6);
  const firstYTick = Math.ceil(minimumLevel / yStep) * yStep;
  const yTicks: number[] = [];
  for (let value = firstYTick; value <= maximumLevel + 1e-9; value += yStep) {
    yTicks.push(value);
  }

  const chartPoints = (values: number[]): ChartPoint[] =>
    data.frequenciesHz.map((frequency, index) => ({
      frequency,
      level: values[index],
    }));
  const markerLevel = (frequencyHz: number) => {
    const level = nearestLevel(
      data.frequenciesHz,
      data.rawAverageDb,
      frequencyHz,
    );
    return level === null ? null : level + alignmentOffsetsDb.raw;
  };
  const alignmentSummary = availableCurves
    .filter((curve) => Math.abs(alignmentOffsetsDb[curve]) >= 0.05)
    .map(
      (curve) =>
        `${copy[curve]} ${alignmentOffsetsDb[curve] >= 0 ? "+" : ""}${alignmentOffsetsDb[curve].toFixed(1)} dB`,
    )
    .join(" · ");
  const smoothingDenominator = Math.round(
    1 / data.displaySmoothingFwhmOctaves,
  );
  const smoothingFraction = Number.isFinite(smoothingDenominator)
    ? `1/${smoothingDenominator}`
    : data.displaySmoothingFwhmOctaves.toFixed(3);

  return (
    <section className="chart-card measured-response-chart" aria-labelledby={headingId}>
      <div className="chart-card__header">
        <div>
          <p className="eyebrow">{data.algorithmVersion}</p>
          <h3 id={headingId}>{copy.heading}</h3>
          <p>{copy.subheading}</p>
          <small className="measured-response-chart__smoothing">
            {copy.displaySmoothing.replace("{fraction}", smoothingFraction)}
          </small>
        </div>
        <div className="segmented-control" aria-label={copy.xAxis}>
          <button
            className={!fullBand ? "is-active" : ""}
            type="button"
            aria-pressed={!fullBand}
            onClick={() => setFullBand(false)}
          >
            {copy.zoom}
          </button>
          <button
            className={fullBand ? "is-active" : ""}
            type="button"
            aria-pressed={fullBand}
            onClick={() => setFullBand(true)}
          >
            {copy.full}
          </button>
        </div>
      </div>

      <div className="chart-controls">
        <fieldset>
          <legend>{copy.typeLabel}</legend>
          <div className="toggle-row">
            {availableCurves.map((curve) => (
              <label className={`plot-toggle plot-toggle--${curve}`} key={curve}>
                <input
                  type="checkbox"
                  checked={visibleCurves[curve]}
                  onChange={(event) =>
                    setVisibleCurves((current) => ({
                      ...current,
                      [curve]: event.target.checked,
                    }))
                  }
                />
                <i className="plot-toggle__sample" aria-hidden="true" />
                {copy[curve]}
              </label>
            ))}
          </div>
        </fieldset>
        <fieldset>
          <legend>{copy.channelLabel}</legend>
          <div className="toggle-row">
            {CHANNELS.map((channel) => {
              const copyKey = channel === "average" ? "spatial" : channel;
              const className =
                channel === "average" ? "spatial" : channel;
              return (
                <label
                  className={`channel-toggle channel-toggle--${className}`}
                  key={channel}
                >
                  <input
                    type="checkbox"
                    checked={visibleChannels[channel]}
                    onChange={(event) =>
                      setVisibleChannels((current) => ({
                        ...current,
                        [channel]: event.target.checked,
                      }))
                    }
                  />
                  <i className="channel-toggle__sample" aria-hidden="true" />
                  {copy[copyKey]}
                </label>
              );
            })}
          </div>
        </fieldset>
      </div>

      <div className="plot-frame">
        <svg
          className="response-plot"
          viewBox="0 0 940 390"
          role="img"
          aria-label={copy.accessibleDescription}
        >
          <defs>
            <clipPath id={clipId}>
              <rect
                x={bounds.left}
                y={bounds.top}
                width={bounds.right - bounds.left}
                height={bounds.bottom - bounds.top}
              />
            </clipPath>
          </defs>
          <rect className="plot-background" width="940" height="390" />
          {!fullBand && (
            <rect
              className="plot-correction-band"
              x={logX(data.correctionLowHz, bounds)}
              y={bounds.top}
              width={
                logX(data.correctionHighHz, bounds) -
                logX(data.correctionLowHz, bounds)
              }
              height={bounds.bottom - bounds.top}
            />
          )}
          {fullBand && data.taperEndHz <= maximumFrequency && (
            <rect
              className="plot-taper-band"
              x={logX(data.correctionHighHz, bounds)}
              y={bounds.top}
              width={
                logX(data.taperEndHz, bounds) -
                logX(data.correctionHighHz, bounds)
              }
              height={bounds.bottom - bounds.top}
            />
          )}
          {xTicks.map((frequency) => {
            const x = logX(frequency, bounds);
            return (
              <g key={frequency}>
                <line
                  className="grid-line"
                  x1={x}
                  x2={x}
                  y1={bounds.top}
                  y2={bounds.bottom}
                />
                <text
                  className="axis-label"
                  x={x}
                  y={bounds.bottom + 20}
                  textAnchor="middle"
                >
                  {frequencyLabel(frequency)}
                </text>
              </g>
            );
          })}
          {yTicks.map((level) => {
            const y = linearY(level, bounds);
            return (
              <g key={level}>
                <line
                  className="grid-line"
                  x1={bounds.left}
                  x2={bounds.right}
                  y1={y}
                  y2={y}
                />
                <text
                  className="axis-label"
                  x={bounds.left - 10}
                  y={y + 3}
                  textAnchor="end"
                >
                  {level.toFixed(0)}
                </text>
              </g>
            );
          })}
          <g clipPath={`url(#${clipId})`}>
            {activeSeries.map(({ curve, channel, values }) => (
              <path
                className={`response-line trace--${curve} channel--${
                  channel === "average" ? "spatial" : channel
                }`}
                d={buildPath(chartPoints(values), bounds)}
                key={`${curve}-${channel}`}
              />
            ))}
            {data.protectedDipFrequenciesHz
              .filter(
                (frequency) =>
                  frequency >= minimumFrequency &&
                  frequency <= maximumFrequency,
              )
              .map((frequency) => {
                const level = markerLevel(frequency);
                if (level === null) return null;
                const x = logX(frequency, bounds);
                const y = linearY(level, bounds);
                return (
                  <path
                    className="marker marker--protected"
                    d={`M${x},${y - 7} L${x - 6},${y + 5} L${x + 6},${y + 5} Z`}
                    key={`protected-${frequency}`}
                  />
                );
              })}
            {data.correctedPeakFrequenciesHz
              .filter(
                (frequency) =>
                  frequency >= minimumFrequency &&
                  frequency <= maximumFrequency,
              )
              .map((frequency) => {
                const level = markerLevel(frequency);
                if (level === null) return null;
                return (
                  <circle
                    className="marker marker--corrected"
                    cx={logX(frequency, bounds)}
                    cy={linearY(level, bounds)}
                    r="5"
                    key={`corrected-${frequency}`}
                  />
                );
              })}
          </g>
          <text
            className="axis-title"
            x={(bounds.left + bounds.right) / 2}
            y="384"
            textAnchor="middle"
          >
            {copy.xAxis}
          </text>
          <text
            className="axis-title"
            x="15"
            y={(bounds.top + bounds.bottom) / 2}
            textAnchor="middle"
            transform={`rotate(-90 15 ${(bounds.top + bounds.bottom) / 2})`}
          >
            {copy.yAxis}
          </text>
        </svg>
      </div>
      <div className="marker-legend" aria-label={copy.markersLabel}>
        <span>
          <i className="legend-shape legend-shape--triangle" aria-hidden="true" />
          {copy.protectedDip}
        </span>
        <span>
          <i className="legend-shape legend-shape--circle" aria-hidden="true" />
          {copy.correctedPeak}
        </span>
      </div>
      <p className="measured-response-chart__scope">{copy.scopeNote}</p>
      {alignmentSummary !== "" && (
        <p className="measured-response-chart__scope">
          {copy.levelAlignment
            .replace("{low}", DISPLAY_ALIGNMENT_LOW_HZ.toFixed(0))
            .replace("{high}", DISPLAY_ALIGNMENT_HIGH_HZ.toFixed(0))
            .replace("{offsets}", alignmentSummary)}
        </p>
      )}
    </section>
  );
}
