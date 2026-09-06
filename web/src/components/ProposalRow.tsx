import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { fetchDoc, type ProposalItem } from "../api/client";
import { relativeAge } from "../lib/time";
import { useConfirm } from "./ConfirmProvider";
import { toast } from "../lib/toast";

// W2.15b — one row in the /inbox "Proposals" section: a queued memory
// CANDIDATE (agent-authored, `kb propose`). Approve/reject are the only
// actions — both destructive-ish (approve WRITES a memory; reject discards
// the candidate permanently), so both go through `useConfirm()` (#32), never
// a bare click. The parent (inbox.tsx) owns the query + optimistic removal;
// this component only calls the two mutation callbacks it's handed.

export type ProposalRowActions = {
  onApprove: (kb: string, id: string) => Promise<void>;
  onReject: (kb: string, id: string) => Promise<void>;
};

/// Flatten + cap the markdown body to a plain-text excerpt — same shape as
/// `routes/inbox.tsx`'s comment excerpts (server truncates those; a
/// proposal's `body` travels whole, so the excerpt is cut client-side).
function excerpt(body: string, max = 220): string {
  const flat = body.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

export default function ProposalRow({
  item,
  onApprove,
  onReject,
}: { item: ProposalItem } & ProposalRowActions) {
  const confirm = useConfirm();

  // MI-W3.2a — a proposal that carries `supersedes` would retire an
  // EXISTING memory on approve; a reviewer clicking Approve blind to that
  // has no way to know. Resolve the target's title so it renders inline,
  // same-corpus lookup (the wire never carries a cross-kb supersedes id —
  // `IngestBody.supersedes` is always resolved within `item.kb`). Best-
  // effort: a 404 (the target was itself since deleted/purged) just falls
  // back to the bare id rather than blocking the row.
  const supersedesTarget = useQuery({
    queryKey: ["doc", item.kb, item.supersedes] as const,
    queryFn: ({ signal }) => fetchDoc(item.kb, item.supersedes as string, signal),
    enabled: !!item.supersedes,
  });

  const approve = async () => {
    const ok = await confirm({
      title: "Approve this proposal?",
      body: `Write “${item.title}” into ${item.kb} as a memory? This fires the same write \`kb remember\` uses — it can be forgotten afterward with \`kb forget\`.`,
      confirmLabel: "Approve",
    });
    if (!ok) return;
    try {
      await onApprove(item.kb, item.id);
      toast.ok("approved — written as a memory");
    } catch (err) {
      toast.err(
        `approve failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  };

  const reject = async () => {
    const ok = await confirm({
      title: "Reject this proposal?",
      body: `Discard “${item.title}”? The candidate is gone, not archived — this can't be undone.`,
      confirmLabel: "Reject",
      danger: true,
    });
    if (!ok) return;
    try {
      await onReject(item.kb, item.id);
      toast.ok("rejected");
    } catch (err) {
      toast.err(
        `reject failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  };

  return (
    <li className="proposal-row" data-testid="proposal-row">
      <div className="proposal-row__head">
        <span className="proposal-row__kb" title={`corpus: ${item.kb}`}>
          {item.kb}
        </span>
        <span className="proposal-row__title">{item.title}</span>
        <span className="proposal-row__age">{relativeAge(item.created_at)}</span>
      </div>
      <p className="proposal-row__excerpt">{excerpt(item.body)}</p>
      <div className="proposal-row__meta">
        {item.tags.map((t) => (
          <span key={t} className="proposal-row__chip">
            #{t}
          </span>
        ))}
        {typeof item.salience === "number" && (
          <span className="proposal-row__chip proposal-row__chip--salience">
            salience {item.salience.toFixed(2)}
          </span>
        )}
        {item.session_id && (
          <Link
            className="proposal-row__chip proposal-row__chip--session"
            to={`/sessions?focus=${encodeURIComponent(item.session_id)}`}
          >
            session
          </Link>
        )}
      </div>
      {item.supersedes && (
        <p
          className="proposal-row__supersedes"
          data-testid="proposal-row-supersedes"
        >
          Approving retires{" "}
          <strong>
            {supersedesTarget.data?.title ?? item.supersedes}
          </strong>{" "}
          (<code>{item.supersedes}</code>)
        </p>
      )}
      <div className="proposal-row__actions">
        <button
          type="button"
          className="proposal-row__act proposal-row__act--approve"
          data-kb-act="proposal-approve"
          onClick={() => void approve()}
        >
          Approve
        </button>
        <button
          type="button"
          className="proposal-row__act proposal-row__act--reject"
          data-kb-act="proposal-reject"
          onClick={() => void reject()}
        >
          Reject
        </button>
      </div>
    </li>
  );
}
