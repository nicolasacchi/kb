import { useEffect, useState } from "react";
import { sse, type AggregatedStatus } from "../api/sse";

let started = false;

// useDaemonStatus — subscribes to the SSE manager's aggregated status
// (phase + in-flight + open-errors + per-daemon detail). Starts the
// manager on first mount; the manager itself is a singleton so multiple
// hook callers share state.
export function useDaemonStatus(): AggregatedStatus {
  const [status, setStatus] = useState<AggregatedStatus>({
    phase: "idle",
    inFlight: 0,
    openErrors: 0,
    openComments: 0,
    authSuspect: false,
    daemons: [],
  });

  useEffect(() => {
    if (!started) {
      sse.start();
      started = true;
    }
    return sse.subscribe(setStatus);
  }, []);

  return status;
}
