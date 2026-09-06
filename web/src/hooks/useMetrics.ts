import { useEffect, useState } from "react";
import { sse } from "../api/sse";

// Per-route stats emitted in metrics.tick.routes. Order matches the
// kb_server::state::RouteKind enum (8 entries always).
export type RouteSnapshot = {
  kind: string;
  count: number;
  p50_ms: number;
  p95_ms: number;
};

// Aggregated metrics across all configured daemons. Each daemon emits
// its own 1Hz metrics.tick; we aggregate by:
//   - requests_last_sec: sum across daemons (fleet-wide req/sec)
//   - requests_total: sum (cumulative since each daemon's boot)
//   - storage_channel_depth: max (worst back-pressured kb in fleet)
//   - routes: sum per route kind, max p95 per route kind
export type FleetMetrics = {
  requestsLastSec: number;
  requestsTotal: number;
  storageDepth: number;
  storageCapacity: number;
  routes: RouteSnapshot[];
  // Wall-clock time of the latest metrics.tick from any daemon. Used
  // to dim the bar when it goes stale (no tick in >5s).
  lastTickAt: number | null;
};

const EMPTY: FleetMetrics = {
  requestsLastSec: 0,
  requestsTotal: 0,
  storageDepth: 0,
  storageCapacity: 1024,
  routes: [],
  lastTickAt: null,
};

// Per-daemon state — keyed by the daemon URL the SSE manager attaches.
// The metrics.tick payload doesn't carry the daemon URL itself; we
// derive it from the EventCallback's daemon argument (the second arg
// in sse.subscribeEvent's callback signature).
type PerDaemon = {
  requestsLastSec: number;
  requestsTotal: number;
  storageDepth: number;
  storageCapacity: number;
  routes: RouteSnapshot[];
};

// useMetrics — subscribes to `metrics.tick` SSE events and exposes a
// fleet-aggregated snapshot. The hook is cheap to mount (one
// subscribeEvent call); state updates fire ≤1Hz per daemon.
//
// `enabled` gates the subscription: pass `false` (the default) to skip
// it entirely and return EMPTY. The subscription is what opens the
// on-demand metrics SSE stream (see sse.ts), so a disabled hook means
// no 1Hz wire traffic and no 1Hz re-renders. StatusPill enables it only
// while its detail panel is open.
export function useMetrics(enabled = false): FleetMetrics {
  const [snap, setSnap] = useState<FleetMetrics>(EMPTY);

  useEffect(() => {
    if (!enabled) {
      setSnap(EMPTY);
      return;
    }
    // Per-daemon state lives here so the unsubscribe can drop it.
    const byDaemon = new Map<string, PerDaemon>();

    const unsubscribe = sse.subscribeEvent(
      "metrics.tick",
      (payload, daemonUrl) => {
        const requestsLastSec = num(payload.requests_last_sec);
        const requestsTotal = num(payload.requests_total);
        const storageDepth = num(payload.storage_channel_depth);
        const storageCapacity = num(payload.storage_channel_capacity) || 1024;
        const routes = parseRoutes(payload.routes);
        byDaemon.set(daemonUrl, {
          requestsLastSec,
          requestsTotal,
          storageDepth,
          storageCapacity,
          routes,
        });
        // Aggregate.
        let reqLast = 0;
        let reqTotal = 0;
        let depth = 0;
        let cap = 1024;
        const routeAcc = new Map<string, RouteSnapshot>();
        for (const d of byDaemon.values()) {
          reqLast += d.requestsLastSec;
          reqTotal += d.requestsTotal;
          depth = Math.max(depth, d.storageDepth);
          cap = d.storageCapacity || cap;
          for (const r of d.routes) {
            const cur = routeAcc.get(r.kind);
            if (!cur) {
              routeAcc.set(r.kind, { ...r });
            } else {
              cur.count += r.count;
              cur.p50_ms = Math.max(cur.p50_ms, r.p50_ms);
              cur.p95_ms = Math.max(cur.p95_ms, r.p95_ms);
            }
          }
        }
        setSnap({
          requestsLastSec: reqLast,
          requestsTotal: reqTotal,
          storageDepth: depth,
          storageCapacity: cap,
          routes: Array.from(routeAcc.values()),
          lastTickAt: Date.now(),
        });
      },
    );
    return unsubscribe;
  }, [enabled]);

  return snap;
}

function num(v: unknown): number {
  return typeof v === "number" && Number.isFinite(v) ? v : 0;
}

function parseRoutes(v: unknown): RouteSnapshot[] {
  if (!Array.isArray(v)) return [];
  return v.flatMap((r): RouteSnapshot[] => {
    if (!r || typeof r !== "object") return [];
    const o = r as Record<string, unknown>;
    const kind = typeof o.kind === "string" ? o.kind : "";
    if (!kind) return [];
    return [
      {
        kind,
        count: num(o.count),
        p50_ms: num(o.p50_ms),
        p95_ms: num(o.p95_ms),
      },
    ];
  });
}
