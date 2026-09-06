import { useEffect, useRef, useState } from "react";
import type { Comment, ReviewFile } from "../api/client";
import { anchorLabel } from "../lib/commentFmt";
import { filterComments, groupByFile } from "./CommentsPanel";

// W2.13 — the errata slip: a comments-dock SKIN, not a new panel home
// (invariant #30). CommentsPanel owns the ON/OFF toggle in its header; this
// component is the sheet itself, mounted by detail.tsx as a sibling of the
// iframe inside `.detail__main` (never inside the sandboxed iframe — #5).
// It renders every OPEN comment as a numbered correction-sheet line —
// number · author · excerpt · anchor label — clicking through to the same
// jump/flash the panel's own rows use (props threaded straight from
// detail.tsx's existing AnnotatorBridge handlers, so there is exactly one
// jump implementation, not a second one).
//
// Resolving a comment — from this panel, the CLI, or another tab — always
// arrives as a whole-file SSE-refetch (useReview invalidates `["review",
// kb, id]`; there is no per-row event, and this component never mutates
// anything itself). The tear-off is purely a LOCAL animation: a comment
// that drops out of the open set is kept rendered with a `--leaving`
// class for one transition, then removed — honoring
// `prefers-reduced-motion` with an instant (unanimated) removal, the
// repo's convention (mobile.css `.kb-pinsp-scrim`, gallery.css view
// transitions, …).

const TEAR_MS = 220;

type DisplayItem = { comment: Comment; leaving: boolean };

export type ErrataSheetProps = {
  /// `null` while `useReview`'s file is still loading — the sheet renders
  /// nothing (not an error) during that window, same as the panel's own
  /// `cp__hint loading…` placeholder tolerates it.
  file: ReviewFile | null;
  activeCommentId: string | null;
  /// Mirrors a panel row's "↗ jump" button — flash the in-page marker.
  onSelect: (commentId: string) => void;
  /// Mirrors a panel row's hover glow. Optional (native-note callers, if
  /// any are ever added, have no in-page marker to glow).
  onHover?: (commentId: string | null) => void;
};

function excerpt(body: string, max = 88): string {
  const flat = body.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

function reducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

/// Same ordering the panel renders under the "open" filter (grouped by
/// file, original append order) — see the export note in CommentsPanel.tsx.
function openInPanelOrder(file: ReviewFile | null): Comment[] {
  if (!file) return [];
  return groupByFile(filterComments(file.comments, "open")).flatMap(
    (g) => g.items,
  );
}

export default function ErrataSheet({
  file,
  activeCommentId,
  onSelect,
  onHover,
}: ErrataSheetProps) {
  const openComments = openInPanelOrder(file);
  const [display, setDisplay] = useState<DisplayItem[]>(() =>
    openComments.map((c) => ({ comment: c, leaving: false })),
  );
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  useEffect(() => {
    const live = timers.current;
    return () => {
      live.forEach((t) => clearTimeout(t));
      live.clear();
    };
  }, []);

  useEffect(() => {
    const nextIds = new Set(openComments.map((c) => c.id));
    const byId = new Map(openComments.map((c) => [c.id, c]));
    const reduced = reducedMotion();
    setDisplay((prev) => {
      const seen = new Set<string>();
      const merged: DisplayItem[] = [];
      for (const item of prev) {
        const id = item.comment.id;
        seen.add(id);
        if (nextIds.has(id)) {
          merged.push({ comment: byId.get(id) ?? item.comment, leaving: false });
        } else if (item.leaving) {
          // Already animating out from a prior tick — keep as-is until its
          // own timer fires.
          merged.push(item);
        } else if (!reduced) {
          merged.push({ comment: item.comment, leaving: true });
          const t = setTimeout(() => {
            timers.current.delete(id);
            setDisplay((d) => d.filter((x) => x.comment.id !== id));
          }, TEAR_MS);
          timers.current.set(id, t);
        }
        // reduced-motion + just-resolved: drop silently, no leaving stage.
      }
      for (const c of openComments) {
        if (!seen.has(c.id)) merged.push({ comment: c, leaving: false });
      }
      return merged;
    });
    // `openComments` is recomputed from `file` every render; keying the
    // effect on `file` (its identity changes on every SSE refetch) is the
    // correct re-sync trigger — see the file-scope comment above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [file]);

  if (display.length === 0) return null;

  return (
    <div className="errata-sheet" role="region" aria-label="errata">
      <div className="errata-sheet__mast">
        <span className="errata-sheet__rule" aria-hidden="true" />
        <span className="errata-sheet__kicker">Errata</span>
        <span className="errata-sheet__rule" aria-hidden="true" />
      </div>
      <ol className="errata-sheet__list">
        {display.map((item, i) => {
          const c = item.comment;
          const cls = [
            "errata-sheet__line",
            item.leaving ? "errata-sheet__line--leaving" : "",
            c.id === activeCommentId ? "errata-sheet__line--active" : "",
          ]
            .filter(Boolean)
            .join(" ");
          return (
            <li key={c.id} className={cls}>
              <button
                type="button"
                data-kb-act="errata-line"
                data-comment-id={c.id}
                className="errata-sheet__btn"
                onClick={() => onSelect(c.id)}
                onMouseEnter={() => onHover?.(c.id)}
                onMouseLeave={() => onHover?.(null)}
              >
                <span className="errata-sheet__num">{i + 1}.</span>
                <span className="errata-sheet__author">{c.author}</span>
                <span className="errata-sheet__excerpt">{excerpt(c.body)}</span>
                <span className="errata-sheet__anchor">{anchorLabel(c)}</span>
              </button>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
