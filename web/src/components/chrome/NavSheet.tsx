import { useEffect, useState, type ReactNode } from "react";
import { Sheet, type Detent } from "@kb/ui/Sheet";

// OL-L1 — the mobile (<=860px) navigation + filters, on the shared
// `@kb/ui/Sheet` (scrim, drag handle, Esc, focus trap and body-scroll lock come
// from it). app.tsx mounts it as a sibling of the Spine wrapper, never inside
// it, so the fixed scrim is not trapped in the bar's stacking context. Every
// open starts at the peek detent. `MobileDrawer` stays for the bottom-side
// sheets that still use it (view options, search filters).
export default function NavSheet({
  open,
  onClose,
  children,
}: {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
}) {
  const [detent, setDetent] = useState<Detent>("peek");
  useEffect(() => {
    if (open) setDetent("peek");
  }, [open]);
  return (
    <Sheet
      open={open}
      detent={detent}
      onDetentChange={setDetent}
      onClose={onClose}
      title="kb"
      id="kb-navsheet"
      peek="70vh"
      full="92vh"
    >
      {children}
    </Sheet>
  );
}
