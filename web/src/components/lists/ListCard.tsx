import { Link, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { fetchList, type ListSummary } from "../../api/client";
import { continueTarget, entryTrailHref } from "../../lib/listTrail";

// RL-track — one reading list on the /lists index: title (→ detail),
// kb chip, clamped description, segmented progress bar, "3/7 read ·
// ~38m left", pin/archive toggles, and Continue reading (jumps into the
// first unread entry's trail).

export function ListProgressBar({ list }: { list: ListSummary }) {
  const total = Math.max(1, list.entry_count);
  const seg = (n: number) => `${(n / total) * 100}%`;
  return (
    <div
      className="kb-list-progress"
      role="img"
      aria-label={`${list.read_count} of ${list.entry_count} read`}
    >
      <span
        className="kb-list-progress__read"
        style={{ width: seg(list.read_count) }}
      />
      <span
        className="kb-list-progress__inprog"
        style={{ width: seg(list.in_progress_count) }}
      />
    </div>
  );
}

export function listStatsLine(list: ListSummary): string {
  const counts = `${list.read_count}/${list.entry_count} read`;
  if (list.entry_count === 0) return "empty";
  if (list.remaining_minutes > 0)
    return `${counts} · ~${list.remaining_minutes}m left`;
  return counts;
}

export default function ListCard({
  list,
  onPin,
  onArchive,
}: {
  list: ListSummary;
  onPin: (l: ListSummary, pinned: boolean) => void;
  onArchive: (l: ListSummary, archived: boolean) => void;
}) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const detailHref = `/lists/${encodeURIComponent(list.kb)}/${encodeURIComponent(list.id)}`;

  // Continue reading — the index summary doesn't carry entries, so
  // resolve the first unread target through the (cached) detail fetch.
  const onContinue = async () => {
    try {
      const detail = await queryClient.fetchQuery({
        queryKey: ["list", list.kb, list.id] as const,
        queryFn: ({ signal }) => fetchList(list.kb, list.id, signal),
      });
      const target = continueTarget(detail.entries);
      const href = target && entryTrailHref(list.kb, list.id, target);
      navigate(href ?? detailHref);
    } catch {
      navigate(detailHref);
    }
  };

  const allRead =
    list.entry_count > 0 && list.read_count === list.entry_count;

  return (
    <article className="kb-list-card" data-testid="list-card">
      <header className="kb-list-card__head">
        <Link className="kb-list-card__title" to={detailHref}>
          {list.title}
        </Link>
        <span className="kb-list-card__kb">{list.kb}</span>
        <span className="kb-list-card__spacer" />
        <button
          type="button"
          className={`kb-list-card__act ${list.pinned ? "is-on" : ""}`}
          onClick={() => onPin(list, !list.pinned)}
          aria-pressed={list.pinned}
        >
          {list.pinned ? "unpin" : "pin"}
        </button>
        <button
          type="button"
          className={`kb-list-card__act ${list.archived ? "is-on" : ""}`}
          onClick={() => onArchive(list, !list.archived)}
          aria-pressed={list.archived}
        >
          {list.archived ? "unarchive" : "archive"}
        </button>
      </header>
      {list.description && (
        <p className="kb-list-card__desc">{list.description}</p>
      )}
      <ListProgressBar list={list} />
      <footer className="kb-list-card__foot">
        <span className="kb-list-card__stats">{listStatsLine(list)}</span>
        <button
          type="button"
          className="kb-list-card__continue"
          onClick={() => void onContinue()}
          disabled={list.entry_count === 0 || allRead}
          title={
            allRead
              ? "everything read"
              : "jump to the first unread entry"
          }
        >
          Continue reading
        </button>
      </footer>
    </article>
  );
}
