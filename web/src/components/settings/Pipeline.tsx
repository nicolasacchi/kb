import { useEffect, useState } from "react";
import {
  fetchKbs,
  fetchQueries,
  fetchRuns,
  fetchSources,
  pauseSource,
  recomputeAtlas,
  reclusterAtlas,
  reindexKb,
  reindexSource,
  resumeSource,
  type KbSummary,
  type QueryEntry,
  type RunEntry,
  type SourceSummary,
} from "../../api/client";
import { sse } from "../../api/sse";
import { Icon } from "../icons";

// Pipeline tab. Sources / runs / queries for each kb on the daemon.
// Read-only in S2 — pause/resume + reindex buttons land in S4 (they
// hang off the rows already exposed here, so the layout doesn't
// shift). One open accordion per kb so multi-kb daemons stay
// scrollable.
export default function Pipeline() {
  const [kbs, setKbs] = useState<KbSummary[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const ctrl = new AbortController();
    fetchKbs(ctrl.signal)
      .then((ks) => {
        if (!cancelled) setKbs(ks);
      })
      .catch((e) => {
        if (!cancelled && e?.name !== "AbortError") {
          setError(String(e?.message ?? e));
        }
      });
    return () => {
      cancelled = true;
      ctrl.abort();
    };
  }, []);

  return (
    <div className="dash">
      {error && <div className="settings__error">kbs unreachable: {error}</div>}
      {kbs.map((k) => (
        <KbPipelineCard key={k.name} kb={k.name} />
      ))}
      {kbs.length === 0 && !error && (
        <div className="settings__hint">no kbs configured</div>
      )}
    </div>
  );
}

function KbPipelineCard({ kb }: { kb: string }) {
  const [sources, setSources] = useState<SourceSummary[]>([]);
  const [runs, setRuns] = useState<RunEntry[]>([]);
  const [queries, setQueries] = useState<QueryEntry[]>([]);
  // Tracks the per-row optimistic state for in-flight pause/resume
  // so the button shows "…" while the daemon thinks. Keyed by slug.
  const [pendingSrc, setPendingSrc] = useState<Set<string>>(new Set());
  const [reindexing, setReindexing] = useState(false);
  const [atlasBusy, setAtlasBusy] = useState<"recompute" | "recluster" | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const ctrl = new AbortController();
    const load = () => {
      Promise.all([
        fetchSources(kb, ctrl.signal),
        fetchRuns(kb, 50, ctrl.signal),
        fetchQueries(kb, 50, ctrl.signal),
      ])
        .then(([s, r, q]) => {
          if (cancelled) return;
          setSources(s);
          setRuns(r);
          setQueries(q);
        })
        .catch((e) => {
          if (!cancelled && e?.name !== "AbortError") {
            // The kb may have been dropped or unreachable; leave the
            // tables empty rather than surfacing a per-card error
            // (the page-level fetchKbs guard already shows fleet
            // outages). Console-warn so the regression is debuggable.
            console.warn(`[pipeline] ${kb} load failed`, e);
          }
        });
    };
    load();
    // Refresh on indexing milestones for THIS kb (cheap — index.start
    // / index.complete fire once per run, not per file). Same for
    // source.paused/resumed — the table needs to flip the paused glyph.
    const unsubs = [
      sse.subscribeEvent("index.complete", (payload) => {
        if (typeof payload.kb === "string" && payload.kb === kb) {
          load();
          setReindexing(false);
        }
      }),
      sse.subscribeEvent("source.paused", (payload) => {
        if (typeof payload.kb === "string" && payload.kb === kb) load();
      }),
      sse.subscribeEvent("source.resumed", (payload) => {
        if (typeof payload.kb === "string" && payload.kb === kb) load();
      }),
    ];
    return () => {
      cancelled = true;
      ctrl.abort();
      for (const u of unsubs) u();
    };
  }, [kb]);

  async function toggleSource(s: SourceSummary) {
    setPendingSrc((prev) => new Set(prev).add(s.slug));
    setActionError(null);
    try {
      if (s.paused) await resumeSource(kb, s.slug);
      else await pauseSource(kb, s.slug);
      // Optimistic local flip — SSE confirmation will overwrite when it lands.
      setSources((prev) => prev.map((r) => (r.slug === s.slug ? { ...r, paused: !s.paused } : r)));
    } catch (e) {
      setActionError(String(e instanceof Error ? e.message : e));
    } finally {
      setPendingSrc((prev) => {
        const next = new Set(prev);
        next.delete(s.slug);
        return next;
      });
    }
  }

  async function triggerReindex() {
    setReindexing(true);
    setActionError(null);
    try {
      await reindexKb(kb);
      // The kb-wide reindex emits index.start / index.complete which
      // the SSE listener above picks up — reindexing flag flips off
      // when index.complete arrives. We keep it true here so the
      // button shows "running…" until then.
    } catch (e) {
      setActionError(String(e instanceof Error ? e.message : e));
      setReindexing(false);
    }
  }

  async function triggerAtlas(kind: "recompute" | "recluster") {
    setAtlasBusy(kind);
    setActionError(null);
    try {
      if (kind === "recompute") await recomputeAtlas(kb);
      else await reclusterAtlas(kb);
    } catch (e) {
      setActionError(String(e instanceof Error ? e.message : e));
    } finally {
      // Atlas runs are typically <2s; clear the spinner immediately.
      // The complete event would flip our UI again — see Atlas tab
      // future work for a finer state machine.
      setTimeout(() => setAtlasBusy(null), 1500);
    }
  }

  return (
    <section className="dash__kbcard" aria-label={`pipeline for ${kb}`}>
      <header className="dash__kbcard-head">
        <h3 className="settings__h3">{kb}</h3>
        <div className="dash__actions">
          <button
            type="button"
            className="settings__btn"
            onClick={triggerReindex}
            disabled={reindexing}
            title="POST /api/kb/{kb}/reindex"
          >
            {reindexing ? (
              <><Icon.Refresh aria-hidden="true" /> running…</>
            ) : (
              <><Icon.Refresh aria-hidden="true" /> reindex</>
            )}
          </button>
          <button
            type="button"
            className="settings__btn"
            onClick={() => triggerAtlas("recluster")}
            disabled={atlasBusy != null}
            title="POST /api/kb/{kb}/atlas/recluster"
          >
            {atlasBusy === "recluster" ? (
              "⋯ reclustering"
            ) : (
              <><Icon.Plus aria-hidden="true" /> recluster atlas</>
            )}
          </button>
          <button
            type="button"
            className="settings__btn"
            onClick={() => triggerAtlas("recompute")}
            disabled={atlasBusy != null}
            title="POST /api/kb/{kb}/atlas/recompute"
          >
            {atlasBusy === "recompute" ? (
              "⋯ recomputing"
            ) : (
              <><Icon.Refresh aria-hidden="true" /> recompute atlas</>
            )}
          </button>
        </div>
      </header>
      {actionError && (
        <div className="settings__error" role="alert">
          {actionError}
        </div>
      )}

      <SectionTable
        title="sources"
        rows={sources}
        head={["slug", "path", "state", "docs", "actions"]}
        empty="no sources"
        row={(s) => {
          const busy = pendingSrc.has(s.slug);
          return [
            <code key="slug">{s.slug}</code>,
            <span key="path" className="dash__path" title={s.path}>
              {s.path}
            </span>,
            <span key="paused" className={s.paused ? "is-warn" : "is-ok"}>
              {s.paused ? "paused" : "active"}
            </span>,
            <span key="docs">{s.doc_count}</span>,
            <div key="act" className="dash__row-actions">
              <button
                type="button"
                className="settings__btn settings__btn--sm"
                disabled={busy}
                onClick={() => toggleSource(s)}
              >
                {busy ? (
                  "…"
                ) : s.paused ? (
                  <><Icon.Play aria-hidden="true" /> resume</>
                ) : (
                  <><Icon.Pause aria-hidden="true" /> pause</>
                )}
              </button>
              <button
                type="button"
                className="settings__btn settings__btn--sm"
                disabled={busy}
                onClick={async () => {
                  setActionError(null);
                  try {
                    await reindexSource(kb, s.slug);
                  } catch (e) {
                    setActionError(String(e instanceof Error ? e.message : e));
                  }
                }}
                title="POST /api/kb/{kb}/sources/{src}/reindex"
              >
                <Icon.Refresh aria-hidden="true" /> reindex
              </button>
            </div>,
          ];
        }}
      />

      <SectionTable
        title={`recent runs (${runs.length})`}
        rows={runs}
        head={["run", "src", "started", "duration", "ok / err", "status"]}
        empty="no runs yet"
        row={(r) => [
          <code key="run" className="dash__id">
            {r.run.slice(0, 7)}
          </code>,
          <span key="src">{r.src ?? "—"}</span>,
          <span key="started">{relTime(r.started_at)}</span>,
          <span key="dur">{r.duration_ms != null ? `${r.duration_ms}ms` : "—"}</span>,
          <span key="oe">
            <span className="is-ok">{r.ok_count}</span>
            {" / "}
            <span className={r.err_count > 0 ? "is-err" : ""}>{r.err_count}</span>
          </span>,
          <span key="st" className={r.status === "running" ? "is-info" : "is-ok"}>
            {r.status}
          </span>,
        ]}
      />

      <SectionTable
        title={`recent queries (${queries.length})`}
        rows={queries}
        head={["at", "mode", "q", "hits", "ms"]}
        empty="no queries"
        row={(q, i) => [
          <span key="at">{relTime(q.at)}</span>,
          <span key="mode">
            <code>{q.mode}</code>
          </span>,
          <span key={`q-${i}`} className="dash__q" title={q.q}>
            {q.q}
          </span>,
          <span key="hits">{q.hits}</span>,
          <span key="ms">{q.ms}</span>,
        ]}
      />
    </section>
  );
}

function SectionTable<T>({
  title,
  rows,
  head,
  empty,
  row,
}: {
  title: string;
  rows: T[];
  head: string[];
  empty: string;
  row: (r: T, i: number) => React.ReactNode[];
}) {
  return (
    <details className="dash__sub" open>
      <summary>
        <span>{title}</span>
      </summary>
      {rows.length === 0 ? (
        <div className="settings__hint">{empty}</div>
      ) : (
        <table className="dash__table">
          <thead>
            <tr>
              {head.map((h) => (
                <th key={h}>{h}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={i}>
                {row(r, i).map((cell, j) => (
                  <td key={j}>{cell}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </details>
  );
}

function relTime(iso: string): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "?";
  const s = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
