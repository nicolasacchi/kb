import type { StoryEntry } from "../../api/types";
import { useStory } from "../../hooks/useStory";
import { formatUnixSeconds } from "../../lib/format";
import { gapBeatLabel, isGapBeat } from "../../lib/storyBeats";

export interface StoryTimelineProps {
  repo: string;
  path: string;
}

function beatKey(e: StoryEntry, i: number): string {
  return e.session_id ?? e.sha ?? `gap-${e.first_seen}-${i}`;
}

/// The provenance tab's "File story" section (CT-E2): `GET /api/story`'s
/// chronological session timeline for the OPEN file, rendered below the
/// per-line `WhyPanel` (see `routes/Reader.tsx`'s `whyPanel` — this only
/// mounts while the provenance tab is actually open, which is what gates
/// the expensive fetch, `hooks/useStory.ts`). Covered beats render as
/// compact session rows; a `status: "gap"` attention-gap beat renders as a
/// MUTED DIVIDER row — count + date range, copy from `lib/storyBeats.ts` —
/// so a stretch of history no captured session records reads as an explicit
/// beat instead of silent absence. Fetch failures degrade to a quiet
/// "unavailable" hint (the panel above stands on its own), never an error
/// surface.
export default function StoryTimeline({ repo, path }: StoryTimelineProps) {
  const story = useStory(repo, path);
  const entries = story.data?.entries ?? [];

  return (
    <div className="kbc-story-timeline" data-kbc-story-timeline>
      <div className="kbc-story-timeline__title">File story</div>
      {story.isLoading && <div className="kbc-story-timeline__hint">Loading story…</div>}
      {story.isError && <div className="kbc-story-timeline__hint">Story unavailable.</div>}
      {story.data && entries.length === 0 && (
        <div className="kbc-story-timeline__hint">No history.</div>
      )}
      {entries.length > 0 && (
        <ul className="kbc-story-timeline__list">
          {entries.map((e, i) =>
            isGapBeat(e) ? (
              <li
                key={beatKey(e, i)}
                className="kbc-story-timeline__gap"
                data-kbc-story-gap={e.reason ?? "join-unavailable"}
                title={e.subject ?? undefined}
              >
                <span className="kbc-story-timeline__gap-label">{gapBeatLabel(e)}</span>
              </li>
            ) : (
              <li key={beatKey(e, i)} className="kbc-story-timeline__beat" data-kbc-story-beat>
                <span className="kbc-story-timeline__when">{formatUnixSeconds(e.first_seen)}</span>
                <span className="kbc-story-timeline__label">
                  {e.display_name ?? e.subject ?? e.session_id}
                </span>
                <span className="kbc-story-timeline__meta">
                  {e.status} · {e.confidence} · {e.lines_touched} line
                  {e.lines_touched === 1 ? "" : "s"}
                </span>
              </li>
            ),
          )}
        </ul>
      )}
    </div>
  );
}
