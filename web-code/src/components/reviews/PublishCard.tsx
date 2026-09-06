// PRR-U5+U6 (design-ui.md §2 S2 side panel + §2 S5 — publish preview). The
// side-panel entry point: "N marked" + "Preview round →". Marks are the
// ephemeral client-side selection in `lib/publishMarks.ts` (toggled inline
// on `FindingCard`/`FindingRow`'s own foot row) — this card is a thin
// reader over that store, never mutates it directly.
import { useMarkedCount } from "../../lib/publishMarks";

export interface PublishCardProps {
  reviewId: number;
  onOpenPreview: () => void;
}

export default function PublishCard({ reviewId, onOpenPreview }: PublishCardProps) {
  const count = useMarkedCount(reviewId);
  return (
    <section className="kbc-review__card kbc-publish-card" data-kbc-publish-card>
      <h2 className="kbc-review__card-title">
        Publish
        <span className="n" data-kbc-publish-marked-count>
          {count} marked
        </span>
      </h2>
      <button
        type="button"
        className="kbc-btn kbc-btn--primary kbc-publish-card__preview"
        onClick={onOpenPreview}
        disabled={count === 0}
        title={count === 0 ? "mark at least one finding on the Report tab first" : undefined}
        data-kbc-publish-preview-open
      >
        Preview round →
      </button>
    </section>
  );
}
