// CONVRAIL — the Conversation tab of the terminal's right panel: one row per
// human turn of the terminal's bound session (prompt, final-answer excerpt,
// asks, commits), read from the capture-scoped `fields=conversation`
// projection. Derived and honest: freshness is "as of the last capture", never
// live. All text is rendered as text nodes. No xterm import (registerTabs.ts
// keeps this out of the terminal chunk).

import { useEffect, useRef, useState, type ReactElement } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { StateCard } from "@kb/ui/StateCard";
import type { ConvRow } from "../../api/generated/ConvRow";
import { conversationKey, useConversation } from "../../hooks/useConversation";
import { useSessionDetail } from "../../hooks/useSessions";
import { toast } from "../../lib/toast";
import { tryWriteClipboardText } from "../clipboard";
import { clearPendingJump, hasPendingJump, usePendingJumpTick } from "../needs/pendingJump";
import { useTermRail } from "./context";
import {
  CONV_FILTERS,
  LIVE_CAPTURE_HINT,
  copyText,
  excerpt,
  filterRows,
  freshnessText,
  jumpHref,
  windowCaption,
  type ConvFilter,
} from "./conversationRows";
import "./conversationRail.css";

const FILTER_LABEL: Record<ConvFilter, string> = {
  all: "All",
  questions: "Questions",
  answers: "Answers",
};

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function Row({ row, href }: { row: ConvRow; href: string | null }): ReactElement {
  const onCopy = async (): Promise<void> => {
    if (!(await tryWriteClipboardText(copyText(row)))) toast.err("Could not copy to the clipboard");
  };
  return (
    <li className="kb-convrail__row" data-testid="conv-row" data-turn={row.id}>
      <div className="kb-convrail__head">
        <span className="kb-convrail__n">#{row.n}</span>
        <span className="kb-convrail__tools">
          {href ? (
            <a
              className="kb-convrail__btn"
              data-testid="conv-jump"
              href={href}
              target="_blank"
              rel="noreferrer"
              aria-label={`Open turn ${row.n} in the session reader`}
            >
              Open
            </a>
          ) : null}
          <button
            type="button"
            className="kb-convrail__btn"
            data-testid="conv-copy"
            aria-label={`Copy turn ${row.n}`}
            onClick={() => void onCopy()}
          >
            Copy
          </button>
        </span>
      </div>
      <p className="kb-convrail__prompt">{row.prompt}</p>
      {row.answer === null ? (
        <p className="kb-convrail__answer kb-convrail__answer--none">No answer yet</p>
      ) : (
        <p className="kb-convrail__answer">{excerpt(row.answer)}</p>
      )}
      {row.asks.length > 0 ? (
        <ul className="kb-convrail__asks" role="list">
          {row.asks.map((a, i) => (
            <li key={i} className="kb-convrail__ask" data-testid="conv-ask" data-kind={a.kind}>
              <span className="kb-convrail__q">{a.q}</span>
              {a.options.length > 0 ? (
                <span className="kb-convrail__opts">{a.options.join(" · ")}</span>
              ) : null}
              {a.pending ? (
                <span className="kb-convrail__chip kb-convrail__chip--pending" data-pending="true">
                  Pending
                </span>
              ) : a.answer !== null ? (
                <span className="kb-convrail__chip">{a.answer}</span>
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
      <div className="kb-convrail__meta">
        {row.commits.map((c) => (
          <span key={c} className="kb-convrail__chip kb-convrail__chip--sha" data-testid="conv-commit">
            {c.slice(0, 7)}
          </span>
        ))}
        {row.files_touched > 0 ? <span className="kb-convrail__count">{plural(row.files_touched, "file")}</span> : null}
        {row.errors > 0 ? <span className="kb-convrail__count">{plural(row.errors, "error")}</span> : null}
        {row.subagents.n > 0 ? <span className="kb-convrail__count">{plural(row.subagents.n, "subagent")}</span> : null}
      </div>
    </li>
  );
}

export function ConversationRail(): ReactElement {
  const { sid } = useTermRail();
  const qc = useQueryClient();
  const q = useConversation(sid);
  const { detail } = useSessionDetail(sid ?? null);
  const [filter, setFilter] = useState<ConvFilter>("all");
  const [now, setNow] = useState<number>(() => Date.now());
  const rootRef = useRef<HTMLDivElement | null>(null);
  const jumpTick = usePendingJumpTick();

  // CONVNEED: a jump request from the needs-you banner scrolls to and focuses
  // the pending row once rows are rendered. A non-ready state leaves the
  // request alive (it expires); a missing pending row just clears it.
  useEffect(() => {
    if (!hasPendingJump()) return;
    const root = rootRef.current;
    if (!root) return;
    const chip = root.querySelector('[data-pending="true"]');
    const row = chip?.closest<HTMLElement>('[data-testid="conv-row"]') ?? null;
    if (!row) {
      if (filter !== "all") {
        setFilter("all");
        return;
      }
      clearPendingJump();
      return;
    }
    clearPendingJump();
    row.tabIndex = -1;
    row.scrollIntoView?.({ block: "nearest" });
    row.focus();
  }, [jumpTick, q.data, filter]);

  // A local re-render tick for the freshness caption: a timer, never a fetch.
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(t);
  }, []);

  if (!sid) {
    return (
      <div className="kb-convrail" data-testid="conv-rail">
        <StateCard
          kind="empty"
          size="inline"
          title="No session linked yet"
          reason="This terminal has no bound session; it links on the first harness beat."
        />
      </div>
    );
  }
  if (q.isPending) {
    return (
      <div className="kb-convrail" data-testid="conv-rail">
        <StateCard kind="loading" size="inline" title="Loading conversation" />
      </div>
    );
  }
  if (!q.data) {
    return (
      <div className="kb-convrail" data-testid="conv-rail">
        <StateCard
          kind="degraded"
          size="inline"
          title="Could not load the conversation"
          actions={[
            {
              label: "Retry",
              key: "retry",
              onClick: () => void qc.invalidateQueries({ queryKey: conversationKey(sid) }),
            },
          ]}
        />
      </div>
    );
  }
  if (q.data.kind === "none") {
    return (
      <div className="kb-convrail" data-testid="conv-rail">
        <StateCard
          kind="empty"
          size="inline"
          title="No capture yet"
          reason="Turns appear here after the session's first capture."
        />
      </div>
    );
  }

  const { conversation, kb } = q.data;
  const rows = conversation.rows;
  if (rows.length === 0) {
    return (
      <div className="kb-convrail" data-testid="conv-rail">
        <StateCard kind="empty" size="inline" title="No turns yet" />
      </div>
    );
  }
  const shown = filterRows(rows, filter);
  const stale = q.isError;
  const win = windowCaption(conversation.total, rows.length);
  const rel = detail?.source_relative;

  return (
    <div className="kb-convrail" data-testid="conv-rail" ref={rootRef}>
      <div className="kb-convrail__bar">
        <div className="kb-convrail__filters" role="radiogroup" aria-label="Filter turns">
          {CONV_FILTERS.map((f) => (
            <button
              key={f}
              type="button"
              role="radio"
              aria-checked={filter === f}
              className="kb-convrail__filter"
              data-testid={`conv-filter-${f}`}
              onClick={() => setFilter(f)}
            >
              {FILTER_LABEL[f]}
            </button>
          ))}
        </div>
        <p className="kb-convrail__fresh" data-testid="conv-fresh">
          {freshnessText(conversation.capture_gen, now)}
        </p>
        <p className="kb-convrail__hint">{LIVE_CAPTURE_HINT}</p>
        {stale ? (
          <p className="kb-convrail__hint" data-testid="conv-stale" role="status">
            Refresh failed; showing the last loaded turns.
          </p>
        ) : null}
        {win ? <p className="kb-convrail__hint" data-testid="conv-window">{win}</p> : null}
      </div>
      {shown.length === 0 ? (
        <StateCard
          kind="empty"
          size="inline"
          title="No matching turns"
          actions={[{ label: "Show all", key: "show-all", onClick: () => setFilter("all") }]}
        />
      ) : (
        <ul className="kb-convrail__list" role="list">
          {shown.map((r) => (
            <Row key={r.id} row={r} href={rel && kb ? jumpHref(kb, rel, r.id) : null} />
          ))}
        </ul>
      )}
    </div>
  );
}
