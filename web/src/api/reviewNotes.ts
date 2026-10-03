// v0.40 TN — client for GET /api/review-notes (the tag-filtered private-note
// browser) and PATCH …/review/{id}/comments/{cid}/meta (the SPA's write path
// for an EXISTING comment's tags / private flag — a new comment carries both
// on its own create request instead; see `patchCommentMeta`).
//
// Self-contained problem+json helpers, mirroring api/notes.ts /
// api/sessions.ts rather than reaching into client.ts's private ones.
//
// SCOPE, stated so nobody widens it by accident: these are the OPERATOR's
// reads. A private comment is invisible to every agent-facing surface by
// construction, and this index is one of the three places (the two
// `?visibility=all` route reads, and here) where it is visible AT ALL —
// this one because it lists notes and nothing else.
//
// `/review-notes` (SPA) = private comments in `.review/*.json`;
// `/notes` (SPA) = kb note artifacts. Different entities, both kebab-cased
// with "notes" in the name — don't let the two drift together.

import { announceReviewMutation, withOperatorIntent } from "./reviewIntent";
import { currentDaemonBase } from "./base";
// Wire types are the generated ts-rs bindings (routes/review_notes.rs is the
// source of truth; `just types` regenerates).
import type { ReviewNoteRow } from "./generated/ReviewNoteRow";
import type { ReviewNotesResponse } from "./generated/ReviewNotesResponse";
import type { TagSummary } from "./generated/TagSummary";

export type { ReviewNoteRow, ReviewNotesResponse, TagSummary };

/// `status` is tri-state on the wire but its DEFAULT is `all` here (a note
/// browser wants resolved notes — the opposite of /reviews, whose default is
/// `open`), so "all" is the value that serialises out of the URL entirely.
export type ReviewNoteStatus = "open" | "resolved" | "all";

export type ReviewNotesQuery = {
  /// Absent = every kb (fleet-wide, like /inbox).
  kb?: string;
  /// Repeatable on the wire as `tag=`, ANDed server-side. The client sends
  /// them verbatim; slug normalisation is the server's single normaliser
  /// (`normalize_comment_tags`) and re-deriving it here would let the two
  /// drift.
  tags?: readonly string[];
  /// Case-insensitive body substring.
  q?: string;
  status?: ReviewNoteStatus;
  /// `false` asks for rows WITHOUT the note bodies (a count/label-only
  /// listing). The ONE axis passed through verbatim instead of being
  /// default-dropped like `status`: it is a shape switch, not a filter, so
  /// a bare `bodies=true` can't "look applied and isn't" — and only the
  /// caller knows which shape it needs. Undefined → the param is absent
  /// and the server's own default stands.
  bodies?: boolean;
};

export type CommentMetaPatch = {
  /// FULL replace. Read-merge-write clients race every other writer, so the
  /// tag editor sends the deltas below instead (like the CLI).
  tags?: string[];
  /// DELTA: union into the comment's CURRENT tags, server-side, under the lock.
  add_tags?: string[];
  /// DELTA: drop these (slug-matched) from the CURRENT tags.
  remove_tags?: string[];
  private?: boolean;
};

/// The tag-editor delta between a comment's current tags and the edited
/// list: `add_tags` = typed tags the comment lacks, `remove_tags` = current
/// tags no longer typed. Matching is case-insensitive on the raw strings
/// (the daemon's `normalize_comment_tags` stays the one slugifier; this only
/// avoids sending a remove+add pair for a tag that merely changed case).
/// `add_tags` is always present (possibly empty) so an unchanged editor is
/// still a valid, no-op PATCH rather than the daemon's "set at least one" 400.
export function tagDelta(
  current: readonly string[],
  draft: readonly string[],
): { add_tags: string[]; remove_tags?: string[] } {
  const key = (t: string) => t.trim().toLowerCase();
  const have = new Set(current.map(key));
  const want = new Set(draft.map(key));
  const seen = new Set<string>();
  const add_tags: string[] = [];
  for (const t of draft) {
    const k = key(t);
    if (!have.has(k) && !seen.has(k)) {
      seen.add(k);
      add_tags.push(t);
    }
  }
  const remove_tags = current.filter((t) => !want.has(key(t)));
  return remove_tags.length > 0 ? { add_tags, remove_tags } : { add_tags };
}

/// The EFFECTIVE values after normalisation (`{"ok":true,"changed":bool,
/// "tags":[…],"private":bool}`). The SPA never re-derives slug/dedupe/sort
/// locally — it renders exactly what came back, so the editor can't disagree
/// with the sidecar. `changed:false` is the documented no-op (G8: no save,
/// no `comments.updated`), and is NOT an error.
export type CommentMetaResult = {
  ok: boolean;
  changed: boolean;
  tags: string[];
  private: boolean;
  /// v0.44 P2 (A2.f4) — present when this comment is now a note but was
  /// earlier kept as a memory (`keep_memory`): the memory is a separate
  /// artifact that stays recalled into agent prompts. Never a visibility
  /// switch — the daemon only reports it.
  kept_as_memory?: { id: string };
};

type Problem = { title: string; detail?: string };

async function problem(r: Response): Promise<Error> {
  const ct = r.headers.get("content-type") ?? "";
  if (ct.includes("application/problem+json")) {
    const p = (await r.json()) as Problem;
    return new Error(`${p.title}: ${p.detail ?? ""}`);
  }
  return new Error(`${r.status} ${r.statusText}`);
}

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    headers: { Accept: "application/json" },
    signal,
  });
  if (!r.ok) throw await problem(r);
  return (await r.json()) as T;
}

async function mutateJson<T>(
  path: string,
  method: "POST" | "PATCH" | "DELETE",
  body?: unknown,
): Promise<T> {
  const headers: Record<string, string> = {
    Accept: "application/json",
    "X-Requested-By": "kb-spa",
  };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  withOperatorIntent(path, headers);
  const r = await fetch(`${currentDaemonBase()}${path}`, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) throw await problem(r);
  // A tag / private edit on a note emits no daemon event any more (it would
  // be printed to an agent), so tell this tab and the others directly.
  announceReviewMutation(path);
  return (await r.json()) as T;
}

/// The query grammar, defaults dropped: `kb` when blank, every `tag` (ANDed,
/// one param per value), `q` when blank, `status` ONLY when it isn't the
/// `all` default, and `bodies` only when the caller set it explicitly — so
/// the browser's URL and the request stay one string and a default-out axis
/// can never ride the wire.
function notesQuery(q: ReviewNotesQuery = {}): string {
  const p = new URLSearchParams();
  if (q.kb) p.set("kb", q.kb);
  for (const t of q.tags ?? []) {
    if (t) p.append("tag", t);
  }
  if (q.q && q.q.trim()) p.set("q", q.q.trim());
  if (q.status && q.status !== "all") p.set("status", q.status);
  if (q.bodies !== undefined) p.set("bodies", String(q.bodies));
  const qs = p.toString();
  return qs ? `?${qs}` : "";
}

export const fetchReviewNotes = (
  q: ReviewNotesQuery = {},
  signal?: AbortSignal,
): Promise<ReviewNotesResponse> =>
  getJson<ReviewNotesResponse>(`/api/review-notes${notesQuery(q)}`, signal);

const reviewBase = (kb: string, artifactId: string) =>
  `/api/kb/${encodeURIComponent(kb)}/review/${encodeURIComponent(artifactId)}`;

/// PATCH …/comments/{cid}/meta — set an EXISTING comment's tags and/or its
/// private flag. At least one field must be set (the daemon 400s on
/// neither), and a patch that matches what's already stored is a documented
/// no-op rather than a write. A NEW comment carries both on its own create
/// (`api/client.ts`'s `addComment`) — creating it public and patching after
/// would leave the note's EXISTENCE in the history ledger's per-day count,
/// which is agent-readable. This is the only other way the SPA sets the
/// flag: an agent-reachable CLI verb would be a second one, and the whole
/// point of the flag is that the agent has none.
export const patchCommentMeta = (
  kb: string,
  artifactId: string,
  commentId: string,
  patch: CommentMetaPatch,
): Promise<CommentMetaResult> =>
  mutateJson<CommentMetaResult>(
    `${reviewBase(kb, artifactId)}/comments/${encodeURIComponent(commentId)}/meta`,
    "PATCH",
    patch,
  );
