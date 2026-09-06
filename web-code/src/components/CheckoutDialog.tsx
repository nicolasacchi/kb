import { useEffect, useReducer, useRef } from "react";
import { postCheckout } from "../api/client";
import { checkoutFlowReducer, type CheckoutFlowState } from "../lib/checkoutFlow";
import { toast } from "../lib/toast";
import "../styles/provenance.css";

export interface CheckoutDialogProps {
  repo: string;
  target: string;
  onClose: () => void;
}

/// W4.7 UI — "switch to this ref" behind a confirm. Modeled on kb's own
/// `useConfirm`/`ConfirmModal` pattern (invariant #32: promise-based dialog
/// host, `.confirm`/`.confirm__go` classes — see `ConfirmProvider.tsx`),
/// but NOT routed through that generic yes/no hook: a plain boolean isn't
/// expressive enough for "confirm → submit → maybe told the tree is dirty,
/// with the actual path list, in the SAME dialog" (`routes::checkout_route`'s
/// 409 body carries `dirty_paths`, see that route's doc), so this is its
/// own small stateful dialog (`lib/checkoutFlow.ts`'s reducer) reusing the
/// SAME `.confirm` CSS so it reads as part of the same dialog family.
/// Mounted only while open (the caller owns show/hide); success just
/// closes — the resulting `repo.head_moved` SSE frame drives the banner
/// (`useLiveMirror`), this dialog does nothing more once the daemon
/// confirms the switch.
export default function CheckoutDialog({ repo, target, onClose }: CheckoutDialogProps) {
  const [state, dispatch] = useReducer(checkoutFlowReducer, {
    phase: "confirming",
    repo,
    target,
  } as CheckoutFlowState);
  const dlgRef = useRef<HTMLDialogElement | null>(null);

  useEffect(() => {
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
  }, []);

  useEffect(() => {
    const dlg = dlgRef.current;
    if (!dlg) return;
    const onCancel = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    dlg.addEventListener("cancel", onCancel);
    return () => dlg.removeEventListener("cancel", onCancel);
  }, [onClose]);

  async function submit() {
    dispatch({ type: "submit" });
    const result = await postCheckout(repo, target);
    if (result.kind === "ok") {
      dispatch({ type: "succeeded" });
      onClose();
    } else if (result.kind === "dirty") {
      dispatch({ type: "dirty", dirtyPaths: result.data.dirty_paths });
    } else {
      dispatch({ type: "failed", message: result.error });
      // F3a — the dialog already shows `result.error` inline (the `errored`
      // branch below), but a toast too means the failure is still visible
      // even if the operator has already dismissed/looked away from the
      // dialog (e.g. mid-close-and-retry from elsewhere).
      toast.err(`checkout failed: ${result.error}`);
    }
  }

  const busy = state.phase === "submitting";
  const dirty = state.phase === "dirty";
  const errored = state.phase === "error";

  return (
    <dialog ref={dlgRef} className="confirm confirm--danger" aria-labelledby="kbc-checkout-title">
      <h2 id="kbc-checkout-title" className="confirm__title">
        Switch {repo} to {target}?
      </h2>
      <div className="confirm__body">
        {dirty ? (
          <div data-kbc-checkout-dirty>
            <p>
              The working tree is dirty — commit, stash, or discard these paths before switching.
            </p>
            <ul className="kbc-checkout__dirty-list">
              {state.dirtyPaths.map((p) => (
                <li key={p} data-kbc-dirty-path className="kbc-checkout__dirty-path">
                  {p}
                </li>
              ))}
            </ul>
          </div>
        ) : errored ? (
          <p className="kbc-checkout__error" data-kbc-checkout-error>
            {state.message}
          </p>
        ) : (
          <p>
            This switches the working tree to <code>{target}</code>. Uncommitted changes will
            refuse the switch rather than being lost.
          </p>
        )}
      </div>
      <div className="confirm__actions">
        <button type="button" className="confirm__cancel" onClick={onClose} disabled={busy}>
          {dirty || errored ? "Close" : "Cancel"}
        </button>
        {!dirty && !errored && (
          <button type="button" className="confirm__go is-danger" disabled={busy} onClick={submit}>
            {busy ? "Switching…" : "Switch"}
          </button>
        )}
      </div>
    </dialog>
  );
}
