import { useEffect, useState } from "react";
import {
  applyFixError,
  dismissError,
  fetchErrors,
  fetchKbs,
  type ErrorEntry,
  type KbSummary,
} from "../../api/client";
import { sse } from "../../api/sse";
import { Icon } from "../icons";

// Errors tab. Per-kb open-error list with row-level Dismiss +
// Apply-fix buttons. Reacts to `error*` SSE events so a newly-raised
// error appears without a refresh, and a dismissed/fixed row vanishes
// optimistically (the SSE confirmation just affirms what we already
// removed). Apply-fix is a v0.1 daemon stub — the server returns 202
// and emits `error.fixed` immediately; the SPA surfaces the "this is
// a stub" note from the response so the user isn't surprised.
export default function Errors() {
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
        <KbErrorsCard key={k.name} kb={k.name} />
      ))}
      {kbs.length === 0 && !error && (
        <div className="settings__hint">no kbs configured</div>
      )}
    </div>
  );
}

function KbErrorsCard({ kb }: { kb: string }) {
  const [errors, setErrors] = useState<ErrorEntry[]>([]);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [pending, setPending] = useState<Set<string>>(new Set());
  const [actionError, setActionError] = useState<string | null>(null);
  const [noteByErr, setNoteByErr] = useState<Record<string, string>>({});

  useEffect(() => {
    let cancelled = false;
    const ctrl = new AbortController();
    const load = () => {
      fetchErrors(kb, ctrl.signal)
        .then((es) => {
          if (!cancelled) setErrors(es);
        })
        .catch((e) => {
          if (!cancelled && e?.name !== "AbortError") {
            console.warn(`[errors] ${kb} load failed`, e);
          }
        });
    };
    load();
    const unsubs = [
      sse.subscribeEvent("error", (payload) => {
        if (typeof payload.kb === "string" && payload.kb === kb) load();
      }),
      sse.subscribeEvent("error.dismissed", (payload) => {
        if (typeof payload.kb === "string" && payload.kb === kb) load();
      }),
      sse.subscribeEvent("error.fixed", (payload) => {
        if (typeof payload.kb === "string" && payload.kb === kb) load();
      }),
    ];
    return () => {
      cancelled = true;
      ctrl.abort();
      for (const u of unsubs) u();
    };
  }, [kb]);

  function toggle(id: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function dismiss(id: string) {
    setPending((prev) => new Set(prev).add(id));
    setActionError(null);
    try {
      await dismissError(kb, id);
      setErrors((prev) => prev.filter((e) => e.id !== id));
    } catch (e) {
      setActionError(String(e instanceof Error ? e.message : e));
    } finally {
      setPending((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
  }

  async function fix(id: string) {
    setPending((prev) => new Set(prev).add(id));
    setActionError(null);
    try {
      const r = await applyFixError(kb, id);
      if (r.note) {
        setNoteByErr((prev) => ({ ...prev, [id]: r.note! }));
      }
      // Daemon emits error.fixed → SSE listener refetches and the row
      // drops out of the list anyway. We don't optimistically remove
      // here because (in v0.1) the fix is a stub and the row may
      // re-appear on the next reconcile pass.
    } catch (e) {
      setActionError(String(e instanceof Error ? e.message : e));
    } finally {
      setPending((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
  }

  return (
    <section className="dash__kbcard" aria-label={`errors for ${kb}`}>
      <header className="dash__kbcard-head">
        <h3 className="settings__h3">
          {kb}
          <span className="dash__count">{errors.length}</span>
        </h3>
      </header>
      {actionError && (
        <div className="settings__error" role="alert">
          {actionError}
        </div>
      )}
      {errors.length === 0 ? (
        <div className="settings__hint">no open errors — nothing to do</div>
      ) : (
        <table className="dash__table">
          <thead>
            <tr>
              <th>kind</th>
              <th>path</th>
              <th>retry</th>
              <th>age</th>
              <th>actions</th>
            </tr>
          </thead>
          <tbody>
            {errors.map((e) => {
              const isExpanded = expanded.has(e.id);
              const isPending = pending.has(e.id);
              return (
                <>
                  <tr key={e.id} className="errors__row">
                    <td>
                      <code className="is-err">{e.kind}</code>
                    </td>
                    <td>
                      <button
                        type="button"
                        className="dash__inlinebtn"
                        onClick={() => toggle(e.id)}
                        aria-expanded={isExpanded}
                      >
                        <span className="dash__path" title={e.path}>{e.path}</span>
                      </button>
                    </td>
                    <td>{e.retry_count}</td>
                    <td>{relAge(e.created_at)}</td>
                    <td>
                      <div className="dash__row-actions">
                        <button
                          type="button"
                          className="settings__btn settings__btn--sm"
                          disabled={isPending}
                          onClick={() => dismiss(e.id)}
                        >
                          {isPending ? "…" : <><Icon.X aria-hidden="true" /> dismiss</>}
                        </button>
                        <button
                          type="button"
                          className="settings__btn settings__btn--sm"
                          disabled={isPending}
                          onClick={() => fix(e.id)}
                          title="POST /api/kb/{kb}/errors/{id}/apply-fix (v0.1 stub)"
                        >
                          ⚙ apply fix
                        </button>
                      </div>
                    </td>
                  </tr>
                  {isExpanded && (
                    <tr key={`${e.id}-detail`} className="errors__detail-row">
                      <td colSpan={5}>
                        <pre className="errors__detail">{e.message}</pre>
                        {noteByErr[e.id] && (
                          <div className="settings__hint">{noteByErr[e.id]}</div>
                        )}
                      </td>
                    </tr>
                  )}
                </>
              );
            })}
          </tbody>
        </table>
      )}
    </section>
  );
}

function relAge(unix: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unix));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}
