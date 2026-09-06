// Assembles the CodeMirror 6 extension set for the kb markdown editor.
//
// The view is created ONCE (see useCodeMirror.ts) with this extension set;
// callbacks that change across renders (onSubmit, onAttach) are read through
// getters so the view never needs rebuilding. Order matters in the keymap:
// completion/close-brackets win over the markdown continuation, which wins over
// the generic default keymap.

import { EditorView, keymap, placeholder } from "@codemirror/view";
import { EditorState, type Extension } from "@codemirror/state";
import { history, historyKeymap, defaultKeymap } from "@codemirror/commands";
import { markdown, markdownLanguage, markdownKeymap } from "@codemirror/lang-markdown";
import { syntaxTree } from "@codemirror/language";
import {
  closeBrackets,
  closeBracketsKeymap,
  completionKeymap,
} from "@codemirror/autocomplete";
import { kbThemeExtension } from "./theme";
import {
  toggleBold,
  toggleItalic,
  toggleStrike,
  toggleInlineCode,
  insertLink,
  toggleHeading,
  continueList,
  tabOrRelease,
  shiftTabOrRelease,
  wrapOnType,
} from "./commands";
import { slashCommands } from "./slash";
import { livePreview, attachmentCtxFacet } from "./livePreview";

export type EditorConfig = {
  /// Placeholder shown when the doc is empty.
  placeholder?: string;
  /// Accessible name for the editable region (generic — the per-surface label
  /// lives on the hidden mirror <textarea> so test/getByRole queries are
  /// unambiguous; see CodeMirrorInput.tsx).
  ariaLabel?: string;
  /// Live read of the submit handler (⌘/Ctrl-Enter). Stable identity.
  getOnSubmit: () => (() => void) | undefined;
  /// Live read of the attach handler (slash `/attach`). Stable identity.
  getOnAttach: () => (() => void) | undefined;
  /// Inline live-preview decorations (the Obsidian-style render-in-place).
  livePreview: boolean;
  /// Resolves inline `attachment:<aid>` image thumbnails in live preview.
  attachment?: { kb: string; id: string } | null;
  /// Whether this surface supports attachments (gates the slash `/attach`
  /// item + the toolbar 📎 — off for notes / modal reply/edit).
  attachable?: boolean;
  /// Enable `[[` wikilink autocomplete scoped to this kb (note composer only).
  wikilinkKb?: string | null;
};

export function buildExtensions(cfg: EditorConfig): Extension {
  const formatKeymap = keymap.of([
    { key: "Mod-b", run: toggleBold, preventDefault: true },
    { key: "Mod-i", run: toggleItalic, preventDefault: true },
    { key: "Mod-e", run: toggleInlineCode, preventDefault: true },
    { key: "Mod-k", run: insertLink, preventDefault: true },
    { key: "Mod-Shift-x", run: toggleStrike, preventDefault: true },
    { key: "Mod-Alt-1", run: toggleHeading(1) },
    { key: "Mod-Alt-2", run: toggleHeading(2) },
    { key: "Mod-Alt-3", run: toggleHeading(3) },
  ]);

  const submitKeymap = keymap.of([
    {
      key: "Mod-Enter",
      preventDefault: true,
      run: () => {
        const fn = cfg.getOnSubmit();
        if (!fn) return false;
        fn();
        return true;
      },
    },
  ]);

  const editKeymap = keymap.of([
    { key: "Enter", run: continueList },
    { key: "Tab", run: tabOrRelease },
    { key: "Shift-Tab", run: shiftTabOrRelease },
  ]);

  const out: Extension[] = [
    history(),
    markdown({ base: markdownLanguage, addKeymap: false }),
    EditorView.lineWrapping,
    closeBrackets(),
    wrapOnType,
    cfg.placeholder ? placeholder(cfg.placeholder) : [],
    slashCommands({
      onAttach: cfg.attachable ? () => cfg.getOnAttach()?.() : undefined,
      wikilinkKb: cfg.wikilinkKb ?? undefined,
    }),
    // Keymap precedence: submit → completion/brackets → format → md edit →
    // markdown continuation (blockquotes + Backspace) → history → default.
    submitKeymap,
    keymap.of([...closeBracketsKeymap, ...completionKeymap]),
    formatKeymap,
    editKeymap,
    keymap.of([...markdownKeymap, ...historyKeymap, ...defaultKeymap]),
    EditorState.allowMultipleSelections.of(true),
    EditorView.contentAttributes.of({
      "aria-label": cfg.ariaLabel ?? "Markdown editor",
      "aria-multiline": "true",
      spellcheck: "true",
      autocapitalize: "sentences",
    }),
    kbThemeExtension(),
  ];

  if (cfg.attachment) out.push(attachmentCtxFacet.of(cfg.attachment));
  if (cfg.livePreview) out.push(livePreview());

  return out;
}

// --- toolbar active-format detection ---------------------------------------

// Lezer-markdown node names → toolbar button keys. Walked from the cursor up.
const NODE_TO_KEY: { test: RegExp; key: string }[] = [
  { test: /^StrongEmphasis$/, key: "bold" },
  { test: /^Emphasis$/, key: "italic" },
  { test: /^Strikethrough$/, key: "strike" },
  { test: /^(InlineCode|FencedCode|CodeBlock)$/, key: "code" },
  { test: /^ATXHeading[1-6]$|^SetextHeading[12]$/, key: "heading" },
  { test: /^Blockquote$/, key: "quote" },
  { test: /^BulletList$/, key: "bullet" },
  { test: /^OrderedList$/, key: "ordered" },
  { test: /^(Task|TaskMarker)$/, key: "task" },
];

/// Which formats apply at the current selection — drives toolbar `aria-pressed`.
export function activeFormats(state: EditorState): Set<string> {
  const out = new Set<string>();
  const pos = state.selection.main.head;
  let node: ReturnType<typeof syntaxTree>["topNode"] | null = syntaxTree(
    state,
  ).resolveInner(pos, -1);
  for (; node; node = node.parent) {
    for (const { test, key } of NODE_TO_KEY) {
      if (test.test(node.name)) out.add(key);
    }
  }
  return out;
}
