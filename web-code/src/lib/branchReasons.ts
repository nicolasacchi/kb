import type { BranchOut } from "../api/types";

/// Recency half-life on the wire is 14 days (`history::branches::
/// RECENCY_HALF_LIFE_SECS`). `2^(-(1 day)/14d) ≈ 0.951` is "active today";
/// anything still above 0.5 is inside one half-life ("this week" by
/// magnitude, per V4.L2).
const RECENCY_TODAY = 2 ** (-1 / 14);

export interface ReasonChip {
  key: "recency" | "has_open_review" | "attribution" | "ahead";
  label: string;
}

/// Reason chips for a ranked-list row. Server `suggest.terms` is the
/// source of truth when present; an older daemon (no `suggest`) degrades
/// to RepoCard's recency sort companion — only a recency chip from
/// `last.author_time`.
export function reasonChips(branch: BranchOut, nowUnix = Date.now() / 1000): ReasonChip[] {
  const chips: ReasonChip[] = [];
  const terms = branch.suggest?.terms;
  if (terms) {
    const recency = terms.recency;
    if (typeof recency === "number" && recency > 0.5) {
      chips.push({
        key: "recency",
        label: recency >= RECENCY_TODAY ? "active today" : "this week",
      });
    }
    if (terms.has_open_review) {
      chips.push({ key: "has_open_review", label: "open review" });
    }
    if (terms.attribution) {
      chips.push({ key: "attribution", label: "agent session" });
    }
    if (terms.ahead !== undefined && branch.ahead !== null && branch.ahead > 0) {
      chips.push({ key: "ahead", label: `ahead ${branch.ahead}` });
    }
    return chips;
  }
  const t = branch.last?.author_time;
  if (t !== undefined) {
    const age = nowUnix - t;
    if (age < 86_400) chips.push({ key: "recency", label: "active today" });
    else if (age < 7 * 86_400) chips.push({ key: "recency", label: "this week" });
  }
  return chips;
}

/// RepoCard's exact degrade: `author_time` desc, missing last sorts last.
export function sortByAuthorTimeDesc<T extends { last?: { author_time: number } }>(rows: readonly T[]): T[] {
  return [...rows].sort((a, b) => (b.last?.author_time ?? 0) - (a.last?.author_time ?? 0));
}

/// Server `has_open_review` wins; client join on open-review `head_ref` is
/// the degrade when the field is absent (pre-V4.L1 daemon).
export function branchHasOpenReview(
  branch: BranchOut,
  openReviews: readonly { head_ref: string }[],
): boolean {
  if (branch.has_open_review !== undefined) return branch.has_open_review;
  return openReviews.some((r) => r.head_ref === branch.name);
}
