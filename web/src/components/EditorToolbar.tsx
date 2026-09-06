import type { Command } from "@codemirror/view";
import type { ReactNode } from "react";
import { Icon } from "./icons";
import {
  toggleBold,
  toggleItalic,
  toggleStrike,
  toggleInlineCode,
  insertLink,
  toggleHeading,
  toggleQuote,
  toggleBulletList,
  toggleOrderedList,
  toggleTaskList,
  insertCodeBlock,
  insertTable,
  insertCallout,
} from "../editor/commands";

// Formatting toolbar for the markdown editor. Buttons dispatch CM6 commands via
// `run`; `active` (from the editor's current selection) drives aria-pressed.
//
// The full set is shown on EVERY surface — at narrow widths the row simply
// flex-wraps (styles/editor.css), so no feature is ever hidden in the 360px
// panel vs the modal. Each button preventDefaults mousedown so clicking never
// blurs the editor or collapses the selection before the command reads it.

const isMac =
  typeof navigator !== "undefined" && /Mac|iP(hone|ad|od)/.test(navigator.platform);
const mod = isMac ? "⌘" : "Ctrl";

type Btn = {
  key: string; // toolbar id + active-format key
  glyph: ReactNode;
  label: string;
  hint?: string;
  cmd?: Command;
  className?: string;
  toggle?: boolean; // reflects aria-pressed from `active`
};

const GROUPS: Btn[][] = [
  [
    { key: "bold", glyph: "B", label: "Bold", hint: `${mod}B`, cmd: toggleBold, className: "cme__tb-b", toggle: true },
    { key: "italic", glyph: "I", label: "Italic", hint: `${mod}I`, cmd: toggleItalic, className: "cme__tb-i", toggle: true },
    { key: "strike", glyph: "S", label: "Strikethrough", hint: `${mod}⇧X`, cmd: toggleStrike, className: "cme__tb-s", toggle: true },
    { key: "code", glyph: "</>", label: "Inline code", hint: `${mod}E`, cmd: toggleInlineCode, toggle: true },
    { key: "link", glyph: <Icon.Link />, label: "Link", hint: `${mod}K`, cmd: insertLink },
  ],
  [
    { key: "heading", glyph: "H", label: "Heading", hint: `${mod}⌥2`, cmd: toggleHeading(2), toggle: true },
    { key: "quote", glyph: "❝", label: "Quote", cmd: toggleQuote, toggle: true },
  ],
  [
    { key: "bullet", glyph: "•", label: "Bullet list", cmd: toggleBulletList, toggle: true },
    { key: "ordered", glyph: "1.", label: "Numbered list", cmd: toggleOrderedList, toggle: true },
    { key: "task", glyph: "☑", label: "Task list", cmd: toggleTaskList, toggle: true },
  ],
  [
    { key: "codeblock", glyph: "⌗", label: "Code block", cmd: insertCodeBlock },
    { key: "table", glyph: "▦", label: "Table", cmd: insertTable },
    { key: "callout", glyph: "!", label: "Callout", cmd: insertCallout },
  ],
];

type Props = {
  run: (cmd: Command) => void;
  active: Set<string>;
  onAttach?: () => void;
  disabled?: boolean;
};

export default function EditorToolbar({ run, active, onAttach, disabled }: Props) {
  return (
    <div className="cme__toolbar" role="toolbar" aria-label="formatting" aria-disabled={disabled}>
      {GROUPS.map((group, gi) => (
        <div className="cme__tb-group" key={gi}>
          {group.map((b) => (
            <button
              key={b.key}
              type="button"
              className={`cme__tb-btn ${b.className ?? ""} ${
                b.toggle && active.has(b.key) ? "is-active" : ""
              }`}
              title={b.hint ? `${b.label} · ${b.hint}` : b.label}
              aria-label={b.label}
              aria-pressed={b.toggle ? active.has(b.key) : undefined}
              disabled={disabled}
              // Keep the editor focus + selection alive for the command.
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => b.cmd && run(b.cmd)}
            >
              <span aria-hidden="true">{b.glyph}</span>
            </button>
          ))}
        </div>
      ))}
      {onAttach && (
        <div className="cme__tb-group">
          <button
            type="button"
            className="cme__tb-btn"
            title="Attach file or image"
            aria-label="Attach file or image"
            disabled={disabled}
            onMouseDown={(e) => e.preventDefault()}
            onClick={onAttach}
          >
            <span aria-hidden="true"><Icon.Paperclip /></span>
          </button>
        </div>
      )}
    </div>
  );
}
