import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { fetchCommit, fetchDiff, fetchFile } from "../../api/client";
import AttributionCard from "../history/AttributionCard";
import CodeView, { type GotoSel } from "../CodeView";
import EmptyState from "../EmptyState";
import { Icon } from "../icons";
import { useCommit } from "../../hooks/useCommit";
import { useDiff } from "../../hooks/useDiff";
import { useFile } from "../../hooks/useFile";
import { useFileHistory } from "../../hooks/useFileHistory";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import { commitUrl } from "../../lib/codeUrl";
import { changedNewLineRanges, firstChangedLine, parseUnifiedDiff } from "../../lib/diff";
import { relativeTime } from "../../lib/format";
import { clampStep, initialStepIndex, playbackSteps } from "../../lib/storyPlayer";
import { createStoryUrlSync } from "../../lib/storyUrlSync";
import type { CommitSummary } from "../../api/types";
import StepperBar from "./StepperBar";
import "../../styles/story.css";

// Phase C7 ("kb-code v2 — The Operable Reader") — "watch this file being
// made": replay a file's evolution commit by commit, diff-highlighted, with
// the owning commit's own facts (subject/author/attribution) as narration.
// A pure COMPOSITION of existing endpoints — no new server route: each step
// is `useFile` at that commit's sha (the exact hook every reader pane
// already uses), `useDiff` against `<sha>^..<sha>` (the same per-commit
// revspec `routes/Commit.tsx`'s own file-change rows already use for a
// non-root commit) parsed by the shared `lib/diff.ts` hunk parser for the
// changed-line tint, and `useCommit` for the narration footer (rendering its
// `attribution` through the same `AttributionCard` the Commit page uses).
//
// Playback order is OLDEST → NEWEST (`lib/storyPlayer.ts`'s
// `playbackSteps`), the opposite of `file-history/1`'s own newest-first
// wire order and of the History tab's/`[c`/`]c`'s reading direction — story
// mode is a forward narrative, not a "step back in time" tool.
//
// The scrubber's step + `?at=` URL are two SEPARATE pieces of state: the
// step index updates immediately (arrow keys must feel instant), the URL
// write is debounced (`lib/storyUrlSync.ts`, mirroring `lib/
// cursorUrlSync.ts`'s discipline) so a fast key-repeat doesn't spam
// `history.replaceState`.

const STORY_DOT_CAP = 30;
const AUTOPLAY_MS = 3000;

function isTypingTarget(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  return !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
}

export interface StoryPlayerProps {
  repo: string;
  /// Repo-relative file path (never a directory — `Reader.tsx` only mounts
  /// this for `isFile`).
  path: string;
  /// The `?at=<sha>` the reader route carried on entry, if any. `undefined`
  /// starts at the OLDEST commit.
  atSha?: string;
  /// Esc / ✕ — hands the CURRENT step's sha back to the caller, which
  /// returns to the plain reader pinned there (`?ref=<sha>`).
  onExit: (currentSha: string) => void;
}

export default function StoryPlayer({ repo, path, atSha, onExit }: StoryPlayerProps) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const prefersReducedMotion = usePrefersReducedMotion();

  const fileHistory = useFileHistory(repo, path, true);
  const entries = fileHistory.data?.entries ?? [];
  const steps = useMemo(() => playbackSteps(entries), [entries]);

  const [stepIndex, setStepIndex] = useState(() => initialStepIndex(steps, atSha));
  // `steps` starts empty (the fetch hasn't resolved yet) — re-seed from
  // `?at=` exactly ONCE, the first time a non-empty list arrives. After
  // that the player itself (not the URL) owns `stepIndex`; the URL is kept
  // in sync the OTHER direction, via `storyUrlSync` below.
  const seededRef = useRef(false);
  useEffect(() => {
    if (seededRef.current || steps.length === 0) return;
    seededRef.current = true;
    setStepIndex(initialStepIndex(steps, atSha));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [steps.length]);

  const clamped = clampStep(stepIndex, steps.length);
  const current: CommitSummary | undefined = steps[clamped];

  const [autoPlay, setAutoPlay] = useState(false);

  function goToStep(next: number, fromAutoPlay = false) {
    setStepIndex(clampStep(next, steps.length));
    // Any MANUAL interaction pauses autoplay — deliverable 3's "pauses on
    // any manual interaction." A step driven BY the autoplay timer itself
    // must not immediately cancel the very playback that produced it.
    if (!fromAutoPlay) setAutoPlay(false);
  }

  // --- data for the current step ------------------------------------------
  const file = useFile(repo, current ? path : undefined, current?.sha);
  const commit = useCommit(repo, current?.sha);
  // A root commit has no `^` parent to diff against — skip the diff fetch
  // entirely rather than 404 (mirrors `routes/Commit.tsx`'s own
  // `isRoot`-gated `from`). Rare for a FILE's own oldest history entry (it
  // would mean the file has existed since the repo's genesis commit), but
  // `parents` only arrives once `commit.data` resolves, so `diffFrom`
  // briefly assumes non-root and corrects itself a tick later — a harmless
  // wasted request in that edge case, never a crash.
  const isRoot = commit.data ? commit.data.parents.length === 0 : false;
  const diffFrom = current && !isRoot ? `${current.sha}^` : undefined;
  const diff = useDiff(repo, current ? path : undefined, diffFrom, current?.sha);

  const parsedDiff = useMemo(() => (diff.data ? parseUnifiedDiff(diff.data.diff) : null), [diff.data]);
  const changedRanges = useMemo(() => (parsedDiff ? changedNewLineRanges(parsedDiff) : []), [parsedDiff]);
  const firstLine = firstChangedLine(changedRanges);

  // `nonce: clamped` — a fresh value every step, even when two consecutive
  // steps happen to share the same first-changed line, so the view always
  // re-scrolls/re-centers (mirrors every other `GotoSel` caller's nonce
  // discipline in `Reader.tsx`).
  const gotoSel = useMemo<GotoSel | null>(() => {
    if (firstLine === undefined) return null;
    return { start: firstLine, end: firstLine, nonce: clamped };
  }, [firstLine, clamped]);

  // --- prefetch the NEXT step's file + diff + commit ----------------------
  // Deliberately best-effort: `queryClient.prefetchQuery` populates the
  // SAME cache keys `useFile`/`useDiff`/`useCommit` read (`api/
  // queryClient.ts`'s key conventions), so stepping forward into an
  // already-prefetched step resolves from cache instantly. A rejected
  // prefetch (e.g. the next step happens to be a root commit — see
  // `isRoot` above) is swallowed here; the step's OWN hooks re-fetch
  // correctly (or skip the diff entirely) once actually navigated to.
  useEffect(() => {
    const next = steps[clamped + 1];
    if (!next) return;
    void queryClient
      .prefetchQuery({ queryKey: ["file", repo, path, next.sha], queryFn: () => fetchFile(repo, path, next.sha) })
      .catch(() => {});
    void queryClient
      .prefetchQuery({ queryKey: ["commit", repo, next.sha], queryFn: () => fetchCommit(repo, next.sha) })
      .catch(() => {});
    void queryClient
      .prefetchQuery({
        queryKey: ["diff", repo, path, `${next.sha}^`, next.sha ?? null],
        queryFn: () => fetchDiff(repo, path, `${next.sha}^`, next.sha),
      })
      .catch(() => {});
  }, [clamped, steps, repo, path, queryClient]);

  // --- autoplay: one step every 3s, stops at the last step ----------------
  useEffect(() => {
    if (!autoPlay) return;
    if (clamped >= steps.length - 1) {
      setAutoPlay(false);
      return;
    }
    const t = setTimeout(() => goToStep(clamped + 1, true), AUTOPLAY_MS);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoPlay, clamped, steps.length]);

  // --- ?at=<sha> URL sync (debounced) -------------------------------------
  const urlSyncRef = useRef<ReturnType<typeof createStoryUrlSync> | null>(null);
  useEffect(() => {
    const sync = createStoryUrlSync({
      replace: (search) => navigate({ search }, { replace: true }),
      getSearch: () => window.location.search,
    });
    urlSyncRef.current = sync;
    return () => {
      sync.dispose();
      urlSyncRef.current = null;
    };
    // `navigate` is identity-unstable across renders by design (same
    // reasoning `Reader.tsx`'s own `cursorSyncRef1` effect documents) — the
    // sync only needs A working navigate, not the latest closure.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, path]);
  useEffect(() => {
    if (current) urlSyncRef.current?.onStep(current.sha);
  }, [current]);

  // --- keyboard: ←/→ step, `p` toggles autoplay, Esc EXITS THE MODE --------
  //
  // V70-A5, two changes, both from D2/§P2:
  //   * autoplay moved from `Space` to `p` (play/pause). Space is the global
  //     leader and only the leader; a player that swallowed it was a hole in
  //     the leader's "works everywhere" promise.
  //   * Esc is `dismiss.mode` (dismiss_order 7), not navigation. It leaves
  //     the story MODE — the same thing the visible Exit control does — which
  //     is a dismissal, and stays honest with "Esc never navigates" because
  //     `onExit` returns you to the file you were already reading rather than
  //     pushing somewhere new.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isTypingTarget(e.target)) return;
      switch (e.key) {
        case "ArrowLeft":
          e.preventDefault();
          goToStep(clamped - 1);
          break;
        case "ArrowRight":
          e.preventDefault();
          goToStep(clamped + 1);
          break;
        case "p":
          // Deliverable 5 — reduced motion means no default-on/available
          // autoplay at all, not just a quieter transition.
          if (prefersReducedMotion) return;
          e.preventDefault();
          setAutoPlay((v) => !v);
          break;
        case "Escape":
          e.preventDefault();
          if (current) onExit(current.sha);
          break;
        default:
          break;
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clamped, steps.length, current, prefersReducedMotion]);

  if (fileHistory.isLoading) {
    return <div className="kbc-reader__hint">Loading history…</div>;
  }
  if (steps.length === 0) {
    return (
      <EmptyState
        icon={<Icon.History />}
        title="No history to play"
        hint="This file has no commits yet — story mode needs at least one."
      />
    );
  }

  const author = commit.data?.author;
  const attribution = commit.data?.attribution;

  return (
    <div className="kbc-story" data-kbc-story>
      <header className="kbc-story__head">
        <span className="kbc-story__path">{path}</span>
        <button
          type="button"
          className="kbc-story__exit"
          onClick={() => current && onExit(current.sha)}
          title="Exit story mode"
          aria-label="exit story mode"
          data-kbc-story-exit
        >
          <Icon.X />
        </button>
      </header>

      <div className="kbc-story__stage">
        {file.isLoading ? (
          <div className="kbc-reader__hint">Loading…</div>
        ) : file.error ? (
          <div className="kbc-reader__hint kbc-reader__hint--error">{(file.error as Error).message}</div>
        ) : file.data && file.data.encoding === "utf8" ? (
          <CodeView
            content={file.data.content}
            spans={file.data.highlights}
            blobHash={file.data.blob_hash}
            gotoSel={gotoSel}
            storyLines={changedRanges}
          />
        ) : file.data ? (
          <div className="kbc-reader__hint">Binary file — preview not supported</div>
        ) : null}
      </div>

      <footer className="kbc-story__narration" data-kbc-story-narration>
        {commit.isLoading ? (
          <div className="kbc-reader__hint">Loading commit…</div>
        ) : commit.data ? (
          <>
            <div className="kbc-story__narration-head">
              <span className="kbc-story__narration-subject" data-kbc-story-narration-subject>
                {commit.data.subject}
              </span>
              {author && (
                <span className="kbc-story__narration-meta">
                  {author.name} · {relativeTime(author.time)}
                </span>
              )}
            </div>
            <div className="kbc-story__narration-foot">
              {/* Honest absence: no session → `AttributionCard` itself
                  renders the plain confidence badge with no session link,
                  never a fabricated one (`AttributionCard.tsx`'s own doc). */}
              {attribution && <AttributionCard attribution={attribution} compact />}
              <a
                className="kbc-story__open-commit"
                href={commitUrl(repo, commit.data.sha)}
                onClick={(e) => {
                  e.preventDefault();
                  navigate(commitUrl(repo, commit.data!.sha));
                }}
                data-kbc-story-open-commit
              >
                open commit →
              </a>
            </div>
          </>
        ) : null}
      </footer>

      <StepperBar
        prefix="kbc-story"
        index={clamped}
        total={steps.length}
        dotCap={STORY_DOT_CAP}
        stepKey={(i) => steps[i].sha}
        stepLabel={(i) => steps[i].subject}
        groupAriaLabel="story steps"
        sliderAriaLabel="story step"
        onStep={goToStep}
        autoplay={
          prefersReducedMotion ? undefined : { on: autoPlay, onToggle: () => setAutoPlay((v) => !v) }
        }
      />
    </div>
  );
}
