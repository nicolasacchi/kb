// Pure direction-grouping + row-shaping for the Framework card (T1 —
// design-ui.md §9.4a). Kept free of React/DOM concerns —
// `components/provenance/FrameworkCard.tsx` is the thin renderer that maps
// whatever this module computes onto JSX; this module owns the actual
// grouping/labeling logic so it's testable without a render harness (this
// app's vitest config is `environment: "node"`, `.test.ts`-only).

import type { FrameworkEdgeOut } from "../api/types";

export interface FrameworkEdgeRow {
  /// Stable React key — see `groupFrameworkEdges`'s doc for why the source
  /// array index is folded in.
  key: string;
  /// `kind` with underscores turned to spaces for display (`"render_view"` →
  /// `"render view"`) — the raw `kind` string stays available via `kind`.
  kind: string;
  kindLabel: string;
  /// `"likely"` | `"candidate"` — `frameworks::Trust` has no `"exact"`
  /// variant; fed straight into `TrustBadge`'s `cls` prop (`lib/
  /// trustBadge.ts`'s `trustTierFrom` classifies down on anything else).
  trust: string;
  /// The deep-link target — `null` when the edge carries no linkable
  /// destination at all (e.g. a dom-id-only `turbo_stream_target` with no
  /// `dst_path`), in which case the row renders as plain text instead.
  linkPath: string | null;
  linkLine: number | null;
  /// Secondary label — the OTHER side's symbol/kind, shown alongside the
  /// deep link when it says more than the bare path (e.g. `dst_symbol`
  /// alongside a `dst_path`), or AS the row's whole label when there is no
  /// `linkPath` at all.
  detail: string | null;
}

export interface FrameworkEdgeGroups {
  /// `direction === "src"` — edges this file PRODUCES.
  produces: FrameworkEdgeRow[];
  /// `direction === "dst"` — edges that TARGET this file.
  targets: FrameworkEdgeRow[];
}

function kindLabel(kind: string): string {
  return kind.replace(/_/g, " ");
}

/// Group `edges` (as returned by `GET /api/framework/edges`, both
/// directions interleaved) into the two panels `FrameworkCard` renders.
/// Row order is preserved within each group (server order — `src_path`
/// rows first, then `dst_path` rows, per `framework_edges.rs`'s own doc) —
/// this function only PARTITIONS, never re-sorts.
export function groupFrameworkEdges(edges: FrameworkEdgeOut[]): FrameworkEdgeGroups {
  const produces: FrameworkEdgeRow[] = [];
  const targets: FrameworkEdgeRow[] = [];
  edges.forEach((e, i) => {
    if (e.direction === "src") {
      produces.push({
        // The source array index disambiguates two edges of the same kind
        // pointing at the same (or no) destination — real fixture data
        // (`tests/rails_lens.rs`) has exactly this shape (e.g. two
        // `render_partial` edges from different call sites).
        key: `src:${i}`,
        kind: e.kind,
        kindLabel: kindLabel(e.kind),
        trust: e.trust,
        linkPath: e.dst_path,
        // `rails_edges` carries no destination LINE column at all (see
        // `symbol_addr.rs`'s own comment on why a rails: sym= jump always
        // lands on line 1) — a file-level link is the honest destination.
        linkLine: null,
        detail: e.dst_symbol ?? e.dst_kind ?? null,
      });
    } else {
      targets.push({
        key: `dst:${i}`,
        kind: e.kind,
        kindLabel: kindLabel(e.kind),
        trust: e.trust,
        linkPath: e.src_path,
        linkLine: e.src_line,
        detail: e.src_symbol ?? null,
      });
    }
  });
  return { produces, targets };
}

export function frameworkEdgeGroupsAreEmpty(groups: FrameworkEdgeGroups): boolean {
  return groups.produces.length === 0 && groups.targets.length === 0;
}
