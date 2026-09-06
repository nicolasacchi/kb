// The confirmed-checkout dialog's state machine (W4.7 UI): confirm → submit
// → {success | dirty-refusal | error}. Pure reducer — `CheckoutDialog.tsx`
// is the sole caller, dispatching on the `useConfirm`-style promise-based
// host's own open/close plus `postCheckout`'s result.

export type CheckoutFlowState =
  | { phase: "idle" }
  | { phase: "confirming"; repo: string; target: string }
  | { phase: "submitting"; repo: string; target: string }
  | { phase: "dirty"; repo: string; target: string; dirtyPaths: string[] }
  | { phase: "error"; repo: string; target: string; message: string };

export type CheckoutFlowAction =
  | { type: "open"; repo: string; target: string }
  | { type: "cancel" }
  | { type: "submit" }
  | { type: "succeeded" }
  | { type: "dirty"; dirtyPaths: string[] }
  | { type: "failed"; message: string };

export const initialCheckoutFlowState: CheckoutFlowState = { phase: "idle" };

/// `submit` only fires from a state that still carries `repo`/`target`
/// (`confirming`, or a retry from `dirty`/`error`); from `idle` (nothing to
/// submit) or an in-flight `submitting` (already submitted) it's a no-op —
/// same "ignore an impossible transition rather than throw" discipline the
/// rest of this codebase's reducers use (`ladderState.ts`). `dirty`/`failed`
/// only apply while `submitting` — a stale response for a flow the user
/// already cancelled back to `idle` must not resurrect it.
export function checkoutFlowReducer(
  state: CheckoutFlowState,
  action: CheckoutFlowAction,
): CheckoutFlowState {
  switch (action.type) {
    case "open":
      return { phase: "confirming", repo: action.repo, target: action.target };
    case "cancel":
      return state.phase === "idle" ? state : { phase: "idle" };
    case "submit":
      if (state.phase === "idle" || state.phase === "submitting") return state;
      return { phase: "submitting", repo: state.repo, target: state.target };
    case "succeeded":
      return state.phase === "idle" ? state : { phase: "idle" };
    case "dirty":
      if (state.phase !== "submitting") return state;
      return { phase: "dirty", repo: state.repo, target: state.target, dirtyPaths: action.dirtyPaths };
    case "failed":
      if (state.phase !== "submitting") return state;
      return { phase: "error", repo: state.repo, target: state.target, message: action.message };
    default:
      return state;
  }
}
