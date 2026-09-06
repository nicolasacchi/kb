import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { Icon } from "./icons";

// F5 — accessible off-canvas drawer for the mobile shell (≤860px), ported
// from kb's own `web/src/components/chrome/MobileDrawer.tsx` (root
// CLAUDE.md invariant #23), kbc-prefixed. Powers the reader's file-tree
// overlay (`side="left"`, `Reader.tsx`) — the ONLY caller today, but the
// `side` prop is kept for parity with the ported original rather than
// trimmed to a single-use shape. Renders nothing until opened, animates in
// and out, then unmounts its children on close so the tree's own data
// fetches don't keep running in the background. On open it moves focus into
// the panel and traps Tab; Esc and a backdrop tap close it; body scroll is
// locked while it's mounted.
export default function MobileDrawer({
  open,
  onClose,
  side = "left",
  title,
  ariaLabel,
  children,
}: {
  open: boolean;
  onClose: () => void;
  side?: "left" | "bottom";
  title?: string;
  ariaLabel?: string;
  children: ReactNode;
}) {
  // `render` keeps the node mounted through the close transition; `shown`
  // drives the `.is-open` class that triggers the slide.
  const [render, setRender] = useState(open);
  const [shown, setShown] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (open) {
      setRender(true);
      // Next frame so the off-screen transform paints before `.is-open`
      // flips — otherwise the browser skips the transition.
      const raf = requestAnimationFrame(() => setShown(true));
      return () => cancelAnimationFrame(raf);
    }
    setShown(false);
    const t = setTimeout(() => setRender(false), 240);
    return () => clearTimeout(t);
  }, [open]);

  // Esc-to-close + body scroll-lock while mounted.
  useEffect(() => {
    if (!render) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey);
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = prevOverflow;
    };
  }, [render, onClose]);

  // Move focus into the panel once it has slid in.
  useEffect(() => {
    if (shown) panelRef.current?.focus();
  }, [shown]);

  if (!render) return null;

  // Basic focus trap: keep Tab cycling within the panel.
  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Tab") return;
    const focusables = panelRef.current?.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), input, [tabindex]:not([tabindex="-1"])',
    );
    if (!focusables || focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <div className={`kbc-drawer-root kbc-drawer-root--${side}`}>
      <div
        className={`kbc-drawer-scrim ${shown ? "is-open" : ""}`}
        onClick={onClose}
        aria-hidden="true"
        data-kbc-drawer-scrim
      />
      <div
        ref={panelRef}
        className={`kbc-drawer kbc-drawer--${side} ${shown ? "is-open" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel ?? title ?? "menu"}
        tabIndex={-1}
        onKeyDown={onKeyDown}
        data-kbc-drawer
      >
        <div className="kbc-drawer__head">
          {title && <span className="kbc-drawer__title">{title}</span>}
          <button
            type="button"
            className="kbc-drawer__close"
            onClick={onClose}
            aria-label="close menu"
            data-kbc-drawer-close
          >
            <Icon.X />
          </button>
        </div>
        <div className="kbc-drawer__body">{children}</div>
      </div>
    </div>
  );
}
