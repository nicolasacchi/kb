import { useEffect, useState } from "react";
import {
  fetchKbs,
  fetchQuarantine,
  restoreAllQuarantine,
  restoreQuarantine,
  type KbSummary,
  type QuarantineEntry,
} from "../../api/client";
import { sse } from "../../api/sse";
import { Icon } from "../icons";

// Quarantine tab. Per-kb list of artifacts the indexer gave up on
// after QUARANTINE_THRESHOLD (3) consecutive failures, with row-level
// + bulk Restore actions. Restore clears the open error rows, deletes
// the sidecar pair on disk, and emits `watch.modify` (force=true) so
// the indexer retries immediately. Refetches on the `error` and
// `quarantine.restored` SSE events so the list stays live.
export default function Quarantine() {
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
      {error && (
        <div className="settings__error">kbs unreachable: {error}</div>
      )}
      {kbs.map((k) => (
        <KbQuarantineCard key={k.name} kb={k.name} />
      ))}
      {kbs.length === 0 && !error && (
        <div className="settings__hint">no kbs configured</div>
      )}
    </div>
  );
}

function KbQuarantineCard({ kb }: { kb: string }) {
  const [entries, setEntries] = useState<QuarantineEntry[]>([]);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [pending, setPending] = useState<Set<string>>(new Set());
  const [bulkPending, setBulkPending] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const ctrl = new AbortController();
    const load = () => {
      fetchQuarantine(kb, ctrl.signal)
        .then((es) => {
          if (!cancelled) setEntries(es);
        })
        .catch((e) => {
          if (!cancelled && e?.name !== "AbortError") {
            console.warn(`[quarantine] ${kb} load failed`, e);
          }
        });
    };
    load();
    const sameKb = (payload: Record<string, unknown>) =>
      typeof payload.kb === "string" && payload.kb === kb;
    const unsubs = [
      sse.subscribeEvent("error", (p) => sameKb(p) && load()),
      sse.subscribeEvent("error.dismissed", (p) => sameKb(p) && load()),
      sse.subscribeEvent("quarantine.restored", (p) => sameKb(p) && load()),
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

  async function restoreOne(path: string) {
    setPending((prev) => new Set(prev).add(path));
    setActionError(null);
    try {
      await restoreQuarantine(kb, path);
      setEntries((prev) => prev.filter((e) => e.path !== path));
    } catch (e) {
      setActionError(String(e instanceof Error ? e.message : e));
    } finally {
      setPending((prev) => {
        const next = new Set(prev);
        next.delete(path);
        return next;
      });
    }
  }

  async function restoreAll() {
    if (entries.length === 0) return;
    setBulkPending(true);
    setActionError(null);
    try {
      await restoreAllQuarantine(kb);
      setEntries([]);
    } catch (e) {
      setActionError(String(e instanceof Error ? e.message : e));
    } finally {
      setBulkPending(false);
    }
  }

  return (
    <section
      className="dash__kbcard"
      aria-label={`quarantine for ${kb}`}
    >
      <header className="dash__kbcard-head">
        <h3 className="settings__h3">
          {kb}
          <span className="dash__count">{entries.length}</span>
        </h3>
        {entries.length > 0 && (
          <button
            type="button"
            className="settings__btn settings__btn--sm"
            disabled={bulkPending}
            onClick={restoreAll}
            title="POST /api/kb/{kb}/quarantine/restore-all"
          >
            {bulkPending ? "restoring…" : <><Icon.Refresh aria-hidden="true" /> restore all</>}
          </button>
        )}
      </header>
      {actionError && (
        <div className="settings__error" role="alert">
          {actionError}
        </div>
      )}
      {entries.length === 0 ? (
        <div className="settings__hint">
          no quarantined artifacts — nothing to do
        </div>
      ) : (
        <table className="dash__table">
          <thead>
            <tr>
              <th>kind</th>
              <th>path</th>
              <th>retry</th>
              <th>age</th>
              <th>file</th>
              <th>actions</th>
            </tr>
          </thead>
          <tbody>
            {entries.map((e) => {
              const isExpanded = expanded.has(e.error_id);
              const isPending = pending.has(e.path);
              return (
                <>
                  <tr key={e.error_id} className="errors__row">
                    <td>
                      <code className="is-err">{e.kind}</code>
                    </td>
                    <td>
                      <button
                        type="button"
                        className="dash__inlinebtn"
                        onClick={() => toggle(e.error_id)}
                        aria-expanded={isExpanded}
                      >
                        <span className="dash__path" title={e.path}>
                          {e.path}
                        </span>
                      </button>
                    </td>
                    <td>{e.retry_count}</td>
                    <td>{relAge(e.created_at)}</td>
                    <td>
                      {e.sidecar_present ? (
                        <span title="sidecar copy exists in quarantine dir">
                          <Icon.Folder />
                        </span>
                      ) : (
                        <span
                          className="settings__hint"
                          title="no sidecar on disk; restoring just clears the DB rows"
                        >
                          —
                        </span>
                      )}
                    </td>
                    <td>
                      <button
                        type="button"
                        className="settings__btn settings__btn--sm"
                        disabled={isPending || bulkPending}
                        onClick={() => restoreOne(e.path)}
                        title="POST /api/kb/{kb}/quarantine/restore — clears errors, removes sidecar, re-queues a forced index attempt"
                      >
                        {isPending ? "…" : <><Icon.Refresh aria-hidden="true" /> restore</>}
                      </button>
                    </td>
                  </tr>
                  {isExpanded && (
                    <tr
                      key={`${e.error_id}-detail`}
                      className="errors__detail-row"
                    >
                      <td colSpan={6}>
                        <pre className="errors__detail">{e.message}</pre>
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
