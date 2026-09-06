import { useEffect, useState } from "react";
import { sse } from "../api/sse";

// Manages the per-user daemon list in localStorage. Adding a URL pings
// /api/identity at the candidate origin first so we surface bad URLs
// immediately. Removing is unconditional (no confirm); the user can
// re-add later. Changes call sse.saveDaemonUrls which both persists
// AND reconciles open EventSource connections.
export default function DaemonsManager() {
  const [urls, setUrls] = useState<string[]>([]);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setUrls(sse.loadDaemonUrls());
  }, []);

  async function add() {
    const cleaned = draft.replace(/\/+$/, "").trim();
    if (!cleaned) return;
    if (urls.includes(cleaned)) {
      setError("already in list");
      return;
    }
    setError(null);
    setBusy(true);
    try {
      const r = await fetch(`${cleaned}/api/identity`);
      if (!r.ok) throw new Error(`identity check failed: ${r.status}`);
      const next = [...urls, cleaned];
      sse.saveDaemonUrls(next);
      setUrls(next);
      setDraft("");
    } catch (e) {
      setError(`unreachable: ${String(e).replace(/^Error:\s*/, "")}`);
    } finally {
      setBusy(false);
    }
  }

  function remove(url: string) {
    const next = urls.filter((u) => u !== url);
    sse.saveDaemonUrls(next);
    setUrls(next);
  }

  return (
    <section className="settings__section" aria-label="daemons">
      <h2 className="settings__h2">daemons</h2>
      <p className="settings__hint">
        Configure additional kb daemons. The SPA aggregates status across
        all configured daemons. Removing a daemon does not stop it — it
        just hides it from this client.
      </p>
      <ul className="dmgr__list">
        {urls.map((u) => (
          <li key={u} className="dmgr__row">
            <span className="dmgr__url" title={u}>
              {u}
            </span>
            <button
              className="dmgr__remove"
              onClick={() => remove(u)}
              aria-label={`remove ${u}`}
            >
              remove
            </button>
          </li>
        ))}
      </ul>
      <div className="dmgr__add">
        <input
          type="url"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder="http://192.168.1.5:4737"
          aria-label="daemon URL"
          className="dmgr__input"
          disabled={busy}
        />
        <button
          onClick={add}
          disabled={busy || !draft.trim()}
          className="dmgr__add-btn"
        >
          {busy ? "checking…" : "add"}
        </button>
      </div>
      {error && (
        <div className="dmgr__error" role="alert">
          {error}
        </div>
      )}
    </section>
  );
}
