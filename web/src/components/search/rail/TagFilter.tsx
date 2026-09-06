import type { TagSummary } from "../../../api/client";

// FS3 — tristate tag chips for the search rail. Each chip cycles
// none → include → exclude → none. Include is any-of (`tags`); exclude is
// none-of (`exclude_tags`); the parent recomputes both sets and writes
// them in one setMany. Single-kb only — a tag list is per-corpus, so the
// rail hides this section under scope=all. Colour matches the gallery
// sidebar (`hsl(seed % 360, 64%, 64%)`).

type Props = {
  tags: TagSummary[];
  include: ReadonlySet<string>;
  exclude: ReadonlySet<string>;
  onCycle: (name: string) => void;
};

export default function TagFilter({ tags, include, exclude, onCycle }: Props) {
  if (tags.length === 0) {
    return <div className="kb-search-rail__empty">no tags in this corpus</div>;
  }
  return (
    <div className="kb-search-rail__tags">
      {tags.map((t) => {
        const state = include.has(t.name)
          ? "include"
          : exclude.has(t.name)
            ? "exclude"
            : "none";
        const hint =
          state === "include"
            ? "including — click to exclude"
            : state === "exclude"
              ? "excluding — click to clear"
              : `${t.count} artifact${t.count === 1 ? "" : "s"} — click to include`;
        return (
          <button
            key={t.name}
            type="button"
            className="kb-search-rail__tag"
            data-state={state}
            aria-pressed={state !== "none"}
            title={hint}
            onClick={() => onCycle(t.name)}
          >
            <span
              className="kb-search-rail__tagdot"
              style={{ background: `hsl(${t.color_seed % 360}, 64%, 64%)` }}
            />
            <span className="kb-search-rail__tagname">{t.name}</span>
            <span className="kb-search-rail__tagct">{t.count}</span>
          </button>
        );
      })}
    </div>
  );
}
