import { useMemo, useState } from "react";
import { Link, useNavigate } from "react-router";
import type { BranchOut } from "../../api/types";
import { useBranches } from "../../hooks/useBranches";
import { usePrs } from "../../hooks/usePrs";
import { useReviews } from "../../hooks/useReviews";
import { readerUrl } from "../../lib/breadcrumbs";
import { reviewUrl } from "../../lib/codeUrl";
import { branchHasOpenReview, reasonChips, sortByAuthorTimeDesc } from "../../lib/branchReasons";
import { workspacesUrl } from "../../lib/setsUrl";
import { BranchAhead, BranchBehind } from "../history/BranchAheadBehind";
import MergeCheckButton from "../history/MergeCheckButton";
import StartReviewDialog from "../reviews/StartReviewDialog";

const RANKED_CAP = 8;

export interface RankedBranchListProps {
  repo: string;
  defaultBranch: string | null;
  /// V70-A10 — branch name → workspace count (`Branches.tsx`'s ONE
  /// page-level fetch); a branch absent from the map has none.
  workspaceCountsByRef?: Record<string, number>;
}

/// Zone (b) — suggested ranking. Server order when `suggest` is present;
/// RepoCard's author_time desc when an older daemon omits it.
export default function RankedBranchList({
  repo,
  defaultBranch,
  workspaceCountsByRef = {},
}: RankedBranchListProps) {
  const suggested = useBranches(repo, "suggested");
  const openReviews = useReviews(repo, "open");
  const prs = usePrs(repo);
  const navigate = useNavigate();
  const [expanded, setExpanded] = useState(false);
  const [startFor, setStartFor] = useState<string | null>(null);

  const reviews = openReviews.data?.reviews ?? [];
  const prByHead = useMemo(() => {
    const map = new Map<string, { number: number; title: string }>();
    for (const pr of prs.data?.prs ?? []) {
      if (!map.has(pr.head_ref)) map.set(pr.head_ref, { number: pr.number, title: pr.title });
    }
    return map;
  }, [prs.data]);

  if (suggested.isLoading) {
    return <div className="kbc-reader__hint">Ranking branches…</div>;
  }
  if (suggested.error || !suggested.data) return null;

  const all = suggested.data.branches.filter((b) => b.name !== suggested.data!.default);
  if (all.length === 0) return null;

  const hasSuggest = all.some((b) => b.suggest);
  const ranked = hasSuggest ? all : sortByAuthorTimeDesc(all);
  const visible = expanded || ranked.length <= RANKED_CAP ? ranked : ranked.slice(0, RANKED_CAP);

  return (
    <section className="kbc-ranked" data-kbc-ranked>
      <h2 className="kbc-ranked__title">Suggested</h2>
      <ul className="kbc-ranked__list">
        {visible.map((b) => (
          <RankedRow
            key={b.name}
            repo={repo}
            defaultBranch={defaultBranch}
            branch={b}
            openReviewId={
              branchHasOpenReview(b, reviews)
                ? reviews.find((r) => r.head_ref === b.name)?.id
                : undefined
            }
            pr={prByHead.get(b.name)}
            onStart={() => setStartFor(b.name)}
            workspaceCount={workspaceCountsByRef[b.name]}
          />
        ))}
      </ul>
      {!expanded && ranked.length > RANKED_CAP && (
        <button
          type="button"
          className="kbc-ranked__more"
          onClick={() => setExpanded(true)}
          data-kbc-ranked-more
        >
          Show all {ranked.length}
        </button>
      )}
      {startFor && (
        <StartReviewDialog
          repo={repo}
          initialHead={startFor}
          initialBase={defaultBranch ?? undefined}
          onClose={() => setStartFor(null)}
          onCreated={(id) => {
            setStartFor(null);
            navigate(reviewUrl(repo, id));
          }}
        />
      )}
    </section>
  );
}

function RankedRow({
  repo,
  defaultBranch,
  branch,
  openReviewId,
  pr,
  onStart,
  workspaceCount,
}: {
  repo: string;
  defaultBranch: string | null;
  branch: BranchOut;
  openReviewId: number | undefined;
  pr: { number: number; title: string } | undefined;
  onStart: () => void;
  workspaceCount?: number;
}) {
  const chips = reasonChips(branch);
  return (
    <li className="kbc-ranked__row" data-kbc-ranked-row={branch.name}>
      <div className="kbc-ranked__identity">
        <Link to={readerUrl(repo, "", branch.name)} className="kbc-ranked__name">
          {branch.name}
        </Link>
        {branch.remote && (
          <span className="kbc-reason-chip" data-kbc-ranked-remote>
            {branch.remote}
          </span>
        )}
        {pr && (
          <span className="kbc-reason-chip kbc-reason-chip--pr" data-kbc-ranked-pr={pr.number} title={pr.title}>
            #{pr.number}
          </span>
        )}
        {!!workspaceCount && (
          <Link
            to={workspacesUrl(repo, branch.name)}
            className="kbc-reason-chip"
            data-kbc-ranked-workspaces={branch.name}
          >
            {workspaceCount} workspace{workspaceCount === 1 ? "" : "s"}
          </Link>
        )}
      </div>
      <div className="kbc-ranked__chips">
        {chips.map((c) => (
          <span key={c.key} className="kbc-reason-chip" data-kbc-reason={c.key}>
            {c.label}
          </span>
        ))}
      </div>
      <div className="kbc-ranked__counts">
        <span className="kbc-ranked__ab" data-kbc-ranked-ahead={branch.ahead}>
          <BranchAhead repo={repo} defaultBranch={defaultBranch} branch={branch} /> ahead
        </span>
        <span className="kbc-ranked__ab" data-kbc-ranked-behind={branch.behind}>
          <BranchBehind repo={repo} defaultBranch={defaultBranch} branch={branch} /> behind
        </span>
        {defaultBranch !== null && defaultBranch !== branch.name ? (
          <MergeCheckButton repo={repo} from={defaultBranch} to={branch.name} />
        ) : null}
      </div>
      <div className="kbc-ranked__cta">
        {openReviewId !== undefined ? (
          <Link
            to={reviewUrl(repo, openReviewId)}
            className="kbc-ranked__cta-link"
            data-kbc-ranked-open={branch.name}
          >
            Open review
          </Link>
        ) : (
          <button
            type="button"
            className="kbc-ranked__cta-btn"
            onClick={onStart}
            data-kbc-ranked-start={branch.name}
          >
            Start review
          </button>
        )}
      </div>
    </li>
  );
}
