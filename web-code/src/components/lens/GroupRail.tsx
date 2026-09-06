// DCB W2.B — the lens page's group rail: one row per `codelens/1` heading
// group (server `ordinal` order, never re-sorted here) plus a fixed "All"
// row at the top and an explicit "Ungrouped · N" trailer at the bottom.

import type { CodeLensGroup } from "../../api/types";

// (reconciled: R9, recheck M20-followthrough) — the ONE place the ungrouped
// SELECTION token's literal value is written, so a grep for it lands on
// this comment. It is a piece of LOCAL REACT STATE naming "the user clicked
// the Ungrouped row," nothing more. It is NEVER serialised, NEVER sent to
// kb-code, NEVER compared against a wire field, and MUST NEVER be minted
// into `groups[]` — R9's rule is that no sentinel exists on either WIRE
// (`refs[].group === null` + the envelope's `ungrouped_count` are the whole
// mechanism). A real group key is always `kb-h-<slug>` (`12-w1c` §11.1), so
// this value cannot collide with one.
export const UNGROUPED_SEL = "__ungrouped__";
/// `null` = "All" (every ref, regardless of group); [`UNGROUPED_SEL`] =
/// ungrouped only; any other string = that group's `key`.
export type GroupSelection = string | null;

export interface GroupRailProps {
  groups: CodeLensGroup[];
  /// (reconciled: R9) drives the "Ungrouped" trailer directly —
  /// `lens.data.ungrouped_count`, never a client-side scan of `refs` for
  /// `group === null`.
  ungroupedCount: number;
  selectedGroup: GroupSelection;
  onSelectGroup: (key: GroupSelection) => void;
}

export default function GroupRail({ groups, ungroupedCount, selectedGroup, onSelectGroup }: GroupRailProps) {
  return (
    <nav className="kbc-lens__rail" aria-label="reference groups" data-kbc-lens-rail>
      <button
        type="button"
        className={`kbc-lens__rail-row${selectedGroup === null ? " is-selected" : ""}`}
        onClick={() => onSelectGroup(null)}
        aria-pressed={selectedGroup === null}
        data-kbc-lens-group="all"
      >
        <span className="kbc-lens__rail-label">All</span>
      </button>
      {/* Server-supplied `ordinal` order — NEVER re-sorted client-side. */}
      {groups.map((g) => (
        <button
          key={g.key}
          type="button"
          className={`kbc-lens__rail-row${selectedGroup === g.key ? " is-selected" : ""}`}
          onClick={() => onSelectGroup(g.key)}
          aria-pressed={selectedGroup === g.key}
          data-kbc-lens-group={g.key}
        >
          <span className="kbc-lens__rail-label">{g.label}</span>
          <span className="kbc-lens__rail-count">{g.ref_count}</span>
        </button>
      ))}
      {ungroupedCount > 0 && (
        <button
          type="button"
          className={`kbc-lens__rail-row kbc-lens__rail-row--ungrouped${
            selectedGroup === UNGROUPED_SEL ? " is-selected" : ""
          }`}
          onClick={() => onSelectGroup(UNGROUPED_SEL)}
          aria-pressed={selectedGroup === UNGROUPED_SEL}
          data-kbc-lens-group={UNGROUPED_SEL}
        >
          <span className="kbc-lens__rail-label">Ungrouped</span>
          <span className="kbc-lens__rail-count">{ungroupedCount}</span>
        </button>
      )}
    </nav>
  );
}
