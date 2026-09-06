import { Link } from "react-router-dom";
import type { BranchOut } from "../../api/types";
import { compareUrl } from "../../lib/codeUrl";

export interface BranchAheadBehindProps {
  repo: string;
  /// `BranchesResponse.default` — `null` only when the repo has no
  /// resolvable default (rare; `commit_info` itself failed).
  defaultBranch: string | null;
  branch: BranchOut;
}

/// The Branches table's "ahead"/"behind" compare-link logic (`routes/
/// Branches.tsx`'s own C3 doc: "ahead N" reads as "N commits `branch` has
/// that `default` doesn't", `compareUrl(repo, {from: default, to: branch})`;
/// "behind N" the mirror), extracted so Home's per-repo card (F4,
/// `components/home/RepoCard.tsx`) renders the EXACT same compare-link
/// derivation rather than re-deriving `canLink`/`compareUrl` args a second
/// time. Each caller supplies its own wrapper element (a `<td>` for the
/// table, a chip `<span>` for the card) — these two return only the inner
/// `<Link>`/`<span>`, matching Branches.tsx's pre-existing markup byte for
/// byte so its own `data-kbc-branch-ahead`/`-behind` e2e selectors
/// (`time.spec.ts`) are unaffected by the extraction.
export function BranchAhead({ repo, defaultBranch, branch }: BranchAheadBehindProps) {
  const canLink = defaultBranch !== null && defaultBranch !== branch.name;
  return canLink ? (
    <Link to={compareUrl(repo, { from: defaultBranch as string, to: branch.name })}>{branch.ahead}</Link>
  ) : (
    <span>{branch.ahead}</span>
  );
}

export function BranchBehind({ repo, defaultBranch, branch }: BranchAheadBehindProps) {
  const canLink = defaultBranch !== null && defaultBranch !== branch.name;
  return canLink ? (
    <Link to={compareUrl(repo, { from: branch.name, to: defaultBranch as string })}>{branch.behind}</Link>
  ) : (
    <span>{branch.behind}</span>
  );
}
