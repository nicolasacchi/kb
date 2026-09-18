import { Link, useParams } from "react-router";
import { ApiError } from "../api/client";
import AttributionCard from "../components/history/AttributionCard";
import FileChangeRow from "../components/history/FileChangeRow";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import MetaLine from "../components/MetaLine";
import PageHeader from "../components/PageHeader";
import { useCommit } from "../hooks/useCommit";
import { branchesUrl, commitUrl } from "../lib/codeUrl";
import { formatUnixSeconds, shortSha } from "../lib/format";
import { sessionUrl } from "../lib/searchLanes";
import { toast } from "../lib/toast";
import { copyToClipboard } from "../editor/vimReader";
import "../styles/history.css";

/// `/r/:repo/~commit/:sha` (Phase C1's SPA half) — the commit page hub:
/// `GET /api/commit`'s full metadata (header, body, trailers, join-ladder
/// attribution) plus its per-file numstat list, each row expandable into
/// the file's own diff (`components/history/FileChangeRow.tsx`). Registered
/// as its own route in `app.tsx` (a fixed, non-file path segment right
/// after `repo` — see `lib/codeUrl.ts`'s "Phase C-SPA" header comment for
/// why this ISN'T folded into Reader's splat handling the way `~diff` is).
export default function Commit() {
  const { repo = "", sha = "" } = useParams<{ repo: string; sha: string }>();
  const { data, isLoading, error } = useCommit(repo, sha);

  if (isLoading) return <div className="kbc-reader__hint">Loading commit…</div>;
  if (error) {
    // A 404 is honestly "nothing here" (a bad/stale sha, or one that hasn't
    // been indexed yet) — worth an EmptyState with a next step, distinct
    // from a transient network/500 failure, which stays a bare error line.
    if (error instanceof ApiError && error.status === 404) {
      return (
        <EmptyState
          icon={<Icon.Folder />}
          title="Commit not found"
          hint={`${shortSha(sha)} doesn't exist on ${repo}, or hasn't been indexed yet.`}
          action={{ label: "Browse branches", to: branchesUrl(repo) }}
        />
      );
    }
    return <div className="kbc-reader__hint kbc-reader__hint--error">{(error as Error).message}</div>;
  }
  if (!data) return null;

  const isRoot = data.parents.length === 0;
  const sameCommitter = data.committer.name === data.author.name && data.committer.time === data.author.time;

  function copySha() {
    copyToClipboard(data!.sha);
    toast.ok("sha copied");
  }

  return (
    <div className="kbc-commit">
      <PageHeader title={data.subject} titleClassName="kbc-commit__subject" />
      {/* V80-R5 — the sha/author/committer facts as a shared `MetaLine`
          (was a hand-rolled `.kbc-commit__meta` flex row at 12px, the
          smallest text on the page). */}
      <MetaLine
        className="kbc-commit__meta"
        items={[
          <button
            type="button"
            className="kbc-commit__sha"
            onClick={copySha}
            title="Copy the full sha"
            data-kbc-commit-sha
          >
            {shortSha(data.sha)}
          </button>,
          <span>
            {data.author.name} authored {formatUnixSeconds(data.author.time)}
          </span>,
          !sameCommitter && (
            <span>
              {data.committer.name} committed {formatUnixSeconds(data.committer.time)}
            </span>
          ),
        ]}
      />
      {data.parents.length > 0 && (
        <div className="kbc-commit__parents" data-kbc-commit-parents>
          Parent{data.parents.length === 1 ? "" : "s"}:{" "}
          {data.parents.map((p, i) => (
            <span key={p}>
              {i > 0 && ", "}
              <Link to={commitUrl(repo, p)} className="kbc-commit__parent-link">
                {shortSha(p)}
              </Link>
            </span>
          ))}
        </div>
      )}

      {data.body && <pre className="kbc-commit__body">{data.body}</pre>}

      {data.trailers.length > 0 && (
        <ul className="kbc-commit__trailers" data-kbc-commit-trailers>
          {data.trailers.map((t, i) => {
            // `sessionUrl` returns `null` once the kb-session lane is
            // unavailable (V76-R4f, `[kb_daemon]` disabled/unconfigured) —
            // fall back to the same plain-text rendering every other
            // trailer already gets, rather than a dead/misleading link.
            const href = t.key.toLowerCase() === "kb-session" ? sessionUrl(t.value) : null;
            return (
              <li key={i}>
                <span className="kbc-commit__trailer-key">{t.key}</span>:{" "}
                {href ? (
                  <a href={href} target="_blank" rel="noreferrer">
                    {t.value}
                  </a>
                ) : (
                  <span>{t.value}</span>
                )}
              </li>
            );
          })}
        </ul>
      )}

      <section className="kbc-commit__attribution">
        <AttributionCard attribution={data.attribution} />
      </section>

      <section className="kbc-commit__files">
        <h2 className="kbc-compare__section-title">Files changed</h2>
        <MetaLine
          items={[
            `${data.totals.files} file${data.totals.files === 1 ? "" : "s"} changed`,
            <span className="kbc-filechange__additions">+{data.totals.insertions}</span>,
            <span className="kbc-filechange__deletions">-{data.totals.deletions}</span>,
          ]}
        />
        {data.files.map((f) => (
          <FileChangeRow
            key={f.path}
            repo={repo}
            file={f}
            from={isRoot ? undefined : `${data.sha}^`}
            to={isRoot ? undefined : data.sha}
            disabledNote={isRoot ? "root commit" : undefined}
            browseRef={data.sha}
          />
        ))}
      </section>
    </div>
  );
}
