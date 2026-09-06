// Pure helpers for the annotations panel + diff comment strip (W4.6, Phase D
// adds anchor kinds/threads/intents): the composer's payload shapes, thread
// grouping, intent/anchor-kind labeling, and the gutter's line→annotations
// grouping. Kept free of React/CM6 so the composer's "what did we just send"
// contract is testable without a network mock — every `.tsx` in this crate
// has no unit-test harness of its own (`vitest.config.ts` only globs
// `src/**/*.test.ts`), so this file is where that coverage has to live.

import type { AnchorKind, AnnotationIntent, AnnotationView } from "../api/types";
import type { CreateAnnotationInput } from "../api/client";

/// A top-level composer's in-progress state, validated + shaped into the
/// exact `POST /api/annotations` body by [`buildCreatePayload`]. Mirrors
/// `routes::CreateAnnotationBody`'s per-kind requirements one-to-one:
/// `range` needs `lineEnd`, `diff` needs `sha`, neither `line`/`symbol` need
/// anything beyond `line`.
export interface ComposerDraft {
  repo: string;
  path: string;
  line: number;
  /// `range` only.
  lineEnd?: number;
  body: string;
  /// Defaults to `"line"`.
  anchorKind?: AnchorKind;
  /// Defaults to `"note"` (also the server's own default — omitted from
  /// the wire body when it's the default, so an older daemon that predates
  /// intents entirely still accepts the request).
  intent?: AnnotationIntent;
  /// `diff` only.
  sha?: string;
  author?: string;
}

/// Build the exact `POST /api/annotations` body for a TOP-LEVEL annotation
/// (`routes::CreateAnnotationBody`), or `null` when there's nothing worth
/// sending / the draft is incomplete for its own `anchorKind`: an empty/
/// whitespace-only body, a non-positive `line`, a `range` missing (or
/// non-positive) `lineEnd`, or a `diff` missing `sha`. The route itself
/// would happily 400 on most of these (it does no server-side trimming of
/// `body` either), but a composer offering to save something incomplete is
/// a UI bug, not a valid request — this is where that guard lives so every
/// composer's Save button can just disable on `null`.
export function buildCreatePayload(draft: ComposerDraft): CreateAnnotationInput | null {
  const body = draft.body.trim();
  if (!body || draft.line < 1) return null;
  const anchorKind = draft.anchorKind ?? "line";
  if (anchorKind === "range" && (draft.lineEnd === undefined || draft.lineEnd < 1)) return null;
  if (anchorKind === "diff" && !draft.sha) return null;

  const payload: CreateAnnotationInput = { repo: draft.repo, path: draft.path, line: draft.line, body };
  if (draft.author) payload.author = draft.author;
  if (anchorKind !== "line") payload.anchor_kind = anchorKind;
  if (anchorKind === "range") payload.line_end = draft.lineEnd;
  if (anchorKind === "diff") payload.sha = draft.sha;
  if (draft.intent && draft.intent !== "note") payload.intent = draft.intent;
  return payload;
}

/// Build the exact `POST /api/annotations` body for a REPLY (`parent_id`
/// set — no anchor, no `line`), or `null` for an empty/whitespace-only
/// body. Same "guard lives here, not repeated per composer" rationale as
/// [`buildCreatePayload`].
export function buildReplyPayload(
  repo: string,
  path: string,
  parentId: string,
  rawBody: string,
): CreateAnnotationInput | null {
  const body = rawBody.trim();
  if (!body) return null;
  return { repo, path, parent_id: parentId, body };
}

/// Line-ascending, `created_at`-ascending on ties — the panel's TOP-LEVEL
/// list order (replies are ordered within their own thread by
/// [`groupThreads`] instead, oldest-first, conversational order).
export function sortAnnotations(annotations: AnnotationView[]): AnnotationView[] {
  return [...annotations].sort((a, b) => a.line - b.line || a.created_at - b.created_at);
}

/// One top-level annotation plus its direct replies (oldest-first) — the
/// shape `GET /api/annotations`'s flat parent+reply list needs regrouped
/// into before a panel can render "replies indented one level under
/// parents" (a reply's own `line` mirrors its parent's resolved line, so a
/// naive `sortAnnotations` over the flat list would interleave a thread's
/// replies with unrelated parents that happen to share that line).
export interface AnnotationThread {
  parent: AnnotationView;
  replies: AnnotationView[];
}

/// Group a flat `(parents + replies)` list — exactly what `GET /api/
/// annotations`/the diff-comment strip's client-filter both hand over —
/// into per-parent threads, parents ordered the same way [`sortAnnotations`]
/// orders a flat list, replies within each thread oldest-first. A reply
/// whose `parent_id` doesn't match any parent IN THIS SLICE (the diff strip
/// filters by `(path, sha)` before calling this, so a reply could in theory
/// reference a parent excluded by that filter — can't happen in practice
/// since a reply always inherits its parent's repo/path and a `diff`
/// parent's `sha` is immutable, but the type is honest about the
/// possibility) is silently dropped rather than crashing.
export function groupThreads(annotations: AnnotationView[]): AnnotationThread[] {
  const parents = annotations.filter((a) => a.parent_id === null);
  const repliesByParent = new Map<string, AnnotationView[]>();
  for (const a of annotations) {
    if (a.parent_id === null) continue;
    const list = repliesByParent.get(a.parent_id);
    if (list) list.push(a);
    else repliesByParent.set(a.parent_id, [a]);
  }
  return sortAnnotations(parents).map((parent) => ({
    parent,
    replies: (repliesByParent.get(parent.id) ?? []).sort((a, b) => a.created_at - b.created_at),
  }));
}

/// Group TOP-LEVEL annotations by live-resolved `line` — the gutter's
/// marker map (one dot per line carrying 1+ THREADS; every other line gets
/// the hover-only "+" fallback, see `editor/lineGutter.ts`'s
/// `hoverFallback`). Replies are excluded — a reply's `line` always mirrors
/// its already-counted parent's, so including it would double-count the
/// same thread rather than surface a second one.
export function annotationsByLine(annotations: AnnotationView[]): Map<number, AnnotationView[]> {
  const map = new Map<number, AnnotationView[]>();
  for (const a of annotations) {
    if (a.parent_id !== null) continue;
    const existing = map.get(a.line);
    if (existing) existing.push(a);
    else map.set(a.line, [a]);
  }
  return map;
}

/// The gutter dot's hover title for one line's thread(s) — several threads
/// on one line collapse to a count (no room to show more than one body
/// preview); a single `range` thread's title is prefixed with its own
/// `L{start}–{end}` span (deliverable D5: "range annotations mark their
/// START line; the title says the range" — there is no multi-line gutter
/// painting this phase, so the span text is the only way the gutter
/// communicates a range's extent at all).
export function annotationGutterTitle(list: AnnotationView[]): string {
  if (list.length > 1) return `${list.length} annotations`;
  const a = list[0];
  const rangeLabel = a.anchor_kind === "range" && a.line_end !== null ? `L${a.line}–${a.line_end}: ` : "";
  return `${rangeLabel}${a.body.slice(0, 80)}`;
}

/// The reader inspector icon's unresolved-count badge counts PARENTS only
/// (a thread is "open" or not as a unit — an unresolved reply under a
/// resolved parent can't happen server-side, `PATCH` never lets a caller
/// resolve a reply independently of its parent through this UI, but the
/// count would be misleading either way: a reply isn't itself a separate
/// open question needing attention, its parent already is/isn't).
export function unresolvedCount(annotations: AnnotationView[]): number {
  return annotations.reduce((n, a) => (a.parent_id === null && !a.resolved ? n + 1 : n), 0);
}

const INTENT_LABELS: Record<string, string> = {
  note: "Note",
  question: "Question",
  todo: "To-do",
  "flag-for-agent": "Flag for agent",
  "tour-stop": "Tour stop",
};

/// Display label for an intent value — an unrecognized string (an older/
/// newer daemon) degrades to itself verbatim rather than throwing, matching
/// this crate's general forward-compatibility posture (see e.g.
/// `PeekPanel.tsx`'s `precision` handling).
export function intentLabel(intent: string): string {
  return INTENT_LABELS[intent] ?? intent;
}

/// The five intents in the composer's own fixed display order (matches
/// `crate::annotations::INTENTS`) — every intent `<select>` in this crate
/// iterates this rather than re-deriving the list.
export const INTENT_OPTIONS: readonly AnnotationIntent[] = ["note", "question", "todo", "flag-for-agent", "tour-stop"];

/// The anchor-kind badge's display label — `range`/`symbol` are a clickable
/// jump target (the caller decides the click handler, this is text only);
/// `diff` has no live line to jump to (see `crate::annotations::diff_line`'s
/// doc), so callers render it as a short-sha chip instead and use this only
/// for the line number portion.
export function anchorBadgeLabel(a: Pick<AnnotationView, "anchor_kind" | "line" | "line_end">): string {
  if (a.anchor_kind === "range" && a.line_end !== null) return `L${a.line}–${a.line_end}`;
  return `L${a.line}`;
}
