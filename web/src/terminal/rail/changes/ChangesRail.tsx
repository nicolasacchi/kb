// RAILCHANGES — the Changes tab of the terminal's right panel: the bound
// session's files (grouped by directory), commits and checks, read from the
// existing session card / detail hooks. Derived and honest: freshness is "as of
// the last capture", never live; invalidation rides the existing sessions SSE
// bridge (no timer here fetches). All text is rendered as text nodes. Doors to
// kb-code exist only when the kb carries a `code_url`. No xterm import.

import { useEffect, useMemo, useState, type ReactElement } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { StateCard } from "@kb/ui/StateCard";
import { artifactHref } from "../../../lib/artifactHref";
import { httpCodeUrl } from "../../../lib/codeDoors";
import { SESSION_CARD_KEY, useSessionCard } from "../../../hooks/useSessionCard";
import { useCodeLane } from "../../../hooks/useCodeLane";
import { useCodeUrlForKb } from "../../../hooks/useCodeUrlForKb";
import { useSessionDetail } from "../../../hooks/useSessions";
import { useTermRail } from "../context";
import { ChecksLazy } from "./ChecksLazy";
import {
  LIVE_CAPTURE_HINT,
  NOT_CAPTURED_REASON,
  capCommits,
  commitDoor,
  fileDoor,
  freshnessText,
  groupFiles,
  matchCodeItem,
  readCount,
  sessionPageHref,
  shortSha,
  tierFor,
} from "./changesModel";
import "./changesRail.css";

function Wrap({ children }: { children: ReactElement }): ReactElement {
  return (
    <div className="kb-chgrail" data-testid="changes-rail">
      {children}
    </div>
  );
}

function PageDoor({ sid }: { sid: string }): ReactElement {
  return (
    <a
      className="kb-chgrail__btn"
      data-testid="changes-page-door"
      href={sessionPageHref(sid)}
      target="_blank"
      rel="noreferrer"
    >
      Open in session page
    </a>
  );
}

export function ChangesRail(): ReactElement {
  const { sid } = useTermRail();
  const qc = useQueryClient();
  const { card, loading, error } = useSessionCard(sid ?? "");
  const pending = !!card?.capture_pending;
  // A live-only session has no capture: hold the detail bundle off (it would
  // only 404); the bridge's session.captured handler refetches both keys.
  const { detail, files, commits, loading: detailLoading } = useSessionDetail(pending ? null : (sid ?? null));
  const codeUrl = httpCodeUrl(useCodeUrlForKb(detail?.kb || undefined));
  const lane = useCodeLane(codeUrl, sid ?? "");
  const laneItems = lane.data?.commits.available ? lane.data.commits.items : undefined;
  const [showReads, setShowReads] = useState(false);
  const [showAllCommits, setShowAllCommits] = useState(false);
  const [now, setNow] = useState<number>(() => Date.now());

  // A local re-render tick for the freshness caption: a timer, never a fetch.
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(t);
  }, []);

  const groups = useMemo(() => groupFiles(files, showReads), [files, showReads]);
  const reads = useMemo(() => readCount(files), [files]);

  if (!sid) {
    return (
      <Wrap>
        <StateCard
          kind="empty"
          size="inline"
          title="No session linked yet"
          reason="This terminal has no bound session; it links on the first harness beat."
        />
      </Wrap>
    );
  }
  if (loading || (!pending && detailLoading)) {
    return (
      <Wrap>
        <StateCard kind="loading" size="inline" title="Loading changes" />
      </Wrap>
    );
  }
  if (error || !card) {
    return (
      <Wrap>
        <StateCard
          kind="degraded"
          size="inline"
          title="Could not load the changes"
          actions={[
            {
              label: "Retry",
              key: "retry",
              onClick: () => {
                void qc.invalidateQueries({ queryKey: SESSION_CARD_KEY(sid) });
                void qc.invalidateQueries({ queryKey: ["session", sid] });
              },
            },
          ]}
        />
      </Wrap>
    );
  }
  if (pending) {
    return (
      <Wrap>
        <StateCard kind="empty" size="inline" title="Not captured yet" reason={NOT_CAPTURED_REASON} />
      </Wrap>
    );
  }

  const editedOnly = groupFiles(files, false);
  const hasEvidence = !!detail?.evidence;
  if (editedOnly.length === 0 && reads === 0 && commits.length === 0 && !hasEvidence) {
    return (
      <Wrap>
        <StateCard kind="empty" size="inline" title="No changes recorded" />
      </Wrap>
    );
  }

  const shownCommits = capCommits(commits, showAllCommits);

  return (
    <Wrap>
      <>
        <div className="kb-chgrail__bar">
          <p className="kb-chgrail__fresh" data-testid="changes-fresh">
            {freshnessText(detail?.ended_at, now)}
          </p>
          <p className="kb-chgrail__hint">{LIVE_CAPTURE_HINT}</p>
        </div>

        <section className="kb-chgrail__section" data-testid="changes-files">
          <div className="kb-chgrail__head">
            <h3 className="kb-chgrail__h">Files</h3>
            <PageDoor sid={sid} />
          </div>
          {groups.length === 0 ? (
            <p className="kb-chgrail__hint">No files edited.</p>
          ) : (
            groups.map((g) => (
              <div key={g.dir} className="kb-chgrail__dir" data-testid="changes-dir" data-dir={g.dir}>
                <p className="kb-chgrail__dirname" title={g.dir}>
                  {g.dir} <span className="kb-chgrail__count">{g.rows.length}</span>
                </p>
                <ul className="kb-chgrail__list" role="list">
                  {g.rows.map((r) => {
                    const door = fileDoor(codeUrl, r.path);
                    const inCorpus = r.inCorpus && r.kb && r.sourceRelative ? artifactHref(r.kb, r.sourceRelative) : null;
                    return (
                      <li key={`${r.path}:${r.action}:${r.viaSubagent}`} className="kb-chgrail__file" data-testid="changes-file" data-action={r.action}>
                        <span className="kb-chgrail__name" title={r.path}>
                          {r.basename}
                        </span>
                        <span className="kb-chgrail__chip">{r.action}</span>
                        {r.viaSubagent ? (
                          <span className="kb-chgrail__chip" data-testid="changes-subagent">
                            via subagent
                          </span>
                        ) : null}
                        {door ? (
                          <a className="kb-chgrail__door" data-testid="changes-file-door" href={door} target="_blank" rel="noreferrer">
                            kb-code
                          </a>
                        ) : null}
                        {inCorpus ? (
                          <a className="kb-chgrail__door" data-testid="changes-file-artifact" href={inCorpus} target="_blank" rel="noreferrer">
                            Open
                          </a>
                        ) : null}
                      </li>
                    );
                  })}
                </ul>
              </div>
            ))
          )}
          {reads > 0 ? (
            <button type="button" className="kb-chgrail__btn" data-testid="changes-toggle-reads" onClick={() => setShowReads((v) => !v)}>
              {showReads ? "Hide read files" : `Show ${reads} read file${reads === 1 ? "" : "s"}`}
            </button>
          ) : null}
        </section>

        <section className="kb-chgrail__section" data-testid="changes-commits">
          <div className="kb-chgrail__head">
            <h3 className="kb-chgrail__h">Commits</h3>
            <PageDoor sid={sid} />
          </div>
          {commits.length === 0 ? (
            <p className="kb-chgrail__hint">No commits recorded.</p>
          ) : (
            <ul className="kb-chgrail__list" role="list">
              {shownCommits.map((c, i) => {
                const item = matchCodeItem(c.sha, laneItems);
                const tier = tierFor(item);
                const door = commitDoor(codeUrl, item);
                const sha = shortSha(c);
                return (
                  <li key={`${c.sha ?? ""}:${i}`} className="kb-chgrail__commit" data-testid="changes-commit">
                    <span className="kb-chgrail__subject">{c.subject ?? ""}</span>
                    <span className="kb-chgrail__meta">
                      <span className="kb-chgrail__chip">{c.kind}</span>
                      {sha ? <code className="kb-chgrail__sha">{sha}</code> : null}
                      {c.resolved === false ? (
                        <span className="kb-chgrail__chip kb-chgrail__chip--warn" data-testid="changes-unresolved">
                          unresolved
                        </span>
                      ) : null}
                      {tier ? (
                        <span className="kb-chgrail__chip" data-testid="changes-tier" data-tier={tier}>
                          {tier}
                        </span>
                      ) : null}
                      {door ? (
                        <a className="kb-chgrail__door" data-testid="changes-commit-door" href={door} target="_blank" rel="noreferrer">
                          kb-code
                        </a>
                      ) : null}
                    </span>
                  </li>
                );
              })}
            </ul>
          )}
          {!showAllCommits && commits.length > shownCommits.length ? (
            <button type="button" className="kb-chgrail__btn" data-testid="changes-show-all" onClick={() => setShowAllCommits(true)}>
              {`Show all ${commits.length}`}
            </button>
          ) : null}
        </section>

        <div className="kb-chgrail__checks" data-testid="changes-checks">
          {detail ? <ChecksLazy detail={detail} /> : null}
          <PageDoor sid={sid} />
        </div>
      </>
    </Wrap>
  );
}
