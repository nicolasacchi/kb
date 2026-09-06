// Pure helpers for V4.C4 review-scoped comment threads. The GET
// `/api/reviews/{id}/comments` response is already nested (parent +
// replies) and already resolved against a target patchset — this file
// only indexes that payload for the two renderers (by-line vs orphan
// foot) and shapes the POST body a review composer sends. Same "guard
// lives here, not repeated per composer" rationale as `annotations.ts`.

import type { CreateAnnotationInput } from "../api/client";
import type { ApplySuggestionOut, ReviewComment, ReviewCommentsOut, ReviewFinding } from "../api/types";
import { buildCreatePayload, type ComposerDraft } from "./annotations";
import type { FindingComposerDraft, FindingDisposition, OverlayMode } from "./diffFindings";

export type DiffSide = "old" | "new";

/// `${path}|${side}|${line}` — the DiffFile by-line lookup key.
export function threadLineKey(path: string, side: DiffSide, line: number): string {
  return `${path}|${side}|${line}`;
}

export function commentSide(comment: Pick<ReviewComment, "side">): DiffSide {
  return comment.side === "old" ? "old" : "new";
}

export interface FileThreadRollup {
  open: number;
  total: number;
}

export interface IndexedThreads {
  byLine: Map<string, ReviewComment[]>;
  orphansByPath: Map<string, ReviewComment[]>;
  rollup: {
    perFile: Map<string, FileThreadRollup>;
    open: number;
  };
}

export const EMPTY_INDEX: IndexedThreads = {
  byLine: new Map(),
  orphansByPath: new Map(),
  rollup: { perFile: new Map(), open: 0 },
};

function pushMap<K, V>(map: Map<K, V[]>, key: K, value: V): void {
  const list = map.get(key);
  if (list) list.push(value);
  else map.set(key, [value]);
}

/// Index a `review-comments/1` response. Orphaned threads
/// (`resolution.orphaned`) go to `orphansByPath` and NEVER to `byLine`.
/// Resolved parents count in `total` but not `open`.
export function indexThreads(response: ReviewCommentsOut): IndexedThreads {
  const byLine = new Map<string, ReviewComment[]>();
  const orphansByPath = new Map<string, ReviewComment[]>();
  const perFile = new Map<string, FileThreadRollup>();
  let open = 0;

  for (const group of response.groups) {
    const roll: FileThreadRollup = { open: 0, total: 0 };
    for (const comment of group.comments) {
      roll.total += 1;
      if (!comment.resolved) {
        roll.open += 1;
        open += 1;
      }
      if (comment.resolution.orphaned) {
        pushMap(orphansByPath, group.path, comment);
        continue;
      }
      const line = comment.resolution.line;
      if (line == null || line < 1) continue;
      pushMap(byLine, threadLineKey(group.path, commentSide(comment), line), comment);
    }
    if (roll.total > 0) perFile.set(group.path, roll);
  }

  return { byLine, orphansByPath, rollup: { perFile, open } };
}

export function findThread(response: ReviewCommentsOut, id: string): ReviewComment | null {
  for (const group of response.groups) {
    const hit = group.comments.find((c) => c.id === id);
    if (hit) return hit;
  }
  return null;
}

/// A top-level review composer's draft: `ComposerDraft` plus the review
/// scope (`reviewId` / optional `ps` / `side`). Anchor kinds other than
/// `line`/`range` are rejected — review comments pin to a patchset blob,
/// never a live working-tree `diff`/`symbol` kind.
export interface ReviewComposerDraft extends ComposerDraft {
  reviewId: number;
  ps?: number | string;
  side?: DiffSide;
}

/// Build the `POST /api/annotations` body for a review-scoped top-level
/// comment (`line`/`range` only), or `null` when the draft is incomplete
/// — same discipline as [`buildCreatePayload`].
export function buildReviewCommentPayload(draft: ReviewComposerDraft): CreateAnnotationInput | null {
  const kind = draft.anchorKind ?? "line";
  if (kind !== "line" && kind !== "range") return null;
  const base = buildCreatePayload({ ...draft, sha: undefined, anchorKind: kind });
  if (!base) return null;
  const payload: CreateAnnotationInput = { ...base, review_id: draft.reviewId };
  if (draft.ps !== undefined && draft.ps !== "" && draft.ps !== "latest") {
    const n = typeof draft.ps === "number" ? draft.ps : Number(draft.ps);
    if (Number.isFinite(n) && n > 0) payload.ps = n;
  }
  if (draft.side === "old") payload.side = "old";
  return payload;
}

/// The callbacks + lookup a `DiffFile` needs to render review threads
/// and compose on both sides. Path is implicit (the file being rendered).
export interface DiffCommentsApi {
  reviewId: number;
  ps: string | number;
  byLine: Map<string, ReviewComment[]>;
  orphansByPath: Map<string, ReviewComment[]>;
  onCreate: (
    side: DiffSide,
    line: number,
    lineEnd: number | undefined,
    body: string,
    intent: string,
  ) => Promise<void>;
  /// PRR-U3 (addendum §E) — `POST /api/reviews/{id}/findings`, a manual
  /// finding at the composer's own line/side. `draft` omits `path`/`side`/
  /// `line` (the caller — `UnifiedHunks`/`SplitHunks` — already has them in
  /// closure); see `buildManualFindingPayload` for the shape this builds.
  onCreateFinding: (
    side: DiffSide,
    line: number,
    draft: Omit<FindingComposerDraft, "path" | "side" | "line">,
  ) => Promise<void>;
  onReply: (id: string, body: string) => Promise<void>;
  onResolve: (id: string, resolved: boolean) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
  /// V4.S2 — persist / drop / apply a suggestion on this thread.
  onSetSuggestion: (id: string, replacement: string) => Promise<void>;
  onClearSuggestion: (id: string) => Promise<void>;
  onApplySuggestion: (id: string, resolve: boolean) => Promise<ApplySuggestionOut>;
  /// Keyboard / deep-link compose request — the renderer opens ComposerV2.
  /// `token` changes on each request so a re-press of `c` reopens even
  /// after the user cancelled.
  compose?: { side: DiffSide; line: number; token?: number } | null;
  /// Force-expand + flash this thread id (`?thread=`).
  flashThreadId?: string | null;
  /// PRR-U3 — findings joined to their thread's annotation id
  /// (`lib/diffFindings.ts`'s `findingsByAnnotationId`). Empty on any
  /// caller that predates the findings overlay (Commit/Compare never sets
  /// `comments` at all, so this is only reachable from review-diff callers).
  findingsById: ReadonlyMap<string, ReviewFinding>;
  /// The active overlay filter (`?overlay=`, default `"all"`).
  overlay: OverlayMode;
  /// `PUT`/`DELETE /api/reviews/{id}/findings/{slug}/disposition`.
  onSetDisposition: (slug: string, disposition: FindingDisposition) => Promise<void>;
  onClearDisposition: (slug: string) => Promise<void>;
}

export function threadsAt(
  comments: DiffCommentsApi,
  path: string,
  side: DiffSide,
  line: number,
): ReviewComment[] {
  return comments.byLine.get(threadLineKey(path, side, line)) ?? [];
}

export function orphansAt(comments: DiffCommentsApi, path: string): ReviewComment[] {
  return comments.orphansByPath.get(path) ?? [];
}

// ── PRR-U4 (§4 — the question/answer loop) ──────────────────────────────
//
// The side panel's "Ask the agent" card posts a REVIEW-LEVEL question: no
// file, no line — `anchor_kind: "review"`, `path: ""` (PRR-R3 design
// arbitration #6's path-less "general question" kind,
// `routes::assemble_top_level_annotation`'s dedicated branch, confirmed
// against `crates/kb-code-server/src/review_comments.rs`'s
// `build_comment_groups`: a `path: ""` group sorts first and renders under
// ReviewThreadsCard's "General" section). Never pins a `ps`/`side` — the
// question targets the review as a whole, not one patchset's blob, so the
// server resolves it against the review's latest patchset by default and
// it stays resolved forever (`resolve_for_ps_with_content`'s `anchor_kind
// == ANCHOR_KIND_REVIEW` branch never orphans).

export interface AskAgentDraft {
  repo: string;
  reviewId: number;
  body: string;
}

/// Build the `POST /api/annotations` body for a review-level "ask the
/// agent" question, or `null` for an empty/whitespace-only body — same
/// "guard lives here, not repeated per composer" discipline as
/// [`buildReviewCommentPayload`].
export function buildAskAgentPayload(draft: AskAgentDraft): CreateAnnotationInput | null {
  const body = draft.body.trim();
  if (!body) return null;
  return {
    repo: draft.repo,
    path: "",
    anchor_kind: "review",
    intent: "question",
    review_id: draft.reviewId,
    body,
  };
}
