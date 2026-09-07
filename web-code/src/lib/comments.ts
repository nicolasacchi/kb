// V72-J2 (D8) — comments/1 in the SPA: pure derivations shared by the
// per-file comment gutter, the `~comments` dashboard, doc-hover freshness,
// and the claim → annotation bridge. Kept free of React/CodeMirror/DOM
// concerns — `components/CodeView.tsx`'s `commentMarkersFrom`,
// `components/comments/*.tsx` and `routes/Comments.tsx` are the thin
// renderers that map whatever this module computes onto JSX/CM6, same split
// `lib/diagnostics.ts`/`lib/blameGutter.ts` already establish for their own
// gutters/cards.
//
// Every count and every state rendered anywhere in this feature comes off
// the wire (`CommentOut.state`, `CommentsListOut.total`/`scan`/`blame`,
// `CommentsSummaryOut`) — this module never invents a verdict `state` did
// not already carry, and never re-derives a total the server already sent.

import type { AnnotationView, CommentOut, CommentState } from "../api/types";

// --- the gutter's three modes -----------------------------------------------

/// `all` (every kind) → `quiet` (annotation/directive rows that carry a
/// STATE only — a `doc` row is ALSO hidden in quiet, since quiet's whole
/// point is "just the actionable directives/annotations") → `doc-only`
/// (doc rows only, regardless of state) → back to `all`. Persisted client-
/// side (`kbc:prefs`), never sent to the server — comments/1 has no display-
/// mode concept, this is purely a reading filter over one file's already-
/// fetched rows.
export type CommentGutterMode = "all" | "quiet" | "doc-only";

export const COMMENT_GUTTER_MODES: readonly CommentGutterMode[] = ["all", "quiet", "doc-only"];

export function nextGutterMode(mode: CommentGutterMode): CommentGutterMode {
  const i = COMMENT_GUTTER_MODES.indexOf(mode);
  return COMMENT_GUTTER_MODES[(i + 1) % COMMENT_GUTTER_MODES.length];
}

/// A mode NEVER hides a comment class silently — `quiet`/`doc-only` are
/// declared filters with an on-screen mode chip (`Reader.tsx`), not a
/// "fewer markers, no explanation" degrade.
export function commentVisibleInMode(c: CommentOut, mode: CommentGutterMode): boolean {
  switch (mode) {
    case "all":
      return true;
    case "doc-only":
      return c.kind === "doc";
    case "quiet":
      return (c.kind === "annotation" || c.kind === "directive") && c.state.state !== "none";
  }
}

export function filterCommentsForMode(
  comments: readonly CommentOut[],
  mode: CommentGutterMode,
): CommentOut[] {
  return comments.filter((c) => commentVisibleInMode(c, mode));
}

// --- the gutter's marker map ------------------------------------------------

export interface CommentGutterMark {
  kind: string;
  state: string;
  /// Absent for `state: "none"` — nothing to justify a reason chip with.
  reason?: string;
  title: string;
}

function markerTitle(c: CommentOut): string {
  const head = c.keyword ? `${c.keyword}${c.keyword_text ? `: ${c.keyword_text}` : ""}` : c.kind;
  const fresh = freshnessCaption(c.state);
  return fresh && fresh !== "fresh" ? `${head} — ${fresh}` : head;
}

/// Fold `comments` (already filtered to the active mode by the caller) into
/// a per-line mark map — every line in `[line_start, line_end]` gets the
/// SAME mark, mirroring `lib/diagnostics.ts`'s `diagnosticGutterMarks`
/// region-spanning convention. Distinct `CommentOut` rows never overlap in
/// practice (`comments::classify`'s doc: a FIXED, exclusive precedence
/// classifies every source line into exactly one run), so there is no
/// "worst wins" tiebreak to write — the last writer for a line is also the
/// only one that ever claims it.
export function commentGutterMarkers(comments: readonly CommentOut[]): Map<number, CommentGutterMark> {
  const marks = new Map<number, CommentGutterMark>();
  for (const c of comments) {
    const end = c.line_end >= c.line_start ? c.line_end : c.line_start;
    const mark: CommentGutterMark = {
      kind: c.kind,
      state: c.state.state,
      title: markerTitle(c),
      ...(c.state.reason ? { reason: c.state.reason } : {}),
    };
    for (let line = c.line_start; line <= end; line++) marks.set(line, mark);
  }
  return marks;
}

// --- buffer navigation (`]m`/`[m`) ------------------------------------------

/// The next (`dir: 1`) or previous (`dir: -1`) comment's `line_start`
/// strictly beyond `fromLine`, wrapping around the file once — `comments`
/// is assumed already filtered to the active gutter mode (so navigation
/// only visits what the gutter actually shows) and sorted is NOT assumed
/// (this function sorts its own copy). `null` when `comments` is empty.
export function nextCommentLine(
  comments: readonly CommentOut[],
  fromLine: number,
  dir: 1 | -1,
): number | null {
  if (comments.length === 0) return null;
  const lines = [...new Set(comments.map((c) => c.line_start))].sort((a, b) => a - b);
  if (dir === 1) {
    const next = lines.find((l) => l > fromLine);
    return next ?? lines[0];
  }
  for (let i = lines.length - 1; i >= 0; i--) {
    if (lines[i] < fromLine) return lines[i];
  }
  return lines[lines.length - 1];
}

/// The block whose `[line_start, line_end]` covers `line`, or the block
/// whose `line_start` is closest to it — used by `comments.track-as-
/// annotation`/`comments.open-card` (cursor-driven) and the gutter's own
/// click/hover handlers (line-exact). `null` for an empty list.
export function commentAtLine(comments: readonly CommentOut[], line: number): CommentOut | null {
  const covering = comments.find((c) => line >= c.line_start && line <= Math.max(c.line_end, c.line_start));
  if (covering) return covering;
  if (comments.length === 0) return null;
  return comments.reduce((closest, c) =>
    Math.abs(c.line_start - line) < Math.abs(closest.line_start - line) ? c : closest,
  );
}

/// The `doc`-kind comments/1 block documenting `symbolName` in THIS file, or
/// `null` when none does (an undocumented symbol, or a symbol defined
/// elsewhere — comments/1's own hover enrichment only ever reads the CURRENT
/// file's already-fetched `doc` rows, never a second cross-file fetch). Used
/// by `editor/hoverTooltip.ts` to append comments/1's freshness caption to
/// the existing identifier hover, which otherwise renders `Symbol.doc`
/// verbatim with no state at all.
export function findDocCommentForSymbol(
  comments: readonly CommentOut[],
  symbolName: string | null | undefined,
): CommentOut | null {
  if (!symbolName) return null;
  return comments.find((c) => c.kind === "doc" && c.symbol?.name === symbolName) ?? null;
}

// --- freshness captions ------------------------------------------------------

function shortSha(sha: string | undefined): string {
  return sha ? sha.slice(0, 7) : "unknown";
}

function pluralDays(n: number): string {
  return `${n} day${n === 1 ? "" : "s"}`;
}

/// The oracle's own arithmetic, rendered as a sentence — NEVER a verdict
/// about whether the comment is wrong (`comments::drift`'s own rule).
/// `"unknown: <reason>"` is a visible state, never blank; an unrecognized
/// `state.state` (a newer daemon) degrades to that string verbatim.
export function freshnessCaption(state: CommentState): string {
  switch (state.state) {
    case "fresh":
      return "fresh";
    case "drifted": {
      const days = state.age_days ?? 0;
      return `drifted ${pluralDays(days)} (code moved at ${shortSha(state.code_commit)}, doc last touched at ${shortSha(state.doc_commit)})`;
    }
    case "unknown":
      return `unknown: ${state.reason ?? "no reason given"}`;
    case "aged": {
      const days = state.age_days ?? 0;
      return `aged ${pluralDays(days)}${state.on_date ? ` (due ${state.on_date})` : ""}`;
    }
    case "unreasoned":
      return `unreasoned suppression${state.tool ? ` (${state.tool})` : ""}`;
    case "none":
      return "";
    default:
      return state.state;
  }
}

// --- YARD-vs-signature disagreement (doc hover) -----------------------------

/// A doc block is YARD-shaped when it carries at least one `@param` or
/// `@return` tag — the ONLY shape this module attempts to cross-check; a
/// plain-prose doc comment never gets a disagreement chip (there is
/// nothing structured to disagree with).
export function isYardShaped(docText: string): boolean {
  return /@param\b/.test(docText) || /@return\b/.test(docText);
}

const YARD_PARAM_RE = /@param\s+(?:\[[^\]]*\]\s+)?(\w+)/g;

/// Every `@param` name YARD documents, in the order they appear. The
/// optional `[Type]` tag YARD allows before the name is skipped, never
/// mistaken for the name itself.
export function parseYardParams(docText: string): string[] {
  return [...docText.matchAll(YARD_PARAM_RE)].map((m) => m[1]);
}

/// Best-effort Ruby method-signature parameter names, from the plain
/// `def name(a, b = 1, *c, d:, e: 2, **f, &g)` text the symbols/hover wire
/// already carries (`HoverSymbol.signature` / `Symbol.signature`) — this
/// build has no structured `params` field to read (root project has none
/// either), so this is a small, pure, best-effort parse: strip `*`/`**`/`&`
/// markers, then a trailing `:` (keyword arg) or `=...` (default value).
/// Returns `[]` for a signature with no parens, empty parens, or `null`/
/// `undefined` — the honest "nothing to compare" case, never a guess.
export function parseRubySignatureParams(signature: string | null | undefined): string[] {
  if (!signature) return [];
  const open = signature.indexOf("(");
  const close = signature.lastIndexOf(")");
  if (open === -1 || close === -1 || close <= open) return [];
  const inner = signature.slice(open + 1, close).trim();
  if (inner === "") return [];
  return inner
    .split(",")
    .map((raw) => {
      let s = raw.trim();
      s = s.replace(/^\*\*/, "").replace(/^[*&]/, "");
      const colon = s.indexOf(":");
      if (colon !== -1) s = s.slice(0, colon);
      const eq = s.indexOf("=");
      if (eq !== -1) s = s.slice(0, eq);
      return s.trim();
    })
    .filter((s) => s.length > 0);
}

export interface YardSigDisagreement {
  yardParams: string[];
  sigParams: string[];
  /// Named by `@param` but absent from the signature — a stale doc.
  missingInSig: string[];
  /// A real parameter YARD never mentions — an incomplete doc.
  missingInYard: string[];
}

/// `null` when the doc isn't YARD-shaped, the signature has no parameters
/// to compare against (honest absence — never a guess), or the two agree.
/// Pure and client-side ONLY, per its own rule: this NEVER says which side
/// is right, only that they name different parameters.
export function yardSignatureDisagreement(
  docText: string,
  signature: string | null | undefined,
): YardSigDisagreement | null {
  if (!isYardShaped(docText)) return null;
  const sigParams = parseRubySignatureParams(signature);
  if (sigParams.length === 0) return null;
  const yardParams = parseYardParams(docText);
  const missingInSig = yardParams.filter((p) => !sigParams.includes(p));
  const missingInYard = sigParams.filter((p) => !yardParams.includes(p));
  if (missingInSig.length === 0 && missingInYard.length === 0) return null;
  return { yardParams, sigParams, missingInSig, missingInYard };
}

// --- the claim → annotation bridge ------------------------------------------

/// `annotations::INTENT_CLAIM` — the ONE new value the bridge adds to the
/// existing intent vocabulary ("add a value, not a table").
export const CLAIM_INTENT = "claim";

/// Whether a comments/1 `annotation`-kind comment is a candidate for the
/// bridge at all — its keyword must be one of the TODO-family markers
/// (`GET /api/comments/keywords`'s `todo_family`, NEVER hardcoded here).
export function isBridgeable(c: CommentOut, todoFamily: readonly string[]): boolean {
  return c.kind === "annotation" && !!c.keyword && todoFamily.includes(c.keyword);
}

export type BridgeState = "open" | "tracked" | "resolved" | "gone";

export interface BridgeRow {
  state: BridgeState;
  /// `null` only for `state: "gone"` — the comment that once anchored this
  /// claim annotation is no longer at its live-resolved line.
  comment: CommentOut | null;
  /// `null` only for `state: "open"` — no claim annotation exists yet.
  annotation: AnnotationView | null;
}

/// Join bridgeable comments/1 rows against `intent: "claim"` annotations by
/// CURRENT line — both sides are independently re-resolved against the same
/// live blob per request (comments/1's scan; `annotations::resolve`'s own
/// carry-forward, `AnnotationView.line`/`.stale`), so "same live line" is
/// the honest match key; this bridge mints no second id of its own.
///
/// The four states, DERIVED per render, never stored:
///  - `open`     — a bridgeable comment with no claim annotation at its line.
///  - `tracked`  — an UNRESOLVED claim annotation matched to a live comment.
///  - `resolved` — a RESOLVED claim annotation, whether or not the comment
///                 is still there ("resolved but the TODO text remains" is
///                 still `resolved`, not a fifth state — once closed, a
///                 vanished source line adds nothing actionable).
///  - `gone`     — an UNRESOLVED claim annotation whose line no longer
///                 matches any current bridgeable comment: an orphan.
export function deriveBridgeRows(
  comments: readonly CommentOut[],
  todoFamily: readonly string[],
  claimAnnotations: readonly AnnotationView[],
): BridgeRow[] {
  const bridgeable = comments.filter((c) => isBridgeable(c, todoFamily));
  const byLine = new Map<number, CommentOut>();
  for (const c of bridgeable) byLine.set(c.line_start, c);

  const rows: BridgeRow[] = [];
  const matchedLines = new Set<number>();
  for (const a of claimAnnotations) {
    if (a.parent_id !== null) continue; // a reply is never itself a claim
    const c = byLine.get(a.line) ?? null;
    if (c) matchedLines.add(c.line_start);
    const state: BridgeState = a.resolved ? "resolved" : c ? "tracked" : "gone";
    rows.push({ state, comment: c, annotation: a });
  }
  for (const c of bridgeable) {
    if (matchedLines.has(c.line_start)) continue;
    rows.push({ state: "open", comment: c, annotation: null });
  }
  return rows;
}

/// The body `POST /api/annotations` gets for a NEW claim annotation —
/// the comment's own text plus its smart_todo fields rendered as plain
/// `key: value` lines, so the annotation is legible with no back-reference
/// to a comment row that may itself move or vanish.
export function claimAnnotationBody(c: CommentOut): string {
  const lines = [c.text];
  if (c.fields && Object.keys(c.fields.raw).length > 0) {
    lines.push("");
    for (const [k, v] of Object.entries(c.fields.raw)) lines.push(`${k}: ${v}`);
  }
  return lines.join("\n");
}

// --- the `~comments` dashboard ----------------------------------------------

/// Mirrors `comments::drift::ACTIONABLE_STATES` — the dashboard's DEFAULT
/// slice, and the exact three lanes `kb-code comments audit` prints. Kept
/// as its own small mirror (not read off any wire — no response carries
/// this list) rather than a fourth `GET /api/comments/summary` round trip
/// just to fetch three literals; a drift here would surface immediately as
/// a wrong dashboard default, not a silent divergence.
export const ACTIONABLE_STATES: readonly string[] = ["drifted", "aged", "unreasoned"];

export interface CommentsDashboardFilters {
  kind: string | null;
  keyword: string | null;
  /// `null` = the actionable default (three lanes); a specific value here
  /// means the "show everything" facet picked ONE state to view instead.
  state: string | null;
  pathPrefix: string | null;
}

export const DEFAULT_DASHBOARD_FILTERS: CommentsDashboardFilters = {
  kind: null,
  keyword: null,
  state: null,
  pathPrefix: null,
};

/// The exact `FetchCommentsParams`-shaped query object for one lane of the
/// dashboard (`state` here is a SINGLE value, never the whole actionable
/// set — the dashboard's default view calls this once per entry of
/// [`ACTIONABLE_STATES`], the "show everything" toggle calls it once with
/// `state: null`). Server paging always: this never returns more than the
/// caller's own `limit`/`offset` for the SPA to slice further.
export function dashboardQueryParams(
  repo: string,
  filters: CommentsDashboardFilters,
  state: string | null,
  limit: number,
  offset = 0,
): { repo: string; path?: string; kind?: string; keyword?: string; state?: string; limit: number; offset: number } {
  return {
    repo,
    ...(filters.pathPrefix ? { path: filters.pathPrefix } : {}),
    ...(filters.kind ? { kind: filters.kind } : {}),
    ...(filters.keyword ? { keyword: filters.keyword } : {}),
    ...(state ? { state } : {}),
    limit,
    offset,
  };
}

/// Stable display order for the dashboard's per-state groups — actionable
/// lanes first (in `ACTIONABLE_STATES` order), then the rest.
export const DASHBOARD_STATE_ORDER: readonly string[] = [
  "drifted",
  "aged",
  "unreasoned",
  "unknown",
  "fresh",
  "none",
];

export function sortStatesForDashboard(states: readonly string[]): string[] {
  return [...states].sort((a, b) => {
    const ia = DASHBOARD_STATE_ORDER.indexOf(a);
    const ib = DASHBOARD_STATE_ORDER.indexOf(b);
    if (ia === -1 && ib === -1) return a.localeCompare(b);
    if (ia === -1) return 1;
    if (ib === -1) return -1;
    return ia - ib;
  });
}

/// Group a page of rows by `state.state` — the dashboard's per-state
/// sections. Preserves each row's server-given order within its group.
export function groupCommentsByState(comments: readonly CommentOut[]): Map<string, CommentOut[]> {
  const map = new Map<string, CommentOut[]>();
  for (const c of comments) {
    const list = map.get(c.state.state);
    if (list) list.push(c);
    else map.set(c.state.state, [c]);
  }
  return map;
}
