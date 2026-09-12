import { useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import type { HotspotRow } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import Sparkline from "../components/Sparkline";
import { useHotspots, useTimeseries } from "../hooks/useBehavioral";
import { useScopes } from "../hooks/useScopes";
import { readerUrl } from "../lib/breadcrumbs";
import { formatAttentionScore } from "../lib/attentionRamp";
import { speedFilterItems } from "../lib/speedSearch";
import "../styles/hotspots.css";
import "../styles/browser.css"; // sparkline styles
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

const LIMIT_OPTIONS = [25, 50, 100, 250, 500] as const;

function termsTitle(row: HotspotRow): string {
  const t = row.hotspot.terms;
  const parts = [
    `churn_rank ${t.churn_rank}`,
    `complexity_rank ${t.complexity_rank}`,
  ];
  if (t.pain != null) parts.push(`pain ${t.pain.toFixed(2)}`);
  return parts.join(" · ");
}

const SPARK_WEEKS = 26;

/** Lazy path sparkline — fetches only while the score expander is open. */
function HotspotSpark({
  repo,
  path,
  enabled,
}: {
  repo: string;
  path: string;
  enabled: boolean;
}) {
  const ts = useTimeseries(
    enabled ? { repo, path, weeks: SPARK_WEEKS } : undefined,
    enabled,
  );
  if (!enabled) return null;
  if (ts.isLoading) {
    return (
      <span className="kbc-hotspots__spark-muted" data-kbc-hotspots-spark-loading>
        …
      </span>
    );
  }
  if (ts.error) {
    return (
      <span className="kbc-hotspots__spark-muted" title={(ts.error as Error).message}>
        spark n/a
      </span>
    );
  }
  const buckets = ts.data?.buckets ?? [];
  return (
    <div className="kbc-hotspots__spark-wrap" data-kbc-hotspots-spark={path}>
      <Sparkline
        buckets={buckets}
        width={88}
        height={20}
        label={`weekly churn for ${path}`}
        data-kbc-sparkline={path}
      />
      {ts.data?.truncated && (
        <span className="kbc-hotspots__spark-trunc" title={ts.data.note}>
          truncated
        </span>
      )}
    </div>
  );
}

/// V3.2-B3 — `/r/:repo/~hotspots`: ranked attention table (not quality).
export default function Hotspots() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const [limit, setLimit] = useState<number>(50);
  const [scope, setScope] = useState<string>("");
  const [filter, setFilter] = useState("");
  const [expanded, setExpanded] = useState<string | null>(null);

  const scopesQ = useScopes();
  const scopeNames = useMemo(
    () => Object.keys(scopesQ.data?.scopes ?? {}).sort(),
    [scopesQ.data],
  );

  const hotspots = useHotspots({
    repo,
    limit,
    scope: scope || undefined,
  });

  const filtered = useMemo(() => {
    const items = hotspots.data?.items ?? [];
    // Server already sorts by score desc; keep that order after speed-filter.
    if (filter.trim() === "") return items;
    return speedFilterItems(items, filter, (it) => it.path).map((h) => h.item);
  }, [hotspots.data, filter]);

  return (
    <div className="kbc-hotspots" id="main" data-kbc-hotspots>
      <header className="kbc-hotspots__head">
        <h1 className="kbc-hotspots__title">Hotspots — {repo}</h1>
        <p className="kbc-hotspots__hint" data-kbc-hotspots-hint>
          Ranks where to look first from change history — attention signals, not
          code quality.
        </p>
      </header>

      {hotspots.data?.truncated && (
        <div className="kbc-hotspots__trunc" role="status">
          Showing {hotspots.data.items.length} of {hotspots.data.total} — truncated
          by limit.
        </div>
      )}

      <div className="kbc-hotspots__filters">
        <input
          className="kbc-hotspots__search"
          type="search"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter paths…"
          aria-label="filter hotspot paths"
          data-kbc-hotspots-filter
        />
        <label className="kbc-hotspots__limit-lab">
          Limit
          <select
            className="kbc-hotspots__limit"
            value={limit}
            onChange={(e) => setLimit(Number(e.target.value))}
            aria-label="hotspots limit"
            data-kbc-hotspots-limit
          >
            {LIMIT_OPTIONS.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </label>
        {scopeNames.length > 0 && (
          <select
            className="kbc-hotspots__scope"
            value={scope}
            onChange={(e) => setScope(e.target.value)}
            aria-label="scope filter"
            data-kbc-hotspots-scope
          >
            <option value="">All scopes</option>
            {scopeNames.map((name) => (
              <option key={`inc-${name}`} value={name}>
                include: {name}
              </option>
            ))}
            {scopeNames.map((name) => (
              <option key={`exc-${name}`} value={`!${name}`}>
                exclude: {name}
              </option>
            ))}
          </select>
        )}
      </div>

      {hotspots.isLoading && <div className="kbc-hotspots__muted">Loading…</div>}
      {hotspots.error && (
        <div className="kbc-hotspots__error">{(hotspots.error as Error).message}</div>
      )}
      {!hotspots.isLoading && !hotspots.error && filtered.length === 0 && (
        <EmptyState
          icon={<Icon.List />}
          title="No hotspot rows"
          hint={
            filter || scope
              ? "Nothing matches the current filters."
              : "No behavioral counters yet — run a backfill, or open after history is indexed."
          }
        />
      )}

      {filtered.length > 0 && (
        <div className="kbc-hotspots__table-wrap">
          <table className="kbc-hotspots__table" data-kbc-hotspots-table>
            <thead>
              <tr>
                <th>Path</th>
                <th>Revisions</th>
                <th>Churn</th>
                <th>Complexity</th>
                <th>Age</th>
                <th>Score</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((row) => {
                const open = expanded === row.path;
                return (
                  <tr
                    key={row.path}
                    className="kbc-hotspots__row"
                    data-kbc-hotspots-row={row.path}
                  >
                    <td>
                      <Link
                        to={readerUrl(repo, row.path)}
                        className="kbc-hotspots__path"
                        data-kbc-hotspots-path
                      >
                        {row.path}
                      </Link>
                    </td>
                    <td data-kbc-hotspots-revisions>{row.revisions}</td>
                    <td data-kbc-hotspots-churn>{row.churn}</td>
                    <td data-kbc-hotspots-complexity title="loc + indent_sum">
                      {row.complexity.loc}
                      <span className="kbc-hotspots__dim">
                        {" "}
                        / {row.complexity.indent_sum}
                      </span>
                    </td>
                    <td data-kbc-hotspots-age>
                      {row.age_days != null ? `${Math.round(row.age_days)}d` : "—"}
                    </td>
                    <td>
                      <button
                        type="button"
                        className={
                          "kbc-hotspots__score" + (open ? " is-open" : "")
                        }
                        onClick={() =>
                          setExpanded((cur) => (cur === row.path ? null : row.path))
                        }
                        title={termsTitle(row)}
                        aria-expanded={open}
                        data-kbc-hotspots-score
                      >
                        {formatAttentionScore(row.hotspot.score)}
                        <span className="kbc-hotspots__score-chev" aria-hidden>
                          <Icon.Chevron className={open ? "kbc-twisty is-open" : "kbc-twisty"} />
                        </span>
                      </button>
                      {open && (
                        <>
                          <dl className="kbc-hotspots__terms" data-kbc-hotspots-terms>
                            <div>
                              <dt>churn_rank</dt>
                              <dd>{row.hotspot.terms.churn_rank}</dd>
                            </div>
                            <div>
                              <dt>complexity_rank</dt>
                              <dd>{row.hotspot.terms.complexity_rank}</dd>
                            </div>
                            {row.hotspot.terms.pain != null ? (
                              <div>
                                <dt>pain</dt>
                                <dd>{row.hotspot.terms.pain.toFixed(3)}</dd>
                              </div>
                            ) : null}
                          </dl>
                          {/* V3.4-C3 — weekly churn sparkline, lazy on expander open only. */}
                          <HotspotSpark repo={repo} path={row.path} enabled={open} />
                        </>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
