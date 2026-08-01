import { useId } from "react";
import type { Messages } from "../i18n";
import {
  formatMetric,
  type LiveDesignSummary,
  type LiveExportSummary,
  type LiveSecsDesignSummary,
  type LiveVerificationSummary,
} from "../lib/liveMeasurement";
import { MeasuredFrequencyResponseChart } from "./MeasuredFrequencyResponseChart";

type Props = {
  copy: Messages["liveMeasurement"];
  chartCopy: Messages["chart"];
  /** Phase 4 design; null when the SECS advanced option produced the trial. */
  design: LiveDesignSummary | null;
  /** SECS design; null on the Phase 4 path. */
  secsDesign?: LiveSecsDesignSummary | null;
  verification: LiveVerificationSummary;
  exported: LiveExportSummary | null;
};

function changeSummary(
  raw: number,
  verified: number,
  copy: Messages["liveMeasurement"]["resultAnalysis"],
): string {
  if (
    !Number.isFinite(raw) ||
    !Number.isFinite(verified) ||
    raw <= Number.EPSILON
  ) {
    return "—";
  }
  const percentage = (Math.abs(raw - verified) / raw) * 100;
  const template = verified <= raw ? copy.improvement : copy.regression;
  return template.replace("{percent}", percentage.toFixed(1));
}

export function FinalMeasurementResults({
  copy,
  chartCopy,
  design,
  secsDesign = null,
  verification,
  exported,
}: Props) {
  const headingId = useId();
  const resultCopy = copy.resultAnalysis;
  const positionCount = design?.positionCount ?? secsDesign?.positionCount ?? 0;

  return (
    <section className="final-measurement-results" aria-labelledby={headingId}>
      <header className="final-measurement-results__header">
        <div>
          <p className="eyebrow">{resultCopy.eyebrow}</p>
          <h3 id={headingId}>{resultCopy.title}</h3>
          <p>{resultCopy.body}</p>
        </div>
        <span>
          <i aria-hidden="true">✓</i>
          {resultCopy.measuredBadge}
        </span>
      </header>

      <section
        className="final-result-metrics"
        aria-labelledby={`${headingId}-metrics`}
      >
        <h4 id={`${headingId}-metrics`}>{resultCopy.keyMetrics}</h4>
        <div className="final-result-metrics__grid">
          <article>
            <span>L</span>
            <div>
              <strong>{resultCopy.leftResponse}</strong>
              <p>
                {formatMetric(verification.leftRawRmseDb, "dB", 3)}
                <i aria-hidden="true">→</i>
                {formatMetric(verification.leftVerifiedRmseDb, "dB", 3)}
              </p>
              <small>
                {changeSummary(
                  verification.leftRawRmseDb,
                  verification.leftVerifiedRmseDb,
                  resultCopy,
                )}
              </small>
            </div>
          </article>
          <article>
            <span>R</span>
            <div>
              <strong>{resultCopy.rightResponse}</strong>
              <p>
                {formatMetric(verification.rightRawRmseDb, "dB", 3)}
                <i aria-hidden="true">→</i>
                {formatMetric(verification.rightVerifiedRmseDb, "dB", 3)}
              </p>
              <small>
                {changeSummary(
                  verification.rightRawRmseDb,
                  verification.rightVerifiedRmseDb,
                  resultCopy,
                )}
              </small>
            </div>
          </article>
          <article>
            <span aria-hidden="true">≈</span>
            <div>
              <strong>{resultCopy.predictionAgreement}</strong>
              <p>
                L {formatMetric(verification.leftPredictedVerifiedRmseDb, "dB", 3)}
                <i aria-hidden="true">·</i>
                R {formatMetric(verification.rightPredictedVerifiedRmseDb, "dB", 3)}
              </p>
              <small>
                ≤{" "}
                {formatMetric(
                  verification.maximumAllowedPredictedVerifiedRmseDb,
                  "dB",
                  1,
                )}
              </small>
            </div>
          </article>
          <article>
            <span aria-hidden="true">◇</span>
            <div>
              <strong>{resultCopy.filterSafety}</strong>
              {design ? (
                <>
                  <p>
                    −{formatMetric(design.maximumAttenuationDb, "dB", 2)}
                    <i aria-hidden="true">/</i>+
                    {formatMetric(design.maximumBoostDb, "dB", 2)}
                  </p>
                  <small>
                    {design.protectedDipsPassed
                      ? resultCopy.protectedDipsPassed
                      : resultCopy.protectedDipsFailed}
                  </small>
                </>
              ) : (
                <>
                  <p>
                    +{formatMetric(secsDesign?.settings.maxBoostDb ?? 0, "dB", 1)}
                    <i aria-hidden="true">/</i>
                    {formatMetric(secsDesign?.preampDb ?? 0, "dB", 2)}
                  </p>
                  <small>{resultCopy.secsFilterNote}</small>
                </>
              )}
            </div>
          </article>
          <article>
            <span aria-hidden="true">P</span>
            <div>
              <strong>{copy.positionsUsed}</strong>
              <p>{positionCount}</p>
              <small>
                {resultCopy.measuredPositions.replace(
                  "{count}",
                  positionCount.toString(),
                )}
              </small>
            </div>
          </article>
          <article>
            <span aria-hidden="true">dB</span>
            <div>
              <strong>{copy.headroom}</strong>
              <p>
                {exported
                  ? formatMetric(exported.recommendedHeadroomDb, "dB", 1)
                  : "—"}
              </p>
              <small>
                {exported
                  ? `${copy.nativeRates}: ${exported.nativeRateCount}`
                  : resultCopy.exportPending}
              </small>
            </div>
          </article>
        </div>
        <p className="final-result-metrics__note">{resultCopy.evidenceNote}</p>
      </section>

      <MeasuredFrequencyResponseChart
        copy={chartCopy}
        data={verification.frequencyResponse}
      />
    </section>
  );
}
