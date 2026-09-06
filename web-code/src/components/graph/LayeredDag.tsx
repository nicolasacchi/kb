import { useEffect, useMemo, useRef, useState } from "react";
import type {
  EgoLaidOutNode,
  LayeredDagLaidOutEdge,
  LayeredDagLayoutResult,
} from "../../lib/egoGraph";

const NODE_W = 112;
const NODE_H = 28;

export interface LayeredDagProps {
  layout: LayeredDagLayoutResult;
  title?: string;
  loading?: boolean;
  error?: string | null;
  /** Extra badge text rendered next to the node label (e.g. symbol count). */
  nodeBadge?: (node: EgoLaidOutNode) => string | null;
  /** Status color / stroke override (status wire string on node.kind). */
  nodeStroke?: (node: EgoLaidOutNode) => string | undefined;
  /** When true, show a small agent-touched glyph. */
  nodeAgent?: (node: EgoLaidOutNode) => boolean;
  onActivate(node: EgoLaidOutNode): void;
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

function statusStroke(status: string | undefined): string {
  const s = (status || "").toLowerCase();
  if (s === "added") return "var(--green)";
  if (s === "deleted") return "var(--red)";
  if (s === "renamed") return "var(--accent)";
  return "var(--ink-mute)";
}

function edgePath(from: EgoLaidOutNode, to: EgoLaidOutNode): string {
  // Same orthogonal elbow as EgoGraph — ONE layout family, no physics.
  const x1 = from.x + NODE_W;
  const y1 = from.y + NODE_H / 2;
  const x2 = to.x;
  const y2 = to.y + NODE_H / 2;
  if (from.layer < to.layer) {
    const mid = (x1 + x2) / 2;
    return `M ${x1} ${y1} L ${mid} ${y1} L ${mid} ${y2} L ${x2} ${y2}`;
  }
  const mid = (x1 + x2) / 2;
  return `M ${from.x} ${y1} L ${mid} ${y1} L ${mid} ${y2} L ${to.x + NODE_W} ${y2}`;
}

/**
 * Layered DAG renderer reusing the ego-graph geometry/edge style (V3.3-S1
 * ruling D5: ONE layout family, no graph lib). Edges: solid = import,
 * dashed = call; class on tooltip.
 */
export default function LayeredDag({
  layout,
  title,
  loading,
  error,
  nodeBadge,
  nodeStroke,
  nodeAgent,
  onActivate,
}: LayeredDagProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [cursor, setCursor] = useState(0);
  const nodes = layout.nodes;
  const byId = useMemo(() => Object.fromEntries(nodes.map((n) => [n.id, n])), [nodes]);

  // V70-A3S — no mount-time focus steal: unlike `EgoGraph` (a modal
  // `role="dialog"` popover, which SHOULD grab focus when it opens),
  // `LayeredDag` renders in flow inside its panel (the review Map tab), so
  // auto-focusing it on every mount/patchset-switch would yank keyboard
  // focus off whatever the operator was doing on the page. `tabIndex={0}`
  // below keeps j/k/Enter reachable via Tab or a click, same as any other
  // inline interactive region.
  useEffect(() => {
    setCursor(0);
  }, [layout]);

  function onKeyDown(e: React.KeyboardEvent) {
    e.stopPropagation();
    switch (e.key) {
      case "ArrowDown":
      case "j":
        e.preventDefault();
        setCursor((c) => Math.min(nodes.length - 1, c + 1));
        return;
      case "ArrowUp":
      case "k":
        e.preventDefault();
        setCursor((c) => Math.max(0, c - 1));
        return;
      case "Enter": {
        e.preventDefault();
        const n = nodes[cursor];
        if (n) onActivate(n);
        return;
      }
    }
  }

  return (
    <div
      ref={containerRef}
      className="kbc-layered-dag"
      tabIndex={0}
      onKeyDown={onKeyDown}
      role="img"
      aria-label={title ?? "Dependency map"}
      data-kbc-layered-dag
    >
      {loading && <div className="kbc-peek__hint kbc-peek__loading">Loading…</div>}
      {error && <div className="kbc-peek__hint kbc-peek__error">{error}</div>}
      {!loading && !error && (
        <>
          <svg
            className="kbc-ego__svg"
            width={layout.width}
            height={layout.height}
            data-kbc-layered-dag-svg
          >
            <defs>
              <marker
                id="kbc-dag-arrow"
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
            {layout.edges.map((e: LayeredDagLaidOutEdge) => {
              const from = byId[e.from];
              const to = byId[e.to];
              if (!from || !to) return null;
              const dashed = e.kind === "call";
              return (
                <path
                  key={`${e.kind}-${e.from}-${e.to}`}
                  d={edgePath(from, to)}
                  className="kbc-ego__edge"
                  data-kbc-layered-dag-edge={e.kind}
                  data-kbc-ego-edge-class={e.class}
                  fill="none"
                  stroke="var(--ink-mute)"
                  strokeWidth={1.2}
                  strokeDasharray={dashed ? "4 3" : undefined}
                  markerEnd="url(#kbc-dag-arrow)"
                >
                  <title>
                    {e.kind} · {e.class} · {e.from} → {e.to}
                  </title>
                </path>
              );
            })}
            {nodes.map((n, i) => {
              const active = i === cursor;
              const stroke = nodeStroke?.(n) ?? statusStroke(n.kind) ?? classFill(n.class);
              const badge = nodeBadge?.(n);
              const agent = nodeAgent?.(n) === true;
              return (
                <g
                  key={n.id}
                  className={"kbc-ego__node" + (active ? " kbc-ego__node--active" : "")}
                  transform={`translate(${n.x},${n.y})`}
                  data-kbc-layered-dag-node={n.id}
                  data-kbc-ego-layer={n.layer}
                  onClick={() => onActivate(n)}
                  style={{ cursor: "pointer" }}
                >
                  <title>
                    {n.path ?? n.id}
                    {n.kind ? ` · ${n.kind}` : ""}
                    {agent ? " · agent-touched" : ""}
                  </title>
                  <rect
                    width={NODE_W}
                    height={NODE_H}
                    rx={6}
                    ry={6}
                    fill="var(--bg-card-hi)"
                    stroke={active ? "var(--accent)" : stroke}
                    strokeWidth={active ? 2 : 1.4}
                  />
                  {agent && (
                    <circle
                      cx={NODE_W - 10}
                      cy={8}
                      r={3}
                      fill="var(--accent)"
                      data-kbc-layered-dag-agent
                    />
                  )}
                  <text
                    x={8}
                    y={NODE_H / 2 + 4}
                    className="kbc-ego__label"
                    fill="var(--ink)"
                    fontSize={11}
                    fontFamily="var(--font-mono, monospace)"
                  >
                    {n.name.length > 12 ? n.name.slice(0, 11) + "…" : n.name}
                    {badge ? ` ${badge}` : ""}
                  </text>
                </g>
              );
            })}
          </svg>
          {layout.truncated > 0 && (
            <div className="kbc-ego__more" data-kbc-layered-dag-more>
              +{layout.truncated} more (node cap)
            </div>
          )}
          <div className="kbc-layered-dag__legend" data-kbc-layered-dag-legend>
            <span>
              <i className="kbc-layered-dag__leg-import" /> import (solid)
            </span>
            <span>
              <i className="kbc-layered-dag__leg-call" /> call (dashed)
            </span>
            <span>class on edge tooltip</span>
          </div>
        </>
      )}
    </div>
  );
}
