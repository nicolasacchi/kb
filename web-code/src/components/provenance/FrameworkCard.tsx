import { Link } from "react-router-dom";
import { useFrameworkEdges } from "../../hooks/useFrameworkEdges";
import { codeUrl } from "../../lib/codeUrl";
import { groupFrameworkEdges, frameworkEdgeGroupsAreEmpty, type FrameworkEdgeRow } from "../../lib/frameworkEdges";
import TrustBadge from "../TrustBadge";

export interface FrameworkCardProps {
  repo: string;
  path: string;
}

/// T1 — design-ui.md §9.4a's Framework card: an always-visible slot in the
/// reader's `InspectorRail`, mounted the SAME way `CitedBy` is
/// (`InspectorRailProps.citedBy`'s own doc / root CLAUDE.md invariant #30's
/// "passport" precedent) — file-scoped, has nothing to do with a gutter
/// click, so it does NOT live inside the ladder-gated Provenance tab the way
/// `WhyPanel` does. For a controller: its own `route_action`/`render_view`/
/// `render_partial` edges (the "produces" group) plus any `route_action`
/// edges that TARGET it from `routes.rb` (the "targets" group); for a
/// partial/ERB: its render sites arrive as "targets" naturally, with no
/// controller-vs-view special-casing needed — `direction` alone determines
/// the split (`lib/frameworkEdges.ts`'s `groupFrameworkEdges`).
///
/// Renders NOTHING while unfetched / no file open (`!data`, mirrors
/// `CitedBy`'s own `null` return) — but once the fetch lands with a genuine
/// zero-edge result, it renders the card with a NAMED absence line ("no
/// framework edges for this file") rather than disappearing, so a Rails
/// operator can tell "this lens ran and found nothing" apart from "this
/// panel hasn't loaded yet."
export default function FrameworkCard({ repo, path }: FrameworkCardProps) {
  const { data } = useFrameworkEdges(repo, path);

  if (!data) return null;

  const groups = groupFrameworkEdges(data.edges);
  const empty = frameworkEdgeGroupsAreEmpty(groups);

  return (
    <div className="kbc-framework" data-kbc-framework>
      <div className="kbc-framework__head">
        <span className="kbc-framework__title">Framework</span>
      </div>
      {empty ? (
        <p className="kbc-framework__absence" data-kbc-framework-absence>
          no framework edges for this file
        </p>
      ) : (
        <>
          {groups.produces.length > 0 && (
            <FrameworkGroup title="This file →" rows={groups.produces} repo={repo} direction="produces" />
          )}
          {groups.targets.length > 0 && (
            <FrameworkGroup title="→ This file" rows={groups.targets} repo={repo} direction="targets" />
          )}
        </>
      )}
    </div>
  );
}

function FrameworkGroup({
  title,
  rows,
  repo,
  direction,
}: {
  title: string;
  rows: FrameworkEdgeRow[];
  repo: string;
  direction: "produces" | "targets";
}) {
  return (
    <div className="kbc-framework__group" data-kbc-framework-group={direction}>
      <div className="kbc-framework__group-title">{title}</div>
      <ul className="kbc-framework__list">
        {rows.map((row) => (
          <FrameworkRow key={row.key} row={row} repo={repo} />
        ))}
      </ul>
    </div>
  );
}

function FrameworkRow({ row, repo }: { row: FrameworkEdgeRow; repo: string }) {
  return (
    <li className="kbc-framework__row" data-kbc-framework-row data-kbc-framework-kind={row.kind}>
      <span className="kbc-framework__kind">{row.kindLabel}</span>
      {row.linkPath ? (
        <Link
          className="kbc-framework__link"
          to={codeUrl({ repo, path: row.linkPath, line: row.linkLine ?? undefined })}
        >
          {row.linkPath}
          {row.linkLine ? `:${row.linkLine}` : ""}
        </Link>
      ) : (
        <span className="kbc-framework__plain">{row.detail ?? "—"}</span>
      )}
      {row.linkPath && row.detail && <span className="kbc-framework__detail">{row.detail}</span>}
      <TrustBadge cls={row.trust} />
    </li>
  );
}
