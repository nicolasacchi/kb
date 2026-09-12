import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { Icon } from "./icons";
import { UNCOMMITTED_SHA } from "../api/types";
import { useBlameTimeline } from "../hooks/useBlameTimeline";
import { commitUrl } from "../lib/codeUrl";
import { formatUnixSeconds, shortSha } from "../lib/format";

export interface LineHistoryPopupProps {
  open: boolean;
  onClose: () => void;
  repo: string;
  path: string;
  /// First line of the visual-line selection (server timeline is single-line).
  line: number;
  /// Optional end of the selection — only affects the title copy.
  lineEnd?: number;
}

/// R12 — history-for-selection popup (`gh`). Lists `GET /api/blame/timeline`
/// for the selection's FIRST line (the route is single-line only). Enter /
/// click → `~commit/:sha`.
export default function LineHistoryPopup({
  open,
  onClose,
  repo,
  path,
  line,
  lineEnd,
}: LineHistoryPopupProps) {
  const navigate = useNavigate();
  const timeline = useBlameTimeline(repo, path, line, open);
  const [cursor, setCursor] = useState(0);
  const listRef = useRef<HTMLUListElement | null>(null);

  const entries = (timeline.data?.entries ?? []).filter((e) => e.sha !== UNCOMMITTED_SHA);

  useEffect(() => {
    if (!open) return;
    setCursor(0);
  }, [open, line, path]);

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key === "ArrowDown" || e.key === "j") {
        e.preventDefault();
        setCursor((c) => Math.min(entries.length - 1, c + 1));
        return;
      }
      if (e.key === "ArrowUp" || e.key === "k") {
        e.preventDefault();
        setCursor((c) => Math.max(0, c - 1));
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        const row = entries[cursor];
        if (!row) return;
        onClose();
        navigate(commitUrl(repo, row.sha));
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, entries, cursor, navigate, onClose, repo]);

  if (!open) return null;

  const rangeLabel =
    lineEnd != null && lineEnd !== line ? `lines ${line}–${lineEnd}` : `line ${line}`;

  return (
    <div
      className="kbc-omnibox-backdrop"
      role="presentation"
      data-kbc-line-history
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="kbc-omnibox kbc-line-history"
        role="dialog"
        aria-modal="true"
        aria-label={`history for ${rangeLabel}`}
      >
        <div className="kbc-omnibox__head">
          <div className="kbc-line-history__title" data-kbc-line-history-title>
            History for {path}:{rangeLabel}
            <span className="kbc-line-history__note"> (first line; server is single-line)</span>
          </div>
          <button type="button" className="kbc-kbdhelp__close" onClick={onClose} aria-label="close">
            <Icon.X />
          </button>
        </div>
        {timeline.isLoading ? (
          <div className="kbc-reader__hint">Loading timeline…</div>
        ) : timeline.isError ? (
          <div className="kbc-reader__hint kbc-reader__hint--error">
            {(timeline.error as Error).message}
          </div>
        ) : entries.length === 0 ? (
          <div className="kbc-reader__hint">No history for this line.</div>
        ) : (
          <ul className="kbc-line-history__list" ref={listRef} data-kbc-line-history-list>
            {entries.map((e, i) => (
              <li key={e.sha}>
                <button
                  type="button"
                  className={
                    "kbc-line-history__row" + (i === cursor ? " kbc-line-history__row--active" : "")
                  }
                  onClick={() => {
                    onClose();
                    navigate(commitUrl(repo, e.sha));
                  }}
                  onMouseEnter={() => setCursor(i)}
                  data-kbc-line-history-row={e.sha}
                >
                  <code className="kbc-why__sha">{shortSha(e.sha)}</code>
                  <span className="kbc-line-history__date">{formatUnixSeconds(e.author_time)}</span>
                  <span className="kbc-line-history__subject">{e.subject}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
