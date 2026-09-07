// DCB W2.B — the lens page's row/group keyboard nav. Co-located with the
// other page-level key hooks (not inside `components/lens/`, since this is
// Lens.tsx-only wiring, not a rendered component).
//
// V73-K6: `j`/`k`/`(`/`)` used to be hard-coded `e.key === …` checks with no
// registry involvement — a second home for `lens.row-next`/`lens.row-prev`/
// `lens.group-next`/`lens.group-prev` (`scope: "board"`,
// `when: "board == lens"`), which shipped in `registry.json` with this exact
// behaviour already but nothing here ever asked the registry what a
// keystroke meant. `onKey` now asks `dispatch.ts`'s `resolve()` — the same
// resolver `CommandRoot` uses — and runs the returned id's handler; see
// `lensCommands.ts` for the declaration↔handler contract.

import { useEffect } from "react";
import type { CodeLensGroup, CodeLensRef } from "../api/types";
import type { GroupSelection } from "../components/lens/GroupRail";
import { resolve as resolveCommand, tokenOf } from "../commands/dispatch";
import { useCommandScope } from "../commands/CommandRoot";
import { refsForGroup, stepGroup } from "../lib/docLensUrl";
import type { LensHandlers } from "./lensCommands";

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

  useCommandScope("board", { board: "lens" });

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target)) return;
      if (isInsideBuffer(e.target)) return; // vimReader owns the buffer's keys
      const token = tokenOf(e);
      const ctx = { board: "lens" as const };
      // `lens.row-next`/`lens.row-prev` also carry a `plain`-preset
      // `ArrowDown`/`ArrowUp` this hook never wired (pre-registry it only
      // ever matched the literal `"j"`/`"k"` characters) — trying `vim` then
      // `plain` picks that up too, regardless of the SPA's live preset
      // setting, the same choice `Browser.tsx`'s onKey makes for the
      // identical reason. `lens.group-next`/`lens.group-prev` have no
      // `plain` key at all, so the fallback is a no-op for them.
      const cmd = resolveCommand(token, "board", ctx, "vim") ?? resolveCommand(token, "board", ctx, "plain");
      if (!cmd) return;
      const rows = refsForGroup(refs, selectedGroup);
      const idx = rows.findIndex((r) => r.ordinal === selectedRefOrdinal);
      const handlers: LensHandlers = {
        "lens.row-next": () => {
          const next = idx === -1 ? rows[0] : rows[Math.min(idx + 1, rows.length - 1)];
          setSelectedRefOrdinal(next?.ordinal ?? null);
        },
        "lens.row-prev": () => {
          const next = idx === -1 ? rows[0] : rows[Math.max(idx - 1, 0)];
          setSelectedRefOrdinal(next?.ordinal ?? null);
        },
        "lens.group-next": () => setSelectedGroup(stepGroup(groups, ungroupedCount, selectedGroup, 1)),
        "lens.group-prev": () => setSelectedGroup(stepGroup(groups, ungroupedCount, selectedGroup, -1)),
      };
      const handler = (handlers as Record<string, (() => void) | undefined>)[cmd.id];
      if (!handler) return;
      e.preventDefault();
      handler();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [groups, ungroupedCount, refs, selectedGroup, setSelectedGroup, selectedRefOrdinal, setSelectedRefOrdinal]);
}
