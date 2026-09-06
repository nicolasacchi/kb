// PRR-U5+U6 (kb v0.39 "The PR Room," design-ui.md §2 S5 — publish preview)
// — the "N marked" ephemeral client-side selection. Plan arbitration #5:
// marks live ONLY in the browser tab (never persisted, never synced to the
// server — the export preview always re-derives from live server state at
// open time via `finding_slugs=`).
//
// Held in a module-scoped store (NOT React state lifted through props) so
// the mark toggle can live inline on `FindingCard`/`FindingRow` without
// threading a new prop through `ReportPanel`/`ReviewThreadsCard` — both
// sibling-owned files this unit deliberately avoids editing. Same idiom as
// `DispositionMenu.tsx`'s `dispositionLoopbackLatched` / `AskAgentCard.tsx`'s
// loopback latch: a plain `Set` + a listener set, bridged to React via
// `useSyncExternalStore`.
import { useSyncExternalStore } from "react";
import type { FindingDispositionState, ReviewFinding } from "../api/types";

const marksByReview = new Map<number, Set<string>>();
// PRR-U8 fix — `useMarkedSlugs`'s `useSyncExternalStore` `getSnapshot`
// contract (below) requires a STABLE reference when nothing changed; this
// cache is what makes that true. See `markedSlugs`'s own doc for why a
// bare `Array.from(...)` on every call broke it.
const slugsCacheByReview = new Map<number, string[]>();
const listeners = new Set<() => void>();

function emit(): void {
  for (const l of listeners) l();
}

function setFor(reviewId: number): Set<string> {
  let s = marksByReview.get(reviewId);
  if (!s) {
    s = new Set();
    marksByReview.set(reviewId, s);
  }
  return s;
}

export function isMarked(reviewId: number, slug: string): boolean {
  return marksByReview.get(reviewId)?.has(slug) ?? false;
}

/// Pure toggle over an immutable snapshot — the underlying store applies
/// this same rule; kept as a standalone export so the flip semantics are
/// unit-testable without touching the module-level store.
export function toggleMarkedSet(marked: ReadonlySet<string>, slug: string): Set<string> {
  const next = new Set(marked);
  if (next.has(slug)) next.delete(slug);
  else next.add(slug);
  return next;
}

export function toggleMark(reviewId: number, slug: string): void {
  const s = setFor(reviewId);
  if (s.has(slug)) s.delete(slug);
  else s.add(slug);
  slugsCacheByReview.delete(reviewId);
  emit();
}

/// Clears every mark for one review — called after "Mark round done"
/// succeeds (the marked set describes THIS round; a published item has no
/// reason to stay marked for the next one).
export function clearMarks(reviewId: number): void {
  const s = marksByReview.get(reviewId);
  if (s && s.size > 0) {
    s.clear();
    slugsCacheByReview.delete(reviewId);
    emit();
  }
}

/// PRR-U8 fix — `useMarkedSlugs` feeds this straight into
/// `useSyncExternalStore` as `getSnapshot`, which REQUIRES a referentially
/// stable return when the store hasn't changed (React compares via
/// `Object.is` on every render to decide whether to re-render). The
/// original body (`Array.from(marksByReview.get(reviewId) ?? [])`)
/// allocated a NEW array on every single call — "same contents, different
/// reference" reads as "the store changed" to React, which re-renders,
/// calls this again, gets ANOTHER new array, and loops forever
/// ("Maximum update depth exceeded" — caught live opening `PublishPreview`,
/// `useMarkedCount`/`useIsMarked` above never hit this because a
/// `number`/`boolean` primitive already compares correctly via `Object.is`).
/// Cached per review, invalidated only on an actual mutation
/// (`toggleMark`/`clearMarks` above delete the entry; rebuilt lazily here).
export function markedSlugs(reviewId: number): string[] {
  const cached = slugsCacheByReview.get(reviewId);
  if (cached) return cached;
  const fresh = Array.from(marksByReview.get(reviewId) ?? []);
  slugsCacheByReview.set(reviewId, fresh);
  return fresh;
}

export function markedCount(reviewId: number): number {
  return marksByReview.get(reviewId)?.size ?? 0;
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

export function useMarkedCount(reviewId: number): number {
  return useSyncExternalStore(
    subscribe,
    () => markedCount(reviewId),
    () => markedCount(reviewId),
  );
}

export function useIsMarked(reviewId: number, slug: string): boolean {
  return useSyncExternalStore(
    subscribe,
    () => isMarked(reviewId, slug),
    () => isMarked(reviewId, slug),
  );
}

export function useMarkedSlugs(reviewId: number): string[] {
  return useSyncExternalStore(
    subscribe,
    () => markedSlugs(reviewId),
    () => markedSlugs(reviewId),
  );
}

/// design-ui.md §2 S5: "default = every non-waived undecided-or-agreed
/// unpublished finding UNMARKED — explicit opt-in." Read as an ELIGIBILITY
/// gate (which findings the toggle even appears on), not a starting value —
/// every mark starts unset regardless (the store above never seeds
/// anything). A published finding has nothing left to mark; a waived one is
/// excluded from the export by default (`classify_for_export`'s sibling
/// guard server-side) and a disputed/deferred one isn't ready to publish —
/// so the toggle only renders for undecided or agreed, unpublished findings.
export function eligibleForPublishMark(
  finding: Pick<ReviewFinding, "published_state" | "disposition">,
): boolean {
  if (finding.published_state === "published") return false;
  const state: FindingDispositionState | undefined = finding.disposition?.state;
  return state == null || state === "agree";
}

/// Test-only reset so suites don't bleed marks into each other.
export function __resetPublishMarksForTests(): void {
  marksByReview.clear();
  slugsCacheByReview.clear();
  emit();
}
