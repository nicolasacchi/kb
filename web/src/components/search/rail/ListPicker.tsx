import { useLists } from "../../../hooks/useLists";

// FS7 — filter results to membership in a reading list. Single-kb only
// (lists + their entries are per-corpus); the rail hides this under
// scope=all. Membership resolves SERVER-SIDE via the `list` param (the
// daemon turns the list into an artifact-id set), so the whole entry set
// never ships to the client.

type Props = {
  activeKb?: string;
  value: string;
  onSelect: (id: string | null) => void;
};

export default function ListPicker({ activeKb, value, onSelect }: Props) {
  const { lists } = useLists();
  const mine = activeKb
    ? lists.filter((l) => l.kb === activeKb && !l.archived)
    : lists.filter((l) => !l.archived);
  if (mine.length === 0) {
    return <div className="kb-search-rail__empty">no reading lists</div>;
  }
  // Pinned first, then by title.
  const sorted = [...mine].sort((a, b) =>
    a.pinned === b.pinned ? a.title.localeCompare(b.title) : a.pinned ? -1 : 1,
  );
  return (
    <select
      className="kb-search-rail__select"
      value={value}
      aria-label="filter by reading list"
      onChange={(e) => onSelect(e.target.value || null)}
    >
      <option value="">any list</option>
      {sorted.map((l) => (
        <option key={l.id} value={l.id}>
          {l.pinned ? "★ " : ""}
          {l.title}
        </option>
      ))}
    </select>
  );
}
