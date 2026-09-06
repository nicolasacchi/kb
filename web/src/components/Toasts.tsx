import { dismissToast, useToasts } from "../lib/toast";
import { Icon } from "./icons";

// F1 — the single global feedback viewport, mounted once in App. role=status +
// aria-live=polite so a screen reader announces failures/successes. Pure
// presentation; the store (lib/toast) owns state + TTL.
export default function Toasts() {
  const toasts = useToasts();
  if (toasts.length === 0) return null;
  return (
    <div className="kb-toasts" role="status" aria-live="polite">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`kb-toast kb-toast--${t.kind}`}
          data-kb-toast={t.kind}
        >
          <span className="kb-toast__msg">{t.msg}</span>
          {t.action && (
            <button
              type="button"
              className="kb-toast__action"
              onClick={() => {
                t.action?.onClick();
                dismissToast(t.id);
              }}
            >
              {t.action.label}
            </button>
          )}
          <button
            type="button"
            className="kb-toast__x"
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
