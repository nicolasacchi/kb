import { useEffect, useState } from "react";

// Phase C7's story player deliverable 5: respect `prefers-reduced-motion` —
// no flash transition (handled purely in CSS, `styles/story.css`), and no
// default-on autoplay (the autoplay control itself is hidden under reduced
// motion, since autoplay IS the motion this preference asks to avoid).
// Reactive `matchMedia` wrapper, same shape as `useIsMobile.ts`.
export function usePrefersReducedMotion(): boolean {
  const query = "(prefers-reduced-motion: reduce)";
  const [reduced, setReduced] = useState<boolean>(() =>
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia(query).matches
      : false,
  );

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia(query);
    const onChange = () => setReduced(mql.matches);
    onChange();
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, [query]);

  return reduced;
}
