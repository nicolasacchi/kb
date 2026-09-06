import { useEffect, useState } from "react";
import { currentDaemonBase } from "../../api/base";
import { fetchKbs, type KbSummary } from "../../api/client";
import ConfirmModal from "./ConfirmModal";
import { Icon } from "../icons";

// Admin tab. Three destructive actions, each gated by a typed-token
// ConfirmModal: drop a kb's storage, purge a kb's history, drain
// the daemon (graceful shutdown). The CLI/TUI/operator can do all
// three already; this surfaces them in the SPA so the operator
// doesn't have to context-switch out of the dashboard for routine
// cleanup.
//
// All three call new daemon routes (S5: drop.rs, history.rs::purge,
// shutdown.rs). Errors surface inline; success refetches the kb
// list so the new doc count is visible immediately on Overview.

type Pending =
  | { kind: "drop"; kb: string }
  | { kind: "purge"; kb: string }
  | { kind: "drain" }
  | null;

export default function Admin() {
  const [kbs, setKbs] = useState<KbSummary[]>([]);
  const [pending, setPending] = useState<Pending>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draining, setDraining] = useState(false);

  function reload() {
    fetchKbs()
      .then(setKbs)
      .catch((e) => setError(String(e instanceof Error ? e.message : e)));
  }

  useEffect(() => {
    reload();
  }, []);

  async function confirm() {
    if (!pending) return;
    setBusy(true);
    setError(null);
    try {
      const base = currentDaemonBase();
      if (pending.kind === "drop") {
        const r = await fetch(
          `${base}/api/kb/${encodeURIComponent(pending.kb)}`,
          { method: "DELETE" },
        );
        if (!r.ok) throw new Error(`drop failed: ${r.status} ${r.statusText}`);
      } else if (pending.kind === "purge") {
        const r = await fetch(
          `${base}/api/kb/${encodeURIComponent(pending.kb)}/history/purge`,
          { method: "POST" },
        );
        if (!r.ok) throw new Error(`purge failed: ${r.status} ${r.statusText}`);
      } else {
        const r = await fetch(`${base}/api/shutdown`, { method: "POST" });
        if (!r.ok) throw new Error(`drain failed: ${r.status} ${r.statusText}`);
        setDraining(true);
      }
      setPending(null);
      reload();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="dash">
      <div className="settings__danger-banner" role="region" aria-label="danger zone">
        <strong>Danger zone.</strong> Actions on this tab destroy data or
        stop the daemon. Each step requires typing the kb name (or{" "}
        <code>DRAIN</code>) to confirm — the modal won't enable the
        confirm button otherwise.
      </div>

      {error && (
        <div className="settings__error" role="alert">
          {error}
        </div>
      )}

      {draining && (
        <div className="settings__danger" role="alert">
          <strong>Drain signalled.</strong> The daemon is finishing
          in-flight requests and will exit. The SPA will lose its SSE
          connection within a few seconds; relaunch <code>kb daemon</code>{" "}
          to bring it back.
        </div>
      )}

      <section aria-label="per-kb destructive actions">
        <h3 className="settings__h3">per-kb</h3>
        {kbs.length === 0 ? (
          <div className="settings__hint">no kbs configured</div>
        ) : (
          <table className="dash__table">
            <thead>
              <tr>
                <th>kb</th>
                <th>drop storage</th>
                <th>purge history</th>
              </tr>
            </thead>
            <tbody>
              {kbs.map((k) => (
                <tr key={k.name}>
                  <td>
                    <code>{k.name}</code>
                  </td>
                  <td>
                    <button
                      type="button"
                      className="settings__btn settings__btn--sm settings__btn--danger"
                      onClick={() => setPending({ kind: "drop", kb: k.name })}
                    >
                      <Icon.Trash aria-hidden="true" /> drop kb
                    </button>
                  </td>
                  <td>
                    <button
                      type="button"
                      className="settings__btn settings__btn--sm settings__btn--danger"
                      onClick={() => setPending({ kind: "purge", kb: k.name })}
                    >
                      <Icon.Trash aria-hidden="true" /> purge history
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <section aria-label="daemon-wide actions">
        <h3 className="settings__h3">daemon</h3>
        <div className="settings__danger-row">
          <div>
            <strong>Drain daemon.</strong>
            <div className="settings__hint">
              Signals graceful shutdown via{" "}
              <code>POST /api/shutdown</code>. In-flight requests
              finish; SSE streams close; the process exits. Restart
              with <code>kb daemon</code> (or your supervisor).
            </div>
          </div>
          <button
            type="button"
            className="settings__btn settings__btn--sm settings__btn--danger"
            onClick={() => setPending({ kind: "drain" })}
            disabled={draining}
          >
            <Icon.Warn aria-hidden="true" /> drain
          </button>
        </div>
      </section>

      {pending?.kind === "drop" && (
        <ConfirmModal
          title={`Drop kb "${pending.kb}"?`}
          body={
            <>
              Wipes <code>lance</code> + sqlite{" "}
              <code>history/errors/edges</code> for this kb. The
              source folder + <code>.review/*.json</code> comments are
              untouched. Shares stay (they track external
              deployments). Doc count → 0; a reindex re-walks the
              source.
            </>
          }
          expectedToken={pending.kb}
          confirmLabel="Drop kb"
          busy={busy}
          onConfirm={confirm}
          onClose={() => setPending(null)}
        />
      )}

      {pending?.kind === "purge" && (
        <ConfirmModal
          title={`Purge history for "${pending.kb}"?`}
          body={
            <>
              Clears the sqlite <code>history</code> table for this
              kb (visit log + scroll positions + searches +
              comment-author events). The artifacts and their content
              are untouched.
            </>
          }
          expectedToken={pending.kb}
          confirmLabel="Purge history"
          busy={busy}
          onConfirm={confirm}
          onClose={() => setPending(null)}
        />
      )}

      {pending?.kind === "drain" && (
        <ConfirmModal
          title="Drain the daemon?"
          body={
            <>
              Signals graceful shutdown. Active requests finish; SSE
              streams close; the process exits. To bring it back, run{" "}
              <code>kb daemon</code> in a shell (or your supervisor
              restarts it). The SPA will reconnect automatically once
              the daemon is back.
            </>
          }
          expectedToken="DRAIN"
          confirmLabel="Drain"
          busy={busy}
          onConfirm={confirm}
          onClose={() => setPending(null)}
        />
      )}
    </div>
  );
}
