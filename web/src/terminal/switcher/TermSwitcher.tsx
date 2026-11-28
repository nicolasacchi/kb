// TERMSWITCH — the header's terminal switcher: a "Terminals" button (live
// count + needs-you badge) opening an in-place listbox grouped "This project" /
// "Everywhere". Data is the existing `["terms"]` + live-status queries and the
// TERMNOTIFY marks store: no fetch, stream or query key of its own, and no
// terminal bytes. The popover is NOT portalled (a phone panel is a full-bleed
// Sheet; a body-level popover would sit behind it).

import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type ReactElement } from "react";
import { useNavigate } from "react-router-dom";
import { useIdentity } from "../../hooks/useArtifactHost";
import { useLiveSessions } from "../../hooks/useLiveSessions";
import { useTerms } from "../useTerms";
import { loadPrefs } from "../hub/termPrefs";
import { buildHubUrl } from "../hub/termHub";
import { useTermAttention } from "../notify/attention";
import { buildSwitcher, type SwitcherRow } from "./switcherModel";
import { chordOf, nextId, nextNeedsId } from "./switcherKeys";
import "./switcher.css";

const markOf = (r: SwitcherRow): "asks" | "finished" | null => (r.asks ? "asks" : r.finished ? "finished" : null);
const MARK_TEXT = { asks: "needs you", finished: "finished" } as const;

export default function TermSwitcher({ currentId }: { currentId: string }): ReactElement | null {
  const identity = useIdentity();
  if (identity?.terminal_admitted !== true) return null;
  return <Switcher currentId={currentId} />;
}

function Switcher({ currentId }: { currentId: string }): ReactElement {
  const navigate = useNavigate();
  const { terms, isLoading, isError } = useTerms();
  const { rows: live } = useLiveSessions();
  const marks = useTermAttention();
  const prefs = useMemo(() => loadPrefs(), [terms]); // eslint-disable-line react-hooks/exhaustive-deps
  const model = useMemo(
    () => buildSwitcher(terms, live, prefs, marks, currentId),
    [terms, live, prefs, marks, currentId],
  );
  const [open, setOpen] = useState(false);
  const [activeId, setActiveId] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const uid = useId();
  const optId = (id: string): string => `${uid}-opt-${id}`;

  const latest = useRef<{ model: typeof model; currentId: string; go: (id: string) => void }>({
    model,
    currentId,
    go: () => {},
  });
  latest.current = {
    model,
    currentId,
    go: (id) => {
      setOpen(false);
      void navigate(buildHubUrl({ id }));
    },
  };

  // Alt chords: capture phase on window so a focused xterm never sees a match.
  // Registered only while this component is mounted.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent): void => {
      const chord = chordOf(e);
      if (chord === null) return;
      const { model: m, currentId: cur, go } = latest.current;
      const to = chord === "needs" ? nextNeedsId(m.flat, cur) : nextId(m.flat, cur, chord === "next" ? 1 : -1);
      // No target: the chord is not ours to steal from the pane.
      if (to === null) return;
      e.preventDefault();
      e.stopPropagation();
      // Key-repeat must not navigate (and remount terminals) at repeat rate.
      if (!e.repeat) go(to);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const close = (refocus: boolean): void => {
    setOpen(false);
    if (refocus) button.current?.focus();
  };

  useEffect(() => {
    if (!open) return;
    // Outside pointerdown closes WITHOUT refocusing the button: the user just
    // pointed somewhere else (often the terminal) and focus must follow that.
    const onDown = (e: PointerEvent): void => {
      if (root.current && !root.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onDown, true);
    return () => document.removeEventListener("pointerdown", onDown, true);
  }, [open]);

  useEffect(() => {
    if (open) list.current?.focus();
  }, [open]);

  const flat = model.flat;
  // The active row is keyed by terminal id and its index derived per render, so a
  // live re-sort (beats, marks) can never move Enter onto another terminal.
  const activeRow: SwitcherRow | undefined = flat.find((r) => r.id === activeId) ?? flat[0];
  const activeIdx = activeRow ? flat.indexOf(activeRow) : -1;
  const toggle = (): void => {
    if (!open) {
      const first =
        flat.find((r) => r.tier === "needs" && !r.current) ?? flat.find((r) => !r.current) ?? flat[0];
      setActiveId(first?.id ?? null);
    }
    setOpen(!open);
  };
  const step = (to: number): void => {
    const r = flat[to];
    if (r) setActiveId(r.id);
  };

  const onListKey = (e: KeyboardEvent<HTMLDivElement>): void => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    const n = flat.length;
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        if (n > 0) step((activeIdx + 1) % n);
        break;
      case "ArrowUp":
        e.preventDefault();
        if (n > 0) step((activeIdx - 1 + n) % n);
        break;
      case "Home":
        e.preventDefault();
        step(0);
        break;
      case "End":
        e.preventDefault();
        step(Math.max(0, n - 1));
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        if (activeRow) latest.current.go(activeRow.id);
        break;
      case "Escape":
        e.preventDefault();
        e.stopPropagation();
        close(true);
        break;
      case "Tab":
        setOpen(false);
        break;
    }
  };

  const loadingCaption = isLoading && terms.length === 0;
  const errorCaption = isError && terms.length === 0;

  return (
    <div className="kb-term-switcher" ref={root} data-testid="kb-term-switcher">
      <button
        type="button"
        ref={button}
        className="kb-term__btn kb-term-switcher__btn"
        data-testid="kb-term-switcher-btn"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-keyshortcuts="Alt+ArrowDown Alt+ArrowUp Alt+N"
        title="Terminals (Alt+Up / Alt+Down switch, Alt+N next that needs you)"
        onClick={toggle}
      >
        <span>Terminals</span>{" "}
        <span className="kb-term-switcher__count" data-testid="kb-term-switcher-count">
          {model.count}
        </span>
        {model.needs > 0 ? (
          <span
            className="kb-term-switcher__badge"
            data-testid="kb-term-switcher-needs"
            aria-label={`${model.needs} need you`}
          >
            {model.needs}
          </span>
        ) : null}
      </button>
      {open ? (
        <div className="kb-term-switcher__pop" data-testid="kb-term-switcher-pop">
          {loadingCaption ? (
            <p className="kb-term-switcher__cap" role="status">
              Loading terminals…
            </p>
          ) : null}
          {errorCaption ? (
            <p className="kb-term-switcher__cap" role="status">
              Cannot read the terminal list.
            </p>
          ) : null}
          {!loadingCaption && !errorCaption && flat.length === 0 ? (
            <p className="kb-term-switcher__cap" role="status">
              No running terminals.
            </p>
          ) : null}
          {flat.length > 0 ? (
            <div
              ref={list}
              role="listbox"
              tabIndex={-1}
              aria-label="Terminals"
              aria-activedescendant={activeRow ? optId(activeRow.id) : undefined}
              className="kb-term-switcher__list"
              data-testid="kb-term-switcher-list"
              onKeyDown={onListKey}
            >
              {model.groups.map((g) => (
                <div role="group" aria-labelledby={`${uid}-g-${g.key}`} key={g.key}>
                  <div id={`${uid}-g-${g.key}`} role="presentation" className="kb-term-switcher__group">
                    {g.label}
                  </div>
                  {g.rows.map((r) => (
                    <div
                      key={r.id}
                      id={optId(r.id)}
                      role="option"
                      aria-selected={r.current}
                      aria-current={r.current ? "true" : undefined}
                      className="kb-term-switcher__row"
                      data-testid="kb-term-switcher-row"
                      data-term={r.id}
                      data-mark={markOf(r) ?? undefined}
                      data-active={activeRow?.id === r.id ? "true" : undefined}
                      onClick={() => latest.current.go(r.id)}
                    >
                      <span className="kb-term-switcher__title">{r.title}</span>
                      <span className="kb-term-switcher__meta">
                        {r.harness}
                        {g.key === "everywhere" ? ` · ${r.project}` : ""}
                        {r.current ? " · this terminal" : ""}
                      </span>
                      {markOf(r) ? (
                        <span className={`kb-term-switcher__mark kb-term-switcher__mark--${markOf(r)}`}>
                          <span className="kb-term-switcher__shape" aria-hidden="true" />
                          {MARK_TEXT[markOf(r)!]}
                        </span>
                      ) : null}
                    </div>
                  ))}
                </div>
              ))}
            </div>
          ) : null}
          {flat.length === 1 && flat[0].current ? (
            <p className="kb-term-switcher__cap" role="status">
              No other terminals.
            </p>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
