// `kbc-claim/1` (V73-K2c, design D18) — the claim register. Rendered on the
// Document tab and beside the Report tab's findings (`ReportPanel.tsx`),
// and reused by the reader inspector's Claims card (`components/lens/
// ClaimsCard.tsx`) for a single-file scope. ONE component, three mount
// points — the "one projection, two renderers" rule kbc-tree/1 states,
// applied a third time.
//
// Three rules from `lib/claims.ts`'s own header apply here directly:
// surfaced-never-scored (wire order, never re-sorted by confidence — see
// `claims.test.ts`'s pin), the ladder state gets its OWN small badge (never
// `TrustBadge`'s fact channel), and `confidence` renders as agent-declared
// TEXT, never a bar.
import { useMemo, useState } from "react";
import type { ClaimOut } from "../../api/types";
import {
  claimKindLabel,
  claimsRenderOrder,
  confidenceText,
  evidenceRefView,
  ladderLabel,
} from "../../lib/claims";
import { parseMarkdownLite, type InlineRun, type MarkdownBlock } from "../../lib/markdownLite";
import { Icon } from "../icons";
import { FenceBlock } from "../SafeMarkdown";

function InlineRuns({ runs }: { runs: InlineRun[] }) {
  return (
    <>
      {runs.map((r, i) => {
        if (r.kind === "bold") return <strong key={i}>{r.text}</strong>;
        if (r.kind === "code") return <code key={i}>{r.text}</code>;
        return <span key={i}>{r.text}</span>;
      })}
    </>
  );
}

/// The claim's own prose, via the SAME options-off `markdownLite` path
/// `ReportPanel.tsx`'s summary uses — a claim body is agent-written prose
/// with no fences/headings of its own, so neither of the K2b-added block
/// kinds is reachable here (each renderer here hand-rolls its own mapping,
/// the established pattern this crate already carries for `DocPanel`/
/// `ReportPanel`).
function ClaimBody({ text }: { text: string }) {
  const blocks: MarkdownBlock[] = useMemo(
    () => parseMarkdownLite(text, { fences: true }),
    [text],
  );
  return (
    <div className="kbc-claim__body">
      {blocks.map((b, i) => {
        if (b.kind === "list") {
          return (
            <ul key={i}>
              {b.items.map((item, j) => (
                <li key={j}>
                  <InlineRuns runs={item} />
                </li>
              ))}
            </ul>
          );
        }
        if (b.kind === "paragraph" || b.kind === "heading") {
          return (
            <p key={i}>
              <InlineRuns runs={b.runs} />
            </p>
          );
        }
        return <FenceBlock key={i} text={b.text} lang={b.lang} />;
      })}
    </div>
  );
}

function EvidenceRow({
  raw,
  repo,
  reviewId,
}: {
  raw: string;
  repo: string;
  reviewId: number | undefined;
}) {
  const view = evidenceRefView(raw, repo, reviewId);
  return (
    <li className="kbc-claim__evidence-row" data-kbc-claim-evidence={view.raw}>
      {view.href ? (
        <a href={view.href} className="kbc-claim__evidence-link" data-kbc-claim-evidence-link={view.raw}>
          {view.label}
        </a>
      ) : (
        <span className="kbc-claim__evidence-plain" data-kbc-claim-evidence-plain={view.raw}>
          {view.label}
        </span>
      )}
    </li>
  );
}

function ClaimRow({
  claim,
  repo,
  reviewId,
}: {
  claim: ClaimOut;
  repo: string;
  reviewId: number | undefined;
}) {
  const [open, setOpen] = useState(true);
  return (
    <li
      className="kbc-claim"
      data-kbc-claim={claim.id}
      data-kbc-claim-kind={claim.kind}
      data-kbc-claim-state={claim.state}
    >
      <button
        type="button"
        className="kbc-claim__head"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        data-kbc-claim-toggle={claim.id}
      >
        <span className="kbc-claim__kind" data-kbc-claim-kind-chip>
          {claimKindLabel(claim.kind)}
        </span>
        <span className="kbc-claim__subject" data-kbc-claim-subject>
          {claim.subject}
        </span>
        <span
          className={`kbc-claim__ladder kbc-claim__ladder--${claim.state}`}
          data-kbc-claim-ladder={claim.state}
          title={claim.caption}
        >
          {ladderLabel(claim.state)}
        </span>
        <Icon.ChevDown />
      </button>
      {open && (
        <div className="kbc-claim__detail">
          <ClaimBody text={claim.body_md} />
          {claim.state === "drifted" && (
            <p className="kbc-claim__drift" data-kbc-claim-drift>
              {claim.caption}
            </p>
          )}
          {claim.state === "unanchored" && (
            <p className="kbc-claim__unanchored" data-kbc-claim-unanchored>
              {claim.caption}
            </p>
          )}
          {claim.evidence.length > 0 && (
            <ul className="kbc-claim__evidence" data-kbc-claim-evidence-list>
              {claim.evidence.map((e, i) => (
                <EvidenceRow key={`${e}:${i}`} raw={e} repo={repo} reviewId={reviewId} />
              ))}
            </ul>
          )}
          <p className="kbc-claim__meta" data-kbc-claim-meta>
            <span data-kbc-claim-confidence>{confidenceText(claim.confidence)}</span>
            {" · "}
            <span data-kbc-claim-provenance>
              {claim.model ? claim.model : "agent"}
              {claim.session_id ? ` · session ${claim.session_id.slice(0, 12)}` : ""}
            </span>
          </p>
        </div>
      )}
    </li>
  );
}

export interface ClaimRegisterProps {
  repo: string;
  reviewId?: number;
  claims: ClaimOut[];
  /// A caption naming the scope this list was fetched for (e.g. "this
  /// review" / this file's path) — every empty-state and every non-empty
  /// header names what it is a register OF, never a bare "0" or a silent
  /// gap.
  scopeLabel: string;
  /// `[data-kbc-claims-toggle]` — the `doc.claims-toggle` registry row's
  /// DOM-click target (the same delegation pattern `doc.compose-copy`
  /// already uses).
  open?: boolean;
  onSetOpen?: (open: boolean) => void;
}

export default function ClaimRegister({
  repo,
  reviewId,
  claims,
  scopeLabel,
  open,
  onSetOpen,
}: ClaimRegisterProps) {
  const [localOpen, setLocalOpen] = useState(true);
  const isOpen = open ?? localOpen;
  const setOpen = onSetOpen ?? setLocalOpen;
  return (
    <section className="kbc-claims" data-kbc-claims>
      <button
        type="button"
        className="kbc-claims__toggle"
        onClick={() => setOpen(!isOpen)}
        aria-expanded={isOpen}
        aria-controls="kbc-claims-list"
        data-kbc-claims-toggle
      >
        <span className="kbc-claims__title">
          Claims{claims.length > 0 ? ` (${claims.length})` : ""}
        </span>
        <span className="kbc-claims__scope" data-kbc-claims-scope>
          {scopeLabel}
        </span>
        <Icon.ChevDown />
      </button>
      {isOpen &&
        (claims.length === 0 ? (
          <p className="kbc-review__card-empty" data-kbc-claims-empty>
            No agent claims on {scopeLabel}.
          </p>
        ) : (
          <ul className="kbc-claims__list" id="kbc-claims-list" data-kbc-claims-list>
            {claimsRenderOrder(claims).map((c) => (
              <ClaimRow key={c.id} claim={c} repo={repo} reviewId={reviewId} />
            ))}
          </ul>
        ))}
    </section>
  );
}
