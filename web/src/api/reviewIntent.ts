// v0.44 P2 — operator intent on review mutations, and the tab-local refresh
// that replaces the daemon's `comments.updated` for note-only writes.
//
// The daemon refuses to let a request touch a PRIVATE note (resolve, reply,
// edit, delete, attach/detach, make public) unless it carries explicit
// operator intent — the write-side twin of `?visibility=all` on reads. The
// SPA is the operator's UI, so every review mutation it sends carries
// `X-Kb-Visibility: all`; the CLI and every agent surface never do. It is an
// intent marker, not an authorization (kb has one trust tier).
//
// A note-only write also stops emitting `comments.updated` (the frame
// would be printed to an agent by `kb push`), so the operator's OTHER tabs
// are told through a BroadcastChannel and this tab through a direct
// listener; the SSE bridge (queryClient.ts) feeds both into the same
// invalidation handler the daemon event uses.

export const OPERATOR_INTENT_HEADER = "X-Kb-Visibility";

const REVIEW_PATH = /^\/api\/kb\/([^/]+)\/review\/([^/?]+)/;

export type ReviewMutationTarget = { kb: string; artifact_id: string };

/// `{kb, artifact_id}` for a review-mutation path, or `null` for anything
/// else (the header and the announcement are review-scoped).
export function reviewMutationTarget(path: string): ReviewMutationTarget | null {
  const m = REVIEW_PATH.exec(path);
  if (!m) return null;
  return {
    kb: decodeURIComponent(m[1]),
    artifact_id: decodeURIComponent(m[2]),
  };
}

/// Add the operator-intent header to `headers` when `path` is a review path.
export function withOperatorIntent(
  path: string,
  headers: Record<string, string>,
): Record<string, string> {
  if (reviewMutationTarget(path)) headers[OPERATOR_INTENT_HEADER] = "all";
  return headers;
}

const CHANNEL = "kb-review-mutation";
type Listener = (p: ReviewMutationTarget) => void;
const listeners = new Set<Listener>();
let channel: BroadcastChannel | null | undefined;

function getChannel(): BroadcastChannel | null {
  if (channel === undefined) {
    channel =
      typeof BroadcastChannel === "undefined" ? null : new BroadcastChannel(CHANNEL);
  }
  return channel;
}

/// Tell this tab and every other tab that a review changed. Call after a
/// SUCCESSFUL review mutation.
export function announceReviewMutation(path: string): void {
  const target = reviewMutationTarget(path);
  if (!target) return;
  for (const l of listeners) l(target);
  getChannel()?.postMessage(target);
}

/// Subscribe to review-mutation announcements from this tab and others.
/// Returns the unsubscribe.
export function onReviewMutation(fn: Listener): () => void {
  listeners.add(fn);
  const ch = getChannel();
  const onMessage = (e: MessageEvent) => {
    const d = e.data as Partial<ReviewMutationTarget> | null;
    if (d && typeof d.kb === "string" && typeof d.artifact_id === "string") {
      fn({ kb: d.kb, artifact_id: d.artifact_id });
    }
  };
  ch?.addEventListener("message", onMessage);
  return () => {
    listeners.delete(fn);
    ch?.removeEventListener("message", onMessage);
  };
}
