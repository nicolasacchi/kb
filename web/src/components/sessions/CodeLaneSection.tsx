// A5 — the session page's code-lane section: kb-code's `kbc-session-card/1`
// (commits with join-ladder confidence, reviews, findings authored, files
// being edited now), fetched cross-origin through the doclens client
// (`hooks/useCodeLane.ts`). Rendered by the `/sessions/:sid` page
// (`routes/sessionPage.tsx`); nothing when the corpus has no code_url.
// One column at every width; the compound caption on failure is the
// SPA-wide doclens degrade vocabulary (`lib/codeFreshness.ts`), not a
// second message.

import type { ReactNode } from "react";

import { useCodeUrlForKb } from "../../hooks/useCodeUrlForKb";
import { useCodeLane } from "../../hooks/useCodeLane";
import { commitHref } from "../../api/sessionCard";
import { COMPOUND_UNAVAILABLE } from "../../lib/codeFreshness";
import "../../styles/code-lane.css";

export interface CodeLaneSectionProps {
  sid: string;
  kb: string | undefined;
  repo?: string;
}

export function CodeLaneSection({ sid, kb, repo }: CodeLaneSectionProps) {
  const codeUrl = useCodeUrlForKb(kb);
  const q = useCodeLane(codeUrl, sid, repo);
  if (!codeUrl) return null;

  const card = q.data;
  let body: ReactNode;
  if (q.isPending) {
    body = <p className="kb-code-lane__skeleton">Loading code lane…</p>;
  } else if (q.isError || !card) {
    body = (
      <div className="kb-code-lane__error">
        <p>{COMPOUND_UNAVAILABLE}</p>
        <button type="button" onClick={() => void q.refetch()}>
          Refresh
        </button>
      </div>
    );
  } else {
    body = (
      <>
        <div className="kb-code-lane__block">
          <h3 className="kb-code-lane__h">Commits · {card.commits.total}</h3>
          {!card.commits.available ? (
            <p className="kb-code-lane__caption">{card.commits.caption}</p>
          ) : card.commits.items.length === 0 ? (
            <p className="kb-code-lane__empty">no commits</p>
          ) : (
            <ul className="kb-code-lane__list">
              {card.commits.items.map((c) => (
                <li key={`${c.repo}:${c.sha}`}>
                  {c.visible ? (
                    <a
                      href={commitHref(codeUrl, c.repo, c.sha)}
                      target="_blank"
                      rel="noreferrer"
                    >
                      <code>{c.sha.slice(0, 12)}</code> {c.subject ?? ""}
                    </a>
                  ) : (
                    <span>
                      <code>{c.sha.slice(0, 12)}</code> {c.subject ?? ""}
                    </span>
                  )}{" "}
                  <span className="kb-code-lane__badge">{c.confidence}</span>
                  {c.attributed_here && (
                    <span className="kb-code-lane__meta"> attributed here</span>
                  )}
                  {!c.visible && c.caption && (
                    <span className="kb-code-lane__caption"> {c.caption}</span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="kb-code-lane__block">
          <h3 className="kb-code-lane__h">Reviews · {card.reviews.total}</h3>
          {!card.reviews.available ? (
            <p className="kb-code-lane__caption">{card.reviews.caption}</p>
          ) : card.reviews.items.length === 0 ? (
            <p className="kb-code-lane__empty">no reviews</p>
          ) : (
            <ul className="kb-code-lane__list">
              {card.reviews.items.map((r) => (
                <li key={r.id}>
                  <span>{r.title || `review ${r.id}`}</span>{" "}
                  <span className="kb-code-lane__meta">{r.state}</span>
                  {r.verdict && <span className="kb-code-lane__badge">{r.verdict}</span>}
                  {r.pr_number != null && (
                    <span className="kb-code-lane__meta"> PR #{r.pr_number}</span>
                  )}
                  <span className="kb-code-lane__meta"> · {r.basis}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="kb-code-lane__block">
          <h3 className="kb-code-lane__h">Findings authored · {card.findings.total}</h3>
          {!card.findings.available ? (
            <p className="kb-code-lane__caption">{card.findings.caption}</p>
          ) : (
            <p className="kb-code-lane__meta">
              {card.findings.by_severity.blocker} blocker ·{" "}
              {card.findings.by_severity.concern} concern ·{" "}
              {card.findings.by_severity.ok} ok
            </p>
          )}
        </div>
        <div className="kb-code-lane__block">
          <h3 className="kb-code-lane__h">Editing now</h3>
          {!card.presence.available ? (
            <p className="kb-code-lane__caption">{card.presence.caption}</p>
          ) : card.presence.files === 0 ? (
            <p className="kb-code-lane__empty">no edits in sight</p>
          ) : (
            <p className="kb-code-lane__meta">
              {card.presence.files} files · newest {card.presence.newest_age_secs}s ago
            </p>
          )}
        </div>
      </>
    );
  }

  return (
    <section className="kb-code-lane" data-testid="code-lane">
      <div className="kb-code-lane__body">{body}</div>
    </section>
  );
}
