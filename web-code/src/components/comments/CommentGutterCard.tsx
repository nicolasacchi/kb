import type { CommentOut } from "../../api/types";
import { freshnessCaption } from "../../lib/comments";
import "../../styles/comments.css";

export interface CommentGutterCardProps {
  rect: DOMRect;
  comment: CommentOut;
  onJumpSymbol?: () => void;
}

const KIND_LABELS: Record<string, string> = {
  doc: "Doc",
  annotation: "Annotation",
  directive: "Directive",
  section: "Section",
  licence: "Licence",
  generated: "Generated",
  commented_code: "Commented-out code",
  prose: "Prose",
};

/// The comments/1 gutter's hover card (D8: "hover on a marker = a small
/// card — kind, keyword fields incl. smart_todo `on:`/`to:`, state +
/// reason, 'jump to symbol' for doc blocks"). `position: fixed` next to the
/// hovered dot's `getBoundingClientRect()`, same placement idiom
/// `BlameChip` already uses — deliberately outside `CodeView`'s scrolling
/// container so it never clips.
export default function CommentGutterCard({ rect, comment, onJumpSymbol }: CommentGutterCardProps) {
  const fresh = freshnessCaption(comment.state);
  return (
    <div
      className="kbc-comment-card"
      style={{ position: "fixed", left: rect.right + 6, top: Math.max(4, rect.top - 4) }}
      data-kbc-comment-card
      data-kbc-comment-card-kind={comment.kind}
    >
      <div className="kbc-comment-card__head">
        <span className="kbc-comment-card__kind">{KIND_LABELS[comment.kind] ?? comment.kind}</span>
        {comment.keyword && <span className="kbc-comment-card__keyword">{comment.keyword}</span>}
      </div>
      {comment.directive && (
        <div className="kbc-comment-card__directive">
          {comment.directive.tool}
          {comment.directive.has_reason === false && " — no reason given"}
        </div>
      )}
      {comment.fields && (
        <dl className="kbc-comment-card__fields">
          {Object.entries(comment.fields.raw).map(([k, v]) => (
            <div key={k} className="kbc-comment-card__field">
              <dt>{k}:</dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
      )}
      <p className="kbc-comment-card__text">{comment.text}</p>
      {fresh && (
        <div
          className={`kbc-comment-card__state kbc-comment-card__state--${comment.state.state}`}
          data-kbc-comment-card-state={comment.state.state}
        >
          {fresh}
        </div>
      )}
      {comment.symbol && onJumpSymbol && (
        <button
          type="button"
          className="kbc-comment-card__jump"
          onClick={onJumpSymbol}
          data-kbc-comment-card-jump-symbol
        >
          jump to {comment.symbol.name}
        </button>
      )}
    </div>
  );
}
