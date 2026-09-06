import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { fetchCrossStats, type CrossStats } from "../../api/client";
import { useIdentity } from "../../hooks/useArtifactHost";
import { sse } from "../../api/sse";
import { Icon } from "../icons";

// v0.10 D2 — refined-cartographic StatusBar.
//
// Layout (left → right): daemon pulse + name + sync hint · corpus
// counts · warn cluster (identity warnings, daemon-unreachable, etc.)
// · per-route extra slot · keyboard hint chips.
//
// S1 will wire the live pulse from the `metrics.tick` SSE; this D2
// build derives "idle" from a successful /api/stats fetch and counts
// artifacts/kbs from the same payload, so the chrome reads true even
// before metrics SSE arrives.
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
/// the StatusBar's per-route slot; it clears on unmount automatically.
export function useStatusExtra(node: ReactNode) {
  const { setExtra } = useContext(StatusBarContext);
  useEffect(() => {
    setExtra(node);
    return () => setExtra(null);
  }, [node, setExtra]);
}

export default function StatusBar({ warnings }: { warnings: string[] }) {
  const identity = useIdentity();
  const [stats, setStats] = useState<CrossStats | null>(null);
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
        // Best-effort; the daemon may be offline. The identity warning
        // cluster already covers reachability messaging.
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
  const reachable = stats !== null || identity !== null;
  // "active" if the last tick reported >0 requests AND fired recently.
  const active = reqRate !== null && reqRate > 0
    && lastTickAt !== null && Date.now() - lastTickAt < 5_000;
  const statusLabel = !reachable
    ? "unreachable"
    : active
      ? `active · ${reqRate}/s`
      : "idle";

  return (
    <footer className="kb-statusbar" role="contentinfo" aria-label="status bar">
      <span className="kb-status-cell">
        <span
          className={`kb-status-pulse ${!reachable ? "is-warn" : ""} ${active ? "is-active" : ""}`}
          aria-hidden="true"
        />
        <span className="kb-status-daemon">{daemonName}</span>
        <span className="kb-status-mute">{statusLabel}</span>
      </span>
      <span className="kb-status-div">|</span>
      <span className="kb-status-counts">
        {totalDocs.toLocaleString()} artifacts · {kbCount} kb
        {kbCount === 1 ? "" : "s"}
      </span>
      {warnings.length > 0 && (
        <>
          <span className="kb-status-div">|</span>
          <span
            className="kb-status-warn"
            title={warnings.join("\n")}
            role="status"
          >
            <Icon.Warn /> {warnings.length} warning
            {warnings.length === 1 ? "" : "s"}
          </span>
        </>
      )}
      {embedderDegraded && (
        <>
          <span className="kb-status-div">|</span>
          <span
            className="kb-status-warn"
            title={
              embedderRespawns > 0
                ? `embedder subprocess unrecoverable after ${embedderRespawns} respawns — semantic search is keyword-only`
                : "embedder subprocess down — semantic search is keyword-only"
            }
            role="status"
          >
            <Icon.Warn /> embedder degraded
          </span>
        </>
      )}
      {extra && (
        <>
          <span className="kb-status-div">|</span>
          <span className="kb-status-extra">{extra}</span>
        </>
      )}
      <span className="kb-status-keys" aria-hidden="true">
        <span>
          <b>?</b> help
        </span>
        <span>
          <b>/</b> search
        </span>
        <span>
          <b>j k</b> nav
        </span>
        <span>
          <b>⌘K</b> palette
        </span>
        <span>
          <b>g a</b> atlas
        </span>
      </span>
    </footer>
  );
}
