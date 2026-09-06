import { forwardRef, lazy, Suspense } from "react";
import type {
  MarkdownEditorHandle,
  MarkdownEditorProps,
} from "./MarkdownEditor";

// P2 — defer CodeMirror 6 (+ lezer, a ~530 KB chunk) until a composer actually
// mounts. The detail route statically imports CommentsPanel, which statically
// imported MarkdownEditor → pulling CM into the reader's critical path even
// though most reads never open a composer. Routing the three composer surfaces
// (CommentsPanel / CommentModal / NoteEditor) through this lazy wrapper keeps CM
// out of every page that merely MIGHT show one; it loads on first composer open.
//
// The wrapper re-exposes the forwardRef MarkdownEditorHandle contract
// (insertAtCursor / focusWrite) — React.lazy forwards refs to a forwardRef
// component — so call sites are unchanged (same default import name, same ref).
const MarkdownEditor = lazy(() => import("./MarkdownEditor"));

const LazyMarkdownEditor = forwardRef<MarkdownEditorHandle, MarkdownEditorProps>(
  function LazyMarkdownEditor(props, ref) {
    return (
      <Suspense
        // Inline-sized placeholder (no editor.css dependency — that ships in the
        // lazy chunk) so the surrounding panel doesn't collapse during the fetch.
        fallback={<div className="cme-loading" aria-busy="true" style={{ minHeight: 120 }} />}
      >
        <MarkdownEditor {...props} ref={ref} />
      </Suspense>
    );
  },
);

export default LazyMarkdownEditor;
export type { MarkdownEditorHandle } from "./MarkdownEditor";
