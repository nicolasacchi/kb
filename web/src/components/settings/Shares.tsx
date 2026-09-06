import { useEffect, useState } from "react";
import {
  fetchKbs,
  listShares,
  revokeShare,
  type KbSummary,
  type ShareListItem,
} from "../../api/client";
import ShareModal from "../ShareModal";
import ConfirmModal from "./ConfirmModal";
import { Icon } from "../icons";

// Shares tab. Per-kb published-shares table, with row Revoke and a
// kb-level Create button. Create opens the existing `ShareModal` so
// the full publish flow (Cloudflare-Pages + Access gate vs GitHub
// Pages public, gate selection) stays single-source-of-truth.
//
// Create requires a target path (the file/folder to publish). The
// kb root is the default; the input below lets the user override it
// before opening the modal — matches the SPA's existing "share this
// artifact" flow but starts from the kb instead of the active doc.
export default function Shares() {
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
        <KbSharesCard key={k.name} kb={k.name} />
      ))}
      {kbs.length === 0 && !error && (
        <div className="settings__hint">no kbs configured</div>
      )}
    </div>
  );
}

function KbSharesCard({ kb }: { kb: string }) {
  const [shares, setShares] = useState<ShareListItem[]>([]);
  const [target, setTarget] = useState("");
  const [openCreate, setOpenCreate] = useState(false);
  const [revokeTarget, setRevokeTarget] = useState<ShareListItem | null>(null);
  const [revoking, setRevoking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function reload(signal?: AbortSignal) {
    listShares(kb, signal)
      .then(setShares)
      .catch((e) => {
        if (e?.name !== "AbortError") {
          console.warn(`[shares] ${kb} load failed`, e);
        }
      });
  }

  useEffect(() => {
    const ctrl = new AbortController();
    reload(ctrl.signal);
    return () => ctrl.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kb]);

  async function confirmRevoke() {
    if (!revokeTarget) return;
    setRevoking(true);
    setError(null);
    try {
      await revokeShare(kb, revokeTarget.name);
      setShares((prev) => prev.filter((s) => s.name !== revokeTarget.name));
      setRevokeTarget(null);
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setRevoking(false);
    }
  }

  return (
    <section className="dash__kbcard" aria-label={`shares for ${kb}`}>
      <header className="dash__kbcard-head">
        <h3 className="settings__h3">
          {kb}
          <span className="dash__count">{shares.length}</span>
        </h3>
        <div className="dash__actions">
          <input
            type="text"
            className="dmgr__input"
            value={target}
            onChange={(e) => setTarget(e.target.value)}
            placeholder="path/to/file-or-folder (empty = kb root)"
            aria-label="share target"
            style={{ minWidth: 280 }}
          />
          <button
            type="button"
            className="settings__btn"
            onClick={() => setOpenCreate(true)}
          >
            + create
          </button>
        </div>
      </header>
      {error && (
        <div className="settings__error" role="alert">
          {error}
        </div>
      )}
      {shares.length === 0 ? (
        <div className="settings__hint">no shares yet</div>
      ) : (
        <table className="dash__table">
          <thead>
            <tr>
              <th>name</th>
              <th>host</th>
              <th>gate</th>
              <th>created</th>
              <th>url</th>
              <th>actions</th>
            </tr>
          </thead>
          <tbody>
            {shares.map((s) => (
              <tr key={s.name}>
                <td>
                  <code>{s.name}</code>
                </td>
                <td>{s.host}</td>
                <td>{s.gate ?? "public"}</td>
                <td>{relAge(s.created_at)}</td>
                <td>
                  <a href={s.url} target="_blank" rel="noopener noreferrer" className="dash__path">
                    {s.url}
                  </a>
                </td>
                <td>
                  <button
                    type="button"
                    className="settings__btn settings__btn--sm settings__btn--danger"
                    onClick={() => setRevokeTarget(s)}
                  >
                    <Icon.Trash aria-hidden="true" /> revoke
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {openCreate && (
        <ShareModal
          kb={kb}
          target={target.trim()}
          onClose={() => {
            setOpenCreate(false);
            reload();
          }}
        />
      )}

      {revokeTarget && (
        <ConfirmModal
          title={`Revoke share "${revokeTarget.name}"?`}
          body={
            <>
              This tears down the live deployment at
              <br />
              <code>{revokeTarget.url}</code>
              <br />
              and drops the share record. Source artifacts are untouched.
            </>
          }
          expectedToken={revokeTarget.name}
          confirmLabel="Revoke"
          busy={revoking}
          onConfirm={confirmRevoke}
          onClose={() => setRevokeTarget(null)}
        />
      )}
    </section>
  );
}

function relAge(unix: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000 - unix));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
