import type { ReactNode } from "react";
import MobileDrawer from "./MobileDrawer";

// D4 — the gallery's Sort/Group/Density row overflows the context line at
// ≤860px. Rather than fork a bespoke bottom-sheet implementation, this is a
// thin `MobileDrawer` (side="bottom") wrapper: the parent (ContextLine) is
// the only caller and passes its EXISTING SortControl/GroupControl/density
// toggle as children unchanged — no control logic is duplicated here, just
// laid out in a column instead of the ribbon's inline row. MobileDrawer
// already owns the open/close slide, scrim, Esc-to-close, and focus trap
// (the ✕ + scrim-tap + Esc dismiss trio other sheets in this app use).
export default function ViewOptionsSheet({
  open,
  onClose,
  children,
}: {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
}) {
  return (
    <MobileDrawer
      open={open}
      onClose={onClose}
      side="bottom"
      title="View options"
      ariaLabel="view options"
    >
      <div className="kb-viewopts-sheet">{children}</div>
    </MobileDrawer>
  );
}
