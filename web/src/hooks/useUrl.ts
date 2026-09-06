import { useCallback } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";

// useUrl — keeps URL search params in sync with view state. Reading is a
// thin wrapper over useSearchParams; writing preserves other params and
// uses replace: true so bf/fwd nav stays intuitive.
export function useUrl() {
  const [params, setParams] = useSearchParams();
  const navigate = useNavigate();

  // Apply one or more param mutations in a SINGLE navigation. Two
  // separate `setParams` calls in the same event handler don't compose:
  // react-router issues a `navigate` per call and the functional
  // updater's `prev` is read from a ref that only refreshes on
  // re-render, so the second call clobbers the first. `setMany` folds
  // every change into one URLSearchParams + one navigate, which is the
  // only safe way to update e.g. `sort` and `dir` together.
  const setMany = useCallback(
    (updates: Record<string, string | null>) => {
      const next = new URLSearchParams(params);
      for (const [key, value] of Object.entries(updates)) {
        if (value === null || value === "") next.delete(key);
        else next.set(key, value);
      }
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  // Single-key convenience wrapper. Safe on its own; never call it
  // twice in one handler for related keys — use `setMany` instead.
  const set = useCallback(
    (key: string, value: string | null) => setMany({ [key]: value }),
    [setMany],
  );

  return { params, set, setMany, navigate };
}
