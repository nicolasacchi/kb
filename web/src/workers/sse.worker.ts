// kb-sse SharedWorker — owns ONE unfiltered /api/events stream per
// daemon on behalf of every same-origin tab (invariant #24). Tabs are
// MessagePort clients (sse/transport.ts); the connection logic is the
// same context-agnostic SseCore the direct fallback runs in-tab.
//
// Lifecycle: the browser starts this worker when the first tab connects
// and terminates it when the last client document goes away (a single-tab
// reload usually restarts it — cursors survive via the tabs' localStorage
// mirror, carried back in `hello.cursors`). The worker URL is a hashed
// asset: after a deploy, pre-deploy tabs keep the old worker while new
// tabs start the new one — two connections per daemon for that transient
// window (bounded; the build-sha drift banner nudges stale tabs to
// reload), versus per-tab connections before SW2.
//
// Port hygiene: a port joins the broadcast set only after a valid
// `hello`, so the `snapshot` reply is always the FIRST message a tab
// receives (MessagePort delivery is FIFO). Dead ports are pruned via the
// MessagePort `close` event where supported, the tab's `bye` on
// pagehide, and defensively on postMessage failure — posting to a dead
// port is otherwise a harmless no-op, so stragglers cost nothing.

import { SseCore, memoryCursorStore, type CoreSink } from "../sse/core";
import {
  SSE_PROTOCOL_V,
  type TabToWorker,
  type WorkerToTab,
} from "../sse/protocol";

type ConnectEvent = Event & { ports: readonly MessagePort[] };

const ports = new Set<MessagePort>();
let core: SseCore | null = null;

function broadcast(msg: WorkerToTab) {
  for (const port of ports) {
    try {
      port.postMessage(msg);
    } catch {
      ports.delete(port);
    }
  }
}

const sink: CoreSink = {
  event: (e) => broadcast({ kind: "event", event: e }),
  status: (s) => broadcast({ kind: "status", daemon: s }),
  resync: (daemonUrl) => broadcast({ kind: "resync", daemonUrl }),
};

(self as unknown as { onconnect: (ev: ConnectEvent) => void }).onconnect = (
  ev,
) => {
  const port = ev.ports[0];
  const drop = () => ports.delete(port);
  // `close` fires when the owning document is destroyed (2025+ browsers).
  port.addEventListener("close", drop);
  port.onmessage = (me: MessageEvent) => {
    const msg = me.data as TabToWorker;
    switch (msg.kind) {
      case "hello": {
        if (msg.v !== SSE_PROTOCOL_V) return; // stale tab: no reply → it falls back to direct
        if (!core) {
          // First tab seeds config + cursors (worker memory is the sole
          // cursor ADVANCER; tabs only mirror). Later hellos must not
          // revert a newer set-daemons, so seeding happens exactly once.
          core = new SseCore({
            cursors: memoryCursorStore(msg.cursors),
            sink,
          });
          core.setDaemons(msg.daemons);
        }
        port.postMessage({
          kind: "snapshot",
          v: SSE_PROTOCOL_V,
          daemons: core.snapshots(),
        } satisfies WorkerToTab);
        ports.add(port);
        break;
      }
      case "set-daemons": {
        if (!core) return;
        core.setDaemons(msg.daemons);
        broadcast({
          kind: "snapshot",
          v: SSE_PROTOCOL_V,
          daemons: core.snapshots(),
        });
        break;
      }
      case "bye": {
        drop();
        break;
      }
    }
  };
};
