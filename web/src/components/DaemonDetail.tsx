import { useDaemonStatus } from "../hooks/useDaemonStatus";
import { useMetrics } from "../hooks/useMetrics";
import type { Phase } from "../api/sse";

export const PHASE_LABEL: Record<Phase, string> = {
  idle: "idle",
  indexing: "indexing",
  degraded: "degraded",
  disconnected: "disconnected",
};

// The per-daemon rows + fleet-aggregate metrics (req/s, storage queue depth,
// per-route p50/p95) shown in a status popover: the Spine's DaemonStatus on
// desktop, the bottom-right StatusPill on phones. Mount it only while the
// popover is open: the live metrics ride a separate on-demand SSE stream
// that opens with this component, so a closed popover pulls no 1Hz traffic.
export default function DaemonDetail() {
  const status = useDaemonStatus();
  const metrics = useMetrics(true);
  // Stale = no metrics.tick in the last 5s. Dim the metrics block.
  const stale =
    metrics.lastTickAt === null || Date.now() - metrics.lastTickAt > 5000;
  return (
    <>
      {status.daemons.length === 0 && (
        <div className="pill-panel__row">no daemons configured</div>
      )}
      {status.daemons.map((d) => (
        <div key={d.url} className="pill-panel__row">
          <span className={`pill-panel__dot pill-panel__dot--${d.phase}`} />
          <span className="pill-panel__url" title={d.url}>
            {strip(d.url)}
          </span>
          <span className="pill-panel__phase">{PHASE_LABEL[d.phase]}</span>
          {d.openErrors > 0 && (
            <span className="pill-panel__errs">{d.openErrors}e</span>
          )}
          {d.inFlight > 0 && (
            <span className="pill-panel__flight">{d.inFlight}↻</span>
          )}
          {d.openComments > 0 && (
            <span className="pill-panel__comments">
              {d.openComments}✎
            </span>
          )}
        </div>
      ))}

      {/* P2: fleet-aggregate metrics from metrics.tick */}
      {status.daemons.length > 0 && (
        <div
          className={`pill-panel__metrics ${stale ? "is-stale" : ""}`}
          aria-label="fleet metrics"
        >
          <div className="pill-panel__metrics-head">
            fleet metrics
            {stale && (
              <span className="pill-panel__stale" title="no metrics.tick in >5s">
                {" "}— waiting for tick
              </span>
            )}
          </div>
          <div className="pill-panel__metrics-row">
            <span title="HTTP requests/sec across the fleet">
              {metrics.requestsLastSec} req/s
            </span>
            <span title="cumulative HTTP requests since daemon boot">
              {compact(metrics.requestsTotal)} total
            </span>
            <span
              title="max storage actor channel depth across the fleet"
              className={depthClass(metrics.storageDepth, metrics.storageCapacity)}
            >
              store {metrics.storageDepth}/{metrics.storageCapacity}
            </span>
          </div>
          {metrics.routes.length > 0 && (
            <table className="pill-panel__routes" aria-label="per-route">
              <thead>
                <tr>
                  <th>route</th>
                  <th>count</th>
                  <th>p50</th>
                  <th>p95</th>
                </tr>
              </thead>
              <tbody>
                {metrics.routes.map((r) => (
                  <tr key={r.kind}>
                    <td>{r.kind}</td>
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
        </div>
      )}
    </>
  );
}

function strip(url: string): string {
  return url.replace(/^https?:\/\//, "");
}

// Compact-form integer: 1234 → "1.2k", 1_234_567 → "1.2M".
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
