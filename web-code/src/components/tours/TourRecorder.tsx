// Record-a-tour from navigation (V74-L3b, D12).
//
// D12 asks for "record-a-tour from navigation", and this SPA already records
// navigation: `lib/trail.ts` is `kbc-trail/0`, the browser-local, per-tab ring
// of typed hops the Ramp has been appending to since V70-A6. Recording a tour
// is therefore not new machinery — it is a WINDOW over that ring:
//
//   `Space k r`  mark the ring's current end as the window's start
//   `Space k s`  stop, and open the composer prefilled with the hops since
//
// Two consequences worth stating.
//
// **The mark is browser-local and survives a reload.** It is one integer in
// `sessionStorage`, in the same store the ring itself lives in, so a recording
// that spans a page load does not silently restart. It is per TAB, exactly as
// the ring is.
//
// **Nothing is written until a human applies the composer.** A recorded path
// is raw material; a tour is prose about it. `hopsToDraft` produces a DRAFT,
// the composer is where the prose is written, and the only write is the
// explicit, loopback-only apply — which is also why this component renders no
// affordance at all for a non-loopback caller (rule 2 of the actions posture:
// ABSENT, not disabled).

import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { useCommandHandlers } from "../../commands/CommandRoot";
import { useLoopback } from "../../hooks/useLoopback";
import { currentTrailId, loadTrail, type TrailStep } from "../../lib/trail";
import { hopsToDraft, type TourDraft } from "../../lib/tourDoc";
import { tourHref } from "../../lib/toursUrl";
import { toast } from "../../lib/toast";
import TourComposer from "./TourComposer";

const MARK_KEY = "kbc:tour:recording-from";

function readMark(): number | null {
  try {
    const raw = sessionStorage.getItem(MARK_KEY);
    if (raw === null) return null;
    const n = Number(raw);
    return Number.isInteger(n) && n >= 0 ? n : null;
  } catch {
    return null;
  }
}

function writeMark(v: number | null): void {
  try {
    if (v === null) sessionStorage.removeItem(MARK_KEY);
    else sessionStorage.setItem(MARK_KEY, String(v));
  } catch {
    // Quota / private mode: recording is an enhancement, never a dependency.
  }
}

/// The hops recorded since `from` — the window a tour is drafted from.
export function hopsSince(from: number): readonly TrailStep[] {
  const id = currentTrailId();
  const trail = id ? loadTrail(id) : null;
  if (!trail) return [];
  return trail.steps.slice(Math.max(0, from));
}

export default function TourRecorder({ repo }: { repo: string | null }) {
  const [mark, setMark] = useState<number | null>(() => readMark());
  const [draft, setDraft] = useState<TourDraft | null>(null);
  const loopback = useLoopback();
  const navigate = useNavigate();

  useEffect(() => {
    writeMark(mark);
  }, [mark]);

  const start = useCallback(() => {
    if (!loopback) {
      toast.warn(
        "recording a tour needs loopback — applying one is a loopback-only write, so there would be nowhere to save it",
      );
      return;
    }
    const id = currentTrailId();
    const trail = id ? loadTrail(id) : null;
    const at = trail?.steps.length ?? 0;
    setMark(at);
    toast.ok(
      at === 0
        ? "recording a tour — walk somewhere, then Space k s to stop"
        : `recording a tour from here — ${at} earlier hop${at === 1 ? "" : "s"} are NOT included`,
    );
  }, [loopback]);

  const stop = useCallback(() => {
    if (mark === null) {
      toast.warn("nothing is being recorded — Space k r starts a tour from where you are");
      return;
    }
    const hops = hopsSince(mark);
    setMark(null);
    if (hops.length === 0) {
      toast.warn(
        "no hops since you started recording — a tour is a walk, so walk somewhere first",
      );
      return;
    }
    setDraft(hopsToDraft(hops, { slug: "", title: "" }));
  }, [mark]);

  useCommandHandlers({
    "tour.record": start,
    "tour.record-stop": stop,
  });

  if (!repo) return null;

  return (
    <>
      {mark !== null && (
        <div className="kbc-tourrec" data-kbc-tour-recording role="status">
          <span className="kbc-tourrec__dot" aria-hidden />
          recording a tour — {hopsSince(mark).length} step
          {hopsSince(mark).length === 1 ? "" : "s"} so far
          <button type="button" onClick={stop} data-kbc-tour-recording-stop>
            Stop
          </button>
        </div>
      )}
      {draft && (
        <TourComposer
          repo={repo}
          draft={draft}
          onClose={() => setDraft(null)}
          onApplied={(slug) => {
            setDraft(null);
            // Through the ONE builder, never a hand-assembled path
            // (`nav/rawUrls.test.ts` is the lint, root CLAUDE.md #35 the rule).
            navigate(tourHref(repo, slug));
          }}
        />
      )}
    </>
  );
}
