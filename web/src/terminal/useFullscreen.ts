// FUTERMFULL — React glue for `createFullscreen`: one control per panel element
// and term id. The effect cleanup is the guarantee that leaving (close, unmount,
// a term id change, the desktop element going away on a breakpoint switch)
// exits native fullscreen, releases the keyboard lock and drops the overlay
// class, so a reload or a navigation never leaves a stuck state.

import { useCallback, useEffect, useRef, useState } from "react";
import { createFullscreen, MAX_CLASS, type FsControl, type FsDoc, type FsNav, type FsPanel } from "./fullscreen";

export interface UseFullscreen {
  /** Native fullscreen or the overlay (derived by the control). */
  on: boolean;
  /**
   * The overlay is on. The component renders the overlay class from this: the
   * control adds the class to the element, but a React `className` re-render
   * (focus chip, idle state) would otherwise overwrite it.
   */
  max: boolean;
  toggle: () => void;
  exit: () => void;
}

export function useFullscreen(
  el: HTMLElement | null,
  key: string,
  opts?: { overlay?: boolean; lockEscape?: boolean; onChange?: (on: boolean) => void },
): UseFullscreen {
  const [on, setOn] = useState(false);
  const [max, setMax] = useState(false);
  const ctl = useRef<FsControl | null>(null);
  const onChangeRef = useRef(opts?.onChange);
  onChangeRef.current = opts?.onChange;
  const overlay = opts?.overlay;
  const lockEscape = opts?.lockEscape;
  useEffect(() => {
    if (!el) return;
    const c = createFullscreen({
      panel: el as unknown as FsPanel,
      doc: document as unknown as FsDoc,
      nav: navigator as unknown as FsNav,
      overlay,
      lockEscape,
      onChange: (v) => {
        setOn(v);
        setMax(v && el.classList.contains(MAX_CLASS));
        onChangeRef.current?.(v);
      },
    });
    ctl.current = c;
    return () => {
      c.dispose();
      if (ctl.current === c) ctl.current = null;
      setOn(false);
      setMax(false);
    };
  }, [el, key, overlay, lockEscape]);
  const toggle = useCallback((): void => {
    const c = ctl.current;
    if (!c) return;
    void (c.active() ? c.exit() : c.enter());
  }, []);
  const exit = useCallback((): void => {
    void ctl.current?.exit();
  }, []);
  return { on, max, toggle, exit };
}
