import { useMemo } from "react";
import { useParams } from "react-router-dom";
import BranchesHero from "../components/branches/BranchesHero";
import BrowseAllBranches from "../components/branches/BrowseAllBranches";
import RankedBranchList from "../components/branches/RankedBranchList";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useBranches } from "../hooks/useBranches";
import { useWorkspaceGroups } from "../hooks/useSets";
import { readerUrl } from "../lib/breadcrumbs";
import "../styles/history.css";
import "../styles/landing.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

/// `/r/:repo/~branches` — V4.L2 three-zone landing: default-branch hero,
/// suggested ranking, then today's table demoted into a browse-all
/// `<details>`. Ahead/behind compare-link derivation still lives in
/// `BranchAhead`/`BranchBehind` (unchanged; `time.spec.ts` selects the
/// moved table cells).
export default function Branches() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const branches = useBranches(repo);
  // V70-A10 — ONE fetch for the whole page (never N+1 per row): both
  // `RankedBranchList` and `BrowseAllBranches` read this same map for
  // their own "N workspaces" chip.
  const workspaceGroups = useWorkspaceGroups(repo);
  const workspaceCountsByRef = useMemo(() => {
    const map: Record<string, number> = {};
    for (const g of workspaceGroups.data?.groups ?? []) {
      if (g.ref) map[g.ref] = g.workspaces.length;
    }
    return map;
  }, [workspaceGroups.data]);

  if (branches.isLoading) return <div className="kbc-reader__hint">Loading branches…</div>;
  if (branches.error) {
    return <div className="kbc-reader__hint kbc-reader__hint--error">{(branches.error as Error).message}</div>;
  }
  if (!branches.data) return null;

  const { default: defaultBranch, branches: rows, truncated } = branches.data;
  const defaultRow = defaultBranch ? rows.find((b) => b.name === defaultBranch) : undefined;

  return (
    <div className="kbc-branches">
      <h1 className="kbc-branches__title">Branches</h1>
      {truncated && (
        <div className="kbc-branches__truncated" data-kbc-branches-truncated>
          Showing a bounded subset of branches.
        </div>
      )}
      <BranchesHero repo={repo} defaultBranch={defaultBranch} defaultRow={defaultRow} />
      {rows.length <= 1 ? (
        <EmptyState
          icon={<Icon.Branch />}
          title="Only one branch"
          hint={`${repo} has a single branch — nothing to compare yet. Push a feature branch to see ahead/behind here.`}
          action={{ label: "Back to files", to: readerUrl(repo, "") }}
        />
      ) : (
        <>
          <RankedBranchList repo={repo} defaultBranch={defaultBranch} workspaceCountsByRef={workspaceCountsByRef} />
          <BrowseAllBranches
            repo={repo}
            defaultBranch={defaultBranch}
            rows={rows}
            workspaceCountsByRef={workspaceCountsByRef}
          />
        </>
      )}
    </div>
  );
}
