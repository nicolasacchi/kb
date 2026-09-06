import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  type ReactNode,
} from "react";
import ConfirmModal from "./settings/ConfirmModal";

export type ConfirmOptions = {
  title: string;
  body: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
  /// GitHub-style guard: when set, the user must type this exact token before
  /// the confirm button enables. Omit for a plain yes/no prompt.
  expectedToken?: string;
};

type ConfirmFn = (opts: ConfirmOptions) => Promise<boolean>;

const ConfirmContext = createContext<ConfirmFn | null>(null);

// Imperative confirm dialog. `await confirm({ title, body })` resolves to
// `true` if the user confirms, `false` otherwise — so a call site stays a
// one-line guard (`if (!(await confirm(...))) return;`), a drop-in for
// `window.confirm` but themeable, focus-trapped, and aria-correct. One
// home for every destructive prompt: a single <ConfirmModal> host means
// there is never more than one prompt on screen.
export function useConfirm(): ConfirmFn {
  const fn = useContext(ConfirmContext);
  if (!fn) throw new Error("useConfirm must be used within <ConfirmProvider>");
  return fn;
}

export default function ConfirmProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<ConfirmOptions | null>(null);
  // The promise resolver for the in-flight prompt, kept out of state so
  // settling never depends on a stale closure.
  const resolveRef = useRef<((v: boolean) => void) | null>(null);

  const confirm = useCallback<ConfirmFn>(
    (opts) =>
      new Promise<boolean>((resolve) => {
        // A second confirm() before the first settles is degenerate; resolve
        // the orphan false so its caller doesn't hang.
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
          expectedToken={pending.expectedToken}
          onConfirm={() => settle(true)}
          onClose={() => settle(false)}
        />
      )}
    </ConfirmContext.Provider>
  );
}
