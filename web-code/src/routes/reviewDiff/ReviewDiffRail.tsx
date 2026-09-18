// `ReviewDiff`'s RAIL — the mobile files drawer, the disposition menu, the
// drafts tray and the keyboard sheet (V73-K2b; the JSX moved out of
// `routes/ReviewDiff.tsx` verbatim, no behaviour change).
//
// The four overlays this page can put over its own body, in one file. They
// are grouped because they share one rule and it is easy to break singly: a
// focused panel may stop only the keys it HANDLES (web-code/CLAUDE.md
// § Keyboard) — none of these installs a blanket `stopPropagation`, and none
// of them may start.
import MobileDrawer from "../../components/MobileDrawer";
import KeyboardHelp from "../../components/KeyboardHelp";
import DraftsTray from "../../components/reviews/DraftsTray";
import ReviewFileTree from "../../components/reviews/ReviewFileTree";
import {
  FilesModeToggle,
  OutsideDiffChapter,
  type OutsideDiffFile,
} from "../../components/reviews/ReviewMapColumn";
import { useSyntax } from "../../hooks/useSyntax";
import type { ReviewFileRow } from "../../api/types";
import type { ReviewFinding } from "../../api/types";
import type { FileTreeNode } from "../../lib/reviewFileTree";
import type { MapRowState } from "../../lib/reviewMapColumn";
import type { DraftsState } from "../../lib/reviewDrafts";
import { cssAttr } from "./helpers";

export interface ReviewDiffRailProps {
  filesOpen: boolean;
  onSetFilesOpen: (open: boolean) => void;
  ordered: ReviewFileRow[];
  cursorPath: string;
  /// V80-F1 — the SAME per-path chip state (viewed/comments/findings/
  /// drafts/noise) the desktop map column renders, replacing the old
  /// flat drawer's own ad hoc `fileRollup`-derived annotation count so
  /// the two homes read identically (`web-code/CLAUDE.md`'s Review diff
  /// v2 "one file tree" rule, extended to the mobile drawer).
  stateByPath: ReadonlyMap<string, MapRowState>;
  onGoFile: (idx: number) => void;
  /// V80-F1 — a path outside `ordered` (an "All files" plain row, or an
  /// "outside the diff" row) has no rendered section to scroll to; this
  /// navigates to the single-file focus route instead (the SAME fallback
  /// `ReviewDiffCenter.tsx`'s `pickFile` uses for the desktop map column).
  onOpenFile: (path: string) => void;
  dispositionMenuOpen: boolean;
  focusThreadId: string | null;
  findingsById: Map<string, ReviewFinding>;
  onCloseDispositionMenu: () => void;
  draftsOpen: boolean;
  drafts: DraftsState;
  publishing: boolean;
  paths: string[];
  onSetDraftsOpen: (open: boolean) => void;
  onSetDrafts: (fn: (cur: DraftsState) => DraftsState) => void;
  removeDraft: (cur: DraftsState, draftId: string) => DraftsState;
  onPublishDrafts: () => void;
  onDiscardDrafts: () => void;
  helpOpen: boolean;
  onSetHelpOpen: (open: boolean) => void;
  /// V80-F1 — "Changed (N) | All files" parity with the desktop map
  /// column (both homes, `web-code/CLAUDE.md`'s Review diff v2 section).
  filesMode: "changed" | "all";
  onSetFilesMode: (mode: "changed" | "all") => void;
  outsideDiffFiles: readonly OutsideDiffFile[];
  /// `buildAllFilesTree`'s output, computed ONCE in `ReviewDiff.tsx` and
  /// shared with the desktop map column — `null`/absent while
  /// `filesMode !== "all"` or the whole-tree fetch hasn't landed yet.
  allTree?: readonly FileTreeNode[] | null;
  changedPaths?: ReadonlySet<string>;
}

export default function ReviewDiffRail({
  filesOpen,
  onSetFilesOpen: setFilesOpen,
  ordered,
  cursorPath,
  stateByPath,
  onGoFile: goFile,
  onOpenFile,
  dispositionMenuOpen,
  focusThreadId,
  findingsById,
  onCloseDispositionMenu: closeDispositionMenu,
  draftsOpen,
  drafts,
  publishing,
  paths,
  onSetDraftsOpen: setDraftsOpen,
  onSetDrafts: setDrafts,
  removeDraft,
  onPublishDrafts: publishDrafts,
  onDiscardDrafts: discardDrafts,
  helpOpen,
  onSetHelpOpen: setHelpOpen,
  filesMode,
  onSetFilesMode,
  outsideDiffFiles,
  allTree,
  changedPaths,
}: ReviewDiffRailProps) {
  const syntaxQ = useSyntax();

  // V80-F1 — a path already rendered on THIS page (a real `ordered` /
  // `paths` entry) just scrolls to its section and closes, exactly the
  // pre-existing "tapping a row scrolls and closes" contract
  // (mobile.spec.ts) — no navigation, so the multi-file page stays put.
  // A path OUTSIDE `paths` (an "All files" plain row, or an "outside the
  // diff" row) has no section to scroll to, so it falls through to
  // `onOpenFile` (the single-file focus navigate the desktop map column's
  // own `pickFile` fallback already uses for the same case).
  function pickAndClose(path: string) {
    const idx = paths.indexOf(path);
    if (idx >= 0) {
      goFile(idx);
      const el = document.querySelector(`[data-kbc-rdiff-file="${cssAttr(path)}"]`);
      el?.scrollIntoView({ block: "start" });
    } else {
      onOpenFile(path);
    }
    setFilesOpen(false);
  }

  return (
    <>
      <MobileDrawer
        open={filesOpen}
        onClose={() => setFilesOpen(false)}
        title="Files"
        ariaLabel="Review files"
      >
        {/* V80-F1 — the SAME status-sectioned folder tree as the Files tab
            and the desktop map column (`web-code/CLAUDE.md`'s Review diff
            v2 "one file tree" rule), not a flat list — folders, real kind
            icons, and (V80-F1) the "Changed | All files" toggle + the
            "outside the diff" chapter for parity with the desktop map
            column, its other home. */}
        <FilesModeToggle
          filesMode={filesMode}
          fileCount={ordered.length}
          onSetFilesMode={onSetFilesMode}
        />
        <OutsideDiffChapter files={outsideDiffFiles} currentPath={cursorPath} onPick={pickAndClose} />
        <ReviewFileTree
          files={ordered}
          stateByPath={stateByPath}
          currentPath={cursorPath}
          syntaxRows={syntaxQ.data?.rows}
          onPick={pickAndClose}
          rowAttr="drawer"
          mode={filesMode}
          allTree={allTree}
          changedPaths={changedPaths}
        />
      </MobileDrawer>
      {dispositionMenuOpen && focusThreadId && findingsById.get(focusThreadId) && (
        <div
          className="kbc-rdiff__disp-menu-scrim"
          onClick={closeDispositionMenu}
          data-kbc-rdiff-disposition-menu
        >
          <div
            className="kbc-rdiff__disp-menu"
            role="dialog"
            aria-modal="true"
            aria-label="Set disposition"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="kbc-rdiff__disp-menu-title">
              {findingsById.get(focusThreadId)?.slug} — set disposition
            </div>
            <ul className="kbc-rdiff__disp-menu-list">
              <li>
                <kbd>a</kbd> agree
              </li>
              <li>
                <kbd>d</kbd> dispute
              </li>
              <li>
                <kbd>w</kbd> waive
              </li>
              <li>
                <kbd>f</kbd> fix-later
              </li>
              <li>
                <kbd>Esc</kbd> cancel
              </li>
            </ul>
          </div>
        </div>
      )}
      <DraftsTray
        open={draftsOpen}
        drafts={drafts.drafts}
        publishing={publishing}
        onClose={() => setDraftsOpen(false)}
        onGoTo={(d) => {
          const idx = paths.indexOf(d.path);
          if (idx >= 0) goFile(idx);
          document
            .querySelector(`[data-kbc-rdiff-file="${cssAttr(d.path)}"]`)
            ?.scrollIntoView({ block: "start" });
        }}
        onRemove={(draftId) => setDrafts((cur) => removeDraft(cur, draftId))}
        onPublish={publishDrafts}
        onDiscardAll={discardDrafts}
      />
      <KeyboardHelp open={helpOpen} onClose={() => setHelpOpen(false)} context="review-diff" />
    </>
  );
}
