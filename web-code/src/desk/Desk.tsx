// V70-A4 — the Desk: kb-code v7's five-region shell.
//
//   ┌──────────────────────────────────────────────────────────────┐
//   │ TopBar (app.tsx, above every route — not part of the Desk)   │
//   ├──┬────────────┬───────────────────────────────┬───────────┬──┤
//   │S │            │  header (breadcrumbs, toggles │           │S │
//   │t │   DOCK     │   ref picker, working set)    │   RAIL    │t │
//   │r │  (tree)    ├───────────────────────────────┤ (the six  │r │
//   │i │            │  MAIN — the center mode       │  → four   │i │
//   │p │            │  (reader today; diff, board,  │  tabs)    │p │
//   │e │            │   dossier, … later)           │           │e │
//   │  │            ├───────────────────────────────┤           │  │
//   │  │            │  DRAWER — named result sets   │           │  │
//   └──┴────────────┴───────────────────────────────┴───────────┴──┘
//
// docs/research/kb-code-v7-continuum-2026-09.html §P1. The operator's
// bar behind it: "travelling continuously between code without hard
// stops that change the interface". The structural answer is that a
// center MODE changes what main shows while every other region — its
// identity, its order, its keyboard address — stays exactly where it
// was. `e2e/desk-landmarks.spec.ts` is that promise as a test.
//
// ## `react-resizable-panels` is the mechanism, never the truth
//
// Three rules, all from the research report (panel-layout-system.md
// §1.11):
//
//  1. **No `autoSaveId`.** Sizes come out of `deskState.ts` into the
//     Group's `defaultLayout` at mount and into `panelRef.resize()`
//     afterwards; they come back only through `onLayoutChanged`'s
//     `isUserInteraction` branch. One serialisable object, one home.
//  2. **`disableCursor`.** The library's own cursor feedback injects a
//     document-level `*, *:hover { cursor: … !important }` rule. That is
//     precisely the Lumino/Chromium trap the report documents
//     (jupyterlab/lumino#450 — ~1.7s style recalc per drag frame,
//     because a `*` selector invalidates every element in the
//     document). We disable it and paint the cursor on ONE dedicated
//     overlay div that only exists while a drag is live.
//  3. **`defaultSize` per Panel = the PRESET's size**, not the persisted
//     one. The library resolves a separator double-click against
//     `defaultSize`, so this is what makes "double-click resets that
//     separator to the preset value" true by construction rather than
//     by a hand-rolled dblclick handler.
//
// ## CodeMirror
//
// CM6 must be TOLD its box changed (`view.requestMeasure()`); a
// `ResizeObserver` inside `CodeView` does that, and every wrapper
// between a sized Panel and `.cm-scroller` carries an explicit height in
// `styles/desk.css` — the two halves of the report's CM6 note.

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Group, Panel, Separator, usePanelRef, type Layout } from "react-resizable-panels";
import { Icon } from "../components/icons";
import MobileDrawer from "../components/MobileDrawer";
import Drawer, { DRAWER_STRIP_PX } from "./Drawer";
import type { RampRung } from "../nav/ramp";
import Stripe, { type StripeButton } from "./Stripe";
import { DESK_PRESETS, DESK_PRESET_NAMES, type DeskPresetName } from "./presets";
import { RESIZE_SUBMODE_HINT, resizeSubmodeKey } from "./resizeSubmode";
import { effectiveCollapsed, type DeskApi } from "./useDesk";
import type { CenterMode } from "./centerModes";
import type { DrawerRow } from "./drawerSets";
import type { DeskRegionId, RailTab } from "./deskState";

// --- the viewport floor -------------------------------------------------
//
// §P1's viewport golden: "at 1280×720 with the default desk the code
// region is ≥80 columns × 28 lines at the default font; the drawer opens
// as an overlay when it would violate that."
//
// These are the PIXEL form of that floor at the default 13px reader font
// (char advance ≈ 7.8px, line height ≈ 18.2px), plus CM6's own gutter
// column. They exist so the shell can make the overlay decision without
// reaching into CodeMirror; `e2e/desk-viewport.spec.ts` measures the
// real `.cm-content` metrics and would fail here first if the two ever
// disagreed.
export const MAIN_MIN_COLS = 80;
export const MAIN_MIN_ROWS = 28;
/// 28 lines at the default 13px reader font (≈18.2px each ⇒ ≈510px),
/// plus the reader center mode's own chrome inside the main region (the
/// header row and the working-set strip, ≈70px). Measured, not guessed —
/// `e2e/desk-viewport.spec.ts` reports the real column/row count on every
/// run, and its short-viewport case pins the overlay branch this
/// constant selects.
const MAIN_MIN_HEIGHT_PX = 580;

export interface DeskRailSlotCtx {
  /// `true` only on mobile, where the rail is promoted to the single
  /// bottom sheet. Desktop passes `false`, so the docked `<aside>`'s
  /// markup is byte-identical to pre-Desk (kb-code's F5 mobile ruling).
  asSheet: boolean;
  onMobileClose?: () => void;
  tab: RailTab;
  setTab: (t: RailTab) => void;
}

export interface DeskProps {
  repo: string;
  desk: DeskApi;
  centerMode: CenterMode;
  isMobile: boolean;

  /// Left-dock body — the file tree today. On mobile this is hoisted
  /// into the existing `MobileDrawer` (one `FileTree` instance either
  /// way, so an imperative ref never has to pick between two copies).
  dock: ReactNode;
  dockOpen: boolean;
  onDockOpenChange: (open: boolean) => void;

  /// The center mode's own chrome, rendered ABOVE main INSIDE the main
  /// region — so it resizes with the code rather than spanning the whole
  /// window, and so a mode swap replaces the chrome and the body
  /// together.
  header: ReactNode;
  /// The center mode's body.
  children: ReactNode;

  rail: (ctx: DeskRailSlotCtx) => ReactNode;
  /// `null` when the center mode has no rail subject at all (a diff, a
  /// binary file) — the rail region collapses to its stripe rather than
  /// rendering an empty column.
  railAvailable: boolean;
  railBadges?: Partial<Record<RailTab, number>>;

  /// Which pane the URL is actually rendering — see
  /// `DeskPanesState.count`'s doc for why this is not read off the desk.
  paneCount: 1 | 2;
  onOpenDrawerRow: (row: DrawerRow) => void;
  /// V70-A6 — the Ramp (§P7) for the drawer's kept result sets. Threaded
  /// rather than resolved here: the Desk is shell geometry, and knowing where
  /// a row opens is the reader's business.
  onRampDrawerRow?: (rung: RampRung, row: DrawerRow) => void;

  /// The mobile sheet's open state (the rail AND the drawer share it —
  /// "the drawer is a tab inside that same sheet, never a second
  /// sheet").
  sheetOpen: boolean;
  onSheetOpenChange: (open: boolean) => void;

  /// V70-A10 ("Workspaces v0", D26) — opens the "Save workspace" dialog.
  /// `undefined` on any host that hasn't wired workspaces (the button
  /// then doesn't render at all — same "no prop, no affordance" posture
  /// `onJumpBookmark`'s absence takes on `InspectorRail`).
  onSaveWorkspace?: () => void;
}

const RAIL_STRIPE_META: { tab: RailTab; label: string; icon: ReactNode }[] = [
  { tab: "all", label: "All", icon: <Icon.Layers /> },
  { tab: "understand", label: "Understand", icon: <Icon.Entity /> },
  { tab: "history", label: "History", icon: <Icon.History /> },
  { tab: "notes", label: "Notes", icon: <Icon.Note /> },
  { tab: "review", label: "Review", icon: <Icon.ClipboardCheck /> },
];

/// The Group's `defaultLayout`: percentages for the EXPANDED sizes. The
/// collapsed state is applied separately (a `useLayoutEffect` calling
/// the imperative `collapse()` before the first paint), because a
/// collapsed size is expressed in pixels — `collapsedSize={30}` for the
/// drawer's tab strip — and a `Layout` map speaks only percentages.
export function colsLayout(dock: number, rail: number): Layout {
  const center = Math.max(20, 100 - dock - rail);
  return { dock, center, rail };
}

export function centerLayout(drawer: number): Layout {
  return { main: Math.max(20, 100 - drawer), drawer };
}

export default function Desk(props: DeskProps) {
  const {
    desk,
    centerMode,
    isMobile,
    dock,
    dockOpen,
    onDockOpenChange,
    header,
    children,
    rail,
    railAvailable,
    railBadges = {},
    paneCount,
    onOpenDrawerRow,
    onRampDrawerRow,
    sheetOpen,
    onSheetOpenChange,
    onSaveWorkspace,
  } = props;
  const { state, chrome, zoom } = desk;

  const dockPanel = usePanelRef();
  const railPanel = usePanelRef();
  const drawerPanel = usePanelRef();
  const centerRef = useRef<HTMLDivElement | null>(null);
  const [dragging, setDragging] = useState<"horizontal" | "vertical" | null>(null);
  const [hintOpen, setHintOpen] = useState(false);
  const [centerHeight, setCenterHeight] = useState<number | null>(null);
  const [presetMenu, setPresetMenu] = useState(false);

  const dockCollapsed = effectiveCollapsed(state, "dock", chrome, zoom);
  const railCollapsed = !railAvailable || effectiveCollapsed(state, "rail", chrome, zoom);
  const drawerCollapsedWanted = effectiveCollapsed(state, "drawer", chrome, zoom);

  // --- the viewport golden's overlay branch -------------------------------
  //
  // Docking the drawer costs main `drawer%` of the center's height. When
  // what is left would fall under the readable floor, the drawer floats
  // over main instead of shrinking it — the code region never goes below
  // 80×28 because a result set opened.
  const wouldSqueeze =
    centerHeight !== null &&
    centerHeight * (1 - state.regions.drawer.size / 100) < MAIN_MIN_HEIGHT_PX;
  const drawerOverlay = !drawerCollapsedWanted && wouldSqueeze;
  // Two different collapses, deliberately separate:
  //  * the PANEL collapses whenever the drawer is not docked — including
  //    while it floats, because a floating drawer must not also be
  //    holding height in the vertical group;
  //  * the DRAWER ITSELF is collapsed only when the operator asked for
  //    it. A floating drawer is OPEN — it shows its rows and says why it
  //    is floating. Conflating the two made the overlay branch render an
  //    empty strip, which is the opposite of honest.
  const drawerPanelCollapsed = drawerCollapsedWanted || drawerOverlay;
  const drawerCollapsed = drawerCollapsedWanted;

  useEffect(() => {
    const el = centerRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) setCenterHeight(e.contentRect.height);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // --- collapse/expand, applied before paint ------------------------------
  useLayoutEffect(() => {
    if (isMobile) return;
    if (dockCollapsed) dockPanel.current?.collapse();
    else dockPanel.current?.expand();
  }, [isMobile, dockCollapsed, dockPanel]);

  useLayoutEffect(() => {
    if (isMobile) return;
    if (railCollapsed) railPanel.current?.collapse();
    else railPanel.current?.expand();
  }, [isMobile, railCollapsed, railPanel]);

  useLayoutEffect(() => {
    if (isMobile) return;
    if (drawerPanelCollapsed) drawerPanel.current?.collapse();
    else drawerPanel.current?.expand();
  }, [isMobile, drawerPanelCollapsed, drawerPanel]);

  // --- programmatic sizes -------------------------------------------------
  //
  // Only for EXPANDED regions: resizing a collapsed panel would expand
  // it, which is not what a preset switch or a keyboard nudge means.
  useLayoutEffect(() => {
    if (isMobile || dockCollapsed) return;
    dockPanel.current?.resize(`${state.regions.dock.size}%`);
  }, [isMobile, dockCollapsed, state.regions.dock.size, dockPanel]);

  useLayoutEffect(() => {
    if (isMobile || railCollapsed) return;
    railPanel.current?.resize(`${state.regions.rail.size}%`);
  }, [isMobile, railCollapsed, state.regions.rail.size, railPanel]);

  useLayoutEffect(() => {
    if (isMobile || drawerPanelCollapsed) return;
    drawerPanel.current?.resize(`${state.regions.drawer.size}%`);
  }, [isMobile, drawerPanelCollapsed, state.regions.drawer.size, drawerPanel]);

  // --- the drag cursor overlay -------------------------------------------
  const onSeparatorDown = useCallback((orientation: "horizontal" | "vertical") => {
    setDragging(orientation);
  }, []);

  useEffect(() => {
    if (!dragging) return;
    const stop = () => setDragging(null);
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
    return () => {
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
    };
  }, [dragging]);

  // --- the resize submode + the two chrome keys ---------------------------
  const { setResizeMode, nudge, equalise, setChrome, focusRegion } = desk;
  useEffect(() => {
    if (!desk.resizeMode) return;
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const cmd = resizeSubmodeKey(e.key, { focus: focusRegion, paneCount });
      // The submode is MODAL: every key is swallowed, including the ones
      // it does not act on, so a stray letter can never fall through to
      // the reader's own keymap while the chip says RESIZE.
      e.preventDefault();
      e.stopPropagation();
      switch (cmd.t) {
        case "resize":
          nudge(cmd.target, cmd.delta);
          return;
        case "equalise":
          equalise();
          return;
        case "hint":
          setHintOpen((v) => !v);
          return;
        case "exit":
          setResizeMode(false);
          setHintOpen(false);
          return;
        case "noop":
          return;
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [desk.resizeMode, focusRegion, paneCount, nudge, equalise, setResizeMode]);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key !== "F11") return;
      e.preventDefault();
      // Never a single binary toggle (NN/g's recall tax): F11 is the
      // graduated tier that keeps the stripes and the status line;
      // Shift-F11 is the one that takes them away.
      if (e.shiftKey) setChrome(chrome === "present" ? "full" : "present");
      else setChrome(chrome === "focus" ? "full" : "focus");
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [chrome, setChrome]);

  // --- layout write-back --------------------------------------------------
  const onColsLayout = useCallback(
    (layout: Layout, meta: { isUserInteraction: boolean }) => {
      if (!meta.isUserInteraction) return;
      if (typeof layout.dock === "number") {
        desk.dispatch({ type: "resize", target: "dock", size: layout.dock, user: true });
      }
      if (typeof layout.rail === "number") {
        desk.dispatch({ type: "resize", target: "rail", size: layout.rail, user: true });
      }
    },
    [desk],
  );

  const onCenterLayout = useCallback(
    (layout: Layout, meta: { isUserInteraction: boolean }) => {
      if (!meta.isUserInteraction) return;
      if (typeof layout.drawer === "number") {
        desk.dispatch({ type: "resize", target: "drawer", size: layout.drawer, user: true });
      }
    },
    [desk],
  );

  // --- stripes ------------------------------------------------------------
  const leftButtons: StripeButton[] = [
    {
      id: "dock",
      label: dockCollapsed ? "show files" : "hide files",
      icon: <Icon.Folder />,
      active: !dockCollapsed,
      cmd: "desk.toggle.dock",
      onClick: () => (isMobile ? onDockOpenChange(!dockOpen) : desk.toggleRegion("dock")),
    },
  ];
  const leftFoot: StripeButton[] = [
    {
      id: "drawer",
      label: drawerCollapsed ? "open the drawer" : "collapse the drawer",
      icon: <Icon.Terminal />,
      badge: desk.drawer.sets.filter((s) => !s.evicted).length,
      active: !drawerCollapsed,
      cmd: "desk.toggle.drawer",
      onClick: () => desk.toggleRegion("drawer"),
    },
    {
      id: "resize",
      label: "resize regions (Ctrl-w r)",
      icon: <Icon.Swap />,
      active: desk.resizeMode,
      cmd: "desk.resize",
      onClick: () => desk.setResizeMode(!desk.resizeMode),
    },
  ];
  const rightButtons: StripeButton[] = RAIL_STRIPE_META.map((m) => ({
    id: m.tab,
    label: m.label,
    icon: m.icon,
    badge: railBadges[m.tab],
    active: !railCollapsed && state.railTab === m.tab,
    cmd: `desk.rail.${m.tab}`,
    onClick: () => {
      desk.setRailTab(m.tab);
      if (isMobile) onSheetOpenChange(true);
      else if (railCollapsed) desk.dispatch({ type: "expand", region: "rail", user: true });
    },
  }));

  const railCtx: DeskRailSlotCtx = useMemo(
    () => ({
      asSheet: isMobile,
      onMobileClose: isMobile ? () => onSheetOpenChange(false) : undefined,
      tab: state.railTab,
      setTab: desk.setRailTab,
    }),
    [isMobile, onSheetOpenChange, state.railTab, desk.setRailTab],
  );

  // A rail with no subject renders NOTHING inside its region — the
  // region itself stays present and addressable (the landmark golden),
  // but its panels do not mount, so a file the reader cannot show (a
  // missing repo, a binary blob, a diff) never runs the rail's fetches
  // or surfaces their errors from behind a zero-width column.
  const railNode = railAvailable ? rail(railCtx) : null;
  const drawerNode = (
    <Drawer
      state={desk.drawer}
      dispatch={desk.drawerDispatch}
      collapsed={drawerCollapsed}
      overlay={drawerOverlay}
      onExpand={() => desk.dispatch({ type: "expand", region: "drawer", user: true })}
      onCollapse={() => desk.dispatch({ type: "collapse", region: "drawer", user: true })}
      onOpenRow={onOpenDrawerRow}
      onRamp={onRampDrawerRow}
    />
  );

  const rootClass =
    "kbc-desk" +
    (isMobile && sheetOpen ? " kbc-desk--sheet-open" : "") +
    (desk.resizeMode ? " kbc-desk--resizing" : "");

  // --- mobile (≤860px): one column, ONE sheet ------------------------------
  //
  // kb #30's mobile ruling, ported and kept intact:
  //
  //  * The dock is the existing off-canvas `MobileDrawer` (focus trap,
  //    Esc, scrim, body-scroll-lock all come from that component).
  //  * The rail is the SINGLE bottom sheet — the same
  //    `.kbc-reader__outline` element the CSS promotes on
  //    `.kbc-reader--sheet-open`, with `asSheet` adding the dialog
  //    semantics. Its own icon tab bar rides inside it, so tabs switch
  //    FROM WITHIN the sheet: never a railless dead end.
  //  * The drawer lives INSIDE that same sheet, below the rail, as its
  //    own tab strip — never a second sheet. Collapsed it is just the
  //    strip; tapping a set expands it within the sheet.
  //  * No desktop stripes: on a phone the sole entry point stays the ONE
  //    badged `data-kbc-inspector-toggle` button in the reader's header,
  //    exactly as before this unit. Spending 40px of a 390px viewport on
  //    a permanent icon column would be a regression, not a port.
  //
  // The result is that the desktop DOM below never depends on
  // `isMobile`, which is the property `e2e/mobile.spec.ts`'s
  // desktop-parity case actually asserts.
  if (isMobile) {
    return (
      <div
        className={rootClass}
        data-desk-preset={state.preset}
        data-desk-chrome={chrome}
        data-desk-center-mode={centerMode}
        data-desk-mobile="1"
      >
        <MobileDrawer
          open={dockOpen}
          onClose={() => onDockOpenChange(false)}
          title="Files"
          ariaLabel="file tree"
        >
          <div className="kbc-desk__dock kbc-reader__tree" data-region="dock">
            {dock}
          </div>
        </MobileDrawer>
        <div className="kbc-desk__main" data-region="main">
          {header}
          {children}
        </div>
        {railAvailable && (
          <aside className="kbc-reader__outline kbc-desk__rail" data-region="rail">
            <div className="kbc-desk__sheet-body">
              <div className="kbc-desk__sheet-rail">{railNode}</div>
              <div className="kbc-desk__sheet-drawer">{drawerNode}</div>
            </div>
          </aside>
        )}
      </div>
    );
  }

  return (
    <div
      className={rootClass}
      data-desk-preset={state.preset}
      data-desk-chrome={chrome}
      data-desk-center-mode={centerMode}
      data-desk-zoom={zoom ?? ""}
      data-desk-dirty={state.dirty ? "1" : "0"}
    >
      {chrome !== "present" && (
        <Stripe side="left" ariaLabel="workspace" buttons={leftButtons} footButtons={leftFoot} />
      )}
      <Group
        id="desk-cols"
        className="kbc-desk__group"
        orientation="horizontal"
        // See this module's header, rule 2 — the Lumino/Chromium trap.
        disableCursor
        defaultLayout={colsLayout(state.regions.dock.size, state.regions.rail.size)}
        onLayoutChanged={onColsLayout}
        resizeTargetMinimumSize={{ coarse: 24, fine: 8 }}
      >
        <Panel
          id="dock"
          className="kbc-desk__panel"
          panelRef={dockPanel}
          collapsible
          collapsedSize={0}
          minSize="120px"
          // The PRESET's size, not the persisted one — this is what a
          // separator double-click resets to (header rule 3).
          //
          // The `%` suffix is load-bearing: the library reads a bare
          // NUMBER as PIXELS ("Numbers are assumed to be pixels"), so
          // `defaultSize={18}` means 18px — below `minSize`, which makes
          // a `collapsible` panel collapse on mount. Every size this
          // shell hands the library is either an explicit percentage
          // string or an explicit `px` one; none is ever bare.
          defaultSize={`${DESK_PRESETS[state.preset].regions.dock.size}%`}
          style={{ overflow: "hidden" }}
        >
          <aside className="kbc-desk__dock kbc-reader__tree" data-region="dock" aria-label="files">
            {dock}
          </aside>
        </Panel>
        <Separator
          id="sep-dock"
          className="kbc-desk__sep kbc-desk__sep--v"
          data-desk-sep="dock"
          onPointerDown={() => onSeparatorDown("horizontal")}
        />
        <Panel id="center" className="kbc-desk__panel" style={{ overflow: "hidden" }}>
          <div className="kbc-desk__center" ref={centerRef}>
            <Group
              id="desk-center"
              className="kbc-desk__group kbc-desk__group--v"
              orientation="vertical"
              disableCursor
              defaultLayout={centerLayout(state.regions.drawer.size)}
              onLayoutChanged={onCenterLayout}
              resizeTargetMinimumSize={{ coarse: 24, fine: 8 }}
            >
              <Panel id="main" className="kbc-desk__panel" minSize="30%" style={{ overflow: "hidden" }}>
                <div className="kbc-desk__main" data-region="main">
                  {header}
                  {children}
                </div>
              </Panel>
              <Separator
                id="sep-drawer"
                className="kbc-desk__sep kbc-desk__sep--h"
                data-desk-sep="drawer"
                onPointerDown={() => onSeparatorDown("vertical")}
              />
              <Panel
                id="drawer"
                className="kbc-desk__panel"
                panelRef={drawerPanel}
                collapsible
                collapsedSize={DRAWER_STRIP_PX}
                minSize="120px"
                defaultSize={`${DESK_PRESETS[state.preset].regions.drawer.size}%`}
                style={{ overflow: "hidden" }}
              >
                {drawerOverlay ? <div className="kbc-desk__drawer-ghost" aria-hidden /> : drawerNode}
              </Panel>
            </Group>
            {drawerOverlay && <div className="kbc-desk__drawer-float">{drawerNode}</div>}
          </div>
        </Panel>
        <Separator
          id="sep-rail"
          className="kbc-desk__sep kbc-desk__sep--v"
          data-desk-sep="rail"
          onPointerDown={() => onSeparatorDown("horizontal")}
        />
        <Panel
          id="rail"
          className="kbc-desk__panel"
          panelRef={railPanel}
          collapsible
          collapsedSize={0}
          minSize="140px"
          defaultSize={`${DESK_PRESETS[state.preset].regions.rail.size}%`}
          style={{ overflow: "hidden" }}
        >
          <aside className="kbc-reader__outline kbc-desk__rail" data-region="rail">
            {railNode}
          </aside>
        </Panel>
      </Group>
      {chrome !== "present" && (
        <Stripe side="right" ariaLabel="inspector" buttons={rightButtons} footButtons={[]} />
      )}

      {/* The dedicated cursor overlay — the ONE element that carries a
          resize cursor during a drag. Never `document.body`. */}
      {dragging && (
        <div
          className={`kbc-desk__cursor-overlay kbc-desk__cursor-overlay--${dragging}`}
          data-desk-cursor-overlay={dragging}
          aria-hidden
        />
      )}

      {desk.resizeMode && (
        <div className="kbc-desk__mode-chip" role="status" data-desk-mode-chip>
          <strong>RESIZE</strong>
          <span>{focusRegion}</span>
          {hintOpen ? <span className="kbc-desk__mode-hint">{RESIZE_SUBMODE_HINT}</span> : <span>? for keys</span>}
        </div>
      )}

      {chrome !== "full" && (
        <div className="kbc-desk__status" role="status" data-desk-status>
          {chrome === "present" ? (
            <span>press ? for chrome</span>
          ) : (
            <>
              <span data-desk-status-preset>{DESK_PRESETS[state.preset].label}</span>
              <span>·</span>
              <span>{centerMode}</span>
              <span>·</span>
              <span>F11 to leave focus</span>
            </>
          )}
        </div>
      )}

      {chrome === "full" && (
        <div className="kbc-desk__preset-chip" data-desk-preset-chip>
          {onSaveWorkspace && (
            <button
              type="button"
              className="kbc-desk__save-workspace-btn"
              data-cmd="desk.save-workspace"
              onClick={onSaveWorkspace}
              title="Save workspace — the open files, desk layout and ref as a named set"
            >
              <Icon.Layers />
              Save workspace
            </button>
          )}
          <button
            type="button"
            className="kbc-desk__preset-btn"
            aria-haspopup="menu"
            aria-expanded={presetMenu}
            data-cmd="desk.preset"
            onClick={() => setPresetMenu((v) => !v)}
            title="desk preset"
          >
            {DESK_PRESETS[state.preset].label}
            {state.dirty && <span className="kbc-desk__preset-dirty"> · edited</span>}
          </button>
          {presetMenu && (
            <div className="kbc-desk__preset-menu" role="menu">
              {DESK_PRESET_NAMES.map((n: DeskPresetName) => (
                <button
                  key={n}
                  type="button"
                  role="menuitem"
                  className={"kbc-desk__preset-item" + (n === state.preset ? " is-on" : "")}
                  data-desk-preset-item={n}
                  onClick={() => {
                    desk.applyPreset(n);
                    setPresetMenu(false);
                  }}
                >
                  <span className="kbc-desk__preset-item-name">{DESK_PRESETS[n].label}</span>
                  <span className="kbc-desk__preset-item-hint">{DESK_PRESETS[n].hint}</span>
                </button>
              ))}
              <button
                type="button"
                role="menuitem"
                className="kbc-desk__preset-item"
                data-desk-preset-item="reset"
                onClick={() => {
                  desk.equalise();
                  setPresetMenu(false);
                }}
              >
                <span className="kbc-desk__preset-item-name">Reset</span>
                <span className="kbc-desk__preset-item-hint">back to this preset's own sizes</span>
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export type { DeskRegionId };
