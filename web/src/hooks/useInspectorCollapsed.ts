import { useCallback, useEffect, useState } from "react";

// v0.13.2 — shared collapse state for the design's right-rail
// inspectors (PreviewInspector on Detail, AtlasInspector on Atlas).
// One localStorage key per surface so the choice survives reloads
// without leaking across surfaces.
//
// State key: `kb:inspector-collapsed.<surface>` ("true" or "false").
// Default is false (expanded) — first-time visitors see the rail.

function read(key: string): boolean {
  if (typeof localStorage === "undefined") return false;
  try {
    return localStorage.getItem(`kb:inspector-collapsed.${key}`) === "true";
  } catch {
    return false;
  }
}

function write(key: string, value: boolean) {
  try {
    localStorage.setItem(`kb:inspector-collapsed.${key}`, value ? "true" : "false");
    // Same-tab subscribers get a custom event (`storage` events only
    // fire cross-tab).
    window.dispatchEvent(
      new CustomEvent(`kb:inspector-collapsed.${key}`),
    );
  } catch {
    // localStorage might be denied; state still updates this turn,
    // refresh would lose it.
  }
}

export function useInspectorCollapsed(surface: string): {
  collapsed: boolean;
  toggle: () => void;
  setCollapsed: (next: boolean) => void;
} {
  const [collapsed, setLocal] = useState<boolean>(() => read(surface));

  useEffect(() => {
    const evt = `kb:inspector-collapsed.${surface}`;
    const onChange = () => setLocal(read(surface));
    window.addEventListener("storage", onChange);
    window.addEventListener(evt, onChange);
    return () => {
      window.removeEventListener("storage", onChange);
      window.removeEventListener(evt, onChange);
    };
  }, [surface]);

  const setCollapsed = useCallback(
    (next: boolean) => {
      setLocal(next);
      write(surface, next);
    },
    [surface],
  );

  const toggle = useCallback(() => {
    setLocal((prev) => {
      const next = !prev;
      write(surface, next);
      return next;
    });
  }, [surface]);

  return { collapsed, toggle, setCollapsed };
}
