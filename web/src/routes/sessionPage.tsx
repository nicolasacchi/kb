// A4 — one session's page at `/sessions/:sid`, rendering A3's
// `kb-session-card/1`. Track A owns this page and the card; kb-term owns
// the panel (filled into `<TerminalSlot>` later). Every number comes from
// the card (surfaced, never scored); the SPA builds no command string and
// no transcript excerpt. One column at every width; sections are
// `<details>` — collapsed at 390px (useIsMobile), open at >=1280px.

import { useMemo, type ReactNode } from "react";
import { Link, useLocation, useParams } from "react-router-dom";
import { useInfiniteQuery } from "@tanstack/react-query";
import { StateCard } from "@kb/ui/StateCard";
import { Icon } from "@kb/ui/icons";
import { useIsMobile } from "../hooks/useIsMobile";
import { useNowTick } from "../hooks/useNowTick";
import { useSessionCard } from "../hooks/useSessionCard";
import { useSessionDetail } from "../hooks/useSessions";
import {
  fetchSessionChildren,
  type ChildRow,
  type ChildrenLane,
  type ChildrenResponse,
} from "../api/sessions";
import { TerminalSlot, shouldRenderTerminal } from "../components/TerminalSlot";
import { CodeLaneSection } from "../components/sessions/CodeLaneSection";
import { ChecksSection, CommitsSection, ResumeCommand } from "./sessions";
import { sessionDisplayName } from "../lib/sessionDisplayName";
import {
  formatActiveDuration,
  harnessGlyph,
  outcomeLine,
  substanceBadge,
} from "../lib/sessionChips";
import { replayUrl } from "../lib/sessionsUrl";
import { artifactHref } from "../lib/artifactHref";
import { relativeAge } from "../lib/time";

function fmtDate(unixSeconds: number): string {
  if (!unixSeconds) return "";
  const d = new Date(unixSeconds * 1000);
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function Section({
  name,
  title,
  open,
  children,
}: {
  name: string;
  title: string;
  open: boolean;
  children: ReactNode;
}) {
  return (
    <details
      id={`sec-${name}`}
      data-testid={`session-sec-${name}`}
      open={open}
      className="kb-sp__sec"
    >
      <summary className="kb-sp__sum">{title}</summary>
      <div className="kb-sp__body">{children}</div>
    </details>
  );
}

function Unavailable({ reason }: { reason?: string }) {
  return (
    <p className="kb-sp__empty">
      unavailable{reason ? ` — ${reason}` : ""}
    </p>
  );
}

function ChildrenSection({
  sid,
  lane,
  open,
}: {
  sid: string;
  lane: ChildrenLane;
  open: boolean;
}) {
  const firstPage: ChildrenResponse = useMemo(
    () => ({
      session_id: sid,
      total: lane.total,
      items: lane.items,
      next_after: lane.next_after ?? null,
    }),
    [sid, lane],
  );
  const q = useInfiniteQuery({
    queryKey: ["sessions", "card", sid, "children"] as const,
    enabled: lane.available,
    initialPageParam: null as string | null,
    queryFn: ({ pageParam, signal }): Promise<ChildrenResponse> =>
      pageParam === null
        ? Promise.resolve(firstPage)
        : fetchSessionChildren(sid, { after: pageParam, limit: 50 }, signal),
    getNextPageParam: (last: ChildrenResponse): string | undefined =>
      last.next_after ?? undefined,
  });
  const items: ChildRow[] = useMemo(
    () => (q.data?.pages ?? []).flatMap((p) => p.items),
    [q.data],
  );

  return (
    <Section name="children" title={`Subagents · ${lane.total}`} open={open}>
      {!lane.available ? (
        <Unavailable reason={lane.reason} />
      ) : (
        <>
          <p className="kb-sp__total" data-testid="session-children-total">
            {lane.total} total
          </p>
          <ul className="kb-sp__list">
            {items.map((c) => (
              <li key={c.session_id} data-testid="session-child-row">
                <Link
                  to={`/sessions/${encodeURIComponent(c.session_id)}`}
                  data-testid="session-child-link"
                >
                  {c.display_name || c.session_id}
                </Link>
                <span className="kb-sp__meta">
                  {harnessGlyph(c.harness)} {c.harness} ·{" "}
                  {fmtDate(c.started_at)} · {c.files_edited_count} edited ·{" "}
                  {c.tool_calls} tools
                </span>
              </li>
            ))}
          </ul>
          {q.hasNextPage && (
            <button
              type="button"
              className="kb-sp__more"
              data-testid="session-children-more"
              onClick={() => void q.fetchNextPage()}
              disabled={q.isFetchingNextPage}
            >
              {q.isFetchingNextPage ? "loading…" : "load more"}
            </button>
          )}
        </>
      )}
    </Section>
  );
}

export default function SessionPage() {
  const { sid } = useParams<{ sid: string }>();
  const location = useLocation();
  const isMobile = useIsMobile();
  const now = useNowTick(30_000);
  const { card, loading, error } = useSessionCard(sid ?? "");
  // The outcome section reuses the inspector's ChecksSection, which needs
  // the row+detail pair — the card summary is not a SessionRow, so the
  // page fetches the existing detail hook for it.
  const { detail } = useSessionDetail(sid ?? null);

  if (!sid) {
    return (
      <StateCard
        kind="empty"
        title="No session selected"
        reason="Navigate to a session page first."
      />
    );
  }
  if (loading) {
    return (
      <StateCard kind="loading" title="Loading session" reason={`… ${sid}`} />
    );
  }
  if (error || !card) {
    return (
      <StateCard
        kind="degraded"
        title="Could not load session"
        reason={error ?? `session ${sid} not found`}
      />
    );
  }

  const sum = card.summary;
  const title = detail ? sessionDisplayName(detail) : sum.display_name;
  const outcomeText = card.outcome.last_assistant_text ?? "";
  const lead = outcomeLine(outcomeText, detail?.first_user_prompt);
  const husk = substanceBadge(sum.substance);
  const treeTarget = sum.root_session_id ?? sum.parent_session_id;
  const from = (location.state as { from?: string } | null)?.from;
  const open = !isMobile;
  const notCommitted = card.outcome.not_committed;
  const notCommittedFiles = new Set(notCommitted?.files ?? []);
  const transcriptHref =
    detail?.source_relative != null
      ? artifactHref(card.kb, detail.source_relative)
      : null;

  const leaf = (n: string) => "/sessions/" + encodeURIComponent(n);

  return (
    <main className="kb-sp" data-testid="session-page">
      <header className="kb-sp__head">
        <p className="kb-sp__back">
          <Link
            to={from ?? `/sessions?focus=${encodeURIComponent(sid)}`}
            data-testid="session-back"
          >
            <Icon.ArrowLeft aria-hidden="true" /> back
          </Link>
          {treeTarget && treeTarget !== sid && (
            <>
              {" · "}
              <Link to={leaf(treeTarget)} data-testid="session-tree-link">
                {sum.root_session_id ? "root session" : "parent session"}
              </Link>
            </>
          )}
        </p>
        <h1 className="kb-sp__title">{title}</h1>
        <p className="kb-sp__sub">
          <code>{sid}</code> · <span>{card.kb}</span>
          {sum.project_slug ? ` · ${sum.project_slug}` : ""}
        </p>
        <p className="kb-sp__chips">
          <span className="kb-sp__chip">
            {harnessGlyph(sum.harness)} {sum.harness}
          </span>
          {husk && <span className="kb-sp__chip">{husk}</span>}
          <span className="kb-sp__chip">
            active {formatActiveDuration(sum.active_secs)}
          </span>
          <span className="kb-sp__chip">
            started {fmtDate(sum.started_at)} · {relativeAge(sum.started_at, now)} ago
          </span>
          {sum.ended_at > 0 && (
            <span className="kb-sp__chip">
              ended {fmtDate(sum.ended_at)} · {relativeAge(sum.ended_at, now)} ago
            </span>
          )}
        </p>
        {lead && <p className="kb-sp__lead">{lead.text}</p>}
      </header>

      {shouldRenderTerminal(card.terminal) && (
        <details
          id="sec-terminal"
          data-testid="session-sec-terminal"
          open={open}
          className="kb-sp__sec"
        >
          <summary className="kb-sp__sum">Terminal</summary>
          <div className="kb-sp__body">
            <TerminalSlot
              sid={sid}
              harness={sum.harness}
              lane={card.terminal}
            />
          </div>
        </details>
      )}

      <Section name="outcome" title="Outcome" open={open}>
        {!card.outcome.available ? (
          <Unavailable reason={card.outcome.reason} />
        ) : (
          <>
            {outcomeText && (
              <p className="kb-sp__outcome-text" data-testid="session-page-outcome-text">
                {outcomeText}
              </p>
            )}
            {detail && <ChecksSection row={detail} detail={detail} />}
            {notCommitted && notCommitted.files.length > 0 && (
              <div className="kb-sp__notcommitted" data-testid="session-page-notcommitted">
                <ul className="kb-sp__list">
                  {notCommitted.files.map((f) => (
                    <li key={f}>
                      <code>{f}</code>
                    </li>
                  ))}
                </ul>
                {notCommitted.caption && (
                  <p className="kb-sp__caption">{notCommitted.caption}</p>
                )}
              </div>
            )}
          </>
        )}
      </Section>

      <Section name="transcript" title="Transcript" open={open}>
        <p className="kb-sp__links">
          <Link to={replayUrl(card.kb, sid)} data-testid="session-transcript-replay">
            replay this session <Icon.Arrow aria-hidden="true" />
          </Link>
          {transcriptHref && (
            <>
              {" · "}
              <Link to={transcriptHref} data-testid="session-transcript-reader">
                open transcript <Icon.Arrow aria-hidden="true" />
              </Link>
            </>
          )}
        </p>
      </Section>

      <ChildrenSection sid={sid} lane={card.children} open={open} />

      <Section name="files" title={`Files · ${card.files.available ? card.files.total : 0}`} open={open}>
        {!card.files.available ? (
          <Unavailable reason={card.files.reason} />
        ) : (
          <>
            <p className="kb-sp__total" data-testid="session-files-total">
              {card.files.total} files · {card.files.read} read ·{" "}
              {card.files.edited} edited · {card.files.wrote} wrote
            </p>
            <ul className="kb-sp__list">
              {card.files.files.map((f) => (
                <li key={f.path}>
                  <code>{f.path}</code>
                  <span className="kb-sp__meta"> {f.action}</span>
                  {f.via_subagent && (
                    <span className="kb-sp__meta"> via subagent</span>
                  )}
                  {notCommittedFiles.has(f.path) && (
                    <span
                      className="kb-sp__marker"
                      data-testid="session-files-notcommitted"
                    >
                      {" "}
                      (not committed)
                    </span>
                  )}
                </li>
              ))}
            </ul>
          </>
        )}
      </Section>

      <Section
        name="decisions"
        title={`Decisions · ${card.decisions.available ? card.decisions.total : 0}`}
        open={open}
      >
        {!card.decisions.available ? (
          <Unavailable reason={card.decisions.reason} />
        ) : (
          <>
            <p className="kb-sp__total">{card.decisions.total} total</p>
            {card.decisions.decisions.map((d, i) => (
              <div key={i} className="kb-sp__entry">
                <p className="kb-sp__entry-kind">{d.kind}</p>
                <p>{d.prompt}</p>
                {d.answer && <p>{d.answer}</p>}
              </div>
            ))}
          </>
        )}
      </Section>

      <Section
        name="commits"
        title={`Commits · ${card.commits.available ? card.commits.total : 0}`}
        open={open}
      >
        {!card.commits.available ? (
          <Unavailable reason={card.commits.reason} />
        ) : (
          <CommitsSection
            commits={card.commits.commits.map((c) => ({
              ...c,
              trailers: [],
            }))}
            loading={false}
          />
        )}
      </Section>

      {/* A5 — the code-side lane (`kbc-session-card/1`), fetched
          cross-origin through the doclens client. Renders nothing when
          the corpus has no code_url. For a subagent child the ROOT sid is
          passed (kb-code does no root resolution and trailer attribution
          stamps the MAIN sid); a root row has root_session_id null and
          falls back to its own sid. Deliberately NOT the parent fallback
          `treeTarget` uses above. */}
      <CodeLaneSection sid={sum.root_session_id ?? sid} kb={card.kb} />

      <Section name="memory" title="Memory" open={open}>
        {!card.memory.available ? (
          <Unavailable reason={card.memory.reason} />
        ) : (
          <>
            <p className="kb-sp__total" data-testid="session-memory-produced-total">
              {card.memory.produced_total} produced
            </p>
            <ul className="kb-sp__list">
              {card.memory.produced.map((id) => (
                <li key={id}>
                  <code>{id}</code>
                </li>
              ))}
            </ul>
            <p className="kb-sp__total" data-testid="session-memory-recalled-total">
              {card.memory.recalled.length} recalled
            </p>
            <ul className="kb-sp__list">
              {card.memory.recalled.map((r, i) => (
                <li key={`${r.kb}:${r.id}:${i}`}>
                  {r.source_relative ? (
                    <Link
                      to={artifactHref(r.kb, r.source_relative)}
                      data-testid="session-memory-recalled-link"
                    >
                      {r.title || r.id}
                    </Link>
                  ) : (
                    <span>{r.title || r.id}</span>
                  )}
                  <span className="kb-sp__meta"> {r.kb}</span>
                  {r.pos != null && (
                    <span className="kb-sp__meta" data-testid="session-memory-pos">
                      {" "}
                      pos {r.pos}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          </>
        )}
      </Section>

      <Section name="slate" title="Slate" open={open}>
        {!card.slate.available ? (
          <Unavailable reason={card.slate.reason} />
        ) : card.slate.slug == null ? (
          <p className="kb-sp__empty">no slate named</p>
        ) : (
          <>
            <p>
              <Link
                to={`/slates/${encodeURIComponent(card.slate.slug)}`}
                data-testid="session-slate-link"
              >
                {card.slate.slug}
              </Link>
              {card.slate.head_seq != null && (
                <span className="kb-sp__meta"> · seq {card.slate.head_seq}</span>
              )}
            </p>
            <p className="kb-sp__total">{card.slate.total} posts</p>
            <ul className="kb-sp__list">
              {card.slate.posts.map((p) => (
                <li key={`${p.seq}:${p.id}`}>
                  <span className="kb-sp__entry-kind">{p.kind}</span> {p.line}
                </li>
              ))}
            </ul>
          </>
        )}
      </Section>

      <Section name="comments" title="Comments" open={open}>
        {!card.comments.available ? (
          <Unavailable reason={card.comments.reason} />
        ) : card.comments.open_count === 0 && card.comments.raised_count === 0 ? (
          <p className="kb-sp__empty">no comments</p>
        ) : (
          <>
            <p className="kb-sp__total">
              {card.comments.open_count} open · {card.comments.raised_count} raised
            </p>
            <ul className="kb-sp__list">
              {[...card.comments.open, ...card.comments.raised].map((c) => (
                <li key={c.comment_id}>
                  <span className="kb-sp__entry-kind">{c.author}</span>{" "}
                  {c.body}
                  {c.file_label && (
                    <span className="kb-sp__meta"> {c.file_label}</span>
                  )}
                </li>
              ))}
            </ul>
          </>
        )}
        {!card.readings.available ? (
          <Unavailable reason={card.readings.reason} />
        ) : (
          <>
            <p className="kb-sp__total">{card.readings.total} readings</p>
            <ul className="kb-sp__list">
              {card.readings.items.map((r) => (
                <li key={`${r.kb}:${r.artifact_id}`}>
                  {r.source_relative ? (
                    <Link to={artifactHref(r.kb, r.source_relative)}>
                      {r.title || r.artifact_id}
                    </Link>
                  ) : (
                    <code>{r.artifact_id}</code>
                  )}
                  {r.read_pct != null && (
                    <span className="kb-sp__meta"> {r.read_pct}%</span>
                  )}
                </li>
              ))}
            </ul>
          </>
        )}
      </Section>

      {sum.segment_chain.length >= 2 && (
        <Section name="segments" title="Segments" open={open}>
          <p className="kb-sp__chain">
            {sum.segment_chain.map((id, i) => (
              <span key={`${id}:${i}`}>
                {i > 0 && <Icon.Arrow aria-hidden="true" />}
                <Link to={leaf(id)}>{id}</Link>
              </span>
            ))}
          </p>
        </Section>
      )}

      <Section name="resume" title="Resume" open={open}>
        <ResumeCommand sessionId={sid} resume={sum.resume} />
      </Section>
    </main>
  );
}
