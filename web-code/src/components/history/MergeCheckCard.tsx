import { Link } from "react-router";
import { useMergeCheck } from "../../hooks/useMergeCheck";
import { Icon } from "../icons";
import { readerUrl } from "../../lib/breadcrumbs";
import { commitUrl } from "../../lib/codeUrl";
import { shortSha } from "../../lib/format";

export interface MergeCheckCardProps {
  repo: string;
  from: string;
  to: string;
}

/// Phase G1 — the Compare page's merge-readiness card: `GET
/// /api/merge-check`'s dry-run `git merge-tree` result (`history::
/// merge_check`'s module doc has the full working-tree-safe contract) — a
/// reviewer wants to know BEFORE opening a PR whether `from` merges
/// cleanly into `to`. Clean renders a plain checkmark; conflicted renders
/// every conflicted path, each linking into the reader at `to` (the side
/// being merged in) — plus ahead/behind chips and the merge-base sha
/// (linking into the commit page), same "ahead N / behind N" semantics
/// `BranchAheadBehind`'s own doc documents for the SAME `from`/`to`
/// direction.
export default function MergeCheckCard({ repo, from, to }: MergeCheckCardProps) {
  const mergeCheck = useMergeCheck(repo, from, to);
  const data = mergeCheck.data;

  return (
    <section className="kbc-mergecheck" data-kbc-mergecheck>
      <h2 className="kbc-compare__section-title">Merge readiness</h2>
      {mergeCheck.isLoading ? (
        <div className="kbc-reader__hint">Checking…</div>
      ) : mergeCheck.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(mergeCheck.error as Error).message}</div>
      ) : data ? (
        <div className="kbc-mergecheck__body">
          <div className="kbc-mergecheck__chips">
            {data.clean ? (
              <span className="kbc-mergecheck__clean" data-kbc-mergecheck-clean>
                <Icon.Check width={12} height={12} aria-hidden /> clean merge
              </span>
            ) : (
              <span className="kbc-mergecheck__conflict-badge" data-kbc-mergecheck-conflict>
                {data.conflicts.length} conflict{data.conflicts.length === 1 ? "" : "s"}
              </span>
            )}
            <span className="kbc-mergecheck__chip" data-kbc-mergecheck-ahead>
              ahead {data.ahead}
            </span>
            <span className="kbc-mergecheck__chip" data-kbc-mergecheck-behind>
              behind {data.behind}
            </span>
            {data.resolved.merge_base && (
              <Link
                to={commitUrl(repo, data.resolved.merge_base)}
                className="kbc-mergecheck__chip kbc-mergecheck__mergebase"
                data-kbc-mergecheck-mergebase
              >
                merge-base {shortSha(data.resolved.merge_base)}
              </Link>
            )}
          </div>
          {!data.clean && (
            <ul className="kbc-mergecheck__paths" data-kbc-mergecheck-paths>
              {data.conflicts.map((c) => (
                <li key={c.path}>
                  <Link to={readerUrl(repo, c.path, to)} data-kbc-mergecheck-path={c.path}>
                    {c.path}
                  </Link>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </section>
  );
}
