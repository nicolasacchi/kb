import { useEffect, useState } from "react";
import { useDaemonStatus } from "../../hooks/useDaemonStatus";
import type { DaemonStatus } from "../../api/sse";
import { Icon } from "../icons";

// E1 — has any daemon actually CONNECTED yet? The latch the banner gates on.
// True iff some daemon's per-daemon phase is anything but `disconnected`. The
// empty initial snapshot and the synchronous connectOne `connected:false`
// snapshot both yield false, so this never flips true before a real connection
// (the bug that flashed the banner on load).
export function anyDaemonConnected(daemons: DaemonStatus[]): boolean {
  return daemons.some((d) => d.phase !== "disconnected");
}

// Show the banner only once we've BEEN connected and are now disconnected.
export function daemonBannerVisible(
  phase: DaemonStatus["phase"],
  everConnected: boolean,
): boolean {
  return phase === "disconnected" && everConnected;
}

// CT — how long the disconnected state must hold before the banner actually
// renders. A reload/tab-throttle/brief network blip flickers `disconnected`
// for well under this and should never flash the banner; a real outage
// holds past it.
export const BANNER_GRACE_MS = 3000;

// Delays a transition to `true` by `graceMs`; a transition to `false` (the
// connection recovered) takes effect IMMEDIATELY — no grace on the way
// down. Extracted from the component below so the setTimeout mechanics are
// testable with fake timers independent of the SSE plumbing.
export function useGracePeriod(active: boolean, graceMs: number): boolean {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (!active) {
      setShown(false);
      return;
    }
    const t = setTimeout(() => setShown(true), graceMs);
    return () => clearTimeout(t);
  }, [active, graceMs]);
  return shown;
}

// E1 — a slim, full-width alert when the live SSE link to the daemon drops.
// Connection loss was previously silent except the StatusBar's small pulse, so
// a reader watching a stale page had no idea the daemon went away. The SSE
// manager auto-reconnects (resync), so the banner clears itself on recovery.
//
// Gated on "ever connected" so it never flashes during the initial connect
// window (and "lost connection" is only true once we had one). The gate keys
// on a real per-daemon CONNECTED signal — `daemons.some(d => not disconnected)`
// — NOT the aggregate `phase`: the aggregate is `idle` for BOTH "connected and
// quiet" and "never connected yet" (empty snapshots), and connectOne emits a
// `connected:false` snapshot synchronously on start, so gating on the aggregate
// phase would flip the latch true on the first idle render and flash the banner.
export default function DaemonDownBanner() {
  const status = useDaemonStatus();
  const [everConnected, setEverConnected] = useState(false);

  useEffect(() => {
    if (anyDaemonConnected(status.daemons)) setEverConnected(true);
  }, [status.daemons]);

  // CT — the raw predicate is gated behind a 3s grace period so a blip
  // (a reload, a throttled background tab, a momentary network drop)
  // never flashes the banner; only a disconnect that HOLDS earns it.
  const wantsToShow = daemonBannerVisible(status.phase, everConnected);
  const visible = useGracePeriod(wantsToShow, BANNER_GRACE_MS);

  if (!visible) return null;

  return (
    <div className="kb-daemon-down" role="alert">
      <Icon.Warn />
      <span>
        Lost connection to the daemon — reconnecting… Content may be stale.
        {status.authSuspect && (
          <>
            {" "}Your session may have expired —{" "}
            <a href={window.location.href}>reload to sign in</a>.
          </>
        )}
      </span>
    </div>
  );
}
