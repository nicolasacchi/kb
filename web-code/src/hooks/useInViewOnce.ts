import { useCallback, useEffect, useRef, useState } from "react";

/// IntersectionObserver that flips `inView` true the first time the node
/// intersects, then disconnects. SSR / vitest (no `IntersectionObserver`)
/// fall back to `inView: true` so callers fetch eagerly rather than hang.
export function useInViewOnce(rootMargin = "200px 0px"): {
  ref: (node: Element | null) => void;
  inView: boolean;
} {
  const ioAvailable =
    typeof window !== "undefined" && typeof IntersectionObserver === "function";
  const [inView, setInView] = useState(!ioAvailable);
  const seen = useRef(!ioAvailable);
  const observerRef = useRef<IntersectionObserver | null>(null);

  const ref = useCallback(
    (node: Element | null) => {
      observerRef.current?.disconnect();
      observerRef.current = null;
      if (!node || seen.current || !ioAvailable) return;
      const obs = new IntersectionObserver(
        (entries) => {
          if (!entries.some((e) => e.isIntersecting)) return;
          seen.current = true;
          setInView(true);
          obs.disconnect();
          observerRef.current = null;
        },
        { rootMargin },
      );
      observerRef.current = obs;
      obs.observe(node);
    },
    [ioAvailable, rootMargin],
  );

  useEffect(() => () => observerRef.current?.disconnect(), []);

  return { ref, inView };
}
