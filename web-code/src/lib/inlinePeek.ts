// V70-A6 — inline peek: the pure state (§P7's "inline expansion", first
// slice).
//
// `gd` on a SINGLE candidate used to do one of two things: navigate away
// (losing the call site), or open a bottom-docked list (a second surface with
// its own scroll, its own focus and its own dismissal). Neither lets you read
// a call chain in one column, which is the thing the design asks for: the
// destination's source appears UNDER the line you are on, three deep, with a
// breadcrumb, and `Esc` puts you back exactly where you were.
//
// The nesting cap is 3 and it is a design decision, not a limit of the
// implementation: Code Bubbles' measured failure was that an unbounded
// surface costs more in arranging than it returns (CodeRibbon's
// replication), and Patchworks beat it by BOUNDING the space. Three frames
// is a call chain; ten is a canvas, and kb-code already has one of those.
//
// This module is DOM-free and CM6-free so the whole lifecycle — open, nest,
// pop, the context dial, the breadcrumb — is unit-testable; `editor/
// inlinePeek.ts` is the thin widget that renders it.

export const INLINE_PEEK_MAX_DEPTH = 3;
/// The context dial's floor, ceiling and step, in TOTAL lines shown.
export const CONTEXT_MIN = 12;
export const CONTEXT_MAX = 30;
export const CONTEXT_STEP = 6;
export const CONTEXT_DEFAULT = 12;

export interface InlinePeekFrame {
  repo: string;
  path: string;
  /// The 1-based line in `path` this frame is about — the excerpt is
  /// centred on it and it is the line that highlights.
  line: number;
  /// What the frame is called in the breadcrumb (a symbol name, else
  /// `path:line`).
  title: string;
  /// Total lines to render. Clamped to `[CONTEXT_MIN, CONTEXT_MAX]`.
  context: number;
  /// The trust class of the edge that opened this frame, when the caller had
  /// one. Rendered as a badge; never inferred here.
  trust?: string;
  /// Fetched source of `path` (whole file — the excerpt is a window over
  /// it), plus the server highlight spans for it. Absent while loading.
  content?: string;
  loading: boolean;
  error?: string;
}

export interface InlinePeekState {
  /// The 1-based line in the HOST document the widget hangs under. Fixed for
  /// the whole stack: nesting deepens the card, it never moves the anchor.
  hostLine: number;
  /// Newest last. Empty ⇒ closed.
  frames: InlinePeekFrame[];
  /// The host buffer's scroll offset at the moment the stack opened, so
  /// closing can restore it exactly (§P7).
  savedScrollTop: number;
}

export const closedInlinePeek: InlinePeekState = { hostLine: 0, frames: [], savedScrollTop: 0 };

export function isOpen(s: InlinePeekState): boolean {
  return s.frames.length > 0;
}

export function clampContext(n: number): number {
  if (!Number.isFinite(n)) return CONTEXT_DEFAULT;
  return Math.min(CONTEXT_MAX, Math.max(CONTEXT_MIN, Math.round(n)));
}

export type InlinePeekAction =
  | {
      type: "OPEN";
      hostLine: number;
      scrollTop: number;
      frame: Omit<InlinePeekFrame, "context" | "loading"> & { context?: number };
    }
  /// Nest one deeper. A PUSH at max depth REPLACES the deepest frame rather
  /// than growing the stack — the alternative (refusing the key) would make
  /// `gd` silently dead at depth 3, which reads as broken.
  | { type: "PUSH"; frame: Omit<InlinePeekFrame, "context" | "loading"> & { context?: number } }
  | { type: "POP" }
  | { type: "CLOSE" }
  | { type: "SET_CONTENT"; path: string; content: string }
  | { type: "SET_ERROR"; path: string; message: string }
  | { type: "CONTEXT"; delta: number };

function frameFrom(
  input: Omit<InlinePeekFrame, "context" | "loading"> & { context?: number },
): InlinePeekFrame {
  return { ...input, context: clampContext(input.context ?? CONTEXT_DEFAULT), loading: !input.content };
}

export function inlinePeekReducer(state: InlinePeekState, action: InlinePeekAction): InlinePeekState {
  switch (action.type) {
    case "OPEN":
      return {
        hostLine: action.hostLine,
        savedScrollTop: action.scrollTop,
        frames: [frameFrom(action.frame)],
      };
    case "PUSH": {
      if (state.frames.length === 0) return state;
      const next = frameFrom(action.frame);
      const frames =
        state.frames.length >= INLINE_PEEK_MAX_DEPTH
          ? [...state.frames.slice(0, INLINE_PEEK_MAX_DEPTH - 1), next]
          : [...state.frames, next];
      return { ...state, frames };
    }
    case "POP":
      // Popping the last frame closes; the caller then restores the scroll.
      return state.frames.length <= 1 ? { ...state, frames: [] } : { ...state, frames: state.frames.slice(0, -1) };
    case "CLOSE":
      return { ...state, frames: [] };
    case "SET_CONTENT":
      return {
        ...state,
        frames: state.frames.map((f) =>
          f.path === action.path ? { ...f, content: action.content, loading: false, error: undefined } : f,
        ),
      };
    case "SET_ERROR":
      return {
        ...state,
        frames: state.frames.map((f) =>
          f.path === action.path ? { ...f, loading: false, error: action.message } : f,
        ),
      };
    case "CONTEXT": {
      if (state.frames.length === 0) return state;
      const i = state.frames.length - 1;
      const cur = state.frames[i];
      const next = clampContext(cur.context + action.delta * CONTEXT_STEP);
      if (next === cur.context) return state;
      return { ...state, frames: state.frames.map((f, n) => (n === i ? { ...f, context: next } : f)) };
    }
  }
}

/// The line window an excerpt renders — `context` lines centred on `line`,
/// clamped to the file. `totalLines` of 0 (content not yet loaded) yields an
/// empty window rather than a negative range.
export function excerptWindow(
  line: number,
  context: number,
  totalLines: number,
): { start: number; end: number } {
  if (totalLines <= 0) return { start: 0, end: -1 };
  const ctx = clampContext(context);
  // Bias UP: the definition's own line and its signature matter more than
  // what follows, so the window starts one third above the target.
  const above = Math.floor(ctx / 3);
  let start = Math.max(1, line - above);
  let end = Math.min(totalLines, start + ctx - 1);
  // Re-anchor when the clamp at the bottom left the window short.
  start = Math.max(1, Math.min(start, end - ctx + 1));
  return { start, end };
}

/// The breadcrumb across the open stack, outermost first.
export function breadcrumb(state: InlinePeekState): string[] {
  return state.frames.map((f) => f.title);
}

export function topFrame(state: InlinePeekState): InlinePeekFrame | null {
  return state.frames[state.frames.length - 1] ?? null;
}
