import { useEffect, useState } from "react";
import { stickTopCss } from "../lib/stickyHead";

/// v0.47 SH — publish the review toolbar's measured height as
/// `--rdiff-stick-top` on the review root so the per-file headers pin directly
/// under it. Returns the callback ref to put on the root element. Re-measures
/// on toolbar resize (type ramp, density, wrapping) via ResizeObserver; with no
/// ResizeObserver (old test envs) it measures once.
export function useStickyOffset(): (el: HTMLElement | null) => void {
  const [root, setRoot] = useState<HTMLElement | null>(null);
  useEffect(() => {
    if (!root) return;
    const toolbar = root.querySelector<HTMLElement>("[data-kbc-rdiff-toolbar]");
    if (!toolbar) return;
    const measure = () => {
      const v = stickTopCss(toolbar.getBoundingClientRect().height);
      if (v) root.style.setProperty("--rdiff-stick-top", v);
      else root.style.removeProperty("--rdiff-stick-top");
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(measure);
    ro.observe(toolbar);
    return () => {
      ro.disconnect();
      root.style.removeProperty("--rdiff-stick-top");
    };
  }, [root]);
  return setRoot;
}
