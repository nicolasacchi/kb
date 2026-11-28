// HUBSWITCH — the hub's switcher column on /terminals/:id (desktop). The same
// client join the hub list runs (joinHub -> orderHub), grouped "This project" /
// "Everywhere" with a collapsed Ended group, needs-you first, with TERMNOTIFY's
// attention marks. Fetches nothing of its own (useSwitchRows reads the two
// shared hooks). The `terminals` key scope is mounted by HubFrame, so the keys
// work whether the column is open or collapsed.

import { useMemo, useRef, useState, type KeyboardEvent, type ReactElement } from "react";
import { Link } from "react-router-dom";
import { useNowTick } from "../../hooks/useNowTick";
import { relativeAge } from "../../lib/time";
import { buildHubUrl, type HubRow, type HubTier } from "./termHub";
import { useSwitchRows } from "./useSwitchRows";
import { groupSwitcher } from "./switchModel";
import { AttentionMarks } from "./AttentionMarks";
import "../../styles/hubSwitcher.css";

const TIER_LABEL: Record<HubTier, string> = {
  needs: "needs you",
  pinned: "pinned",
  working: "working",
  idle: "idle",
  ended: "ended",
};

function SwitchRow({
  row,
  now,
  current,
  tabbable,
  onFocusRow,
}: {
  row: HubRow;
  now: number;
  current: boolean;
  tabbable: boolean;
  onFocusRow: (id: string) => void;
}): ReactElement {
  const { term, live, tier } = row;
  const age = relativeAge(tier === "needs" ? row.needsSince : term.started_unix, now);
  return (
    <li className="kb-hubsw__item">
      <Link
        className="kb-hubsw__row"
        to={buildHubUrl({ id: term.id })}
        data-testid="switch-row"
        data-switch-row=""
        data-term={term.id}
        data-tier={tier}
        aria-current={current ? "page" : undefined}
        tabIndex={tabbable ? 0 : -1}
        onFocus={() => onFocusRow(term.id)}
      >
        <span className="kb-hubsw__top">
          <span className="kb-hubsw__name">{row.title}</span>
          <span className="kb-hubsw__age">{age}</span>
        </span>
        <span className="kb-hubsw__sub">
          <span className="kb-hubsw__tier" data-tier={tier}>
            {TIER_LABEL[tier]}
          </span>
          <AttentionMarks termId={term.id} />
          <span className="kb-hubsw__meta">
            {row.project} · {term.harness}
          </span>
        </span>
        {live?.last_line ? <span className="kb-hubsw__line">{live.last_line}</span> : null}
      </Link>
    </li>
  );
}

export function HubSwitcher({ currentId }: { currentId: string | null }): ReactElement {
  const now = useNowTick();
  const all = useSwitchRows();
  const currentProject = useMemo(
    () => (currentId === null ? null : (all.find((r) => r.term.id === currentId)?.project ?? null)),
    [all, currentId],
  );
  const groups = useMemo(() => groupSwitcher(all, currentProject), [all, currentProject]);
  const needsCount = useMemo(() => all.filter((r) => r.tier === "needs").length, [all]);

  const listRef = useRef<HTMLElement | null>(null);
  const [focusId, setFocusId] = useState<string | null>(null);
  const visible = [...groups.thisProject, ...groups.everywhere, ...groups.ended];
  // Roving tabindex: the focused row, else the open one, else the first.
  const stop =
    [focusId, currentId].find((id) => id !== null && visible.some((r) => r.term.id === id)) ??
    visible[0]?.term.id ??
    null;

  const onKeyDown = (e: KeyboardEvent<HTMLElement>): void => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const t = e.target as HTMLElement;
    if (!t.hasAttribute("data-switch-row")) return;
    const links = [...(listRef.current?.querySelectorAll<HTMLElement>("[data-switch-row]") ?? [])].filter(
      (el) => el.closest("details:not([open])") === null,
    );
    const i = links.indexOf(t);
    if (i === -1) return;
    const next = links[i + (e.key === "ArrowDown" ? 1 : -1)];
    if (!next) return;
    e.preventDefault();
    next.focus();
  };

  const group = (title: string, rows: HubRow[], testId: string): ReactElement | null =>
    rows.length === 0 ? null : (
      <section className="kb-hubsw__group" data-testid={testId} aria-label={title}>
        <h3 className="kb-hubsw__heading">{title}</h3>
        <ul className="kb-hubsw__list">
          {rows.map((r) => (
            <SwitchRow
              key={r.term.id}
              row={r}
              now={now}
              current={r.term.id === currentId}
              tabbable={r.term.id === stop}
              onFocusRow={setFocusId}
            />
          ))}
        </ul>
      </section>
    );

  return (
    <nav
      className="kb-hubsw"
      aria-label="Switch terminal"
      data-testid="hub-switcher"
      ref={listRef}
      onKeyDown={onKeyDown}
    >
      <header className="kb-hubsw__head">
        <span className="kb-hubsw__title">Terminals</span>
        <span className={`kb-hubsw__count${needsCount > 0 ? " is-needs" : ""}`} data-testid="switch-needs-count">
          {needsCount} need you
        </span>
      </header>
      {all.length === 0 ? (
        <p className="kb-hubsw__empty" data-testid="switch-empty">
          No terminals yet.
        </p>
      ) : (
        <>
          {group("This project", groups.thisProject, "switch-this")}
          {group("Everywhere", groups.everywhere, "switch-everywhere")}
          {groups.ended.length > 0 && (
            <details className="kb-hubsw__ended" data-testid="switch-ended">
              <summary>Ended ({groups.ended.length})</summary>
              <ul className="kb-hubsw__list">
                {groups.ended.map((r) => (
                  <SwitchRow
                    key={r.term.id}
                    row={r}
                    now={now}
                    current={r.term.id === currentId}
                    tabbable={r.term.id === stop}
                    onFocusRow={setFocusId}
                  />
                ))}
              </ul>
            </details>
          )}
        </>
      )}
    </nav>
  );
}
