import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { fetchFile } from "../api/client";
import type { SetSpanOut } from "../api/types";
import CodeView, { type GotoSel } from "../components/CodeView";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import StepperBar from "../components/story/StepperBar";
import { useFile } from "../hooks/useFile";
import { useSet } from "../hooks/useSets";
import { shortSha } from "../lib/format";
import { clampStep } from "../lib/storyPlayer";
import { parseTourStepParam } from "../lib/tourPlayer";
import { createTourUrlSync } from "../lib/tourUrlSync";
import { setUrl } from "../lib/setsUrl";
import { useCommandHandlers, useCommandScope } from "../commands/CommandRoot";
import { spanLineLabel } from "./SetDetail";
import "../styles/sets.css";

// Phase E4 ("kb-code v2 — The Operable Reader") — tour mode: a guided
// code walkthrough, stepping through one reading set's ordered spans. Same
// composition shape as Phase C7's StoryPlayer (`components/story/
// StoryPlayer.tsx`) — a plain `useFile` per step, `gotoSel` to center the
// span's line range, a `StepperBar` footer (extracted FROM StoryPlayer for
// exactly this reuse) — but stepping across DIFFERENT files (a set's spans)
// rather than one file's own commit history. The span's own `note` is the
// narration; a pinned `ref` (when the span carries one) is respected
// exactly as captured, not re-resolved against the working tree.

const TOUR_DOT_CAP = 30;

function spanLabel(s: SetSpanOut | undefined): string {
  if (!s) return "";
  return `${s.path}${spanLineLabel(s)}`;
}

export default function Tour() {
  const { repo = "", id = "" } = useParams<{ repo: string; id: string }>();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [searchParams] = useSearchParams();
  const stepParam = searchParams.get("step");

  const set = useSet(repo, id);
  const spans = useMemo(() => set.data?.spans ?? [], [set.data]);

  const [stepIndex, setStepIndex] = useState(() => clampStep(parseTourStepParam(stepParam), spans.length));
  // `spans` starts empty (the fetch hasn't resolved yet) — re-seed from
  // `?step=` exactly ONCE, the first time a non-empty list arrives. Mirrors
  // `StoryPlayer.tsx`'s own `seededRef` discipline.
  const seededRef = useRef(false);
  useEffect(() => {
    if (seededRef.current || spans.length === 0) return;
    seededRef.current = true;
    setStepIndex(clampStep(parseTourStepParam(stepParam), spans.length));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [spans.length]);

  const clamped = clampStep(stepIndex, spans.length);
  const current: SetSpanOut | undefined = spans[clamped];

  function goToStep(next: number) {
    setStepIndex(clampStep(next, spans.length));
  }

  const file = useFile(repo, current?.path, current?.ref);

  const gotoSel = useMemo<GotoSel | null>(() => {
    if (!current || current.line_start == null || current.line_end == null) return null;
    return { start: current.line_start, end: current.line_end, nonce: clamped };
  }, [current, clamped]);

  // --- prefetch the NEXT step's file — mirrors StoryPlayer's own
  // best-effort prefetch (same cache keys `useFile` reads).
  useEffect(() => {
    const next = spans[clamped + 1];
    if (!next) return;
    void queryClient
      .prefetchQuery({
        queryKey: ["file", repo, next.path, next.ref ?? null],
        queryFn: () => fetchFile(repo, next.path, next.ref),
      })
      .catch(() => {});
  }, [clamped, spans, repo, queryClient]);

  // --- ?step=<N> URL sync (debounced) -------------------------------------
  const urlSyncRef = useRef<ReturnType<typeof createTourUrlSync> | null>(null);
  useEffect(() => {
    const sync = createTourUrlSync({
      replace: (search) => navigate({ search }, { replace: true }),
      getSearch: () => window.location.search,
    });
    urlSyncRef.current = sync;
    return () => {
      sync.dispose();
      urlSyncRef.current = null;
    };
    // `navigate` is identity-unstable across renders by design (same
    // reasoning `StoryPlayer.tsx`'s own sync effect documents).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, id]);
  useEffect(() => {
    if (spans.length > 0) urlSyncRef.current?.onStep(clamped);
  }, [clamped, spans.length]);
  // The handlers below are registered once; this ref is how they read the
  // live step without re-registering on every keystroke.
  const stepRef = useRef(clamped);
  stepRef.current = clamped;

  // --- keyboard (V70-A5: kbc-cmd/1 handlers, scope `board`) ---------------
  //
  // `Escape` NO LONGER NAVIGATES. It used to jump back to the set detail,
  // which is the exact pattern §P2 rules out ("Esc rows are a first-class key
  // class with a dismiss order; Esc never navigates") — one key that
  // sometimes closes a thing and sometimes throws away your place. Leaving
  // the tour is `u` (`nav.back`) or the browser's own Back, both of which
  // restore where you came from. Esc here has nothing left to dismiss, so it
  // is simply not bound.
  useCommandScope("board", { board: "player" });
  useCommandHandlers({
    "player.prev": () => goToStep(stepRef.current - 1),
    "player.next": () => goToStep(stepRef.current + 1),
  });

  if (set.isLoading) {
    return <div className="kbc-reader__hint">Loading tour…</div>;
  }
  if (set.error) {
    return <div className="kbc-reader__hint kbc-reader__hint--error">{(set.error as Error).message}</div>;
  }
  if (spans.length === 0 || !current) {
    return (
      <EmptyState
        icon={<Icon.List />}
        title="Nothing to tour"
        hint="This reading set has no spans yet."
        action={{ label: "Back to set", to: setUrl(repo, id) }}
      />
    );
  }

  return (
    <div className="kbc-tour" data-kbc-tour>
      <header className="kbc-tour__head">
        <span className="kbc-tour__set-name">{set.data?.name}</span>
        <span className="kbc-tour__path" data-kbc-tour-path>
          {current.path}
          {spanLineLabel(current)}
          {current.ref && (
            <span className="kbc-tour__ref-chip" data-kbc-tour-ref title={current.ref}>
              {shortSha(current.ref)}
            </span>
          )}
        </span>
        <button
          type="button"
          className="kbc-tour__exit"
          onClick={() => navigate(setUrl(repo, id))}
          title="Exit tour"
          aria-label="exit tour"
          data-kbc-tour-exit
        >
          <Icon.X />
        </button>
      </header>

      <div className="kbc-tour__stage">
        {file.isLoading ? (
          <div className="kbc-reader__hint">Loading…</div>
        ) : file.error ? (
          <div className="kbc-reader__hint kbc-reader__hint--error">{(file.error as Error).message}</div>
        ) : file.data && file.data.encoding === "utf8" ? (
          <CodeView content={file.data.content} spans={file.data.highlights} blobHash={file.data.blob_hash} gotoSel={gotoSel} />
        ) : file.data ? (
          <div className="kbc-reader__hint">Binary file — preview not supported</div>
        ) : null}
      </div>

      {current.note && (
        <footer className="kbc-tour__narration" data-kbc-tour-narration>
          {current.note}
        </footer>
      )}

      <StepperBar
        prefix="kbc-tour"
        index={clamped}
        total={spans.length}
        dotCap={TOUR_DOT_CAP}
        stepKey={(i) => String(spans[i]?.ordinal ?? i)}
        stepLabel={(i) => spanLabel(spans[i])}
        groupAriaLabel="tour steps"
        sliderAriaLabel="tour step"
        onStep={goToStep}
      />
    </div>
  );
}
