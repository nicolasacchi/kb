// CodeMirror 6 markdown editing commands — the keyboard + toolbar verbs.
//
// Everything here emits PLAIN MARKDOWN (the kb backend strips raw HTML from
// comment bodies — see crates/kb-core/src/markdown.rs::render_comment_fragment),
// so there is no rich-text document model: a command is just a text transform
// over the current selection. Toggles are idempotent (detect-and-unwrap) so the
// toolbar buttons + ⌘B/⌘I shortcuts feel like Obsidian.

import {
  EditorSelection,
  EditorState,
  type ChangeSpec,
  type TransactionSpec,
} from "@codemirror/state";
import { EditorView, type Command } from "@codemirror/view";
import { indentMore, indentLess } from "@codemirror/commands";
import { syntaxTree } from "@codemirror/language";

// A GFM list item: indent, bullet (-/*/+) OR ordered (N. / N)), trailing space,
// optional task marker, then content. Used for smart continuation + Tab gating.
// (Matched via String.match — same groups as a non-global regex.)
const LIST_RE = /^(\s*)(?:([-*+])|(\d+)([.)]))(\s+)(\[[ xX]\]\s+)?(.*)$/;

// --- inline wrap toggles (bold / italic / code / strike) -------------------

/// Pure transaction for toggling `marker` around each selection range (no view
/// — unit-testable). If already wrapped (markers inside the selection, or just
/// outside it), removes them; otherwise wraps. Empty selection → caret between.
export function wrapSpec(
  state: EditorState,
  marker: string,
  close = marker,
): TransactionSpec {
  return state.changeByRange((range) => {
    const { from, to } = range;
    const outBefore = state.sliceDoc(Math.max(0, from - marker.length), from);
    const outAfter = state.sliceDoc(to, Math.min(state.doc.length, to + close.length));
    // Markers immediately outside the selection → unwrap them.
    if (outBefore === marker && outAfter === close) {
      return {
        changes: [
          { from: from - marker.length, to: from, insert: "" },
          { from: to, to: to + close.length, insert: "" },
        ],
        range: EditorSelection.range(from - marker.length, to - marker.length),
      };
    }
    const sel = state.sliceDoc(from, to);
    // Markers captured inside the selection → unwrap.
    if (
      sel.length >= marker.length + close.length &&
      sel.startsWith(marker) &&
      sel.endsWith(close)
    ) {
      const inner = sel.slice(marker.length, sel.length - close.length);
      return {
        changes: { from, to, insert: inner },
        range: EditorSelection.range(from, from + inner.length),
      };
    }
    // Otherwise wrap.
    const insert = marker + sel + close;
    return {
      changes: { from, to, insert },
      range: sel.length
        ? EditorSelection.range(from + marker.length, to + marker.length)
        : EditorSelection.cursor(from + marker.length),
    };
  });
}

/// Toggle `marker` around the selection (idempotent wrap/unwrap).
export function toggleWrap(marker: string, close = marker): Command {
  return (view) => {
    view.dispatch(
      view.state.update(wrapSpec(view.state, marker, close), {
        scrollIntoView: true,
        userEvent: "input.format",
      }),
    );
    return true;
  };
}

export const toggleBold = toggleWrap("**");
export const toggleItalic = toggleWrap("*");
export const toggleStrike = toggleWrap("~~");
export const toggleInlineCode = toggleWrap("`");

/// Wrap the selection (or "text") in a markdown link, dropping the caret onto
/// the placeholder `url` so the user types the destination next.
export const insertLink: Command = (view) => {
  const { state } = view;
  const tr = state.changeByRange((range) => {
    const sel = state.sliceDoc(range.from, range.to) || "text";
    const insert = `[${sel}](url)`;
    const urlFrom = range.from + 1 + sel.length + 2; // past "[sel]("
    return {
      changes: { from: range.from, to: range.to, insert },
      range: EditorSelection.range(urlFrom, urlFrom + 3), // select "url"
    };
  });
  view.dispatch(state.update(tr, { scrollIntoView: true, userEvent: "input.link" }));
  return true;
};

// --- block / line transforms ----------------------------------------------

// Run `transform` over every line touched by the selection, replacing each
// line's text and keeping the selection spanning the affected lines.
function eachLine(
  view: EditorView,
  transform: (text: string) => string,
  userEvent: string,
): boolean {
  const { state } = view;
  const ranges = state.selection.ranges;
  const changes: ChangeSpec[] = [];
  const seen = new Set<number>();
  for (const r of ranges) {
    let pos = r.from;
    while (pos <= r.to) {
      const line = state.doc.lineAt(pos);
      if (!seen.has(line.from)) {
        seen.add(line.from);
        const next = transform(line.text);
        if (next !== line.text) {
          changes.push({ from: line.from, to: line.to, insert: next });
        }
      }
      if (line.to + 1 > r.to) break;
      pos = line.to + 1;
    }
  }
  if (!changes.length) return true;
  view.dispatch(state.update({ changes, scrollIntoView: true, userEvent }));
  return true;
}

/// Toggle a heading of `level` on the selected lines. Re-running with the same
/// level strips it; a different level replaces it.
export function toggleHeading(level: number): Command {
  const prefix = "#".repeat(level) + " ";
  return (view) =>
    eachLine(
      view,
      (text) => {
        const m = text.match(/^(#{1,6})\s+/);
        if (m && m[1].length === level) return text.slice(m[0].length);
        const body = m ? text.slice(m[0].length) : text;
        return prefix + body;
      },
      "input.heading",
    );
}

/// Toggle a `> ` blockquote on the selected lines.
export const toggleQuote: Command = (view) =>
  eachLine(
    view,
    (text) => (/^>\s?/.test(text) ? text.replace(/^>\s?/, "") : "> " + text),
    "input.quote",
  );

/// Toggle a `- ` bullet on the selected lines (strips ordered/task markers).
export const toggleBulletList: Command = (view) =>
  eachLine(
    view,
    (text) => {
      const m = text.match(LIST_RE);
      if (m && m[2] && !m[6]) return m[1] + m[7]; // already a plain bullet → strip
      const indent = m ? m[1] : "";
      const content = m ? m[7] : text.trim();
      return `${indent}- ${content}`;
    },
    "input.list",
  );

/// Toggle a `- [ ] ` task on the selected lines.
export const toggleTaskList: Command = (view) =>
  eachLine(
    view,
    (text) => {
      const m = text.match(LIST_RE);
      if (m && m[6]) return `${m[1]}${m[2] ?? m[3] + m[4]}${m[5]}${m[7]}`; // strip task
      const indent = m ? m[1] : "";
      const content = m ? m[7] : text.trim();
      return `${indent}- [ ] ${content}`;
    },
    "input.task",
  );

/// Number the selected lines `1.`, `2.`, … (toggle off if already numbered).
export const toggleOrderedList: Command = (view) => {
  const { state } = view;
  const r = state.selection.main;
  const first = state.doc.lineAt(r.from);
  const already = /^\s*\d+[.)]\s+/.test(first.text);
  let n = 0;
  return eachLine(
    view,
    (text) => {
      n += 1;
      const m = text.match(LIST_RE);
      const indent = m ? m[1] : "";
      const content = m ? m[7] : text.trim();
      return already ? `${indent}${content}` : `${indent}${n}. ${content}`;
    },
    "input.ordered",
  );
};

/// Insert (or wrap) a fenced code block around the selection.
export const insertCodeBlock: Command = (view) => {
  const { state } = view;
  const r = state.selection.main;
  const sel = state.sliceDoc(r.from, r.to);
  const insert = "```\n" + sel + "\n```\n";
  view.dispatch(
    state.update({
      changes: { from: r.from, to: r.to, insert },
      selection: EditorSelection.cursor(r.from + 3), // onto the opening fence (type a lang)
      scrollIntoView: true,
      userEvent: "input.codeblock",
    }),
  );
  return true;
};

/// Insert a small GFM table skeleton at the caret.
export const insertTable: Command = (view) => {
  const { state } = view;
  const r = state.selection.main;
  const tbl = "| Column | Column |\n| --- | --- |\n| cell | cell |\n";
  view.dispatch(
    state.update({
      changes: { from: r.from, to: r.to, insert: tbl },
      selection: EditorSelection.cursor(r.from + 2), // onto first "Column"
      scrollIntoView: true,
      userEvent: "input.table",
    }),
  );
  return true;
};

/// Insert an Obsidian callout block (`> [!note] …`) — matches the server's
/// callout renderer (markdown::rewrite_callouts).
export const insertCallout: Command = (view) => {
  const { state } = view;
  const r = state.selection.main;
  const sel = state.sliceDoc(r.from, r.to);
  const insert = `> [!note] \n> ${sel || ""}\n`;
  view.dispatch(
    state.update({
      changes: { from: r.from, to: r.to, insert },
      selection: EditorSelection.cursor(r.from + 9), // after "> [!note] "
      scrollIntoView: true,
      userEvent: "input.callout",
    }),
  );
  return true;
};

/// Insert a horizontal rule on its own line.
export const insertDivider: Command = (view) => {
  const { state } = view;
  const r = state.selection.main;
  const line = state.doc.lineAt(r.head);
  const atLineStart = r.head === line.from;
  const insert = (atLineStart ? "" : "\n") + "---\n";
  view.dispatch(
    state.update({
      changes: { from: r.head, insert },
      selection: EditorSelection.cursor(r.head + insert.length),
      scrollIntoView: true,
      userEvent: "input.hr",
    }),
  );
  return true;
};

// --- smart Enter: continue / terminate lists -------------------------------

/// Pure transaction for the smart-Enter behaviour (no view — unit-testable).
/// Returns null on a non-list line / multi-range selection (caller falls back
/// to the default newline). Otherwise: empty item → clear the marker
/// (terminate); non-empty → continue (increment ordered numbers, carry task
/// markers as unchecked).
export function continueListSpec(state: EditorState): TransactionSpec | null {
  const range = state.selection.main;
  if (!range.empty) return null;
  const line = state.doc.lineAt(range.head);
  const m = line.text.match(LIST_RE);
  if (!m) return null;
  const [, indent, bullet, num, delim, space, task, content] = m;
  // Only act when the caret is at/after the content start (not mid-marker).
  if (range.head < line.from + (m[0].length - content.length)) return null;

  if (content.trim() === "") {
    // Empty item → clear the marker (terminate the list).
    return {
      changes: { from: line.from, to: line.to, insert: indent },
      selection: EditorSelection.cursor(line.from + indent.length),
      userEvent: "input",
    };
  }
  const taskPart = task ? "[ ] " : "";
  const marker = bullet
    ? `${indent}${bullet}${space}${taskPart}`
    : `${indent}${parseInt(num, 10) + 1}${delim}${space}${taskPart}`;
  const insert = state.lineBreak + marker;
  return {
    changes: { from: range.head, insert },
    selection: EditorSelection.cursor(range.head + insert.length),
    scrollIntoView: true,
    userEvent: "input",
  };
}

/// Enter inside a list item continues the list; Enter on an empty item
/// terminates it. Returns false on a non-list line so the default runs.
export const continueList: Command = (view) => {
  const spec = continueListSpec(view.state);
  if (!spec) return false;
  view.dispatch(view.state.update(spec));
  return true;
};

// --- Tab discipline: indent inside lists/code, else release focus ----------

function inListOrCode(view: EditorView): boolean {
  const { state } = view;
  const pos = state.selection.main.head;
  if (LIST_RE.test(state.doc.lineAt(pos).text)) return true;
  for (
    let n: ReturnType<typeof syntaxTree>["topNode"] | null =
      syntaxTree(state).resolveInner(pos, -1);
    n;
    n = n.parent
  ) {
    if (/Code|Fenced|CodeBlock|CodeText/.test(n.name)) return true;
  }
  return false;
}

/// Tab indents a list/code line; elsewhere it returns false so the browser's
/// default Tab moves focus OUT of the editor (keyboard accessibility — CM's
/// stock indentWithTab would trap the user).
export const tabOrRelease: Command = (view) =>
  inListOrCode(view) ? indentMore(view) : false;

export const shiftTabOrRelease: Command = (view) =>
  inListOrCode(view) ? indentLess(view) : false;

// --- wrap-on-selection input handler ---------------------------------------

const WRAP: Record<string, string> = { "*": "*", _: "_", "`": "`", "~": "~" };

/// When the user types `*`/`_`/`` ` ``/`~` WITH a selection, wrap the selection
/// (Obsidian behaviour) instead of replacing it. Empty selection → normal
/// insert (closeBrackets handles bracket/quote auto-pairing separately). IME /
/// multi-char input never matches (single-char lookup), so it's composition-safe.
export const wrapOnType = EditorView.inputHandler.of((view, from, to, text) => {
  if (from === to) return false;
  const close = WRAP[text];
  if (!close) return false;
  const sel = view.state.sliceDoc(from, to);
  view.dispatch({
    changes: { from, to, insert: text + sel + close },
    selection: EditorSelection.range(from + text.length, to + text.length),
    userEvent: "input.type.wrap",
  });
  return true;
});
