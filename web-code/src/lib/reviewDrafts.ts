// V73-K2a — draft-as-you-go for the review diff (design §D9: "draft-as-you-go
// with atomic publish").
//
// THE SHAPE, and why it is this one:
//
// A reviewer reading a 40-file patchset writes eight comments before they
// know what the ninth is. Today each one POSTs the instant it is typed, so
// a half-formed thought is already visible to the agent, a second thought
// needs a delete, and there is no moment where the reviewer decides "this
// is my review." Diff v2 makes composition LOCAL and publication ONE
// TRANSACTION: every comment/question typed in the diff lands here, is
// badged as a draft on screen (never mistakable for a landed thread), and
// `Publish` sends the whole tray as ONE `POST /api/annotations/batch` —
// which `routes.rs::batch_annotations` applies inside a single store
// transaction and announces with at most ONE `annotation.changed` SSE.
// All or nothing: a batch that fails writes nothing and the tray is
// untouched, so a failed publish never leaves half a review on the server
// and half in the browser.
//
// THREE RULES, each with a home:
//
//   1. **A draft is visibly a draft.** `DraftsTray` and every gutter badge
//      read `isDraft`; nothing in this module ever hands a draft to a
//      renderer that would show it as a landed comment.
//   2. **Drafts are BROWSER state and say so.** They live in
//      `sessionStorage` under `draftsStorageKey(repo, reviewId)` — a
//      recorded CLI-parity exemption of exactly the kind
//      `lib/searchHistory.ts` already carries ("History and saved searches
//      never leave the browser"): there is no `review_drafts` table and no
//      `kb-code review draft` verb, because an unpublished draft is not a
//      fact about the review yet. `sessionStorage` rather than
//      `localStorage` on purpose — a draft that outlived the tab would
//      resurface weeks later against a patchset that no longer exists.
//      A reload restores them (the brief's own test); closing the tab
//      discards them, which is what "session" means and what the tray
//      says.
//   3. **This module is pure over an injected `StorageLike`** (the
//      `desk/deskState.ts` shape), so every branch — including a corrupt
//      blob — is exercised by the node-environment unit suite with no DOM.
//
// FINDINGS ARE DELIBERATELY NOT DRAFTED, and the reason is a gate, not an
// oversight. `POST /api/annotations/batch` is an ORDINARY bearer route;
// creating a finding (`POST /api/reviews/{id}/findings`) rides
// `review_gate::review_mutations_gate` — `[review] remote_mutations`,
// default OFF, 404 to a non-loopback caller. Adding an `add_finding` op to
// the batch would graduate finding creation off that gate as a side
// effect of a UI feature, which is precisely the class of change
// `crates/kb-code-server/CLAUDE.md` exists to stop. So the composer's
// finding tab keeps posting immediately through its own gated route, and
// says so on screen. Folding findings into the atomic publish is a real
// piece of work — a gated batch op — and it is named in the docs as such
// rather than smuggled in here.

import type { AnnotationBatchAddCommentOp, AnnotationsBatchRequest } from "../api/client";
import type { AnnotationIntent } from "../api/types";
import type { StorageLike } from "../desk/deskState";

/// Bumped only when a shape change cannot be forward-migrated silently;
/// `loadDrafts` treats any other version as "no drafts" rather than
/// guessing at an older shape.
export const REVIEW_DRAFTS_VERSION = 1;

/// A draft's own intent — the two composer modes the atomic batch can
/// carry. `"question"` is what makes the landed thread show the awaiting-
/// agent chip (`lib/questionState.ts`); `"comment"` is the plain thread.
///
/// This is the DRAFT vocabulary, not the wire's: `annotations.intent` is
/// `note|question|todo|flag-for-agent|tour-stop` (`AnnotationIntent`), and
/// a plain review comment is a `note` there. `wireIntent` below is the one
/// place the two meet — a second hand-typed `"note"` somewhere else is
/// exactly how a vocabulary drifts.
export type DraftIntent = "comment" | "question";

export function wireIntent(intent: DraftIntent): AnnotationIntent {
  return intent === "question" ? "question" : "note";
}

export interface ReviewDraft {
  /// Client-minted, stable for the draft's whole life — the tray's React
  /// key, the discard target, and what makes `upsertDraft` an edit rather
  /// than a duplicate.
  id: string;
  path: string;
  side: "old" | "new";
  line: number;
  lineEnd?: number;
  intent: DraftIntent;
  body: string;
  /// V4.S2's suggestion replacement, carried on the SAME op
  /// (`AddComment.suggestion`) the server already accepts — a suggestion
  /// draft is a comment draft with a replacement, never a second entity.
  suggestion?: string;
  /// Unix ms, client clock — display order only, never a server fact.
  createdAt: number;
}

export interface DraftsState {
  v: number;
  drafts: ReviewDraft[];
}

export const EMPTY_DRAFTS: DraftsState = { v: REVIEW_DRAFTS_VERSION, drafts: [] };

/// Per repo AND per review: two reviews open in two tabs must not share a
/// tray, and a repo-wide key would collide the moment they did.
export function draftsStorageKey(repo: string, reviewId: number): string {
  return `kbc:review-drafts:${repo}:${reviewId}`;
}

function defaultStorage(): StorageLike | null {
  try {
    return typeof sessionStorage === "undefined" ? null : sessionStorage;
  } catch {
    // A privacy mode that throws on access — no drafts, never a crash.
    return null;
  }
}

/// TOTAL: a missing key, unparseable JSON, a wrong `v`, or a rows array
/// carrying anything that is not a well-formed draft all degrade to the
/// empty tray. A partially-valid blob keeps its valid rows — losing seven
/// good drafts because an eighth is malformed would be the worse failure.
export function loadDrafts(
  repo: string,
  reviewId: number,
  storage: StorageLike | null = defaultStorage(),
): DraftsState {
  if (!storage) return EMPTY_DRAFTS;
  let raw: string | null = null;
  try {
    raw = storage.getItem(draftsStorageKey(repo, reviewId));
  } catch {
    return EMPTY_DRAFTS;
  }
  if (!raw) return EMPTY_DRAFTS;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return EMPTY_DRAFTS;
  }
  if (typeof parsed !== "object" || parsed === null) return EMPTY_DRAFTS;
  const blob = parsed as Partial<DraftsState>;
  if (blob.v !== REVIEW_DRAFTS_VERSION || !Array.isArray(blob.drafts)) return EMPTY_DRAFTS;
  const drafts = blob.drafts.filter(isDraft);
  return { v: REVIEW_DRAFTS_VERSION, drafts };
}

function isDraft(v: unknown): v is ReviewDraft {
  if (typeof v !== "object" || v === null) return false;
  const d = v as Partial<ReviewDraft>;
  return (
    typeof d.id === "string" &&
    d.id !== "" &&
    typeof d.path === "string" &&
    d.path !== "" &&
    (d.side === "old" || d.side === "new") &&
    typeof d.line === "number" &&
    Number.isInteger(d.line) &&
    d.line > 0 &&
    (d.intent === "comment" || d.intent === "question") &&
    typeof d.body === "string" &&
    typeof d.createdAt === "number"
  );
}

export function saveDrafts(
  repo: string,
  reviewId: number,
  state: DraftsState,
  storage: StorageLike | null = defaultStorage(),
): void {
  if (!storage) return;
  const key = draftsStorageKey(repo, reviewId);
  try {
    if (state.drafts.length === 0) storage.removeItem(key);
    else storage.setItem(key, JSON.stringify({ v: REVIEW_DRAFTS_VERSION, drafts: state.drafts }));
  } catch {
    // A full/blocked quota loses persistence, never the in-memory tray.
  }
}

/// Insert or replace by `id`, preserving position on an edit (the tray
/// must not reshuffle under an operator's cursor) and appending on a
/// create.
export function upsertDraft(state: DraftsState, draft: ReviewDraft): DraftsState {
  const i = state.drafts.findIndex((d) => d.id === draft.id);
  if (i === -1) return { ...state, drafts: [...state.drafts, draft] };
  const drafts = state.drafts.slice();
  drafts[i] = draft;
  return { ...state, drafts };
}

export function removeDraft(state: DraftsState, id: string): DraftsState {
  return { ...state, drafts: state.drafts.filter((d) => d.id !== id) };
}

export function clearDrafts(state: DraftsState): DraftsState {
  return { ...state, drafts: [] };
}

/// How many drafts sit on one file — the map column's own draft chip.
export function draftCountByPath(state: DraftsState): Map<string, number> {
  const m = new Map<string, number>();
  for (const d of state.drafts) m.set(d.path, (m.get(d.path) ?? 0) + 1);
  return m;
}

/// Drafts anchored inside `[start, end]` on `side` — the per-hunk draft
/// badge. Same span arithmetic `hunkHasThreads` uses for landed threads,
/// deliberately kept separate so a draft is never counted as a thread.
export function draftsInSpan(
  state: DraftsState,
  path: string,
  side: "old" | "new",
  start: number,
  end: number,
): ReviewDraft[] {
  return state.drafts.filter(
    (d) => d.path === path && d.side === side && d.line >= start && d.line <= end,
  );
}

/// A short, collision-resistant client id. `crypto.randomUUID` where the
/// browser has it, else a time+random fallback — this is a React key and a
/// tray handle, never a server identifier.
export function newDraftId(): string {
  try {
    if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
      return `d-${crypto.randomUUID().slice(0, 12)}`;
    }
  } catch {
    // fall through
  }
  return `d-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

/// THE PUBLISH SHAPE. One `AddComment` op per draft, in tray order, on the
/// ONE batch route (`routes.rs::batch_annotations`: one store transaction,
/// at most one `annotation.changed` SSE). `created_ids[i]` corresponds to
/// `ops[i]` — every op here is an `AddComment`, which is exactly the
/// condition `AnnotationsBatchResult`'s own doc states for that
/// correspondence to hold.
///
/// `ps` is sent as a NUMBER or omitted: the wire's `ps` is `Option<i64>`
/// and the server defaults an absent one to the review's latest patchset,
/// which is the same thing `"latest"` means on this page. Sending the
/// string would 400 the whole batch.
export function draftsToBatch(
  repo: string,
  reviewId: number,
  ps: number | null,
  drafts: readonly ReviewDraft[],
): AnnotationsBatchRequest {
  const ops: AnnotationBatchAddCommentOp[] = drafts.map((d) => {
    const op: AnnotationBatchAddCommentOp = {
      op: "add_comment",
      path: d.path,
      line: d.line,
      body: d.body,
      anchor_kind: d.lineEnd !== undefined && d.lineEnd > d.line ? "range" : "line",
      intent: wireIntent(d.intent),
      review_id: reviewId,
      side: d.side,
    };
    if (d.lineEnd !== undefined && d.lineEnd > d.line) op.line_end = d.lineEnd;
    if (ps !== null) op.ps = ps;
    if (d.suggestion !== undefined) op.suggestion = { replacement: d.suggestion };
    return op;
  });
  return { repo, ops };
}

/// `MAX_ANNOTATION_BATCH_OPS` on the server (`routes.rs:3573`). Mirrored
/// here so the tray can REFUSE loudly at compose time rather than have the
/// whole publish 400 after the reviewer has written 120 comments — the
/// same "never a silent truncation" posture root CLAUDE.md #35 states for
/// `?ids=`'s own cap.
export const MAX_PUBLISH_OPS = 100;

/// `null` = publishable. Otherwise the sentence the Publish button's
/// disabled state shows, naming the number and the cap.
export function publishRefusal(drafts: readonly ReviewDraft[]): string | null {
  if (drafts.length === 0) return "no drafts to publish";
  if (drafts.length > MAX_PUBLISH_OPS) {
    return `${drafts.length} drafts exceeds the ${MAX_PUBLISH_OPS}-op batch limit — publish some first`;
  }
  const empty = drafts.filter((d) => d.body.trim() === "").length;
  if (empty > 0) return `${empty} draft(s) have an empty body`;
  return null;
}
