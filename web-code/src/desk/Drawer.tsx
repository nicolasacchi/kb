// V70-A4 — the bottom drawer: named result sets that outlive the popup.
//
// docs/research/kb-code-v7-continuum-2026-09.html §P1: "The bottom
// drawer is a tab bar of named, stable-id result sets … Sets render as
// stitched excerpts ordered exact ▷ likely ▷ candidate ▷ observed".
// The research report's framing (panel-layout-system.md §3.6) is the one
// that matters for this component: "*reading inside the drawer is a
// first-class activity* — not a link list."
//
// One tenant is wired in V70-A4: the reference list `gr` shows in the
// peek popup gains a "Keep in drawer" action, so a result set survives
// the Esc that closes the popup. Everything else (search, diagnostics,
// recipes) is declared in `placement.ts` and unrouted — the plumbing is
// proven by one real user, not by six stubs.
//
// The drawer collapses to its own TAB BAR (the panel's `collapsedSize`
// is exactly the bar's height), which is why the bar renders first and
// the body scrolls under it: a collapsed drawer still shows every tab it
// holds, so a kept set is never invisible.

import { useEffect, useRef } from "react";
import { rungForMouse, type RampRung } from "../nav/ramp";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import TrustBadge from "../components/TrustBadge";
import {
  activeDrawerSet,
  drawerTabOrder,
  type DrawerRow,
  type DrawerSetsAction,
  type DrawerSetsState,
} from "./drawerSets";

/// Must match `--desk-drawer-strip` in `styles/desk.css` — the panel's
/// collapsed size IS the tab bar's height, so the two cannot drift
/// without the collapsed drawer either clipping its tabs or showing a
/// sliver of body.
export const DRAWER_STRIP_PX = 32;

export interface DrawerProps {
  state: DrawerSetsState;
  dispatch: (a: DrawerSetsAction) => void;
  collapsed: boolean;
  onExpand: () => void;
  onCollapse: () => void;
  /// Open a row in the focused pane. `Enter`, or a click.
  onOpenRow: (row: DrawerRow) => void;
  /// V70-A6 — the Ramp (§P7): the drawer's kept result sets are a result
  /// surface like any other, so middle-click / Ctrl-Cmd-click open elsewhere
  /// through the ONE shared handler (`nav/ramp.ts`).
  onRamp?: (rung: RampRung, row: DrawerRow) => void;
  /// Overlay mode: the drawer would have squeezed the code region below
  /// the viewport floor, so it floats over main instead of docking. The
  /// caption says so — a drawer that quietly behaves differently is
  /// worse than one that explains itself.
  overlay?: boolean;
}

/// exact ▷ likely ▷ candidate ▷ observed ▷ (unclassed). Sourcegraph's
/// "precise before fuzzy", mapped onto kb-code's own trust vocabulary;
/// a hard rule that exact never mixes with the rest.
const TRUST_ORDER = ["exact", "likely", "candidate", "observed"];

function trustRank(cls: string | undefined): number {
  const i = TRUST_ORDER.indexOf(cls ?? "");
  return i === -1 ? TRUST_ORDER.length : i;
}

export function groupRowsByTrust(rows: readonly DrawerRow[]): { cls: string | null; rows: DrawerRow[] }[] {
  const buckets = new Map<string, DrawerRow[]>();
  for (const r of rows) {
    const key = r.trustClass ?? "";
    const list = buckets.get(key);
    if (list) list.push(r);
    else buckets.set(key, [r]);
  }
  return [...buckets.entries()]
    .sort((a, b) => trustRank(a[0] || undefined) - trustRank(b[0] || undefined))
    .map(([cls, rs]) => ({ cls: cls === "" ? null : cls, rows: rs }));
}

export default function Drawer({
  state,
  dispatch,
  collapsed,
  onExpand,
  onCollapse,
  onOpenRow,
  onRamp,
  overlay = false,
}: DrawerProps) {
  const tabs = drawerTabOrder(state);
  const active = activeDrawerSet(state);
  const bodyRef = useRef<HTMLDivElement | null>(null);

  // Keep the cursor row in view while j/k walks it — the drawer is a
  // reading surface, so the row you are on has to be the row you see.
  useEffect(() => {
    if (collapsed || !active) return;
    const el = bodyRef.current?.querySelector<HTMLElement>("[data-desk-drawer-row].is-cursor");
    el?.scrollIntoView({ block: "nearest" });
  }, [collapsed, active, active?.cursor, active?.id]);

  function onKeyDown(e: React.KeyboardEvent) {
    if (!active) return;
    // The drawer owns its own keys while focused — never let j/k leak
    // out to the reader's window-level tree handler.
    switch (e.key) {
      case "j":
      case "ArrowDown":
        e.preventDefault();
        e.stopPropagation();
        dispatch({ type: "moveCursor", delta: 1 });
        return;
      case "k":
      case "ArrowUp":
        e.preventDefault();
        e.stopPropagation();
        dispatch({ type: "moveCursor", delta: -1 });
        return;
      case "Enter": {
        e.preventDefault();
        e.stopPropagation();
        const row = active.rows[active.cursor];
        if (row) onOpenRow(row);
        return;
      }
      case "Escape":
        e.preventDefault();
        e.stopPropagation();
        onCollapse();
        return;
    }
  }

  return (
    <aside
      className={"kbc-desk__drawer" + (collapsed ? " is-collapsed" : "") + (overlay ? " is-overlay" : "")}
      data-region="drawer"
      data-desk-drawer-collapsed={collapsed ? "1" : "0"}
      data-desk-drawer-overlay={overlay ? "1" : "0"}
      aria-label="result sets"
    >
      <div className="kbc-desk__drawer-bar" role="tablist" aria-label="result sets">
        {tabs.length === 0 && (
          <span className="kbc-desk__drawer-empty-tab">no result sets — keep one from a references popup</span>
        )}
        {tabs.map((s) => {
          const sel = !s.evicted && s.id === state.activeId;
          return (
            <span
              key={s.id}
              className={
                "kbc-desk__drawer-tab" +
                (sel ? " is-on" : "") +
                (s.evicted ? " is-evicted" : "") +
                (s.pinned ? " is-pinned" : "")
              }
              data-desk-drawer-tab={s.id}
              data-desk-drawer-tab-evicted={s.evicted ? "1" : "0"}
            >
              <button
                type="button"
                role="tab"
                aria-selected={sel}
                className="kbc-desk__drawer-tab-main"
                title={s.evicted ? `${s.title} — closed, click to reopen` : s.title}
                onClick={() => {
                  dispatch({ type: "activate", id: s.id });
                  if (collapsed) onExpand();
                }}
              >
                {s.pinned && <Icon.Pin />}
                <span className="kbc-desk__drawer-tab-name">{s.title}</span>
                <span className="kbc-desk__drawer-tab-count">{s.rows.length}</span>
                {s.evicted && <span className="kbc-desk__drawer-tab-note">closed — reopen</span>}
              </button>
              {!s.evicted && (
                <>
                  <button
                    type="button"
                    className="kbc-desk__drawer-tab-act"
                    title={s.pinned ? "unpin — allow eviction" : "pin — never evict"}
                    aria-label={s.pinned ? "unpin set" : "pin set"}
                    data-cmd="drawer.pin"
                    onClick={() => dispatch({ type: "togglePin", id: s.id })}
                  >
                    <Icon.Pin />
                  </button>
                  <button
                    type="button"
                    className="kbc-desk__drawer-tab-act"
                    title="close — the rows are kept, reopen from the greyed tab"
                    aria-label="close set"
                    data-cmd="drawer.close"
                    onClick={() => dispatch({ type: "close", id: s.id })}
                  >
                    <Icon.X />
                  </button>
                </>
              )}
            </span>
          );
        })}
        <span className="kbc-desk__drawer-bar-spacer" />
        <button
          type="button"
          className="kbc-desk__drawer-toggle"
          title={collapsed ? "open the drawer" : "collapse the drawer to its tab strip"}
          aria-label={collapsed ? "open the drawer" : "collapse the drawer"}
          aria-expanded={!collapsed}
          data-cmd="drawer.toggle"
          data-desk-drawer-toggle
          onClick={() => (collapsed ? onExpand() : onCollapse())}
        >
          {collapsed ? <Icon.Expand /> : <Icon.Collapse />}
        </button>
      </div>
      {!collapsed && (
        <div
          className="kbc-desk__drawer-body"
          ref={bodyRef}
          tabIndex={0}
          role="listbox"
          aria-label={active ? `${active.title} results` : "results"}
          onKeyDown={onKeyDown}
          data-desk-drawer-body
        >
          {overlay && (
            <p className="kbc-desk__drawer-caption">
              floating over the code — the window is too short to dock the drawer without squeezing the
              code region below its readable floor
            </p>
          )}
          {!active ? (
            <EmptyState
              variant="inline"
              title="No result set open"
              hint="Press gr on a symbol, then Keep in drawer — the set stays here beside the code."
            />
          ) : active.rows.length === 0 ? (
            <EmptyState variant="inline" title={`${active.title} — no rows`} />
          ) : (
            groupRowsByTrust(active.rows).map((group) => (
              <section key={group.cls ?? "unclassed"} className="kbc-desk__drawer-group">
                {group.cls && (
                  <h3 className="kbc-desk__drawer-group-head">
                    <TrustBadge cls={group.cls} />
                    <span>{group.rows.length}</span>
                  </h3>
                )}
                {group.rows.map((row) => {
                  const idx = active.rows.indexOf(row);
                  return (
                    <div
                      key={`${row.repo}/${row.path}:${row.line}:${idx}`}
                      className={"kbc-desk__drawer-row" + (idx === active.cursor ? " is-cursor" : "")}
                      role="option"
                      aria-selected={idx === active.cursor}
                      data-desk-drawer-row={idx}
                      onMouseDown={(e) => {
                        const rung = rungForMouse(e);
                        if (!rung || rung === "here" || !onRamp) return;
                        e.preventDefault();
                        dispatch({ type: "setCursor", index: idx });
                        onRamp(rung, row);
                      }}
                      onClick={() => {
                        dispatch({ type: "setCursor", index: idx });
                        onOpenRow(row);
                      }}
                    >
                      <span className="kbc-desk__drawer-row-loc">
                        <span className="kbc-desk__drawer-row-path">{row.path}</span>
                        <span className="kbc-desk__drawer-row-line">:{row.line}</span>
                      </span>
                      {row.text && <code className="kbc-desk__drawer-row-text">{row.text}</code>}
                    </div>
                  );
                })}
              </section>
            ))
          )}
        </div>
      )}
    </aside>
  );
}
