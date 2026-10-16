import { ViewBar, type Cluster, type Control } from "@kb/ui/ViewBar";
import type { ReactNode } from "react";
import { NO_MODEL_REASON } from "../../lib/searchControls";

type Props = {
  clusters: Cluster[];
  /** True when hybrid/semantic are disabled because the kb has no embedding model. */
  noModel: boolean;
  /** Draws the Sort control (it owns the direction toggle). */
  renderSort: () => ReactNode;
};

// The Search page's one control bar. Each cluster is named by its legend, so
// the bar carries no title of its own and no control repeats its legend as an
// inner label (the accessible name rides `aria-label`). The "no embedding
// model" explanation is ONE muted line under the bar and the disabled
// buttons' `title`; no `reason` is passed, so nothing sits between the buttons.
export default function SearchViewBar({ clusters, noModel, renderSort }: Props) {
  const renderControl = (c: Control, node: ReactNode): ReactNode => {
    if (c.id === "sort") return renderSort();
    if (c.id === "limit") {
      return (
        <select
          className="kb-viewbar__select"
          data-kb-viewbar-control="limit"
          aria-label={c.label}
          title={c.label}
          value={c.value ?? ""}
          onChange={(e) => c.onSelect?.(e.target.value)}
        >
          {(c.options ?? []).map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      );
    }
    if (c.id === "mode-model") return <span title={NO_MODEL_REASON}>{node}</span>;
    return node;
  };
  return (
    <div className="kb-search__viewbar">
      <ViewBar ariaLabel="Search view" clusters={clusters} renderControl={renderControl} />
      {noModel ? (
        <p className="kb-search__viewbar-note" role="status">
          {NO_MODEL_REASON}
        </p>
      ) : null}
    </div>
  );
}
