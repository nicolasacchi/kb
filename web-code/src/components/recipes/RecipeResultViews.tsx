import { Fragment } from "react";
import type { KbcAddr, KbcStepRun, KbcViewRun } from "../../api/types";
import { buildRecipeTree, type RecipeTreeNode } from "../../lib/recipeTree";
import { layoutLayeredDag } from "../../lib/egoGraph";
import { addrLabel } from "../../lib/recipeAddr";
import AddressCell from "./AddressCell";
import TrustBadge from "../TrustBadge";
import { Icon } from "../icons";

export interface RecipeResultViewsProps {
  repo: string;
  step: KbcStepRun;
  /// Every `KbcViewRun` whose `.step === step.id`, in the recipe's own
  /// declared order (the view switcher's tab order).
  views: KbcViewRun[];
  activeViewId: string | undefined;
  onSelectView: (viewId: string) => void;
  /// Row-nav (Up/Down/Enter) is meaningful only for the two linear views —
  /// see this file's module doc on why tree/graph are click-only.
  focusedRowIndex: number | null;
}

const VIEW_ICON: Record<string, (p: { className?: string }) => JSX.Element> = {
  list: Icon.List,
  table: Icon.Grid,
  tree: Icon.Folder,
  graph: Icon.Graph,
};

/// The four `kbc-recipe/1` result views over ONE step's already-fetched
/// `rows: Addr[]`. `list`/`table` support keyboard row-nav
/// (`recipe.row-next/prev/open`, owned by the parent `Recipes.tsx`, which
/// passes down `focusedRowIndex`); `tree`/`graph` are click-only — every
/// address is still an ordinary link, just not linearly orderable the way
/// up/down expects (a directory tree and a laid-out graph have no single
/// "next" direction that survives a re-layout the way a list row does).
export default function RecipeResultViews({
  repo,
  step,
  views,
  activeViewId,
  onSelectView,
  focusedRowIndex,
}: RecipeResultViewsProps) {
  const active = views.find((v) => v.id === activeViewId) ?? views[0];

  return (
    <div className="kbc-recipe-views" data-kbc-recipe-views>
      {views.length > 1 && (
        <div className="kbc-recipe-views__tabs" role="tablist" data-kbc-recipe-view-tabs>
          {views.map((v) => {
            const Icon2 = VIEW_ICON[v.kind] ?? Icon.List;
            return (
              <button
                key={v.id}
                type="button"
                role="tab"
                aria-selected={v.id === active?.id}
                className={
                  "kbc-recipe-views__tab" + (v.id === active?.id ? " kbc-recipe-views__tab--active" : "")
                }
                onClick={() => onSelectView(v.id)}
                data-kbc-recipe-view-tab={v.id}
              >
                <Icon2 className="kbc-recipe-views__tab-icon" />
                {v.title ?? v.kind}
              </button>
            );
          })}
        </div>
      )}
      {active ? (
        <ViewBody repo={repo} step={step} view={active} focusedRowIndex={focusedRowIndex} />
      ) : (
        <DefaultListView repo={repo} rows={step.rows} focusedRowIndex={focusedRowIndex} />
      )}
    </div>
  );
}

function ViewBody({
  repo,
  step,
  view,
  focusedRowIndex,
}: {
  repo: string;
  step: KbcStepRun;
  view: KbcViewRun;
  focusedRowIndex: number | null;
}) {
  switch (view.kind) {
    case "list":
      return <ListView repo={repo} rows={step.rows} view={view} focusedRowIndex={focusedRowIndex} />;
    case "table":
      return <TableView repo={repo} rows={step.rows} view={view} focusedRowIndex={focusedRowIndex} />;
    case "tree":
      return <TreeView repo={repo} rows={step.rows} />;
    case "graph":
      return <GraphView repo={repo} rows={step.rows} />;
    default:
      return <DefaultListView repo={repo} rows={step.rows} focusedRowIndex={focusedRowIndex} />;
  }
}

function DefaultListView({
  repo,
  rows,
  focusedRowIndex,
}: {
  repo: string;
  rows: KbcAddr[];
  focusedRowIndex: number | null;
}) {
  return (
    <ul className="kbc-recipe-list" data-kbc-recipe-list>
      {rows.map((addr, i) => (
        <li
          key={i}
          className={"kbc-recipe-list__row" + (i === focusedRowIndex ? " kbc-recipe-list__row--focus" : "")}
          data-kbc-recipe-row={i}
        >
          <AddressCell repo={repo} addr={addr} />
        </li>
      ))}
    </ul>
  );
}

function ListView({
  repo,
  rows,
  view,
  focusedRowIndex,
}: {
  repo: string;
  rows: KbcAddr[];
  view: KbcViewRun;
  focusedRowIndex: number | null;
}) {
  const labelCol = view.columns[0];
  return (
    <ul className="kbc-recipe-list" data-kbc-recipe-list>
      {rows.map((addr, i) => {
        const text = labelCol ? view.rows[i]?.[0] : undefined;
        return (
          <li
            key={i}
            className={
              "kbc-recipe-list__row" + (i === focusedRowIndex ? " kbc-recipe-list__row--focus" : "")
            }
            data-kbc-recipe-row={i}
          >
            <AddressCell repo={repo} addr={addr} text={text} />
          </li>
        );
      })}
    </ul>
  );
}

/// The FIRST column of every row is always the address link (a row is
/// fundamentally one address; every other column is a plain scalar
/// DERIVED from that same row, per the honesty contract's "or a scalar
/// derived from one") — except a column named `field: "trust"`, which
/// renders the shared `TrustBadge` instead of its raw string, for the same
/// visual consistency every other trust-carrying surface uses.
function TableView({
  repo,
  rows,
  view,
  focusedRowIndex,
}: {
  repo: string;
  rows: KbcAddr[];
  view: KbcViewRun;
  focusedRowIndex: number | null;
}) {
  return (
    <div className="kbc-recipe-table-wrap">
      <table className="kbc-recipe-table" data-kbc-recipe-table>
        <thead>
          <tr>
            {view.columns.map((c, ci) => (
              <th key={ci}>{c.header}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((addr, ri) => {
            const cells = view.rows[ri] ?? [];
            return (
              <tr
                key={ri}
                className={ri === focusedRowIndex ? "kbc-recipe-table__row--focus" : undefined}
                data-kbc-recipe-row={ri}
              >
                {view.columns.map((c, ci) => (
                  <td key={ci}>
                    {ci === 0 ? (
                      <AddressCell repo={repo} addr={addr} text={cells[ci]} showTrust={false} />
                    ) : c.field === "trust" ? (
                      <TrustBadge cls={cells[ci]} />
                    ) : (
                      (cells[ci] ?? "—")
                    )}
                  </td>
                ))}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function TreeNodeView({ repo, node, depth }: { repo: string; node: RecipeTreeNode; depth: number }) {
  if (node.kind === "leaf") {
    return (
      <li className="kbc-recipe-tree__leaf" style={{ paddingLeft: depth * 14 }} data-kbc-recipe-tree-leaf>
        <AddressCell repo={repo} addr={node.addr} text={node.name} />
      </li>
    );
  }
  return (
    <li className="kbc-recipe-tree__dir" data-kbc-recipe-tree-dir={node.path}>
      <span className="kbc-recipe-tree__dir-name" style={{ paddingLeft: depth * 14 }}>
        <Icon.Folder className="kbc-recipe-tree__dir-icon" />
        {node.name}
      </span>
      <ul className="kbc-recipe-tree__children">
        {node.children.map((c, i) => (
          <TreeNodeView key={i} repo={repo} node={c} depth={depth + 1} />
        ))}
      </ul>
    </li>
  );
}

function TreeView({ repo, rows }: { repo: string; rows: KbcAddr[] }) {
  const { roots, ungrouped } = buildRecipeTree(rows);
  return (
    <div className="kbc-recipe-tree" data-kbc-recipe-tree>
      <ul className="kbc-recipe-tree__root">
        {roots.map((n, i) => (
          <TreeNodeView key={i} repo={repo} node={n} depth={0} />
        ))}
      </ul>
      {ungrouped.length > 0 && (
        <div className="kbc-recipe-tree__ungrouped" data-kbc-recipe-tree-ungrouped>
          <p className="kbc-recipe-tree__ungrouped-caption">
            {ungrouped.length} row(s) with no file path — not placeable on a tree:
          </p>
          <ul>
            {ungrouped.map((addr, i) => (
              <li key={i}>
                <AddressCell repo={repo} addr={addr} />
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

/// No edge data rides the wire (`KbcRunOut` carries only flat `rows: Addr[]`
/// per step — no from/to structure) — rendering fabricated edges from
/// guessed column names would misrepresent what the recipe actually
/// returned, so this lays out every row as an ISOLATED node (reusing L2's
/// `layoutLayeredDag` purely for its deterministic positioning) and
/// captions the honest reason plainly rather than implying real topology.
function GraphView({ repo, rows }: { repo: string; rows: KbcAddr[] }) {
  const nodes = rows.map((addr, i) => ({
    id: String(i),
    name: addrLabel(addr),
    class: addr.trust,
    path: addr.path,
    line: addr.line,
    kind: addr.kind,
  }));
  const { nodes: laid, width, height, truncated } = layoutLayeredDag({ nodes, edges: [] });
  const byId = new Map(rows.map((addr, i) => [String(i), addr]));
  return (
    <div className="kbc-recipe-graph" data-kbc-recipe-graph>
      <p className="kbc-recipe-graph__caption">
        No edge data in this step — showing every address as an unconnected node.
        {truncated > 0 && ` (${truncated} row(s) beyond the node cap are not shown)`}
      </p>
      <svg
        className="kbc-recipe-graph__svg"
        viewBox={`0 0 ${Math.max(width, 1)} ${Math.max(height, 1)}`}
        role="img"
        aria-label="Recipe result addresses, laid out as unconnected nodes"
      >
        {laid.map((n) => {
          const addr = byId.get(n.id);
          if (!addr) return null;
          return (
            <Fragment key={n.id}>
              <foreignObject x={n.x - 60} y={n.y - 12} width={120} height={24}>
                <div className="kbc-recipe-graph__node" data-kbc-recipe-graph-node={n.id}>
                  <AddressCell repo={repo} addr={addr} text={n.name} showTrust={false} />
                </div>
              </foreignObject>
            </Fragment>
          );
        })}
      </svg>
    </div>
  );
}
