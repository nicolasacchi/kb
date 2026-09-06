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
import { Icon } from "../../components/icons";
import type { ReviewFileRow } from "../../api/types";
import type { ReviewFinding } from "../../api/types";
import type { DraftsState } from "../../lib/reviewDrafts";
import { cssAttr } from "./helpers";

export interface ReviewDiffRailProps {
  filesOpen: boolean;
  onSetFilesOpen: (open: boolean) => void;
  ordered: ReviewFileRow[];
  cursorPath: string;
  fileRollup: Map<string, { open: number }> | null;
  onGoFile: (idx: number) => void;
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
}

export default function ReviewDiffRail({
  filesOpen,
  onSetFilesOpen: setFilesOpen,
  ordered,
  cursorPath,
  fileRollup,
  onGoFile: goFile,
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
}: ReviewDiffRailProps) {
  return (
    <>
      <MobileDrawer
        open={filesOpen}
        onClose={() => setFilesOpen(false)}
        title="Files"
        ariaLabel="Review files"
      >
        {ordered.map((file, idx) => {
          const checked = !!(file.viewed && !file.viewed_stale);
          const openN = fileRollup?.get(file.path)?.open ?? file.open_annotations;
          return (
            <button
              key={file.path}
              type="button"
              className={
                "kbc-rdiff__file-row" + (file.path === cursorPath ? " is-current" : "")
              }
              onClick={() => {
                goFile(idx);
                const el = document.querySelector(
                  `[data-kbc-rdiff-file="${cssAttr(file.path)}"]`,
                );
                el?.scrollIntoView({ block: "start" });
                setFilesOpen(false);
              }}
              data-kbc-rdiff-drawer-file={file.path}
            >
              <span
                className="kbc-rdiff__file-check"
                aria-label={checked ? "viewed" : "unviewed"}
                data-kbc-rdiff-drawer-viewed={checked ? "1" : "0"}
              >
                {checked ? <Icon.Check /> : null}
              </span>
              <span className="kbc-rdiff__file-row-path">{file.path}</span>
              {openN > 0 && (
                <span className="kbc-review__file-ann" data-kbc-rdiff-drawer-ann>
                  {openN}
                </span>
              )}
            </button>
          );
        })}
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
