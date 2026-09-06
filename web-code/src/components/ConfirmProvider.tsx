import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  type ReactNode,
} from "react";
import ConfirmModal from "./ConfirmModal";

export interface ConfirmOptions {
  title: string;
  body: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
}

type ConfirmFn = (opts: ConfirmOptions) => Promise<boolean>;

const ConfirmContext = createContext<ConfirmFn | null>(null);

/// Ported from kb's own `web/src/components/ConfirmProvider.tsx` (invariant
/// #32). `await confirm({ title, body })` resolves `true`/`false` — a
/// drop-in for `window.confirm` but themeable + focus-trapped. ONE host at
/// the app root (`app.tsx`) means there is never more than one destructive
/// prompt on screen; annotation delete (W4.6) and the plain "switch
/// branches?" step of the checkout flow (W4.7 — the DIRTY-refusal outcome
/// itself is its own richer `CheckoutDialog`, not this hook, since it needs
/// to show the path list past a first confirm) both go through this.
export function useConfirm(): ConfirmFn {
  const fn = useContext(ConfirmContext);
  if (!fn) throw new Error("useConfirm must be used within <ConfirmProvider>");
  return fn;
}

export default function ConfirmProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<ConfirmOptions | null>(null);
  const resolveRef = useRef<((v: boolean) => void) | null>(null);

  const confirm = useCallback<ConfirmFn>(
    (opts) =>
      new Promise<boolean>((resolve) => {
        resolveRef.current?.(false);
        resolveRef.current = resolve;
        setPending(opts);
      }),
    [],
  );

  const settle = useCallback((value: boolean) => {
    resolveRef.current?.(value);
    resolveRef.current = null;
    setPending(null);
  }, []);

  return (
    <ConfirmContext.Provider value={confirm}>
      {children}
      {pending && (
        <ConfirmModal
          title={pending.title}
          body={pending.body}
          confirmLabel={pending.confirmLabel ?? "Confirm"}
          danger={pending.danger ?? true}
          onConfirm={() => settle(true)}
          onClose={() => settle(false)}
        />
      )}
    </ConfirmContext.Provider>
  );
}
