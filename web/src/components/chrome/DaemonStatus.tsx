import { createContext, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { fetchCrossStats, type CrossStats } from "../../api/client";
import { useIdentity } from "../../hooks/useArtifactHost";
import { sse } from "../../api/sse";
import { Icon } from "@kb/ui/icons";

// OL-L1 — the footer status bar folded into ONE indicator in the Spine's right
// slot: a dot + a short text label (never hue-only) that opens a popover with
// everything the bar used to print (daemon name, artifact/kb counts, request
// rate, embedder-degraded, identity warnings, the per-route extra slot). The
// `metrics.tick` SSE subscription moved here unchanged. The hardcoded key-hint
// chips are gone: the `?` sheet teaches keys.
type StatusExtraCtx = { extra: ReactNode; setExtra: (n: ReactNode) => void };

const StatusBarContext = createContext<StatusExtraCtx>({
  extra: null,
  // eslint-disable-next-line @typescript-eslint/no-empty-function
  setExtra: () => {},
});

export function StatusBarProvider({ children }: { children: ReactNode }) {
  const [extra, setExtra] = useState<ReactNode>(null);
  const value = useMemo(() => ({ extra, setExtra }), [extra]);
  return <StatusBarContext.Provider value={value}>{children}</StatusBarContext.Provider>;
}

/// Routes call `useStatusExtra(<some node>)` once on mount to populate
/// the daemon-status popover's per-route slot; it clears on unmount automatically.
export function useStatusExtra(node: ReactNode) {
  const { setExtra } = useContext(StatusBarContext);
  useEffect(() => {
    setExtra(node);
    return () => setExtra(null);
  }, [node, setExtra]);
}

export default function DaemonStatus({ warnings }: { warnings: string[] }) {
  const identity = useIdentity();
  const [stats, setStats] = useState<CrossStats | null>(null);
  // Unreachable only once the probe has actually failed -- not during the
  // first fetch, which would paint a warning dot on every cold load.
  const [probeFailed, setProbeFailed] = useState(false);
  const { extra } = useContext(StatusBarContext);
  // S1 — request rate from the metrics.tick SSE (1Hz). Drives the
  // "active" pulse class when > 0 requests/sec; status label flips
  // from "idle" → "active" so the operator sees real throughput.
  const [reqRate, setReqRate] = useState<number | null>(null);
  const [lastTickAt, setLastTickAt] = useState<number | null>(null);
  // v0.16 Q-track — embedder subprocess health from metrics.tick. When
  // degraded, semantic search has fallen back to keyword-only; surface it
  // so the reader/operator isn't silently getting worse results.
  const [embedderDegraded, setEmbedderDegraded] = useState(false);
  const [embedderRespawns, setEmbedderRespawns] = useState(0);

  useEffect(() => {
    const ctl = new AbortController();
    fetchCrossStats(ctl.signal)
      .then(setStats)
      .catch(() => {
        if (!ctl.signal.aborted) setProbeFailed(true);
      });
    return () => ctl.abort();
  }, []);

  // metrics.tick fires every second from the daemon's spawn_metrics_ticker.
  // Payload: { requests_total, requests_last_sec, storage_channel_depth,
  // storage_channel_capacity, routes, embedder_degraded,
  // embedder_respawn_count }. The embedder_* fields are absent on
  // pre-v0.16 daemons → defaults keep the indicator hidden.
  useEffect(() => {
    return sse.subscribeEvent("metrics.tick", (payload) => {
      const r = payload.requests_last_sec;
      if (typeof r === "number") setReqRate(r);
      setLastTickAt(Date.now());
      const deg = payload.embedder_degraded;
      if (typeof deg === "boolean") setEmbedderDegraded(deg);
      const rs = payload.embedder_respawn_count;
      if (typeof rs === "number") setEmbedderRespawns(rs);
    });
  }, []);

  const totalDocs = stats?.total_docs ?? 0;
  const kbCount = identity?.kbs.length ?? stats?.kbs.length ?? 0;
  const daemonName = identity?.name ?? stats?.daemon.name ?? "daemon";
  const reachable = stats !== null || identity !== null || !probeFailed;
  // "active" if the last tick reported >0 requests AND fired recently.
  const active = reqRate !== null && reqRate > 0
    && lastTickAt !== null && Date.now() - lastTickAt < 5_000;
  const statusLabel = !reachable
    ? "unreachable"
    : active
      ? `active · ${reqRate}/s`
      : "idle";
  // Short, fixed-width-ish word for the button itself: the right slot is
  // right-aligned, so a label whose width tracks the request rate would shift
  // every neighbour once a second.
  const hasWarn = warnings.length > 0 || embedderDegraded;
  const short = !reachable ? "offline" : hasWarn ? "warn" : active ? "active" : "idle";
  const dotClass = !reachable || hasWarn ? "is-warn" : active ? "is-active" : "";

  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="kb-statusind" ref={wrapRef}>
      <button
        type="button"
        className="kb-statusind__btn"
        data-kb-act="daemon-status"
        aria-haspopup="dialog"
        aria-expanded={open}
        title={`${daemonName}: ${statusLabel}`}
        onClick={() => setOpen((o) => !o)}
      >
        <span className={`kb-status-pulse ${dotClass}`} aria-hidden="true" />
        <span className="kb-statusind__label">{short}</span>
      </button>
      {open && (
        <div className="kb-statusind__pop" role="dialog" aria-label="daemon status">
          <div className="kb-status-cell">
            <span className={`kb-status-pulse ${dotClass}`} aria-hidden="true" />
            <span className="kb-status-daemon">{daemonName}</span>
            <span className="kb-status-mute">{statusLabel}</span>
          </div>
          <div className="kb-status-counts">
            {totalDocs.toLocaleString()} artifacts · {kbCount} kb
            {kbCount === 1 ? "" : "s"}
          </div>
          {warnings.length > 0 && (
            <div className="kb-status-warn" role="status">
              <Icon.Warn /> {warnings.length} warning{warnings.length === 1 ? "" : "s"}
              <ul className="kb-statusind__warnlist">
                {warnings.map((w, i) => (
                  <li key={i}>{w}</li>
                ))}
              </ul>
            </div>
          )}
          {embedderDegraded && (
            <div className="kb-status-warn" role="status">
              <Icon.Warn />{" "}
              {embedderRespawns > 0
                ? `embedder degraded: unrecoverable after ${embedderRespawns} respawns — semantic search is keyword-only`
                : "embedder degraded: subprocess down — semantic search is keyword-only"}
            </div>
          )}
          {extra && <div className="kb-status-extra">{extra}</div>}
        </div>
      )}
    </div>
  );
}
