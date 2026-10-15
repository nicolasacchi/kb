import { useEffect, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

// OL-C11 — the top bar's `scope` slot is filled by the ROUTE, not by the bar.
// `TopBar` renders an empty `ScopeSlot` host; a route that owns a scope (the
// reader's path crumbs) renders its crumbs through `ScopePortal`, which
// portals them into that host. No context value, no URL state, nothing the
// Location Contract has to know about: the host is a DOM node looked up by id
// after mount, and the portal vanishes with the route.

export const SCOPE_SLOT_ID = "kbc-scope-slot";

export function ScopeSlot() {
  return <div id={SCOPE_SLOT_ID} className="kbc-scope-slot" data-kbc-scope-slot />;
}

export function ScopePortal({ children }: { children: ReactNode }) {
  const [host, setHost] = useState<HTMLElement | null>(null);
  useEffect(() => {
    setHost(document.getElementById(SCOPE_SLOT_ID));
  }, []);
  return host ? createPortal(children, host) : null;
}
