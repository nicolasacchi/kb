import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { fetchTimeline } from "../api/client";
import { Icon } from "./icons";
import { dayBoundsUnix, unixToUtcDay } from "../lib/calendar";
import {
  brushFromIndices,
  brushSummary,
  buildLanes,
  indexOfDay,
  laneEventsInBrush,
  pivotUrl,
  planPivot,
  TRACK_ORDER,
  type Brush,
  type CanvasLane,
} from "../lib/timeline";
import type { TimelineTrack } from "../api/generated/TimelineTrack";
import { censusBump } from "../lib/census";
import { toast } from "../lib/toast";
import { useSavedQueries } from "../hooks/useSavedQueries";
import { useConfirm } from "./ConfirmProvider";

// W3.C-b — the multi-facet reflection canvas (`?view=canvas`): four
// synchronized UTC-day tracks — creation · reading · sessions · comments —
// sharing ONE brush, mounted as a gallery view beside `?view=history`.
//
// All the math lives in `lib/timeline.ts` (pure, clock-free, golden-tested);
// this file is wiring. What it owns:
//
//  - ONE `["timeline", kb, from, to]` query, staleTime Infinity, with the
//    window PINNED AT MOUNT (`useMemo(..., [])`) — an unpinned trailing
//    window would change the query's identity on every render tick, exactly
//    the rule ActivityCalendar.tsx already carries.
//  - The brush is pure CLIENT state over the already-fetched buckets:
//    dragging/keying it triggers NO refetch. Only *activating the pivot*
//    re-asks the route, and only because the wire resolves artifact ids per
//    REQUEST WINDOW rather than per day — a full-window pivot is a cache hit
//    on the mounted key (identical `from`/`to`, both snapped to whole UTC
//    days), so the common case is free.
//  - NO SSE subscription of its own (invariants #23/#24): freshness comes
//    from the three `["timeline", …]` invalidations already wired into the
//    existing history.recorded / artifact-churn / session.captured handlers
//    in `api/queryClient.ts`.
//
// The brush is operable THREE ways: pointer drag over any lane, KEYBOARD
// (two `role="slider"` handles — arrows/PageUp/PageDown/Home/End), and two
// plain `<input type="date">`s as the accessible fallback. The keyboard path
// is the one e2e drives (pointer drag is flaky in Playwright).
//
// CALM-COMPUTING CONTRACT (the kb-resurface ruling, same posture as the
// activity calendar this sits beside): density and evidence ONLY — no
// streaks, no "best day", no goals, no completion, nothing framed as an
// achievement. Every number here is a plain count, and every lane's ramp is
// relative to ITS OWN busiest day so a 3-comment track stays visible beside
// a 300-open one. The sessions lane reads EMPTY on an ordinary content
// corpus (session rows live in the kb whose corpus holds the transcript) —
// the server's own lane label says so and the lane stays on screen; hiding
// it would be the dishonest option.

/// Days in the pinned window. Denser than the calendar's 365 because these
/// are four stacked linear tracks, not a 7-row grid — a quarter reads at a
/// glance and keeps each day cell wide enough to hit with a pointer.
const WINDOW_DAYS = 90;
/// One PageUp/PageDown step on a brush handle.
const PAGE_STEP = 7;

// W3.C-c — SCENES: a named, restorable canvas view. A scene is a QUERY, not
// a collection (see `commands::queries`' doc comment for the full storage
// ruling): it rides the ALREADY-SHIPPED daemon-wide saved-query store
// (`useSavedQueries`, `/api/saved-queries`) as one row with `path: "/"` and
// a `search` string this file builds/parses — no new store, no `kind`
// discriminator on `SavedQuery` (a name-convention filter below is enough:
// a scene is any saved query whose search parses to `view=canvas` for
// THIS kb).
//
// The brush/track selection is pure client state (never written into the
// URL bar while dragging — see the header note above), so "save the
// current brush as a scene" can't just snapshot `window.location.search`
// the way the generic saved-query ribbon does; `sceneSearch` builds the
// string explicitly, and the mount-time effect below parses it back out
// when a scene chip navigates here. `cfrom`/`cto` deliberately reuse the
// `YYYY-MM-DD` day strings already used everywhere else in this file
// (never a re-derivation); a stale scene whose days have aged out of the
// rolling 90-day window degrades to "whole window" plus a plain note,
// never a silent wrong brush.
function sceneSearch(kb: string, tracks: readonly TimelineTrack[], brush: Brush | null): string {
  const p = new URLSearchParams();
  p.set("view", "canvas");
  p.set("kb", kb);
  p.set("ctracks", tracks.join(","));
  if (brush) {
    p.set("cfrom", brush.fromDay);
    p.set("cto", brush.toDay);
  }
  return `?${p.toString()}`;
}

type Props = { kb: string };

export default function ReflectionCanvas({ kb }: Props) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const confirm = useConfirm();
  const {
    queries: savedQueries,
    save: saveQuery,
    remove: removeQuery,
  } = useSavedQueries();

  // The window is pinned at mount and snapped to whole UTC days, so (a) the
  // query key is stable across renders and (b) a full-window brush produces
  // byte-identical `from`/`to` bounds — which makes the pivot's fetch a
  // cache hit instead of a second request.
  const [fromUnix, toUnix] = useMemo(() => {
    const today = dayBoundsUnix(unixToUtcDay(Math.floor(Date.now() / 1000)));
    const first = dayBoundsUnix(
      unixToUtcDay(today.from - (WINDOW_DAYS - 1) * 86400),
    );
    return [first.from, today.to];
  }, []);

  const timelineQ = useQuery({
    queryKey: ["timeline", kb, fromUnix, toUnix],
    queryFn: ({ signal }) => fetchTimeline(kb, { from: fromUnix, to: toUnix }, signal),
    staleTime: Infinity,
  });

  const lanes = useMemo(
    () => buildLanes(timelineQ.data, fromUnix, toUnix),
    [timelineQ.data, fromUnix, toUnix],
  );
  const days = lanes.days;
  const last = Math.max(0, days.length - 1);

  // null = the whole pinned window (the honest default: brush nothing, see
  // everything). Pure client state — never a fetch.
  const [sel, setSel] = useState<[number, number] | null>(null);
  const brush = useMemo(
    () => brushFromIndices(sel?.[0] ?? 0, sel?.[1] ?? last, days),
    [sel, last, days],
  );

  const [tracks, setTracks] = useState<TimelineTrack[]>([...TRACK_ORDER]);
  const [pivotNote, setPivotNote] = useState<string | null>(null);
  const [pivoting, setPivoting] = useState(false);

  const commitSel = useCallback((a: number, b: number) => {
    setSel([Math.min(a, b), Math.max(a, b)]);
    setPivotNote(null);
  }, []);

  // ── scenes: restore-on-mount ─────────────────────────────────────────────
  // A scene chip navigates here with `?view=canvas&kb=…&ctracks=…` (+
  // `cfrom`/`cto` unless the saved brush was the whole window). Applied
  // once per distinct set of restore params (the `appliedRef` guard) so it
  // never re-fires on every render, and never clobbers a user's own
  // in-progress brush after the initial application.
  const [searchParams] = useSearchParams();
  const restoreKey = [
    searchParams.get("cfrom") ?? "",
    searchParams.get("cto") ?? "",
    searchParams.get("ctracks") ?? "",
  ].join("|");
  const appliedRestoreRef = useRef<string | null>(null);
  useEffect(() => {
    if (restoreKey === "||") return; // nothing to restore
    if (appliedRestoreRef.current === restoreKey) return;
    // ids/day lookups need the fetched axis — wait for it rather than
    // guessing, and don't mark this key "applied" until we actually can.
    if (days.length === 0) return;
    appliedRestoreRef.current = restoreKey;

    const ctracksRaw = searchParams.get("ctracks");
    if (ctracksRaw) {
      const wanted = ctracksRaw
        .split(",")
        .filter((t): t is TimelineTrack =>
          (TRACK_ORDER as readonly string[]).includes(t),
        );
      if (wanted.length > 0) setTracks(wanted);
    }

    const cfrom = searchParams.get("cfrom");
    const cto = searchParams.get("cto");
    if (cfrom && cto) {
      const fi = indexOfDay(cfrom, days);
      const ti = indexOfDay(cto, days);
      if (fi != null && ti != null) {
        commitSel(fi, ti);
      } else {
        setPivotNote(
          `this scene's window (${cfrom} → ${cto}) has aged out of the current ${days.length}-day view — showing the whole window instead.`,
        );
      }
    }
  }, [restoreKey, days, searchParams, commitSel]);

  // ── pointer drag ────────────────────────────────────────────────────────
  const dragAnchor = useRef<number | null>(null);
  const indexFromEvent = useCallback(
    (el: HTMLElement, clientX: number): number => {
      const rect = el.getBoundingClientRect();
      if (rect.width <= 0 || days.length === 0) return 0;
      const frac = (clientX - rect.left) / rect.width;
      return Math.min(last, Math.max(0, Math.floor(frac * days.length)));
    },
    [days.length, last],
  );
  const onTrackPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    const i = indexFromEvent(e.currentTarget, e.clientX);
    dragAnchor.current = i;
    e.currentTarget.setPointerCapture(e.pointerId);
    commitSel(i, i);
  };
  const onTrackPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragAnchor.current == null) return;
    commitSel(dragAnchor.current, indexFromEvent(e.currentTarget, e.clientX));
  };
  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragAnchor.current == null) return;
    dragAnchor.current = null;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    censusBump("canvas.brush");
  };

  // ── keyboard handles ────────────────────────────────────────────────────
  const moveHandle = (which: "start" | "end", delta: number | "min" | "max") => {
    if (!brush) return;
    const cur = which === "start" ? brush.fromIndex : brush.toIndex;
    const nextRaw =
      delta === "min" ? 0 : delta === "max" ? last : cur + delta;
    const next = Math.min(last, Math.max(0, nextRaw));
    // Each handle clamps against the other rather than swapping — a slider
    // that jumps past its twin is disorienting under keyboard repeat.
    if (which === "start") commitSel(Math.min(next, brush.toIndex), brush.toIndex);
    else commitSel(brush.fromIndex, Math.max(next, brush.fromIndex));
    censusBump("canvas.brush");
  };
  const handleKey = (which: "start" | "end") => (e: React.KeyboardEvent) => {
    const map: Record<string, number | "min" | "max"> = {
      ArrowLeft: -1,
      ArrowDown: -1,
      ArrowRight: 1,
      ArrowUp: 1,
      PageDown: -PAGE_STEP,
      PageUp: PAGE_STEP,
      Home: "min",
      End: "max",
    };
    const step = map[e.key];
    if (step === undefined) return;
    e.preventDefault();
    moveHandle(which, step);
  };

  // ── date-input fallback ─────────────────────────────────────────────────
  const setDay = (which: "start" | "end", day: string) => {
    const i = indexOfDay(day, days);
    if (i == null || !brush) {
      // Refuse rather than clamp to an edge the operator didn't pick.
      setPivotNote(`${day} is outside this canvas's ${days.length}-day window.`);
      return;
    }
    if (which === "start") commitSel(Math.min(i, brush.toIndex), brush.toIndex);
    else commitSel(brush.fromIndex, Math.max(i, brush.fromIndex));
    censusBump("canvas.brush");
  };

  const toggleTrack = (track: TimelineTrack) => {
    setPivotNote(null);
    setTracks((cur) =>
      cur.includes(track) ? cur.filter((t) => t !== track) : [...cur, track],
    );
  };

  // ── the pivot ───────────────────────────────────────────────────────────
  //
  // Known up front ONLY when the brush is the whole pinned window (the
  // mounted response's ids already answer for exactly that window). For a
  // sub-window the plan is resolved on activation — see the fetch note at
  // the top of this file.
  const fullWindow = brush != null && brush.fromIndex === 0 && brush.toIndex === last;
  const knownPlan = useMemo(
    () => (fullWindow && brush && timelineQ.data ? planPivot(lanes, tracks, brush) : null),
    [fullWindow, brush, timelineQ.data, lanes, tracks],
  );

  const runPivot = async (b: Brush) => {
    setPivoting(true);
    setPivotNote(null);
    try {
      const res = await queryClient.fetchQuery({
        queryKey: ["timeline", kb, b.fromUnix, b.toUnix],
        queryFn: ({ signal }: { signal: AbortSignal }) =>
          fetchTimeline(kb, { from: b.fromUnix, to: b.toUnix }, signal),
        staleTime: Infinity,
      });
      const plan = planPivot(buildLanes(res, b.fromUnix, b.toUnix), tracks, b);
      const url = pivotUrl(kb, plan);
      if (plan.kind === "blocked" || url == null) {
        setPivotNote(plan.kind === "blocked" ? plan.reason : "no pivot for this brush.");
        return;
      }
      if (plan.kind === "window") toast.info(plan.reason);
      censusBump("canvas.pivot");
      navigate(url);
    } catch (e) {
      // #32 — a user action never swallows its failure.
      toast.err(`could not open this brush: ${String(e)}`);
    } finally {
      setPivoting(false);
    }
  };

  const blockedReason =
    knownPlan && knownPlan.kind === "blocked" ? knownPlan.reason : null;
  const pivotLabel = (() => {
    if (pivoting) return "opening…";
    if (knownPlan?.kind === "ids") {
      return `open ${knownPlan.ids.length} artifact${knownPlan.ids.length === 1 ? "" : "s"} in the gallery`;
    }
    if (knownPlan?.kind === "window") return "open this date window in the gallery";
    return "open this brush in the gallery";
  })();
  // Rendered on the anchor-ish button so a deep-link is still copyable when
  // the plan is known; a sub-window pivot has no href until it resolves.
  const knownHref = knownPlan ? pivotUrl(kb, knownPlan) : null;

  // ── scenes: list + save + delete ─────────────────────────────────────────
  // A scene is any saved query whose stored search parses to `view=canvas`
  // for THIS kb — a name-convention filter over the shared store rather
  // than a schema change (see the header comment on `sceneSearch`).
  const scenes = useMemo(
    () =>
      savedQueries.filter((q) => {
        if (q.path !== "/") return false;
        const p = new URLSearchParams(q.search);
        return p.get("view") === "canvas" && p.get("kb") === kb;
      }),
    [savedQueries, kb],
  );

  const onSaveScene = () => {
    const name = window.prompt("Save this brush as a scene…");
    if (!name || !name.trim()) return;
    const ok = saveQuery(name.trim(), {
      path: "/",
      search: sceneSearch(kb, tracks, brush),
    });
    // #32 — a user action never swallows its failure. `saveQuery` only
    // returns false on an empty (post-trim) name, already guarded above,
    // but the guard stays in case a future caller widens what's "empty".
    if (!ok) toast.err("scene name can't be empty");
  };

  const onDeleteScene = async (name: string) => {
    // #32 — the ONLY destructive prompt; never window.confirm.
    const ok = await confirm({
      title: "Delete this scene?",
      body: `"${name}" will be gone for good — this can't be undone.`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    removeQuery(name);
  };

  const overlayStyle =
    brush && days.length > 0
      ? {
          left: `${(brush.fromIndex / days.length) * 100}%`,
          width: `${((brush.toIndex - brush.fromIndex + 1) / days.length) * 100}%`,
        }
      : undefined;

  return (
    <section className="rcanvas" aria-label="reflection canvas">
      <header className="rcanvas__head">
        <h2 className="rcanvas__title">reflection</h2>
        <span className="rcanvas__sub mono">
          {days.length}-day window · four tracks, one brush
        </span>
      </header>

      {timelineQ.isLoading && (
        <p className="rcanvas__status" role="status">
          loading the timeline…
        </p>
      )}
      {timelineQ.isError && (
        <p className="rcanvas__status rcanvas__status--err" role="alert">
          could not load the timeline: {String(timelineQ.error)}
        </p>
      )}

      <div className="rcanvas__lanes">
        {overlayStyle && (
          <div className="rcanvas__overlay" style={overlayStyle} aria-hidden="true" />
        )}
        {lanes.lanes.map((lane) => (
          <Lane
            key={lane.track}
            lane={lane}
            brush={brush}
            selected={tracks.includes(lane.track)}
            onToggle={() => toggleTrack(lane.track)}
            onPointerDown={onTrackPointerDown}
            onPointerMove={onTrackPointerMove}
            onPointerUp={endDrag}
          />
        ))}
      </div>

      <div className="rcanvas__brush">
        <div className="rcanvas__handles">
          <div
            role="slider"
            tabIndex={0}
            aria-label="brush start"
            aria-valuemin={0}
            aria-valuemax={last}
            aria-valuenow={brush?.fromIndex ?? 0}
            aria-valuetext={brush?.fromDay ?? ""}
            className="rcanvas__handle"
            data-kb-brush="start"
            onKeyDown={handleKey("start")}
          >
            <span className="rcanvas__handle-label mono">{brush?.fromDay ?? "—"}</span>
          </div>
          <span className="rcanvas__handle-arrow" aria-hidden="true">
            →
          </span>
          <div
            role="slider"
            tabIndex={0}
            aria-label="brush end"
            aria-valuemin={0}
            aria-valuemax={last}
            aria-valuenow={brush?.toIndex ?? last}
            aria-valuetext={brush?.toDay ?? ""}
            className="rcanvas__handle"
            data-kb-brush="end"
            onKeyDown={handleKey("end")}
          >
            <span className="rcanvas__handle-label mono">{brush?.toDay ?? "—"}</span>
          </div>
          <span className="rcanvas__summary mono">{brush ? brushSummary(brush) : ""}</span>
        </div>

        <div className="rcanvas__dates">
          <label className="rcanvas__date">
            <span>from</span>
            <input
              type="date"
              value={brush?.fromDay ?? ""}
              min={days[0] ?? ""}
              max={days[last] ?? ""}
              onChange={(e) => setDay("start", e.target.value)}
            />
          </label>
          <label className="rcanvas__date">
            <span>to</span>
            <input
              type="date"
              value={brush?.toDay ?? ""}
              min={days[0] ?? ""}
              max={days[last] ?? ""}
              onChange={(e) => setDay("end", e.target.value)}
            />
          </label>
          <button
            type="button"
            className="rcanvas__reset"
            onClick={() => {
              setSel(null);
              setPivotNote(null);
            }}
            disabled={fullWindow}
          >
            whole window
          </button>
        </div>

        <div className="rcanvas__pivot">
          <button
            type="button"
            className="rcanvas__pivot-go"
            data-kb-act="canvas-pivot"
            data-kb-href={knownHref ?? ""}
            disabled={!brush || pivoting || blockedReason != null}
            title={blockedReason ?? "open the brushed set as a gallery filter"}
            onClick={() => {
              if (brush) void runPivot(brush);
            }}
          >
            {pivotLabel}
          </button>
          {(blockedReason || pivotNote) && (
            <p className="rcanvas__pivot-note" role="status">
              {blockedReason ?? pivotNote}
            </p>
          )}
        </div>
      </div>

      <div className="rcanvas__scenes">
        <div className="rcanvas__scenes-head">
          <span className="rcanvas__scenes-title mono">scenes</span>
          <button
            type="button"
            className="rcanvas__scene-save"
            data-kb-act="scene-save"
            onClick={onSaveScene}
          >
            + save this brush as a scene
          </button>
        </div>
        {scenes.length === 0 ? (
          <p className="rcanvas__scenes-empty">
            no scenes saved for this kb yet — a scene remembers a brush +
            track selection so you can come straight back to it.
          </p>
        ) : (
          <ul className="rcanvas__scene-list">
            {scenes.map((s) => (
              <li key={s.name} className="rcanvas__scene-chip">
                <button
                  type="button"
                  className="rcanvas__scene-restore"
                  data-kb-scene={s.name}
                  onClick={() => navigate(`${s.path}${s.search}`)}
                >
                  {s.name}
                </button>
                <button
                  type="button"
                  className="rcanvas__scene-del"
                  aria-label={`delete scene ${s.name}`}
                  onClick={() => void onDeleteScene(s.name)}
                >
                  <Icon.X />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>

      <p className="rcanvas__note">
        days are labeled in UTC, not your local timezone · each track's shading
        is relative to its own busiest day, so the tracks are comparable in
        shape, not in magnitude
      </p>
    </section>
  );
}

function Lane({
  lane,
  brush,
  selected,
  onToggle,
  onPointerDown,
  onPointerMove,
  onPointerUp,
}: {
  lane: CanvasLane;
  brush: Brush | null;
  selected: boolean;
  onToggle: () => void;
  onPointerDown: (e: React.PointerEvent<HTMLDivElement>) => void;
  onPointerMove: (e: React.PointerEvent<HTMLDivElement>) => void;
  onPointerUp: (e: React.PointerEvent<HTMLDivElement>) => void;
}) {
  const inBrush = brush ? laneEventsInBrush(lane, brush) : lane.total;
  return (
    <div className={`rcanvas__lane${selected ? " is-selected" : ""}`} data-track={lane.track}>
      <div className="rcanvas__lane-head">
        <label className="rcanvas__lane-toggle">
          <input type="checkbox" checked={selected} onChange={onToggle} />
          <span className="rcanvas__lane-title mono">{lane.title}</span>
        </label>
        <span className="rcanvas__lane-count mono" data-lane-count={lane.track}>
          {/* `max/day` is the DENOMINATOR of this lane's shading ramp, printed
              so the per-track scaling is legible — not a "best day" award. */}
          {inBrush.toLocaleString()} in brush · {lane.total.toLocaleString()} in window
          {lane.max > 0 ? ` · max ${lane.max.toLocaleString()}/day` : ""}
        </span>
      </div>
      <div
        className="rcanvas__track"
        style={{ gridTemplateColumns: `repeat(${lane.cells.length}, 1fr)` }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
      >
        {lane.cells.map((cell, i) => (
          <span
            key={cell.day}
            className={
              `rcanvas__cell rcanvas__cell--level-${cell.level}` +
              (brush && (i < brush.fromIndex || i > brush.toIndex) ? " is-outside" : "")
            }
            title={`${cell.day} (UTC) — ${cell.count} ${lane.title}`}
          />
        ))}
      </div>
      <p className="rcanvas__lane-label">
        {lane.label}
        {lane.present && lane.total === 0 ? " · nothing in this window" : ""}
      </p>
    </div>
  );
}
