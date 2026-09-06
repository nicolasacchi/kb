import { forwardRef, useImperativeHandle, useMemo, useRef } from "react";
import type { EditorView } from "@codemirror/view";
import { useCodeMirror } from "../editor/useCodeMirror";
import { activeFormats, type EditorConfig } from "../editor/extensions";

// The CodeMirror 6 editing surface + a hidden, value-synced mirror <textarea>.
//
// CM6's `.cm-content` is the REAL textbox: it carries the per-surface accessible
// name (`editorAriaLabel`), so getByRole("textbox",{name}).fill() drives the
// actual editor (exercising its keymaps / live preview), not a shadow input.
//
// The mirror is an `aria-hidden`, value-synced <textarea> kept ONLY as a stable
// value-readout: CM6 is a contenteditable with no `.value`, and live-preview
// decorations corrupt its textContent, so `.toHaveValue()` on the mirror (via
// its `.cp__*-input` class) is the reliable way to assert the raw markdown.
//   - humans edit CM6 → updateListener → onChange → value → mirror reflects it
//   - Playwright fills `.cm-content` → CM6 onChange → value → mirror reflects it
// The mirror is aria-hidden so it's NOT a second textbox in the a11y tree (a
// name-based getByRole resolves uniquely to `.cm-content`).

export type CodeMirrorHandle = {
  focus: () => void;
  insertAtCursor: (text: string) => void;
  view: () => EditorView | null;
};

type Props = {
  value: string;
  onChange: (v: string) => void;
  /// Placeholder for the empty editor.
  placeholder?: string;
  /// Generic accessible label for the CM editing region (NOT a test name).
  editorAriaLabel?: string;
  /// Inline live-preview decorations (default on).
  livePreview?: boolean;
  /// Resolves inline `attachment:<aid>` thumbnails (image live preview).
  attachment?: { kb: string; id: string } | null;
  autoFocus?: boolean;
  /// Visually hide the CM surface (Preview-only tab) while keeping the view +
  /// mirror alive so insertAtCursor + the value seam still work.
  hidden?: boolean;
  /// ⌘/Ctrl-Enter submit.
  onSubmit?: () => void;
  /// Slash `/attach` → open the composer file picker.
  onAttach?: () => void;
  /// Whether the surface supports attachments (gates slash `/attach`).
  attachable?: boolean;
  /// Enable `[[` wikilink autocomplete scoped to this kb (note composer).
  wikilinkKb?: string | null;
  /// Mirror seam: the per-surface class (`cp__file-scope-input` etc.) carried by
  /// the hidden value-readout <textarea> (used by `.toHaveValue` assertions).
  mirrorClassName?: string;
  /// Active markdown formats at the selection (drives the toolbar).
  onActiveFormats?: (active: Set<string>) => void;
};

const CodeMirrorInput = forwardRef<CodeMirrorHandle, Props>(
  function CodeMirrorInput(
    {
      value,
      onChange,
      placeholder,
      editorAriaLabel,
      livePreview = true,
      attachment = null,
      autoFocus,
      hidden,
      onSubmit,
      onAttach,
      attachable,
      wikilinkKb = null,
      mirrorClassName,
      onActiveFormats,
    },
    ref,
  ) {
    const onSubmitRef = useRef(onSubmit);
    onSubmitRef.current = onSubmit;
    const onAttachRef = useRef(onAttach);
    onAttachRef.current = onAttach;
    const onActiveRef = useRef(onActiveFormats);
    onActiveRef.current = onActiveFormats;

    const kb = attachment?.kb ?? null;
    const id = attachment?.id ?? null;

    const config: EditorConfig = useMemo(
      () => ({
        placeholder,
        ariaLabel: editorAriaLabel,
        getOnSubmit: () => onSubmitRef.current,
        getOnAttach: () => onAttachRef.current,
        livePreview,
        attachment: kb && id ? { kb, id } : null,
        attachable,
        wikilinkKb,
      }),
      [placeholder, editorAriaLabel, livePreview, kb, id, attachable, wikilinkKb],
    );
    const configKey = useMemo(
      () =>
        JSON.stringify({
          placeholder,
          editorAriaLabel,
          livePreview,
          kb,
          id,
          attachable,
          wikilinkKb,
        }),
      [placeholder, editorAriaLabel, livePreview, kb, id, attachable, wikilinkKb],
    );

    const { setParent, viewRef } = useCodeMirror({
      value,
      onChange,
      config,
      configKey,
      autoFocus,
      onUpdate: (view) => onActiveRef.current?.(activeFormats(view.state)),
    });

    useImperativeHandle(
      ref,
      () => ({
        focus() {
          viewRef.current?.focus();
        },
        insertAtCursor(text: string) {
          const view = viewRef.current;
          if (!view) {
            onChange(value + text);
            return;
          }
          view.dispatch(view.state.replaceSelection(text));
          view.focus();
        },
        view: () => viewRef.current,
      }),
      [value, onChange, viewRef],
    );

    return (
      <div className={`cme__cm${hidden ? " cme__cm--hidden" : ""}`}>
        <div ref={setParent} className="cme__cm-host" />
        <textarea
          className={`cme__mirror ${mirrorClassName ?? ""}`}
          aria-hidden="true"
          tabIndex={-1}
          spellCheck={false}
          value={value}
          onChange={(e) => onChange(e.target.value)}
        />
      </div>
    );
  },
);

export default CodeMirrorInput;
