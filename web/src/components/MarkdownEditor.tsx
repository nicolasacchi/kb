import {
  forwardRef,
  useImperativeHandle,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Command } from "@codemirror/view";
// P2 — editor.css ships INSIDE this lazy-loaded module (was eager in main.tsx)
// so the ~few-KB CodeMirror stylesheet loads on demand with the CM chunk. The
// classes are exclusively .cme*/.cm-kb-* (no eager consumer), and Vite loads a
// dynamic chunk's CSS before executing its JS, so the composer is styled on
// first paint.
import "../styles/editor.css";
import CommentBody from "./CommentBody";
import CodeMirrorInput, { type CodeMirrorHandle } from "./CodeMirrorInput";
import EditorToolbar from "./EditorToolbar";

// Obsidian-class markdown composer, shared by every authoring surface (new
// comment, reply, edit, note). A CodeMirror 6 editor (syntax highlight + inline
// live preview + slash menu + ⌘-shortcuts) wrapped in a [ Write | Preview |
// Split ] view control and a formatting toolbar. Controlled — the parent owns
// `value`.
//
// The same FEATURE SET renders on every surface (the user's call); container
// queries in styles/editor.css only reflow layout (toolbar wraps; Split flips
// side-by-side ↔ stacked) when width demands it — nothing is removed in the
// 360px panel vs the 640px modal.
//
// Back-compat the e2e suite depends on (do not change lightly):
//   - the `.cp__editor` wrapper, the `Write`/`Preview` tab roles, and the
//     `.cp__editor-preview` render pane survive (CodeMirrorInput keeps a hidden
//     mirror <textarea> carrying `ariaLabel` + `textareaClassName` + the value).

export type MarkdownEditorHandle = {
  /// Switch to an editing view and focus the editor (used after "quote").
  focusWrite: () => void;
  /// Insert `text` at the caret, switch to an editing view, update the value.
  /// Load-bearing for attachment refs (dndProps / ComposerAttachBar).
  insertAtCursor: (text: string) => void;
};

type ViewMode = "write" | "preview" | "split";

export type MarkdownEditorProps = {
  value: string;
  onChange: (v: string) => void;
  ariaLabel?: string;
  placeholder?: string;
  autoFocus?: boolean;
  /// The per-surface input class (CSS hook + e2e selector) → the mirror.
  textareaClassName?: string;
  /// Override the Preview renderer. Defaults to `CommentBody`.
  renderPreview?: (value: string) => ReactNode;
  /// Offer the side-by-side Split view (default true).
  allowSplit?: boolean;
  /// Inline live-preview decorations (default true).
  livePreview?: boolean;
  /// kb/id for resolving inline `attachment:` image thumbnails in live preview.
  attachment?: { kb: string; id: string } | null;
  /// ⌘/Ctrl-Enter submit.
  onSubmit?: () => void;
  /// Toolbar 📎 + slash `/attach` → open the composer file picker.
  onAttach?: () => void;
  /// Enable `[[` wikilink autocomplete scoped to this kb (note composer).
  wikilinkKb?: string | null;
};

const VIEW_KEY = "kb.editor.view";

function storedView(): ViewMode {
  try {
    const v = localStorage.getItem(VIEW_KEY);
    if (v === "write" || v === "preview" || v === "split") return v;
  } catch {
    /* private mode / disabled storage → default */
  }
  return "write";
}

function persistView(v: ViewMode) {
  try {
    localStorage.setItem(VIEW_KEY, v);
  } catch {
    /* ignore */
  }
}

const MarkdownEditor = forwardRef<MarkdownEditorHandle, MarkdownEditorProps>(
  function MarkdownEditor(
    {
      value,
      onChange,
      ariaLabel,
      placeholder,
      autoFocus,
      textareaClassName = "cp__editor-input",
      renderPreview,
      allowSplit = true,
      livePreview = true,
      attachment = null,
      onSubmit,
      onAttach,
      wikilinkKb = null,
    },
    ref,
  ) {
    const initial = storedView();
    const [tab, setTab] = useState<ViewMode>(
      initial === "split" && !allowSplit ? "write" : initial,
    );
    const [active, setActive] = useState<Set<string>>(() => new Set());
    const cmRef = useRef<CodeMirrorHandle | null>(null);

    const setView = (v: ViewMode) => {
      setTab(v);
      persistView(v);
    };

    useImperativeHandle(
      ref,
      () => ({
        focusWrite() {
          if (tab === "preview") setTab("write");
          requestAnimationFrame(() => cmRef.current?.focus());
        },
        insertAtCursor(text: string) {
          if (tab === "preview") setTab("write");
          requestAnimationFrame(() => cmRef.current?.insertAtCursor(text));
        },
      }),
      [tab],
    );

    const runCmd = (cmd: Command) => {
      const view = cmRef.current?.view();
      if (!view) return;
      cmd(view);
      view.focus();
    };

    const preview = (
      <div className="cp__editor-preview" aria-label="preview">
        {renderPreview ? (
          renderPreview(value || "_nothing to preview_")
        ) : (
          <CommentBody body={value || "_nothing to preview_"} />
        )}
      </div>
    );

    const modes: ViewMode[] = allowSplit
      ? ["write", "preview", "split"]
      : ["write", "preview"];
    const tabLabel = (m: ViewMode) =>
      m === "write" ? "Write" : m === "preview" ? "Preview" : "Split";

    const showEditor = tab === "write" || tab === "split";
    const showPreview = tab === "preview" || tab === "split";

    return (
      <div className={`cp__editor cme cme--${tab}`}>
        <div className="cp__editor-head">
          <div className="cp__editor-tabs" role="tablist" aria-label="editor view">
            {modes.map((m) => (
              <button
                key={m}
                type="button"
                role="tab"
                aria-selected={tab === m}
                className={`cp__editor-tab ${tab === m ? "is-active" : ""}`}
                onClick={() => setView(m)}
              >
                {tabLabel(m)}
              </button>
            ))}
          </div>
          {tab !== "preview" && (
            <EditorToolbar run={runCmd} active={active} onAttach={onAttach} />
          )}
        </div>

        <div className="cme__body">
          <CodeMirrorInput
            ref={cmRef}
            value={value}
            onChange={onChange}
            placeholder={placeholder}
            editorAriaLabel={ariaLabel || "Markdown editor"}
            mirrorClassName={textareaClassName}
            autoFocus={autoFocus}
            hidden={!showEditor}
            livePreview={livePreview}
            attachment={attachment}
            wikilinkKb={wikilinkKb}
            onSubmit={onSubmit}
            onAttach={onAttach}
            attachable={!!onAttach}
            onActiveFormats={setActive}
          />
          {showPreview && preview}
        </div>
      </div>
    );
  },
);

export default MarkdownEditor;
