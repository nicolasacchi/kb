import { useEffect, useRef } from "react";
import { Icon } from "../icons";
import { glyphForKind } from "../../lib/slateGlyphs";
import { relativeAge } from "../../lib/time";
import type { SlateHistoryRow } from "../../api/slateTypes";

// The history drawer (§10 "Crossing out" / §7 D17's "an attributed
// tombstone"): every dropped and superseded post, struck through, with WHO
// hid it and WHY. This is the one place a wiped note still exists — the
// board itself never renders a hidden post.
//
// On mobile it is a bottom SHEET with a scrim, dismissed by ✕, scrim tap or
// Esc — the `.kb-pinsp` pattern from `styles/mobile.css`. On desktop it is a
// right-hand drawer. Both are the same DOM; only the CSS differs, so the
// e2e selectors and the aria tree are identical at both widths.

type Props = {
  open: boolean;
  rows: SlateHistoryRow[];
  loading: boolean;
  error: string | null;
  onClose: () => void;
  mobile?: boolean;
  /// A `(was #n)` link flashes its ancestor here.
  flashSeq?: number | null;
};

export default function HistoryDrawer({
  open,
  rows,
  loading,
  error,
  onClose,
  mobile = false,
  flashSeq = null,
}: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  useEffect(() => {
    if (open && flashSeq != null) {
      ref.current
        ?.querySelector(`[data-hist-seq="${flashSeq}"]`)
        ?.scrollIntoView({ block: "center", behavior: "smooth" });
    }
  }, [open, flashSeq, rows]);

  return (
    <>
      {mobile && (
        <div
          className={`kb-pinsp-scrim${open ? " is-open" : ""}`}
          onClick={onClose}
          aria-hidden="true"
        />
      )}
      <div
        ref={ref}
        className={`slate-hist${open ? " is-open" : ""}`}
        id="slate-history"
        hidden={!open}
        {...(mobile ? { role: "dialog" as const, "aria-modal": true } : {})}
        aria-label="History — dropped and superseded posts"
      >
        <header className="slate-hist__head">
          <h2 className="slate-hist__title">History</h2>
          <button
            type="button"
            className="slate-hist__close"
            onClick={onClose}
            aria-label="Close history"
          >
            <Icon.X />
          </button>
        </header>
        {error && (
          <p className="slate-hist__error" role="alert">
            {error}
          </p>
        )}
        {!error && loading && <p className="slate-hist__empty">loading…</p>}
        {!error && !loading && rows.length === 0 && (
          <p className="slate-hist__empty">
            Nothing has been dropped or rewritten yet.
          </p>
        )}
        <ul className="slate-hist__rows">
          {rows.map((r) => {
            const k = glyphForKind(r.post.kind);
            return (
              <li
                key={r.post.seq}
                className={`slate-hist__row${flashSeq === r.post.seq ? " is-flashing" : ""}`}
                data-hist-seq={r.post.seq}
              >
                <span className="slate-hist__kind">
                  <span aria-hidden="true">{k.glyph}</span> {k.word}
                </span>
                <span className="slate-hist__seq">#{r.post.seq}</span>
                {/* Struck through — the ONE place strikethrough is used, and
                    it is decoration on top of the prose reason beside it,
                    never the only cue (§4 "Crossing out"). */}
                <s className="slate-hist__line">{r.post.line}</s>
                <span className="slate-hist__why">
                  {r.reason} by {r.who} (#{r.hidden_by})
                  {r.why ? ` — ${r.why}` : ""} · {relativeAge(r.at)}
                </span>
              </li>
            );
          })}
        </ul>
      </div>
    </>
  );
}
