// A vim-style READING keymap for the read-only CM6 viewer (`CodeView.tsx`).
//
// This is the CM6-facing half of the pair with `vimKeys.ts`: ALL
// sequence/count/mode logic lives there (pure, DOM-free, unit-tested); this
// module does nothing but (a) turn a guarded keydown into a `KeyInput`, feed
// it to `vimKeysReducer`, and (b) map the resulting `VimCommand`s onto CM6
// calls — selection/scroll/clipboard/callback side effects, NEVER a document
// change (the reader has no editor, ever — see the milestone's design
// ruling, mirrored in `highlightField.ts`'s doc). Because this module only
// exists in a browser (`EditorView` needs a real DOM), and the SPA's
// `vitest.config.ts` runs `environment: "node"`, it has no colocated
// `vimReader.test.ts` — the sequence/mode/count logic it depends on is
// fully covered by `vimKeys.test.ts` instead; this file is exercised
// end-to-end once wired into `CodeView` (a later phase).
//
// Marks (`m{a-z}` / `'{a-z}`) live in a `StateField` scoped to ONE
// `EditorView` instance — `CodeView` recreates the whole `EditorView` per
// file/ref load (see that component's doc), so marks are implicitly scoped
// to "while viewing this file" and reset on navigating away. That matches
// the milestone brief's "StateField, per-editor-instance" wording; making
// marks survive a file switch would need a different owner (a React ref
// outside the view) and is left to whichever later phase wants it.

import {
  EditorSelection,
  EditorState,
  Prec,
  StateEffect,
  StateField,
  type Extension,
  type Line,
  type SelectionRange,
  type Text,
} from "@codemirror/state";
import { drawSelection, EditorView, keymap } from "@codemirror/view";
import {
  findNext,
  findPrevious,
  gotoLine as gotoLinePanel,
  openSearchPanel,
  search,
  searchKeymap,
  SearchQuery,
  setSearchQuery,
} from "@codemirror/search";
import {
  initialVimKeyState,
  vimKeysReducer,
  wordAt,
  type VimCommand,
  type VimKeyState,
  type VimMode,
} from "./vimKeys";

export interface LineSel {
  line: number;
  lineEnd?: number;
}

export interface WordPos {
  line: number;
  /// 0-based UTF-16 column offset of the word's FIRST character on its line.
  col: number;
  word: string;
}

export interface VimReaderCallbacks {
  onAnnotate?(sel: LineSel): void; // 'a'
  onPermalink?(sel: LineSel): void; // 'Y'
  onGotoDef?(pos: WordPos): void; // 'gd'
  onFindRefs?(pos: WordPos): void; // 'gr'
  onHover?(pos: WordPos): void; // 'K'
  onHistoryStep?(dir: -1 | 1): void; // '[c' / ']c'
  onCycleFile?(dir: -1 | 1): void; // '[f' / ']f'
  onPaneFocus?(dir: "prev" | "next"): void; // Ctrl-w h / Ctrl-w l / Ctrl-w Ctrl-w
  onSplitSelf?(): void; // Ctrl-w v — Wave E "split with self" (open into pane2)
  onClosePane?(): void; // Ctrl-w q — Wave E close the focused pane
  onShowHelp?(): void; // '?'
  /// V3.N1 — jump-list / recent-locations. `onRecordJump` fires AFTER a
  /// jump-class motion lands (G/gg/NG, mark jump, / search step) with the
  /// new cursor line + that line's text so the host can call `recordJump`
  /// without reading the editor. Ctrl-o/i and `g.` are pure host callbacks.
  onRecordJump?(info: { line: number; snippet: string }): void;
  onPinPane?(): void; // V70-A6 `p` — pin a provisional pane
  onJumpBack?(): void; // Ctrl-o
  onJumpForward?(): void; // Ctrl-i
  /// V70-A6 `u` — the Ramp's Back rung (§P7). Bare, so it only reaches this
  /// layer at all because of guard 2 (`CommandRoot.tsx`'s "the vim layer
  /// owns every bare key inside the buffer") — without this arm `u` would be
  /// swallowed the instant a trail-linked tab's destination file focuses the
  /// buffer (A3's "focus follows the file"), which is exactly when it is
  /// meant to fire. Forwards to the SAME registered `nav.back` handler every
  /// other surface uses (`CommandRoot`'s `bus.run`, not a duplicate here).
  onNavBack?(): void;
  onRecentLocations?(): void; // g.
  /// V3.N2 — structure popup / bookmarks.
  onStructurePopup?(): void; // gO
  onToggleBookmark?(): void; // gm
  onMnemonicPopup?(): void; // gM
  /// V3.R2 / R12 — history for selection (`gh`); receives the current
  /// line/range the way annotate/`Y` already do.
  onLineHistory?(sel: LineSel): void; // gh
  /// V3.1-H3a — call/type hierarchy panels.
  onHierarchyCallers?(pos: WordPos): void; // gc
  onHierarchyCallees?(pos: WordPos): void; // gC
  onHierarchyTypes?(pos: WordPos): void; // gt
  /// V3.1-H3b — impact panel + ego-graph.
  onImpact?(pos: WordPos): void; // gi
  onEgoGraph?(pos: WordPos): void; // gG
  /// V70-A4 — the Desk's region chords. Both are pure host callbacks
  /// (nothing about them touches the buffer): `Ctrl-w r` opens the
  /// keyboard resize submode, `Ctrl-w m` zooms the focused region.
  onResizeMode?(): void; // Ctrl-w r
  onZoomRegion?(): void; // Ctrl-w m
}

// --- vim mode/count/prefix state — mirrors `vimKeysReducer`'s state -------

const setVimState = StateEffect.define<VimKeyState>();

const vimStateField = StateField.define<VimKeyState>({
  create: initialVimKeyState,
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setVimState)) value = e.value;
    }
    return value;
  },
});

/// Read the current mode + a display string for any in-flight count/prefix
/// (e.g. `"12g"`, `"5"`, `"'"`) — for a status-line affordance. Safe to call
/// on a state that never installed `vimReader` (returns the initial state).
export function vimStatus(state: EditorState): { mode: VimMode; pending: string } {
  const s = state.field(vimStateField, false) ?? initialVimKeyState();
  return { mode: s.mode, pending: s.pendingCount + s.pendingPrefix };
}

// --- marks ------------------------------------------------------------

interface MarkPos {
  line: number;
  col: number;
}

const setMarkEffect = StateEffect.define<{ id: string; pos: MarkPos }>();

const marksField = StateField.define<Record<string, MarkPos>>({
  create: () => ({}),
  update(marks, tr) {
    for (const e of tr.effects) {
      if (e.is(setMarkEffect)) marks = { ...marks, [e.value.id]: e.value.pos };
    }
    return marks;
  },
});

function setMark(view: EditorView, id: string): void {
  const head = view.state.selection.main.head;
  const line = view.state.doc.lineAt(head);
  view.dispatch({ effects: setMarkEffect.of({ id, pos: { line: line.number, col: head - line.from } }) });
}

function jumpMark(view: EditorView, id: string): void {
  const mark = view.state.field(marksField)[id];
  if (!mark) return;
  const doc = view.state.doc;
  const lineNo = Math.min(Math.max(mark.line, 1), doc.lines);
  const line = doc.line(lineNo);
  const pos = line.from + Math.min(Math.max(mark.col, 0), line.length);
  view.dispatch({ selection: EditorSelection.cursor(pos), effects: EditorView.scrollIntoView(pos, { y: "center" }) });
}

// --- word-under-cursor / line-selection extraction (reads the live view) --

function wordAtCursor(view: EditorView): WordPos | null {
  const head = view.state.selection.main.head;
  const line = view.state.doc.lineAt(head);
  const found = wordAt(line.text, head - line.from);
  if (!found) return null;
  return { line: line.number, col: found.start, word: found.word };
}

function lineSelForCallback(view: EditorView): LineSel {
  const sel = view.state.selection.main;
  const startLine = view.state.doc.lineAt(sel.from).number;
  const endLine = view.state.doc.lineAt(sel.to).number;
  return startLine === endLine ? { line: startLine } : { line: startLine, lineEnd: endLine };
}

/// Cursor line + trimmed line text for the nav-history recorder.
function jumpInfoFromView(view: EditorView): { line: number; snippet: string } {
  const line = view.state.doc.lineAt(view.state.selection.main.head);
  return { line: line.number, snippet: line.text };
}

function notifyJumpRecord(view: EditorView, cb: VimReaderCallbacks): void {
  cb.onRecordJump?.(jumpInfoFromView(view));
}

// --- clipboard (yank) ------------------------------------------------------

function fallbackCopy(text: string): void {
  // The textarea steals focus for the `execCommand` call — remember and
  // RESTORE the previous focus target (the CM content element), or every
  // key after a fallback-path yank lands on `<body>` and the whole modal
  // keymap goes dead until the user clicks back into the buffer.
  const prev = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.focus();
    ta.select();
    // Deprecated, but the only synchronous copy path when the async
    // Clipboard API is unavailable/denied (non-secure context, iframe
    // permissions) — best-effort fallback only.
    document.execCommand("copy");
    document.body.removeChild(ta);
  } catch {
    // Best-effort only — nothing more to do if even the fallback is denied.
  }
  prev?.focus();
}

/// Best-effort copy — async Clipboard API first, sync `execCommand`
/// fallback. Exported for Reader's own copy affordances (`Y` permalink)
/// so the SPA has exactly one clipboard path.
export function copyToClipboard(text: string): void {
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
  } else {
    fallbackCopy(text);
  }
}

// --- motion math (the only place that talks to CM6's position primitives) -

const WORD_CHAR_RE = /[A-Za-z0-9_]/;
type CharClass = "space" | "word" | "punct";
function classify(ch: string): CharClass {
  if (ch === "" || /\s/.test(ch)) return "space";
  return WORD_CHAR_RE.test(ch) ? "word" : "punct";
}

/// `e` has no ready-made CM6 primitive (unlike `w`/`b`, which ride
/// `view.moveByGroup`) — a small hand-rolled scan over the WHOLE document
/// text, classifying runs as word/punctuation/space (vim's own "word"
/// definition). O(file size) per `e` press; fine for the reader's typical
/// file sizes (same assumption `highlightField.ts` documents), and only
/// paid when `e` is actually pressed.
function nextWordEnd(doc: Text, pos: number, count: number): number {
  const text = doc.sliceString(0);
  if (text.length === 0) return 0;
  let i = pos;
  for (let step = 0; step < count; step++) {
    i++;
    while (i < text.length && classify(text[i]) === "space") i++;
    if (i >= text.length) {
      i = text.length - 1;
      break;
    }
    const cls = classify(text[i]);
    while (i + 1 < text.length && classify(text[i + 1]) === cls) i++;
  }
  return Math.min(Math.max(i, 0), text.length - 1);
}

function firstNonBlank(line: Line): number {
  const idx = line.text.search(/\S/);
  return idx === -1 ? line.from : line.from + idx;
}

/// Move to the next/previous paragraph boundary (a blank line), vim-style —
/// `count` blank-line-runs in `forward`'s direction, clamped to the doc.
function paragraphBoundary(doc: Text, pos: number, forward: boolean, count: number): number {
  let lineNo = doc.lineAt(pos).number;
  for (let i = 0; i < count; i++) {
    if (forward) {
      lineNo++;
      while (lineNo <= doc.lines && doc.line(lineNo).text.trim() !== "") lineNo++;
      if (lineNo > doc.lines) {
        lineNo = doc.lines;
        break;
      }
    } else {
      lineNo--;
      while (lineNo >= 1 && doc.line(lineNo).text.trim() !== "") lineNo--;
      if (lineNo < 1) {
        lineNo = 1;
        break;
      }
    }
  }
  lineNo = Math.min(Math.max(lineNo, 1), doc.lines);
  return doc.line(lineNo).from;
}

function computeNewHead(view: EditorView, kind: Exclude<Extract<VimCommand, { t: "move" }>["kind"], "scrollCenter" | "scrollTop" | "scrollBottom">, count: number | null): number {
  const { state } = view;
  const range = state.selection.main;
  const n = count ?? 1;
  switch (kind) {
    case "charLeft": {
      let r: SelectionRange = range;
      for (let i = 0; i < n; i++) r = view.moveByChar(r, false);
      return r.head;
    }
    case "charRight": {
      let r: SelectionRange = range;
      for (let i = 0; i < n; i++) r = view.moveByChar(r, true);
      return r.head;
    }
    case "lineUp": {
      let r: SelectionRange = range;
      for (let i = 0; i < n; i++) r = view.moveVertically(r, false);
      return r.head;
    }
    case "lineDown": {
      let r: SelectionRange = range;
      for (let i = 0; i < n; i++) r = view.moveVertically(r, true);
      return r.head;
    }
    case "wordForward": {
      let r: SelectionRange = range;
      for (let i = 0; i < n; i++) r = view.moveByGroup(r, true);
      return r.head;
    }
    case "wordBackward": {
      let r: SelectionRange = range;
      for (let i = 0; i < n; i++) r = view.moveByGroup(r, false);
      return r.head;
    }
    case "wordEnd":
      return nextWordEnd(state.doc, range.head, n);
    case "lineStart":
      return state.doc.lineAt(range.head).from;
    case "lineFirstNonBlank":
      return firstNonBlank(state.doc.lineAt(range.head));
    case "lineEnd": {
      let line = state.doc.lineAt(range.head);
      for (let i = 1; i < n; i++) {
        if (line.number >= state.doc.lines) break;
        line = state.doc.line(line.number + 1);
      }
      return line.to;
    }
    case "gotoTop": {
      const target = Math.min(Math.max(count ?? 1, 1), state.doc.lines);
      return firstNonBlank(state.doc.line(target));
    }
    case "gotoBottom": {
      const target = count == null ? state.doc.lines : Math.min(Math.max(count, 1), state.doc.lines);
      return firstNonBlank(state.doc.line(target));
    }
    case "paragraphBackward":
      return paragraphBoundary(state.doc, range.head, false, n);
    case "paragraphForward":
      return paragraphBoundary(state.doc, range.head, true, n);
    case "halfPageDown":
      return view.moveVertically(range, true, view.scrollDOM.clientHeight / 2).head;
    case "halfPageUp":
      return view.moveVertically(range, false, view.scrollDOM.clientHeight / 2).head;
    case "fullPageDown":
      return view.moveVertically(range, true, view.scrollDOM.clientHeight).head;
    case "fullPageUp":
      return view.moveVertically(range, false, view.scrollDOM.clientHeight).head;
    default: {
      const _exhaustive: never = kind;
      return _exhaustive;
    }
  }
}

function shapeSelection(state: EditorState, anchor: number, head: number, mode: VimMode): EditorSelection {
  if (mode === "normal") return EditorSelection.create([EditorSelection.cursor(head)]);
  if (mode === "visual") return EditorSelection.create([EditorSelection.range(anchor, head)]);
  // visual-line — snap both ends out to the full lines they land in,
  // preserving which end is the anchor vs. the head so a further motion
  // still extends from the right side.
  const anchorLine = state.doc.lineAt(anchor);
  const headLine = state.doc.lineAt(head);
  const forward = head >= anchor;
  const a = forward ? anchorLine.from : anchorLine.to;
  const h = forward ? headLine.to : headLine.from;
  return EditorSelection.create([EditorSelection.range(a, h)]);
}

function applyMove(
  view: EditorView,
  cmd: Extract<VimCommand, { t: "move" }>,
  modeBeforeThisKey: VimMode,
  cb: VimReaderCallbacks,
): void {
  if (cmd.kind === "scrollCenter" || cmd.kind === "scrollTop" || cmd.kind === "scrollBottom") {
    const head = view.state.selection.main.head;
    const y = cmd.kind === "scrollCenter" ? "center" : cmd.kind === "scrollTop" ? "start" : "end";
    view.dispatch({ effects: EditorView.scrollIntoView(head, { y }) });
    return;
  }
  const anchor = view.state.selection.main.anchor;
  const newHead = computeNewHead(view, cmd.kind, cmd.count);
  const selection = shapeSelection(view.state, anchor, newHead, modeBeforeThisKey);
  view.dispatch({ selection, effects: EditorView.scrollIntoView(newHead) });
  // V3.N1 — G / gg / NG are jump-class motions (NOT plain hjkl). Record
  // after the selection lands so the snippet matches the destination line.
  if (cmd.kind === "gotoTop" || cmd.kind === "gotoBottom") {
    notifyJumpRecord(view, cb);
  }
}

function collapseToNormal(view: EditorView): void {
  view.dispatch({ selection: EditorSelection.cursor(view.state.selection.main.head) });
}

function applyModeSet(view: EditorView, mode: VimMode, prevMode: VimMode): void {
  if (mode === "normal") {
    collapseToNormal(view);
    return;
  }
  const sel = view.state.selection.main;
  // Fresh entry from normal mode anchors at the current cursor; switching
  // between visual <-> visual-line keeps the existing anchor/head, just
  // reshapes the rendered selection.
  const anchor = prevMode === "normal" ? sel.head : sel.anchor;
  view.dispatch({ selection: shapeSelection(view.state, anchor, sel.head, mode) });
}

// --- command dispatch -------------------------------------------------------

function dispatchCommand(view: EditorView, cmd: VimCommand, cb: VimReaderCallbacks, prevMode: VimMode): boolean {
  switch (cmd.t) {
    case "move":
      applyMove(view, cmd, prevMode, cb);
      return true;
    case "mode-set":
      applyModeSet(view, cmd.mode, prevMode);
      return true;
    case "yank-line":
      copyToClipboard(view.state.doc.lineAt(view.state.selection.main.head).text);
      return true;
    case "yank-selection": {
      const sel = view.state.selection.main;
      copyToClipboard(view.state.sliceDoc(sel.from, sel.to));
      collapseToNormal(view);
      return true;
    }
    case "mark-set":
      setMark(view, cmd.id);
      return true;
    case "mark-jump":
      jumpMark(view, cmd.id);
      notifyJumpRecord(view, cb);
      return true;
    case "search-open":
      openSearchPanel(view);
      return true;
    case "search-step": {
      const before = view.state.selection.main.head;
      (cmd.dir === 1 ? findNext : findPrevious)(view);
      if (view.state.selection.main.head !== before) notifyJumpRecord(view, cb);
      return true;
    }
    case "search-word": {
      const w = wordAtCursor(view);
      if (!w) return true;
      view.dispatch({ effects: setSearchQuery.of(new SearchQuery({ search: w.word })) });
      const before = view.state.selection.main.head;
      (cmd.dir === 1 ? findNext : findPrevious)(view);
      if (view.state.selection.main.head !== before) notifyJumpRecord(view, cb);
      return true;
    }
    case "goto-line-open":
      gotoLinePanel(view);
      return true;
    case "cb-annotate":
      if (!cb.onAnnotate) return false;
      cb.onAnnotate(lineSelForCallback(view));
      return true;
    case "cb-permalink":
      if (!cb.onPermalink) return false;
      cb.onPermalink(lineSelForCallback(view));
      return true;
    case "cb-goto-def": {
      if (!cb.onGotoDef) return false;
      const w = wordAtCursor(view);
      if (!w) return false;
      cb.onGotoDef(w);
      return true;
    }
    case "cb-find-refs": {
      if (!cb.onFindRefs) return false;
      const w = wordAtCursor(view);
      if (!w) return false;
      cb.onFindRefs(w);
      return true;
    }
    case "cb-hover": {
      if (!cb.onHover) return false;
      const w = wordAtCursor(view);
      if (!w) return false;
      cb.onHover(w);
      return true;
    }
    case "cb-history-step":
      if (!cb.onHistoryStep) return false;
      cb.onHistoryStep(cmd.dir);
      return true;
    case "cb-cycle-file":
      if (!cb.onCycleFile) return false;
      cb.onCycleFile(cmd.dir);
      return true;
    case "cb-pane-focus":
      if (!cb.onPaneFocus) return false;
      cb.onPaneFocus(cmd.dir);
      return true;
    case "cb-split-self":
      if (!cb.onSplitSelf) return false;
      cb.onSplitSelf();
      return true;
    case "cb-close-pane":
      if (!cb.onClosePane) return false;
      cb.onClosePane();
      return true;
    case "cb-resize-mode":
      if (!cb.onResizeMode) return false;
      cb.onResizeMode();
      return true;
    case "cb-zoom-region":
      if (!cb.onZoomRegion) return false;
      cb.onZoomRegion();
      return true;
    case "cb-pin-pane":
      if (!cb.onPinPane) return false;
      cb.onPinPane();
      return true;
    case "cb-show-help":
      if (!cb.onShowHelp) return false;
      cb.onShowHelp();
      return true;
    case "cb-jump-back":
      if (!cb.onJumpBack) return false;
      cb.onJumpBack();
      return true;
    case "cb-jump-forward":
      if (!cb.onJumpForward) return false;
      cb.onJumpForward();
      return true;
    case "cb-nav-back":
      if (!cb.onNavBack) return false;
      cb.onNavBack();
      return true;
    case "cb-recent-locations":
      if (!cb.onRecentLocations) return false;
      cb.onRecentLocations();
      return true;
    case "cb-structure-popup":
      if (!cb.onStructurePopup) return false;
      cb.onStructurePopup();
      return true;
    case "cb-toggle-bookmark":
      if (!cb.onToggleBookmark) return false;
      cb.onToggleBookmark();
      return true;
    case "cb-mnemonic-popup":
      if (!cb.onMnemonicPopup) return false;
      cb.onMnemonicPopup();
      return true;
    case "cb-line-history":
      if (!cb.onLineHistory) return false;
      cb.onLineHistory(lineSelForCallback(view));
      return true;
    case "cb-hierarchy-callers": {
      if (!cb.onHierarchyCallers) return false;
      const w = wordAtCursor(view);
      if (!w) return false;
      cb.onHierarchyCallers(w);
      return true;
    }
    case "cb-hierarchy-callees": {
      if (!cb.onHierarchyCallees) return false;
      const w = wordAtCursor(view);
      if (!w) return false;
      cb.onHierarchyCallees(w);
      return true;
    }
    case "cb-hierarchy-types": {
      if (!cb.onHierarchyTypes) return false;
      const w = wordAtCursor(view);
      if (!w) return false;
      cb.onHierarchyTypes(w);
      return true;
    }
    case "cb-impact": {
      if (!cb.onImpact) return false;
      const w = wordAtCursor(view);
      if (!w) return false;
      cb.onImpact(w);
      return true;
    }
    case "cb-ego-graph": {
      if (!cb.onEgoGraph) return false;
      const w = wordAtCursor(view);
      if (!w) return false;
      cb.onEgoGraph(w);
      return true;
    }
    default: {
      const _exhaustive: never = cmd;
      return _exhaustive;
    }
  }
}

// --- guarded keydown ---------------------------------------------------

// V3.N1 adds Ctrl-o / Ctrl-i (jump list). Keep this set in lock-step with
// the Ctrl branch of `vimKeysReducer` — any key not listed here never
// reaches the reducer when Ctrl is held (browser default wins).
const ALLOWED_CTRL_KEYS = new Set(["d", "u", "f", "b", "w", "o", "i"]);

function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.tagName === "INPUT" || target.tagName === "TEXTAREA") return true;
  return target.isContentEditable;
}

function stateChanged(a: VimKeyState, b: VimKeyState): boolean {
  return a.mode !== b.mode || a.pendingCount !== b.pendingCount || a.pendingPrefix !== b.pendingPrefix;
}

function makeKeydownHandler(cb: VimReaderCallbacks) {
  return (event: KeyboardEvent, view: EditorView): boolean => {
    // Guard rails: never steal input from the search/goto-line panel's own
    // `<input>`, never touch Meta/Alt combos (Mod-K omnibox, OS shortcuts),
    // and never touch a Ctrl combo outside our own small set (so
    // Ctrl-C/Ctrl-V/etc. reach the browser untouched).
    if (isTypingTarget(event.target)) return false;
    if (event.metaKey || event.altKey) return false;
    if (event.ctrlKey && !ALLOWED_CTRL_KEYS.has(event.key)) return false;

    const prevState = view.state.field(vimStateField);
    const { state: nextState, commands } = vimKeysReducer(prevState, { key: event.key, ctrl: event.ctrlKey });

    let handled = stateChanged(prevState, nextState);
    for (const cmd of commands) {
      if (dispatchCommand(view, cmd, cb, prevState.mode)) handled = true;
    }
    view.dispatch({ effects: setVimState.of(nextState) });

    if (handled) event.preventDefault();
    return handled;
  };
}

// --- extension assembly -----------------------------------------------

/// A theme nudge so the cursor reads clearly against the reader's own
/// palette (`styles/tokens.css`'s `--accent`) instead of CM6's baked-in
/// black/`#ddd` — visibility (not layout) is the only thing this overrides;
/// CM6's own base theme already gates `.cm-cursor` on `.cm-focused` (hidden
/// otherwise), which is exactly the "visible when focused" behavior wanted.
const cursorTheme = EditorView.theme({
  ".cm-cursor, .cm-dropCursor": {
    // R6 — the `#4c8bf5` fallback disagreed with the real `--accent`
    // (`#8a7fff` dark / `#6657f5` light) and was dead anyway; a bare
    // `var()` cannot paint a plausible lie after a token rename.
    borderLeftColor: "var(--accent)",
    borderLeftWidth: "2px",
  },
});

/// The vim-style reading keymap extension. Adds: a visible, focusable
/// cursor (`drawSelection` + a focusable content element — `editable`
/// stays whatever the host set, per the reader's read-only invariant); the
/// CM6 search-state facet (so `n`/`N`/`*`/`#`/`setSearchQuery` work); and a
/// single high-precedence keydown handler that delegates all sequence logic
/// to `vimKeysReducer` and maps its output onto view calls. Never dispatches
/// a transaction with document changes — see `vimKeys.ts`'s `VIM_COMMAND_KINDS`
/// for the exhaustive, mutation-free command vocabulary this is limited to.
/// Wrap CM6 search-panel Enter/Shift-Enter so a `/` search submit also
/// feeds the nav-history recorder (V3.N1). Other searchKeymap bindings
/// pass through unchanged.
function searchKeymapWithJumpRecord(cb: VimReaderCallbacks) {
  return searchKeymap.map((binding) => {
    const run = binding.run;
    if (!run) return binding;
    if (binding.key !== "Enter" && binding.key !== "Shift-Enter") return binding;
    return {
      ...binding,
      run: (view: EditorView) => {
        const before = view.state.selection.main.head;
        const ok = run(view);
        if (ok && view.state.selection.main.head !== before) notifyJumpRecord(view, cb);
        return ok;
      },
    };
  });
}

export function vimReader(cb: VimReaderCallbacks): Extension {
  return [
    vimStateField,
    marksField,
    drawSelection(),
    EditorView.contentAttributes.of({ tabindex: "0" }),
    cursorTheme,
    search(),
    // The stock search keymap rides BELOW the vim handler (which is
    // Prec.highest and consumes `/`, `n`, `N`, Ctrl-f… first when the
    // CONTENT is focused) — it exists for the panels' own scope: Escape
    // closes the search/goto panel from inside its `<input>` (a typing
    // target the vim handler deliberately ignores), Enter/Shift-Enter step
    // matches, and Mod-f still reaches CM6's in-buffer find rather than
    // the browser's page find. V3.N1 wraps Enter to record search jumps.
    keymap.of(searchKeymapWithJumpRecord(cb)),
    Prec.highest(EditorView.domEventHandlers({ keydown: makeKeydownHandler(cb) })),
  ];
}
