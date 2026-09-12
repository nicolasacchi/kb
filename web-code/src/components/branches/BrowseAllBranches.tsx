import { useMemo, useState } from "react";
import { Link } from "react-router";
import type { BranchOut } from "../../api/types";
import { readerUrl } from "../../lib/breadcrumbs";
import { commitUrl } from "../../lib/codeUrl";
import { shortSha } from "../../lib/format";
import { workspacesUrl } from "../../lib/setsUrl";
import { speedFilterItems } from "../../lib/speedSearch";
import AttributionCard from "../history/AttributionCard";
import { BranchAhead, BranchBehind } from "../history/BranchAheadBehind";
import MergeCheckButton from "../history/MergeCheckButton";
import RefTypeahead from "../RefTypeahead";

export interface BrowseAllBranchesProps {
  repo: string;
  defaultBranch: string | null;
  rows: BranchOut[];
  /// V70-A10 — branch name → workspace count (`Branches.tsx`'s ONE
  /// page-level fetch); a branch absent from the map has none.
  workspaceCountsByRef?: Record<string, number>;
}

/// Zone (c) — today's table, moved into a `<details>` section. Markup +
/// `data-kbc-branch-*` attrs stay byte-compatible so `time.spec.ts` keeps
/// matching the same cells.
export default function BrowseAllBranches({
  repo,
  defaultBranch,
  rows,
  workspaceCountsByRef = {},
}: BrowseAllBranchesProps) {
  const [open, setOpen] = useState(rows.length <= 5);
  const [filter, setFilter] = useState("");
  const names = useMemo(() => rows.map((b) => b.name), [rows]);
  const visible = useMemo(() => {
    if (!filter.trim()) return rows;
    const keep = new Set(speedFilterItems(names, filter, (s) => s).map((h) => h.item));
    return rows.filter((b) => keep.has(b.name));
  }, [rows, names, filter]);

  return (
    <details
      className="kbc-browse"
      data-kbc-browse
      open={open}
      onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}
    >
      <summary className="kbc-browse__summary" data-kbc-browse-toggle>
        All branches ({rows.length})
      </summary>
      <div className="kbc-browse__filter">
        <RefTypeahead
          value={filter}
          onChange={setFilter}
          items={names}
          placeholder="Filter branches"
          aria-label="filter branches"
          className="kbc-browse__typeahead"
          inputProps={{ "data-kbc-browse-filter": "" }}
        />
      </div>
      <div className="kbc-branches__table-wrap">
        <table className="kbc-branches__table">
          <thead>
            <tr>
              <th>Branch</th>
              <th>Ahead</th>
              <th>Behind</th>
              <th>Last commit</th>
              <th>Tip attribution</th>
              <th>Merge check</th>
            </tr>
          </thead>
          <tbody>
            {visible.map((b) => (
              <tr key={b.name} data-kbc-branch-row={b.name}>
                <td>
                  <Link to={readerUrl(repo, "", b.name)} className="kbc-branches__name">
                    {b.name}
                  </Link>
                  {b.is_head && (
                    <span className="kbc-branches__head-badge" data-kbc-branch-head>
                      HEAD
                    </span>
                  )}
                  {workspaceCountsByRef[b.name] > 0 && (
                    <Link
                      to={workspacesUrl(repo, b.name)}
                      className="kbc-reason-chip"
                      data-kbc-branch-workspaces={b.name}
                    >
                      {workspaceCountsByRef[b.name]} workspace{workspaceCountsByRef[b.name] === 1 ? "" : "s"}
                    </Link>
                  )}
                </td>
                <td data-kbc-branch-ahead={b.ahead}>
                  <BranchAhead repo={repo} defaultBranch={defaultBranch} branch={b} />
                </td>
                <td data-kbc-branch-behind={b.behind}>
                  <BranchBehind repo={repo} defaultBranch={defaultBranch} branch={b} />
                </td>
                <td>
                  <Link to={commitUrl(repo, b.target_sha)} className="kbc-branches__last-subject">
                    {b.last ? b.last.subject : shortSha(b.target_sha)}
                  </Link>
                </td>
                <td>
                  {b.attribution.confidence === "none" ? (
                    <span className="kbc-branches__no-attrib">—</span>
                  ) : (
                    <AttributionCard attribution={b.attribution} compact />
                  )}
                </td>
                <td>
                  {defaultBranch !== null && defaultBranch !== b.name ? (
                    <MergeCheckButton repo={repo} from={defaultBranch} to={b.name} />
                  ) : (
                    <span className="kbc-branches__no-attrib">—</span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </details>
  );
}
