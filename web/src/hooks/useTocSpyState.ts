import { useCallback, useEffect, useState } from "react";

// TocSpy rail state — binary toggle (expanded ↔ shrunk), persisted
// globally so the choice survives reloads and crosses artifacts.
// Sibling to `useInspectorCollapsed`; kept separate because the
// HotkeyRoot's `g t` chord drives it via a custom event the
// inspector hook doesn't know about.
//
// Persisted under `kb:toc-spy.state`. Default is "expanded".
// Multi-tab sync via the native `storage` event; same-tab sync via a
// custom event that `advance()` and `setState()` both broadcast — the
// HotkeyRoot's `g t` chord also emits the advance event.

export type TocSpyState = "expanded" | "shrunk";

const KEY = "kb:toc-spy.state";
const EVENT = "kb:toc-spy.state-changed";
const ADVANCE_EVENT = "kb:toc-spy.advance";

function read(): TocSpyState {
  if (typeof localStorage === "undefined") return "expanded";
  try {
    const v = localStorage.getItem(KEY);
    // Legacy "hidden" (tri-state era) collapses to "shrunk" — both
    // were the "out of the way" half of the cycle.
    if (v === "shrunk" || v === "hidden") return "shrunk";
    return "expanded";
  } catch {
    return "expanded";
  }
}

function write(value: TocSpyState) {
  try {
    localStorage.setItem(KEY, value);
    window.dispatchEvent(new CustomEvent(EVENT));
  } catch {
    // localStorage denied; this turn's state still updates, refresh
    // would lose it.
  }
}

function next(state: TocSpyState): TocSpyState {
  return state === "expanded" ? "shrunk" : "expanded";
}

export function useTocSpyState(): {
  state: TocSpyState;
  advance: () => void;
  setState: (next: TocSpyState) => void;
} {
  const [state, setLocal] = useState<TocSpyState>(() => read());

  useEffect(() => {
    const onChange = () => setLocal(read());
    const onAdvance = () => {
      setLocal((prev) => {
        const n = next(prev);
        write(n);
        return n;
      });
    };
    window.addEventListener("storage", onChange);
    window.addEventListener(EVENT, onChange);
    window.addEventListener(ADVANCE_EVENT, onAdvance);
    return () => {
      window.removeEventListener("storage", onChange);
      window.removeEventListener(EVENT, onChange);
      window.removeEventListener(ADVANCE_EVENT, onAdvance);
    };
  }, []);

  const setState = useCallback((value: TocSpyState) => {
    setLocal(value);
    write(value);
  }, []);

  const advance = useCallback(() => {
    setLocal((prev) => {
      const n = next(prev);
      write(n);
      return n;
    });
  }, []);

  return { state, advance, setState };
}
