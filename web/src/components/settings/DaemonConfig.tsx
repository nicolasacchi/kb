// CE — the "Config" Settings tab: a structured editor over the daemon's
// whole kb.toml. Loads the running config, tracks an editable draft, and
// on Save persists + triggers the daemon's in-process restart, then
// reconnects (polling /api/identity until started_at changes). A changed
// bind addr is gated behind a type-the-addr confirm and, since it moves
// the daemon out from under this same-origin page, surfaces a link to the
// new origin instead of polling a dead socket.

import { useEffect, useMemo, useState } from "react";
import { currentDaemonBase } from "../../api/base";
import { fetchIdentity } from "../../api/client";
import {
  ConfigValidationError,
  fetchDaemonConfig,
  mergeConfig,
  saveDaemonConfig,
  type DaemonConfig,
  type FieldError,
} from "../../api/config";
import ConfirmModal from "./ConfirmModal";
import {
  DefaultsEditor,
  type Errors,
  IndexerEditor,
  PerKbEditor,
  ServerEditor,
  ShareEditor,
  UiEditor,
} from "./config/sections";

type Phase =
  | "loading"
  | "idle"
  | "confirming-addr"
  | "saving"
  | "reconnecting"
  | "reconnected"
  | "addr-moved"
  | "error";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/// Derive the origin to reach the daemon on after a bind-addr change.
/// Wildcard/loopback hosts aren't reachable as typed, so fall back to the
/// host this page is already served from.
function newOriginFor(addr: string): string | null {
  const m = addr.match(/^(.*):(\d+)$/);
  if (!m) return null;
  let host = m[1].replace(/^\[|\]$/g, "");
  const port = m[2];
  if (["0.0.0.0", "::", "127.0.0.1", "::1", "localhost"].includes(host)) {
    host = window.location.hostname;
  }
  const hostPart = host.includes(":") ? `[${host}]` : host;
  return `${window.location.protocol}//${hostPart}:${port}`;
}

export default function DaemonConfig() {
  const [phase, setPhase] = useState<Phase>("loading");
  const [rawConfig, setRawConfig] = useState<DaemonConfig | null>(null);
  const [draft, setDraft] = useState<DaemonConfig | null>(null);
  const [envPresent, setEnvPresent] = useState<Record<string, boolean>>({});
  const [models, setModels] = useState<string[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [fieldErrors, setFieldErrors] = useState<Errors>({});
  const [warnings, setWarnings] = useState<FieldError[]>([]);
  const [newOrigin, setNewOrigin] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const ctrl = new AbortController();
    fetchDaemonConfig(ctrl.signal)
      .then((r) => {
        if (cancelled) return;
        setRawConfig(r.config);
        setDraft(structuredClone(r.config));
        setEnvPresent(r.env_present ?? {});
        setModels(r.embedding_models ?? []);
        setPhase("idle");
      })
      .catch((e) => {
        if (!cancelled && e?.name !== "AbortError") {
          setLoadError(String(e?.message ?? e));
          setPhase("error");
        }
      });
    return () => {
      cancelled = true;
      ctrl.abort();
    };
  }, []);

  const dirty = useMemo(
    () => !!draft && !!rawConfig && JSON.stringify(draft) !== JSON.stringify(rawConfig),
    [draft, rawConfig],
  );
  const addrChanged = !!draft && !!rawConfig && draft.server.addr !== rawConfig.server.addr;

  // Warn before navigating away with unsaved edits.
  useEffect(() => {
    if (!dirty) return;
    const h = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", h);
    return () => window.removeEventListener("beforeunload", h);
  }, [dirty]);

  const busy = phase === "saving" || phase === "reconnecting";

  async function doSave() {
    if (!draft || !rawConfig) return;
    setPhase("saving");
    setSaveError(null);
    setFieldErrors({});

    // Snapshot started_at to detect the restart (the old process may
    // answer briefly before going down).
    let prevStartedAt: string | null = null;
    try {
      prevStartedAt = (await fetchIdentity()).started_at;
    } catch {
      /* best-effort */
    }

    const payload = mergeConfig(rawConfig, draft);
    try {
      const res = await saveDaemonConfig(payload);
      setWarnings(res.warnings ?? []);
    } catch (e) {
      if (e instanceof ConfigValidationError) {
        const map: Errors = {};
        for (const f of e.fields) map[f.pointer] = f.detail;
        setFieldErrors(map);
        setSaveError(e.message);
      } else {
        setSaveError(String((e as Error)?.message ?? e));
      }
      setPhase("error");
      return;
    }

    // Saved + restart scheduled. A changed addr moves the daemon to a new
    // socket; this same-origin page can't follow it, so surface the link.
    if (addrChanged && currentDaemonBase() === "") {
      setNewOrigin(newOriginFor(draft.server.addr));
      setPhase("addr-moved");
      return;
    }

    // Same addr: poll until the daemon comes back on a new started_at.
    setPhase("reconnecting");
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      await sleep(500);
      try {
        const id = await fetchIdentity();
        if (!prevStartedAt || id.started_at !== prevStartedAt) {
          const fresh = await fetchDaemonConfig();
          setRawConfig(fresh.config);
          setDraft(structuredClone(fresh.config));
          setEnvPresent(fresh.env_present ?? {});
          setModels(fresh.embedding_models ?? []);
          setPhase("reconnected");
          setTimeout(() => setPhase("idle"), 2500);
          return;
        }
      } catch {
        /* daemon down mid-restart — keep polling */
      }
    }
    setSaveError("Daemon did not come back within 30s. Check the daemon process / logs, then reload.");
    setPhase("error");
  }

  function onSaveClick() {
    if (!dirty) return;
    if (addrChanged) {
      setPhase("confirming-addr");
    } else {
      void doSave();
    }
  }

  function onRevert() {
    if (rawConfig) setDraft(structuredClone(rawConfig));
    setFieldErrors({});
    setSaveError(null);
    setWarnings([]);
  }

  if (phase === "loading") {
    return <div className="settings__panel">Loading config…</div>;
  }
  if (loadError && !draft) {
    return (
      <div className="settings__panel">
        <div className="settings__error" role="alert">
          config unreachable: {loadError}
        </div>
      </div>
    );
  }
  if (!draft || !rawConfig) return null;

  const kbNames = Object.keys(draft.kb).sort();

  return (
    <div className="settings__panel cfg">
      <div className="cfg__restart-note">
        Saving rewrites <code>kb.toml</code> (comments preserved) and{" "}
        <strong>restarts the daemon</strong> to apply changes — about a second of downtime.
      </div>

      {saveError && (
        <div className="settings__error cfg__save-error" role="alert">
          {saveError}
        </div>
      )}
      {warnings.length > 0 && (
        <div className="cfg__warnings" role="status">
          <strong>saved with warnings:</strong>
          <ul>
            {warnings.map((w) => (
              <li key={w.pointer}>
                <code>{w.pointer}</code> — {w.detail}
              </li>
            ))}
          </ul>
        </div>
      )}
      {phase === "reconnected" && (
        <div className="cfg__ok" role="status">
          daemon restarted — config applied.
        </div>
      )}
      {phase === "addr-moved" && (
        <div className="settings__danger-banner cfg__moved" role="alert">
          The daemon is restarting on <code>{draft.server.addr}</code> and this page (served by the
          old address) will lose its connection.{" "}
          {newOrigin ? (
            <a href={`${newOrigin}${window.location.pathname}${window.location.search}`}>
              Open kb at {newOrigin} →
            </a>
          ) : (
            <>Open the new address manually.</>
          )}
        </div>
      )}

      <fieldset className="cfg__form" disabled={busy} aria-busy={busy}>
        <ServerEditor
          value={draft.server}
          errors={fieldErrors}
          onChange={(server) => setDraft({ ...draft, server })}
        />
        <IndexerEditor value={draft.indexer} onChange={(indexer) => setDraft({ ...draft, indexer })} />
        <DefaultsEditor
          value={draft.defaults}
          models={models}
          onChange={(defaults) => setDraft({ ...draft, defaults })}
        />
        <ShareEditor
          value={draft.share}
          envPresent={envPresent}
          onChange={(share) => setDraft({ ...draft, share })}
        />
        <UiEditor value={draft.ui} onChange={(ui) => setDraft({ ...draft, ui })} />

        <section className="settings__section cfg__section" aria-label="knowledge bases">
          <h2 className="settings__h2">knowledge bases</h2>
          {kbNames.length === 0 && (
            <p className="settings__hint">
              no kbs configured — add one below (or with{" "}
              <code>kb add &lt;path&gt;</code>).
            </p>
          )}
          {kbNames.map((name) => (
            <PerKbEditor
              key={name}
              name={name}
              value={draft.kb[name]}
              errors={fieldErrors}
              models={models}
              onChange={(kbSection) =>
                setDraft({ ...draft, kb: { ...draft.kb, [name]: kbSection } })
              }
            />
          ))}
          <NewKbCard
            existingNames={kbNames}
            onAdd={(name, path) =>
              setDraft({
                ...draft,
                kb: {
                  ...draft.kb,
                  [name]: { path, skip_patterns: [], ui: {}, templates: {} },
                },
              })
            }
          />
        </section>
      </fieldset>

      <div className="cfg__bar">
        {busy && <span className="cfg__bar-status">saving &amp; restarting…</span>}
        <button
          type="button"
          className="settings__btn settings__btn--sm"
          onClick={onRevert}
          disabled={!dirty || busy}
        >
          Revert
        </button>
        <button
          type="button"
          className="cfg__save"
          onClick={onSaveClick}
          disabled={!dirty || busy}
        >
          Save changes
        </button>
      </div>

      {phase === "confirming-addr" && (
        <ConfirmModal
          title="Change the daemon bind address?"
          danger
          expectedToken={draft.server.addr}
          confirmLabel="Save & restart on new address"
          body={
            <>
              The daemon will restart and listen on <code>{draft.server.addr}</code> instead of{" "}
              <code>{rawConfig.server.addr}</code>.{" "}
              {currentDaemonBase() === "" && (
                <>Because this UI is served by that daemon, this page will lose its connection.</>
              )}{" "}
              Type the new address to confirm.
            </>
          }
          onConfirm={() => void doSave()}
          onClose={() => setPhase("idle")}
        />
      )}
    </div>
  );
}

// A2 — add a brand-new corpus from the Config tab. The server already accepts a
// brand-new kb via PUT /api/config (no new endpoint), and the watcher now boots
// an as-yet-missing source dir empty instead of failing. The card just splices a
// minimal valid KbSection into the draft; the operator then reviews + Saves
// (which restarts the daemon). Two guards the server can't give a clean error
// for: the KbName regex (a bad key fails the bare Json extractor → opaque 422)
// and a name collision (would silently overwrite an existing corpus's config).
const KB_NAME_RE = /^[a-z0-9_-]{1,64}$/;

function NewKbCard({
  existingNames,
  onAdd,
}: {
  existingNames: string[];
  onAdd: (name: string, path: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [path, setPath] = useState("");

  const trimmedName = name.trim();
  const trimmedPath = path.trim();
  const nameValid = KB_NAME_RE.test(trimmedName);
  const collides = existingNames.includes(trimmedName);
  const error = !trimmedName
    ? null
    : !nameValid
      ? "Name must be lowercase letters, digits, - or _ (1–64 chars)."
      : collides
        ? `A kb named "${trimmedName}" already exists — pick another name.`
        : null;
  const canAdd = nameValid && !collides && trimmedPath.length > 0;

  function reset() {
    setOpen(false);
    setName("");
    setPath("");
  }

  if (!open) {
    return (
      <button
        type="button"
        className="settings__btn settings__btn--sm cfg__add-kb"
        onClick={() => setOpen(true)}
      >
        + Add knowledge base
      </button>
    );
  }

  return (
    <div className="cfg__newkb" role="group" aria-label="add knowledge base">
      <div className="cfg__newkb-row">
        <label className="cfg__newkb-label" htmlFor="newkb-name">
          name
        </label>
        <input
          id="newkb-name"
          className="cfg__input"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="my-corpus"
          autoFocus
          aria-invalid={!!error}
          spellCheck={false}
        />
      </div>
      <div className="cfg__newkb-row">
        <label className="cfg__newkb-label" htmlFor="newkb-path">
          path
        </label>
        <input
          id="newkb-path"
          className="cfg__input"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          placeholder="/srv/my-corpus"
          spellCheck={false}
        />
      </div>
      {error && (
        <div className="settings__error" role="alert">
          {error}
        </div>
      )}
      <p className="settings__hint">
        The path is a folder on the daemon host. If it doesn’t exist yet, the kb
        starts empty and indexes once you create + populate it. Saving restarts
        the daemon.
      </p>
      <div className="cfg__newkb-bar">
        <button
          type="button"
          className="settings__btn settings__btn--sm"
          onClick={reset}
        >
          Cancel
        </button>
        <button
          type="button"
          className="cfg__save"
          disabled={!canAdd}
          onClick={() => {
            onAdd(trimmedName, trimmedPath);
            reset();
          }}
        >
          Add — review &amp; Save below
        </button>
      </div>
    </div>
  );
}
