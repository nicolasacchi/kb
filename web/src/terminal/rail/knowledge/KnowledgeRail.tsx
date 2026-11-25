// RAILKNOW — the Knowledge tab of the terminal's right panel. Reads only:
// memories injected into / written by the bound session, a project memory
// search, similar past sessions, and what the session opened or cited. Every
// lane is its own component with its own query and its own freshness caption.
// Surfaced, never scored. All text is rendered as text nodes. No xterm import.

import { useEffect, useState, type ReactElement } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { MemoryScope } from "../../../api/client";
import { useTermRail } from "../context";
import { LaneFrame, LaneState } from "./Lane";
import { MemoryRow } from "./MemoryRow";
import {
  RESEARCH_CAP,
  fromRecall,
  groupResearch,
  sessionHref,
  type MemoryRowData,
} from "./knowledgeRows";
import {
  knowledgeKey,
  useKnowledgeDetail,
  useKnowledgeMemories,
  useKnowledgeRecalls,
  useKnowledgeResearch,
  useKnowledgeSearch,
  useKnowledgeSimilar,
} from "./useKnowledge";
import "./knowledge.css";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : "Request failed";
}

function InjectedLane({ sid }: { sid: string | undefined }): ReactElement {
  const qc = useQueryClient();
  const q = useKnowledgeRecalls(sid);
  let body: ReactElement;
  if (!sid) body = <LaneState lane="injected" kind="no-session" />;
  else if (q.isPending) body = <LaneState lane="injected" kind="loading" />;
  else if (q.isError)
    body = (
      <LaneState
        lane="injected"
        kind="error"
        reason={errMsg(q.error)}
        onRetry={() => void qc.invalidateQueries({ queryKey: knowledgeKey("recalls", sid) })}
      />
    );
  else if (q.data.kind === "not-captured") body = <LaneState lane="injected" kind="not-captured" />;
  else if (q.data.data.recalls.length === 0) body = <LaneState lane="injected" kind="empty" />;
  else
    body = (
      <ul className="kb-knowrail__list" role="list">
        {q.data.data.recalls.map((h, i) => (
          <MemoryRow key={`${h.kb}/${h.id}/${i}`} row={fromRecall(h)} />
        ))}
      </ul>
    );
  return (
    <LaneFrame lane="injected" title="Injected into this session" fresh="From last capture" source="recall ledger">
      {body}
    </LaneFrame>
  );
}

function WrittenLane({ sid }: { sid: string | undefined }): ReactElement {
  const qc = useQueryClient();
  const q = useKnowledgeMemories(sid);
  let body: ReactElement;
  if (!sid) body = <LaneState lane="written" kind="no-session" />;
  else if (q.isPending) body = <LaneState lane="written" kind="loading" />;
  else if (q.isError)
    body = (
      <LaneState
        lane="written"
        kind="error"
        reason={errMsg(q.error)}
        onRetry={() => void qc.invalidateQueries({ queryKey: knowledgeKey("memories", sid) })}
      />
    );
  else if (q.data.kind === "not-captured") body = <LaneState lane="written" kind="not-captured" />;
  else if (q.data.data.memories.length === 0) body = <LaneState lane="written" kind="empty" />;
  else
    body = (
      <ul className="kb-knowrail__list" role="list">
        {q.data.data.memories.map((m) => {
          const row: MemoryRowData = { kb: m.kb, id: m.id, title: m.title, source_relative: m.source_relative };
          return <MemoryRow key={`${m.kb}/${m.id}`} row={row} />;
        })}
      </ul>
    );
  return (
    <LaneFrame lane="written" title="Written by this session" fresh="From last capture" source="session memories">
      {body}
    </LaneFrame>
  );
}

const SCOPES: ReadonlyArray<{ id: MemoryScope; label: string }> = [
  { id: "project", label: "Project" },
  { id: "global", label: "Global" },
  { id: "all", label: "All" },
];

function SearchLane(): ReactElement {
  const qc = useQueryClient();
  const [text, setText] = useState("");
  const [debounced, setDebounced] = useState("");
  const [scope, setScope] = useState<MemoryScope>("project");
  useEffect(() => {
    const t = window.setTimeout(() => setDebounced(text.trim().length >= 2 ? text.trim() : ""), 250);
    return () => window.clearTimeout(t);
  }, [text]);
  const q = useKnowledgeSearch(scope, debounced);
  let body: ReactElement;
  if (q.isPending) body = <LaneState lane="search" kind="loading" />;
  else if (q.isError)
    body = (
      <LaneState
        lane="search"
        kind="error"
        reason={errMsg(q.error)}
        onRetry={() => void qc.invalidateQueries({ queryKey: ["memories", "rail", scope, debounced] })}
      />
    );
  else if (q.data.hits.length === 0) body = <LaneState lane="search" kind="empty" />;
  else
    body = (
      <ul className="kb-knowrail__list" role="list">
        {q.data.hits.map((h) => (
          <MemoryRow
            key={`${h.kb}/${h.id}`}
            row={{ kb: h.kb, id: h.id, title: h.title, source_relative: h.source_relative, summary: h.summary }}
          />
        ))}
      </ul>
    );
  return (
    <LaneFrame lane="search" title="Project memory" fresh="Live search" source="memory recall">
      <input
        type="search"
        className="kb-knowrail__input"
        data-testid="kn-search-input"
        aria-label="Search project memory"
        placeholder="Search memories"
        value={text}
        onChange={(e) => setText(e.target.value)}
      />
      <div className="kb-knowrail__scopes" role="radiogroup" aria-label="Memory scope">
        {SCOPES.map((s, i) => (
          <button
            key={s.id}
            type="button"
            role="radio"
            aria-checked={scope === s.id}
            tabIndex={scope === s.id ? 0 : -1}
            onKeyDown={(e) => {
              const d = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
              if (d === 0) return;
              e.preventDefault();
              const next = SCOPES[(i + d + SCOPES.length) % SCOPES.length];
              setScope(next.id);
              (e.currentTarget.parentElement?.querySelector(`[data-testid="kn-scope-${next.id}"]`) as HTMLElement | null)?.focus();
            }}
            className="kb-knowrail__btn"
            data-testid={`kn-scope-${s.id}`}
            onClick={() => setScope(s.id)}
          >
            {s.label}
          </button>
        ))}
      </div>
      {body}
    </LaneFrame>
  );
}

function SimilarLane({ sid }: { sid: string | undefined }): ReactElement {
  const qc = useQueryClient();
  const d = useKnowledgeDetail(sid);
  const detail = d.data?.kind === "ok" ? d.data.data : undefined;
  const prompt = detail?.first_user_prompt?.trim() || undefined;
  const q = useKnowledgeSimilar(sid, prompt, detail?.project_key ?? undefined);
  const retry = () => {
    void qc.invalidateQueries({ queryKey: knowledgeKey("detail", sid) });
    void qc.invalidateQueries({ queryKey: knowledgeKey("similar", sid) });
  };
  let body: ReactElement;
  if (!sid) body = <LaneState lane="similar" kind="no-session" />;
  else if (d.isPending) body = <LaneState lane="similar" kind="loading" />;
  else if (d.isError) body = <LaneState lane="similar" kind="error" reason={errMsg(d.error)} onRetry={retry} />;
  else if (d.data?.kind === "not-captured") body = <LaneState lane="similar" kind="not-captured" />;
  else if (!prompt) body = <LaneState lane="similar" kind="empty" reason="No prompt to compare yet" />;
  else if (q.isPending) body = <LaneState lane="similar" kind="loading" />;
  else if (q.isError)
    body = (
      <LaneState
        lane="similar"
        kind="error"
        reason={errMsg(q.error)}
        onRetry={retry}
      />
    );
  else {
    const rows = q.data.sessions.filter((s) => s.session_id !== sid);
    body =
      rows.length === 0 ? (
        <LaneState lane="similar" kind="empty" />
      ) : (
        <ul className="kb-knowrail__list" role="list">
          {rows.map((s) => (
            <li key={`${s.kb}/${s.session_id}`} className="kb-knowrail__row" data-testid="kn-similar-row">
              <p className="kb-knowrail__title">{s.display_name}</p>
              <div className="kb-knowrail__rowhead">
                <span className="kb-knowrail__chip">{s.age_days} d ago</span>
                {s.committed ? <span className="kb-knowrail__chip">committed</span> : null}
                {s.stale ? <span className="kb-knowrail__chip">stale</span> : null}
              </div>
              {s.summary ? <p className="kb-knowrail__summary">{s.summary}</p> : null}
              <div className="kb-knowrail__tools">
                <a
                  className="kb-knowrail__btn"
                  data-testid="kn-similar-open"
                  href={sessionHref(s.session_id)}
                  target="_blank"
                  rel="noreferrer"
                >
                  Open session
                </a>
              </div>
            </li>
          ))}
        </ul>
      );
  }
  return (
    <LaneFrame lane="similar" title="Similar past sessions" fresh="From last capture" source="recollect">
      {body}
    </LaneFrame>
  );
}

function ResearchLane({ sid }: { sid: string | undefined }): ReactElement {
  const qc = useQueryClient();
  const q = useKnowledgeResearch(sid);
  let body: ReactElement;
  if (!sid) body = <LaneState lane="research" kind="no-session" />;
  else if (q.isPending) body = <LaneState lane="research" kind="loading" />;
  else if (q.isError)
    body = (
      <LaneState
        lane="research"
        kind="error"
        reason={errMsg(q.error)}
        onRetry={() => void qc.invalidateQueries({ queryKey: knowledgeKey("research", sid) })}
      />
    );
  else if (q.data.kind === "not-captured") body = <LaneState lane="research" kind="not-captured" />;
  else if (q.data.data.research.length === 0) body = <LaneState lane="research" kind="empty" />;
  else {
    const { groups, shown, total } = groupResearch(q.data.data.research);
    body = (
      <>
        {groups.map((g) => (
          <div key={g.kind} className="kb-knowrail__group" data-testid="kn-research-group" data-kind={g.kind}>
            <h4 className="kb-knowrail__sub">{g.label}</h4>
            <ul className="kb-knowrail__list" role="list">
              {g.rows.map((r, i) => (
                <li key={i} className="kb-knowrail__row" data-testid="kn-research-row">
                  <p className="kb-knowrail__query" title={r.query}>
                    {r.query}
                  </p>
                </li>
              ))}
            </ul>
          </div>
        ))}
        {total > RESEARCH_CAP ? (
          <p className="kb-knowrail__fresh" data-testid="kn-research-cap">
            Showing {shown} of {total}
          </p>
        ) : null}
      </>
    );
  }
  return (
    <LaneFrame lane="research" title="Opened or cited" fresh="From last capture" source="session research">
      {body}
    </LaneFrame>
  );
}

export function KnowledgeRail(): ReactElement {
  const { sid } = useTermRail();
  return (
    <div className="kb-knowrail" data-testid="knowledge-rail">
      <InjectedLane sid={sid} />
      <WrittenLane sid={sid} />
      <SearchLane />
      <SimilarLane sid={sid} />
      <ResearchLane sid={sid} />
    </div>
  );
}
