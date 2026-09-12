import { Link } from "react-router";
import type { CommitSummary } from "../../api/types";
import EmptyState from "../EmptyState";
import { Icon } from "../icons";
import { commitUrl } from "../../lib/codeUrl";
import { currentHistoryIndex } from "../../lib/historyStep";
import { relativeTime, shortSha } from "../../lib/format";

export interface HistoryPanelProps {
  repo: string;
  entries: CommitSummary[];
  /// The URL's `?ref=` sha, or `undefined` for the working tree.
  currentRef: string | undefined;
  /// Navigate the open file to `sha` (`undefined` = the working tree) —
  /// "time travel in place," same as the `[c`/`]c` vim binding.
  onNavigate: (sha: string | undefined) => void;
  /// Phase C7 — open story mode ("watch this file being made") for the open
  /// file. `sha` (a specific row's own commit) starts the player pinned
  /// there; omitted (the header button) starts at the file's OLDEST
  /// commit. `undefined` when there's no file open for a story to replay
  /// (mirrors `onNavigate`'s own optionality one layer up — see
  /// `Reader.tsx`).
  onOpenStory?: (sha?: string) => void;
  isLoading: boolean;
  truncated: boolean;
}

/// The reader's History inspector tab (Wave C, InspectorRail's 4th icon):
/// `file-history/1`'s entries for the open file, newest-first, plus a
/// virtual "working tree" row at the top. The CURRENT position is
/// highlighted (`lib/historyStep.ts`'s `currentHistoryIndex`, the exact
/// same logic `[c`/`]c` steps against). Clicking a row's subject/time
/// area time-travels in place (`onNavigate`); clicking the sha specifically
/// opens the commit page instead — two separate elements (never an anchor
/// nested inside a button), each with its own destination.
///
/// Phase C7 adds the tab's story-mode entry points: a header "▶ Story"
/// button (plays from the oldest commit) and, per row, a small "▶" that
/// starts the player pinned to THAT commit — both only rendered once there's
/// at least one entry (nothing to play otherwise).
export default function HistoryPanel({
  repo,
  entries,
  currentRef,
  onNavigate,
  onOpenStory,
  isLoading,
  truncated,
}: HistoryPanelProps) {
  if (isLoading) {
    return <div className="kbc-inspector__hint">Loading history…</div>;
  }

  const curIndex = currentHistoryIndex(entries, currentRef);
  const atWorkingTree = curIndex === -1 && currentRef === undefined;

  return (
    <div className="kbc-history" data-kbc-history-panel>
      {entries.length > 0 && onOpenStory && (
        <div className="kbc-history__head">
          <button
            type="button"
            className="kbc-history__story-btn"
            onClick={() => onOpenStory()}
            title="Watch this file being made, commit by commit"
            data-kbc-history-story
          >
            ▶ Story
          </button>
        </div>
      )}
      <ul className="kbc-history__list">
        <li
          className={"kbc-history__entry" + (atWorkingTree ? " is-current" : "")}
          data-kbc-history-entry="working-tree"
        >
          <button
            type="button"
            className="kbc-history__row-btn"
            onClick={() => onNavigate(undefined)}
            data-kbc-history-goto="working-tree"
          >
            <span className="kbc-history__sha kbc-history__sha--worktree">working tree</span>
            {atWorkingTree && (
              <span className="kbc-history__current-badge" data-kbc-history-current>
                current
              </span>
            )}
          </button>
        </li>
        {entries.map((e, i) => {
          const current = i === curIndex;
          return (
            <li
              key={e.sha}
              className={"kbc-history__entry" + (current ? " is-current" : "")}
              data-kbc-history-entry={e.sha}
            >
              <Link
                to={commitUrl(repo, e.sha)}
                className="kbc-history__sha"
                title="Open commit page"
                data-kbc-history-commit-link={e.sha}
              >
                {shortSha(e.sha)}
              </Link>
              <button
                type="button"
                className="kbc-history__row-btn"
                onClick={() => onNavigate(e.sha)}
                data-kbc-history-goto={e.sha}
              >
                <span className="kbc-history__subject">{e.subject}</span>
                <span className="kbc-history__time">{relativeTime(e.author_time)}</span>
              </button>
              {current && (
                <span className="kbc-history__current-badge" data-kbc-history-current>
                  current
                </span>
              )}
              {onOpenStory && (
                <button
                  type="button"
                  className="kbc-history__row-story"
                  onClick={() => onOpenStory(e.sha)}
                  title="Watch the story starting here"
                  aria-label={`watch the story starting at ${shortSha(e.sha)}`}
                  data-kbc-history-story-row={e.sha}
                >
                  ▶
                </button>
              )}
            </li>
          );
        })}
      </ul>
      {truncated && <div className="kbc-history__truncated">Older history truncated.</div>}
      {entries.length === 0 && (
        <EmptyState
          variant="rail"
          icon={<Icon.Clock />}
          title="No history yet"
          hint="This file has no commits — it's new, or exists only in the working tree."
        />
      )}
    </div>
  );
}
