// V80-M3 — "the current review travels with the reader."
//
// WHAT THIS IS: a per-repo "which review am I working" marker, kept
// ENTIRELY in the browser. The daemon has no notion of a current review —
// no route, no table, no wire type — this is chrome state, the same kind
// of recorded CLI-parity exemption `lib/reviewDrafts.ts` already carries
// ("there is no `review_drafts` table and no `kb-code review draft`
// verb, because an unpublished draft is not a fact about the review
// yet"). Here the underlying fact IS real (the review exists, server-side)
// but WHICH one the operator is currently working, in THIS tab, is a
// browser-only lens over it — the same footing `lib/searchHistory.ts`'s
// own exemption already documents ("History and saved searches never
// leave the browser").
//
// PERSISTENCE: `sessionStorage` (never `localStorage`), under
// `kbc:current-review:<repo>` — `reviewDrafts.ts`'s own reasoning applies
// verbatim: a marker that outlived the tab would resurface weeks later
// against a review that has since closed, merged, or been deleted. A
// reload restores it; closing the tab discards it.
//
// CROSS-COMPONENT SYNC, WITHIN ONE TAB: the TopBar chip, the reader's rail
// gate + URL sync, and the search results chip all mount as separate React
// subtrees with no shared owner, and `sessionStorage` fires no `storage`
// event for a same-tab write (only OTHER tabs observe that event) — so a
// plain module-level pub-sub is the one channel that lets them agree
// immediately. Same idiom as `lib/publishMarks.ts`'s `useSyncExternalStore`
// bridge, applied here over a persisted value instead of a purely in-memory
// one.
//
// PURITY: `getCurrentReview`/`setCurrentReview`/`clearCurrentReview` all
// take an injectable `StorageLike` (`desk/deskState.ts`'s shape), so the
// unit suite exercises every branch — including a corrupt blob — with no
// DOM, `deskState.ts`'s own posture. The hook layer additionally keeps an
// in-memory snapshot per repo (seeded from storage on first touch) purely
// so `useSyncExternalStore`'s referential-stability requirement holds
// (`publishMarks.ts`'s own documented fix for the same hazard) — reading/
// writing through an explicit non-default `storage` argument (tests only)
// never touches that cache, so it can never leak between cases.

import { useSyncExternalStore } from "react";
import type { StorageLike } from "../desk/deskState";

/// Bumped only when the shape changes in a way that cannot be forward-
/// migrated silently; `getCurrentReview` treats any other version as
/// "unset" rather than guessing at an older shape.
export const CURRENT_REVIEW_VERSION = 1;

export interface CurrentReview {
  id: string;
  /// Absent when the marker was set from a cold `?review=<id>` load (only
  /// the id round-trips through a URL) — consumers fall back to a short
  /// id in that case; `ReviewDetail.tsx`/`ReviewDiff.tsx` backfill the
  /// title the moment their own fetch resolves it.
  title?: string;
  /// Unix ms, client clock — display/debug only, never a server fact.
  setAt: number;
}

export function currentReviewStorageKey(repo: string): string {
  return `kbc:current-review:${repo}`;
}

function defaultStorage(): StorageLike | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    // A privacy mode that throws on the mere property access.
    return null;
  }
}

function isCurrentReview(v: unknown): v is CurrentReview {
  if (typeof v !== "object" || v === null) return false;
  const r = v as Partial<CurrentReview>;
  return (
    typeof r.id === "string" &&
    r.id !== "" &&
    (r.title === undefined || typeof r.title === "string") &&
    typeof r.setAt === "number"
  );
}

/// TOTAL: a missing key, unparseable JSON, a wrong `v`, or a malformed
/// value all degrade to `null` — never a throw, same `loadDrafts` posture
/// (`lib/reviewDrafts.ts`).
export function getCurrentReview(
  repo: string,
  storage: StorageLike | null = defaultStorage(),
): CurrentReview | null {
  if (!storage || !repo) return null;
  let raw: string | null = null;
  try {
    raw = storage.getItem(currentReviewStorageKey(repo));
  } catch {
    return null;
  }
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const blob = parsed as Record<string, unknown>;
  if (blob.v !== CURRENT_REVIEW_VERSION) return null;
  return isCurrentReview(blob.value) ? blob.value : null;
}

export function setCurrentReview(
  repo: string,
  review: { id: string; title?: string; setAt?: number },
  storage: StorageLike | null = defaultStorage(),
): void {
  if (!repo || !review.id) return;
  const value: CurrentReview = {
    id: review.id,
    ...(review.title !== undefined ? { title: review.title } : {}),
    setAt: review.setAt ?? Date.now(),
  };
  if (storage) {
    try {
      storage.setItem(currentReviewStorageKey(repo), JSON.stringify({ v: CURRENT_REVIEW_VERSION, value }));
    } catch {
      // Quota/denied — the hook's in-memory cache (below) still carries it
      // for this tab's life; a reload loses it, same posture as
      // `saveDeskState`'s own quota-failure comment.
    }
  }
  cacheAndEmit(repo, value);
}

export function clearCurrentReview(repo: string, storage: StorageLike | null = defaultStorage()): void {
  if (!repo) return;
  if (storage) {
    try {
      storage.removeItem(currentReviewStorageKey(repo));
    } catch {
      // ignore — nothing to roll back
    }
  }
  cacheAndEmit(repo, null);
}

// --- the hook layer ---------------------------------------------------------

const listeners = new Set<() => void>();
const cache = new Map<string, CurrentReview | null>();

function emit(): void {
  for (const l of listeners) l();
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

/// Only `setCurrentReview`/`clearCurrentReview` write here, and only when
/// they own the value that actually landed (see the module doc) — a test
/// that passes its own fake `StorageLike` still calls this, but it can
/// never observe cross-test state through the HOOK because no test in
/// this unit renders one (mirrors `lib/publishMarks.test.ts`'s own
/// "exercise the pure functions, not the hook" posture); `__resetForTests`
/// exists anyway, for the same belt-and-suspenders reason
/// `__resetPublishMarksForTests` does.
function cacheAndEmit(repo: string, value: CurrentReview | null): void {
  cache.set(repo, value);
  emit();
}

function snapshotFor(repo: string): CurrentReview | null {
  if (!repo) return null;
  if (!cache.has(repo)) cache.set(repo, getCurrentReview(repo));
  return cache.get(repo) ?? null;
}

/// Subscribe to the current review for `repo` — `null` when unset. Any
/// component under the SPA root that needs to know or react: the TopBar
/// chip, the reader's rail gate + URL sync, the search results chip.
export function useCurrentReview(repo: string): CurrentReview | null {
  return useSyncExternalStore(
    subscribe,
    () => snapshotFor(repo),
    () => snapshotFor(repo),
  );
}

/// Test-only reset so suites don't bleed the hook's in-memory cache into
/// each other — the same belt-and-suspenders `publishMarks.ts` carries.
export function __resetCurrentReviewForTests(): void {
  cache.clear();
  emit();
}
