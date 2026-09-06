import { memo } from "react";
import type { DocSummary } from "../api/client";
import Card from "./Card";
import ListRow from "./ListRow";
import { useUrl } from "../hooks/useUrl";
import type { Progress } from "../hooks/useReadingProgress";
import type { SessionRow } from "../api/sessions";

// GallerySection — one folder-bucket in the grouped gallery view
// (v0.8 G1). Renders a header strip (folder path • count, clickable
// to filter to that folder) then a nested grid/list of cards/rows.
//
// `view` controls the inner layout. Atlas view never reaches here —
// Gallery hides the Group control on atlas to make this obvious.

type Props = {
  folder: string;
  docs: DocSummary[];
  kb: string;
  view: "grid" | "list";
  /// G5 — per-artifact reading progress map (from useReadingProgress).
  /// Forwarded to each Card; absent means "no progress for this doc".
  progress?: Map<string, Progress>;
  /// Sessions-gallery join — artifact_id → SessionRow; Cards for session
  /// transcripts render the session-shaped body. Absent off session corpora.
  sessions?: Map<string, SessionRow>;
};

// Wrapped in React.memo: gallery.tsx memoizes the `sections` array
// (keyed on filtered/group/view/sort/dir), so each section's `docs`
// reference is stable across parent re-renders that don't change
// content (e.g. recomputeTick bumps, sibling state). The default
// shallow comparator then skips re-rendering this whole folder bucket
// — all props (folder/docs/kb/view/progress) are referentially stable
// when content is unchanged.
function GallerySection({ folder, docs, kb, view, progress, sessions }: Props) {
  const { set } = useUrl();
  const label = folder === "" ? "(root)" : folder;
  return (
    <section className="gallery-section">
      <header className="gallery-section__h">
        <button
          className="gallery-section__title"
          onClick={() => set("folder", folder === "" ? null : folder)}
          title="filter to this folder"
        >
          <span className="gallery-section__name">{label}</span>
          <span className="gallery-section__sep">•</span>
          <span className="gallery-section__count">{docs.length}</span>
        </button>
      </header>
      {view === "grid" ? (
        <div className="grid grid--in-section">
          {docs.map((d) => (
            <Card
              key={d.id}
              doc={d}
              kb={kb}
              progress={progress?.get(d.id)}
              session={sessions?.get(d.id)}
            />
          ))}
        </div>
      ) : (
        <div className="list list--in-section">
          {docs.map((d) => (
            <ListRow key={d.id} doc={d} kb={kb} />
          ))}
        </div>
      )}
    </section>
  );
}

export default memo(GallerySection);
