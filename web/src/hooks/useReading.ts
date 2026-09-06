import { useEffect, useState } from "react";
import { fetchReading, type ReadingSummary } from "../api/reading";
import { isAbortError } from "../api/client";
import { sse } from "../api/sse";

// RP-track — per-artifact reading summary for the Detail view (TOC heatmap +
// the inspector "Read by you" block). `null` while loading or when there's no
// capture, so consumers degrade silently. Refetches on `history.recorded`
// (visit INSERTs) for the current kb; per-section scroll/dwell UPDATEs emit no
// SSE, so the section data can lag a few seconds — acceptable for a passive
// overlay (mirrors useReadingProgress's refetch trigger).
export function useReading(
  kb: string | undefined,
  id: string | null,
): ReadingSummary | null {
  const [summary, setSummary] = useState<ReadingSummary | null>(null);
  useEffect(() => {
    if (!kb || !id) {
      setSummary(null);
      return;
    }
    let alive = true;
    const ctl = new AbortController();
    const load = () => {
      fetchReading(kb, id, { signal: ctl.signal })
        .then((s) => {
          if (alive) setSummary(s);
        })
        .catch((e) => {
          if (!isAbortError(e)) console.warn("[useReading] failed", e);
        });
    };
    load();
    const off = sse.subscribeEvent("history.recorded", (p) => {
      if (p.kb === kb) load();
    });
    // SSE gap/resync — visit INSERTs during the disconnect were missed, so the
    // cached summary may be stale; reload (mirrors queryClient's invalidation,
    // invariant #23).
    const offResync = sse.onResync(() => load());
    return () => {
      alive = false;
      ctl.abort();
      off();
      offResync();
    };
  }, [kb, id]);
  return summary;
}
