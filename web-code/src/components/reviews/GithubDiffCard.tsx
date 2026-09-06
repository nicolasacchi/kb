// PRR-F — GitHub thread import UI (design-addendum-2.md §A). A read-only,
// muted card for ONE GitHub-origin thread (root + nested replies) — never a
// fork of `DiffThread` (that component is a mutation-heavy `ReviewComment`
// renderer: reply/resolve/delete/suggest/disposition, none of which apply
// to content this daemon never authors). No GitHub brand glyph exists in
// this crate's hand-drawn icon set (`components/icons.tsx`) — per the "no
// invented glyphs" rule, the origin mark is a plain text chip, not an icon.
import type { GithubThread, GithubThreadComment } from "../../api/types";
import { relativeTime } from "../../lib/format";
import { parseGithubTimestamp } from "../../lib/githubThreads";
import { Icon } from "../icons";

function GithubCommentRow({ comment, isReply }: { comment: GithubThreadComment; isReply: boolean }) {
  const at = parseGithubTimestamp(comment.created_at);
  return (
    <div
      className={"kbc-gh-card__comment" + (isReply ? " kbc-gh-card__comment--reply" : "")}
      data-kbc-gh-comment-id={comment.id ?? undefined}
    >
      <div className="kbc-gh-card__meta">
        <span className="kbc-gh-card__author">{comment.author ?? "someone"}</span>
        {at != null && <span className="kbc-gh-card__at">{relativeTime(at)}</span>}
        {comment.html_url && (
          <a
            className="kbc-gh-card__link"
            href={comment.html_url}
            target="_blank"
            rel="noreferrer"
            title="View on GitHub"
            data-kbc-gh-card-open={comment.id ?? undefined}
          >
            <Icon.External />
          </a>
        )}
      </div>
      <p className="kbc-gh-card__body">{comment.body}</p>
    </div>
  );
}

export interface GithubDiffCardProps {
  thread: GithubThread;
  /// `true` (diff view) draws the orphan/general foot label; the side
  /// panel's compact row doesn't need it (its own filter chip already says
  /// "GitHub").
  showPositionFoot?: boolean;
}

export default function GithubDiffCard({ thread, showPositionFoot = false }: GithubDiffCardProps) {
  const foot = thread.general
    ? "general (issue-level) comment"
    : thread.orphaned
      ? "orphaned — couldn't re-resolve onto the latest patchset"
      : null;
  return (
    <div className="kbc-gh-card" data-kbc-gh-card={thread.id ?? undefined}>
      <div className="kbc-gh-card__head">
        <span className="kbc-gh-card__origin" data-kbc-gh-card-origin>
          GitHub
        </span>
        {thread.resolved && (
          <span className="kbc-gh-card__confidence" data-kbc-gh-card-confidence={thread.resolved.confidence}>
            {thread.resolved.confidence}
          </span>
        )}
      </div>
      <GithubCommentRow comment={thread} isReply={false} />
      {thread.replies.map((r) => (
        <GithubCommentRow key={r.id ?? r.created_at} comment={r} isReply />
      ))}
      {showPositionFoot && foot && (
        <p className="kbc-gh-card__foot" data-kbc-gh-card-foot>
          {foot}
        </p>
      )}
    </div>
  );
}
