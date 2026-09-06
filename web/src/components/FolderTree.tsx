import { useEffect, useRef, useState } from "react";
import type { FolderNode } from "../api/client";

// FolderTree — recursive expandable list used by LeftRail's folder
// filter section (v0.8 G1). Each node renders a row with chevron +
// path label + descendant-inclusive count. Click a row to toggle it
// as the active filter (single-select; click again clears).
//
// Indentation uses --rail-folder-indent (12 px in gallery.css) times
// the node's depth. Counts shown right-aligned to match the tags
// section visual rhythm.

type Props = {
  nodes: FolderNode[];
  /// Currently-selected folder path, or null when no folder filter
  /// is active. Used to render the .is-on style.
  active: string | null;
  onSelect: (path: string | null) => void;
};

export default function FolderTree({ nodes, active, onSelect }: Props) {
  if (nodes.length === 0) {
    return (
      <div className="rail-empty" role="status">
        no folders yet
      </div>
    );
  }
  return (
    <ul className="rail-folders">
      {nodes.map((n) => (
        <FolderRow key={n.path} node={n} depth={0} active={active} onSelect={onSelect} />
      ))}
    </ul>
  );
}

type RowProps = {
  node: FolderNode;
  depth: number;
  active: string | null;
  onSelect: (path: string | null) => void;
};

function FolderRow({ node, depth, active, onSelect }: RowProps) {
  // Default-expanded for the active branch so a deep-linked filter
  // shows its container open; otherwise collapsed for browsability.
  const isActive = active === node.path;
  const isAncestorOfActive =
    active !== null && active.startsWith(node.path + "/");
  const [open, setOpen] = useState(isAncestorOfActive);
  const rowRef = useRef<HTMLDivElement | null>(null);
  const hasChildren = node.children.length > 0;
  const label = labelOf(node.path);
  const indent = depth * 12;

  // Re-open the active branch whenever `active` changes (deep-link load,
  // browser back/forward, breadcrumb nav). Open-only — never collapse —
  // so it doesn't fight manual chevron toggles or close other branches.
  useEffect(() => {
    if (isAncestorOfActive) setOpen(true);
  }, [isAncestorOfActive]);

  // Bring the active folder into view when it becomes active. `nearest`
  // only scrolls when the row is actually off-screen.
  useEffect(() => {
    if (isActive) rowRef.current?.scrollIntoView({ block: "nearest" });
  }, [isActive]);

  return (
    <li className="rail-folder-li">
      <div
        ref={rowRef}
        className={`rail-folder ${isActive ? "is-on" : ""}`}
        style={{ paddingLeft: `${indent}px` }}
      >
        {hasChildren ? (
          <button
            className={`rail-folder-chev ${open ? "is-open" : ""}`}
            aria-label={open ? "collapse" : "expand"}
            onClick={(e) => {
              e.stopPropagation();
              setOpen((v) => !v);
            }}
          >
            {open ? "▾" : "▸"}
          </button>
        ) : (
          <span className="rail-folder-chev rail-folder-chev--leaf" aria-hidden>
            •
          </span>
        )}
        <button
          className="rail-folder-name"
          onClick={() => onSelect(isActive ? null : node.path)}
          aria-pressed={isActive}
          title={node.path}
        >
          <span>{label}</span>
          <span className="rail-folder-n">{node.count}</span>
        </button>
      </div>
      {hasChildren && open && (
        <ul className="rail-folders rail-folders--nested">
          {node.children.map((child) => (
            <FolderRow
              key={child.path}
              node={child}
              depth={depth + 1}
              active={active}
              onSelect={onSelect}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

function labelOf(path: string): string {
  // Render only the leaf segment in the tree (the full path lives in
  // the row's `title` tooltip + the URL). Keeps rows compact when
  // nested deep.
  const idx = path.lastIndexOf("/");
  return idx >= 0 ? path.slice(idx + 1) : path;
}
