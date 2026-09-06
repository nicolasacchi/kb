import { useEffect, useState } from "react";

/// Shared wall-clock tick for age/presence labels that would otherwise
/// call `Date.now()` per row each render. One state update per interval
/// re-renders the route tree with a stable `now` prop.
export function useNowTick(intervalMs: number = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(id);
  }, [intervalMs]);
  return now;
}
