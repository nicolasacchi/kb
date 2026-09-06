// PRR-U2 (kb v0.39 "The PR Room," §2 S2) extends this tab strip with
// "report" — present ONLY when the review has an authored report (the
// same "absent → tab doesn't render" pattern `mapAvailable`/`orderAvailable`
// already establish for Map/Reading-order).
//
// ── PRR-U56 (§2 S4 — Timeline tab, design doc §11's SHOULD tier) ──
// Timeline gets the SAME `*Available` gate (`useReviewTimeline`'s 404→null
// degrade, same convention as Map/Order) rather than being unconditional —
// an older server without `GET .../timeline` hides the tab with no stub
// chrome, consistent with every other optional cockpit surface here.
export type CockpitView = "report" | "files" | "map" | "order" | "timeline";

export interface CockpitTabsProps {
  compareMode: boolean;
  reportAvailable: boolean;
  mapAvailable: boolean;
  orderAvailable: boolean;
  timelineAvailable: boolean;
  cockpitView: CockpitView;
  onSelect: (view: CockpitView) => void;
}

export default function CockpitTabs({
  compareMode,
  reportAvailable,
  mapAvailable,
  orderAvailable,
  timelineAvailable,
  cockpitView,
  onSelect,
}: CockpitTabsProps) {
  if (compareMode || !(reportAvailable || mapAvailable || orderAvailable || timelineAvailable)) return null;
  return (
    <div className="kbc-review__view-tabs" role="tablist" data-kbc-review-view-tabs>
      {reportAvailable && (
        <button
          type="button"
          role="tab"
          aria-selected={cockpitView === "report"}
          className={
            "kbc-review__view-tab" + (cockpitView === "report" ? " kbc-review__view-tab--active" : "")
          }
          onClick={() => onSelect("report")}
          data-kbc-review-view="report"
        >
          Report
        </button>
      )}
      <button
        type="button"
        role="tab"
        aria-selected={cockpitView === "files"}
        className={
          "kbc-review__view-tab" + (cockpitView === "files" ? " kbc-review__view-tab--active" : "")
        }
        onClick={() => onSelect("files")}
        data-kbc-review-view="files"
      >
        Files
      </button>
      {mapAvailable && (
        <button
          type="button"
          role="tab"
          aria-selected={cockpitView === "map"}
          className={
            "kbc-review__view-tab" + (cockpitView === "map" ? " kbc-review__view-tab--active" : "")
          }
          onClick={() => onSelect("map")}
          data-kbc-review-view="map"
        >
          Map
        </button>
      )}
      {orderAvailable && (
        <button
          type="button"
          role="tab"
          aria-selected={cockpitView === "order"}
          className={
            "kbc-review__view-tab" +
            (cockpitView === "order" ? " kbc-review__view-tab--active" : "")
          }
          onClick={() => onSelect("order")}
          data-kbc-review-view="order"
        >
          Reading order
        </button>
      )}
      {timelineAvailable && (
        <button
          type="button"
          role="tab"
          aria-selected={cockpitView === "timeline"}
          className={
            "kbc-review__view-tab" +
            (cockpitView === "timeline" ? " kbc-review__view-tab--active" : "")
          }
          onClick={() => onSelect("timeline")}
          data-kbc-review-view="timeline"
        >
          Timeline
        </button>
      )}
    </div>
  );
}
