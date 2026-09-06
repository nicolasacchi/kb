// DCB W2.B — the lens page's row/group keyboard nav. Co-located with the
// other page-level key hooks (not inside `components/lens/`, since this is
// Lens.tsx-only wiring, not a rendered component).

import { useEffect } from "react";
import type { CodeLensGroup, CodeLensRef } from "../api/types";
import type { GroupSelection } from "../components/lens/GroupRail";
import { refsForGroup, stepGroup } from "../lib/docLensUrl";

/// Duplicated locally (2-line pure functions) rather than imported from
/// `Reader.tsx` — the codebase's established convention is a local copy per
/// file (`Tour.tsx`'s own `isTypingTarget`; `Reader.tsx`'s own versions are
/// module-private too), not a shared `lib/` export.
function isEditableTarget(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  return !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
}

/// True when the event originated inside the CM6 buffer — `LensCodeView`
/// embeds one, and `vimReader` owns every key there (even though the lens's
/// own `CodeView` is mounted with no `vim` callbacks, CM6's default keymap
/// + its own internal key handling still lives inside `.kbc-codeview`, so
/// this page-level listener must stay out of its way exactly like
/// `Reader.tsx`'s own window-level handler does).
function isInsideBuffer(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  return !!t && typeof t.closest === "function" && t.closest(".kbc-codeview") !== null;
}

export interface UseLensKeysOpts {
  groups: CodeLensGroup[];
  /// `lens.data.ungrouped_count` — see `docLensUrl.ts`'s `stepGroup` doc for
  /// why this is a count, never a `refs` scan (R9).
  ungroupedCount: number;
  refs: CodeLensRef[];
  selectedGroup: GroupSelection;
  setSelectedGroup: (k: GroupSelection) => void;
  selectedRefOrdinal: number | null;
  setSelectedRefOrdinal: (o: number | null) => void;
}

/// `j`/`k` for ref-row nav (reused, zero new surface), `(`/`)` for group nav
/// (new — chosen for zero vocabulary overlap with `editor/vimKeys.ts`; see
/// the W2.B spec §5 for the alternatives considered and rejected: bare
/// `[`/`]` are TAKEN as vimReader chord prefixes, `Ctrl+j`/`Ctrl+k` collide
/// with the browser chrome, `n`/`p`/`{`/`}`/PageUp/PageDown/`J`/`K` all
/// collide with or are visually confusable with existing bindings). Both
/// gated by the SAME `isInsideBuffer`/`isEditableTarget` guard
/// `Reader.tsx` already establishes as house convention for a page-level
/// `window` keydown listener coexisting with an always-vim-active
/// `CodeView`.
export function useLensKeys(opts: UseLensKeysOpts): void {
  const {
    groups,
    ungroupedCount,
    refs,
    selectedGroup,
    setSelectedGroup,
    selectedRefOrdinal,
    setSelectedRefOrdinal,
  } = opts;

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target)) return;
      if (isInsideBuffer(e.target)) return; // vimReader owns the buffer's keys
      const rows = refsForGroup(refs, selectedGroup);
      const idx = rows.findIndex((r) => r.ordinal === selectedRefOrdinal);
      switch (e.key) {
        case "j": {
          e.preventDefault();
          const next = idx === -1 ? rows[0] : rows[Math.min(idx + 1, rows.length - 1)];
          setSelectedRefOrdinal(next?.ordinal ?? null);
          break;
        }
        case "k": {
          e.preventDefault();
          const next = idx === -1 ? rows[0] : rows[Math.max(idx - 1, 0)];
          setSelectedRefOrdinal(next?.ordinal ?? null);
          break;
        }
        case ")":
          e.preventDefault();
          setSelectedGroup(stepGroup(groups, ungroupedCount, selectedGroup, 1));
          break;
        case "(":
          e.preventDefault();
          setSelectedGroup(stepGroup(groups, ungroupedCount, selectedGroup, -1));
          break;
        default:
          break;
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [groups, ungroupedCount, refs, selectedGroup, setSelectedGroup, selectedRefOrdinal, setSelectedRefOrdinal]);
}
