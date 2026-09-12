import { useMemo } from "react";
import { useParams } from "react-router";
import BranchesHero from "../components/branches/BranchesHero";
import BranchViews from "../components/branches/BranchViews";
import BrowseAllBranches from "../components/branches/BrowseAllBranches";
import RankedBranchList from "../components/branches/RankedBranchList";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useBranches } from "../hooks/useBranches";
import { useWorkspaceGroups } from "../hooks/useSets";
import { readerUrl } from "../lib/breadcrumbs";
import "../styles/branches.css";
import "../styles/history.css";
import "../styles/landing.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

/// `/r/:repo/~branches`.
///
/// V75-M3 (D15) makes `branch-facts/1`'s VIEWS the primary surface: eight
/// URL-addressable views with the membership rules on the wire, reason
/// chips, a classed base per row, favourites, prefix folding, the conflict
/// radar and "compare with common base".
///
/// V4.L2's three zones are KEPT beneath it, inside one `<details>`: the
/// hero (still first — it answers a different question, "what is the
/// default branch"), then the suggested ranking and today's full table.
/// They are superseded, not deleted — `branches/1` is a separate, unchanged
/// endpoint, `time.spec.ts` and `branches-landing.spec.ts` select their
/// cells, and removing a working surface in the same unit that adds its
/// replacement would leave nothing to compare against. The `<details>`
/// defaults open on a small repo, matching `BrowseAllBranches`'s own
/// existing rule so the e2e fixture's visibility is unchanged.
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
          <BranchViews repo={repo} />
          <details className="kbc-branches__legacy" data-kbc-branches-legacy open={rows.length <= 5}>
            <summary className="kbc-branches__legacy-summary">
              Suggested ranking and the full table (the v4 landing)
            </summary>
            <RankedBranchList repo={repo} defaultBranch={defaultBranch} workspaceCountsByRef={workspaceCountsByRef} />
            <BrowseAllBranches
              repo={repo}
              defaultBranch={defaultBranch}
              rows={rows}
              workspaceCountsByRef={workspaceCountsByRef}
            />
          </details>
        </>
      )}
    </div>
  );
}
