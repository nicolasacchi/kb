// `kbc-pseudo/1` (V73-K2c, design D9's chapter zero) — a review's four
// pseudo-files (`~review/pr-body.md`, `review.md`, `findings.json`,
// `commits.md`), rendered in the diff center as a READ-ONLY buffer.
//
// Nothing is stored server-side and there is deliberately NO revision
// chain (`docs/kb-code.md`'s own wording: "the same line, moved" is not a
// thing that happened here) — this view names that honestly rather than
// implying a diff exists. Comment threads attach through the EXISTING
// review-comments path (`useReviewDiffComments`, the same hook the real
// diff uses) — the server's ladder already resolves anchors against pseudo
// bytes (`review_pseudo::resolve_on_pseudo`), so this is a plain-file
// rendering of the SAME thread system, not a second one: each line is a
// `side: "new"` anchor (a pseudo file has no "old" side to speak of).
import { useMemo, useState } from "react";
import DiffLineComposerV2 from "../diff/DiffLineComposerV2";
import DiffThread from "../diff/DiffThread";
import { useHighlight } from "../../hooks/useHighlight";
import { useReviewPseudoFile } from "../../hooks/useReviews";
import { useReviewDiffComments } from "../../hooks/useReviewComments";
import { paintLine, type PaintedSegment } from "../../lib/diffHighlight";
import { paintSpans } from "../../lib/paintSpans";
import { orphansAt, threadsAt, type DiffCommentsApi } from "../../lib/reviewComments";

export interface PseudoFileViewProps {
  repo: string;
  reviewId: number;
  name: string;
  ps: string;
}

function Painted({ segs, text }: { segs: PaintedSegment[] | undefined; text: string }) {
  const painted = segs && segs.length > 0 ? segs : paintLine(text, undefined);
  if (painted.length === 1 && !painted[0].cls) return <>{text}</>;
  return (
    <>
      {painted.map((s, i) =>
        s.cls ? (
          <span key={i} className={s.cls} data-kbc-hl>
            {s.text}
          </span>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </>
  );
}

function PseudoBuffer({
  content,
  path,
  comments,
  composeLine,
  setComposeLine,
}: {
  content: string;
  path: string;
  comments: DiffCommentsApi | null;
  composeLine: number | null;
  setComposeLine: (n: number | null) => void;
}) {
  const items = useMemo(() => [{ id: "p", lang: null, text: content, path }], [content, path]);
  const { byId } = useHighlight(items);
  const painted = useMemo(() => {
    const r = byId.get("p");
    if (!r || r.tier === "none" || r.spans.length === 0) return null;
    return paintSpans(content, r.spans);
  }, [byId, content]);
  const lines = content.split("\n");
  return (
    <div className="kbc-pseudo__buffer" data-kbc-pseudo-buffer role="textbox" aria-readonly="true">
      {lines.map((text, i) => {
        const line = i + 1;
        const threads = comments ? threadsAt(comments, path, "new", line) : [];
        return (
          <div key={line} className="kbc-pseudo__row" data-kbc-pseudo-line={line}>
            <span className="kbc-pseudo__lineno">{line}</span>
            <span className="kbc-pseudo__text">
              <Painted text={text} segs={painted?.[i]} />
            </span>
            {comments && (
              <button
                type="button"
                className="kbc-pseudo__comment-add"
                onClick={() => setComposeLine(line)}
                title="comment on this line"
                data-kbc-pseudo-comment-add={line}
              >
                +
              </button>
            )}
            {threads.map((t) => (
              <DiffThread key={t.id} thread={t} comments={comments!} />
            ))}
            {composeLine === line && comments && (
              <DiffLineComposerV2
                side="new"
                line={line}
                onSubmit={(body, intent) => comments.onCreate("new", line, undefined, body, intent)}
                onDone={() => setComposeLine(null)}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

export default function PseudoFileView({ repo, reviewId, name, ps }: PseudoFileViewProps) {
  const q = useReviewPseudoFile(repo, reviewId, name, ps, true);
  const path = `~review/${name}`;
  const comments = useReviewDiffComments(repo, reviewId, ps, path, {});
  const [composeLine, setComposeLine] = useState<number | null>(null);

  if (q.isLoading) {
    return <div className="kbc-pseudo kbc-pseudo--loading">Loading {path}…</div>;
  }
  if (!q.data) {
    return (
      <div className="kbc-pseudo kbc-pseudo--absent" data-kbc-pseudo-absent={name}>
        This pseudo-file is not available on this server version.
      </div>
    );
  }
  const file = q.data.file;
  const orphans = comments ? orphansAt(comments, path) : [];

  return (
    <section className="kbc-pseudo" data-kbc-pseudo={path} data-kbc-pseudo-present={file.present}>
      <header className="kbc-pseudo__head">
        <span className="kbc-pseudo__path">{path}</span>
        {file.present ? (
          <span className="kbc-pseudo__blob" title={file.source} data-kbc-pseudo-blob={file.blob_sha}>
            blob {file.blob_sha.slice(0, 10)}
          </span>
        ) : (
          <span className="kbc-pseudo__reason" data-kbc-pseudo-reason>
            {file.reason ?? "not present"}
          </span>
        )}
        <span className="kbc-pseudo__note" data-kbc-pseudo-note>
          regenerated whole on every read — no revision history
        </span>
      </header>
      {file.present && (
        <PseudoBuffer
          content={file.content ?? ""}
          path={path}
          comments={comments}
          composeLine={composeLine}
          setComposeLine={setComposeLine}
        />
      )}
      {orphans.length > 0 && (
        <div className="kbc-pseudo__orphans" data-kbc-pseudo-orphans>
          {orphans.map((t) => (
            <DiffThread key={t.id} thread={t} comments={comments!} orphaned />
          ))}
        </div>
      )}
    </section>
  );
}
