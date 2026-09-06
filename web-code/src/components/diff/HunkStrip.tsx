import type { DiffLine } from "../../lib/diff";
import { noiseLabelText, type NoiseLabel } from "../../lib/diffNoise";
import { EXPAND_STEP } from "../../lib/diffContext";
import { Icon } from "../icons";

/// V73-K2a — everything ONE hunk of ONE file knows about itself, computed
/// once by `FileDiffBody` and handed to whichever renderer (unified or
/// split) is mounted. Both renderers get the SAME array, index-aligned
/// with `ParsedDiff.hunks`, so a hunk's fold, its viewed mark, its noise
/// labels and its expanded rows cannot differ between the two layouts —
/// which is the whole reason this is data rather than two copies of a
/// derivation.
export interface HunkView {
  /// `lib/diffHunks.ts`'s content address (`kbc-hunkid/1`) — the viewed
  /// key, the `?hunk=` cursor value, and the DOM address.
  id: string;
  index: number;
  header: string;
  additions: number;
  deletions: number;
  /// Server-backed (`review_hunk_viewed`), not a local flag.
  viewed: boolean;
  /// A landed comment/finding thread resolves inside this hunk's span.
  hasThreads: boolean;
  /// Unpublished drafts anchored inside this hunk (browser-local).
  draftCount: number;
  noise: NoiseLabel[];
  /// Rendered rows — the wire's own lines, plus whatever context the dial
  /// and the expand buttons have spliced in from the file at this
  /// patchset. Never synthesised text (`lib/diffContext.ts`'s rule).
  lines: DiffLine[];
  addedBefore: number;
  addedAfter: number;
  moreAbove: boolean;
  moreBelow: boolean;
  collapsed: boolean;
  /// WHY it is collapsed — an operator fold, or the noise dial. Rendered
  /// verbatim on the collapsed strip, because "kb-code hid this and will
  /// not say why" is the failure the noise feature exists to avoid.
  collapsedBy: "fold" | "noise" | null;
}

export interface HunkStripProps {
  view: HunkView;
  /// `true` when this is the cursor's hunk (`j`/`k`, `?hunk=`).
  current: boolean;
  onToggleFold: () => void;
  onToggleViewed: () => void;
  onExpand: (dir: "up" | "down") => void;
  /// Absent on the surfaces with no review behind them (Commit/Compare/
  /// SessionDiff): those render the plain header they always did.
  reviewMode: boolean;
}

/// The per-hunk header strip. Replaces the bare `@@ … @@` text row with
/// the hunk's own state: counts, viewed, threads, drafts, noise chips,
/// fold. Every number here is a count of something on screen or on the
/// wire; nothing is a score.
export default function HunkStrip({
  view,
  current,
  onToggleFold,
  onToggleViewed,
  onExpand,
  reviewMode,
}: HunkStripProps) {
  return (
    <div
      className={
        "kbc-hunkstrip" +
        (current ? " kbc-hunkstrip--current" : "") +
        (view.viewed ? " kbc-hunkstrip--viewed" : "") +
        (view.collapsed ? " kbc-hunkstrip--collapsed" : "")
      }
      data-kbc-hunk={view.id}
      data-kbc-hunk-index={view.index}
      data-kbc-hunk-viewed={view.viewed ? "1" : "0"}
      data-kbc-hunk-collapsed={view.collapsed ? "1" : "0"}
    >
      <button
        type="button"
        className="kbc-hunkstrip__fold"
        onClick={onToggleFold}
        aria-expanded={!view.collapsed}
        title={view.collapsed ? "Unfold this hunk (z o)" : "Fold this hunk (z c)"}
        data-kbc-hunk-fold={view.id}
      >
        {view.collapsed ? <Icon.Expand /> : <Icon.Collapse />}
      </button>
      <span className="kbc-hunkstrip__header">{view.header}</span>
      <span className="kbc-hunkstrip__stats">
        <span className="kbc-review__file-add">+{view.additions}</span>{" "}
        <span className="kbc-review__file-del">−{view.deletions}</span>
      </span>
      {view.hasThreads && (
        <span className="kbc-hunkstrip__chip" title="a comment or finding resolves inside this hunk" data-kbc-hunk-threads>
          threads
        </span>
      )}
      {view.draftCount > 0 && (
        <span
          className="kbc-hunkstrip__chip kbc-hunkstrip__chip--draft"
          title="unpublished drafts anchored in this hunk"
          data-kbc-hunk-drafts={view.draftCount}
        >
          {view.draftCount} draft{view.draftCount === 1 ? "" : "s"}
        </span>
      )}
      {view.noise.map((label) => (
        <span
          key={label.cls}
          className={`kbc-hunkstrip__chip kbc-hunkstrip__chip--noise kbc-noise--${label.cls}`}
          // The RULE, verbatim, plus this hunk's own evidence for it. A
          // label whose reason a reader cannot see is a verdict, and this
          // page issues none.
          title={`${label.rule} — ${label.detail}`}
          data-kbc-hunk-noise={label.cls}
        >
          {noiseLabelText(label.cls)}
        </span>
      ))}
      {(view.addedBefore > 0 || view.addedAfter > 0) && (
        <span className="kbc-hunkstrip__chip" data-kbc-hunk-context>
          +{view.addedBefore + view.addedAfter} context
        </span>
      )}
      {view.collapsed && view.collapsedBy !== null && (
        <span className="kbc-hunkstrip__why" data-kbc-hunk-collapsed-by={view.collapsedBy}>
          {view.collapsedBy === "noise"
            ? "collapsed by the noise dial — still counted, one click to open"
            : "folded"}
        </span>
      )}
      <span className="kbc-hunkstrip__spacer" />
      {!view.collapsed && view.moreAbove && (
        <button
          type="button"
          className="kbc-hunkstrip__expand"
          onClick={() => onExpand("up")}
          title={`Show ${EXPAND_STEP} more lines above (from the file at this patchset)`}
          data-kbc-hunk-expand="up"
        >
          ↑ {EXPAND_STEP}
        </button>
      )}
      {!view.collapsed && view.moreBelow && (
        <button
          type="button"
          className="kbc-hunkstrip__expand"
          onClick={() => onExpand("down")}
          title={`Show ${EXPAND_STEP} more lines below (from the file at this patchset)`}
          data-kbc-hunk-expand="down"
        >
          ↓ {EXPAND_STEP}
        </button>
      )}
      {reviewMode && (
        <label className="kbc-hunkstrip__viewed" title="Mark this hunk viewed (Space h)">
          <input
            type="checkbox"
            checked={view.viewed}
            onChange={onToggleViewed}
            aria-label={view.viewed ? "mark hunk unviewed" : "mark hunk viewed"}
            data-kbc-hunk-viewed-toggle={view.id}
          />
        </label>
      )}
    </div>
  );
}
