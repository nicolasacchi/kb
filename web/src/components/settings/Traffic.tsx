import { useEffect, useRef, useState } from "react";
import { useMetrics } from "../../hooks/useMetrics";
import {
  fetchMetricsSnapshot,
  isAbortError,
  type MetricsLatency,
  type MetricsSnapshot,
} from "../../api/client";

const SPARK_WINDOW = 60;
// Poll cadence for the detailed snapshot. The metrics.tick SSE drives the
// always-on coarse KPIs above; the detailed layer has no SSE (read-state
// changes too often to cache), so we poll the queryable endpoint.
const DETAIL_POLL_MS = 2_000;

// Traffic tab. Per-route p50/p95 table from `metrics.tick` + two
// rolling sparklines (req/s and storage queue depth). Lifts the
// classification helpers (depthClass / p95Class) from StatusPill so
// the operator sees the same severity rules in both surfaces.
export default function Traffic() {
  const metrics = useMetrics(true);

  // 60-sample rolling windows for req/s + queue depth. Updated each
  // metrics.tick; the metrics.tick payload only carries the latest
  // 1s snapshot, so the SPA maintains the history.
  const reqRef = useRef<number[]>([]);
  const depthRef = useRef<number[]>([]);

  useEffect(() => {
    if (!metrics.lastTickAt) return;
    const reqs = reqRef.current;
    reqs.push(metrics.requestsLastSec);
    if (reqs.length > SPARK_WINDOW) reqs.splice(0, reqs.length - SPARK_WINDOW);
    const depth = depthRef.current;
    depth.push(metrics.storageDepth);
    if (depth.length > SPARK_WINDOW) depth.splice(0, depth.length - SPARK_WINDOW);
  }, [metrics.lastTickAt, metrics.requestsLastSec, metrics.storageDepth]);

  const stale =
    metrics.lastTickAt != null && Date.now() - metrics.lastTickAt > 5_000;

  return (
    <div className="dash">
      <div className="dash__kpis">
        <KpiSpark
          label="req / sec"
          value={metrics.requestsLastSec}
          samples={reqRef.current}
          hint={`${compact(metrics.requestsTotal)} total since boot`}
        />
        <KpiSpark
          label="storage queue"
          value={metrics.storageDepth}
          max={metrics.storageCapacity || 1024}
          samples={depthRef.current}
          tone={depthClass(metrics.storageDepth, metrics.storageCapacity)}
          hint={`capacity ${metrics.storageCapacity}`}
        />
      </div>

      {stale && (
        <div className="settings__hint">
          No <code>metrics.tick</code> in the last 5s — the daemon may be
          paused or unreachable.
        </div>
      )}

      <section aria-label="per-route latency">
        <h3 className="settings__h3">per-route latency</h3>
        {metrics.routes.length === 0 ? (
          <div className="settings__hint">
            waiting for the first <code>metrics.tick</code>…
          </div>
        ) : (
          <table className="dash__table">
            <thead>
              <tr>
                <th>route</th>
                <th>count</th>
                <th>p50</th>
                <th>p95</th>
              </tr>
            </thead>
            <tbody>
              {[...metrics.routes]
                .sort((a, b) => b.count - a.count)
                .map((r) => (
                  <tr key={r.kind}>
                    <td>
                      <code>{r.kind}</code>
                    </td>
                    <td>{compact(r.count)}</td>
                    <td>{r.count > 0 ? `${r.p50_ms}ms` : "—"}</td>
                    <td className={p95Class(r.p95_ms)}>
                      {r.count > 0 ? `${r.p95_ms}ms` : "—"}
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
        )}
      </section>

      <DetailedMetrics />
    </div>
  );
}

// TM-track — detailed metrics: search-stage / per-kb / pipeline timing.
// Polls GET /api/metrics every 2s. Renders the tables only when the daemon
// runs with `[server] metrics = true`; otherwise a hint to enable it.
function DetailedMetrics() {
  const [snap, setSnap] = useState<MetricsSnapshot | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const controller = new AbortController();
    const poll = async () => {
      try {
        const s = await fetchMetricsSnapshot(controller.signal);
        if (alive) {
          setSnap(s);
          setErr(null);
        }
      } catch (e) {
        if (!isAbortError(e) && alive) setErr((e as Error).message);
      }
    };
    poll();
    const id = window.setInterval(poll, DETAIL_POLL_MS);
    return () => {
      alive = false;
      controller.abort();
      window.clearInterval(id);
    };
  }, []);

  if (err) {
    return (
      <section aria-label="detailed metrics">
        <h3 className="settings__h3">detailed metrics</h3>
        <div className="settings__hint">couldn't load /api/metrics — {err}</div>
      </section>
    );
  }
  if (!snap) return null;
  if (!snap.detailed_enabled || !snap.detailed) {
    return (
      <section aria-label="detailed metrics">
        <h3 className="settings__h3">detailed metrics</h3>
        <div className="settings__hint">
          Detailed timing is off. Set <code>[server] metrics = true</code> in
          kb.toml (Settings → Config) to enable per-search-stage, per-kb, and
          ingest-pipeline timing.
        </div>
      </section>
    );
  }

  const { search_stages, per_kb, pipeline } = snap.detailed;
  return (
    <>
      <section aria-label="search stages">
        <h3 className="settings__h3">search stages</h3>
        <LatencyTable colLabel="stage" rows={search_stages} />
      </section>

      <section aria-label="per-kb request latency">
        <h3 className="settings__h3">per-kb request latency</h3>
        <LatencyTable colLabel="kb" rows={per_kb} />
      </section>

      <section aria-label="ingest pipeline">
        <h3 className="settings__h3">ingest pipeline</h3>
        <div className="settings__hint">
          indexer: {compact(pipeline.indexer.files_indexed)} files · p50{" "}
          {dur(pipeline.indexer.p50_ms)} · p95 {dur(pipeline.indexer.p95_ms)} ·
          p99 {dur(pipeline.indexer.p99_ms)}
          <br />
          embed: {compact(pipeline.embed_index.calls)} calls ·{" "}
          {compact(pipeline.embed_index.docs)} docs · p50{" "}
          {dur(pipeline.embed_index.p50_ms)} · p95{" "}
          {dur(pipeline.embed_index.p95_ms)} · p99{" "}
          {dur(pipeline.embed_index.p99_ms)}
        </div>
        <table className="dash__table">
          <thead>
            <tr>
              <th>storage op</th>
              <th>count</th>
              <th>handler p95</th>
              <th>queue p95</th>
            </tr>
          </thead>
          <tbody>
            {pipeline.storage
              .filter((s) => s.count > 0)
              .map((s) => (
                <tr key={s.kind}>
                  <td>
                    <code>{s.kind}</code>
                  </td>
                  <td>{compact(s.count)}</td>
                  <td className={p95Class(s.handler_p95_ms)}>
                    {dur(s.handler_p95_ms)}
                  </td>
                  <td className={p95Class(s.queue_wait_p95_ms)}>
                    {dur(s.queue_wait_p95_ms)}
                  </td>
                </tr>
              ))}
          </tbody>
        </table>
      </section>
    </>
  );
}

function LatencyTable({ colLabel, rows }: { colLabel: string; rows: MetricsLatency[] }) {
  return (
    <table className="dash__table">
      <thead>
        <tr>
          <th>{colLabel}</th>
          <th>count</th>
          <th>p50</th>
          <th>p95</th>
          <th>p99</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((r) => (
          <tr key={r.label}>
            <td>
              <code>{r.label}</code>
            </td>
            <td>{compact(r.count)}</td>
            <td>{r.count > 0 ? dur(r.p50_ms) : "—"}</td>
            <td className={p95Class(r.p95_ms)}>{r.count > 0 ? dur(r.p95_ms) : "—"}</td>
            <td className={p95Class(r.p99_ms)}>{r.count > 0 ? dur(r.p99_ms) : "—"}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

// Format a percentile ms value; the >10s overflow bucket reports as 10001.
function dur(ms: number): string {
  return ms > 10_000 ? ">10s" : `${ms}ms`;
}

function KpiSpark({
  label,
  value,
  max,
  samples,
  tone,
  hint,
}: {
  label: string;
  value: number;
  max?: number;
  samples: number[];
  tone?: string;
  hint?: string;
}) {
  const toneCls = tone === "is-err" ? "dash__kpi--err" : tone === "is-warn" ? "dash__kpi--warn" : "";
  return (
    <div className={`dash__kpi dash__kpi--spark ${toneCls}`}>
      <div className="dash__kpi-val">
        {value}
        {max != null && <span className="dash__kpi-max"> / {max}</span>}
      </div>
      <div className="dash__kpi-label">{label}</div>
      <Sparkline samples={samples} />
      {hint && <div className="dash__kpi-hint">{hint}</div>}
    </div>
  );
}

function Sparkline({ samples }: { samples: number[] }) {
  const w = 160;
  const h = 24;
  if (samples.length < 2) {
    return (
      <svg width={w} height={h} className="dash__spark" aria-hidden="true">
        <line x1="0" y1={h - 1} x2={w} y2={h - 1} stroke="currentColor" />
      </svg>
    );
  }
  const max = Math.max(1, ...samples);
  const step = w / (SPARK_WINDOW - 1);
  // Right-align: latest sample at x = w. Pad the left when the
  // window isn't full yet so the line still draws meaningfully.
  const offset = w - (samples.length - 1) * step;
  const d = samples
    .map((v, i) => {
      const x = offset + i * step;
      const y = h - 1 - (v / max) * (h - 2);
      return `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg width={w} height={h} className="dash__spark" aria-hidden="true">
      <path d={d} fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

function compact(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return n.toString();
}

function depthClass(depth: number, capacity: number): string {
  const load = depth / Math.max(capacity, 1);
  if (load >= 0.9) return "is-err";
  if (load >= 0.5) return "is-warn";
  return "";
}

function p95Class(p95: number): string {
  if (p95 > 1000) return "is-err";
  if (p95 > 250) return "is-warn";
  return "";
}
