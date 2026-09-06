// W1.gallery — pure, colocated-tested helpers behind the corpus lobby
// (deterministic highlights pick, category segments, tag chips, word sums).
// Side-effect-free: census counting goes through lib/census, and read-state
// filtering is server-authoritative (#35) — neither lives here.

import type { DocSummary } from "../api/client";

// ── 3. corpus-lobby pure derivations ──────────────────────────────────────

/// Deterministic "highlights" pick for the lobby strip: most-backlinked
/// first, ties broken by more-recently-modified, final tiebreak on id so the
/// result is a total order (stable across re-renders / server row reorders).
export function pickHighlights(docs: DocSummary[], n = 3): DocSummary[] {
  return [...docs]
    .sort((a, b) => {
      const bl = (b.backlinks ?? 0) - (a.backlinks ?? 0);
      if (bl !== 0) return bl;
      const mt = (b.mtime_unix ?? 0) - (a.mtime_unix ?? 0);
      if (mt !== 0) return mt;
      return a.id.localeCompare(b.id);
    })
    .slice(0, n);
}

export type TagLike = { name: string; count: number };

/// Top-N tags by count (desc), alpha tiebreak — feeds the lobby's "example
/// searches" chip row.
export function topTagChips<T extends TagLike>(tags: T[], n = 4): T[] {
  return [...tags]
    .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name))
    .slice(0, n);
}

export type CategoryLike = { value: string; count: number };
export type CategorySegment = CategoryLike & { pct: number };

/// Turn facet-bucket counts into stacked-bar segments (desc by count, alpha
/// tiebreak), each carrying its fraction (0..1) of the total. Zero-count
/// buckets are dropped; returns `[]` when the total is zero (nothing to bar).
export function categorySegments(categories: CategoryLike[]): CategorySegment[] {
  const total = categories.reduce((a, c) => a + c.count, 0);
  if (total <= 0) return [];
  return categories
    .filter((c) => c.count > 0)
    .sort((a, b) => b.count - a.count || a.value.localeCompare(b.value))
    .map((c) => ({ ...c, pct: c.count / total }));
}

/// Sum of `word_count` across the given rows — best-effort census figure:
/// callers pass the currently-LOADED page of docs (there's no dedicated
/// corpus-wide word-sum endpoint), so on a paginated kb this undercounts the
/// true total until every page has loaded. Documented at the call site.
export function sumWords(docs: DocSummary[]): number {
  return docs.reduce((a, d) => a + (d.word_count ?? 0), 0);
}

// ── 4. visit-gated auto-collapse (SH.D.3) ───────────────────────────────
//
// The lobby is generous on a newcomer's first few visits (shown expanded, so
// the census/highlights/tag chips are seen at least once without hunting for
// a toggle), then gets out of the way once the operator has visibly SEEN it
// expanded that many times — an evidence-gated default, the same shape as
// the map-home / ambient promotion gates (standing rule 3), not a one-shot
// "don't show again". A manual toggle is a STRONGER signal than the visit
// count and always overrides it, in either direction, until reversed —
// "re-pins" the state until the operator flips it back themselves.

/// After the lobby has been SEEN expanded this many times (across past
/// sessions — see `bumpLobbySeenExpanded`), it defaults to collapsed.
export const LOBBY_AUTO_COLLAPSE_AFTER = 3;

/// A manual toggle, once made, always wins over the seen-count heuristic —
/// `null` means "no manual toggle yet, defer to the heuristic".
export type LobbyOverride = "open" | "closed" | null;

export interface LobbyCollapseState {
  /// Times the lobby has rendered expanded across past mounts/sessions.
  seenExpanded: number;
  override: LobbyOverride;
}

/// Pure decision: should the lobby render collapsed on THIS load? An
/// explicit override always wins (either direction); absent one, collapse
/// once `seenExpanded` has reached the threshold. Total function — every
/// `LobbyCollapseState` maps to exactly one answer, so a fresh operator
/// (seenExpanded: 0, override: null) always starts expanded.
export function decideLobbyCollapsed(state: LobbyCollapseState): boolean {
  if (state.override === "open") return false;
  if (state.override === "closed") return true;
  return state.seenExpanded >= LOBBY_AUTO_COLLAPSE_AFTER;
}

const SEEN_KEY = "kb:gallery-lobby:seen-expanded";
const OVERRIDE_KEY = "kb:gallery-lobby:override";

/// Reads the cross-session state from localStorage. Best-effort — private
/// mode / storage-disabled falls back to "never seen, no override" (i.e. the
/// lobby starts expanded), never throws.
export function readLobbyCollapseState(): LobbyCollapseState {
  let seenExpanded = 0;
  let override: LobbyOverride = null;
  try {
    const raw = localStorage.getItem(SEEN_KEY);
    const n = raw ? Number.parseInt(raw, 10) : 0;
    if (Number.isFinite(n) && n >= 0) seenExpanded = n;
    const ov = localStorage.getItem(OVERRIDE_KEY);
    if (ov === "open" || ov === "closed") override = ov;
  } catch {
    /* storage unavailable — best-effort, matches lib/census.ts's contract */
  }
  return { seenExpanded, override };
}

/// Records one more "seen expanded" impression. Called once per mount, only
/// when the lobby actually rendered expanded on that mount (see
/// GalleryLobby.tsx) — repeated manual toggles within a single session don't
/// inflate the count, only genuine fresh-load impressions do.
export function bumpLobbySeenExpanded(): void {
  try {
    const cur = readLobbyCollapseState();
    localStorage.setItem(SEEN_KEY, String(cur.seenExpanded + 1));
  } catch {
    /* best-effort */
  }
}

/// Records (or clears) a manual override. `null` reverts to the seen-count
/// heuristic — not currently exposed in the UI (every toggle sets an
/// explicit open/closed override), but kept total for testability.
export function setLobbyOverride(v: LobbyOverride): void {
  try {
    if (v === null) localStorage.removeItem(OVERRIDE_KEY);
    else localStorage.setItem(OVERRIDE_KEY, v);
  } catch {
    /* best-effort */
  }
}
