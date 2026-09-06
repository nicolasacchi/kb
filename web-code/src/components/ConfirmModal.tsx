import { useEffect, useRef, type ReactNode } from "react";

// Ported from kb's own `web/src/components/settings/ConfirmModal.tsx`
// (invariant #32 in kb's root CLAUDE.md: "promise-based confirm host,
// `.confirm__go` class") — trimmed of the type-the-name `expectedToken`
// guard (kb-code has no admin-drain-shaped destructive action yet; a plain
// yes/no is enough for "delete this annotation" / "switch branches"). Same
// native `<dialog>.showModal()` focus-trap + `::backdrop` + Escape
// behaviour, same `.confirm`/`.confirm__go` class names so both the visual
// language AND any e2e spec written against kb's own convention transfer
// unchanged.
export default function ConfirmModal({
  title,
  body,
  confirmLabel,
  danger = true,
  busy = false,
  onConfirm,
  onClose,
}: {
  title: string;
  body: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDialogElement | null>(null);

  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    const dlg = ref.current;
    if (dlg && !dlg.open) dlg.showModal();
    return () => trigger?.focus?.();
  }, []);

  useEffect(() => {
    const dlg = ref.current;
    if (!dlg) return;
    const onCancel = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    dlg.addEventListener("cancel", onCancel);
    return () => dlg.removeEventListener("cancel", onCancel);
  }, [onClose]);

  return (
    <dialog
      ref={ref}
      className={`confirm ${danger ? "confirm--danger" : ""}`}
      aria-labelledby="kbc-confirm-title"
    >
      <h2 id="kbc-confirm-title" className="confirm__title">
        {title}
      </h2>
      <div className="confirm__body">{body}</div>
      <div className="confirm__actions">
        <button type="button" className="confirm__cancel" onClick={onClose} disabled={busy}>
          Cancel
        </button>
        <button
          type="button"
          className={`confirm__go ${danger ? "is-danger" : ""}`}
          disabled={busy}
          onClick={onConfirm}
        >
          {busy ? "Working…" : confirmLabel}
        </button>
      </div>
    </dialog>
  );
}
