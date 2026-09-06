// PRR-U4 (§4 "the question/answer loop" + S2's "Ask the agent" side-panel
// card) — a review-LEVEL question (no file, no line): `anchor_kind:
// "review"`, `path: ""` (PRR-R3 design arbitration #6,
// `lib/reviewComments.ts`'s `buildAskAgentPayload`). Posting one shows up
// in `ReviewThreadsCard`'s "General" section with the ❓ awaiting-agent
// chip, same as any other question thread.
import { useState, useSyncExternalStore } from "react";
import { ApiError } from "../../api/client";
import { useAskAgentMutation } from "../../hooks/useReviewComments";
import { toast } from "../../lib/toast";
import { Icon } from "../icons";

/// Session-wide latch, same idiom as `DiffThread.tsx`'s
/// `applyLoopbackLatched`/`dispositionLoopbackLatched`: one 404 from the
/// POST hides the Ask box (replaced by a loopback hint) for every
/// `AskAgentCard` instance for the rest of this SPA session. `POST /api/
/// annotations` itself is NOT loopback-gated today (ordinary review
/// comments/replies work bearer-authenticated per design doc §8), so this
/// is defensive rather than expected to fire — kept for consistency with
/// every other review-mutation control's degrade posture, and in case a
/// future daemon build DOES gate the review-level kind.
let askAgentLoopbackLatched = false;
const askAgentLoopbackListeners = new Set<() => void>();

function latchAskAgentLoopback() {
  if (askAgentLoopbackLatched) return;
  askAgentLoopbackLatched = true;
  for (const l of askAgentLoopbackListeners) l();
}

function useAskAgentLoopbackLatched(): boolean {
  return useSyncExternalStore(
    (cb) => {
      askAgentLoopbackListeners.add(cb);
      return () => askAgentLoopbackListeners.delete(cb);
    },
    () => askAgentLoopbackLatched,
    () => askAgentLoopbackLatched,
  );
}

/// The static agent-watch hint's copyable command (design doc §2 S2: "the
/// static CLI hint version is MUST; live beat is SHOULD" — deliberately a
/// named absence, never a spinner or a live presence poll). Exported +
/// pure so it's testable the same way this crate's other `.tsx` cards test
/// their own small pure helpers (e.g. `StalenessBanner.tsx`'s
/// `stalenessMessage`).
export function agentWatchCommand(reviewId: number): string {
  return `kb-code annotate watch --review ${reviewId} --ignore-author claude`;
}

export interface AskAgentCardProps {
  repo: string;
  reviewId: number;
}

export default function AskAgentCard({ repo, reviewId }: AskAgentCardProps) {
  const loopback = useAskAgentLoopbackLatched();
  const ask = useAskAgentMutation(repo, reviewId);
  const [body, setBody] = useState("");

  async function onAsk() {
    const trimmed = body.trim();
    if (!trimmed || ask.isPending) return;
    try {
      await ask.mutateAsync(trimmed);
      setBody("");
      toast.ok("Question posted");
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        latchAskAgentLoopback();
        return;
      }
      toast.err(`couldn't ask: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  const watchCmd = agentWatchCommand(reviewId);

  async function onCopyWatchCmd() {
    try {
      await navigator.clipboard.writeText(watchCmd);
      toast.ok("Command copied");
    } catch {
      toast.err("couldn't copy command");
    }
  }

  return (
    <section className="kbc-review__card kbc-ask-agent" data-kbc-ask-agent>
      <h2 className="kbc-review__card-title">Ask the agent</h2>
      {loopback ? (
        <p className="kbc-review__card-empty" data-kbc-ask-agent-loopback>
          Asking the agent requires a loopback session.
        </p>
      ) : (
        <>
          <textarea
            className="kbc-ask-agent__body"
            placeholder="Ask a review-level question…"
            value={body}
            onChange={(e) => setBody(e.target.value)}
            rows={3}
            aria-label="question for the agent"
            data-kbc-ask-agent-body
          />
          <div className="kbc-ask-agent__row">
            <button
              type="button"
              onClick={() => void onAsk()}
              disabled={!body.trim() || ask.isPending}
              data-kbc-ask-agent-submit
            >
              {ask.isPending ? "Asking…" : "Ask"}
            </button>
          </div>
        </>
      )}
      {/* PRR-U4 — the static agent-watch hint (design doc §2 S2 + §11: "the
          static CLI hint version is MUST; live beat is SHOULD" — the live
          presence beat is deliberately deferred, this is a NAMED ABSENCE,
          never a spinner). */}
      <div className="kbc-ask-agent__watch" data-kbc-ask-agent-watch>
        <Icon.Eye />
        <span className="kbc-ask-agent__watch-label">not watching — run:</span>
        <code className="kbc-ask-agent__watch-cmd" data-kbc-ask-agent-watch-cmd>
          {watchCmd}
        </code>
        <button
          type="button"
          className="kbc-ask-agent__copy"
          onClick={() => void onCopyWatchCmd()}
          aria-label="copy watch command"
          title="Copy command"
          data-kbc-ask-agent-watch-copy
        >
          <Icon.Copy />
        </button>
      </div>
    </section>
  );
}
