// FUTERMPOLISH — keep the inline panel inside the window the reader can see.
//
// The inline view is sized to fit below the app bar (`terminal.css`), but it
// only fits if its bar sits just under the app bar. Opening the panel (Open
// terminal, a launch, Resume) and "Fit to window" scroll it there, so the grid a
// Fit measures is the grid on screen. Never called from a render or an event
// stream: only from an explicit open or click.

export interface ScrollOptions {
  /** No animation (Fit measures straight after the scroll). */
  instant?: boolean;
}

export function scrollPanelIntoView(el: HTMLElement | null, opts: ScrollOptions = {}): boolean {
  if (!el || typeof el.scrollIntoView !== "function") return false;
  const reduced =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  el.scrollIntoView({ block: "start", inline: "nearest", behavior: opts.instant || reduced ? "auto" : "smooth" });
  return true;
}
