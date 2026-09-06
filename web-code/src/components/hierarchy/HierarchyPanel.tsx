import { useEffect, useRef } from "react";
import { rungForMouse, type RampRung } from "../../nav/ramp";
import EmptyState from "../EmptyState";
import { Icon } from "../icons";
import type { PeekAnchor } from "../peek/PeekPanel";
import {
  currentHierarchyRow,
  type HierarchyMode,
  type HierarchyNode,
  type HierarchyState,
} from "../../lib/hierarchyState";

const PANEL_WIDTH = 440;
const VIEWPORT_MARGIN = 12;
const MIN_SPACE_BELOW = 220;

const MODE_LABEL: Record<HierarchyMode, string> = {
  callers: "Callers",
  callees: "Callees",
  types: "Type hierarchy",
};

/// V70-A6 — the Ramp (§P7). Every result surface routes its "open elsewhere"
/// gestures through the ONE shared handler in `nav/ramp.ts`; here that means
/// middle-click and Ctrl/Cmd-click on a hierarchy row finally mean what they
/// mean in every browser (the recon listed this panel's rows among six
/// surfaces where a modifier click silently did nothing).
export interface HierarchyPanelProps {
  state: HierarchyState;
  currentRepo: string;
  anchor: PeekAnchor | null;
  onMove(delta: number): void;
  onActivate(node: HierarchyNode): void;
  onToggleExpand(node: HierarchyNode): void;
  onClose(): void;
  /// Absent ⇒ pre-A6 behaviour: a plain click activates and a modifier click
  /// does nothing. Present ⇒ the Ramp owns every rung.
  onRamp?(rung: RampRung, node: HierarchyNode): void;
}

function computeStyle(anchor: PeekAnchor | null): React.CSSProperties {
  if (!anchor || typeof window === "undefined") return {};
  const left = Math.min(
    Math.max(anchor.left, VIEWPORT_MARGIN),
    window.innerWidth - PANEL_WIDTH - VIEWPORT_MARGIN,
  );
  const spaceBelow = window.innerHeight - anchor.bottom;
  const openUpward = spaceBelow < MIN_SPACE_BELOW && anchor.top > spaceBelow;
  return openUpward
    ? { left: Math.max(left, VIEWPORT_MARGIN), bottom: window.innerHeight - anchor.top + 6 }
    : { left: Math.max(left, VIEWPORT_MARGIN), top: anchor.bottom + 6 };
}

/// Hierarchy class badge — same visual vocabulary as peek's precision badge
/// (`exact` → solid green, `likely` → accent, `candidate` → amber-ish default).
export function ClassBadge({ className }: { className: string }) {
  const c = (className || "candidate").toLowerCase();
  const isExact = c === "exact";
  const isLikely = c === "likely";
  return (
    <span
      className={
        "kbc-peek__badge" +
        (isExact ? " kbc-peek__badge--local" : "") +
        (isLikely ? " kbc-peek__badge--likely" : "")
      }
      title={c}
      data-kbc-hier-class={c}
      data-kbc-peek-badge
    >
      {c}
    </span>
  );
}

function HierarchyRowView({
  node,
  active,
  currentRepo,
  onActivate,
  onToggleExpand,
  onRamp,
}: {
  node: HierarchyNode;
  active: boolean;
  currentRepo: string;
  onActivate: (n: HierarchyNode) => void;
  onToggleExpand: (n: HierarchyNode) => void;
  onRamp?: (rung: RampRung, n: HierarchyNode) => void;
}) {
  const indent = Math.min(node.depth, 8) * 14;
  const isSection = node.section && (node.name === "Supertypes" || node.name.startsWith("Subtypes"));
  const canExpand =
    !node.cycle &&
    !node.depthCapped &&
    !node.truncatedNote &&
    !isSection &&
    node.depth > 0;
  const showTwistie = canExpand || node.expanded || node.loading || (isSection && node.children.length > 0);

  return (
    <div
      className={"kbc-peek__row kbc-hier__row" + (active ? " kbc-peek__row--active" : "")}
      style={{ paddingLeft: 8 + indent }}
      role="option"
      aria-selected={active}
      data-kbc-hier-row
      data-kbc-hier-depth={node.depth}
      onMouseDown={(e) => {
        const rung = rungForMouse(e);
        if (!rung || rung === "here" || !node.path || !onRamp) return;
        e.preventDefault();
        onRamp(rung, node);
      }}
      onClick={() => {
        if (node.truncatedNote) return;
        if (showTwistie && (isSection || canExpand)) {
          onToggleExpand(node);
          return;
        }
        if (node.path) onActivate(node);
      }}
    >
      <span className="kbc-hier__twist" data-kbc-hier-twist>
        {node.loading
          ? "…"
          : node.cycle
            ? "⟳"
            : showTwistie
              ? <Icon.Chevron className={node.expanded ? "kbc-twisty is-open" : "kbc-twisty"} />
              : "·"}
      </span>
      {node.kind && !isSection && <span className="kbc-peek__row-kind">{node.kind}</span>}
      <span className="kbc-peek__row-main">
        <span className="kbc-peek__row-loc">
          <span className="kbc-hier__name">{node.truncatedNote ?? node.name}</span>
          {node.path && (
            <>
              {" "}
              <span className="kbc-peek__row-path">
                {currentRepo ? "" : ""}
                {node.path}
              </span>
              {node.line > 0 && <span className="kbc-peek__row-line">:{node.line}</span>}
            </>
          )}
        </span>
      </span>
      {/* Class badges are MANDATORY on every edge row — never unbadged. */}
      {!node.truncatedNote && <ClassBadge className={node.class} />}
    </div>
  );
}

export default function HierarchyPanel({
  state,
  currentRepo,
  anchor,
  onMove,
  onActivate,
  onToggleExpand,
  onClose,
  onRamp,
}: HierarchyPanelProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    containerRef.current?.focus();
  }, []);

  // Keep the active row in view when cursor moves.
  useEffect(() => {
    const el = containerRef.current?.querySelector(".kbc-peek__row--active");
    el?.scrollIntoView({ block: "nearest" });
  }, [state.cursor]);

  if (!state.open) return null;

  function onKeyDown(e: React.KeyboardEvent) {
    e.stopPropagation();
    switch (e.key) {
      case "ArrowDown":
      case "j":
        e.preventDefault();
        onMove(1);
        return;
      case "ArrowUp":
      case "k":
        e.preventDefault();
        onMove(-1);
        return;
      case "ArrowRight":
      case "l": {
        e.preventDefault();
        const row = currentHierarchyRow(state);
        if (row && !row.expanded && !row.cycle && !row.truncatedNote) onToggleExpand(row);
        return;
      }
      case "ArrowLeft":
      case "h": {
        e.preventDefault();
        const row = currentHierarchyRow(state);
        if (row?.expanded) onToggleExpand(row);
        return;
      }
      case "Enter": {
        e.preventDefault();
        const row = currentHierarchyRow(state);
        if (row && row.path && !row.truncatedNote) onActivate(row);
        return;
      }
      case "Escape":
        e.preventDefault();
        onClose();
        return;
    }
  }

  const style = anchor ? computeStyle(anchor) : undefined;
  const className = "kbc-peek kbc-hier" + (anchor ? "" : " kbc-peek--dock");

  return (
    <div
      ref={containerRef}
      className={className}
      style={style}
      role="dialog"
      aria-modal="true"
      aria-label={`${MODE_LABEL[state.mode]}: ${state.title}`}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      data-kbc-hierarchy
      data-kbc-hier-mode={state.mode}
    >
      <header className="kbc-peek__head">
        <span className="kbc-peek__mode">{MODE_LABEL[state.mode]}</span>
        <span className="kbc-peek__title">{state.title}</span>
        <button type="button" className="kbc-peek__close" onClick={onClose} aria-label="close">
          <Icon.X />
        </button>
      </header>
      <div className="kbc-peek__body" role="listbox" aria-label={`${MODE_LABEL[state.mode]} tree`}>
        {state.loading && <div className="kbc-peek__hint kbc-peek__loading">Loading…</div>}
        {!state.loading && state.error && (
          <div className="kbc-peek__hint kbc-peek__error">{state.error}</div>
        )}
        {!state.loading && !state.error && state.flat.length === 0 && (
          <EmptyState variant="inline" title="No hierarchy edges" />
        )}
        {!state.loading &&
          !state.error &&
          state.flat.map((node, i) => (
            <HierarchyRowView
              key={node.id}
              node={node}
              active={i === state.cursor}
              currentRepo={currentRepo}
              onActivate={onActivate}
              onRamp={onRamp}
              onToggleExpand={onToggleExpand}
            />
          ))}
      </div>
    </div>
  );
}
