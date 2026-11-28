// The landscape-phone media query as React state (the JS twin of drive.css's POLISH2 fold).
// Shared by the dock (quick replies join the key row) and the command sheet (search behind a toggle).

import { useEffect, useState } from "react";
import { LANDSCAPE_PHONE_QUERY } from "./queries";

function mq(): MediaQueryList | null {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" ? window.matchMedia(LANDSCAPE_PHONE_QUERY) : null;
}

export function useLandscapePhone(): boolean {
  const [on, setOn] = useState<boolean>(() => mq()?.matches ?? false);
  useEffect(() => {
    const m = mq();
    if (!m) return;
    const f = (): void => setOn(m.matches);
    f();
    m.addEventListener("change", f);
    return () => m.removeEventListener("change", f);
  }, []);
  return on;
}
