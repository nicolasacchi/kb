// Pure keyboard-cursor + open/close state for the peek panel (B1's `gd`/
// `gr`/`K` quick-navigation surface, `components/peek/PeekPanel.tsx`) —
// mirrors `paletteReducer.ts`'s shape (a clamped row cursor over a result
// list) pared down to ONE flat list: a peek result set is always a single
// list (definitions, references, or a one-row hover summary), never
// grouped into sections the way the omnibox's lanes are.
//
// Row DATA (the fetched `DefHit`/`RefHit` rows, the approximate/note
// honesty flags, the `gd`-vs-`gr`-vs-`K` mode) travels through the state
// too, so this reducer captures the panel's WHOLE visible lifecycle — but
// the actual `fetchDefs`/`fetchXrefs` network calls live in `Reader.tsx`
// (a reducer has no business owning I/O); this module only shapes what
// arrives before/after those calls into what the panel renders.

import type { DefHit, DefsOut, RefsOut, ResolveCandidate } from "../api/types";

export type PeekMode = "defs" | "refs" | "hover";

/// One row the panel can list — a `DefHit` or a `RefHit` normalized to a
/// common shape (`Reader`'s `handlePeekActivate` doesn't need to know which
/// endpoint produced a row to navigate it). `symbolKind`/`container` are
/// def-only; `text` is ref-only — both optional rather than a tagged union,
/// since the panel's row renderer already switches on `PeekState.mode` (the
/// WHOLE list is one kind at a time, never mixed) and a union would just
/// make every call site re-narrow what the mode already tells it.
export interface PeekRow {
  repo: string;
  path: string;
  line: number;
  symbolKind?: string;
  container?: string | null;
  text?: string;
  approximate: boolean;
  /// `resolve::Candidate.precision` (B3) — set ONLY for rows built from
  /// `/api/resolve` (`resolveCandidateToRow`); `undefined` for the
  /// `DefHit`/`RefHit`-sourced rows the name-based `fetchDefs`/`fetchXrefs`
  /// fallback still produces. `PeekPanel`'s per-row badge renders on this
  /// field's presence, not on `state.mode` — see that component's doc.
  precision?: string;
  /// `resolve::Candidate.class` (V3.G1/H1, T1) — the trust tier
  /// (`"exact"`|`"likely"`|`"candidate"`) `PeekPanel`'s badge keys off (via
  /// `lib/trustBadge.ts`'s `trustTierFrom`), independent of `precision`
  /// above. Set alongside `precision` by `resolveCandidateToRow`;
  /// `undefined` wherever `precision` is (same rows, same reason).
  trustClass?: string;
}

/// `K`'s provenance hover card (B3) — the TOP `/api/resolve` candidate plus
/// its (possibly still-loading) `/api/why` provenance line. Renders in place
/// of the `rows`-based list — `PeekPanel` switches on `state.card`'s
/// presence, not `state.mode` (see that component's doc).
export interface HoverProvenance {
  /// `true` when `/api/why`'s attribution confidence was NEITHER
  /// `"trailer"` nor `"exact"` (or the fetch itself failed) — the card's
  /// honest "no recorded session" line, never a guess.
  none: boolean;
  displayName: string | null;
  commitSubject: string | null;
}

export interface HoverCard {
  ident: string;
  role: string | null;
  candidate: ResolveCandidate;
  /// `undefined` while the `/api/why` fetch for `candidate`'s def location
  /// is still in flight — the card OMITS the provenance line entirely
  /// during this window (never a loading placeholder), per the milestone's
  /// design.
  provenance?: HoverProvenance;
}

export interface PeekState {
  open: boolean;
  mode: PeekMode;
  /// The word the panel was opened for — the title.
  word: string;
  /// `true` between an `OPEN` and its matching `SET_ROWS`/`SET_ERROR`/
  /// `SET_CARD` — the panel shows a "Loading…" placeholder rather than a
  /// premature "no results" empty state.
  loading: boolean;
  error: string | null;
  rows: PeekRow[];
  cursor: number;
  /// Whether the CURRENT row set is approximate — drives the "approximate —
  /// name match, not scope resolution" badge (never hidden, per the
  /// milestone's honest-precision house style).
  approximate: boolean;
  /// The server's own honesty note (`RefsOut.note` / `ResolveOut.note`),
  /// shown as a badge's tooltip when present.
  note?: string;
  /// `K`-mode only: total def/ref counts, which may exceed `rows.length`
  /// (K's `rows` holds just the ONE best def row) — the summary line. Dead
  /// under B3's resolve-based `K` (which populates `card` instead), kept for
  /// any future caller that still wants a plain row-list hover summary.
  counts?: { defs: number; refs: number };
  /// `K`'s provenance hover card (B3) — see `HoverCard`'s own doc. Present
  /// only once `/api/resolve` has returned at least one candidate; absent
  /// (and `rows`/`loading`/`error` drive the body instead) while loading, on
  /// a resolve failure, or when resolve found zero candidates.
  card?: HoverCard;
}

export const initialPeekState: PeekState = {
  open: false,
  mode: "defs",
  word: "",
  loading: false,
  error: null,
  rows: [],
  cursor: 0,
  approximate: false,
};

export type PeekAction =
  /// Opens the panel empty + loading — used by `gr`/`K`, whose fetches are
  /// slow enough (a repo-wide grep, or two fetches in parallel) that a
  /// visible loading state is worth it.
  | { type: "OPEN"; mode: PeekMode; word: string }
  /// Opens the panel ALREADY populated in one shot — used by `gd`'s
  /// "ambiguous, must show the list" path: the defs lookup (an in-memory
  /// symbol-table scan) has already resolved by the time `gd` knows it
  /// needs a panel at all, so there is no loading state to show.
  | {
      type: "OPEN_WITH_ROWS";
      mode: PeekMode;
      word: string;
      rows: PeekRow[];
      approximate: boolean;
      note?: string;
      counts?: { defs: number; refs: number };
    }
  | {
      type: "SET_ROWS";
      rows: PeekRow[];
      approximate: boolean;
      note?: string;
      counts?: { defs: number; refs: number };
    }
  | { type: "SET_ERROR"; message: string }
  /// `K` (B3): the resolve fetch landed with >= 1 candidate — `card` is the
  /// TOP one, `note` (when given) is `ResolveOut.note`. `provenance` starts
  /// unset; a later `SET_CARD_PROVENANCE` fills it once `/api/why` resolves.
  | { type: "SET_CARD"; card: HoverCard; note?: string }
  /// `K` (B3): the `/api/why` follow-up for the card's candidate resolved
  /// (or was treated as absent) — merged onto the EXISTING card, a no-op if
  /// there is none (e.g. the panel closed/reopened in the meantime).
  | { type: "SET_CARD_PROVENANCE"; provenance: HoverProvenance }
  | { type: "MOVE"; delta: number }
  | { type: "CLOSE" };

function clampCursor(cursor: number, rowCount: number): number {
  if (rowCount === 0) return 0;
  return Math.min(Math.max(cursor, 0), rowCount - 1);
}

function applyRows(
  state: PeekState,
  rows: PeekRow[],
  approximate: boolean,
  note: string | undefined,
  counts: { defs: number; refs: number } | undefined,
): PeekState {
  return {
    ...state,
    loading: false,
    error: null,
    rows,
    approximate,
    note,
    counts,
    cursor: clampCursor(state.cursor, rows.length),
  };
}

export function peekReducer(state: PeekState, action: PeekAction): PeekState {
  switch (action.type) {
    case "OPEN":
      return { ...initialPeekState, open: true, loading: true, mode: action.mode, word: action.word };
    case "OPEN_WITH_ROWS":
      return applyRows(
        { ...initialPeekState, open: true, mode: action.mode, word: action.word },
        action.rows,
        action.approximate,
        action.note,
        action.counts,
      );
    case "SET_ROWS":
      return applyRows(state, action.rows, action.approximate, action.note, action.counts);
    case "SET_ERROR":
      return { ...state, loading: false, error: action.message, rows: [], counts: undefined };
    case "SET_CARD":
      return { ...state, loading: false, error: null, card: action.card, note: action.note };
    case "SET_CARD_PROVENANCE":
      return state.card ? { ...state, card: { ...state.card, provenance: action.provenance } } : state;
    case "MOVE":
      return state.rows.length === 0
        ? state
        : { ...state, cursor: clampCursor(state.cursor + action.delta, state.rows.length) };
    case "CLOSE":
      return state.open ? { ...initialPeekState } : state;
    default:
      return state;
  }
}

/// The row currently under the keyboard cursor — `Enter`'s target, and what
/// the panel highlights.
export function currentRow(state: PeekState): PeekRow | null {
  return state.rows[state.cursor] ?? null;
}

// --- server-response → panel-row mapping -----------------------------------

/// Exported so `Reader.tsx`'s `K` handler can build a single-row list from
/// just the best def hit without constructing a throwaway `DefsOut`.
export function defHitToRow(h: DefHit): PeekRow {
  return {
    repo: h.repo,
    path: h.path,
    line: h.line_start,
    symbolKind: h.kind,
    container: h.container,
    approximate: h.approximate,
  };
}

export function defRowsFrom(defs: DefsOut): PeekRow[] {
  return defs.results.map(defHitToRow);
}

export function refRowsFrom(refs: RefsOut): PeekRow[] {
  return refs.results.map((r) => ({
    repo: refs.repo,
    path: r.path,
    line: r.line,
    text: r.text,
    approximate: r.approximate,
  }));
}

/// `ResolveCandidate` → `PeekRow` (B3) — the SAME row shape `gd`'s
/// multi-candidate panel AND `K`'s hover-card "jump to def" (Enter) share,
/// so `Reader.tsx`'s `handlePeekActivate` navigates a resolve candidate
/// exactly like a `DefHit`/`RefHit` row, no special-casing. `precision`
/// rides along so `PeekPanel`'s per-row badge (B3) can render it —
/// `approximate` stays `false` here on purpose: resolve carries its own
/// honesty story PER ROW (the `precision` badge), not a whole-set exact/
/// fuzzy split the way `DefsOut.exact` drives the header badge.
export function resolveCandidateToRow(c: ResolveCandidate): PeekRow {
  return {
    repo: c.repo,
    path: c.path,
    line: c.line,
    symbolKind: c.kind ?? undefined,
    container: c.container,
    approximate: false,
    precision: c.precision,
    trustClass: c.class,
  };
}

/// `gd`'s "skip the panel" rule: EXACTLY one exact (non-approximate) match
/// navigates straight there; anything else (several exact matches — an
/// ambiguous same-name symbol in more than one place — or a fuzzy fallback
/// because there was no exact match at all) opens the panel instead of
/// silently guessing. Pure so `Reader.tsx`'s `gd` handler and this module's
/// own tests agree on exactly the same rule (the server itself never mixes
/// exact + fuzzy in one response — see `agentview::xref::resolve_defs` — so
/// checking `defs.exact` is equivalent to checking every result is
/// non-approximate).
export function singleExactMatch(defs: DefsOut): DefHit | null {
  return defs.exact && defs.results.length === 1 ? defs.results[0] : null;
}
