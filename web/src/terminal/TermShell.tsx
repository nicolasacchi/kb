// BSHELL — the one host layout for a terminal at /term/:id.
//
// A pure layout host: no fetching, no TanStack (the rail's open tab and
// collapsed flag are local state mirrored to localStorage; terminal bytes never
// enter a query cache, #23). Slots render in order header, pane, rail.
//
//   desktop  grid `1fr minmax(var(--term-rail-w), 360px)`: the pane, and a
//            collapsible right rail with the registered tabs.
//   phone    the pane child renders unchanged (the panel brings its own
//            full-bleed sheet); the shell adds ONE non-modal bottom layer with
//            the same tabs. It is not `ui/Sheet`: a second focus trap would
//            fight the panel's. Tab content mounts only while it is expanded.
//
// The phone branch predicate is `usePhonePanel()` (the panel's own), never
// `useIsMobile()`, so shell and panel always agree. No global key binding here.

import {
  useCallback,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactElement,
  type ReactNode,
} from "react";
import { useCloseRequest } from "@kb/ui/useCloseRequest";
import { usePhonePanel } from "../hooks/useIsMobile";
import { isRailTabId, railTabs, type RailTab, type RailTabId } from "./rail/registry";
import "./termShell.css";

export type Chip = { id: string; label: string; tone?: "ok" | "warn" | "err" };

export interface TermShellProps {
  termId: string;
  harness: string;
  sid?: string;
  header?: ReactNode;
  /** The pane. */
  children: ReactNode;
  statusChips?: Chip[];
  keyRow?: ReactNode;
}

export const RAIL_TAB_KEY = "kb-term-rail-tab";
export const RAIL_OPEN_KEY = "kb-term-rail-open";

function readTab(): RailTabId {
  try {
    const v = window.localStorage.getItem(RAIL_TAB_KEY);
    return isRailTabId(v) ? v : "conversation";
  } catch {
    return "conversation";
  }
}

function readOpen(): boolean {
  try {
    return window.localStorage.getItem(RAIL_OPEN_KEY) !== "0";
  } catch {
    return true;
  }
}

function write(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // storage unavailable: the state just does not persist
  }
}

function Chips({ chips }: { chips: Chip[] | undefined }): ReactElement | null {
  if (!chips || chips.length === 0) return null;
  return (
    <ul className="kb-termshell__chips" role="list" data-testid="term-status-chips">
      {chips.map((c) => (
        <li key={c.id} className="kb-termshell__chip" data-tone={c.tone ?? "ok"}>
          {c.label}
        </li>
      ))}
    </ul>
  );
}

function tabLabel(t: RailTab): ReactNode {
  return (
    <>
      {t.label}
      {t.badge !== undefined && t.badge > 0 ? (
        <span className="kb-termshell__badge" aria-label={`${t.badge} pending`}>
          {t.badge}
        </span>
      ) : null}
    </>
  );
}

/** Tabs + the active panel; shared by the desktop rail and the phone layer. */
function TabSet({
  tabs,
  active,
  onSelect,
  idBase,
}: {
  tabs: RailTab[];
  active: RailTabId;
  onSelect: (id: RailTabId) => void;
  idBase: string;
}): ReactElement {
  const refs = useRef<Map<RailTabId, HTMLButtonElement>>(new Map());
  const current = tabs.find((t) => t.id === active) ?? tabs[0];
  const onKey = (e: KeyboardEvent<HTMLDivElement>): void => {
    const i = tabs.findIndex((t) => t.id === current?.id);
    let n = -1;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") n = (i + 1) % tabs.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") n = (i - 1 + tabs.length) % tabs.length;
    else if (e.key === "Home") n = 0;
    else if (e.key === "End") n = tabs.length - 1;
    if (n < 0) return;
    e.preventDefault();
    const next = tabs[n];
    if (!next) return;
    onSelect(next.id);
    refs.current.get(next.id)?.focus();
  };
  return (
    <>
      <div className="kb-termshell__tabs" role="tablist" aria-label="Terminal panels" onKeyDown={onKey}>
        {tabs.map((t) => {
          const on = t.id === current?.id;
          return (
            <button
              key={t.id}
              type="button"
              role="tab"
              id={`${idBase}-tab-${t.id}`}
              aria-selected={on}
              aria-controls={`${idBase}-panel`}
              tabIndex={on ? 0 : -1}
              className="kb-termshell__tab"
              data-testid={`term-tab-${t.id}`}
              ref={(el) => {
                if (el) refs.current.set(t.id, el);
                else refs.current.delete(t.id);
              }}
              onClick={() => onSelect(t.id)}
            >
              {tabLabel(t)}
            </button>
          );
        })}
      </div>
      {current ? (
        <div
          role="tabpanel"
          id={`${idBase}-panel`}
          aria-labelledby={`${idBase}-tab-${current.id}`}
          className="kb-termshell__panel"
        >
          {current.node}
        </div>
      ) : null}
    </>
  );
}

export function TermShell(props: TermShellProps): ReactElement {
  const phone = usePhonePanel();
  const idBase = useId();
  const [tab, setTab] = useState<RailTabId>(readTab);
  const [open, setOpen] = useState<boolean>(readOpen);
  const [sheetOpen, setSheetOpen] = useState(false);
  const layerRef = useRef<HTMLDivElement | null>(null);
  const tabs = railTabs();

  const selectTab = useCallback((id: RailTabId) => {
    setTab(id);
    write(RAIL_TAB_KEY, id);
  }, []);
  const toggleRail = useCallback(() => {
    setOpen((o) => {
      write(RAIL_OPEN_KEY, o ? "0" : "1");
      return !o;
    });
  }, []);
  const collapseSheet = useCallback(() => setSheetOpen(false), []);
  // CloseWatcher / platform close request: no history entry, no listener while collapsed.
  useCloseRequest(phone && sheetOpen, collapseSheet);

  const rootProps = {
    className: "kb-termshell",
    "aria-label": "Terminal workspace",
    "data-testid": "term-shell",
    "data-kb-term-layout": phone ? "phone" : "desktop",
    "data-term": props.termId,
    "data-harness": props.harness,
  } as const;

  // ONE tree for both layouts: the pane child keeps its position (bar, body >
  // pane, then the layout's own extra), so crossing the phone breakpoint never
  // remounts the panel (one socket survives a rotation).
  return (
    <section {...rootProps} data-rail={phone ? undefined : open ? "open" : "closed"}>
      <div className="kb-termshell__bar">
        <div className="kb-termshell__head">{props.header}</div>
        <Chips chips={props.statusChips} />
        {phone ? null : (
          <button
            type="button"
            className="kb-termshell__toggle"
            data-testid="term-rail-toggle"
            aria-expanded={open}
            aria-controls={`${idBase}-rail`}
            onClick={toggleRail}
          >
            {open ? "Hide panels" : "Show panels"}
          </button>
        )}
      </div>
      <div className="kb-termshell__body">
        <div className="kb-termshell__pane">{props.children}</div>
        {phone ? null : (
          <aside className="kb-termshell__rail" id={`${idBase}-rail`} hidden={!open} aria-label="Terminal panels">
            {open ? <TabSet tabs={tabs} active={tab} onSelect={selectTab} idBase={idBase} /> : null}
          </aside>
        )}
      </div>
      {phone ? (
        <div
          ref={layerRef}
          className="kb-termshell__sheet"
          data-testid="term-sheet"
          data-open={sheetOpen ? "1" : "0"}
          onKeyDown={(e) => {
            // Esc only when focus is inside this layer: never the terminal's own.
            if (e.key === "Escape" && sheetOpen && layerRef.current?.contains(document.activeElement)) {
              setSheetOpen(false);
            }
          }}
        >
          <button
            type="button"
            className="kb-termshell__handle"
            data-testid="term-sheet-handle"
            aria-expanded={sheetOpen}
            aria-controls={`${idBase}-sheet`}
            aria-label={sheetOpen ? "Hide panels" : "Show panels"}
            onClick={() => setSheetOpen((o) => !o)}
          >
            <span className="kb-termshell__grip" aria-hidden="true" />
          </button>
          {sheetOpen ? (
            <div className="kb-termshell__sheet-body" id={`${idBase}-sheet`}>
              <TabSet tabs={tabs} active={tab} onSelect={selectTab} idBase={idBase} />
            </div>
          ) : null}
          {props.keyRow}
        </div>
      ) : (
        props.keyRow
      )}
    </section>
  );
}
