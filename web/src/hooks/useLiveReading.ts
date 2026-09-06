import { useEffect, useRef, useState } from "react";
import { isOriginOfArtifact } from "../lib/artifactHost";
import { FULLY_READ_PCT, liveSummaryFromBeacon } from "../lib/reading";
import type { ReadingSectionBeacon } from "../api/generated/ReadingSectionBeacon";
import type { ReadingSummary } from "../api/generated/ReadingSummary";

const clampPct = (n: number) => Math.max(0, Math.min(100, Math.round(n)));

// R1 — live, client-only reading progress while the user scrolls. Taps the SAME
// kb:scroll / kb:reading postMessage stream the POST path already consumes (a
// second listener is fine — TocSpy proves it). Adds ZERO network / SSE / bump
// (#19): it only optimistically updates the displayed numbers *between* server
// refreshes (which fire only on history.recorded — visit INSERT, never scroll/
// dwell UPDATE), so the rail stops freezing mid-session.
//
// rAF-coalesced (schedule-on-message, NOT a standing loop): a burst of scroll
// messages collapses to one React commit per frame — the "light" guarantee; the
// rAF self-cancels on unmount and never runs while idle (#2 spirit).
export function useLiveReading(
  id: string | null,
  kb: string | null | undefined,
  hostSuffix: string,
): {
  liveProgress: { pct: number; isDone: boolean } | null;
  liveSummary: ReadingSummary | null;
} {
  const [livePct, setLivePct] = useState<number | null>(null);
  const [liveSummary, setLiveSummary] = useState<ReadingSummary | null>(null);

  // Latest values between frames; the rAF commits them once.
  const hiPctRef = useRef(0);
  const beaconRef = useRef<{
    sections: ReadingSectionBeacon[];
    activeMs: number;
  } | null>(null);
  const frameRef = useRef<number | null>(null);

  // Reset on artifact change so the next doc starts clean.
  useEffect(() => {
    hiPctRef.current = 0;
    beaconRef.current = null;
    setLivePct(null);
    setLiveSummary(null);
  }, [id]);

  useEffect(() => {
    if (!id) return;
    function commit() {
      frameRef.current = null;
      const pct = hiPctRef.current;
      setLivePct(pct);
      if (beaconRef.current) {
        setLiveSummary(
          liveSummaryFromBeacon(
            beaconRef.current.sections,
            beaconRef.current.activeMs,
            pct,
          ),
        );
      }
    }
    function schedule() {
      if (frameRef.current != null) return;
      frameRef.current = requestAnimationFrame(commit);
    }
    function onMessage(e: MessageEvent) {
      // W3.P-a — EXACT origin (`<id>.artifacts.<root>`), not the suffix-only
      // trust boundary. The displayed live progress belongs to ONE artifact;
      // once a second pane exists its scroll/reading beacons also pass a
      // suffix-only check and would drag this readout onto the wrong doc.
      if (!isOriginOfArtifact(e.origin, id, kb, hostSuffix)) return;
      const data = e.data as {
        kind?: string;
        y?: number;
        max?: number;
        sections?: ReadingSectionBeacon[];
        active_ms?: number;
      } | null;
      if (!data || typeof data !== "object" || !data.kind) return;
      if (data.kind === "kb:scroll") {
        const y = typeof data.y === "number" ? data.y : 0;
        const max = typeof data.max === "number" ? data.max : 0;
        if (max > 0) {
          const pct = clampPct((y / max) * 100);
          if (pct > hiPctRef.current) hiPctRef.current = pct; // high-water mark
        }
        schedule();
      } else if (data.kind === "kb:reading") {
        beaconRef.current = {
          sections: Array.isArray(data.sections) ? data.sections : [],
          activeMs: typeof data.active_ms === "number" ? data.active_ms : 0,
        };
        schedule();
      }
    }
    window.addEventListener("message", onMessage);
    return () => {
      window.removeEventListener("message", onMessage);
      if (frameRef.current != null) {
        cancelAnimationFrame(frameRef.current);
        frameRef.current = null;
      }
    };
  }, [id, kb, hostSuffix]);

  const liveProgress =
    livePct === null ? null : { pct: livePct, isDone: livePct >= FULLY_READ_PCT };
  return { liveProgress, liveSummary };
}
