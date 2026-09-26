import { useReviews } from "../../hooks/useReviews";
import { useReviewFileBindHint } from "../../hooks/useReviewComments";
import { reviewBindHintText } from "../../lib/reviewComments";

export interface ReviewBindSelectorProps {
  repo: string;
  /// The file this comment lives on — feeds the `in_diff` hint. `""` for
  /// a path-less composer (none exists yet in this crate, but the prop
  /// stays honest about the case rather than assuming a real path).
  path: string;
  /// `null` = "No review".
  value: number | null;
  onChange: (reviewId: number | null) => void;
}

/// V80-M2 — the "Review" selector shared by `AnnotationsPanel`'s composer
/// (a top-level create, or a bind/rebind picker on an existing card) and
/// `DiffLineComposer`'s (Commit/Compare pages). Lists the repo's OPEN
/// reviews (`useReviews(repo, "open")`) and, under the `<select>`, a hint
/// line: "will appear in the Room of <title>" — the default, whenever the
/// selection resolves in-diff or the answer isn't known yet — or "not in
/// this review's diff — anchors to ps N's tip" once
/// [`useReviewFileBindHint`] positively knows it doesn't.
export default function ReviewBindSelector({ repo, path, value, onChange }: ReviewBindSelectorProps) {
  const openReviews = useReviews(repo, "open");
  const reviews = openReviews.data?.reviews ?? [];
  const selected = value !== null ? reviews.find((r) => r.id === value) : undefined;
  const hint = useReviewFileBindHint(repo, value ?? undefined, path);

  const hintText = reviewBindHintText(value, selected?.title ?? undefined, hint.inDiff, hint.ps);

  return (
    <div className="kbc-annotations__review-select">
      <select
        className="kbc-annotations__review-picker"
        aria-label="bind to review"
        value={value === null ? "" : String(value)}
        onChange={(e) => onChange(e.target.value === "" ? null : Number(e.target.value))}
        data-kbc-annot-review-select
      >
        <option value="">No review</option>
        {/* A bound review that has since closed (or otherwise isn't in
            the currently-open list) still gets an option, rather than the
            `<select>` silently coercing the choice to something else —
            saving unchanged then surfaces the server's own honest 409 on
            a closed target, never a client-side guess. */}
        {value !== null && !selected && <option value={value}>{`Review #${value}`}</option>}
        {reviews.map((r) => (
          <option key={r.id} value={r.id}>
            {r.title ?? `Review #${r.id}`}
          </option>
        ))}
      </select>
      {hintText && (
        <div className="kbc-annotations__review-hint" data-kbc-annot-review-hint>
          {hintText}
        </div>
      )}
    </div>
  );
}
