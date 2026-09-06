// Controlled React ↔ CodeMirror 6 bridge.
//
// The cardinal rule: create the EditorView ONCE (per configKey), emit onChange
// through a ref so it never goes stale, and push EXTERNAL value changes into the
// doc ONLY when they differ from the current doc — otherwise every keystroke
// would re-dispatch a full-doc replace and the cursor would jump. StrictMode is
// handled by the effect's create→destroy→create cycle (DOM node persists).

import { useCallback, useEffect, useRef } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { buildExtensions, type EditorConfig } from "./extensions";

export function useCodeMirror(opts: {
  value: string;
  onChange: (v: string) => void;
  config: EditorConfig;
  /// Recreate the view when this string changes (structural config only —
  /// callbacks are read live via the config's getters, so they stay out of it).
  configKey: string;
  autoFocus?: boolean;
  /// Fired on selection / doc / focus change (drives toolbar active state).
  onUpdate?: (view: EditorView) => void;
}) {
  const { value, onChange, config, configKey, autoFocus, onUpdate } = opts;

  const viewRef = useRef<EditorView | null>(null);
  const parentRef = useRef<HTMLDivElement | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const onUpdateRef = useRef(onUpdate);
  onUpdateRef.current = onUpdate;
  const valueRef = useRef(value);
  valueRef.current = value;
  const configRef = useRef(config);
  configRef.current = config;
  const autoFocusRef = useRef(autoFocus);
  autoFocusRef.current = autoFocus;

  // (Re)create the view on mount / configKey change.
  useEffect(() => {
    const parent = parentRef.current;
    if (!parent) return;
    const view = new EditorView({
      parent,
      state: EditorState.create({
        doc: valueRef.current,
        extensions: [
          EditorView.updateListener.of((u) => {
            if (u.docChanged) onChangeRef.current(u.state.doc.toString());
            if (u.docChanged || u.selectionSet || u.focusChanged) {
              onUpdateRef.current?.(u.view);
            }
          }),
          buildExtensions(configRef.current),
        ],
      }),
    });
    viewRef.current = view;
    onUpdateRef.current?.(view);
    if (autoFocusRef.current) view.focus();
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [configKey]);

  // External value sync — only when the prop genuinely differs from the doc.
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const cur = view.state.doc.toString();
    if (value !== cur) {
      view.dispatch({ changes: { from: 0, to: cur.length, insert: value } });
    }
  }, [value]);

  const setParent = useCallback((node: HTMLDivElement | null) => {
    parentRef.current = node;
  }, []);

  return { setParent, viewRef };
}
