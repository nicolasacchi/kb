// RAILKNOW — one memory row (injected, written or a search hit). Every string
// is a React text node: titles and summaries are memory text, never markup.

import { useState, type ReactElement } from "react";
import { useConfirmOptional } from "../../../components/ConfirmProvider";
import { toast } from "../../../lib/toast";
import { DriveError, sendText } from "../../phone/driveApi";
import { useTermRail } from "../context";
import { flattenRef, openHref, rankLabel, rowTitle, type MemoryRowData } from "./knowledgeRows";

const NEVER_CONFIRM = async (): Promise<boolean> => false;

export function MemoryRow({ row }: { row: MemoryRowData }): ReactElement {
  const { termId, harness } = useTermRail();
  const realConfirm = useConfirmOptional();
  const confirm = realConfirm ?? NEVER_CONFIRM;
  const [busy, setBusy] = useState(false);
  const href = openHref(row);
  const title = rowTitle(row);

  const onPaste = async (): Promise<void> => {
    if (busy || termId === "") return;
    setBusy(true);
    try {
      // Paste only: `enter` is never true in this tab.
      const res = await sendText({ termId, harness, viewer: null, confirm }, flattenRef(row), { enter: false });
      if (res.kind === "sent") toast.ok("Pasted without Enter");
      // An operator cancel is silent; a missing confirm host is not.
      else if (res.kind === "declined" && !realConfirm) toast.err("Paste needs a confirmation that is unavailable here");
    } catch (e) {
      toast.err(e instanceof DriveError ? e.detail : e instanceof Error ? e.message : "Paste failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <li className="kb-knowrail__row" data-testid="kn-row" data-kb={row.kb} data-id={row.id}>
      <div className="kb-knowrail__rowhead">
        {row.showRank ? (
          <span className="kb-knowrail__chip" data-testid="kn-rank">
            {rankLabel(row.pos)}
          </span>
        ) : null}
        {row.used ? (
          <span className="kb-knowrail__chip" data-testid="kn-used">
            referenced later
          </span>
        ) : null}
      </div>
      <p className="kb-knowrail__title" data-testid="kn-title">
        {title}
      </p>
      {row.summary ? <p className="kb-knowrail__summary">{row.summary}</p> : null}
      <div className="kb-knowrail__tools">
        {href ? (
          <a
            className="kb-knowrail__btn"
            data-testid="kn-open"
            href={href}
            target="_blank"
            rel="noreferrer"
            aria-label={`Open ${title} in kb`}
          >
            Open in kb
          </a>
        ) : null}
        <button
          type="button"
          className="kb-knowrail__btn"
          data-testid="kn-paste"
          disabled={busy}
          aria-label={`Paste reference to ${title} into the terminal`}
          onClick={() => void onPaste()}
        >
          Paste reference
        </button>
      </div>
    </li>
  );
}
