// FUTERMFIT — the terminal font stack and the webfont gate.
//
// The first `new Terminal` must not measure a fallback face: the panel waits
// for the webfont (bounded) before it opens the stream. A face that arrives
// after the bound is reported through `done`, so the panel can re-measure and
// refit. No font files are added: the stack names the app's own JetBrains Mono
// and ends in system symbol faces (box drawing, braille and dingbats the mono
// face lacks) before the generic family.

export const TERM_FONT_FAMILY =
  '"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", ' +
  '"Noto Sans Symbols 2", "Noto Sans Symbols", "Apple Symbols", "Segoe UI Symbol", monospace';

/** The CSS font shorthand the gate loads (size is irrelevant to which file loads). */
export const TERM_FONT_LOAD_SPEC = '14px "JetBrains Mono"';

export const FONT_WAIT_MS = 1500;

export interface FontGate {
  /** Resolves `true` when the face loaded within the bound, `false` when the bound won. */
  ready: Promise<boolean>;
  /** Resolves when the face has loaded, whenever that is (immediately when there is no FontFaceSet). */
  done: Promise<void>;
}

type FontsLike = { load(spec: string): Promise<unknown> } | undefined | null;

export function fontGate(
  fonts: FontsLike,
  timeoutMs: number = FONT_WAIT_MS,
  setTimer: (fn: () => void, ms: number) => unknown = (fn, ms) => setTimeout(fn, ms),
  clearTimer: (h: unknown) => void = (h) => clearTimeout(h as number),
): FontGate {
  if (!fonts) return { ready: Promise.resolve(true), done: Promise.resolve() };
  // A rejected load (an unreadable face) is "settled": the fallback face is what there is.
  const done = Promise.resolve()
    .then(() => fonts.load(TERM_FONT_LOAD_SPEC))
    .then(
      () => undefined,
      () => undefined,
    );
  const ready = new Promise<boolean>((resolve) => {
    const timer = setTimer(() => resolve(false), timeoutMs);
    void done.then(() => {
      clearTimer(timer);
      resolve(true);
    });
  });
  return { ready, done };
}
