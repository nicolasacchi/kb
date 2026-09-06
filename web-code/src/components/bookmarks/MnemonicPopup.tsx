import { useEffect, useMemo } from "react";
import type { Bookmark } from "../../api/types";
import { useBookmarks } from "../../hooks/useBookmarks";

export interface MnemonicPopupProps {
  open: boolean;
  onClose: () => void;
  repo: string;
  onJump: (loc: { path: string; line: number }) => void;
}

/// V3.N2 — `gM` popup: list mnemonic'd bookmarks; press [0-9a-z] to jump.
export default function MnemonicPopup({ open, onClose, repo, onJump }: MnemonicPopupProps) {
  const q = useBookmarks(repo);
  const marks = useMemo(() => {
    const list = q.data?.bookmarks ?? [];
    return list
      .filter((b): b is Bookmark & { mnemonic: string } => !!b.mnemonic)
      .sort((a, b) => a.mnemonic.localeCompare(b.mnemonic));
  }, [q.data]);

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key.length === 1 && /^[0-9a-z]$/.test(e.key)) {
        const hit = marks.find((b) => b.mnemonic === e.key);
        if (hit) {
          e.preventDefault();
          e.stopPropagation();
          onJump({ path: hit.path, line: hit.line });
          onClose();
        }
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, marks, onClose, onJump]);

  if (!open) return null;

  return (
    <div
      className="kbc-omnibox-backdrop"
      role="presentation"
      data-kbc-mnemonic-popup
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="kbc-omnibox kbc-mnemonic-popup"
        role="dialog"
        aria-modal="true"
        aria-label="bookmark mnemonics"
      >
        <div className="kbc-omnibox__head">
          <div className="kbc-omnibox__status">Jump to mnemonic (press key)</div>
        </div>
        <div className="kbc-omnibox__body" role="list">
          {marks.length === 0 && (
            <div className="kbc-omnibox__hint">No mnemonic bookmarks yet.</div>
          )}
          {marks.map((b) => (
            <button
              key={b.id}
              type="button"
              className="kbc-recent-locs__row"
              onClick={() => {
                onJump({ path: b.path, line: b.line });
                onClose();
              }}
              data-kbc-mnemonic-row
              data-kbc-mnemonic={b.mnemonic}
            >
              <span className="kbc-bookmarks__mnemonic">{b.mnemonic}</span>
              <span className="kbc-recent-locs__path">
                {b.path}:{b.line}
              </span>
              {b.note && <span className="kbc-recent-locs__snippet">{b.note}</span>}
            </button>
          ))}
        </div>
        <div className="kbc-omnibox__foot">
          <span className="kbc-omnibox__keys">
            <kbd>0-9 a-z</kbd> jump <kbd>Esc</kbd> close
          </span>
        </div>
      </div>
    </div>
  );
}
