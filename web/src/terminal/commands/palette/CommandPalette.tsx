// CMDPALETTE: the desktop palette dialog: a native <dialog> opened with
// showModal() (focus trap, ::backdrop and Esc come from the platform, the
// CaptureSheet pattern) around the shared `CommandList`.

import { useEffect, useId, useRef, type ReactElement } from "react";
import { CommandList } from "./CommandList";

export interface CommandPaletteProps {
  termId: string;
  harness: string;
  /** Dismissed without sending. */
  onDismiss: () => void;
  /** A send reached the pane: close, and the owner moves focus to the pane. */
  onSent: () => void;
}

export function CommandPalette({ termId, harness, onDismiss, onSent }: CommandPaletteProps): ReactElement {
  const ref = useRef<HTMLDialogElement | null>(null);
  const titleId = useId();

  useEffect(() => {
    const dlg = ref.current;
    if (dlg && !dlg.open) dlg.showModal();
  }, []);

  useEffect(() => {
    const dlg = ref.current;
    if (!dlg) return;
    const onCancel = (e: Event): void => {
      e.preventDefault();
      onDismiss();
    };
    dlg.addEventListener("cancel", onCancel);
    return () => dlg.removeEventListener("cancel", onCancel);
  }, [onDismiss]);

  return (
    <dialog
      ref={ref}
      className="kb-cmdp-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      data-testid="kb-cmdp-dialog"
    >
      <div className="kb-cmdp-dialog__head">
        <h2 id={titleId} className="kb-cmdp-dialog__title">
          Commands
        </h2>
        <button type="button" className="kb-cmdp__btn" aria-label="Close commands" onClick={onDismiss}>
          Close
        </button>
      </div>
      <CommandList termId={termId} harness={harness} variant="dialog" autoFocus onSent={onSent} onNavigate={onDismiss} />
    </dialog>
  );
}
