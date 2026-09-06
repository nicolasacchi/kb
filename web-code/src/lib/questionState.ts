// PRR-U4 (§4 — "the question/answer loop, concretely") — pure derivation of
// a thread's question state from its own voices. No new server state: the
// design doc is explicit that this is "a pure client, no new server state"
// derivation over `intent` + who spoke last, computed fresh from whatever
// `ReviewComment`/`ReviewFinding` shape the caller already has in hand.
//
// Two independent consumers share this module:
//   - the ❓ chip (`DiffThread`, `ReviewThreadsCard` rows) + the side
//     panel's `awaiting-agent`/`awaiting-you` filter chips — [`questionChipState`].
//   - the "✳ claude replied" toast (`hooks/useReviewComments.ts`'s
//     `useAgentReplyToast`) — [`diffAgentReplies`], a before/after snapshot
//     diff over the same per-thread "latest voice" primitive.

import type { ReviewComment, ReviewCommentsOut, ReviewFinding } from "../api/types";

/// The `--ignore-author claude` convention (root CLAUDE.md invariant #29's
/// sibling doc-bridge posture, and `FindingCard.tsx`'s own doc: "author
/// strings are already reliable") — the ONLY signal available for a plain
/// `ReviewComment`/`ReviewCommentReply`, neither of which carries a
/// `ReviewFinding`-style `origin` field. Case/whitespace-insensitive so a
/// hook that trims less aggressively than the daemon still matches.
export function isAgentAuthorName(author: string): boolean {
  return author.trim().toLowerCase() === "claude";
}

export interface QuestionVoice {
  author: string;
  createdAt: number;
}

/// The most recent voice on a thread: its last reply, or (no replies) the
/// thread's own opening author. Replies are NOT assumed pre-sorted by the
/// caller — this picks the max `createdAt` defensively (a `>=` tie-break
/// prefers a later array entry over the opener, matching "the conversation
/// moved" even on a same-second race).
export function latestVoice(opener: QuestionVoice, replies: readonly QuestionVoice[]): QuestionVoice {
  let latest = opener;
  for (const r of replies) {
    if (r.createdAt >= latest.createdAt) latest = r;
  }
  return latest;
}

/// Whether the LATEST voice on a thread should read as agent-authored.
/// `findingOrigin` — given only when the latest voice IS the thread's own
/// opener (a reply is always a plain annotation row, never itself a
/// finding) — takes precedence per `FindingCard.tsx`'s `findingAuthorDisplay`
/// doc ("`origin` (NOT `author`) decides the author-mark branch"); every
/// other case falls back to [`isAgentAuthorName`].
export function voiceIsAgent(
  voice: QuestionVoice,
  isOpener: boolean,
  findingOrigin?: "import" | "manual" | null,
): boolean {
  if (isOpener && findingOrigin) return findingOrigin === "import";
  return isAgentAuthorName(voice.author);
}

export type QuestionChipState = "awaiting-agent" | "awaiting-you" | null;

export interface QuestionThreadInput {
  intent: string;
  resolved: boolean;
  opener: QuestionVoice;
  replies: readonly QuestionVoice[];
  /// Set only when this thread is a finding's own annotation
  /// (`findingsByAnnotationId(...).get(thread.id)`).
  findingOrigin?: "import" | "manual" | null;
}

/// The pure derivation design-ui.md §4 specifies:
///
/// - **`"awaiting-agent"`**: `intent === "question"` and the latest voice is
///   the asker (no reply yet, or the last reply is human) — the agent's
///   queue.
/// - **`"awaiting-you"`**: the latest voice is agent-authored AND the
///   thread is still unresolved — the mirror filter, independent of
///   `intent` (an agent finding/reply on ANY open thread is "your turn"),
///   but the two states are mutually exclusive by construction: one
///   requires the latest voice to NOT be the agent, the other requires it
///   TO be.
/// - **resolved** always short-circuits to `null` — a resolved thread
///   collapses to a one-line badge everywhere in this crate (`DiffThread`),
///   so a stale "awaiting" chip on it would be misleading rather than
///   informative.
export function questionChipState(input: QuestionThreadInput): QuestionChipState {
  if (input.resolved) return null;
  const replyCount = input.replies.length;
  const voice = latestVoice(input.opener, input.replies);
  const isOpener = replyCount === 0 || voice === input.opener;
  if (voiceIsAgent(voice, isOpener, input.findingOrigin)) return "awaiting-you";
  if (input.intent === "question") return "awaiting-agent";
  return null;
}

/// Adapt a real `ReviewComment` (+ its joined `ReviewFinding`, if any) into
/// [`questionChipState`]'s pure input shape — the one call every renderer
/// (`DiffThread`, `ReviewThreadsCard`) makes.
export function questionStateForThread(
  thread: Pick<ReviewComment, "intent" | "resolved" | "author" | "created_at" | "replies">,
  finding?: Pick<ReviewFinding, "origin"> | null,
): QuestionChipState {
  return questionChipState({
    intent: thread.intent,
    resolved: thread.resolved,
    opener: { author: thread.author, createdAt: thread.created_at },
    replies: thread.replies.map((r) => ({ author: r.author, createdAt: r.created_at })),
    findingOrigin: finding?.origin ?? null,
  });
}

export type QuestionFilterKey = "awaiting-agent" | "awaiting-you";

/// `ReviewThreadsCard`'s two new filter chips — exact-match over
/// [`questionStateForThread`], kept as its own named predicate (rather than
/// an inline `=== filter` at the call site) so the filter's meaning is
/// documented once and testable without touching the component.
export function matchesQuestionFilter(
  filter: QuestionFilterKey,
  thread: Pick<ReviewComment, "intent" | "resolved" | "author" | "created_at" | "replies">,
  finding?: Pick<ReviewFinding, "origin"> | null,
): boolean {
  return questionStateForThread(thread, finding) === filter;
}

// --- the "✳ claude replied" toast (§4) ------------------------------------

/// The toast's display label: a finding's own slug when the thread IS a
/// finding, else `"General"` for a review-level (`path === ""`) question,
/// else `path:line` (falling back to bare `path` when the thread has no
/// live line — e.g. still resolving, or an edge the resolution ladder
/// hasn't attached a line to).
export function threadToastLabel(
  thread: Pick<ReviewComment, "path" | "resolution">,
  finding?: Pick<ReviewFinding, "slug"> | null,
): string {
  if (finding) return finding.slug;
  if (thread.path === "") return "General";
  const line = thread.resolution.line;
  return line != null ? `${thread.path}:${line}` : thread.path;
}

export interface AgentReplyToastInfo {
  threadId: string;
  label: string;
}

/// Pure before/after diff over one comments snapshot: for every thread
/// whose LATEST voice is agent-authored AND is newer than what
/// `prevByThread` last recorded for that thread id, emit a toast candidate.
/// "Dedupe via author" (design doc §4) falls out of this by construction —
/// a human's own reply/question is never agent-authored, so it can never
/// produce a candidate; `prevByThread`'s per-thread `createdAt` watermark
/// additionally stops the SAME agent voice from re-toasting on a later,
/// unrelated SSE-triggered refetch. `seedOnly` skips emitting candidates
/// entirely (still returns the seeded watermarks) — the caller passes
/// `true` on the FIRST snapshot a review room sees, so opening a review
/// that already has agent replies doesn't fire a toast storm on mount.
export function diffAgentReplies(
  prevByThread: ReadonlyMap<string, number>,
  comments: ReviewCommentsOut,
  findingsByAnnotationId: ReadonlyMap<string, Pick<ReviewFinding, "slug" | "origin">>,
  seedOnly: boolean,
): { toasts: AgentReplyToastInfo[]; nextByThread: Map<string, number> } {
  const nextByThread = new Map<string, number>();
  const toasts: AgentReplyToastInfo[] = [];
  for (const group of comments.groups) {
    for (const thread of group.comments) {
      const finding = findingsByAnnotationId.get(thread.id) ?? null;
      const opener: QuestionVoice = { author: thread.author, createdAt: thread.created_at };
      const replies: QuestionVoice[] = thread.replies.map((r) => ({
        author: r.author,
        createdAt: r.created_at,
      }));
      const voice = latestVoice(opener, replies);
      const isOpener = replies.length === 0 || voice === opener;
      const agent = voiceIsAgent(voice, isOpener, finding?.origin ?? null);
      nextByThread.set(thread.id, voice.createdAt);
      if (seedOnly || !agent) continue;
      const prev = prevByThread.get(thread.id);
      if (prev !== undefined && voice.createdAt <= prev) continue;
      toasts.push({ threadId: thread.id, label: threadToastLabel(thread, finding) });
    }
  }
  return { toasts, nextByThread };
}
