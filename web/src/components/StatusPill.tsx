import { useState } from "react";
import { useDaemonStatus } from "../hooks/useDaemonStatus";
import { useMetrics } from "../hooks/useMetrics";
import type { Phase } from "../api/sse";

const PHASE_LABEL: Record<Phase, string> = {
  idle: "idle",
  indexing: "indexing",
  degraded: "degraded",
  disconnected: "disconnected",
};

// Bottom-right status pill — aggregated across all configured daemons.
// Phase = max severity (disconnected > degraded > indexing > idle);
// in-flight, open errors, and open comments are all sums across daemons.
// Click to toggle a drop-up panel showing per-daemon detail.
//
// P2: the panel also surfaces fleet-aggregate HTTP metrics from
// metrics.tick — req/sec, storage queue depth, per-route p95.
//
// `warnings` are environment-drift diagnostics from verifyAgainstIdentity
// (build-stamp drift, mis-wired artifact-host suffix / parent_origin).
// They used to render as a full-width red banner; now they ride this pill
// as a ⚠ marker — hover the badge for the full text (native title), or
// click to read them at the top of the drop-up panel.
export default function StatusPill({
  warnings = [],
}: {
  warnings?: string[];
}) {
  const status = useDaemonStatus();
  const [open, setOpen] = useState(false);
  // Live metrics ride a separate on-demand SSE stream that only opens
  // while the panel is open — so an idle, collapsed pill pulls no 1Hz
  // traffic and never re-renders on the metrics tick.
  const metrics = useMetrics(open);
  const hasWarn = warnings.length > 0;

  // v0.12 X1 finish — the new StatusBar already shows the daemon
  // phase + corpus counts. Only render the floating drop-up pill when
  // it adds value beyond that: an env warning, open errors, in-flight
  // requests, open comments, or a multi-daemon fleet that needs the
  // per-daemon detail panel. Otherwise the pill stays hidden so the
  // chrome doesn't double-stack info at the bottom.
  const hasUniqueInfo =
    hasWarn ||
    status.openErrors > 0 ||
    status.inFlight > 0 ||
    status.openComments > 0 ||
    status.daemons.length > 1;
  if (!hasUniqueInfo && !open) return null;

  // Collapsed summary deliberately omits live `req/s` — that value updates
  // every second and made the badge flash. It lives in the drop-up panel
  // instead. What's left here changes only on real state transitions.
  const summary =
    status.openErrors > 0
      ? `${status.openErrors} open error${status.openErrors === 1 ? "" : "s"}`
      : status.inFlight > 0
        ? `${status.inFlight} in flight`
        : status.openComments > 0
          ? `${status.openComments} open comment${status.openComments === 1 ? "" : "s"}`
          : status.daemons.length > 1
            ? `${status.daemons.length} daemons`
            : PHASE_LABEL[status.phase];

  // Stale = no metrics.tick in the last 5s. Dim the metrics block.
  const stale =
    metrics.lastTickAt === null || Date.now() - metrics.lastTickAt > 5000;

  return (
    <div className="pill-container">
      {open && (
        <div className="pill-panel" role="dialog" aria-label="daemon detail">
          {hasWarn && (
            <div className="pill-panel__warnings" role="alert">
              <div className="pill-panel__warnings-head">⚠ environment drift</div>
              {warnings.map((w, i) => (
                <p key={i} className="pill-panel__warning">
                  {w}
                </p>
              ))}
            </div>
          )}
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
        </div>
      )}
      <button
        className={`pill pill--${status.phase}${hasWarn ? " pill--warn" : ""}`}
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        title={hasWarn ? warnings.join("\n\n") : undefined}
        aria-label={
          hasWarn
            ? `daemon status ${status.phase}; ${warnings.length} environment warning${
                warnings.length === 1 ? "" : "s"
              }; click for detail`
            : `daemon status ${status.phase}; click for detail`
        }
      >
        {hasWarn && (
          <span className="pill__warn" aria-hidden="true">
            ⚠
          </span>
        )}
        <span className="pill__dot" aria-hidden="true" />
        <span className="pill__text">{summary}</span>
      </button>
    </div>
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
