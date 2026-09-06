// V71-E2 — the ONE action menu (D5), opened by four triggers.
//
// One component, one response, four doors: right-click inside the code
// surface, `.`, `Shift+F10` and the ContextMenu key (the last two are the
// platform's own, wired by hand — MDN/APG are explicit that ARIA roles grant
// no behaviour). The drag-select pill and the mobile sheet render from the
// SAME `ActionsOut`; nothing is composed client-side (risk 10).
//
// Two things here are not decoration:
//
//  * **The target is a SEGMENTED CONTROL, never a hidden cycle.** Embark's
//    "repeat the key to cycle" is invisible; this renders the whole ordered
//    target list as buttons in the header, with the active one pressed, and
//    `Tab`/click switches. Choosing a target re-fetches, because the ROW SET
//    is per target kind and pretending otherwise would show a symbol's rows
//    under a range chip.
//  * **The last row says how to get the browser's own menu.** Overriding
//    `contextmenu` costs the user their extensions, translate and
//    spell-check; Google Docs' undiscoverable ⌥⇧-right-click is the
//    counter-example the research names. Shift+right-click passes through
//    (free on Firefox, `e.shiftKey` on Chromium — the surface's handler
//    honours it), and this footer row states it.

import { useEffect, useMemo, useRef, useState } from "react";
import type { ActionRow, ActionsOut } from "../../api/types";
import { filterGroups, menuAccessibleName, menuOrder } from "../../lib/actionOps";

export interface ActionMenuProps {
  /// Where to anchor (viewport coordinates). The mobile sheet ignores it.
  at: { x: number; y: number };
  out: ActionsOut | null;
  loading: boolean;
  error?: string | null;
  /// Switch the segmented control — the host re-fetches with `?target=`.
  onTarget: (index: number) => void;
  onRun: (row: ActionRow) => void;
  onClose: () => void;
  /// ≤860px: ONE bottom sheet, per root CLAUDE.md #30's one-mobile-entry
  /// rule (and web-code/CLAUDE.md's Desk section, which states it for this
  /// shell). Never a second sheet beside the rail's.
  asSheet?: boolean;
}

export default function ActionMenu({
  at,
  out,
  loading,
  error,
  onTarget,
  onRun,
  onClose,
  asSheet = false,
}: ActionMenuProps) {
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const rootRef = useRef<HTMLDivElement | null>(null);

  const groups = useMemo(() => (out ? filterGroups(out, query) : []), [out, query]);
  const rows = useMemo(() => menuOrder(groups), [groups]);
  const target = out?.targets[out.active];

  useEffect(() => {
    setCursor(0);
  }, [query, out?.active]);

  // APG: focus lands on the menu when it opens, and Escape returns it to the
  // invoking context — the host owns the second half (it restores the
  // buffer's focus in `onClose`).
  useEffect(() => {
    rootRef.current?.focus();
  }, []);

  useEffect(() => {
    function onDocMouseDown(e: MouseEvent) {
      if (!rootRef.current?.contains(e.target as Node)) onClose();
    }
    document.addEventListener("mousedown", onDocMouseDown);
    return () => document.removeEventListener("mousedown", onDocMouseDown);
  }, [onClose]);

  function onKeyDown(e: React.KeyboardEvent) {
    switch (e.key) {
      case "Escape":
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      case "ArrowDown":
      case "ArrowUp": {
        e.preventDefault();
        e.stopPropagation();
        if (rows.length === 0) return;
        const d = e.key === "ArrowDown" ? 1 : -1;
        setCursor((c) => ((c + d) % rows.length + rows.length) % rows.length);
        return;
      }
      case "Tab": {
        // Embark's target cycling, made visible: Tab steps the segmented
        // control rather than leaving the menu.
        if (!out || out.targets.length < 2) return;
        e.preventDefault();
        e.stopPropagation();
        const next = (out.active + (e.shiftKey ? -1 : 1) + out.targets.length) % out.targets.length;
        onTarget(next);
        return;
      }
      case "Enter": {
        e.preventDefault();
        e.stopPropagation();
        const row = rows[cursor];
        if (row?.enabled) onRun(row);
        return;
      }
      default:
        break;
    }
    // A single printable key that is a row's own `key` runs it directly —
    // Embark/Helix's fast path, and the reason every row carries one.
    if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey && query === "") {
      const hit = rows.find((r) => r.key === e.key && r.enabled);
      if (hit) {
        e.preventDefault();
        e.stopPropagation();
        onRun(hit);
      }
    }
  }

  const style = asSheet ? undefined : { left: `${at.x}px`, top: `${at.y}px` };
  let flat = -1;

  return (
    <div
      ref={rootRef}
      className={"kbc-actions" + (asSheet ? " kbc-actions--sheet" : "")}
      style={style}
      role={asSheet ? "dialog" : "menu"}
      aria-modal={asSheet ? true : undefined}
      aria-label={target ? menuAccessibleName(target, rows.length) : "Actions"}
      tabIndex={-1}
      data-kbc-action-menu
      onKeyDown={onKeyDown}
      onContextMenu={(e) => e.preventDefault()}
    >
      <header className="kbc-actions__head">
        <div className="kbc-actions__targets" role="group" aria-label="target">
          {(out?.targets ?? []).map((t, i) => (
            <button
              key={`${t.kind}:${t.label}:${i}`}
              type="button"
              className={"kbc-actions__target" + (i === out?.active ? " is-on" : "")}
              aria-pressed={i === out?.active}
              title={t.note ?? `${t.kind} — ${t.label}`}
              data-kbc-action-target={t.kind}
              onClick={() => onTarget(i)}
            >
              <span className="kbc-actions__target-kind">{t.kind}</span>
              <span className="kbc-actions__target-label">{t.label}</span>
            </button>
          ))}
        </div>
        {target?.note && <p className="kbc-actions__target-note">{target.note}</p>}
        <input
          type="text"
          className="kbc-actions__filter"
          placeholder="filter…"
          value={query}
          aria-label="filter actions"
          data-kbc-action-filter
          onChange={(e) => setQuery(e.target.value)}
        />
        {asSheet && (
          <button type="button" className="kbc-actions__close" aria-label="close actions" onClick={onClose}>
            ✕
          </button>
        )}
      </header>

      {loading && <p className="kbc-actions__note">resolving…</p>}
      {error && <p className="kbc-actions__note is-error">{error}</p>}
      {out && rows.length === 0 && !loading && (
        <p className="kbc-actions__note">No actions available at this location.</p>
      )}

      {groups.map((g) => (
        <section key={g.id} className="kbc-actions__group" role="group" aria-label={g.title}>
          <h3 className="kbc-actions__group-head">{g.title}</h3>
          {g.actions.map((a) => {
            flat += 1;
            const idx = flat;
            return (
              <button
                key={a.id}
                type="button"
                role="menuitem"
                className={
                  "kbc-actions__row" +
                  (idx === cursor ? " is-cursor" : "") +
                  (a.mutating ? " is-mutating" : "")
                }
                aria-disabled={!a.enabled}
                disabled={!a.enabled}
                title={a.doc}
                data-kbc-action={a.id}
                onMouseEnter={() => setCursor(idx)}
                onClick={() => a.enabled && onRun(a)}
              >
                {a.key && <kbd className="kbc-actions__row-key">{a.key}</kbd>}
                <span className="kbc-actions__row-title">{a.title}</span>
                {a.mutating && <span className="kbc-actions__row-flag">changes files</span>}
                {!a.enabled && a.disabled_reason && (
                  <span className="kbc-actions__row-why">{a.disabled_reason}</span>
                )}
              </button>
            );
          })}
        </section>
      ))}

      {out && !out.mutations.available && (
        <p className="kbc-actions__note" data-kbc-action-mutations-note>
          {out.mutations.reason}
        </p>
      )}
      {out?.notes.map((n) => (
        <p key={n} className="kbc-actions__note">
          {n}
        </p>
      ))}

      {/* The escape hatch, IN the menu — never an undiscoverable chord. */}
      <p className="kbc-actions__browser" data-kbc-action-browser-row>
        Browser menu — ⇧ right-click
      </p>
    </div>
  );
}

/// The drag-select pill: the top three rows of the SAME list, derived, plus
/// a "…" that opens the full menu. Desktop only — the research is explicit
/// that a floating pill on touch fights the OS's own selection bubble.
export function ActionPill({
  at,
  rows,
  onRun,
  onMore,
}: {
  at: { x: number; y: number };
  rows: ActionRow[];
  onRun: (row: ActionRow) => void;
  onMore: () => void;
}) {
  return (
    <div
      className="kbc-actions__pill"
      style={{ left: `${at.x}px`, top: `${at.y}px` }}
      data-kbc-action-pill
      role="toolbar"
      aria-label="actions for the selection"
    >
      {rows.map((r) => (
        <button
          key={r.id}
          type="button"
          className="kbc-actions__pill-row"
          title={r.doc}
          data-kbc-action-pill-row={r.id}
          onClick={() => onRun(r)}
        >
          {r.title}
        </button>
      ))}
      <button
        type="button"
        className="kbc-actions__pill-more"
        title="every action for this selection"
        aria-label="more actions"
        data-kbc-action-pill-more
        onClick={onMore}
      >
        …
      </button>
    </div>
  );
}
