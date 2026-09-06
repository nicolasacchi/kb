// PRR-F — the dialectic ledger (design-ui.md §12.1, frontier 1). Pure
// composition over data this Room already fetches (findings + comments) —
// "zero new storage" per the frontier brief. Nothing here is persisted;
// the whole ledger (including the "hand to agent" work order) is
// recomputed fresh on every render from the SAME `ReviewFinding[]`/
// `ReviewCommentsOut` shapes `ReviewThreadsCard`/`FindingCard` already use.

import type { ReviewComment, ReviewCommentsOut, ReviewFinding } from "../api/types";
import { reviewDiffHref } from "./codeUrl";
import { questionStateForThread } from "./questionState";

export type AgentVerdict = "blocker" | "concern" | "ok";
export type HumanVerdict = "comment" | "approve" | "request-changes";

/// -1 (something's wrong) / 0 (doesn't commit) / +1 (fine) — the ONLY
/// shared axis a 3-value agent vocab and a 3-value human vocab both admit.
function agentPolarity(v: AgentVerdict): -1 | 0 | 1 {
  if (v === "blocker") return -1;
  if (v === "ok") return 1;
  return 0; // "concern" — doesn't commit either way
}
function humanPolarity(v: HumanVerdict): -1 | 0 | 1 {
  if (v === "request-changes") return -1;
  if (v === "approve") return 1;
  return 0; // "comment" — doesn't commit either way
}

/// **Judgment call, flagged** (same "the design doc doesn't spell this out,
/// this is this unit's own reading" convention `review_analytics.rs`'s
/// acceptance-rate doc already uses in this codebase): a disagreement
/// requires BOTH sides to have committed to a NON-neutral verdict (agent
/// `"concern"` / human `"comment"` never trigger it — a hedge isn't a
/// clash) whose polarities directly oppose. `blocker` vs `request-changes`
/// is agreement (both negative), not a clash — the strip only ever grows
/// when the two voices actually contradict each other.
export function verdictsDisagree(
  agent: AgentVerdict | null | undefined,
  human: HumanVerdict | null | undefined,
): boolean {
  if (!agent || !human) return false;
  const a = agentPolarity(agent);
  const h = humanPolarity(human);
  if (a === 0 || h === 0) return false;
  return a !== h;
}

export interface DisputedFindingView {
  slug: string;
  title: string;
  /// The agent's last word — the finding's own rationale (design-ui.md
  /// §12.1: "the finding rationale vs the dispute note/latest human
  /// reply").
  agentWord: string;
  /// The human's last word. `finding.disposition.note` — set at dispute
  /// time, the human's own stated reason (`DISPOSITION_HINTS.dispute`:
  /// "opens a reply — thread state awaiting-agent"). `null` when a
  /// disposition was set with no note (an honest absence, not a fabricated
  /// placeholder — the caller renders its own "no note left" copy).
  humanWord: string | null;
  path: string;
  href: string;
}

/// Every non-superseded, `disposition: "dispute"` finding, in the order
/// `findings` arrives (`ReviewFinding[]` is already server-ordered).
export function disputedFindings(
  repo: string,
  reviewId: number,
  findings: readonly ReviewFinding[],
): DisputedFindingView[] {
  return findings
    .filter((f) => !f.superseded && f.disposition?.state === "dispute")
    .map((f) => ({
      slug: f.slug,
      title: f.title,
      agentWord: f.rationale,
      humanWord: f.disposition?.note ?? null,
      path: f.location.path,
      href: reviewDiffHref(repo, reviewId, f.location.path, { finding: f.slug }),
    }));
}

export interface OpenQuestionView {
  id: string;
  path: string;
  body: string;
  href: string;
}

/// Every thread whose derived question state is `"awaiting-agent"`
/// (`lib/questionState.ts`'s own pure predicate — no second derivation).
export function openQuestions(
  repo: string,
  reviewId: number,
  comments: ReviewCommentsOut,
  findingsById: ReadonlyMap<string, Pick<ReviewFinding, "origin">>,
): OpenQuestionView[] {
  const out: OpenQuestionView[] = [];
  for (const group of comments.groups) {
    for (const c of group.comments as ReviewComment[]) {
      if (questionStateForThread(c, findingsById.get(c.id)) !== "awaiting-agent") continue;
      out.push({
        id: c.id,
        path: c.path,
        body: c.body,
        href: `${reviewDiffHref(repo, reviewId, c.path || undefined)}?thread=${encodeURIComponent(c.id)}`,
      });
    }
  }
  return out;
}

/// The "hand to agent" clipboard payload — client-side text composition,
/// zero storage (design-ui.md §12.1's own feasibility note). Absolute
/// links (`origin` prefix) so the work order is usable OUTSIDE this
/// browser tab (pasted into an agent CLI/chat).
export function buildWorkOrder(
  origin: string,
  disputed: readonly DisputedFindingView[],
  questions: readonly OpenQuestionView[],
): string {
  const lines: string[] = ["# Re-review work order", ""];
  if (disputed.length > 0) {
    lines.push(`## Disputed findings (${disputed.length})`, "");
    for (const d of disputed) {
      lines.push(`- ${d.slug} — ${d.title}`);
      lines.push(`  agent: ${d.agentWord}`);
      lines.push(`  human: ${d.humanWord ?? "(no note left)"}`);
      lines.push(`  ${origin}${d.href}`);
      lines.push("");
    }
  }
  if (questions.length > 0) {
    lines.push(`## Open questions (${questions.length})`, "");
    for (const q of questions) {
      lines.push(`- ${q.path || "general"}: ${q.body}`);
      lines.push(`  ${origin}${q.href}`);
      lines.push("");
    }
  }
  if (disputed.length === 0 && questions.length === 0) {
    lines.push("Nothing outstanding — no disputed findings or open questions.");
  }
  return lines.join("\n").trimEnd() + "\n";
}
