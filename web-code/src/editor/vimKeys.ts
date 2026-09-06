// Pure vim-style READING keymap state machine — no CodeMirror, no DOM.
//
// This module owns every bit of sequence/count/mode logic for the reader's
// modal keymap (normal / visual / visual-line, numeric-prefix counts, and the
// two-key sequences `gg`/`gd`/`gr`/`g.`/`zz`/`zt`/`zb`/`[c`/`]c`/`[f`/`]f`/`m{a-z}`/
// `'{a-z}`/`yy`/`Ctrl-w h`/`Ctrl-w l`/`Ctrl-w Ctrl-w`/`Ctrl-w v`/`Ctrl-w q`/`Ctrl-w r`/`Ctrl-w m`).
// `vimReader.ts` (the CM6 extension) does nothing but feed it `KeyInput`s and
// map the resulting `VimCommand`s onto view calls — see that module's doc for the split
// rationale (DOM-free unit tests here; a thin, unit-untestable-in-this-repo
// glue layer there, since `vitest.config.ts` runs `environment: "node"`).
//
// This is a READER, not an editor clone: there is no operator-pending mode
// (no `dd`/`cw`/`yw` motions-as-operators) — the only two-key "operator-ish"
// sequence is `yy` (yank the current line), and `y` in visual mode yanks the
// selection. Nothing here can ever describe a document edit — see
// `VIM_COMMAND_KINDS` below and its test-side no-edit guarantee.

export type VimMode = "normal" | "visual" | "visual-line";

/// A single keydown, already normalised to `KeyboardEvent.key` (so shifted
/// symbols like `$`/`^`/`{`/`}`/`*`/`#` and shifted letters like `G`/`V`/`N`
/// arrive pre-resolved — no separate `shiftKey` needed). `ctrl` covers the
/// handful of Ctrl-modified bindings (`Ctrl-d/u/f/b/o/i`, `Ctrl-w`); every other
/// modifier (Meta/Alt, or a Ctrl combo not listed here) is guarded OUT before
/// reaching this reducer — see `vimReader.ts`'s keydown handler.
export interface KeyInput {
  key: string;
  ctrl?: boolean;
}

export interface VimKeyState {
  mode: VimMode;
  /// Accumulated count digits, as typed (`""` = no pending count). Kept as a
  /// string (not parsed) so leading behaviour — `"0"` alone means
  /// line-start, `"10"` means the count 10 — falls out of simple string
  /// logic rather than a signed/parsed edge case.
  pendingCount: string;
  /// The single in-flight multi-key prefix, if any: one of
  /// `"g" "z" "[" "]" "m" "'" "y" "ctrl-w"`, else `""`.
  pendingPrefix: string;
}

export function initialVimKeyState(): VimKeyState {
  return { mode: "normal", pendingCount: "", pendingPrefix: "" };
}

export type MoveKind =
  | "charLeft"
  | "charRight"
  | "lineUp"
  | "lineDown"
  | "wordForward"
  | "wordBackward"
  | "wordEnd"
  | "lineStart"
  | "lineFirstNonBlank"
  | "lineEnd"
  | "gotoTop"
  | "gotoBottom"
  | "paragraphBackward"
  | "paragraphForward"
  | "halfPageDown"
  | "halfPageUp"
  | "fullPageDown"
  | "fullPageUp"
  | "scrollCenter"
  | "scrollTop"
  | "scrollBottom";

/// Every command this reducer can emit. Deliberately has NO variant that can
/// describe a document mutation (no insert/delete/replace/change kind) — see
/// `VIM_COMMAND_KINDS` + `vimKeys.test.ts`'s no-edit-guarantee suite, which
/// keystroke-storms the reducer and asserts every emitted `t` is one of
/// these, i.e. selection/scroll/clipboard/callback only.
export type VimCommand =
  /// `count`: explicit typed count, or `null` when none was given. For most
  /// kinds `null` means "1" (`vimReader.ts` does `cmd.count ?? 1`); for
  /// `gotoBottom` specifically, `null` means "last line" (bare `G`) — the one
  /// place vim's "no count" default isn't just "count of 1", so it can't be
  /// folded into the same default (`5G` = line 5, plain `G` = last line, NOT
  /// line 1).
  | { t: "move"; kind: MoveKind; count: number | null }
  | { t: "mode-set"; mode: VimMode }
  | { t: "yank-line" }
  | { t: "yank-selection" }
  | { t: "mark-set"; id: string }
  | { t: "mark-jump"; id: string }
  | { t: "search-open" }
  | { t: "search-step"; dir: 1 | -1 } // n / N
  | { t: "search-word"; dir: 1 | -1 } // * / #
  | { t: "goto-line-open" }
  | { t: "cb-annotate" }
  | { t: "cb-permalink" }
  | { t: "cb-goto-def" }
  | { t: "cb-find-refs" }
  | { t: "cb-hover" }
  | { t: "cb-history-step"; dir: 1 | -1 }
  | { t: "cb-cycle-file"; dir: 1 | -1 }
  | { t: "cb-pane-focus"; dir: "prev" | "next" }
  /// `Ctrl-w v` — Wave E's "split with self": open the CURRENT file into
  /// pane2 at the same ref/line.
  | { t: "cb-split-self" }
  /// `Ctrl-w q` — Wave E's "close the focused pane".
  | { t: "cb-close-pane" }
  | { t: "cb-show-help" }
  /// V3.N1 — jump list back/forward (`Ctrl-o` / `Ctrl-i`).
  | { t: "cb-jump-back" }
  | { t: "cb-jump-forward" }
  /// V3.N1 — open the Recent Locations popup (`g.`).
  | { t: "cb-recent-locations" }
  /// V3.N2 — open the file structure popup (`gO`).
  | { t: "cb-structure-popup" }
  /// V3.N2 — toggle bookmark on the current line (`gm`).
  | { t: "cb-toggle-bookmark" }
  /// V3.N2 — open the mnemonic bookmarks popup (`gM`).
  | { t: "cb-mnemonic-popup" }
  /// V3.R2 / R12 — history for selection (`gh`; free in g-prefix).
  | { t: "cb-line-history" }
  /// V3.1-H3a — call hierarchy callers (`gc`) / callees (`gC`).
  | { t: "cb-hierarchy-callers" }
  | { t: "cb-hierarchy-callees" }
  /// V3.1-H3a — type hierarchy (`gt`).
  | { t: "cb-hierarchy-types" }
  /// V3.1-H3b — impact panel (`gi`).
  | { t: "cb-impact" }
  /// V3.1-H3b — ego-graph (`gG`).
  | { t: "cb-ego-graph" }
  /// V70-A4 — `Ctrl-w r`: enter the Desk's keyboard RESIZE submode
  /// (`desk/resizeSubmode.ts` owns the grammar once inside; this
  /// reducer only opens the door). The submode itself is deliberately
  /// NOT modelled here: it is modal and window-level, so a chord
  /// machine scoped to one CodeMirror view is the wrong home for it.
  | { t: "cb-resize-mode" }
  /// V70-A4 — `Ctrl-w m`: zoom the focused region to fill the shell,
  /// and back (Zed's ToggleZoom, tmux's `z`).
  | { t: "cb-zoom-region" }
  /// V70-A6 — `p`: pin a PROVISIONAL pane (§P7's commitment ladder). `p` is
  /// free in a reader (vim's `p` is paste, and nothing here can mutate a
  /// document), and the chip on a provisional pane says the key literally —
  /// VS Code's italic preview tab is the counter-example the research names:
  /// the signal must be loud and the promotion gesture discoverable.
  | { t: "cb-pin-pane" }
  /// V70-A6 — `u`: the Ramp's Back rung (§P7's `nav.back`, registered
  /// `scope: global`/`dispatch: central`). `u` is a BARE key, so
  /// `CommandRoot`'s guard 2 never sees it once a file is open — "focus
  /// follows the file" (A3) lands DOM focus in the buffer the instant a
  /// trail-linked tab's destination renders, which is exactly when `u` is
  /// meant to work. This is the vim layer's own arm for it, so the read-only
  /// buffer stops being the one surface where the Ramp's Back rung is
  /// unreachable; the reducer only requests it, `CommandRoot`'s registered
  /// `nav.back` handler still owns the trail-origin-or-plain-Back decision
  /// (`app.tsx`).
  | { t: "cb-nav-back" };

/// The exhaustive `t` allowlist, used by both the runtime no-edit guard test
/// and (by construction, since it's derived from the type above by hand) as
/// a documentation-level "this is the whole vocabulary" list. Keep in sync
/// with the `VimCommand` union — `vimKeys.test.ts` cross-checks every
/// storm-emitted command's `t` against this array.
export const VIM_COMMAND_KINDS = [
  "move",
  "mode-set",
  "yank-line",
  "yank-selection",
  "mark-set",
  "mark-jump",
  "search-open",
  "search-step",
  "search-word",
  "goto-line-open",
  "cb-annotate",
  "cb-permalink",
  "cb-goto-def",
  "cb-find-refs",
  "cb-hover",
  "cb-history-step",
  "cb-cycle-file",
  "cb-pane-focus",
  "cb-split-self",
  "cb-close-pane",
  "cb-show-help",
  "cb-jump-back",
  "cb-jump-forward",
  "cb-recent-locations",
  "cb-structure-popup",
  "cb-toggle-bookmark",
  "cb-mnemonic-popup",
  "cb-line-history",
  "cb-hierarchy-callers",
  "cb-hierarchy-callees",
  "cb-hierarchy-types",
  "cb-impact",
  "cb-ego-graph",
  "cb-resize-mode",
  "cb-zoom-region",
  "cb-pin-pane",
  "cb-nav-back",
] as const satisfies readonly VimCommand["t"][];

export interface VimKeyResult {
  state: VimKeyState;
  commands: VimCommand[];
}

const MARK_ID_RE = /^[a-z]$/;

/// Reset the count/prefix accumulators (a command was either just completed
/// or the in-flight sequence was invalid/abandoned), optionally landing on a
/// new mode.
function settle(mode: VimMode, commands: VimCommand[]): VimKeyResult {
  return { state: { mode, pendingCount: "", pendingPrefix: "" }, commands };
}

function parsedCount(pendingCount: string): number | null {
  return pendingCount === "" ? null : parseInt(pendingCount, 10);
}

/// The core state machine. `vimReader.ts` calls this once per guarded
/// keydown and applies the returned commands + next state; nothing else in
/// this codebase should need to know the sequence grammar.
export function vimKeysReducer(state: VimKeyState, key: KeyInput): VimKeyResult {
  // Escape always wins: collapse to normal + drop any in-flight sequence,
  // regardless of what was pending.
  if (key.key === "Escape") {
    return settle("normal", [{ t: "mode-set", mode: "normal" }]);
  }

  // --- Resolve an in-flight two-key prefix -------------------------------
  if (state.pendingPrefix !== "") {
    const count = parsedCount(state.pendingCount);
    switch (state.pendingPrefix) {
      case "g":
        if (key.key === "g") return settle(state.mode, [{ t: "move", kind: "gotoTop", count }]);
        if (key.key === "d") return settle(state.mode, [{ t: "cb-goto-def" }]);
        if (key.key === "r") return settle(state.mode, [{ t: "cb-find-refs" }]);
        // V3.N1 — `g.` opens the Recent Locations popup (not a motion).
        if (key.key === ".") return settle(state.mode, [{ t: "cb-recent-locations" }]);
        // V3.N2 — structure / bookmarks.
        if (key.key === "O") return settle(state.mode, [{ t: "cb-structure-popup" }]);
        if (key.key === "m") return settle(state.mode, [{ t: "cb-toggle-bookmark" }]);
        if (key.key === "M") return settle(state.mode, [{ t: "cb-mnemonic-popup" }]);
        // V3.R2 / R12 — `gh` history-for-selection (free; `gL` reserved if taken).
        if (key.key === "h") return settle(state.mode, [{ t: "cb-line-history" }]);
        // V3.1-H3a — call/type hierarchy (gc/gC/gt free in g-prefix).
        if (key.key === "c") return settle(state.mode, [{ t: "cb-hierarchy-callers" }]);
        if (key.key === "C") return settle(state.mode, [{ t: "cb-hierarchy-callees" }]);
        if (key.key === "t") return settle(state.mode, [{ t: "cb-hierarchy-types" }]);
        // V3.1-H3b — impact panel (`gi`) + ego-graph (`gG`). Free: gi/gG
        // (taken: gg gd gr g. gO gm gM gh gc gC gt).
        if (key.key === "i") return settle(state.mode, [{ t: "cb-impact" }]);
        if (key.key === "G") return settle(state.mode, [{ t: "cb-ego-graph" }]);
        return settle(state.mode, []);
      case "z":
        if (key.key === "z") return settle(state.mode, [{ t: "move", kind: "scrollCenter", count: null }]);
        if (key.key === "t") return settle(state.mode, [{ t: "move", kind: "scrollTop", count: null }]);
        if (key.key === "b") return settle(state.mode, [{ t: "move", kind: "scrollBottom", count: null }]);
        return settle(state.mode, []);
      case "[":
        if (key.key === "c") return settle(state.mode, [{ t: "cb-history-step", dir: -1 }]);
        if (key.key === "f") return settle(state.mode, [{ t: "cb-cycle-file", dir: -1 }]);
        return settle(state.mode, []);
      case "]":
        if (key.key === "c") return settle(state.mode, [{ t: "cb-history-step", dir: 1 }]);
        if (key.key === "f") return settle(state.mode, [{ t: "cb-cycle-file", dir: 1 }]);
        return settle(state.mode, []);
      case "m":
        if (MARK_ID_RE.test(key.key)) return settle(state.mode, [{ t: "mark-set", id: key.key }]);
        return settle(state.mode, []);
      case "'":
        if (MARK_ID_RE.test(key.key)) return settle(state.mode, [{ t: "mark-jump", id: key.key }]);
        return settle(state.mode, []);
      case "y":
        if (key.key === "y") return settle("normal", [{ t: "yank-line" }]);
        return settle(state.mode, []);
      case "ctrl-w":
        if (key.key === "h") return settle(state.mode, [{ t: "cb-pane-focus", dir: "prev" }]);
        if (key.key === "l") return settle(state.mode, [{ t: "cb-pane-focus", dir: "next" }]);
        if (key.key === "w" && key.ctrl) return settle(state.mode, [{ t: "cb-pane-focus", dir: "next" }]);
        // Wave E — `v`/`q` continuations. Like `h`/`l` above (and unlike the
        // `w`-continuation just above), these don't require Ctrl still held
        // for the second key: the practical chord is press-release
        // Ctrl-w, then tap v/q — holding Ctrl through the second keypress
        // never reaches the reducer at all for these two keys, since
        // `vimReader.ts`'s `ALLOWED_CTRL_KEYS` gate (mirroring the same
        // limitation `h`/`l` already have) doesn't include them.
        if (key.key === "v") return settle(state.mode, [{ t: "cb-split-self" }]);
        if (key.key === "q") return settle(state.mode, [{ t: "cb-close-pane" }]);
        // V70-A4 — the Desk's two region chords. `r` opens the resize
        // submode, `m` zooms the focused region. Free in the `Ctrl-w`
        // namespace (taken: h l w v q) and both are vim/tmux natives, so
        // neither is a new convention to learn.
        if (key.key === "r") return settle(state.mode, [{ t: "cb-resize-mode" }]);
        if (key.key === "m") return settle(state.mode, [{ t: "cb-zoom-region" }]);
        return settle(state.mode, []);
      default:
        return settle(state.mode, []);
    }
  }

  // --- Digit accumulator --------------------------------------------------
  // `0` only joins a count in progress; a bare `0` (no pending digits) is
  // "line start" (handled below with the rest of the motions).
  if (/^[1-9]$/.test(key.key) || (key.key === "0" && state.pendingCount !== "")) {
    return { state: { ...state, pendingCount: state.pendingCount + key.key }, commands: [] };
  }

  const count = parsedCount(state.pendingCount);
  const move = (kind: MoveKind): VimCommand => ({ t: "move", kind, count });

  // --- Mode toggles (act regardless of the current mode) -----------------
  if (key.key === "v" && !key.ctrl) {
    const mode: VimMode = state.mode === "visual" ? "normal" : "visual";
    return settle(mode, [{ t: "mode-set", mode }]);
  }
  if (key.key === "V" && !key.ctrl) {
    const mode: VimMode = state.mode === "visual-line" ? "normal" : "visual-line";
    return settle(mode, [{ t: "mode-set", mode }]);
  }

  // `y`: yanks + exits when a selection is live; otherwise starts the `yy`
  // (yank-line) prefix — there is no other operator in this reader, so `y`
  // alone never means anything but "wait for a second y".
  if (key.key === "y" && !key.ctrl) {
    if (state.mode !== "normal") return settle("normal", [{ t: "yank-selection" }]);
    return { state: { ...state, pendingPrefix: "y" }, commands: [] };
  }

  // --- Ctrl-modified bindings ---------------------------------------------
  if (key.ctrl) {
    switch (key.key) {
      case "d":
        return settle(state.mode, [move("halfPageDown")]);
      case "u":
        return settle(state.mode, [move("halfPageUp")]);
      case "f":
        return settle(state.mode, [move("fullPageDown")]);
      case "b":
        return settle(state.mode, [move("fullPageUp")]);
      case "o":
        // V3.N1 — jump list older (vim Ctrl-o).
        return settle(state.mode, [{ t: "cb-jump-back" }]);
      case "i":
        // V3.N1 — jump list newer (vim Ctrl-i).
        return settle(state.mode, [{ t: "cb-jump-forward" }]);
      case "w":
        return { state: { ...state, pendingPrefix: "ctrl-w" }, commands: [] };
      default:
        return settle(state.mode, []);
    }
  }

  // --- Prefix starters (await a continuation key) -------------------------
  if (key.key === "g") return { state: { ...state, pendingPrefix: "g" }, commands: [] };
  if (key.key === "z") return { state: { ...state, pendingPrefix: "z", pendingCount: "" }, commands: [] };
  if (key.key === "[") return { state: { ...state, pendingPrefix: "[" }, commands: [] };
  if (key.key === "]") return { state: { ...state, pendingPrefix: "]" }, commands: [] };
  if (key.key === "m") return { state: { ...state, pendingPrefix: "m", pendingCount: "" }, commands: [] };
  if (key.key === "'") return { state: { ...state, pendingPrefix: "'", pendingCount: "" }, commands: [] };

  // --- Single-key motions --------------------------------------------------
  switch (key.key) {
    case "h":
      return settle(state.mode, [move("charLeft")]);
    case "l":
      return settle(state.mode, [move("charRight")]);
    case "j":
      return settle(state.mode, [move("lineDown")]);
    case "k":
      return settle(state.mode, [move("lineUp")]);
    case "w":
      return settle(state.mode, [move("wordForward")]);
    case "b":
      return settle(state.mode, [move("wordBackward")]);
    case "e":
      return settle(state.mode, [move("wordEnd")]);
    case "0":
      return settle(state.mode, [{ t: "move", kind: "lineStart", count: null }]);
    case "^":
      return settle(state.mode, [{ t: "move", kind: "lineFirstNonBlank", count: null }]);
    case "$":
      return settle(state.mode, [move("lineEnd")]);
    case "{":
      return settle(state.mode, [move("paragraphBackward")]);
    case "}":
      return settle(state.mode, [move("paragraphForward")]);
    case "G":
      return settle(state.mode, [move("gotoBottom")]);
    // --- search --------------------------------------------------------
    case "/":
      return settle(state.mode, [{ t: "search-open" }]);
    case ":":
      return settle(state.mode, [{ t: "goto-line-open" }]);
    case "n":
      return settle(state.mode, [{ t: "search-step", dir: 1 }]);
    case "N":
      return settle(state.mode, [{ t: "search-step", dir: -1 }]);
    case "*":
      return settle(state.mode, [{ t: "search-word", dir: 1 }]);
    case "#":
      return settle(state.mode, [{ t: "search-word", dir: -1 }]);
    // --- action keys -----------------------------------------------------
    case "a":
      return settle(state.mode, [{ t: "cb-annotate" }]);
    // V70-A6 — pin the focused pane when it is provisional. A no-op (the
    // host callback simply is not wired) when it is not, which is why this
    // can be an unconditional reducer arm: the reducer knows key grammar,
    // not pane state.
    case "p":
      return settle(state.mode, [{ t: "cb-pin-pane" }]);
    // V70-A6 — the Ramp's Back rung (§P7). Unconditional for the same reason
    // `p` is: the reducer only asks for it, and `nav.back`'s own handler
    // (`app.tsx`) decides whether there is a trail origin to push or this is
    // an ordinary Back with nothing special to do.
    case "u":
      return settle(state.mode, [{ t: "cb-nav-back" }]);
    case "Y":
      return settle(state.mode, [{ t: "cb-permalink" }]);
    case "K":
      return settle(state.mode, [{ t: "cb-hover" }]);
    case "?":
      return settle(state.mode, [{ t: "cb-show-help" }]);
    default:
      // Unrecognised key (e.g. plain `d`/`c`/`p` — no operator-pending in a
      // reader, no default browser action to protect either): drop any
      // accumulated count and do nothing.
      return settle(state.mode, []);
  }
}

// --- word-under-cursor extraction (pure — no DOM/CM6 needed) --------------

const WORD_CHAR_RE = /[A-Za-z0-9_]/;

export interface WordAt {
  word: string;
  start: number;
  end: number;
}

/// Extend both directions from `col` (a UTF-16 offset into `lineText`) over
/// identifier characters (`[A-Za-z0-9_]`) to find the word under the
/// cursor. If `col` sits ON a non-word character (punctuation/whitespace),
/// prefers the word starting AT `col` (cursor sitting just before a word),
/// then falls back to the word ending just before `col` (cursor sitting
/// just after one); returns `null` if there's no word either side.
export function wordAt(lineText: string, col: number): WordAt | null {
  const at = (i: number) => (i >= 0 && i < lineText.length ? lineText[i] : "");
  const isWord = (i: number) => WORD_CHAR_RE.test(at(i));

  // Prefer the char right at the cursor; else the one just before it
  // (cursor resting immediately after a word, e.g. at end-of-line).
  let anchor = -1;
  if (isWord(col)) anchor = col;
  else if (isWord(col - 1)) anchor = col - 1;
  if (anchor === -1) return null;

  let start = anchor;
  while (start > 0 && isWord(start - 1)) start--;
  let end = anchor + 1;
  while (end < lineText.length && isWord(end)) end++;

  return { word: lineText.slice(start, end), start, end };
}
