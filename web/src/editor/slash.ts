// Slash-command menu — type `/` (at line start or after whitespace) to open a
// block-insert palette, Obsidian-style. Built on @codemirror/autocomplete: each
// completion's `apply` strips the typed `/query` and runs the matching command
// from commands.ts. The attachment item delegates to a host callback (the
// composer's hidden file picker), so this module stays free of upload logic.

import {
  autocompletion,
  type Completion,
  type CompletionContext,
  type CompletionResult,
  type CompletionSource,
} from "@codemirror/autocomplete";
import type { EditorView, Command } from "@codemirror/view";
import { fetchWikilinkSuggest } from "../api/notes";
import {
  toggleHeading,
  toggleBulletList,
  toggleOrderedList,
  toggleTaskList,
  toggleQuote,
  insertCodeBlock,
  insertTable,
  insertCallout,
  insertDivider,
} from "./commands";

export type SlashOptions = {
  /// Open the composer's file picker (the `/attach` item). Omitted → no item.
  onAttach?: () => void;
  /// Enable `[[` wikilink autocomplete scoped to this kb. Omitted → no
  /// wikilink source (e.g. comment composers, where wikilinks don't apply).
  wikilinkKb?: string;
};

// Strip the live `/query` token ending at the caret. Returns the caret after.
function stripSlashToken(view: EditorView) {
  const pos = view.state.selection.main.head;
  const line = view.state.doc.lineAt(pos);
  const before = view.state.sliceDoc(line.from, pos);
  const m = before.match(/(^|\s)\/(\w*)$/);
  const start = m ? pos - m[2].length - 1 : pos;
  if (start < pos) view.dispatch({ changes: { from: start, to: pos, insert: "" } });
}

// Build an apply() that removes the slash token then runs a CM command.
function runAfterStrip(cmd: Command) {
  return (view: EditorView) => {
    stripSlashToken(view);
    cmd(view);
  };
}

function options(opts: SlashOptions): Completion[] {
  const list: Completion[] = [
    { label: "Heading 1", detail: "# ", type: "keyword", apply: runAfterStrip(toggleHeading(1)) },
    { label: "Heading 2", detail: "## ", type: "keyword", apply: runAfterStrip(toggleHeading(2)) },
    { label: "Heading 3", detail: "### ", type: "keyword", apply: runAfterStrip(toggleHeading(3)) },
    { label: "Bullet list", detail: "- ", type: "keyword", apply: runAfterStrip(toggleBulletList) },
    { label: "Numbered list", detail: "1. ", type: "keyword", apply: runAfterStrip(toggleOrderedList) },
    { label: "Task list", detail: "- [ ] ", type: "keyword", apply: runAfterStrip(toggleTaskList) },
    { label: "Quote", detail: "> ", type: "keyword", apply: runAfterStrip(toggleQuote) },
    { label: "Code block", detail: "```", type: "keyword", apply: runAfterStrip(insertCodeBlock) },
    { label: "Table", detail: "| … |", type: "keyword", apply: runAfterStrip(insertTable) },
    { label: "Callout", detail: "> [!note]", type: "keyword", apply: runAfterStrip(insertCallout) },
    { label: "Divider", detail: "---", type: "keyword", apply: runAfterStrip(insertDivider) },
  ];
  if (opts.onAttach) {
    list.push({
      label: "Attachment",
      detail: "📎 upload",
      type: "keyword",
      apply: (view: EditorView) => {
        stripSlashToken(view);
        opts.onAttach?.();
      },
    });
  }
  return list;
}

function source(opts: SlashOptions) {
  const list = options(opts);
  return (ctx: CompletionContext): CompletionResult | null => {
    const line = ctx.state.doc.lineAt(ctx.pos);
    const before = ctx.state.sliceDoc(line.from, ctx.pos);
    // `/` at line start or after whitespace (so "and/or" never triggers).
    const m = before.match(/(^|\s)\/(\w*)$/);
    if (!m) return null;
    const word = m[2];
    return {
      from: ctx.pos - word.length, // fuzzy-match the word after the slash vs labels
      to: ctx.pos,
      options: list,
      validFor: /^\w*$/,
    };
  };
}

// --- `[[` wikilink autocomplete -------------------------------------------

/// A completion source that fires on `[[` and suggests corpus artifacts by
/// title (server-ranked via `/wikilinks/suggest`). Accepting inserts
/// `[[Title]]`, consuming a `]]` that closeBrackets may have auto-inserted, so
/// the caret lands cleanly after the link. The server already ranks/filters,
/// so `filter: false` keeps its order.
function wikilinkSource(kb: string): CompletionSource {
  return async (ctx: CompletionContext): Promise<CompletionResult | null> => {
    // `[[` then the query so far (no `]`, no newline), ending at the caret.
    const tok = ctx.matchBefore(/\[\[[^\]\n]*$/);
    if (!tok) return null;
    const query = tok.text.slice(2);
    if (!ctx.explicit && query.length === 0 && tok.from + 2 !== ctx.pos) return null;
    let suggestions;
    try {
      suggestions = (await fetchWikilinkSuggest(kb, query, 12)).suggestions;
    } catch {
      return null;
    }
    if (suggestions.length === 0) return null;
    return {
      from: tok.from,
      to: ctx.pos,
      filter: false,
      options: suggestions.map((s) => ({
        label: s.title,
        detail: s.is_note ? "note" : s.source_relative,
        type: s.is_note ? "text" : "keyword",
        apply: (view: EditorView, _c: Completion, from: number, to: number) => {
          const after = view.state.sliceDoc(to, to + 2);
          const end = after === "]]" ? to + 2 : to;
          // A title carrying `[ ] | #` can't be linked verbatim (it'd terminate
          // early, be read as an alias, or have its `#fragment` stripped) — fall
          // back to the unambiguous source-relative path, which the resolver's
          // path tier matches exactly.
          const target = /[[\]|#]/.test(s.title) ? s.source_relative : s.title;
          const insert = `[[${target}]]`;
          view.dispatch({
            changes: { from, to: end, insert },
            selection: { anchor: from + insert.length },
          });
        },
      })),
    };
  };
}

/// The slash-command extension. `activateOnTyping` lets `/` open the menu; the
/// completion keymap (Enter/Tab to accept, Esc to dismiss) is wired in
/// extensions.ts alongside the other keymaps (`defaultKeymap: false` here). The
/// `[[` wikilink source rides the SAME autocompletion config (a second
/// override source) so the two never fight over the completion facet.
export function slashCommands(opts: SlashOptions = {}) {
  const sources: CompletionSource[] = [source(opts)];
  if (opts.wikilinkKb) sources.push(wikilinkSource(opts.wikilinkKb));
  return autocompletion({
    override: sources,
    activateOnTyping: true,
    defaultKeymap: false,
    icons: false,
  });
}
