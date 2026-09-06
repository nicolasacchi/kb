import { useEffect, useRef, useState, type ReactNode } from "react";

// Type-the-name confirmation modal for destructive admin actions.
// Mirrors GitHub's repo-deletion pattern: the user must type a specific
// token (the kb name, `DRAIN`, etc.) before the action button enables.
// Native <dialog>.showModal() for the focus trap + ::backdrop + Escape
// behaviour, same pattern as CommentModal.
export default function ConfirmModal({
  title,
  body,
  expectedToken,
  confirmLabel,
  danger = true,
  busy = false,
  onConfirm,
  onClose,
}: {
  title: string;
  body: ReactNode;
  /// User must type this exact string to enable the confirm button. Omit for
  /// a plain yes/no prompt (no token field; confirm enabled immediately).
  expectedToken?: string;
  confirmLabel: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDialogElement | null>(null);
  const [typed, setTyped] = useState("");

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

  const matches = expectedToken === undefined ? true : typed === expectedToken;

  return (
    <dialog
      ref={ref}
      className={`confirm ${danger ? "confirm--danger" : ""}`}
      aria-labelledby="confirm-title"
    >
      <h2 id="confirm-title" className="confirm__title">
        {title}
      </h2>
      <div className="confirm__body">{body}</div>
      {expectedToken !== undefined && (
        <label className="confirm__field">
          <span className="confirm__hint">
            Type <code>{expectedToken}</code> to confirm:
          </span>
          <input
            type="text"
            autoFocus
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            aria-label="confirmation token"
            className="confirm__input"
            // Hard to autocomplete a per-action token — disable so the
            // browser doesn't pre-fill with a previous value.
            autoComplete="off"
            spellCheck={false}
          />
        </label>
      )}
      <div className="confirm__actions">
        <button type="button" className="confirm__cancel" onClick={onClose} disabled={busy}>
          Cancel
        </button>
        <button
          type="button"
          className={`confirm__go ${danger ? "is-danger" : ""}`}
          disabled={!matches || busy}
          onClick={onConfirm}
        >
          {busy ? "Working…" : confirmLabel}
        </button>
      </div>
    </dialog>
  );
}
