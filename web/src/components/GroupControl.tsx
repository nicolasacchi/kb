import type { GroupKey } from "../lib/sort";

// GroupControl — gallery header toggle for grouping mode. Off renders
// a flat grid/list; "folder" buckets cards by their relative folder
// path. Hidden by Gallery.tsx when atlas view is active (grouping a
// 2-D scatter is meaningless).

type Props = {
  group: GroupKey;
  onGroup: (group: GroupKey) => void;
};

export default function GroupControl({ group, onGroup }: Props) {
  return (
    <div className="group-control" role="group" aria-label="group by">
      <label className="group-control__label">group</label>
      <div className="group-control__opts">
        <button
          className={`group-control__opt ${group === "none" ? "is-on" : ""}`}
          aria-pressed={group === "none"}
          onClick={() => onGroup("none")}
        >
          none
        </button>
        <button
          className={`group-control__opt ${group === "folder" ? "is-on" : ""}`}
          aria-pressed={group === "folder"}
          onClick={() => onGroup("folder")}
        >
          folder
        </button>
      </div>
    </div>
  );
}
