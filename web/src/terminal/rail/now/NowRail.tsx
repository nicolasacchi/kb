// RAILNOW — the Now tab of the terminal's right panel: one calm column of
// labelled sections (needs-you, activity, status, latest answer, asks for you,
// sibling terminals, session stats), each from data the page already holds and
// each stating its freshness. Every string from a session or slate is a text
// node. No xterm import, no new stream and no timer that fetches (the one
// client tick only re-words "4m ago" labels). The prompt queue is PQUI's: the
// last section is its empty seam.

import { useMemo, type ReactElement } from "react";
import { useNavigate } from "react-router-dom";
import { StateCard } from "@kb/ui/StateCard";
import { useLiveSessions } from "../../../hooks/useLiveSessions";
import { useConversation } from "../../../hooks/useConversation";
import { useNowTick } from "../../../hooks/useNowTick";
import { useSessionCard } from "../../../hooks/useSessionCard";
import { useSessionDetail } from "../../../hooks/useSessions";
import { useSlateBoard } from "../../../hooks/useSlates";
import { useTerms } from "../../useTerms";
import { useTermAttention } from "../../notify/attention";
import { buildHubUrl } from "../../hub/termHub";
import { useTermRail } from "../context";
import { isOpenAsk, splitAsks } from "../board/boardRows";
import { buildNow, type NeedsView, type NowSection } from "./nowModel";
import { NowQueueSeam } from "./NowQueueSeam";
import "./nowRail.css";

const NEEDS_LABEL: Record<NeedsView["kind"], string> = {
  permission: "Permission needed",
  question: "A question is waiting",
  plan: "A plan awaits approval",
  waiting: "Waiting for you",
};

function Head({ title, fresh, id }: { title: string; fresh: string; id: string }): ReactElement {
  return (
    <header className="kb-nowrail__sechead">
      <h3 className="kb-nowrail__h">{title}</h3>
      {fresh !== "" ? (
        <span className="kb-nowrail__fresh" data-testid={`now-fresh-${id}`}>
          {fresh}
        </span>
      ) : null}
    </header>
  );
}

function Body({ s, onOpen }: { s: NowSection; onOpen: (termId: string) => void }): ReactElement | null {
  switch (s.id) {
    case "needs":
      return (
        <div className="kb-nowrail__needs" data-testid="now-needs-body" data-kind={s.needs.kind}>
          <p className="kb-nowrail__lead">
            {NEEDS_LABEL[s.needs.kind]}
            {s.needs.tool ? <span className="kb-nowrail__tool"> {s.needs.tool}</span> : null}
            {s.needs.since ? <span className="kb-nowrail__age" data-testid="now-needs-age"> {s.needs.since}</span> : null}
          </p>
          <p className="kb-nowrail__q" data-testid="now-needs-text" data-source={s.needs.textSource}>
            {s.needs.text}
          </p>
          {s.needs.lastLine ? (
            <p className="kb-nowrail__muted" data-testid="now-needs-last">
              {s.needs.lastLine}
            </p>
          ) : null}
        </div>
      );
    case "activity": {
      const a = s.activity;
      if (!a.lastTool && !a.background && a.subagents === null) {
        return <p className="kb-nowrail__none">Nothing reported yet.</p>;
      }
      return (
        <ul className="kb-nowrail__list" role="list">
          {a.lastTool ? (
            <li data-testid="now-last-tool">
              Last tool started: {a.lastTool.name}
              {a.lastTool.since ? ` (${a.lastTool.since})` : ""}
            </li>
          ) : null}
          {a.background ? (
            <li data-testid="now-background">
              Background:
              {a.background.agent > 0 ? ` ${a.background.agent} agent` : ""}
              {a.background.shell > 0 ? ` ${a.background.shell} shell` : ""}
              {a.background.other > 0 ? ` ${a.background.other} other` : ""}
            </li>
          ) : null}
          {a.subagents !== null ? <li data-testid="now-subagents">Subagents: {a.subagents}</li> : null}
        </ul>
      );
    }
    case "status":
      return s.chips.length === 0 ? (
        <p className="kb-nowrail__none">No status reported.</p>
      ) : (
        <ul className="kb-nowrail__chips" role="list">
          {s.chips.map((c) => (
            <li key={c.id} className="kb-nowrail__chip" data-chip={c.id} title={c.title}>
              {c.label}
            </li>
          ))}
        </ul>
      );
    case "answer":
      return (
        <p className="kb-nowrail__answer" data-testid="now-answer">
          {s.text}
        </p>
      );
    case "asks":
      return s.open.length === 0 ? (
        <p className="kb-nowrail__none" data-testid="now-asks-none">
          None open.
        </p>
      ) : (
        <div>
          <p className="kb-nowrail__lead" data-testid="now-asks-count">
            {s.open.length} open {s.open.length === 1 ? "ask" : "asks"} for you. Open the Board tab to answer.
          </p>
          <ul className="kb-nowrail__list" role="list">
            {s.open.map((c) => (
              <li key={c.seq} data-testid="now-ask">
                {c.line}
              </li>
            ))}
          </ul>
        </div>
      );
    case "siblings":
      return s.siblings.length === 0 ? (
        <p className="kb-nowrail__none" data-testid="now-siblings-none">
          No other terminals on this project.
        </p>
      ) : (
        <ul className="kb-nowrail__list" role="list">
          {s.siblings.map((x) => (
            <li key={x.id}>
              <button
                type="button"
                className="kb-nowrail__sib"
                data-testid="now-sibling"
                data-term={x.id}
                data-mark={x.mark ?? undefined}
                onClick={() => onOpen(x.id)}
              >
                <span className="kb-nowrail__sibtitle">{x.title}</span>
                <span className="kb-nowrail__muted">
                  {x.harness} - {x.state}
                  {x.mark === "asks" ? " - asking" : x.mark === "finished" ? " - finished" : ""}
                </span>
              </button>
            </li>
          ))}
        </ul>
      );
    case "stats":
      return s.stats.rows.length === 0 ? (
        <p className="kb-nowrail__none" data-testid="now-stats-none">
          Stats unavailable{s.stats.reason ? `: ${s.stats.reason}` : ""}.
        </p>
      ) : (
        <dl className="kb-nowrail__stats" data-testid="now-stats">
          {s.stats.rows.map((r) => (
            <div key={r.id} className="kb-nowrail__stat" data-stat={r.id}>
              <dt>{r.label}</dt>
              <dd>
                {r.value}
                {r.source ? <span className="kb-nowrail__muted"> ({r.source})</span> : null}
              </dd>
            </div>
          ))}
        </dl>
      );
    case "queue":
      return null;
  }
}

export function NowRail(): ReactElement {
  const { termId, sid } = useTermRail();
  const navigate = useNavigate();
  const nowMs = useNowTick(30_000);
  const { rows: live } = useLiveSessions();
  const row = sid ? (live.find((r) => r.session_id === sid) ?? null) : null;
  const { card } = useSessionCard(sid ?? "");
  // The conversation is only needed to word a pending question: skip the read
  // unless the session is blocked (the Conversation tab shares the same key).
  const conv = useConversation(row?.blocked ? sid : undefined);
  const { detail } = useSessionDetail(sid ?? null);
  const { board } = useSlateBoard(detail?.project_slug ?? "");
  const { terms, isError } = useTerms();
  const marks = useTermAttention();

  const asks = useMemo(() => (board ? splitAsks(board.sections.ask).forYou.filter(isOpenAsk) : null), [board]);
  const nowUnix = Math.floor(nowMs / 1000);
  const model = useMemo(
    () =>
      buildNow({
        termId,
        sid,
        row,
        card,
        conv: conv.data?.kind === "ok" ? conv.data.conversation : null,
        asks,
        terms,
        termsOk: !isError,
        live,
        marks,
        nowUnix,
      }),
    [termId, sid, row, card, conv.data, asks, terms, isError, live, marks, nowUnix],
  );

  const open = (id: string): void => navigate(buildHubUrl({ id }));

  return (
    <div className="kb-nowrail" data-testid="now-rail" data-linked={model.linked ? "1" : "0"}>
      {!model.linked ? (
        <StateCard kind="empty" size="inline" title="Not linked to a session yet" reason="This terminal links on the first harness beat." />
      ) : null}
      {model.sections.map((s) =>
        s.id === "queue" ? (
          <NowQueueSeam key="queue" />
        ) : (
          <section key={s.id} className="kb-nowrail__section" data-testid={`now-sec-${s.id}`} aria-label={s.title}>
            <Head id={s.id} title={s.title} fresh={s.fresh} />
            <Body s={s} onOpen={open} />
          </section>
        ),
      )}
    </div>
  );
}
