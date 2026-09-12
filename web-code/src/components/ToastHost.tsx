import { Link } from "react-router";
import { dismissToast, useToasts } from "../lib/toast";
import { Icon } from "./icons";

// F3a — the single global feedback viewport, mounted once in app.tsx.
// role=status + aria-live=polite so a screen reader announces failures/
// successes. Pure presentation; the store (lib/toast) owns state + TTL.
// Ported from kb's own `web/src/components/Toasts.tsx` (invariant #32).
//
// Phase E4 — a toast MAY carry an in-app `link` (e.g. "Added to reading
// set" → straight to that set); rendered as a plain `<Link>`, dismissing the
// toast on click (mirrors the ✕ button's own effect) so it doesn't linger
// over whatever page the navigation lands on.
export default function ToastHost() {
  const toasts = useToasts();
  if (toasts.length === 0) return null;
  return (
    <div className="kbc-toasts" role="status" aria-live="polite">
      {toasts.map((t) => (
        <div key={t.id} className={`kbc-toast kbc-toast--${t.kind}`} data-kbc-toast={t.kind}>
          <span className="kbc-toast__msg">{t.msg}</span>
          {t.link && (
            <Link className="kbc-toast__link" to={t.link.to} onClick={() => dismissToast(t.id)}>
              {t.link.label}
            </Link>
          )}
          <button
            type="button"
            className="kbc-toast__x"
            aria-label="dismiss"
            onClick={() => dismissToast(t.id)}
          >
            <Icon.X />
          </button>
        </div>
      ))}
    </div>
  );
}
