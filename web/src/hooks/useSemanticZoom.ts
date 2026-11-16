import { useEffect, useRef } from "react";
import { isEditableTarget } from "../lib/keymap";
import { feedWheel, nextView, ZOOM_IDLE, type ZoomDir, type ZoomGesture, type ZoomView } from "../lib/semanticZoom";
import { loadPinchZoom } from "../api/prefs";
import { isOverlayOpen } from "./useRovingCursor";

type Options = {
  /// The Library content element. The ViewBar inside it is excluded.
  node: HTMLElement | null;
  /// Current `?view=` value.
  view: string;
  /// Phones: no pinch handling (browser zoom must keep working), keys still work.
  gesture: boolean;
  /// Called with the rung to move to; the caller navigates exactly as "View as" does.
  onStep: (v: ZoomView) => void;
};

/// The Library's current `?view=` (absent means the default, the shelf).
function viewOfUrl(search: string): string {
  return new URLSearchParams(search).get("view") || "grid";
}

/// Semantic zoom for the Library: ctrl-wheel (trackpad pinch) over the content
/// steps atlas <-> index <-> shelf; bare `-` / `=` do the same from the keyboard.
/// Ctrl+/Ctrl- and the browser menu are never touched: the wheel handler only
/// calls preventDefault when a step is available or the tail of a step's own
/// gesture is being swallowed.
export function useSemanticZoom({ node, view, gesture, onStep }: Options) {
  // The rung the NEXT step is computed from. The caller navigates exactly to
  // the rung it is handed, but the navigation lands before the re-render that
  // delivers the new `view` prop, so reading the prop alone makes a second key
  // press or wheel notch in that window step from a stale rung (skipping or
  // repeating one). So `stepRef` advances this ref optimistically, and the prop
  // resyncs it ONLY when the prop value itself changes: a re-render that still
  // carries an older prop (the one before the step) must not undo the
  // optimistic rung either.
  const viewRef = useRef(view);
  const seenViewRef = useRef(view);
  if (seenViewRef.current !== view) {
    seenViewRef.current = view;
    viewRef.current = view;
  }
  const onStepRef = useRef(onStep);
  onStepRef.current = onStep;
  // The location right after the last step. While it is unchanged the step is
  // still in flight and the optimistic rung stands; once it differs (the step
  // landed, or another navigation superseded it) the URL is the truth, read at
  // event time, so an external navigation that leaves the `view` prop equal to
  // what it was cannot strand the optimistic rung.
  const stepHrefRef = useRef<string | null>(null);
  const stepRef = useRef((to: ZoomView) => {
    viewRef.current = to;
    onStepRef.current(to);
    stepHrefRef.current = window.location.href;
  });
  const currentView = (): string => {
    if (stepHrefRef.current !== null && window.location.href !== stepHrefRef.current) {
      stepHrefRef.current = null;
      viewRef.current = viewOfUrl(window.location.search);
    }
    return viewRef.current;
  };

  useEffect(() => {
    if (!node || !gesture) return;
    let state: ZoomGesture = ZOOM_IDLE;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      if (!loadPinchZoom()) return;
      const t = e.target as Element | null;
      if (t && t.closest && t.closest(".kb-gallery-viewbar")) return;
      const r = feedWheel(state, e.deltaY, performance.now(), (d: ZoomDir) => nextView(currentView(), d) !== null);
      state = r.state;
      if (r.action === "pass") return;
      e.preventDefault();
      // The Atlas canvas zooms its own map on a wheel: a rung change must not also do that.
      e.stopPropagation();
      if (r.action === "step" && r.dir) {
        const to = nextView(currentView(), r.dir);
        if (to) stepRef.current(to);
      }
    };
    // Capture phase + non-passive so we run before the atlas canvas handler.
    node.addEventListener("wheel", onWheel, { passive: false, capture: true });
    return () => node.removeEventListener("wheel", onWheel, { capture: true });
  }, [node, gesture]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.repeat) return;
      if (e.key !== "-" && e.key !== "=") return;
      if (isEditableTarget(e.target) || isOverlayOpen()) return;
      const to = nextView(currentView(), e.key === "=" ? "in" : "out");
      if (!to) return;
      e.preventDefault();
      stepRef.current(to);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}
