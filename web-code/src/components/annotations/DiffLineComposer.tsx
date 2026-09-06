import { useState } from "react";
import { useCreateAnnotation } from "../../hooks/useAnnotations";
import { toast } from "../../lib/toast";

export interface DiffLineComposerProps {
  repo: string;
  path: string;
  /// The commit this comment pins to — the Commit page's own sha, or a
  /// compare's `to` side (three-dot too — it's the right side); always a
  /// full, already-resolved sha (`FileChangeRow`'s own `to` prop, reused
  /// unchanged, see that component's doc).
  sha: string;
  /// The line's position in `sha`'s OWN blob (the diff's NEW-side gutter
  /// number) — never re-resolved once posted, see `crate::annotations`'s
  /// `diff` doc.
  line: number;
  onDone: () => void;
}

/// The "Comment at this commit" inline composer a NEW-side diff line
/// number opens (Commit/Compare pages, Phase D deliverable 4). POSTs an
/// `anchor_kind: "diff"` annotation — deliberately tiny (a body box + Save/
/// Cancel), this is a comment strip, not a review UI (Wave G builds that).
/// Reuses the SAME `annotationsQueryKey(repo, path)` TanStack Query cache
/// the reader's own `AnnotationsPanel` reads (`useCreateAnnotation`'s
/// `onSuccess` invalidates it) — a comment left here shows up there too,
/// and `DiffFileAnnotations`' thread list below refreshes from the same
/// write, no second index.
export default function DiffLineComposer({ repo, path, sha, line, onDone }: DiffLineComposerProps) {
  const [body, setBody] = useState("");
  const create = useCreateAnnotation(repo, path);

  async function save() {
    const trimmed = body.trim();
    if (!trimmed) return;
    try {
      await create.mutateAsync({ repo, path, line, anchor_kind: "diff", sha, body: trimmed });
      onDone();
    } catch (e) {
      toast.err(`couldn't save comment: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  return (
    <div className="kbc-diffcomment-composer" data-kbc-diffcomment-composer={line}>
      <div className="kbc-diffcomment-composer__label">Comment at this commit — line {line}</div>
      <textarea
        className="kbc-diffcomment-composer__body"
        placeholder="What's worth flagging about this line?"
        value={body}
        onChange={(e) => setBody(e.target.value)}
        rows={2}
        aria-label="diff comment body"
        autoFocus
        data-kbc-diffcomment-body
      />
      <div className="kbc-diffcomment-composer__actions">
        <button
          type="button"
          onClick={() => void save()}
          disabled={!body.trim() || create.isPending}
          data-kbc-diffcomment-save
        >
          {create.isPending ? "Saving…" : "Comment"}
        </button>
        <button type="button" onClick={onDone} data-kbc-diffcomment-cancel>
          Cancel
        </button>
      </div>
    </div>
  );
}
