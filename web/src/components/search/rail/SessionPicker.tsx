import { useSessions } from "../../../hooks/useSessions";
import { relativeAge } from "../../../lib/derive";
import { sessionDisplayName } from "../../../lib/sessionDisplayName";

// FS7 — filter results to a single Claude-Code session. Single-kb only
// (sessions belong to a memory corpus); the rail hides this under
// scope=all. Reuses the cross-kb useSessions cache and filters to the
// active corpus.

type Props = {
  activeKb?: string;
  value: string;
  onSelect: (id: string | null) => void;
  /** When false, skip the sessions page-1 + SSE wiring (section collapsed). */
  enabled?: boolean;
};

export default function SessionPicker({
  activeKb,
  value,
  onSelect,
  enabled = true,
}: Props) {
  // useSessions(folder, query, enabled) — gate so a collapsed rail section
  // never fires /api/sessions just because the search page mounted.
  const { rows } = useSessions(undefined, undefined, enabled);
  const mine = activeKb ? rows.filter((r) => r.kb === activeKb) : rows;
  if (mine.length === 0) {
    return <div className="kb-search-rail__empty">no sessions in this corpus</div>;
  }
  return (
    <select
      className="kb-search-rail__select"
      value={value}
      aria-label="filter by session"
      onChange={(e) => onSelect(e.target.value || null)}
    >
      <option value="">any session</option>
      {mine.map((r) => (
        <option key={r.session_id} value={r.session_id}>
          {sessionDisplayName(r).slice(0, 48)} · {relativeAge(r.started_at)}
        </option>
      ))}
    </select>
  );
}
