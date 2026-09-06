import { Link, useParams } from "react-router-dom";
import { ApiError } from "../api/client";
import AttributionCard from "../components/history/AttributionCard";
import FileChangeRow from "../components/history/FileChangeRow";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
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
      <header className="kbc-commit__head">
        <h1 className="kbc-commit__subject">{data.subject}</h1>
        <div className="kbc-commit__meta">
          <button
            type="button"
            className="kbc-commit__sha"
            onClick={copySha}
            title="Copy the full sha"
            data-kbc-commit-sha
          >
            {shortSha(data.sha)}
          </button>
          <span>
            {data.author.name} authored {formatUnixSeconds(data.author.time)}
          </span>
          {!sameCommitter && (
            <span>
              {data.committer.name} committed {formatUnixSeconds(data.committer.time)}
            </span>
          )}
        </div>
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
      </header>

      {data.body && <pre className="kbc-commit__body">{data.body}</pre>}

      {data.trailers.length > 0 && (
        <ul className="kbc-commit__trailers" data-kbc-commit-trailers>
          {data.trailers.map((t, i) => (
            <li key={i}>
              <span className="kbc-commit__trailer-key">{t.key}</span>:{" "}
              {t.key.toLowerCase() === "kb-session" ? (
                <a href={sessionUrl(t.value)} target="_blank" rel="noreferrer">
                  {t.value}
                </a>
              ) : (
                <span>{t.value}</span>
              )}
            </li>
          ))}
        </ul>
      )}

      <section className="kbc-commit__attribution">
        <AttributionCard attribution={data.attribution} />
      </section>

      <section className="kbc-commit__files">
        <div className="kbc-commit__files-head">
          {data.totals.files} file{data.totals.files === 1 ? "" : "s"} changed · +{data.totals.insertions} -
          {data.totals.deletions}
        </div>
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
