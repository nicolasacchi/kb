import { useEffect, useMemo, useState } from "react";
import { assignHints, type HintTarget } from "../lib/hints";

export type HintActivateMode = "click" | "tab";

// W2.6b — hint mode's overlay. Mounted by `HotkeyRoot` when `f`/`F` fires;
// it owns the keyboard OUTRIGHT while open (the W2.6a two-layer rule) via a
// capture-phase window listener that `stopPropagation()`s every key it
// understands (a–z, Escape) — this is what stops HotkeyRoot's own
// bubble-phase listener from also seeing (and mis-resolving) an arbitrary
// hint letter like `g` or `?` (recon §1's "collision map"). Everything else
// (Tab, arrow keys, modifier chords) passes through untouched.
//
// `role="dialog" aria-modal="true"` doubles as the marker
// `useRovingCursor.ts`'s `isOverlayOpen()` already polls for, so the
// gallery/search roving cursor goes quiet for free while hints are shown.
export default function HintOverlay({
  mode,
  onClose,
}: {
  mode: HintActivateMode;
  onClose: () => void;
}) {
  const [targets] = useState<HintTarget[]>(() => assignHints());
  const [typed, setTyped] = useState("");

  // Nothing to hint (an empty view, or every candidate got filtered out) —
  // close immediately rather than leaving a dead overlay that still eats
  // keystrokes with nothing to show for it.
  useEffect(() => {
    if (targets.length === 0) onClose();
  }, [targets, onClose]);

  useEffect(() => {
    function activate(target: HintTarget) {
      if (mode === "tab" && target.href) {
        window.open(target.href, "_blank", "noopener,noreferrer");
      } else {
        target.element.click();
      }
      onClose();
    }

    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey) return; // let real browser shortcuts through
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      const key = e.key.toLowerCase();
      if (!/^[a-z]$/.test(key)) return; // Tab/arrows/F5/etc. pass through untouched
      const nextTyped = typed + key;
      const matches = targets.filter((t) => t.label.startsWith(nextTyped));
      if (matches.length === 0) {
        // No candidate extends this — ignore the keystroke rather than
        // reset, so a mistype doesn't discard useful narrowing.
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      const exact = matches.find((t) => t.label === nextTyped);
      if (exact) {
        // An exact label match always resolves immediately, even if a
        // longer (2-key) label also happens to start with the same
        // characters — see `hints.ts`'s `labelFor` doc for why that
        // ordering choice is deliberate rather than a bug.
        activate(exact);
        return;
      }
      setTyped(nextTyped);
    }
    // Capture phase on window fires before HotkeyRoot's own bubble-phase
    // window listener even gets a look — `stopPropagation()` above then
    // stops it from ever reaching that phase for the keys this overlay
    // claims.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [mode, onClose, targets, typed]);

  useEffect(() => {
    // Scroll/resize invalidate every chip's captured position — closing is
    // simpler and safer than re-measuring mid-flight.
    function dismiss() {
      onClose();
    }
    window.addEventListener("scroll", dismiss, true);
    window.addEventListener("resize", dismiss);
    return () => {
      window.removeEventListener("scroll", dismiss, true);
      window.removeEventListener("resize", dismiss);
    };
  }, [onClose]);

  const visible = useMemo(
    () => targets.filter((t) => t.label.startsWith(typed)),
    [targets, typed],
  );

  if (visible.length === 0) return null;

  return (
    <div className="kb-hints" role="dialog" aria-modal="true" aria-label="hint mode">
      {visible.map((t) => {
        const rect = t.element.getBoundingClientRect();
        return (
          <span
            key={t.label}
            className="kb-hints__chip"
            style={{ top: rect.top, left: rect.left }}
          >
            {t.label}
          </span>
        );
      })}
    </div>
  );
}
