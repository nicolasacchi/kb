import { useEffect, useRef, useState, type ReactNode } from "react";

// FS2 — one collapsible group in the search filter rail. Native
// <details>/<summary> gives keyboard toggle + screen-reader expanded/
// collapsed state for free. Open/closed persists per section in
// localStorage (key `kb:search-rail.<id>`), and a section that holds an
// active filter (`count > 0`) force-opens so a deep-linked filter is never
// hidden behind a collapsed header.

const KEY = (id: string) => `kb:search-rail.${id}`;

function readOpen(id: string, fallback: boolean): boolean {
  if (typeof localStorage === "undefined") return fallback;
  try {
    const v = localStorage.getItem(KEY(id));
    return v === null ? fallback : v === "true";
  } catch {
    return fallback;
  }
}

function writeOpen(id: string, open: boolean) {
  try {
    localStorage.setItem(KEY(id), open ? "true" : "false");
  } catch {
    /* storage denied — state still holds for this session */
  }
}

export type FilterSectionProps = {
  /** Stable id for the persistence key. */
  id: string;
  title: string;
  /** Active-filter count → a badge on the header; also force-opens. */
  count?: number;
  defaultOpen?: boolean;
  /** Static children, or a render prop that receives expanded state
   *  (so expensive pickers can gate fetches with `enabled={open}`). */
  children: ReactNode | ((open: boolean) => ReactNode);
};

export default function FilterSection({
  id,
  title,
  count = 0,
  defaultOpen = false,
  children,
}: FilterSectionProps) {
  const [open, setOpen] = useState<boolean>(
    () => readOpen(id, defaultOpen) || count > 0,
  );
  // If a filter becomes active after mount while this section is closed,
  // reveal it once (don't fight the user if they re-close it afterwards).
  const forced = useRef(false);
  useEffect(() => {
    if (count > 0 && !open && !forced.current) {
      forced.current = true;
      setOpen(true);
    }
  }, [count, open]);

  const body = typeof children === "function" ? children(open) : children;

  return (
    <details
      className="kb-search-rail__group"
      open={open}
      onToggle={(e) => {
        const next = (e.currentTarget as HTMLDetailsElement).open;
        if (next === open) return;
        setOpen(next);
        writeOpen(id, next);
      }}
    >
      <summary className="kb-search-rail__grouph">
        <span>{title}</span>
        {count > 0 && (
          <span className="kb-search-rail__badge" aria-label={`${count} active`}>
            {count}
          </span>
        )}
      </summary>
      <div className="kb-search-rail__groupbody">{body}</div>
    </details>
  );
}
