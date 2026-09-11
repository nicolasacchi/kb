// V76-R2b — resizable map+stream split on the review diff.
//
// `react-resizable-panels` is the SAME resize mechanism the Desk uses
// (`web-code/CLAUDE.md` § Desk: "the resize MECHANISM only, never the
// source of truth"). Rules copied, not re-derived:
//   1. No `autoSaveId` — sizes live in `lib/reviewMapLayout.ts`.
//   2. `disableCursor` — the library's `* { cursor }` rule is the Lumino
//      trap; we paint the cursor on one overlay while a drag is live.
//   3. `defaultSize` is the DEFAULT percent (22%), so a separator
//      double-click resets to that value by construction.
//
// One resizer in the codebase: this file imports Group/Panel/Separator
// from the same package Desk.tsx does, and reuses `.kbc-desk__sep`.

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useState,
  type MutableRefObject,
  type ReactNode,
} from "react";
import { Group, Panel, Separator, usePanelRef, type Layout } from "react-resizable-panels";
import {
  loadMapWidth,
  REVIEW_MAP_WIDTH_DEFAULT,
  saveMapWidth,
} from "../../lib/reviewMapLayout";

export interface ReviewMapSplitHandle {
  resetWidth: () => void;
}

export default function ReviewMapSplit({
  map,
  stream,
  handleRef,
}: {
  map: ReactNode;
  stream: ReactNode;
  handleRef?: MutableRefObject<ReviewMapSplitHandle | null>;
}) {
  const mapPanel = usePanelRef();
  const [width, setWidth] = useState(() =>
    typeof localStorage === "undefined" ? REVIEW_MAP_WIDTH_DEFAULT : loadMapWidth(localStorage),
  );
  const [dragging, setDragging] = useState(false);

  const onLayout = useCallback((layout: Layout, meta: { isUserInteraction: boolean }) => {
    if (!meta.isUserInteraction) return;
    const n = layout.map;
    if (typeof n !== "number") return;
    setWidth(n);
    if (typeof localStorage !== "undefined") saveMapWidth(localStorage, n);
  }, []);

  const resetWidth = useCallback(() => {
    mapPanel.current?.resize(`${REVIEW_MAP_WIDTH_DEFAULT}%`);
    setWidth(REVIEW_MAP_WIDTH_DEFAULT);
    if (typeof localStorage !== "undefined") saveMapWidth(localStorage, REVIEW_MAP_WIDTH_DEFAULT);
  }, [mapPanel]);

  useLayoutEffect(() => {
    mapPanel.current?.resize(`${width}%`);
  }, [mapPanel, width]);

  useEffect(() => {
    if (!handleRef) return;
    const api: ReviewMapSplitHandle = { resetWidth };
    handleRef.current = api;
    return () => {
      handleRef.current = null;
    };
  }, [handleRef, resetWidth]);

  useEffect(() => {
    if (!dragging) return;
    const stop = () => setDragging(false);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
    return () => {
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
  }, [dragging]);

  return (
    <div className="kbc-rdiff__body kbc-rdiff__body--mapped">
      <Group
        id="rdiff-map"
        className="kbc-rdiff__split"
        orientation="horizontal"
        disableCursor
        defaultLayout={{ map: width, stream: 100 - width }}
        onLayoutChanged={onLayout}
        resizeTargetMinimumSize={{ coarse: 24, fine: 8 }}
      >
        <Panel
          id="map"
          className="kbc-rdiff__map-panel"
          panelRef={mapPanel}
          collapsible
          collapsedSize={0}
          minSize="160px"
          defaultSize={`${REVIEW_MAP_WIDTH_DEFAULT}%`}
          style={{ overflow: "hidden" }}
        >
          {map}
        </Panel>
        <Separator
          id="sep-map"
          className="kbc-desk__sep kbc-desk__sep--v"
          data-kbc-rdiff-map-sep
          onPointerDown={() => setDragging(true)}
        />
        <Panel id="stream" className="kbc-rdiff__stream-panel" minSize="40%" style={{ overflow: "hidden" }}>
          {stream}
        </Panel>
      </Group>
      {dragging && (
        <div
          className="kbc-desk__cursor-overlay kbc-desk__cursor-overlay--horizontal"
          data-desk-cursor-overlay="horizontal"
          aria-hidden
        />
      )}
    </div>
  );
}
