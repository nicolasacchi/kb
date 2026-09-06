// W2.4 — Boards v1: the pan/zoom DOM surface for one board. A DOM
// container (NOT canvas2d — a board is a few dozen node/edge objects,
// human scale, not the atlas's 50k-dot case the S6 port justified),
// transformed via `translate(pan) scale(zoom)` — a CSS analogue of
// AtlasView's `screenToLogical`/zoom-about-cursor math (recon §4), just
// applied to a positioned `<div>` instead of a canvas ctx.
//
// Single writer, no CRDT (standing rule): local `doc` state is the one
// source of truth once mounted (`key={kb+listId}` on the parent forces
// a fresh mount per board — see `routes/board.tsx` — so switching boards
// re-seeds rather than reconciling); every mutation schedules a
// debounced whole-doc `PUT` (`SAVE_DEBOUNCE_MS`), and the response
// splices straight into the `["board", kb, listId]` query-cache entry.

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { useQueryClient } from "@tanstack/react-query";
import { putBoardCanvas, type ListEntry } from "../api/client";
import {
  addNode,
  DEFAULT_CARD_HEIGHT,
  DEFAULT_CARD_WIDTH,
  moveNode,
  placedFiles,
  removeNode,
  type CanvasDoc,
  type CanvasNode,
} from "../lib/canvas";
import { artifactHref } from "../lib/artifactHref";
import { toast } from "../lib/toast";
import { Icon } from "./icons";

const ZOOM_MIN = 0.25;
const ZOOM_MAX = 3;
const SAVE_DEBOUNCE_MS = 500;

function clamp(x: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, x));
}

function newNodeId(): string {
  return `n-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

type Viewport = { x: number; y: number; zoom: number };

type PanState = { startX: number; startY: number; origX: number; origY: number };
type DragState = { nodeId: string; offsetX: number; offsetY: number };

export default function BoardCanvas({
  kb,
  listId,
  entries,
  initialCanvas,
}: {
  kb: string;
  listId: string;
  entries: ListEntry[];
  initialCanvas: CanvasDoc;
}) {
  const queryClient = useQueryClient();
  const [doc, setDoc] = useState<CanvasDoc>(initialCanvas);
  const [viewport, setViewport] = useState<Viewport>({ x: 0, y: 0, zoom: 1 });
  const [trayOpen, setTrayOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const panRef = useRef<PanState | null>(null);
  const dragRef = useRef<DragState | null>(null);
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    },
    [],
  );

  const scheduleSave = useCallback(
    (next: CanvasDoc) => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
      saveTimerRef.current = setTimeout(() => {
        saveTimerRef.current = null;
        putBoardCanvas(kb, listId, next)
          .then((saved) => {
            queryClient.setQueryData(["board", kb, listId], saved);
          })
          .catch((e) => {
            toast.err(
              `board save failed: ${e instanceof Error ? e.message : String(e)}`,
            );
          });
      }, SAVE_DEBOUNCE_MS);
    },
    [kb, listId, queryClient],
  );

  const entryByPath = useMemo(() => {
    const m = new Map<string, ListEntry>();
    for (const e of entries) if (e.source_relative) m.set(e.source_relative, e);
    return m;
  }, [entries]);

  const unplaced = useMemo(() => {
    const placed = placedFiles(doc);
    return entries.filter(
      (e) => e.source_relative && !e.tombstone && !placed.has(e.source_relative),
    );
  }, [entries, doc]);

  // --- pan (background drag) + zoom (wheel, about the cursor) -----------

  const screenToWorld = useCallback(
    (clientX: number, clientY: number, v: Viewport) => {
      const rect = containerRef.current?.getBoundingClientRect();
      const sx = clientX - (rect?.left ?? 0);
      const sy = clientY - (rect?.top ?? 0);
      return { x: (sx - v.x) / v.zoom, y: (sy - v.y) / v.zoom, sx, sy };
    },
    [],
  );

  const onBackgroundPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (e.target !== e.currentTarget) return; // a node handles its own drag
    panRef.current = {
      startX: e.clientX,
      startY: e.clientY,
      origX: viewport.x,
      origY: viewport.y,
    };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onBackgroundPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (panRef.current) {
      const dx = e.clientX - panRef.current.startX;
      const dy = e.clientY - panRef.current.startY;
      setViewport((v) => ({ ...v, x: panRef.current!.origX + dx, y: panRef.current!.origY + dy }));
      return;
    }
    if (dragRef.current) {
      const w = screenToWorld(e.clientX, e.clientY, viewport);
      const nx = w.x - dragRef.current.offsetX;
      const ny = w.y - dragRef.current.offsetY;
      const nodeId = dragRef.current.nodeId;
      setDoc((d) => moveNode(d, nodeId, nx, ny));
    }
  };
  const onBackgroundPointerUp = () => {
    panRef.current = null;
    if (dragRef.current) {
      dragRef.current = null;
      scheduleSave(doc);
    }
  };

  const onWheel = (e: ReactWheelEvent<HTMLDivElement>) => {
    e.preventDefault();
    const rect = containerRef.current?.getBoundingClientRect();
    const sx = e.clientX - (rect?.left ?? 0);
    const sy = e.clientY - (rect?.top ?? 0);
    setViewport((v) => {
      const factor = Math.exp(-e.deltaY * 0.001);
      const zoom = clamp(v.zoom * factor, ZOOM_MIN, ZOOM_MAX);
      const worldX = (sx - v.x) / v.zoom;
      const worldY = (sy - v.y) / v.zoom;
      return { zoom, x: sx - worldX * zoom, y: sy - worldY * zoom };
    });
  };

  // --- node drag ----------------------------------------------------------

  const onNodePointerDown = (e: ReactPointerEvent<HTMLDivElement>, node: CanvasNode) => {
    e.stopPropagation();
    const w = screenToWorld(e.clientX, e.clientY, viewport);
    dragRef.current = { nodeId: node.id, offsetX: w.x - node.x, offsetY: w.y - node.y };
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  // --- add / remove --------------------------------------------------------

  const placeEntry = (entry: ListEntry) => {
    if (!entry.source_relative) return;
    const rect = containerRef.current?.getBoundingClientRect();
    const centerScreenX = (rect?.width ?? 800) / 2;
    const centerScreenY = (rect?.height ?? 500) / 2;
    const worldX = (centerScreenX - viewport.x) / viewport.zoom - DEFAULT_CARD_WIDTH / 2;
    const worldY = (centerScreenY - viewport.y) / viewport.zoom - DEFAULT_CARD_HEIGHT / 2;
    const node: CanvasNode = {
      id: newNodeId(),
      type: "file",
      x: worldX,
      y: worldY,
      width: DEFAULT_CARD_WIDTH,
      height: DEFAULT_CARD_HEIGHT,
      file: entry.source_relative,
    };
    const next = addNode(doc, node);
    setDoc(next);
    scheduleSave(next);
  };

  const removeCard = (nodeId: string) => {
    const next = removeNode(doc, nodeId);
    setDoc(next);
    scheduleSave(next);
  };

  const worldStyle = {
    transform: `translate(${viewport.x}px, ${viewport.y}px) scale(${viewport.zoom})`,
    transformOrigin: "0 0",
  };

  return (
    <div className="kb-board__canvas-wrap">
      <div className="kb-board__toolbar">
        <button
          type="button"
          className="kb-board__tray-toggle"
          onClick={() => setTrayOpen((v) => !v)}
          aria-pressed={trayOpen}
          data-kb-act="board-tray-toggle"
        >
          <Icon.Plus /> add artifact{unplaced.length > 0 ? ` (${unplaced.length})` : ""}
        </button>
        <span className="kb-board__hint">
          drag to move · scroll to zoom · removing a card doesn't remove it from the list
        </span>
      </div>
      <div
        className="kb-board__canvas"
        ref={containerRef}
        onPointerDown={onBackgroundPointerDown}
        onPointerMove={onBackgroundPointerMove}
        onPointerUp={onBackgroundPointerUp}
        onPointerCancel={onBackgroundPointerUp}
        onWheel={onWheel}
        data-kb-act="board-surface"
      >
        <div className="kb-board__world" style={worldStyle}>
          <svg className="kb-board__edges" aria-hidden="true">
            {doc.edges.map((edge) => {
              const from = doc.nodes.find((n) => n.id === edge.fromNode);
              const to = doc.nodes.find((n) => n.id === edge.toNode);
              if (!from || !to) return null;
              const x1 = from.x + from.width / 2;
              const y1 = from.y + from.height / 2;
              const x2 = to.x + to.width / 2;
              const y2 = to.y + to.height / 2;
              return (
                <line
                  key={edge.id}
                  x1={x1}
                  y1={y1}
                  x2={x2}
                  y2={y2}
                  className="kb-board__edge-line"
                />
              );
            })}
          </svg>
          {doc.nodes.map((node) => (
            <BoardNode
              key={node.id}
              kb={kb}
              node={node}
              entry={node.file ? entryByPath.get(node.file) : undefined}
              onPointerDown={(e) => onNodePointerDown(e, node)}
              onRemove={() => removeCard(node.id)}
            />
          ))}
        </div>
      </div>
      {trayOpen && (
        <div className="kb-board__tray" role="menu">
          {unplaced.length === 0 ? (
            <div className="kb-board__tray-empty">
              every list entry is already on the board
            </div>
          ) : (
            <ul className="kb-board__tray-list">
              {unplaced.map((e) => (
                <li key={e.id}>
                  <button
                    type="button"
                    onClick={() => placeEntry(e)}
                    data-kb-act="board-tray-place"
                  >
                    <span className="kb-board__tray-title">
                      {e.title ?? e.source_relative}
                    </span>
                    <span className="kb-board__tray-path">{e.source_relative}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

function BoardNode({
  kb,
  node,
  entry,
  onPointerDown,
  onRemove,
}: {
  kb: string;
  node: CanvasNode;
  entry: ListEntry | undefined;
  onPointerDown: (e: ReactPointerEvent<HTMLDivElement>) => void;
  onRemove: () => void;
}) {
  const style = {
    left: `${node.x}px`,
    top: `${node.y}px`,
    width: `${node.width}px`,
    height: `${node.height}px`,
  };
  const isFile = node.type === "file" && !!node.file;
  const title = entry?.title ?? node.file ?? node.text ?? "untitled";
  const href = isFile && node.file ? artifactHref(kb, node.file, node.subpath ? { sec: node.subpath.replace(/^#/, "") } : undefined) : undefined;

  return (
    <div
      className={`kb-board__node kb-board__node--${node.type}`}
      style={style}
      onPointerDown={onPointerDown}
      data-kb-act="board-node"
    >
      <button
        type="button"
        className="kb-board__node-remove"
        onClick={(e) => {
          e.stopPropagation();
          onRemove();
        }}
        onPointerDown={(e) => e.stopPropagation()}
        title="remove from board (keeps the list entry)"
        aria-label="remove card"
        data-kb-act="board-node-remove"
      >
        <Icon.X />
      </button>
      {isFile ? (
        <a
          className="kb-board__node-link"
          href={href}
          onPointerDown={(e) => e.stopPropagation()}
        >
          <span className="kb-board__node-title">{title}</span>
          {entry?.folder && <span className="kb-board__node-folder">{entry.folder}</span>}
        </a>
      ) : (
        <div className="kb-board__node-text">{node.text ?? title}</div>
      )}
    </div>
  );
}
