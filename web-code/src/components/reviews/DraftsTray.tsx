import { Icon } from "../icons";
import { publishRefusal, type ReviewDraft } from "../../lib/reviewDrafts";

export interface DraftsTrayProps {
  open: boolean;
  drafts: readonly ReviewDraft[];
  publishing: boolean;
  onClose: () => void;
  onGoTo: (draft: ReviewDraft) => void;
  onRemove: (id: string) => void;
  onPublish: () => void;
  onDiscardAll: () => void;
}

/// V73-K2a — the drafts tray. Everything composed in the diff sits here,
/// visibly unpublished, until ONE atomic publish
/// (`POST /api/annotations/batch` — one store transaction, at most one
/// SSE). `Discard` clears the tray through the app's shared confirm host,
/// never `window.confirm`.
///
/// The tray states its own storage in the footer rather than in a comment
/// nobody reading the screen can see: drafts are per-tab, and closing the
/// tab drops them. That is a real limitation and the reviewer is entitled
/// to know it before writing eight comments.
export default function DraftsTray({
  open,
  drafts,
  publishing,
  onClose,
  onGoTo,
  onRemove,
  onPublish,
  onDiscardAll,
}: DraftsTrayProps) {
  if (!open) return null;
  const refusal = publishRefusal(drafts);
  return (
    <aside className="kbc-drafts" aria-label="Review drafts" data-kbc-rdiff-drafts>
      <header className="kbc-drafts__head">
        <span className="kbc-drafts__title">
          Drafts <span data-kbc-rdiff-drafts-count>{drafts.length}</span>
        </span>
        <button
          type="button"
          className="kbc-drafts__close"
          onClick={onClose}
          aria-label="close the drafts tray"
          data-kbc-rdiff-drafts-close
        >
          <Icon.X />
        </button>
      </header>
      {drafts.length === 0 ? (
        <p className="kbc-drafts__empty" data-kbc-rdiff-drafts-empty>
          Nothing drafted yet. Comments and questions you compose in the diff land here until you
          publish them.
        </p>
      ) : (
        <ul className="kbc-drafts__list">
          {drafts.map((d) => (
            <li key={d.id} className="kbc-drafts__item" data-kbc-rdiff-draft={d.id}>
              <button
                type="button"
                className="kbc-drafts__where"
                onClick={() => onGoTo(d)}
                title="jump to this draft's line"
                data-kbc-rdiff-draft-goto={d.id}
              >
                <span className="kbc-drafts__path">{d.path}</span>
                <span className="kbc-drafts__loc">
                  {d.side} {d.line}
                  {d.lineEnd !== undefined && d.lineEnd > d.line ? `–${d.lineEnd}` : ""}
                </span>
              </button>
              <span
                className={
                  "kbc-drafts__intent" +
                  (d.intent === "question" ? " kbc-drafts__intent--question" : "")
                }
                data-kbc-rdiff-draft-intent={d.intent}
              >
                {d.intent}
              </span>
              {d.suggestion !== undefined && (
                <span className="kbc-drafts__intent" data-kbc-rdiff-draft-suggestion>
                  suggestion
                </span>
              )}
              <p className="kbc-drafts__body">{d.body}</p>
              <button
                type="button"
                className="kbc-drafts__drop"
                onClick={() => onRemove(d.id)}
                aria-label={`discard the draft on ${d.path}`}
                data-kbc-rdiff-draft-drop={d.id}
              >
                <Icon.X />
              </button>
            </li>
          ))}
        </ul>
      )}
      <footer className="kbc-drafts__foot">
        <button
          type="button"
          className="kbc-review__action kbc-review__action--primary"
          onClick={onPublish}
          disabled={refusal !== null || publishing}
          title={refusal ?? "Publish every draft in ONE transaction (Space W)"}
          data-kbc-rdiff-drafts-publish
        >
          {publishing ? "Publishing…" : `Publish ${drafts.length}`}
        </button>
        <button
          type="button"
          className="kbc-review__action"
          onClick={onDiscardAll}
          disabled={drafts.length === 0 || publishing}
          data-kbc-rdiff-drafts-discard
        >
          Discard all
        </button>
        <p className="kbc-drafts__note" data-kbc-rdiff-drafts-note>
          Drafts live in this tab only (sessionStorage) and survive a reload. Publishing sends them
          as one batch — all or nothing.
        </p>
      </footer>
    </aside>
  );
}
