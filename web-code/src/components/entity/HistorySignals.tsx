import { useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import Sparkline from "../Sparkline";
import {
  useAge,
  useCoupling,
  useHotspotsMap,
  useOwnership,
  useTimeseries,
} from "../../hooks/useBehavioral";
import { formatAttentionScore } from "../../lib/attentionRamp";
import { readerUrl } from "../../lib/breadcrumbs";
import "../../styles/browser.css"; // sparkline styles

const COUPLING_HINT_KEY = "kbc:coupling-explainer-seen";
const SPARK_WEEKS = 26;

export interface HistorySignalsProps {
  repo: string;
  path: string;
  /** Only fetch while the Entity tab is visible. */
  visible: boolean;
}

/**
 * V3.2-B3 — Entity-rail "History signals" for the CURRENT FILE.
 * Lazy: queries enabled only when `visible`. Attention copy only.
 * V3.4-C3 adds a weekly-churn sparkline + timeseries note/truncated.
 */
export default function HistorySignals({ repo, path, visible }: HistorySignalsProps) {
  const hotMap = useHotspotsMap(repo, visible);
  const ownership = useOwnership(repo, path, visible);
  const age = useAge(repo, path, visible);
  const coupling = useCoupling(repo, path, visible);
  const timeseries = useTimeseries(
    visible ? { repo, path, weeks: SPARK_WEEKS } : undefined,
    visible,
  );

  const hot = hotMap.data?.get(path);
  const [showCouplingHint, setShowCouplingHint] = useState(false);
  const hintShown = useRef(false);

  useEffect(() => {
    if (hintShown.current) return;
    if (!visible || !coupling.data || coupling.data.partners.length === 0) return;
    try {
      if (sessionStorage.getItem(COUPLING_HINT_KEY) === "1") return;
      sessionStorage.setItem(COUPLING_HINT_KEY, "1");
    } catch {
      // sessionStorage may be denied — still show once this mount.
    }
    hintShown.current = true;
    setShowCouplingHint(true);
  }, [visible, coupling.data]);

  const topAuthor = ownership.data?.authors?.[0];
  const partners = (coupling.data?.partners ?? []).slice(0, 3);
  const buckets = age.data?.buckets ?? [];
  const bucketTotal = buckets.reduce((s, b) => s + b.lines, 0) || 1;

  const agentShare = ownership.data?.agent_share;
  const hasAgentShare = agentShare != null && Number.isFinite(agentShare);

  return (
    <section className="kbc-entity__history" data-kbc-entity-history>
      <h3 className="kbc-entity__history-title">History signals</h3>
      <p className="kbc-entity__history-note">
        Attention from change history — look here first, not a quality grade.
      </p>

      <div className="kbc-entity__hist-block" data-kbc-entity-hotspot>
        <span className="kbc-entity__lab">hotspot</span>
        {hotMap.isFetching && !hotMap.data ? (
          <span className="kbc-entity__muted">…</span>
        ) : hot ? (
          <details className="kbc-entity__score-details">
            <summary data-kbc-entity-hotspot-score>
              {formatAttentionScore(hot.hotspot.score)}
            </summary>
            <dl className="kbc-entity__terms">
              <div>
                <dt>churn_rank</dt>
                <dd>{hot.hotspot.terms.churn_rank}</dd>
              </div>
              <div>
                <dt>complexity_rank</dt>
                <dd>{hot.hotspot.terms.complexity_rank}</dd>
              </div>
              {hot.hotspot.terms.pain != null && (
                <div>
                  <dt>pain</dt>
                  <dd>{hot.hotspot.terms.pain.toFixed(3)}</dd>
                </div>
              )}
            </dl>
          </details>
        ) : (
          <span className="kbc-entity__muted" title="no behavioral row for this path">
            —
          </span>
        )}
      </div>

      <div className="kbc-entity__hist-block" data-kbc-entity-timeseries>
        <span className="kbc-entity__lab">weekly churn</span>
        {timeseries.isFetching && !timeseries.data ? (
          <span className="kbc-entity__muted">…</span>
        ) : timeseries.error ? (
          <span className="kbc-entity__muted" title={(timeseries.error as Error).message}>
            —
          </span>
        ) : (
          <div className="kbc-entity__spark-col">
            <Sparkline
              buckets={timeseries.data?.buckets ?? []}
              width={96}
              height={20}
              label={`weekly churn for ${path}`}
              data-kbc-sparkline={path}
            />
            {timeseries.data?.note && (
              <p className="kbc-entity__ts-note" data-kbc-entity-ts-note>
                {timeseries.data.note}
              </p>
            )}
            {timeseries.data?.truncated && (
              <span className="kbc-entity__ts-trunc" data-kbc-entity-ts-truncated>
                truncated
              </span>
            )}
          </div>
        )}
      </div>

      <div className="kbc-entity__hist-block" data-kbc-entity-ownership>
        <span className="kbc-entity__lab">ownership</span>
        {ownership.isFetching && !ownership.data ? (
          <span className="kbc-entity__muted">…</span>
        ) : ownership.data && topAuthor ? (
          <span>
            {topAuthor.author}{" "}
            <span className="kbc-entity__muted">
              ({(topAuthor.share * 100).toFixed(0)}%)
            </span>
            {" · frag "}
            {ownership.data.fragmentation.toFixed(2)}
            {hasAgentShare && (
              <>
                {" · agent "}
                {(agentShare! * 100).toFixed(0)}%
              </>
            )}
          </span>
        ) : (
          <span className="kbc-entity__muted">—</span>
        )}
      </div>

      <div className="kbc-entity__hist-block" data-kbc-entity-age>
        <span className="kbc-entity__lab">age</span>
        {age.isFetching && !age.data ? (
          <span className="kbc-entity__muted">…</span>
        ) : buckets.length > 0 ? (
          <div
            className="kbc-entity__age-bar"
            title={buckets.map((b) => `${b.label}: ${b.lines}`).join(" · ")}
            data-kbc-entity-age-bar
          >
            {buckets.map((b) => (
              <span
                key={b.label}
                className="kbc-entity__age-seg"
                style={{ flex: Math.max(b.lines, 0) / bucketTotal }}
                title={`${b.label}: ${b.lines} lines`}
              />
            ))}
          </div>
        ) : (
          <span className="kbc-entity__muted">—</span>
        )}
      </div>

      <div className="kbc-entity__hist-block" data-kbc-entity-coupling>
        <span className="kbc-entity__lab">changes with</span>
        {coupling.isFetching && !coupling.data ? (
          <span className="kbc-entity__muted">…</span>
        ) : partners.length > 0 ? (
          <ul className="kbc-entity__partners">
            {showCouplingHint && (
              <li className="kbc-entity__partners-hint" data-kbc-entity-coupling-hint>
                Files that historically change together — a hint, not a rule.
              </li>
            )}
            {partners.map((p) => (
              <li key={p.path}>
                <Link
                  to={readerUrl(repo, p.path)}
                  className="kbc-entity__partner-link"
                  data-kbc-entity-partner={p.path}
                  title={`co_commits ${p.co_commits} · support ${p.support} · confidence ${p.confidence.toFixed(2)}`}
                >
                  {p.path}
                </Link>
                <span className="kbc-entity__muted">
                  {" "}
                  conf {(p.confidence * 100).toFixed(0)}%
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <span className="kbc-entity__muted">—</span>
        )}
      </div>
    </section>
  );
}
