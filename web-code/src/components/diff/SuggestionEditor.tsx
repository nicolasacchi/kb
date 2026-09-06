import { useEffect, useMemo, useRef, useState } from "react";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import type { ReviewComment } from "../../api/types";
import { useFile } from "../../hooks/useFile";
import {
  sliceAnchoredLines,
  splitSuggestionLines,
  synthesizeSuggestionDiff,
  threadAcceptsSuggestion,
} from "../../lib/suggestions";
import { toast } from "../../lib/toast";
import UnifiedHunks from "./UnifiedHunks";

export interface SuggestionEditorProps {
  repo: string;
  thread: ReviewComment;
  /// When set, seed the buffer from an existing suggestion rather than
  /// the anchored tip-blob slice.
  existingReplacement?: string;
  onSave: (replacement: string) => Promise<void>;
  onCancel: () => void;
}

/// In-thread CM6 suggestion editor + live unified preview.
/// Rendered only for new-side, non-orphaned, `line|range` threads —
/// old-side / orphaned render nothing.
export default function SuggestionEditor({
  repo,
  thread,
  existingReplacement,
  onSave,
  onCancel,
}: SuggestionEditorProps) {
  const eligible = threadAcceptsSuggestion(thread);
  const sha = thread.resolution.resolved_against.sha;
  const start = thread.resolution.line ?? 1;
  const end = thread.resolution.line_end ?? start;
  const file = useFile(eligible ? repo : undefined, eligible ? thread.path : undefined, sha);

  const originalLines = useMemo(() => {
    if (thread.suggestion) return splitSuggestionLines(thread.suggestion.original);
    if (file.data?.encoding === "utf8") {
      return sliceAnchoredLines(file.data.content, start, end);
    }
    return [];
  }, [thread.suggestion, file.data, start, end]);

  const seed = existingReplacement ?? originalLines.join("\n");
  const [draft, setDraft] = useState(seed);
  const [busy, setBusy] = useState(false);

  // Reset the draft if the seed identity changes (open-for-edit vs
  // open-for-create), not on every originalLines identity.
  const seedKey = existingReplacement ?? `orig:${originalLines.join("\n")}`;
  useEffect(() => {
    setDraft(seed);
    // seedKey is the identity we care about; `seed` is derived from it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seedKey]);

  const parsed = useMemo(
    () => synthesizeSuggestionDiff(originalLines, draft, start),
    [originalLines, draft, start],
  );

  async function save() {
    if (busy) return;
    setBusy(true);
    try {
      await onSave(draft);
    } catch (e) {
      toast.err(`couldn't save suggestion: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  const loading = file.isLoading && !thread.suggestion;
  const loadErr = file.error && !thread.suggestion;

  if (!eligible) return null;

  return (
    <div className="kbc-suggestion" data-kbc-suggestion-editor={thread.id}>
      {loading && <p className="kbc-suggestion__hint">Loading anchored lines…</p>}
      {loadErr && (
        <p className="kbc-suggestion__hint kbc-suggestion__hint--err">
          Couldn&apos;t load the tip blob to seed the editor.
        </p>
      )}
      {!loading && <SuggestionMirror seed={seed} onChange={setDraft} />}
      <div className="kbc-suggestion__preview kbc-diff" data-kbc-suggestion-preview>
        <UnifiedHunks path={thread.path} parsed={parsed} />
      </div>
      <div className="kbc-suggestion__actions">
        <button
          type="button"
          onClick={() => void save()}
          disabled={busy}
          data-kbc-suggestion-save={thread.id}
        >
          {busy ? "Saving…" : "Save"}
        </button>
        <button type="button" onClick={onCancel} disabled={busy} data-kbc-suggestion-cancel={thread.id}>
          Cancel
        </button>
      </div>
    </div>
  );
}

/// Uncontrolled CM6 embed. Recreated when `seed` changes (open / re-open).
/// Line numbers off; plain mono via the existing `.kbc-codeview` theme.
function SuggestionMirror({ seed, onChange }: { seed: string; onChange: (next: string) => void }) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  useEffect(() => {
    const parent = hostRef.current;
    if (!parent) return;
    const view = new EditorView({
      parent,
      state: EditorState.create({
        doc: seed,
        extensions: [
          history(),
          keymap.of([...defaultKeymap, ...historyKeymap]),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) onChangeRef.current(update.state.doc.toString());
          }),
          EditorView.theme({
            "&": { fontSize: "var(--fs-md)" },
            ".cm-scroller": { fontFamily: "var(--font-mono, monospace)" },
            ".cm-content": { fontFamily: "var(--font-mono, monospace)" },
          }),
        ],
      }),
    });
    return () => view.destroy();
  }, [seed]);

  return <div className="kbc-codeview kbc-suggestion__cm" ref={hostRef} data-kbc-suggestion-cm />;
}
