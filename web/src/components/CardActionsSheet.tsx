import { useState } from "react";
import { createPortal } from "react-dom";
import { Sheet, type Detent } from "@kb/ui/Sheet";
import { Icon } from "@kb/ui/icons";
import type { DocSummary } from "../api/client";
import { EyeIcon, EyeOffIcon, type CardActions } from "./cardActions";

// Touch-only "More actions" sheet. Portaled to <body>: virtual-grid rows are
// `transform: translateY`, which would make a fixed Sheet inside a card
// position against the row instead of the viewport. Theme attributes live on
// <html>, so tokens still resolve out there.
export default function CardActionsSheet({
  doc,
  actions,
  close,
  restoreFocus,
}: {
  doc: DocSummary;
  actions: CardActions;
  /** Closes the sheet and returns focus to the More button. */
  close: () => void;
  /** Re-focus the More button (used once the exclude confirm is answered). */
  restoreFocus: () => void;
}) {
  const [detent, setDetent] = useState<Detent>("peek");
  const run = (fn: () => void) => () => {
    close();
    fn();
  };
  // React bubbles synthetic events through portals to the card's React
  // ancestors; the wrapper keeps sheet clicks from reaching them.
  return createPortal(
    <div style={{ display: "contents" }} onClick={(e) => e.stopPropagation()}>
    <Sheet
      open
      detent={detent}
      onDetentChange={setDetent}
      onClose={close}
      title={doc.title || doc.id}
      peek="288px"
      full="288px"
      id="kb-card-actions"
    >
      <ul role="list" className="kb-card-sheet">
        <li>
          <button type="button" className="kb-card-sheet__row" data-kb-act="peek" onClick={run(actions.peek)}>
            <EyeIcon />
            <span>Preview</span>
          </button>
        </li>
        <li>
          <button type="button" className="kb-card-sheet__row" data-kb-act="download" onClick={run(actions.download)}>
            <Icon.Download />
            <span>Download</span>
          </button>
        </li>
        <li>
          <button
            type="button"
            className="kb-card-sheet__row kb-card-sheet__row--warn"
            data-kb-act="exclude"
            onClick={() => {
              close();
              void actions.exclude().then(restoreFocus);
            }}
          >
            <EyeOffIcon />
            <span>Exclude from index</span>
          </button>
        </li>
      </ul>
    </Sheet>
    </div>,
    document.body,
  );
}
