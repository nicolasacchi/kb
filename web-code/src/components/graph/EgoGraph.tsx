import { useEffect, useMemo, useRef, useState } from "react";
import type { EgoLaidOutEdge, EgoLaidOutNode, EgoLayoutResult } from "../../lib/egoGraph";
import { Icon } from "../icons";

const NODE_W = 112;
const NODE_H = 28;

export interface EgoGraphProps {
  layout: EgoLayoutResult;
  title: string;
  loading?: boolean;
  error?: string | null;
  onActivate(node: EgoLaidOutNode): void;
  /** Click center (or Enter on center) re-centers the graph on that symbol. */
  onRecenter(node: EgoLaidOutNode): void;
  onClose(): void;
}

// R6 — these used to carry hex fallbacks (`var(--green, #3d9a5f)`) whose
// values DISAGREED with the real tokens (dark `--green` is `#7eb98f`, dark
// `--accent` is `#8a7fff`, dark `--ink` is `#e8e6df`). tokens.css always
// defines all three, so the fallback branch was dead — but it is exactly the
// shape of the `--amber`/`--bg-raised` bug: one token rename and every one of
// them silently revives with the wrong colour. A bare `var(--token)` fails
// LOUDLY (the property drops) instead of painting a plausible lie.
function classFill(c: string): string {
  const v = c.toLowerCase();
  if (v === "exact") return "var(--green)";
  if (v === "likely") return "var(--accent)";
  return "var(--warn)";
}

function edgePath(
  from: EgoLaidOutNode,
  to: EgoLaidOutNode,
): string {
  // Orthogonal elbow from right of source to left of target (or reverse).
  const x1 = from.x + NODE_W;
  const y1 = from.y + NODE_H / 2;
  const x2 = to.x;
  const y2 = to.y + NODE_H / 2;
  if (from.layer < to.layer) {
    const mid = (x1 + x2) / 2;
    return `M ${x1} ${y1} L ${mid} ${y1} L ${mid} ${y2} L ${x2} ${y2}`;
  }
  // Back-edge or same column: straight-ish.
  const mid = (x1 + x2) / 2;
  return `M ${from.x} ${y1} L ${mid} ${y1} L ${mid} ${y2} L ${to.x + NODE_W} ${y2}`;
}

export default function EgoGraph({
  layout,
  title,
  loading,
  error,
  onActivate,
  onRecenter,
  onClose,
}: EgoGraphProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [cursor, setCursor] = useState(0);

  const nodes = layout.nodes;
  const byId = useMemo(() => Object.fromEntries(nodes.map((n) => [n.id, n])), [nodes]);
  const centerId = nodes.find((n) => n.layer === 0)?.id;

  useEffect(() => {
    containerRef.current?.focus();
  }, []);

  useEffect(() => {
    setCursor(0);
  }, [layout]);

  // V73-K6 — one of `overlayPanels`' several independent owners of these
  // `drawer`-scope ids; see `HierarchyPanel.tsx`'s own doc for the group.
  function onKeyDown(e: React.KeyboardEvent) {
    e.stopPropagation();
    switch (e.key) {
      case "ArrowDown":
      case "j":
        // kbc-owns: "drawer.row-next":
        e.preventDefault();
        setCursor((c) => Math.min(nodes.length - 1, c + 1));
        return;
      case "ArrowUp":
      case "k":
        // kbc-owns: "drawer.row-prev":
        e.preventDefault();
        setCursor((c) => Math.max(0, c - 1));
        return;
      case "Enter": {
        // kbc-owns: "drawer.activate":
        e.preventDefault();
        const n = nodes[cursor];
        if (!n) return;
        if (n.layer === 0) onRecenter(n);
        else onActivate(n);
        return;
      }
      case "Escape":
        // kbc-owns: "dismiss.overlay":
        e.preventDefault();
        onClose();
        return;
    }
  }

  function onNodeClick(n: EgoLaidOutNode) {
    if (n.layer === 0 || n.id === centerId) {
      onRecenter(n);
      return;
    }
    onActivate(n);
  }

  return (
    <div
      ref={containerRef}
      className="kbc-peek kbc-ego kbc-ego--popover"
      role="dialog"
      aria-modal="true"
      aria-label={`Ego graph: ${title}`}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      data-kbc-ego
    >
      <header className="kbc-peek__head">
        <span className="kbc-peek__mode">Graph</span>
        <span className="kbc-peek__title">{title}</span>
        <button type="button" className="kbc-peek__close" onClick={onClose} aria-label="close">
          <Icon.X />
        </button>
      </header>
      <div className="kbc-ego__body">
        {loading && <div className="kbc-peek__hint kbc-peek__loading">Loading…</div>}
        {error && <div className="kbc-peek__hint kbc-peek__error">{error}</div>}
        {!loading && !error && (
          <>
            <svg
              className="kbc-ego__svg"
              width={layout.width}
              height={layout.height}
              data-kbc-ego-svg
            >
              <defs>
                <marker
                  id="kbc-ego-arrow"
                  viewBox="0 0 10 10"
                  refX="9"
                  refY="5"
                  markerWidth="6"
                  markerHeight="6"
                  orient="auto-start-reverse"
                >
                  <path d="M 0 0 L 10 5 L 0 10 z" fill="var(--ink-mute)" />
                </marker>
              </defs>
              {layout.edges.map((e: EgoLaidOutEdge) => {
                const from = byId[e.from];
                const to = byId[e.to];
                if (!from || !to) return null;
                return (
                  <path
                    key={`${e.from}-${e.to}`}
                    d={edgePath(from, to)}
                    className="kbc-ego__edge"
                    data-kbc-ego-edge
                    data-kbc-ego-edge-class={e.class}
                    fill="none"
                    stroke="var(--ink-mute)"
                    strokeWidth={1.2}
                    markerEnd="url(#kbc-ego-arrow)"
                  />
                );
              })}
              {nodes.map((n, i) => {
                const active = i === cursor;
                const isCenter = n.layer === 0;
                return (
                  <g
                    key={n.id}
                    className={
                      "kbc-ego__node" +
                      (active ? " kbc-ego__node--active" : "") +
                      (isCenter ? " kbc-ego__node--center" : "")
                    }
                    transform={`translate(${n.x},${n.y})`}
                    data-kbc-ego-node={n.id}
                    data-kbc-ego-layer={n.layer}
                    onClick={() => onNodeClick(n)}
                    style={{ cursor: "pointer" }}
                  >
                    <rect
                      width={NODE_W}
                      height={NODE_H}
                      rx={6}
                      ry={6}
                      fill="var(--bg-card-hi)"
                      stroke={active ? "var(--accent)" : classFill(n.class)}
                      strokeWidth={active ? 2 : 1.4}
                    />
                    <circle cx={10} cy={NODE_H / 2} r={3.5} fill={classFill(n.class)} />
                    <text
                      x={18}
                      y={NODE_H / 2 + 4}
                      className="kbc-ego__label"
                      fill="var(--ink)"
                      fontSize={11}
                      fontFamily="var(--font-mono, monospace)"
                    >
                      {n.name.length > 12 ? n.name.slice(0, 11) + "…" : n.name}
                    </text>
                  </g>
                );
              })}
            </svg>
            {layout.truncated > 0 && (
              <div className="kbc-ego__more" data-kbc-ego-more>
                +{layout.truncated} more (node cap)
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
