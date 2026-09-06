import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { Icon } from "../icons";
import { useBodyScrollLock } from "../../hooks/useBodyScrollLock";

// Accessible off-canvas drawer for the mobile shell (≤860px). Powers the
// hamburger nav+filters panel; the `bottom` variant is reused by the
// detail-view sheets (MB3). Renders nothing until opened, animates in and
// out, then unmounts its children on close so panels don't keep fetching in
// the background. On open it moves focus into the panel and traps Tab; Esc
// and a backdrop tap close it; body scroll is locked while it's mounted.
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

  // SH.B2 — body scroll-lock while mounted, now the shared hook (was an
  // inline `document.body.style.overflow` toggle here; see the hook's own
  // doc for why a shared counter beats each caller guessing the "real"
  // prior value).
  useBodyScrollLock(render);

  // Esc-to-close while mounted.
  useEffect(() => {
    if (!render) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
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
    <div className={`kb-drawer-root kb-drawer-root--${side}`}>
      <div
        className={`kb-drawer-scrim ${shown ? "is-open" : ""}`}
        onClick={onClose}
        aria-hidden="true"
      />
      <div
        ref={panelRef}
        className={`kb-drawer kb-drawer--${side} ${shown ? "is-open" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel ?? title ?? "menu"}
        tabIndex={-1}
        onKeyDown={onKeyDown}
      >
        <div className="kb-drawer__head">
          {title && <span className="kb-drawer__title">{title}</span>}
          <button
            type="button"
            className="kb-drawer__close"
            onClick={onClose}
            aria-label="close menu"
          >
            <Icon.X />
          </button>
        </div>
        <div className="kb-drawer__body">{children}</div>
      </div>
    </div>
  );
}
