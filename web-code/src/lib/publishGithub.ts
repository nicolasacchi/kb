// PRR-U5+U6 (design-ui.md §2 S5 — publish preview) — pure composition of
// ready-to-run `gh` CLI commands from a `GET /api/reviews/{id}/export/
// github` payload (`kbc-github-export/1`, `review_github_export.rs`). The
// SPA never talks to GitHub itself (module doc, root CLAUDE.md's non-goal
// list has no exception for this): this file only assembles text a human
// copies and runs, or hands to the agent.
//
// Two deliberate, documented choices beyond the design mock's literal
// sketch (which shows no command syntax at all):
//
// 1. Inline (`comments[]`) rows use `gh api repos/{owner}/{repo}/pulls/
//    {n}/comments`, one call per comment (GitHub's review-comments API has
//    no batch-create endpoint). A `range`-kind finding (`line_end` set)
//    becomes a real multi-line review comment via GitHub's own
//    `start_line`/`start_side` + `line`/`side` contract (`line` names the
//    END of the range on a multi-line comment) — `line_end` on the wire IS
//    that end line (`review_github_export.rs`'s own doc).
// 2. `general_comments[]` (file-level / no-line-precision findings) have no
//    path/line to anchor an inline comment to, so they become a plain
//    `gh pr comment` (an ordinary issue-style PR comment), not a review
//    comment.
//
// Quoting: every value rides single-quoted POSIX shell quoting
// (`'...'` with embedded `'` escaped as `'\''`) — safe for arbitrary
// finding bodies (markdown, backticks, `$`, double quotes) without needing
// a second escaping dialect for `-f`/`-F` flags.
import type { GithubExportComment, GithubExportEvent, GithubExportGeneralComment } from "../api/types";

const EVENT_FLAG: Record<GithubExportEvent, string> = {
  APPROVE: "--approve",
  REQUEST_CHANGES: "--request-changes",
  COMMENT: "--comment",
};

/// POSIX single-quote shell-escaping: close the quote, emit an
/// escaped `'`, reopen the quote. Exported so a golden test can pin the
/// exact escaping behavior independent of the command composer.
export function shellQuote(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

export interface GhExportInput {
  /// `"owner/repo"` — `review.pr_repo_slug` (`ReviewPrBinding`).
  ownerRepoSlug: string;
  prNumber: number;
  commitId: string;
  event: GithubExportEvent | null;
  verdictBody: string | null;
  comments: GithubExportComment[];
  generalComments: GithubExportGeneralComment[];
}

function splitOwnerRepo(slug: string): { owner: string; repo: string } | null {
  const parts = slug.split("/");
  if (parts.length !== 2 || !parts[0] || !parts[1]) return null;
  return { owner: parts[0], repo: parts[1] };
}

/// One `gh api .../pulls/comments` command per inline comment. `null` when
/// `ownerRepoSlug` doesn't parse (should be unreachable — the preview only
/// offers this once a review is PR-bound — but never silently drop a
/// comment into a malformed command).
function composeInlineCommand(
  owner: string,
  repo: string,
  input: GhExportInput,
  c: GithubExportComment,
): string {
  const parts = [
    "gh api",
    `repos/${owner}/${repo}/pulls/${input.prNumber}/comments`,
    `-f body=${shellQuote(c.body)}`,
    `-f commit_id=${shellQuote(input.commitId)}`,
    `-f path=${shellQuote(c.path)}`,
  ];
  if (c.line_end != null && c.line != null) {
    parts.push(`-F start_line=${c.line}`);
    parts.push(`-f start_side=${shellQuote(c.side)}`);
    parts.push(`-F line=${c.line_end}`);
  } else if (c.line != null) {
    parts.push(`-F line=${c.line}`);
  }
  parts.push(`-f side=${shellQuote(c.side)}`);
  return parts.join(" ");
}

function composeGeneralCommand(input: GhExportInput, c: GithubExportGeneralComment): string {
  return `gh pr comment ${input.prNumber} --body ${shellQuote(c.body)}`;
}

function composeReviewCommand(input: GhExportInput): string | null {
  if (!input.event) return null;
  const flag = EVENT_FLAG[input.event];
  const bodyArg = input.verdictBody && input.verdictBody.trim() !== "" ? ` -b ${shellQuote(input.verdictBody)}` : "";
  return `gh pr review ${input.prNumber} ${flag}${bodyArg}`;
}

/// The full ordered command list: inline comments, then general (file-level)
/// comments, then the review-level `gh pr review` (verdict) command last —
/// posting the verdict after every comment lands is the safer GitHub-side
/// order (a REQUEST_CHANGES/APPROVE review referencing comments that don't
/// exist yet reads oddly in the PR timeline). `null` `ownerRepoSlug` (not
/// parseable as `owner/repo`) degrades to an empty list — never a
/// malformed command.
export function composeGhCommands(input: GhExportInput): string[] {
  const or = splitOwnerRepo(input.ownerRepoSlug);
  if (!or) return [];
  const cmds: string[] = [];
  for (const c of input.comments) cmds.push(composeInlineCommand(or.owner, or.repo, input, c));
  for (const g of input.generalComments) cmds.push(composeGeneralCommand(input, g));
  const reviewCmd = composeReviewCommand(input);
  if (reviewCmd) cmds.push(reviewCmd);
  return cmds;
}

export function composeGhCommandsText(input: GhExportInput): string {
  return composeGhCommands(input).join("\n");
}
