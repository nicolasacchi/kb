import { useEffect, useState } from "react";
import {
  fetchCrossStats,
  type CrossStats,
  type KbStats,
} from "../../api/client";
import { useMetrics } from "../../hooks/useMetrics";

// Overview tab. Fleet-level KPI tiles at the top from /api/stats
// (doc + open-errors totals) and live signals from `metrics.tick`
// (req/s, queue depth). Per-kb cards below; each card surfaces
// doc_count / open_errors / last-index age / reconcile staleness,
// and is `aria-label`'d so the next phase's "click to filter Pipeline
// to this kb" interaction has a stable hook.
export default function Overview() {
  const [stats, setStats] = useState<CrossStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const metrics = useMetrics(true);

  // Re-render the relative ages once a minute without re-fetching
  // /api/stats. SSE `index.complete` would be a cleaner hook for
  // freshness but a 60s tick keeps Overview honest cheaply.
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((n) => n + 1), 60_000);
    return () => clearInterval(id);
  }, []);

  useEffect(() => {
    let cancelled = false;
    const ctrl = new AbortController();
    const load = () =>
      fetchCrossStats(ctrl.signal)
        .then((s) => {
          if (!cancelled) {
            setStats(s);
            setError(null);
          }
        })
        .catch((e) => {
          if (!cancelled && e?.name !== "AbortError") {
            setError(String(e?.message ?? e));
          }
        });
    load();
    const id = setInterval(load, 30_000);
    return () => {
      cancelled = true;
      ctrl.abort();
      clearInterval(id);
    };
  }, []);

  // useMetrics already aggregates across daemons; v0.1 single-daemon
  // setups just see the one daemon's snapshot. `null` until the first
  // `metrics.tick` arrives (≤1s after mount).
  const fleet = metrics.lastTickAt
    ? {
        reqs: metrics.requestsLastSec,
        depth: metrics.storageDepth,
        cap: metrics.storageCapacity,
      }
    : null;

  return (
    <div className="dash">
      <div className="dash__kpis">
        <Kpi
          label="documents"
          value={stats?.total_docs ?? "—"}
          hint={stats ? `${stats.kbs.length} kb${stats.kbs.length === 1 ? "" : "s"}` : ""}
        />
        <Kpi
          label="open errors"
          value={stats?.total_open_errors ?? "—"}
          tone={(stats?.total_open_errors ?? 0) > 0 ? "warn" : "ok"}
        />
        <Kpi
          label="req / sec"
          value={fleet ? fleet.reqs.toFixed(0) : "—"}
          hint="fleet-wide, 1Hz"
        />
        <Kpi
          label="storage queue"
          value={fleet ? `${fleet.depth} / ${fleet.cap}` : "—"}
          tone={
            fleet && fleet.cap > 0
              ? fleet.depth / fleet.cap > 0.5
                ? "warn"
                : fleet.depth / fleet.cap > 0.9
                  ? "err"
                  : "ok"
              : undefined
          }
        />
      </div>

      {error && <div className="settings__error">stats unreachable: {error}</div>}

      <section aria-label="kb cards">
        <h3 className="settings__h3">kbs on this daemon</h3>
        <div className="dash__cards">
          {stats?.kbs.map((k) => (
            <KbCard key={k.name} kb={k} />
          ))}
          {stats && stats.kbs.length === 0 && (
            <div className="settings__hint">no kbs configured</div>
          )}
        </div>
      </section>
    </div>
  );
}

function Kpi({
  label,
  value,
  hint,
  tone,
}: {
  label: string;
  value: number | string;
  hint?: string;
  tone?: "ok" | "warn" | "err";
}) {
  const toneCls = tone ? `dash__kpi--${tone}` : "";
  return (
    <div className={`dash__kpi ${toneCls}`}>
      <div className="dash__kpi-val">{value}</div>
      <div className="dash__kpi-label">{label}</div>
      {hint && <div className="dash__kpi-hint">{hint}</div>}
    </div>
  );
}

function KbCard({ kb }: { kb: KbStats }) {
  const indexAge = relAge(kb.last_index_at);
  const reconcileAge = relAge(kb.last_reconcile_at);
  // "Stale" if no reconcile pass for >2× the configured interval — same
  // heuristic the TUI uses to colour the reconcile column.
  const stale =
    kb.reconcile_secs > 0 &&
    kb.last_reconcile_at != null &&
    Date.now() / 1000 - kb.last_reconcile_at > kb.reconcile_secs * 2;
  return (
    <article className="dash__card" aria-label={`kb ${kb.name}`}>
      <header className="dash__card-head">
        <h4 className="dash__card-name">{kb.name}</h4>
        {kb.open_errors > 0 && (
          <span className="dash__card-err" title="open errors">
            {kb.open_errors}
          </span>
        )}
      </header>
      <dl className="dash__card-grid">
        <dt>docs</dt>
        <dd>{kb.doc_count}</dd>
        <dt>indexed</dt>
        <dd>{indexAge}</dd>
        <dt>reconcile</dt>
        <dd className={stale ? "is-warn" : ""}>
          {kb.reconcile_secs > 0 ? reconcileAge : "off"}
        </dd>
      </dl>
    </article>
  );
}

function relAge(unix: number | null | undefined): string {
  if (unix == null) return "never";
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unix));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
