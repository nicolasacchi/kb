// `/r/{repo}/~boards/{slug}` — ONE board (V74-L2, D10).
//
// The shell: it owns the queries, the URL state and the command handlers, and
// hands regions that own none of it (`BoardCanvas`, `BoardThreadPanel`) their
// props — the shape `Reader.tsx` got in V70 and `ReviewDiff.tsx` in V73-K2b.
//
// **The URL is the only view state that survives a reload**, and there is
// exactly one writer for each param (`setParam`, `replace: true` — a view knob
// is not a navigation step). `?step=` is the walkthrough's position, so a
// walkthrough is shareable and a reload lands on the same card.
//
// **Every geometry comes from `lib/boardLayout.ts`.** This file never computes
// an `x`. The camera for a step is derived from the layout, so re-laying the
// board can never leave it pointing at nothing.
//
// **Every count and every state comes from the wire.** The honesty strip is
// `honesty`'s own numbers (`boardCensus`), the drift report is the sweep
// route's, and an orphan is a card on the surface like any other.
//
// **The four mutations are loopback-only** (`accept`/`archive`/`apply` for a
// pin or a thread link). `lib/loopback.ts` decides what to OFFER; the daemon
// still decides what to allow, and a refusal renders its own message.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import type { BoardNode } from "../api/types";
import { ApiError } from "../api/client";
import BoardCanvas from "../components/boards/BoardCanvas";
import BoardThreadPanel from "../components/boards/BoardThreadPanel";
import PeekPanel from "../components/peek/PeekPanel";
import { Icon } from "../components/icons";
import { useCommandHandlers, useCommandScope } from "../commands/CommandRoot";
import { useAcceptBoard, useApplyBoard, useBoard, useBoardSweep } from "../hooks/useBoards";
import { useIdentPeek } from "../hooks/useIdentPeek";
import { useLoopback } from "../hooks/useLoopback";
import { boardCensus, hasContextExpansion, readingOrder } from "../lib/boards";
import { composePin } from "../lib/boardDoc";
import { cameraFor, layoutBoard } from "../lib/boardLayout";
import {
  BOARD_CTX_PARAM,
  BOARD_LIVE_PARAM,
  BOARD_STEP_PARAM,
  boardsHref,
  parseBoardFlag,
  parseStep,
} from "../lib/boardsUrl";
import { codeUrl, mergeCurrentSearch } from "../lib/codeUrl";
import { toast } from "../lib/toast";
import "../styles/boards.css";

/// How long `p` (play) waits between steps. A visible indicator names it, and
/// ANY key stops the run — an autoplay that a keystroke cannot interrupt is the
/// thing a reader resents most.
const AUTOPLAY_MS = 6000;

function refusalMessage(err: unknown): string {
  if (err instanceof ApiError && (err.status === 403 || err.status === 404)) {
    return `${err.message} — board mutations are LOOPBACK-ONLY; they only work when kb-code is reached at 127.0.0.1.`;
  }
  return err instanceof Error ? err.message : String(err);
}

export default function BoardDetail() {
  const { repo = "", slug = "" } = useParams<{ repo: string; slug: string }>();
  const [params] = useSearchParams();
  const navigate = useNavigate();

  const live = parseBoardFlag(params.get(BOARD_LIVE_PARAM));
  const ctx = parseBoardFlag(params.get(BOARD_CTX_PARAM));
  const board = useBoard(repo, slug, { ctx, live });
  const apply = useApplyBoard(repo);
  const accept = useAcceptBoard(repo);
  const loopback = useLoopback();

  // V76-R4d.2 — react-router 7 wraps every navigation state update in
  // React.startTransition (no opt-out), so a controlled checkbox bound
  // DIRECTLY to a search param stays at the last committed render's value
  // until the transition lands: React's restoreControlledState snaps the
  // DOM node back right after the change event. The house pattern (the
  // Stacks `all` toggle, V76-R4d round 2) is LOCAL OPTIMISTIC state: each
  // box flips on the same tick as the change and reconciles with the URL
  // when the navigation commits (the effects below). The board QUERY
  // keeps reading the URL — the URL stays the source of truth for data;
  // only each checkbox's own checked rendering is optimistic.
  const [liveOptimistic, setLiveOptimistic] = useState(live);
  const [ctxOptimistic, setCtxOptimistic] = useState(ctx);
  useEffect(() => {
    setLiveOptimistic(live);
  }, [live]);
  useEffect(() => {
    setCtxOptimistic(ctx);
  }, [ctx]);

  const [folded, setFolded] = useState<ReadonlySet<string>>(new Set());
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const [threadNode, setThreadNode] = useState<BoardNode | null>(null);
  const [sweepOpen, setSweepOpen] = useState(false);
  const [playing, setPlaying] = useState(false);
  const sweep = useBoardSweep(repo, slug, sweepOpen);
  const peek = useIdentPeek();

  const data = board.data;
  const steps = useMemo(() => data?.steps ?? [], [data]);
  const stepIndex = parseStep(params.get(BOARD_STEP_PARAM), steps.length);
  const walkthrough = stepIndex !== null;

  const layout = useMemo(
    () =>
      layoutBoard({
        nodes: data?.nodes ?? [],
        edges: data?.edges ?? [],
        steps: steps.map((s) => s.node),
        pins: data?.pins ?? {},
        nodeCap: data?.honesty.budget.max_nodes ?? 200,
      }),
    [data, steps],
  );

  const order = useMemo(() => (data ? readingOrder(data) : []), [data]);

  // V76-R4d.3 — merges onto `window.location.search` AT CALL TIME
  // (`mergeCurrentSearch`), never the render-time `params` snapshot: under
  // react-router 7's deferred commits a snapshot-based second write re-emits
  // the OLD params and silently drops a first write still in flight.
  const setParam = useCallback(
    (key: string, value: string | null) => {
      navigate(
        {
          search: mergeCurrentSearch((next) => {
            if (value === null) next.delete(key);
            else next.set(key, value);
          }),
        },
        { replace: true },
      );
    },
    [navigate],
  );

  const goToStep = useCallback(
    (index: number | null) => {
      if (index === null || steps.length === 0) {
        setParam(BOARD_STEP_PARAM, null);
        setPlaying(false);
        return;
      }
      const clamped = Math.min(Math.max(index, 0), steps.length - 1);
      setParam(BOARD_STEP_PARAM, String(clamped + 1));
      setFocusedId(steps[clamped]?.node ?? null);
    },
    [setParam, steps],
  );

  // Autoplay: a plain interval that ANY keystroke cancels (the listener below).
  // Deliberately not a transition or an animation — `BoardCanvas` honours
  // `prefers-reduced-motion` for the camera itself.
  useEffect(() => {
    if (!playing || !walkthrough) return;
    const t = window.setInterval(() => {
      goToStep((stepIndex ?? 0) + 1);
    }, AUTOPLAY_MS);
    return () => window.clearInterval(t);
  }, [playing, walkthrough, stepIndex, goToStep]);

  useEffect(() => {
    if (!playing) return;
    const stop = () => setPlaying(false);
    window.addEventListener("keydown", stop);
    return () => window.removeEventListener("keydown", stop);
  }, [playing]);

  const cameraNode = walkthrough ? layout.byId.get(steps[stepIndex]?.node ?? "") : undefined;
  const [viewport, setViewport] = useState({ w: 1200, h: 700 });
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = surfaceRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    setViewport({ w: rect.width || 1200, h: rect.height || 700 });
  }, [walkthrough]);

  const camera = useMemo(
    () => (walkthrough ? cameraFor(cameraNode, viewport.w, viewport.h, 1) : null),
    [walkthrough, cameraNode, viewport.w, viewport.h],
  );

  // --- card actions --------------------------------------------------------

  const toggleFold = useCallback((id: string) => {
    setFolded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const setFold = useCallback(
    (id: string, value: boolean) => {
      setFolded((prev) => {
        const next = new Set(prev);
        if (value) next.add(id);
        else next.delete(id);
        return next;
      });
    },
    [],
  );

  const foldAll = useCallback(
    (value: boolean) => setFolded(value ? new Set(order.map((n) => n.id)) : new Set()),
    [order],
  );

  const focusStep = useCallback(
    (delta: number) => {
      if (order.length === 0) return;
      const at = order.findIndex((n) => n.id === focusedId);
      const next = at < 0 ? (delta > 0 ? 0 : order.length - 1) : at + delta;
      const clamped = Math.min(Math.max(next, 0), order.length - 1);
      const id = order[clamped]?.id ?? null;
      setFocusedId(id);
      if (id) {
        document
          .querySelector<HTMLElement>(`[data-kbc-board-node="${CSS.escape(id)}"]`)
          ?.focus({ preventScroll: true });
      }
    },
    [focusedId, order],
  );

  const focusedNode = order.find((n) => n.id === focusedId) ?? null;

  const setContext = useCallback(
    (want: boolean) => {
      const node = focusedNode;
      if (!node) return;
      if (want && !ctx) {
        // The expansion is FETCHED, never synthesised: the daemon only sends
        // `context_snippet` when the read asked for it, so `+` turns the read
        // on rather than inventing surrounding lines.
        setParam(BOARD_CTX_PARAM, "1");
        return;
      }
      if (want && !hasContextExpansion(node)) {
        toast.warn("this card has no context range — its author gave it only a primary range");
        return;
      }
      setExpanded((prev) => {
        const next = new Set(prev);
        if (want) next.add(node.id);
        else next.delete(node.id);
        return next;
      });
    },
    [ctx, focusedNode, setParam],
  );

  const pin = useCallback(
    async (node: BoardNode, at: { x: number; y: number }) => {
      if (!data) return;
      const composed = composePin(data, node.id, at);
      if (composed.coordinateViolations.length > 0) {
        toast.err(`refusing to send: ${composed.coordinateViolations.join(", ")}`);
        return;
      }
      try {
        await apply.mutateAsync({ doc: composed.doc });
        toast.ok(`pinned ${node.id} — the layout will leave it where it is`);
      } catch (e) {
        toast.err(refusalMessage(e));
      }
    },
    [apply, data],
  );

  const unpin = useCallback(
    async (node: BoardNode) => {
      if (!data) return;
      const composed = composePin(data, node.id, null);
      try {
        await apply.mutateAsync({ doc: composed.doc });
        toast.ok(`released ${node.id}'s pin — the layout engine places it again`);
      } catch (e) {
        toast.err(refusalMessage(e));
      }
    },
    [apply, data],
  );

  const acceptThis = useCallback(async () => {
    try {
      const out = await accept.mutateAsync(slug);
      toast.ok(`${out.slug} is ${out.status}`);
    } catch (e) {
      toast.err(refusalMessage(e));
    }
  }, [accept, slug]);

  // --- keyboard ------------------------------------------------------------

  // `mode.active` is published alongside `walkthrough` deliberately: it is the
  // context key the EXISTING `dismiss.mode` rung reads ("a tour / resize
  // submode / canvas selection is active", dismiss_order 7), and a walkthrough
  // is exactly that. Without it, Escape would need a second Escape row — a
  // second home for one keystroke, which the registry exists to prevent.
  useCommandScope("board", {
    board: "boards",
    walkthrough,
    "mode.active": walkthrough,
  });
  useCommandHandlers({
    "boards.card-next": () => focusStep(1),
    "boards.card-prev": () => focusStep(-1),
    "boards.card-open": () => {
      const node = focusedNode;
      if (!node) return;
      const el = document.querySelector<HTMLAnchorElement>(
        `[data-kbc-board-node="${CSS.escape(node.id)}"] a[href]`,
      );
      if (el) el.click();
      else toast.warn(`${node.id} has no honest destination — its address is shown, not linked`);
    },
    "boards.fold": () => focusedId && setFold(focusedId, true),
    "boards.unfold": () => focusedId && setFold(focusedId, false),
    "boards.fold-toggle": () => focusedId && toggleFold(focusedId),
    "boards.fold-all": () => foldAll(true),
    "boards.unfold-all": () => foldAll(false),
    "boards.context-more": () => setContext(true),
    "boards.context-less": () => setContext(false),
    "boards.thread": () => focusedNode && setThreadNode(focusedNode),
    "boards.pin": () => {
      const node = focusedNode;
      const at = node ? layout.byId.get(node.id) : undefined;
      if (node && at) void pin(node, { x: at.x, y: at.y });
    },
    "boards.unpin": () => focusedNode && void unpin(focusedNode),
    "boards.sweep": () => setSweepOpen((v) => !v),
    "boards.accept": () => void acceptThis(),
    "boards.walkthrough": () => {
      if (steps.length === 0) {
        toast.warn("this board declares no steps — a walkthrough needs a reading order");
        return;
      }
      goToStep(0);
    },
    "walkthrough.next": () => goToStep((stepIndex ?? 0) + 1),
    "walkthrough.prev": () => goToStep((stepIndex ?? 0) - 1),
    "walkthrough.play": () => setPlaying((v) => !v),
    // `Escape` leaves the walkthrough through the EXISTING dismiss stack:
    // `dismiss.mode` (order 7, `when: mode.active`) is already the home for "a
    // tour / resize submode / canvas selection is active". A second Escape row
    // would be a second home for one keystroke.
    "dismiss.mode": () => goToStep(null),
  });

  if (board.isLoading) return <div className="kbc-reader__hint">Loading board…</div>;
  if (board.error) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error" data-kbc-board-error>
        {(board.error as Error).message}
      </div>
    );
  }
  if (!data) return null;

  const step = stepIndex !== null ? steps[stepIndex] : null;

  return (
    <div className="kbc-board" data-kbc-board={data.slug}>
      <header className="kbc-board__head">
        <div className="kbc-board__title-row">
          <Link to={boardsHref(repo)} className="kbc-board__back">
            <Icon.ArrowLeft /> Boards
          </Link>
          <h1 className="kbc-board__title">{data.title}</h1>
          <span
            className={`kbc-boards__status kbc-boards__status--${data.status}`}
            data-kbc-board-status={data.status}
          >
            {data.status}
          </span>
          {data.status === "pending" && loopback && (
            <button
              type="button"
              onClick={() => void acceptThis()}
              disabled={accept.isPending}
              data-kbc-board-accept
              title="An agent-proposed board is pending until a human accepts it (loopback only)."
            >
              <Icon.Check /> Accept
            </button>
          )}
          {data.status === "pending" && !loopback && (
            <span className="kbc-board__gate" data-kbc-board-accept-gate>
              accepting is loopback-only
            </span>
          )}
        </div>
        <ul className="kbc-board__census" data-kbc-board-census>
          {boardCensus(data).map((line, i) => (
            <li key={i}>{line}</li>
          ))}
          {data.honesty.notes.map((n, i) => (
            <li key={`n${i}`} data-kbc-board-honesty-note>
              {n}
            </li>
          ))}
        </ul>
        <div className="kbc-board__controls">
          <label>
            <input
              type="checkbox"
              checked={liveOptimistic}
              onChange={(e) => {
                setLiveOptimistic(e.target.checked);
                setParam(BOARD_LIVE_PARAM, e.target.checked ? "1" : null);
              }}
              data-kbc-board-live
            />
            Live query counts
            <span className="kbc-board__caption">
              — re-runs every query card on this read (a full search per card)
            </span>
          </label>
          <label>
            <input
              type="checkbox"
              checked={ctxOptimistic}
              onChange={(e) => {
                setCtxOptimistic(e.target.checked);
                setParam(BOARD_CTX_PARAM, e.target.checked ? "1" : null);
              }}
              data-kbc-board-ctx
            />
            Fetch ± context
            <span className="kbc-board__caption">— the daemon reads each card's context range</span>
          </label>
          <button type="button" onClick={() => foldAll(true)} data-kbc-board-fold-all>
            Fold all
          </button>
          <button type="button" onClick={() => foldAll(false)} data-kbc-board-unfold-all>
            Expand all
          </button>
          <button
            type="button"
            onClick={() => (walkthrough ? goToStep(null) : goToStep(0))}
            disabled={steps.length === 0}
            data-kbc-board-walkthrough
            title={
              steps.length === 0
                ? "this board declares no steps — a walkthrough needs a reading order"
                : "Walk the board's own reading order"
            }
          >
            <Icon.Spark /> {walkthrough ? "Leave walkthrough" : "Walkthrough"}
          </button>
          <button type="button" onClick={() => setSweepOpen((v) => !v)} data-kbc-board-sweep>
            <Icon.Refresh /> {sweepOpen ? "Hide drift" : "Check drift"}
          </button>
        </div>
      </header>

      {walkthrough && (
        <div className="kbc-board__walkthrough" data-kbc-board-walkthrough-bar>
          <span data-kbc-board-step-counter>
            step {(stepIndex ?? 0) + 1} of {steps.length}
          </span>
          <span className="kbc-board__caption" data-kbc-board-step-caption>
            {step?.caption ?? step?.node ?? ""}
          </span>
          <button type="button" onClick={() => goToStep((stepIndex ?? 0) - 1)} data-kbc-board-step-prev>
            Prev
          </button>
          <button type="button" onClick={() => goToStep((stepIndex ?? 0) + 1)} data-kbc-board-step-next>
            Next
          </button>
          <button type="button" onClick={() => setPlaying((v) => !v)} data-kbc-board-step-play>
            {playing ? "Pause" : "Play"}
          </button>
          {playing && (
            <span data-kbc-board-playing>
              playing — one step every {AUTOPLAY_MS / 1000}s; any key stops
            </span>
          )}
        </div>
      )}

      {sweepOpen && (
        <section className="kbc-board__sweep" data-kbc-board-sweep-panel>
          {sweep.isLoading ? (
            <p>Sweeping…</p>
          ) : sweep.error ? (
            <p className="kbc-boards__error">{(sweep.error as Error).message}</p>
          ) : (
            <ul>
              {(sweep.data?.boards[0]?.nodes ?? []).map((n) => (
                <li key={n.node} data-kbc-board-sweep-node={n.node}>
                  {n.node} — {n.state} ({n.reason}) {n.address}
                  {n.shifted_by ? ` · moved ${n.shifted_by}` : ""}
                  {n.delta ? ` · query ${n.delta > 0 ? "+" : ""}${n.delta}` : ""}
                  {n.stale_pin ? " · STALE PIN" : ""}
                </li>
              ))}
              {sweep.data && !sweep.data.boards[0]?.drifted && <li>no drift</li>}
            </ul>
          )}
        </section>
      )}

      <div className="kbc-board__main" ref={surfaceRef}>
        <BoardCanvas
          repo={repo}
          nodes={data.nodes}
          edges={data.edges}
          layout={layout}
          folded={folded}
          expanded={expanded}
          focusedId={focusedId}
          camera={camera}
          onToggleFold={toggleFold}
          onThread={setThreadNode}
          onPin={loopback ? (node, at) => void pin(node, at) : undefined}
          onUnpin={loopback ? (node) => void unpin(node) : undefined}
          onFocus={setFocusedId}
          onSnippetClick={(node, lineIndex, lineText, col, at) => {
            if (!node.code) return;
            peek.openInSnippet({
              repo,
              path: node.code.path,
              snippetStart: node.code.range[0],
              lineIndex,
              lineText,
              col,
              anchor: at,
            });
          }}
        />
        {threadNode && (
          <aside className="kbc-board__rail" data-region="board-thread">
            <BoardThreadPanel board={data} node={threadNode} onClose={() => setThreadNode(null)} />
          </aside>
        )}
      </div>

      {peek.state.open && (
        <PeekPanel
          state={peek.state}
          currentRepo={repo}
          anchor={peek.anchor}
          onMove={peek.move}
          onActivate={(row) => {
            const loc = peek.rowLocation(row);
            peek.close();
            navigate(codeUrl({ repo: loc.repo, path: loc.path, line: loc.line }));
          }}
          onClose={peek.close}
        />
      )}
    </div>
  );
}
