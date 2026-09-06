// W7/LF-2 — LiveTailPanel portal coordination: on mobile (<=860px),
// the panel renders INSIDE the PreviewInspector sheet (via Portal);
// on desktop, it renders inline in the reader column. Single useEffect
// in PreviewInspector registers a mobile portal target; ArtifactPane
// portals to it when available, or renders inline otherwise.

import { createContext, useContext, useMemo, useState } from "react";

interface LiveTailPortalContextType {
  mobilePortalNode: HTMLDivElement | null;
  setMobilePortalNode: (node: HTMLDivElement | null) => void;
}

const LiveTailPortalContext = createContext<LiveTailPortalContextType | undefined>(undefined);

export function LiveTailPortalProvider({ children }: { children: React.ReactNode }) {
  const [mobilePortalNode, setMobilePortalNode] = useState<HTMLDivElement | null>(null);
  // Stable identity when only the node changes — matches StatusBarProvider.
  const value = useMemo(
    () => ({ mobilePortalNode, setMobilePortalNode }),
    [mobilePortalNode],
  );

  return (
    <LiveTailPortalContext.Provider value={value}>
      {children}
    </LiveTailPortalContext.Provider>
  );
}

export function useLiveTailPortal() {
  const ctx = useContext(LiveTailPortalContext);
  if (!ctx) {
    throw new Error("useLiveTailPortal must be used within LiveTailPortalProvider");
  }
  return ctx;
}
