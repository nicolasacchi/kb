import { useState } from "react";
import { useDaemonStatus } from "../hooks/useDaemonStatus";
import DaemonDetail, { PHASE_LABEL } from "./DaemonDetail";

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
          <DaemonDetail />
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
