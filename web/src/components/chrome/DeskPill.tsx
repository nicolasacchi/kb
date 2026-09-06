// kb desk v2 — Header attention pill. Same hidden-at-zero / no-flash-0
// convention as WaitingChip/InboxPill, extracted to its own file so tests
// don't mount the whole Header. Click opens a popover of desk items;
// a row navigates via artifactHref (never a hand-built URL).

import { useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { Icon } from "../icons";
import { useDesk } from "../../hooks/useDesk";
import type { DeskItem } from "../../api/desk";
import { artifactHref } from "../../lib/artifactHref";
import { relativeAge } from "../../lib/time";

export default function DeskPill() {
  const { items, attention, loading, error } = useDesk();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDocClick(e: MouseEvent) {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // Hidden entirely while loading/error and when attention is 0 — never a
  // flashing 0 (WaitingChip/InboxPill's same zero-cost empty render).
  if (loading || error || attention === 0) return null;

  return (
    <div className="kb-desk-wrap" ref={wrapRef}>
      <button
        type="button"
        className="kb-desk"
        data-testid="header-desk"
        title={`${attention} desk item${attention === 1 ? "" : "s"} needing attention`}
        aria-label={`desk: ${attention} needing attention`}
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={() => setOpen((o) => !o)}
      >
        <Icon.Doc aria-hidden />
        <span>{attention}</span>
      </button>
      {open && (
        <div className="kb-desk-pop" role="dialog" aria-label="desk">
          <div className="kb-desk-pop__head">Desk</div>
          {items.length === 0 ? (
            <div className="kb-desk-pop__empty">none yet</div>
          ) : (
            <ul className="kb-desk-pop__list">
              {items.map((item) => (
                <li key={`${item.kb}:${item.id}`}>
                  <DeskRow item={item} onNavigate={() => setOpen(false)} />
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

function DeskRow({ item, onNavigate }: { item: DeskItem; onNavigate: () => void }) {
  const href = artifactHref(item.kb, item.source_relative);
  return (
    <Link
      to={href}
      className="kb-desk-pop__row"
      data-testid={`desk-row-${item.id}`}
      onClick={onNavigate}
    >
      <span className="kb-desk-pop__title">{item.title || item.source_relative}</span>
      <span className="kb-desk-pop__meta">
        <span className="kb-desk-pop__kb">{item.kb}</span>
        <span className="kb-desk-pop__state">{item.read_state}</span>
        {item.changed_since_read && (
          <span className="kb-desk-pop__changed">changed</span>
        )}
        <span className="kb-desk-pop__age">{relativeAge(item.updated_unix)}</span>
      </span>
    </Link>
  );
}
