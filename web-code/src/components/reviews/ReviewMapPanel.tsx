import { useMemo } from "react";
import type { ReviewMapOut } from "../../api/types";
import EmptyState from "../EmptyState";
import LayeredDag from "../graph/LayeredDag";
import { Icon } from "../icons";
import { layoutLayeredDag } from "../../lib/egoGraph";
import { pathBasename } from "../../lib/stacksFormat";

export interface ReviewMapPanelProps {
  repo: string;
  loading: boolean;
  error: Error | null;
  data: ReviewMapOut | null | undefined;
  onOpenFile: (path: string) => void;
}

export default function ReviewMapPanel({
  repo,
  loading,
  error,
  data,
  onOpenFile,
}: ReviewMapPanelProps) {
  const layout = useMemo(() => {
    if (!data || data.nodes.length === 0) return null;
    return layoutLayeredDag({
      nodes: data.nodes.map((n) => ({
        id: n.path,
        name: pathBasename(n.path),
        path: n.path,
        // status rides `kind` for stroke coloring in LayeredDag.
        // `class` is deliberately OMITTED: trust classes describe name
        // resolution, and mapping agent_touched onto exact/candidate
        // would launder authorship into fake resolution confidence.
        kind: n.status,
      })),
      edges: data.edges.map((e) => ({
        from: e.from,
        to: e.to,
        kind: e.kind,
        class: e.class,
      })),
    });
  }, [data]);

  if (loading) return <div className="kbc-reader__hint">Loading map…</div>;
  if (error) return <div className="kbc-reader__hint kbc-reader__hint--error">{error.message}</div>;
  if (data === null) {
    return (
      <div className="kbc-reader__hint" data-kbc-review-map-absent>
        Map surface not available on this server.
      </div>
    );
  }
  // Banners render in BOTH branches: an empty map with inputs_missing set
  // must still say "not computed" (absence ≠ zero — never contradict it
  // with a bare empty-state).
  const banners = data && (
    <>
      {data.inputs_missing.length > 0 && (
        <div className="kbc-recipes__banner" role="status" data-kbc-review-map-missing>
          not computed: {data.inputs_missing.join(", ")}
        </div>
      )}
      {data.note && (
        <div
          className="kbc-recipes__banner kbc-recipes__banner--note"
          role="status"
          data-kbc-review-map-note
        >
          {data.note}
        </div>
      )}
    </>
  );

  if (!data || !layout) {
    return (
      <div className="kbc-review__map" data-kbc-review-map>
        {banners}
        <EmptyState icon={<Icon.List />} title="No map nodes" hint="This patchset has no files." />
      </div>
    );
  }

  const symCount = (path: string) =>
    data.nodes.find((n) => n.path === path)?.symbols_changed.length ?? 0;
  const agentOf = (path: string) =>
    data.nodes.find((n) => n.path === path)?.agent_touched === true;

  return (
    <div className="kbc-review__map" data-kbc-review-map>
      {banners}
      <LayeredDag
        layout={layout}
        title={`Review map · ${repo}`}
        nodeBadge={(n) => {
          const c = symCount(n.id);
          return c > 0 ? `(${c})` : null;
        }}
        nodeAgent={(n) => agentOf(n.id)}
        onActivate={(n) => onOpenFile(n.path ?? n.id)}
      />
    </div>
  );
}
