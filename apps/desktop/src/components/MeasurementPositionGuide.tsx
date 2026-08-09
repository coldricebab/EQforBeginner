import type { Messages } from "../i18n/types";
import type { LiveBaselinePosition } from "../lib/liveMeasurement";

type Props = {
  copy: Messages["liveMeasurement"];
};

const SURROUND_POSITIONS: readonly LiveBaselinePosition[] = [
  "P1",
  "P2",
  "P3",
  "P4",
  "P5",
  "P6",
  "P7",
  "P8",
];

export function MeasurementPositionGuide({ copy }: Props) {
  return (
    <section
      className="measurement-position-guide"
      aria-labelledby="measurement-position-guide-title"
    >
      <header>
        <div>
          <h4 id="measurement-position-guide-title">
            {copy.positionGuide.title}
          </h4>
          <p>{copy.positionGuide.body}</p>
        </div>
        <span>{copy.positionGuide.microphone}</span>
      </header>

      <div className="measurement-position-guide__layout">
        <svg
          className="measurement-position-guide__diagram"
          viewBox="0 0 720 330"
          role="img"
          aria-label={copy.positionGuide.diagramLabel}
        >
          <defs>
            <marker
              id="position-guide-arrow"
              markerWidth="8"
              markerHeight="8"
              refX="6"
              refY="4"
              orient="auto"
            >
              <path d="M0,0 L8,4 L0,8 Z" />
            </marker>
          </defs>

          <g className="position-diagram__top">
            <text className="position-diagram__view-label" x="20" y="26">
              {copy.positionGuide.topView}
            </text>
            <rect className="position-diagram__speaker" x="90" y="45" width="58" height="28" rx="7" />
            <rect className="position-diagram__speaker" x="292" y="45" width="58" height="28" rx="7" />
            <text className="position-diagram__small-label" x="220" y="65" textAnchor="middle">
              {copy.positionGuide.speakers}
            </text>
            <line
              className="position-diagram__direction"
              x1="220"
              y1="78"
              x2="220"
              y2="108"
              markerEnd="url(#position-guide-arrow)"
            />
            <ellipse className="position-diagram__area" cx="220" cy="190" rx="150" ry="100" />
            <text className="position-diagram__area-label" x="220" y="301" textAnchor="middle">
              {copy.positionGuide.listeningArea}
            </text>
            {/* The box footprint: each corner is a column holding an upper
                (P1-P4) and a lower (P5-P8) vertex. */}
            <rect className="position-diagram__offset-rect" x="145" y="130" width="150" height="120" fill="none" />
            <line className="position-diagram__offset" x1="200" y1="173" x2="163" y2="141" />
            <line className="position-diagram__offset" x1="240" y1="173" x2="277" y2="141" />
            <line className="position-diagram__offset" x1="200" y1="207" x2="163" y2="239" />
            <line className="position-diagram__offset" x1="240" y1="207" x2="277" y2="239" />
            <PositionPoint id="P0" x={220} y={190} primary />
            <PositionPoint id="P1" label="P1·P5" x={145} y={130} />
            <PositionPoint id="P2" label="P2·P6" x={295} y={130} />
            <PositionPoint id="P3" label="P3·P7" x={295} y={250} />
            <PositionPoint id="P4" label="P4·P8" x={145} y={250} />
          </g>

          <line className="position-diagram__divider" x1="440" y1="24" x2="440" y2="306" />

          <g className="position-diagram__side">
            <text className="position-diagram__view-label" x="474" y="26">
              {copy.positionGuide.sideView}
            </text>
            <line className="position-diagram__floor" x1="480" y1="280" x2="700" y2="280" />
            <text className="position-diagram__small-label" x="690" y="300" textAnchor="end">
              {copy.positionGuide.floor}
            </text>
            <rect className="position-diagram__seat" x="500" y="210" width="180" height="52" rx="14" />
            <line className="position-diagram__ear-line" x1="490" y1="185" x2="690" y2="185" />
            <text className="position-diagram__small-label" x="688" y="176" textAnchor="end">
              {copy.positionGuide.earHeight}
            </text>
            <line className="position-diagram__stand" x1="590" y1="185" x2="590" y2="264" />
            <line className="position-diagram__stand" x1="590" y1="145" x2="590" y2="185" />
            <PositionPoint id="P0" x={590} y={185} primary />
            <PositionPoint id="P5" label="P1–P4" x={590} y={125} />
            <PositionPoint id="P8" label="P5–P8" x={590} y={245} />
            <line
              className="position-diagram__height"
              x1="630"
              y1="176"
              x2="630"
              y2="134"
              markerEnd="url(#position-guide-arrow)"
            />
            <line
              className="position-diagram__height"
              x1="630"
              y1="194"
              x2="630"
              y2="236"
              markerEnd="url(#position-guide-arrow)"
            />
          </g>
        </svg>

        <ol className="measurement-position-guide__list">
          {SURROUND_POSITIONS.map((positionId) => (
            <li key={positionId}>
              <span>{positionId}</span>
              <p>{copy.positionDetails[positionId]}</p>
            </li>
          ))}
        </ol>
      </div>

      <p className="measurement-position-guide__note">
        <span aria-hidden="true">i</span>
        {copy.positionGuide.note}
      </p>
    </section>
  );
}

function PositionPoint({
  id,
  label,
  x,
  y,
  primary = false,
}: {
  id: string;
  /** Display text; a corner column shows both of its vertices ("P1·P5"). */
  label?: string;
  x: number;
  y: number;
  primary?: boolean;
}) {
  return (
    <g
      className={`position-diagram__point ${
        primary ? "position-diagram__point--primary" : ""
      }`}
      transform={`translate(${x} ${y})`}
      data-position={id}
    >
      <circle r="25" />
      <circle className="position-diagram__mic" cy="-9" r="5" />
      <text y="9" textAnchor="middle">{label ?? id}</text>
    </g>
  );
}
