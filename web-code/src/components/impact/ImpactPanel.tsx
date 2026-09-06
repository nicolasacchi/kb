import { useEffect, useRef } from "react";
import { rungForMouse, type RampRung } from "../../nav/ramp";
import EmptyState from "../EmptyState";
import { Icon } from "../icons";
import { ClassBadge } from "../hierarchy/HierarchyPanel";
import type { PeekAnchor } from "../peek/PeekPanel";
import {
  bucketCount,
  currentImpactRow,
  IMPACT_BUCKET_LABEL,
  isImpactNavigable,
  provenanceFor,
  type ImpactBucketId,
  type ImpactFlatRow,
  type ImpactState,
} from "../../lib/impactState";
/** SPA session-diff route (`app.tsx` `/session/:sid/diff`). */
function sessionDiffHref(sessionId: string): string {
  return `/session/${encodeURIComponent(sessionId)}/diff`;
}

const PANEL_WIDTH = 460;
const VIEWPORT_MARGIN = 12;
const MIN_SPACE_BELOW = 220;

export interface ImpactPanelProps {
  state: ImpactState;
  currentRepo: string;
  anchor: PeekAnchor | null;
  onMove(delta: number): void;
  onActivate(row: ImpactFlatRow): void;
  /// V70-A6 — the Ramp (§P7): middle-click / Ctrl-Cmd-click open elsewhere,
  /// through the ONE shared handler. Absent ⇒ pre-A6 behaviour.
  onRamp?(rung: RampRung, row: ImpactFlatRow): void;
  onToggleBucket(bucket: ImpactBucketId): void;
  onClose(): void;
}

function computeStyle(anchor: PeekAnchor | null): React.CSSProperties {
  if (!anchor || typeof window === "undefined") return {};
  const left = Math.min(
    Math.max(anchor.left, VIEWPORT_MARGIN),
    window.innerWidth - PANEL_WIDTH - VIEWPORT_MARGIN,
  );
  const spaceBelow = window.innerHeight - anchor.bottom;
  const openUpward = spaceBelow < MIN_SPACE_BELOW && anchor.top > spaceBelow;
  return openUpward
    ? { left: Math.max(left, VIEWPORT_MARGIN), bottom: window.innerHeight - anchor.top + 6 }
    : { left: Math.max(left, VIEWPORT_MARGIN), top: anchor.bottom + 6 };
}

function ProvenanceLine({
  prov,
}: {
  prov: { rows_with_session: number; distinct_sessions: number; sample: { session_id: string }[] };
}) {
  return (
    <div className="kbc-impact__prov" data-kbc-impact-prov>
      {prov.rows_with_session} row{prov.rows_with_session === 1 ? "" : "s"} from{" "}
      {prov.distinct_sessions} session{prov.distinct_sessions === 1 ? "" : "s"}
      {prov.sample.length > 0 && (
        <span className="kbc-impact__prov-samples">
          {" · "}
          {prov.sample.map((s, i) => (
            <span key={s.session_id}>
              {i > 0 && ", "}
              <a
                href={sessionDiffHref(s.session_id)}
                className="kbc-impact__prov-link"
                data-kbc-impact-session={s.session_id}
                onClick={(e) => e.stopPropagation()}
              >
                {s.session_id.slice(0, 8)}
              </a>
            </span>
          ))}
        </span>
      )}
    </div>
  );
}

function ImpactRowView({
  row,
  active,
  state,
  onActivate,
  onRamp,
  onToggleBucket,
}: {
  row: ImpactFlatRow;
  active: boolean;
  state: ImpactState;
  onActivate: (r: ImpactFlatRow) => void;
  onRamp?: (rung: RampRung, r: ImpactFlatRow) => void;
  onToggleBucket: (b: ImpactBucketId) => void;
}) {
  if (row.section) {
    const bucket = row.section;
    const count = state.data ? bucketCount(state.data, bucket) : 0;
    const collapsed = state.collapsed.has(bucket);
    const prov = state.data ? provenanceFor(state.data, bucket) : null;
    const isTests = bucket === "tests";
    return (
      <div
        className={
          "kbc-peek__row kbc-impact__section" +
          (active ? " kbc-peek__row--active" : "") +
          (isTests ? " kbc-impact__section--tests" : "")
        }
        role="option"
        aria-selected={active}
        data-kbc-impact-section={bucket}
        onClick={() => onToggleBucket(bucket)}
      >
        <span className="kbc-hier__twist">
          <Icon.Chevron className={collapsed ? "kbc-twisty" : "kbc-twisty is-open"} />
        </span>
        <span className="kbc-impact__section-label">
          {IMPACT_BUCKET_LABEL[bucket]}
          <span className="kbc-impact__section-count"> ({count})</span>
        </span>
        {prov && <ProvenanceLine prov={prov} />}
      </div>
    );
  }

  if (row.depthGroup != null) {
    return (
      <div
        className={"kbc-peek__row kbc-impact__depth" + (active ? " kbc-peek__row--active" : "")}
        role="option"
        aria-selected={active}
        data-kbc-impact-depth={row.depthGroup}
      >
        <span className="kbc-impact__depth-label">depth {row.depthGroup}</span>
      </div>
    );
  }

  if (row.truncatedNote) {
    return (
      <div
        className={"kbc-peek__row kbc-impact__trunc" + (active ? " kbc-peek__row--active" : "")}
        role="option"
        aria-selected={active}
        data-kbc-impact-trunc
      >
        <span className="kbc-peek__row-main">{row.truncatedNote}</span>
      </div>
    );
  }

  return (
    <div
      className={"kbc-peek__row kbc-impact__row" + (active ? " kbc-peek__row--active" : "")}
      role="option"
      aria-selected={active}
      data-kbc-impact-row
      data-kbc-impact-class={row.class}
      onMouseDown={(e) => {
        const rung = rungForMouse(e);
        if (!rung || rung === "here" || !onRamp) return;
        e.preventDefault();
        onRamp(rung, row);
      }}
      onClick={() => onActivate(row)}
    >
      <span className="kbc-peek__row-main">
        <span className="kbc-peek__row-loc">
          <span className="kbc-peek__row-path">{row.path}</span>
          {row.line > 0 && <span className="kbc-peek__row-line">:{row.line}</span>}
        </span>
        {row.name && <span className="kbc-peek__row-container"> — {row.name}</span>}
      </span>
      {row.depth != null && (
        <span className="kbc-impact__depth-chip" data-kbc-impact-depth-chip={row.depth}>
          d{row.depth}
        </span>
      )}
      <ClassBadge className={row.class} />
    </div>
  );
}

export default function ImpactPanel({
  state,
  currentRepo: _currentRepo,
  anchor,
  onMove,
  onActivate,
  onToggleBucket,
  onClose,
  onRamp,
}: ImpactPanelProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    containerRef.current?.focus();
  }, []);

  useEffect(() => {
    const el = containerRef.current?.querySelector(".kbc-peek__row--active");
    el?.scrollIntoView({ block: "nearest" });
  }, [state.cursor]);

  if (!state.open) return null;

  function onKeyDown(e: React.KeyboardEvent) {
    e.stopPropagation();
    switch (e.key) {
      case "ArrowDown":
      case "j":
        e.preventDefault();
        onMove(1);
        return;
      case "ArrowUp":
      case "k":
        e.preventDefault();
        onMove(-1);
        return;
      case "Enter": {
        e.preventDefault();
        const row = currentImpactRow(state);
        if (!row) return;
        if (row.section) {
          onToggleBucket(row.section);
          return;
        }
        if (isImpactNavigable(row)) onActivate(row);
        return;
      }
      case "Escape":
        e.preventDefault();
        onClose();
        return;
    }
  }

  const style = anchor ? computeStyle(anchor) : undefined;
  const className = "kbc-peek kbc-impact" + (anchor ? "" : " kbc-peek--dock");

  return (
    <div
      ref={containerRef}
      className={className}
      style={style}
      role="dialog"
      aria-modal="true"
      aria-label={`Impact: ${state.title}`}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      data-kbc-impact
    >
      <header className="kbc-peek__head">
        <span className="kbc-peek__mode">Impact</span>
        <span className="kbc-peek__title">{state.title}</span>
        <button type="button" className="kbc-peek__close" onClick={onClose} aria-label="close">
          <Icon.X />
        </button>
      </header>
      {state.note && (
        <div className="kbc-impact__note" data-kbc-impact-note title={state.note}>
          {state.note}
        </div>
      )}
      <div className="kbc-peek__body" role="listbox" aria-label="Impact buckets">
        {state.loading && <div className="kbc-peek__hint kbc-peek__loading">Loading…</div>}
        {!state.loading && state.error && (
          <div className="kbc-peek__hint kbc-peek__error">{state.error}</div>
        )}
        {!state.loading && !state.error && state.flat.length === 0 && (
          <EmptyState variant="inline" title="No impact rows" />
        )}
        {!state.loading &&
          !state.error &&
          state.flat.map((row, i) => (
            <ImpactRowView
              key={row.id}
              row={row}
              active={i === state.cursor}
              state={state}
              onActivate={onActivate}
              onRamp={onRamp}
              onToggleBucket={onToggleBucket}
            />
          ))}
      </div>
    </div>
  );
}
