// V71-D2 — the live preview on the focused hit, peek-first (D5).
//
// Three properties, all deliberate:
//
//   - **It never takes keyboard focus.** D5's rule is that a preview is a
//     peek: the cursor stays in the query box, `Down`/`Up` keep walking
//     rows, and nothing here can swallow a key. The CM6 buffer is mounted
//     read-only with NO vim callbacks, exactly as `Lens.tsx`'s own
//     `LensCodeView` does.
//   - **It never navigates.** Opening is `Enter` on the row (the Ramp), and
//     candidate/likely never auto-navigate anyway (D5).
//   - **It is debounced by the caller.** `GET /api/file` bumps the store
//     generation on every successful read (recon R1: "every file open
//     invalidates both search caches, globally"), so previewing on every
//     arrow keypress would rebuild the files/symbols snapshots between
//     keystrokes. The page debounces the focused row before handing it
//     here, and `Alt-p` turns the pane off entirely — `consult`'s
//     `:preview-key` lesson for an expensive previewer.

import CodeView, { type GotoSel } from "../CodeView";
import { useFile } from "../../hooks/useFile";

export interface SearchPreviewProps {
  repo: string | null;
  path: string | null;
  line?: number;
}

export default function SearchPreview({ repo, path, line }: SearchPreviewProps) {
  const file = useFile(repo ?? undefined, path ?? undefined, undefined);
  // `nonce` is what makes CM6 re-scroll when the SAME line is re-selected
  // after a detour; the line number is a fine, deterministic value for it.
  const gotoSel: GotoSel | null = line ? { start: line, end: line, nonce: line } : null;

  return (
    <aside className="kbc-searchpreview" data-kbc-role="search-preview" aria-label="preview">
      <header className="kbc-searchpreview__head">
        {path ? (
          <span className="kbc-searchpreview__path" title={`${repo} · ${path}`}>
            {path}
            {line ? `:${line}` : ""}
          </span>
        ) : (
          <span className="kbc-searchpreview__path kbc-searchpreview__path--none">no file focused</span>
        )}
      </header>
      <div className="kbc-searchpreview__body">
        {!path || !repo ? (
          <p className="kbc-reader__hint">Move the cursor onto a file hit to preview it.</p>
        ) : file.isLoading ? (
          <p className="kbc-reader__hint">Loading…</p>
        ) : file.error ? (
          <p className="kbc-reader__hint kbc-reader__hint--error">{(file.error as Error).message}</p>
        ) : !file.data ? null : file.data.encoding !== "utf8" ? (
          <p className="kbc-reader__hint">Binary file — no preview.</p>
        ) : (
          <CodeView
            content={file.data.content}
            spans={file.data.highlights}
            blobHash={file.data.blob_hash}
            gotoSel={gotoSel}
          />
        )}
      </div>
    </aside>
  );
}
