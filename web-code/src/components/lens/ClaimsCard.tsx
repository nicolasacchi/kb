// V73-K2c (kbc-claim/1, design D18) — the reader inspector's Claims card: a
// RAIL ROW, not a gutter slot (root CLAUDE.md invariant #30's Lane Budget),
// mounted the SAME always-visible way `CitedBy`/`FrameworkCard`/
// `DiagnosticsCard` already are (`InspectorRailProps.claimsCard`). Renders
// nothing while there is no file open or the file has zero claims — an
// absent claim is the overwhelmingly common case and must not compete
// visually with the six real tabs.
//
// Reuses `ClaimRegister` (the SAME component the Review Room's Document/
// Report tabs mount) rather than a second renderer — one projection of
// `ClaimOut[]`, three mount points.
import { useFileClaims } from "../../hooks/useReviews";
import ClaimRegister from "../reviews/ClaimRegister";

export interface ClaimsCardProps {
  repo: string;
  path: string;
}

export default function ClaimsCard({ repo, path }: ClaimsCardProps) {
  const { data } = useFileClaims(repo, path);
  const claims = data?.claims ?? [];

  // Absent while unfetched OR genuinely empty — same "renders nothing"
  // posture `CitedBy.tsx` takes, for the same reason (the common case must
  // not compete with the real tabs).
  if (claims.length === 0) return null;

  return (
    <div className="kbc-inspector__claims" data-kbc-inspector-claims>
      <ClaimRegister repo={repo} claims={claims} scopeLabel={path} />
    </div>
  );
}
