// RAILBOARD — the Board tab of the terminal's right panel: the PROJECT slate
// for the terminal's bound session. Open asks first (addressed-to-operator on
// top, answerable in place), then takes, warnings, the NOW line and this
// session's own posts. A read of the existing slate board projection, live
// through the `slate.updated` invalidation of ["slate", slug] (#23/#24: no
// stream and no timer here). Posts are data: every line renders as a text
// node, and `body` is never passed on (the slate page owns the long form).
// No xterm import (registerTabs.ts keeps this out of the terminal chunk).

import { useId, useMemo, useState, type FormEvent, type ReactElement } from "react";
import { StateCard } from "@kb/ui/StateCard";
import type { SlateBoardCard } from "../../../api/slateTypes";
import SlateCard from "../../../components/slate/SlateCard";
import { useSessionDetail } from "../../../hooks/useSessions";
import { SPA_PROV, useSlateBoard, useSlatePost } from "../../../hooks/useSlates";
import { toast } from "../../../lib/toast";
import { orderCards } from "../../../lib/slateLanes";
import { usePhonePanel } from "../../../hooks/useIsMobile";
import { useTermRail } from "../context";
import { boardAsOf, flattenAsks, freshness, isNoSlate, isOpenAsk, ownPosts, splitAsks } from "./boardRows";
import "./boardRail.css";

const MAX_ANSWER = 200;

/** Posts are data: the card never gets a body (it would render Markdown). */
const bare = (c: SlateBoardCard): SlateBoardCard => ({ ...c, body: null, has_sketch: false });

function AnswerForm({
  card,
  onSubmit,
  onCancel,
}: {
  card: SlateBoardCard;
  onSubmit: (line: string) => Promise<boolean>;
  onCancel: () => void;
}): ReactElement {
  const [line, setLine] = useState("");
  const [busy, setBusy] = useState(false);
  const id = useId();
  const canSubmit = line.trim().length > 0 && !busy;
  const submit = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    if (!canSubmit) return;
    setBusy(true);
    try {
      if (await onSubmit(line.trim())) onCancel();
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className="kb-boardrail__form" data-testid="board-answer-form" onSubmit={(e) => void submit(e)}>
      <label className="kb-boardrail__label" htmlFor={id}>
        Answer to #{card.seq}
      </label>
      <input
        id={id}
        className="kb-boardrail__input"
        data-testid="board-answer-input"
        type="text"
        maxLength={MAX_ANSWER}
        value={line}
        autoFocus
        onChange={(e) => setLine(e.target.value)}
      />
      <div className="kb-boardrail__formtools">
        <button type="submit" className="kb-boardrail__btn" data-testid="board-answer-send" disabled={!canSubmit}>
          Send answer
        </button>
        <button type="button" className="kb-boardrail__btn" data-testid="board-answer-cancel" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}

function Section({
  id,
  title,
  cards,
  children,
}: {
  id: string;
  title: string;
  cards: readonly SlateBoardCard[];
  children: ReactElement | ReactElement[] | null;
}): ReactElement {
  return (
    <section className="kb-boardrail__section" data-testid={`board-sec-${id}`} aria-label={title}>
      <header className="kb-boardrail__sechead">
        <h3 className="kb-boardrail__h">{title}</h3>
        <span className="kb-boardrail__fresh" data-testid={`board-fresh-${id}`}>
          {freshness(cards)}
        </span>
      </header>
      {cards.length === 0 ? <p className="kb-boardrail__none">None</p> : <ul className="kb-boardrail__list" role="list">{children}</ul>}
    </section>
  );
}

export function BoardRail(): ReactElement {
  const { sid } = useTermRail();
  const { detail, loading: detailLoading } = useSessionDetail(sid ?? null);
  const slug = detail?.project_slug ?? "";
  const { board, loading, error } = useSlateBoard(slug);
  const phone = usePhonePanel();
  const [answering, setAnswering] = useState<number | null>(null);
  const post = useSlatePost(slug, {
    // The rail never overrides a refusal: a friction prompt is shown as the toast it is.
    confirm: async (o) => {
      toast.err(o.body);
      return false;
    },
    toastErr: toast.err,
    toastInfo: toast.info,
  });

  const groups = useMemo(() => splitAsks(board?.sections.ask ?? []), [board]);
  const asks = useMemo(() => flattenAsks(groups), [groups]);
  const own = useMemo(() => (board ? ownPosts(board, sid) : []), [board, sid]);

  const wrap = (child: ReactElement): ReactElement => (
    <div className="kb-boardrail" data-testid="board-rail" data-phone={phone ? "1" : undefined}>
      {child}
    </div>
  );
  const empty = (title: string, reason?: string): ReactElement =>
    wrap(<StateCard kind="empty" size="inline" title={title} reason={reason} />);

  if (!sid) return empty("No session linked yet", "This terminal has no bound session; it links on the first harness beat.");
  if (detailLoading) return wrap(<StateCard kind="loading" size="inline" title="Loading the session" />);
  if (!detail) return wrap(<StateCard kind="degraded" size="inline" title="Could not load the session" reason="The board needs the session's project." />);
  if (!slug) return empty("No slate for this project");
  if (loading) return wrap(<StateCard kind="loading" size="inline" title="Loading the board" />);
  if (!board) {
    if (isNoSlate(error)) return empty("No slate for this project", "Nothing has been posted to it yet.");
    return wrap(<StateCard kind="degraded" size="inline" title="Could not load the board" reason={error ?? undefined} />);
  }

  const sec = board.sections;
  const card = (c: SlateBoardCard, extra?: { onAnswer?: (c: SlateBoardCard) => void }): ReactElement => (
    <SlateCard card={bare(c)} mobile={phone} repo={slug} actions={extra} />
  );

  return wrap(
    <>
      <p className="kb-boardrail__asof" data-testid="board-asof">
        {boardAsOf(board)}
      </p>
      <Section id="asks" title="Asks" cards={asks}>
        {asks.map((c) => {
          const open = isOpenAsk(c);
          return (
            <li
              key={c.seq}
              className={`kb-boardrail__item${open ? "" : " kb-boardrail__item--answered"}`}
              data-testid="board-ask"
              data-seq={c.seq}
              data-to={c.to ?? "any"}
              data-open={open ? "1" : "0"}
            >
              {card(c, open ? { onAnswer: (x) => setAnswering(x.seq) } : undefined)}
              {open && answering === c.seq ? (
                <AnswerForm
                  card={c}
                  onCancel={() => setAnswering(null)}
                  onSubmit={async (line) =>
                    (await post(
                      { kind: "answer", line, re: c.seq, ...(c.topic ? { topic: c.topic } : {}), prov: SPA_PROV },
                      { targetKind: "ask", holder: c.who.tag },
                    )) !== null
                  }
                />
              ) : null}
            </li>
          );
        })}
      </Section>
      <Section id="takes" title="Takes" cards={sec.take}>
        {orderCards(sec.take).map((c) => (
          <li key={c.seq} className="kb-boardrail__item" data-testid="board-take">
            {card(c)}
          </li>
        ))}
      </Section>
      <Section id="warnings" title="Warnings" cards={sec.warn}>
        {orderCards(sec.warn).map((c) => (
          <li key={c.seq} className="kb-boardrail__item" data-testid="board-warn">
            {card(c)}
          </li>
        ))}
      </Section>
      <Section id="now" title="Now" cards={sec.now}>
        {orderCards(sec.now).map((c) => (
          <li key={c.seq} className="kb-boardrail__item" data-testid="board-now">
            {card(c)}
          </li>
        ))}
      </Section>
      <Section id="mine" title="This session's posts" cards={own}>
        {own.map((c) => (
          <li key={c.seq} className="kb-boardrail__item" data-testid="board-own">
            {card(c)}
          </li>
        ))}
      </Section>
    </>,
  );
}
