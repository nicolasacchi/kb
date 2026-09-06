import { Link, useParams, useSearchParams } from "react-router-dom";
import SaveAsSetButton from "../components/sets/SaveAsSetButton";
import { useSessionDiff } from "../hooks/useSessionDiff";
import { formatUnixMillis } from "../lib/format";
import { readerUrl } from "../lib/breadcrumbs";
import { commitUrl } from "../lib/codeUrl";
import { commitFileStat, commitShortSha, totalsSummary } from "../lib/sessionDiff";
import "../styles/provenance.css";
import "../styles/sets.css";

/// `/session/:sid/diff` (W4.4) — `GET /api/session-diff` rendered as one
/// narrative review unit, in the session's OWN order: prompt segments as
/// narrative headers, commits with numstat file lists (each file links
/// into the reader), uncommitted groups with their file lists — see
/// `sessiondiff`'s module doc for why this order is never re-sorted.
/// Loopback-only server-side; a non-loopback SPA gets an honest
/// "unavailable" message rather than a raw 403.
export default function SessionDiff() {
  const { sid = "" } = useParams<{ sid: string }>();
  const [searchParams] = useSearchParams();
  const repo = searchParams.get("repo") ?? undefined;
  const { data, isLoading, isError, error } = useSessionDiff(sid, repo);

  if (isLoading) {
    return <div className="kbc-sessiondiff kbc-sessiondiff--loading">Loading session diff…</div>;
  }
  if (isError) {
    return (
      <div className="kbc-sessiondiff kbc-sessiondiff--error" data-kbc-sessiondiff-error>
        Session diff is unavailable{error instanceof Error ? `: ${error.message}` : ""}. This view is
        LOOPBACK-ONLY — it only works when kb-code is reached at 127.0.0.1.
      </div>
    );
  }
  if (!data) return null;

  return (
    <div className="kbc-sessiondiff">
      <header className="kbc-sessiondiff__head">
        <h1 className="kbc-sessiondiff__title">{data.display_name ?? data.session_id}</h1>
        <div className="kbc-sessiondiff__totals">{totalsSummary(data.totals)}</div>
        {data.commits_status.status === "degraded" && (
          <div className="kbc-sessiondiff__degraded" data-kbc-sessiondiff-degraded>
            Commit data degraded — {data.commits_status.reason}
          </div>
        )}
        <SaveAsSetButton sessionId={data.session_id} reposTouched={data.repos_touched} explicitRepo={repo} />
      </header>
      <div className="kbc-sessiondiff__segments">
        {data.segments.map((seg, i) => {
          if (seg.kind === "prompt") {
            return (
              <div className="kbc-sessiondiff__segment kbc-sessiondiff__segment--prompt" key={i}>
                <h2 className="kbc-sessiondiff__prompt-header">{seg.text}</h2>
                <div className="kbc-sessiondiff__ts">{formatUnixMillis(seg.ts)}</div>
              </div>
            );
          }
          if (seg.kind === "commits") {
            return (
              <div className="kbc-sessiondiff__segment kbc-sessiondiff__segment--commits" key={i}>
                {seg.commits.map((c) => (
                  <div className="kbc-sessiondiff__commit" key={c.sha}>
                    <div className="kbc-sessiondiff__commit-head">
                      {/* Wave C — a diffed commit resolved a real repo it belongs
                          to (`c.repo`), so its sha now links to the commit page
                          hub; an unresolved commit has no repo to link into. */}
                      {c.diffed && c.repo ? (
                        <Link to={commitUrl(c.repo, c.sha)} className="kbc-sessiondiff__sha">
                          {commitShortSha(c)}
                        </Link>
                      ) : (
                        <span className="kbc-sessiondiff__sha">{commitShortSha(c)}</span>
                      )}
                      <span className="kbc-sessiondiff__subject">{c.subject ?? "(no subject)"}</span>
                      {c.author && <span className="kbc-sessiondiff__author">{c.author}</span>}
                    </div>
                    {c.diffed ? (
                      <ul className="kbc-sessiondiff__files">
                        {/* `c.files` is omitted (not `[]`) on the wire when
                            empty — a diffed commit can still have zero
                            changed files (`CommitEntryOut`'s doc in
                            `api/types.ts`) — default it. */}
                        {(c.files ?? []).map((f) => (
                          <li key={f.path}>
                            {c.repo ? (
                              <Link to={readerUrl(c.repo, f.path)}>{f.path}</Link>
                            ) : (
                              <span>{f.path}</span>
                            )}
                            <span className="kbc-sessiondiff__filestat">{commitFileStat(f)}</span>
                          </li>
                        ))}
                      </ul>
                    ) : (
                      <div className="kbc-sessiondiff__undiffed">not locally diffable</div>
                    )}
                  </div>
                ))}
              </div>
            );
          }
          return (
            <div className="kbc-sessiondiff__segment kbc-sessiondiff__segment--uncommitted" key={i}>
              <div className="kbc-sessiondiff__uncommitted-label">Uncommitted</div>
              <ul className="kbc-sessiondiff__files">
                {seg.files.map((f) => (
                  <li key={f}>{f}</li>
                ))}
              </ul>
            </div>
          );
        })}
      </div>
    </div>
  );
}
