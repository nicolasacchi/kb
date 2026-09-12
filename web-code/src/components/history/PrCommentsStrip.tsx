import { Link } from "react-router";
import { usePrComments } from "../../hooks/usePrs";
import { readerUrl } from "../../lib/breadcrumbs";
import { relativeTime } from "../../lib/format";

export interface PrCommentsStripProps {
  repo: string;
  number: number;
  /// The ref this PR's inline comments should link into — the fetched PR
  /// head (`refs/kbc/pr/<n>`, the Compare page's own `to` in this case) —
  /// so a `path:line` chip opens the reader at the SAME content the
  /// comment was actually written against.
  atRef: string;
}

/// Phase G4 — a read-only side strip rendering ONE fetched PR's GitHub
/// comments (review + issue, already merged server-side — see
/// `github::GithubClient::list_pull_comments`'s doc) next to the Compare
/// page whenever its `to` names that PR's fetched head (`refs/kbc/pr/<n>`,
/// `lib/prRef.ts`'s `prNumberFromRef`). Never posts — kb-code has no
/// GitHub write path (`github.rs`'s module doc: the ONE ref-write this
/// crate performs is `POST /api/prs/fetch` itself).
export default function PrCommentsStrip({ repo, number, atRef }: PrCommentsStripProps) {
  const comments = usePrComments(repo, number);
  const data = comments.data;

  return (
    <aside className="kbc-prcomments" data-kbc-prcomments>
      <h2 className="kbc-compare__section-title">PR #{number} comments</h2>
      {comments.isLoading ? (
        <div className="kbc-reader__hint">Loading comments…</div>
      ) : comments.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(comments.error as Error).message}</div>
      ) : data?.unavailable_reason ? (
        <div className="kbc-prcomments__unavailable" data-kbc-prcomments-unavailable>
          {data.unavailable_reason}
        </div>
      ) : data && data.comments.length > 0 ? (
        <ul className="kbc-prcomments__list" data-kbc-prcomments-list>
          {data.comments.map((c, i) => (
            <li key={i} className="kbc-prcomments__item">
              <div className="kbc-prcomments__meta">
                <span className="kbc-prcomments__author">{c.author}</span>
                <span className="kbc-prcomments__time">{relativeTime(Date.parse(c.created_at) / 1000)}</span>
              </div>
              {c.path && (
                <Link
                  to={readerUrl(repo, c.path, atRef, c.line)}
                  className="kbc-prcomments__path"
                  data-kbc-prcomments-path={c.path}
                >
                  {c.path}
                  {c.line ? `:${c.line}` : ""}
                </Link>
              )}
              <p className="kbc-prcomments__body">{c.body}</p>
            </li>
          ))}
        </ul>
      ) : (
        <div className="kbc-reader__hint" data-kbc-prcomments-empty>
          No comments yet.
        </div>
      )}
    </aside>
  );
}
