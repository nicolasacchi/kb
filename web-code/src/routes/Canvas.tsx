import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type PointerEvent as ReactPointerEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQueries } from "@tanstack/react-query";
import {
  ApiError,
  fetchFile,
  fetchHierarchyCallees,
  fetchHierarchyCallers,
  fetchSearch,
} from "../api/client";
import type { SymbolHit } from "../api/types";
import EmptyState from "../components/EmptyState";
import { useConfirm } from "../components/ConfirmProvider";
import { Icon } from "../components/icons";
import {
  useCanvas,
  useCanvasList,
  useCreateCanvas,
  useDeleteCanvas,
  useUpdateCanvas,
} from "../hooks/useCanvas";
import {
  canvasPayloadBytes,
  decodeCanvasPayload,
  encodeCanvasPayload,
  EMPTY_CANVAS_PAYLOAD,
  fragmentKey,
  resolveFragmentSymbol,
  type CanvasFragment,
  type CanvasPayloadV1,
} from "../lib/canvasPayload";
import {
  CANVAS_CARD_H,
  CANVAS_CARD_W,
  edgesFromHierarchy,
  placeBeside,
  type EdgeDirection,
} from "../lib/canvasPlacement";
import { canvasUrl, parseCanvasSearch } from "../lib/canvasUrl";
import { readerUrl } from "../lib/breadcrumbs";
import { relativeTime } from "../lib/format";
import { toast } from "../lib/toast";
import "../styles/canvas.css";

const SAVE_DEBOUNCE_MS = 1000;
const MIN_ZOOM = 0.25;
const MAX_ZOOM = 2.5;

function loopbackOnlyToast(e: unknown): boolean {
  if (e instanceof ApiError && (e.status === 404 || e.status === 403)) {
    toast.err("canvas editing is loopback-only");
    return true;
  }
  return false;
}

function extractSymbolHits(sections: Array<{ lane: string; results: unknown }>): SymbolHit[] {
  for (const s of sections) {
    if (s.lane !== "symbols") continue;
    if (Array.isArray(s.results)) return s.results as SymbolHit[];
  }
  return [];
}

function sliceSymbolSource(
  content: string,
  lineStart: number,
  lineEnd: number,
): string {
  const lines = content.split("\n");
  const from = Math.max(0, lineStart - 1);
  const to = Math.min(lines.length, Math.max(from + 1, lineEnd));
  return lines.slice(from, to).join("\n");
}

/// V3.4-C2 — `/r/:repo/~canvas`: working-set symbol fragments on a pan-zoom
/// canvas, auto-laid along call edges, persisted via the canvas API.
export default function CanvasPage() {
  const { repo = "" } = useParams<{ repo: string }>();
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const confirm = useConfirm();

  const { id: openId, review: reviewParam } = parseCanvasSearch(searchParams);

  const listQ = useCanvasList(repo);
  const canvasQ = useCanvas(repo, openId ?? undefined);
  const createMut = useCreateCanvas(repo);
  const updateMut = useUpdateCanvas(repo);
  const deleteMut = useDeleteCanvas(repo);

  const [nameDraft, setNameDraft] = useState("");
  const [payload, setPayload] = useState<CanvasPayloadV1>(EMPTY_CANVAS_PAYLOAD);
  const [dirty, setDirty] = useState(false);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [searchQ, setSearchQ] = useState("");
  const [searchHits, setSearchHits] = useState<SymbolHit[]>([]);
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchLoading, setSearchLoading] = useState(false);

  // Sync local payload from server when canvas opens / reloads.
  const loadedIdRef = useRef<number | null>(null);
  useEffect(() => {
    if (openId == null) {
      loadedIdRef.current = null;
      setPayload(EMPTY_CANVAS_PAYLOAD);
      setDirty(false);
      return;
    }
    if (!canvasQ.data) return;
    if (loadedIdRef.current === openId && dirty) return;
    loadedIdRef.current = openId;
    setPayload(decodeCanvasPayload(canvasQ.data.payload));
    setDirty(false);
  }, [openId, canvasQ.data, dirty]);

  const payloadRef = useRef(payload);
  // Monotonic edit counter — see the save-effect's generation guard.
  const editGenRef = useRef(0);
  payloadRef.current = payload;

  const applyPayload = useCallback((updater: (prev: CanvasPayloadV1) => CanvasPayloadV1) => {
    let changed = false;
    setPayload((prev) => {
      const next = updater(prev);
      if (next === prev) return prev;
      changed = true;
      payloadRef.current = next;
      return next;
    });
    // Functional updaters run synchronously when scheduled from event handlers
    // (React 18), so `changed` is already set here.
    if (changed) {
      editGenRef.current += 1;
      setDirty(true);
    }
  }, []);

  // Debounced PUT (≥1s) — never on every pointermove. EDIT-generation
  // guard: `editGenRef` bumps on every edit; a save captures the
  // generation it serialized and only clears `dirty` if NO newer edit
  // happened while the PUT was in flight. Without this, an older save
  // resolving after a newer edit clears `dirty`, which cancels the newer
  // edit's pending timer (dirty is a dependency of this effect) and
  // silently drops it while showing "saved" — a reload reverts the edit.
  useEffect(() => {
    if (!dirty || openId == null) return;
    const t = window.setTimeout(() => {
      const gen = editGenRef.current;
      const body = encodeCanvasPayload(payloadRef.current);
      updateMut
        .mutateAsync({ id: openId, input: { payload: body } })
        .then(() => {
          if (editGenRef.current === gen) setDirty(false);
        })
        .catch((e) => {
          if (!loopbackOnlyToast(e)) {
            toast.err(`couldn't save canvas: ${e instanceof Error ? e.message : String(e)}`);
          }
        });
    }, SAVE_DEBOUNCE_MS);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only re-debounce on payload/dirty
  }, [dirty, payload, openId]);

  async function createCanvas(e: FormEvent) {
    e.preventDefault();
    const n = nameDraft.trim();
    if (!n) return;
    try {
      const created = await createMut.mutateAsync({
        name: n,
        payload: encodeCanvasPayload(EMPTY_CANVAS_PAYLOAD),
        review_id: reviewParam ?? undefined,
      });
      setNameDraft("");
      navigate(canvasUrl(repo, { id: created.id, review: reviewParam ?? undefined }));
    } catch (err) {
      if (!loopbackOnlyToast(err)) {
        toast.err(`couldn't create canvas: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
  }

  async function renameCanvas(id: number, current: string) {
    const next = window.prompt("Rename canvas", current);
    if (next == null) return;
    const n = next.trim();
    if (!n || n === current) return;
    try {
      // PUT always replaces payload — keep the live body when renaming the
      // open canvas; otherwise re-fetch so we never wipe a closed canvas.
      if (id === openId) {
        await updateMut.mutateAsync({
          id,
          input: { payload: encodeCanvasPayload(payload), name: n },
        });
      } else {
        const { fetchCanvas } = await import("../api/client");
        const row = await fetchCanvas(id);
        await updateMut.mutateAsync({
          id,
          input: { payload: row.payload, name: n },
        });
      }
    } catch (err) {
      if (!loopbackOnlyToast(err)) {
        toast.err(`couldn't rename canvas: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
  }

  async function removeCanvas(id: number, name: string) {
    const ok = await confirm({
      title: `Delete "${name}"?`,
      body: "This removes the saved layout. Fragments are only stored in the canvas payload.",
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await deleteMut.mutateAsync(id);
      if (openId === id) {
        navigate(canvasUrl(repo, { review: reviewParam ?? undefined }));
      }
    } catch (err) {
      if (!loopbackOnlyToast(err)) {
        toast.err(`couldn't delete canvas: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
  }

  // Symbol search (debounced) for "add symbol".
  useEffect(() => {
    if (!searchOpen || !searchQ.trim()) {
      setSearchHits([]);
      return;
    }
    const ctl = new AbortController();
    const t = window.setTimeout(() => {
      setSearchLoading(true);
      fetchSearch({ q: searchQ.trim(), repo, limit: 20, signal: ctl.signal })
        .then((resp) => {
          setSearchHits(extractSymbolHits(resp.sections));
          setSearchLoading(false);
        })
        .catch(() => {
          if (!ctl.signal.aborted) {
            setSearchHits([]);
            setSearchLoading(false);
          }
        });
    }, 150);
    return () => {
      clearTimeout(t);
      ctl.abort();
    };
  }, [searchQ, searchOpen, repo]);

  function addFragment(
    hit: { path: string; name: string; line_start: number },
    dir: EdgeDirection = "free",
    beside?: CanvasFragment,
  ) {
    let addedKey: string | null = null;
    let skipped = false;
    applyPayload((prev) => {
      const existing = prev.fragments;
      // Skip exact duplicate path+symbol+line.
      if (
        existing.some(
          (f) => f.path === hit.path && f.symbol === hit.name && f.line === hit.line_start,
        )
      ) {
        skipped = true;
        return prev;
      }
      // Also skip same path+symbol (different line) — one card per symbol.
      if (existing.some((f) => f.path === hit.path && f.symbol === hit.name)) {
        skipped = true;
        return prev;
      }
      const pos = placeBeside(existing, beside ?? null, dir);
      const frag: CanvasFragment = {
        path: hit.path,
        symbol: hit.name,
        line: hit.line_start,
        x: pos.x,
        y: pos.y,
      };
      addedKey = fragmentKey(frag);
      return { ...prev, fragments: [...existing, frag] };
    });
    if (skipped) {
      toast.warn("already on canvas");
      return;
    }
    if (addedKey) setSelectedKey(addedKey);
    setSearchOpen(false);
    setSearchQ("");
    setSearchHits([]);
  }

  function removeFragment(key: string) {
    applyPayload((prev) => ({
      ...prev,
      fragments: prev.fragments.filter((f) => fragmentKey(f) !== key),
    }));
    if (selectedKey === key) setSelectedKey(null);
  }

  function moveFragment(key: string, x: number, y: number) {
    applyPayload((prev) => ({
      ...prev,
      fragments: prev.fragments.map((f) => (fragmentKey(f) === key ? { ...f, x, y } : f)),
    }));
  }

  // Esc clears selection.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setSelectedKey(null);
        setSearchOpen(false);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const payloadBytes = useMemo(() => canvasPayloadBytes(payload), [payload]);
  const items = listQ.data?.items ?? [];
  const openName = canvasQ.data?.name;

  return (
    <div className="kbc-canvas" id="main" data-kbc-canvas>
      <header className="kbc-canvas__head">
        <div>
          <h1 className="kbc-canvas__title">
            Canvas{openName ? ` — ${openName}` : ""} · {repo}
          </h1>
          <p className="kbc-canvas__hint" data-kbc-canvas-hint>
            Read-only symbol fragments on a pan-zoom surface. Alt-drag or middle-drag to pan;
            ctrl+wheel to zoom. Layout is saved per canvas (loopback edits only).
          </p>
        </div>
        {openId != null && (
          <div
            className={
              "kbc-canvas__save" +
              (dirty ? " kbc-canvas__save--dirty" : " kbc-canvas__save--ok")
            }
            data-kbc-canvas-save
            data-dirty={dirty ? "1" : "0"}
          >
            <span data-kbc-canvas-save-state>{dirty ? "unsaved" : "saved"}</span>
            <span data-kbc-canvas-payload-bytes>{payloadBytes} B</span>
          </div>
        )}
      </header>

      <div className="kbc-canvas__body">
        <aside className="kbc-canvas__sidebar" data-kbc-canvas-sidebar>
          <form className="kbc-canvas__create" onSubmit={(e) => void createCanvas(e)} data-kbc-canvas-create>
            <input
              type="text"
              placeholder="New canvas name"
              value={nameDraft}
              onChange={(e) => setNameDraft(e.target.value)}
              aria-label="new canvas name"
              data-kbc-canvas-create-name
            />
            <button
              type="submit"
              disabled={!nameDraft.trim() || createMut.isPending}
              data-kbc-canvas-create-submit
            >
              {createMut.isPending ? "Creating…" : "Create canvas"}
            </button>
          </form>

          {listQ.isLoading && <div className="kbc-canvas__muted">Loading…</div>}
          {listQ.error && (
            <div className="kbc-canvas__error">{(listQ.error as Error).message}</div>
          )}
          {!listQ.isLoading && items.length === 0 && (
            <EmptyState
              icon={<Icon.List />}
              title="No canvases yet"
              hint="Create one above to pin symbol fragments on a pan-zoom surface."
            />
          )}
          <ul className="kbc-canvas__list" data-kbc-canvas-list>
            {items.map((item) => {
              const active = openId === item.id;
              return (
                <li
                  key={item.id}
                  className={"kbc-canvas__row" + (active ? " kbc-canvas__row--active" : "")}
                  data-kbc-canvas-row={item.id}
                >
                  <button
                    type="button"
                    className="kbc-canvas__row-name"
                    data-kbc-canvas-open={item.id}
                    onClick={() =>
                      navigate(canvasUrl(repo, { id: item.id, review: reviewParam ?? undefined }))
                    }
                  >
                    {item.name}
                  </button>
                  <span className="kbc-canvas__row-meta">
                    {item.payload_bytes} B · {relativeTime(item.updated_unix)}
                    {item.review_id != null ? ` · review ${item.review_id}` : ""}
                  </span>
                  <div className="kbc-canvas__row-actions">
                    <button
                      type="button"
                      data-kbc-canvas-rename={item.id}
                      onClick={() => void renameCanvas(item.id, item.name)}
                    >
                      Rename
                    </button>
                    <button
                      type="button"
                      data-kbc-canvas-delete={item.id}
                      onClick={() => void removeCanvas(item.id, item.name)}
                    >
                      Delete
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        </aside>

        <section className="kbc-canvas__main" data-kbc-canvas-main>
          {openId == null ? (
            <div className="kbc-canvas__empty-main" data-kbc-canvas-pick>
              Select or create a canvas to open the surface.
            </div>
          ) : canvasQ.isLoading ? (
            <div className="kbc-canvas__empty-main">Loading canvas…</div>
          ) : canvasQ.error ? (
            <div className="kbc-canvas__error">{(canvasQ.error as Error).message}</div>
          ) : (
            <CanvasSurface
              repo={repo}
              payload={payload}
              selectedKey={selectedKey}
              onSelect={setSelectedKey}
              onMove={moveFragment}
              onRemove={removeFragment}
              onViewChange={(view) => applyPayload((prev) => ({ ...prev, view }))}
              onAddNeighbor={(frag, hit, dir) => addFragment(hit, dir, frag)}
              searchQ={searchQ}
              onSearchQ={setSearchQ}
              searchOpen={searchOpen}
              onSearchOpen={setSearchOpen}
              searchHits={searchHits}
              searchLoading={searchLoading}
              onPickHit={(hit) =>
                addFragment(
                  { path: hit.path, name: hit.name, line_start: hit.line_start },
                  "free",
                )
              }
            />
          )}
        </section>
      </div>
    </div>
  );
}

interface SurfaceProps {
  repo: string;
  payload: CanvasPayloadV1;
  selectedKey: string | null;
  onSelect: (key: string | null) => void;
  onMove: (key: string, x: number, y: number) => void;
  onRemove: (key: string) => void;
  onViewChange: (view: CanvasPayloadV1["view"]) => void;
  onAddNeighbor: (
    source: CanvasFragment,
    hit: { path: string; name: string; line_start: number },
    dir: EdgeDirection,
  ) => void;
  searchQ: string;
  onSearchQ: (q: string) => void;
  searchOpen: boolean;
  onSearchOpen: (v: boolean) => void;
  searchHits: SymbolHit[];
  searchLoading: boolean;
  onPickHit: (hit: SymbolHit) => void;
}

function CanvasSurface({
  repo,
  payload,
  selectedKey,
  onSelect,
  onMove,
  onRemove,
  onViewChange,
  onAddNeighbor,
  searchQ,
  onSearchQ,
  searchOpen,
  onSearchOpen,
  searchHits,
  searchLoading,
  onPickHit,
}: SurfaceProps) {
  const surfaceRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef(payload.view);
  // Keep viewRef aligned with committed payload view when not mid-gesture.
  // During pan/zoom, pointer handlers write viewRef directly.

  // Pan state (space or middle button).
  const panRef = useRef<{
    mode: "pan" | "drag-card" | null;
    startX: number;
    startY: number;
    originX: number;
    originY: number;
    cardKey?: string;
    cardOriginX?: number;
    cardOriginY?: number;
    lastCardX?: number;
    lastCardY?: number;
    pointerId?: number;
    captured?: boolean;
    moved: boolean;
  } | null>(null);
  // A head-drag that moved must not also fire the title link's navigation.
  const dragSuppressClickRef = useRef(false);
  const [panning, setPanning] = useState(false);
  // Named for the gesture, not the key: V70-A5 moved the modifier from
  // Space (now the global leader) to Alt.
  const [spaceDown, setSpaceDown] = useState(false);
  // Live drag position (avoid dirty PUT on every move — commit on pointerup).
  const [livePos, setLivePos] = useState<Record<string, { x: number; y: number }>>({});
  const [liveView, setLiveView] = useState(payload.view);

  useEffect(() => {
    if (panRef.current) return; // don't clobber an in-flight pan/zoom
    viewRef.current = payload.view;
    setLiveView(payload.view);
  }, [payload.view.x, payload.view.y, payload.view.zoom]);

  // V70-A5 — the hold-to-pan modifier MOVED from Space to Alt.
  //
  // D2: "Space is only the leader, globally." A canvas that swallowed Space
  // was the one surface where the leader — and therefore the whole `Space …`
  // chord family, the drawer tabs and the rehearsal overlay — silently did
  // not exist. Alt is free here (the reader's vim layer never touches Alt,
  // `vimReader.ts`'s guard bails on it) and is the pan modifier Figma and
  // Excalidraw already train.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "Alt" && !e.repeat) {
        const t = e.target as HTMLElement | null;
        if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
        e.preventDefault();
        setSpaceDown(true);
      }
    }
    function onKeyUp(e: KeyboardEvent) {
      if (e.key === "Alt") setSpaceDown(false);
    }
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
    };
  }, []);

  function onPointerDown(e: ReactPointerEvent) {
    dragSuppressClickRef.current = false;
    const target = e.target as HTMLElement;
    const cardEl = target.closest("[data-kbc-canvas-card]") as HTMLElement | null;
    const isHead = !!target.closest("[data-kbc-canvas-card-head]");
    const middle = e.button === 1;
    const wantPan = middle || spaceDown || (!cardEl && e.button === 0);

    if (wantPan && e.button !== 2) {
      e.preventDefault();
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      panRef.current = {
        mode: "pan",
        startX: e.clientX,
        startY: e.clientY,
        originX: liveView.x,
        originY: liveView.y,
        moved: false,
      };
      setPanning(true);
      return;
    }

    if (cardEl && isHead && e.button === 0 && !spaceDown) {
      const key = cardEl.getAttribute("data-kbc-canvas-card");
      if (!key) return;
      e.preventDefault();
      const frag = payload.fragments.find((f) => fragmentKey(f) === key);
      if (!frag) return;
      onSelect(key);
      // Capture is deferred to the first real move (onPointerMove): capturing
      // here would retarget the post-gesture click to the surface, breaking
      // plain clicks on the head's title link.
      panRef.current = {
        mode: "drag-card",
        startX: e.clientX,
        startY: e.clientY,
        originX: liveView.x,
        originY: liveView.y,
        cardKey: key,
        cardOriginX: frag.x,
        cardOriginY: frag.y,
        pointerId: e.pointerId,
        moved: false,
      };
    } else if (!cardEl && e.button === 0) {
      onSelect(null);
    }
  }

  function onPointerMove(e: ReactPointerEvent) {
    const p = panRef.current;
    if (!p) return;
    const dx = e.clientX - p.startX;
    const dy = e.clientY - p.startY;
    if (Math.abs(dx) + Math.abs(dy) > 2) p.moved = true;
    if (p.mode === "drag-card" && p.moved && !p.captured && p.pointerId != null) {
      surfaceRef.current?.setPointerCapture(p.pointerId);
      p.captured = true;
    }
    if (p.mode === "pan") {
      const next = { ...viewRef.current, x: p.originX + dx, y: p.originY + dy };
      // Keep viewRef in sync so pointerup can commit without stale state.
      viewRef.current = next;
      setLiveView(next);
    } else if (p.mode === "drag-card" && p.cardKey != null) {
      const z = viewRef.current.zoom || 1;
      const nx = (p.cardOriginX ?? 0) + dx / z;
      const ny = (p.cardOriginY ?? 0) + dy / z;
      p.lastCardX = nx;
      p.lastCardY = ny;
      setLivePos({ [p.cardKey]: { x: nx, y: ny } });
    }
  }

  function onPointerUp() {
    const p = panRef.current;
    panRef.current = null;
    setPanning(false);
    if (!p) return;
    if (p.mode === "pan" && p.moved) {
      onViewChange({ ...viewRef.current });
    } else if (p.mode === "drag-card" && p.cardKey && p.moved) {
      dragSuppressClickRef.current = true;
      const x = p.lastCardX ?? p.cardOriginX ?? 0;
      const y = p.lastCardY ?? p.cardOriginY ?? 0;
      onMove(p.cardKey, x, y);
      setLivePos({});
    } else {
      setLivePos({});
    }
  }

  function onWheel(e: ReactWheelEvent) {
    if (!e.ctrlKey && !e.metaKey) return;
    e.preventDefault();
    const el = surfaceRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    const v = viewRef.current;
    const factor = e.deltaY < 0 ? 1.08 : 1 / 1.08;
    const nextZoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, v.zoom * factor));
    // Zoom toward cursor.
    const wx = (mx - v.x) / v.zoom;
    const wy = (my - v.y) / v.zoom;
    const next = {
      zoom: nextZoom,
      x: mx - wx * nextZoom,
      y: my - wy * nextZoom,
    };
    viewRef.current = next;
    setLiveView(next);
    onViewChange(next);
  }

  // Unique paths for file fetches.
  const paths = useMemo(() => {
    const s = new Set<string>();
    for (const f of payload.fragments) s.add(f.path);
    return [...s];
  }, [payload.fragments]);

  const fileQueries = useQueries({
    queries: paths.map((path) => ({
      queryKey: ["file", repo, path, null] as const,
      queryFn: () => fetchFile(repo, path),
      staleTime: 60_000,
    })),
  });

  const fileByPath = useMemo(() => {
    const m = new Map<string, (typeof fileQueries)[0]["data"]>();
    paths.forEach((p, i) => {
      m.set(p, fileQueries[i]?.data);
    });
    return m;
  }, [paths, fileQueries]);

  // Hierarchy for edges + neighbor add (callers/callees per fragment).
  const fragKeys = useMemo(() => {
    const m = new Map<string, CanvasFragment>();
    for (const f of payload.fragments) m.set(fragmentKey(f), f);
    return m;
  }, [payload.fragments]);

  const hierQueries = useQueries({
    queries: payload.fragments.map((f) => {
      const key = fragmentKey(f);
      const file = fileByPath.get(f.path);
      const resolved = file?.symbols
        ? resolveFragmentSymbol(f, file.symbols)
        : null;
      const line = resolved?.line_start ?? f.line;
      const enabled = !!file && !!resolved;
      return {
        queryKey: ["canvas-hier", repo, f.path, line, key] as const,
        queryFn: async () => {
          const [callers, callees] = await Promise.all([
            fetchHierarchyCallers({ repo, path: f.path, line, col: 0 }),
            fetchHierarchyCallees({ repo, path: f.path, line, col: 0 }),
          ]);
          return { key, callers, callees };
        },
        enabled,
        staleTime: 120_000,
        retry: false,
      };
    }),
  });

  const { calleesByKey, callersByKey } = useMemo(() => {
    const calleesByKey = new Map<
      string,
      Array<{ path: string; name: string; line?: number; class?: string }>
    >();
    const callersByKey = new Map<
      string,
      Array<{ path: string; name: string; line?: number; class?: string }>
    >();
    for (const q of hierQueries) {
      const data = q.data;
      if (!data) continue;
      const callees = data.callees.callees
        .filter((c) => c.target)
        .map((c) => ({
          path: c.target!.path,
          name: c.name,
          line: c.target!.line,
          class: c.class,
        }));
      calleesByKey.set(data.key, callees);

      const callers: Array<{ path: string; name: string; line?: number; class?: string }> = [];
      for (const g of data.callers.callers) {
        const name = g.enclosing?.name;
        if (!name) continue;
        callers.push({
          path: g.path,
          name,
          line: g.enclosing?.line ?? g.sites[0]?.line,
          class: g.sites[0]?.class,
        });
      }
      callersByKey.set(data.key, callers);
    }
    return { calleesByKey, callersByKey };
  }, [hierQueries]);

  const edges = useMemo(() => {
    const keys = new Map(
      [...fragKeys.entries()].map(([k, f]) => [k, { path: f.path, symbol: f.symbol, line: f.line }]),
    );
    return edgesFromHierarchy(keys, calleesByKey, callersByKey);
  }, [fragKeys, calleesByKey, callersByKey]);

  const posOf = (f: CanvasFragment) => {
    const k = fragmentKey(f);
    return livePos[k] ?? { x: f.x, y: f.y };
  };

  // SVG edge endpoints at card centers.
  const edgeLines = edges.map((e) => {
    const from = fragKeys.get(e.fromKey);
    const to = fragKeys.get(e.toKey);
    if (!from || !to) return null;
    const fp = posOf(from);
    const tp = posOf(to);
    const fw = from.w && from.w > 0 ? from.w : CANVAS_CARD_W;
    const x1 = fp.x + fw;
    const y1 = fp.y + CANVAS_CARD_H / 2;
    const x2 = tp.x;
    const y2 = tp.y + CANVAS_CARD_H / 2;
    // Elbow: mid-x.
    const mx = (x1 + x2) / 2;
    const d = `M ${x1} ${y1} L ${mx} ${y1} L ${mx} ${y2} L ${x2} ${y2}`;
    return { ...e, d };
  });

  return (
    <>
      <div className="kbc-canvas__toolbar" data-kbc-canvas-toolbar>
        <button
          type="button"
          data-kbc-canvas-add-symbol
          onClick={() => onSearchOpen(!searchOpen)}
        >
          Add symbol
        </button>
        {searchOpen && (
          <input
            type="search"
            value={searchQ}
            onChange={(e) => onSearchQ(e.target.value)}
            placeholder="Search symbols…"
            aria-label="search symbols to add"
            data-kbc-canvas-search
            autoFocus
          />
        )}
        <span className="kbc-canvas__muted">
          {payload.fragments.length} fragment{payload.fragments.length === 1 ? "" : "s"}
        </span>
      </div>

      {searchOpen && (searchHits.length > 0 || searchLoading || searchQ.trim()) && (
        <div className="kbc-canvas__search-hits" data-kbc-canvas-search-hits role="listbox">
          {searchLoading && <div className="kbc-canvas__muted">Searching…</div>}
          {!searchLoading && searchHits.length === 0 && searchQ.trim() && (
            <div className="kbc-canvas__muted">No symbols</div>
          )}
          {searchHits.map((hit) => (
            <button
              key={`${hit.path}:${hit.name}:${hit.line_start}`}
              type="button"
              className="kbc-canvas__search-hit"
              data-kbc-canvas-search-hit
              role="option"
              onClick={() => onPickHit(hit)}
            >
              <strong>{hit.name}</strong>
              <span className="kbc-canvas__search-hit-path">
                {hit.path}:{hit.line_start}
              </span>
            </button>
          ))}
        </div>
      )}

      <div
        ref={surfaceRef}
        className={"kbc-canvas__surface" + (panning ? " kbc-canvas__surface--panning" : "")}
        data-kbc-canvas-surface
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onWheel={onWheel}
        // Anchors/images inside cards start NATIVE HTML5 drag on
        // mousedown+move, which fires pointercancel and silently kills
        // the custom card drag (moved=false ⇒ no commit). The canvas
        // owns all drag gestures — suppress native DnD wholesale.
        onDragStart={(e) => e.preventDefault()}
      >
        <div
          className="kbc-canvas__world"
          data-kbc-canvas-world
          style={{
            transform: `translate(${liveView.x}px, ${liveView.y}px) scale(${liveView.zoom})`,
          }}
        >
          <svg
            className="kbc-canvas__edges"
            width={8000}
            height={8000}
            data-kbc-canvas-edges
          >
            {edgeLines.map((el) => {
              if (!el) return null;
              return (
                <path
                  key={`${el.fromKey}->${el.toKey}`}
                  className={`kbc-canvas__edge kbc-canvas__edge--${el.class}`}
                  d={el.d}
                  data-kbc-canvas-edge={el.kind}
                >
                  <title>
                    {el.kind} · {el.class}
                  </title>
                </path>
              );
            })}
          </svg>

          {payload.fragments.map((f) => {
            const key = fragmentKey(f);
            const pos = posOf(f);
            const file = fileByPath.get(f.path);
            const resolved = file?.symbols ? resolveFragmentSymbol(f, file.symbols) : null;
            const stale = !!file && !resolved;
            const selected = selectedKey === key;
            const w = f.w && f.w > 0 ? f.w : CANVAS_CARD_W;
            const callees = calleesByKey.get(key) ?? [];
            const callers = callersByKey.get(key) ?? [];

            return (
              <article
                key={key}
                className={
                  "kbc-canvas__card" +
                  (selected ? " kbc-canvas__card--selected" : "") +
                  (stale ? " kbc-canvas__card--stale" : "")
                }
                style={{ left: pos.x, top: pos.y, width: w }}
                data-kbc-canvas-card={key}
                data-kbc-canvas-card-path={f.path}
                data-kbc-canvas-card-symbol={f.symbol}
                data-stale={stale ? "1" : "0"}
                onClick={(e) => {
                  e.stopPropagation();
                  onSelect(key);
                }}
              >
                <div className="kbc-canvas__card-head" data-kbc-canvas-card-head>
                  <Link
                    className="kbc-canvas__card-link"
                    to={readerUrl(repo, f.path, undefined, resolved?.line_start ?? f.line)}
                    title={`${f.path}:${f.symbol}`}
                    data-kbc-canvas-card-link
                    onClick={(e) => {
                      e.stopPropagation();
                      if (dragSuppressClickRef.current) {
                        dragSuppressClickRef.current = false;
                        e.preventDefault();
                      }
                    }}
                  >
                    {f.path}:{f.symbol}
                  </Link>
                  <div className="kbc-canvas__card-actions">
                    {callers.length > 0 && (
                      <button
                        type="button"
                        title="Add callers"
                        data-kbc-canvas-add-callers
                        onClick={(e) => {
                          e.stopPropagation();
                          for (const c of callers) {
                            onAddNeighbor(
                              f,
                              { path: c.path, name: c.name, line_start: c.line ?? 1 },
                              "caller",
                            );
                          }
                        }}
                        onPointerDown={(e) => e.stopPropagation()}
                      >
                        +callers
                      </button>
                    )}
                    {callees.length > 0 && (
                      <button
                        type="button"
                        title="Add callees"
                        data-kbc-canvas-add-callees
                        onClick={(e) => {
                          e.stopPropagation();
                          for (const c of callees) {
                            onAddNeighbor(
                              f,
                              { path: c.path, name: c.name, line_start: c.line ?? 1 },
                              "callee",
                            );
                          }
                        }}
                        onPointerDown={(e) => e.stopPropagation()}
                      >
                        +callees
                      </button>
                    )}
                    <button
                      type="button"
                      title="Remove fragment"
                      aria-label="Remove fragment"
                      data-kbc-canvas-card-remove
                      onClick={(e) => {
                        e.stopPropagation();
                        onRemove(key);
                      }}
                      onPointerDown={(e) => e.stopPropagation()}
                    >
                      <Icon.X />
                    </button>
                  </div>
                </div>
                {!file ? (
                  <div className="kbc-canvas__card-loading">Loading…</div>
                ) : stale ? (
                  <div className="kbc-canvas__card-stale" data-kbc-canvas-card-stale>
                    stale — symbol not found at HEAD
                  </div>
                ) : (
                  <pre className="kbc-canvas__card-body" data-kbc-canvas-card-body>
                    {sliceSymbolSource(
                      file.content,
                      resolved!.line_start,
                      resolved!.line_end,
                    )}
                  </pre>
                )}
              </article>
            );
          })}
        </div>
      </div>
    </>
  );
}
