// CMDSETTINGS: Settings -> "Terminal commands". Edits the per-harness command
// catalogue overlay stored by the daemon (`/api/terms/commands`).
//
// Server state is the `["term","commands"]` TanStack query (SSE-invalidated
// by `term.commands`); the open draft lives in component state, so a refetch
// can never clobber it. Every change is built from the STORED overlay
// (`data.overlay`/`data.hidden`), never from the effective list, validated by
// the shared validator, then sent as ONE PUT carrying `If-Match`.
import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useConfirm } from "../../../components/ConfirmProvider";
import type { EffItem } from "../../../api/generated/EffItem";
import type { OvItem } from "../../../api/generated/OvItem";
import type { OverlayPut } from "../../../api/generated/OverlayPut";
import { CommandsApiError, putOverlay, resetOverlay } from "./api";
import { Defects, ItemForm, NEW_GROUP, type Draft } from "./ItemForm";
import {
  BLANK_FIELDS,
  EDITOR_HARNESSES,
  EMPTY_OVERLAY,
  counts,
  defaultItem,
  fieldsFromItem,
  hideDefault,
  itemFromFields,
  moveItem,
  newGroupId,
  newItemId,
  originOf,
  prune,
  removeItem,
  resetItem,
  unhide,
  upsertItem,
  validateOverlay,
  type Origin,
} from "./overlayOps";
import { TERM_COMMANDS_KEY, useTermCommands } from "./useTermCommands";
import type { CmdError } from "../schema";
import "./TerminalCommandsSection.css";

interface Problem {
  message: string;
  defects: CmdError[];
}

function preview(i: EffItem): string {
  if (i.kind === "chord") return (i.keys ?? []).join(" + ");
  const t = (i.text ?? "").replace(/\s+/g, " ");
  return t.length > 80 ? `${t.slice(0, 80)}…` : t;
}

const ORIGIN_TEXT: Record<Origin, string> = { default: "default", changed: "changed", added: "added" };

export function TerminalCommandsSection() {
  const qc = useQueryClient();
  const confirm = useConfirm();
  const q = useTermCommands();
  const data = q.data;
  const [harness, setHarness] = useState<string>(EDITOR_HARNESSES[0]);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [draftErrors, setDraftErrors] = useState<CmdError[]>([]);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [busy, setBusy] = useState(false);
  const [importText, setImportText] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);

  const overlay: OverlayPut = data?.overlay ?? EMPTY_OVERLAY;
  const groups = data?.harnesses[harness]?.groups ?? [];
  const hiddenIds = data?.hidden?.[harness] ?? [];

  const fail = (e: unknown): void => {
    if (e instanceof CommandsApiError) {
      if (e.stale) {
        setStale(true);
        return;
      }
      setProblem({ message: e.message, defects: e.errors });
      return;
    }
    setProblem({ message: e instanceof Error ? e.message : String(e), defects: [] });
  };

  /** Validate, PUT against `baseRev`, and take the answer from the response. */
  const save = async (next: OverlayPut, baseRev: number): Promise<boolean> => {
    setProblem(null);
    setNotice(null);
    const defects = validateOverlay(next);
    if (defects.length > 0) {
      setDraftErrors(defects);
      if (draft === null) setProblem({ message: "Not saved: the catalogue has defects.", defects });
      return false;
    }
    setDraftErrors([]);
    setBusy(true);
    try {
      const res = await putOverlay(next, baseRev);
      qc.setQueryData(TERM_COMMANDS_KEY, res);
      setStale(false);
      setNotice(res.rev === baseRev ? "No change." : `Saved (revision ${res.rev}).`);
      return true;
    } catch (e) {
      fail(e);
      return false;
    } finally {
      setBusy(false);
    }
  };

  const apply = (build: (o: OverlayPut) => OverlayPut | null): void => {
    if (!data) return;
    const next = build(overlay);
    if (next === null) return;
    void save(next, data.rev);
  };

  const openAdd = (): void => {
    if (!data) return;
    setDraftErrors([]);
    setDraft({
      mode: "add",
      harness,
      baseRev: data.rev,
      itemId: null,
      groupId: groups[0]?.id ?? NEW_GROUP,
      newGroupLabel: "",
      fields: { ...BLANK_FIELDS },
    });
  };

  const openEdit = (groupId: string, item: EffItem): void => {
    if (!data) return;
    setDraftErrors([]);
    setDraft({
      mode: "edit",
      harness,
      baseRev: data.rev,
      itemId: item.id,
      groupId,
      newGroupLabel: "",
      fields: fieldsFromItem(item as OvItem),
    });
  };

  const saveDraft = async (): Promise<void> => {
    if (!draft || !data) return;
    const f = draft.fields;
    let next: OverlayPut;
    if (draft.mode === "edit" && draft.itemId !== null) {
      const g = groups.find((x) => x.id === draft.groupId);
      next = upsertItem(overlay, draft.harness, draft.groupId, g?.label ?? draft.groupId, itemFromFields(draft.itemId, f));
    } else {
      const isNew = draft.groupId === NEW_GROUP;
      const label = isNew ? draft.newGroupLabel.trim() : (groups.find((x) => x.id === draft.groupId)?.label ?? "");
      const gid = isNew ? newGroupId(groups.map((x) => x.id), label) : draft.groupId;
      next = upsertItem(overlay, draft.harness, gid, label, itemFromFields(newItemId(draft.harness, overlay, f.label), f));
    }
    if (await save(next, draft.baseRev)) setDraft(null);
  };

  const reload = async (): Promise<void> => {
    const r = await q.refetch();
    const fresh = r.data;
    if (fresh) setDraft((d) => (d ? { ...d, baseRev: fresh.rev } : d));
    setStale(false);
  };

  const resetAll = async (): Promise<void> => {
    if (!data) return;
    const ok = await confirm({
      title: "Reset all terminal commands?",
      body: "Drops every added, changed and hidden entry for every harness and returns the shipped defaults. This can't be undone.",
      confirmLabel: "Reset everything",
    });
    if (!ok) return;
    setProblem(null);
    setNotice(null);
    setBusy(true);
    try {
      const res = await resetOverlay(data.rev);
      qc.setQueryData(TERM_COMMANDS_KEY, res);
      setStale(false);
      setNotice(res.rev === data.rev ? "No change." : `Reset to the shipped defaults (revision ${res.rev}).`);
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const exportJson = (): void => {
    const text = JSON.stringify({ harnesses: overlay.harnesses, hidden: overlay.hidden }, null, 2);
    setImportText(text);
    try {
      const url = URL.createObjectURL(new Blob([text], { type: "application/json" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = "terminal-commands.json";
      a.click();
      URL.revokeObjectURL(url);
    } catch {
      // No Blob URLs here (a test DOM): the text area still carries the JSON.
    }
  };

  const importJson = async (): Promise<void> => {
    if (!data) return;
    setProblem(null);
    setNotice(null);
    let parsed: unknown;
    try {
      parsed = JSON.parse(importText);
    } catch (e) {
      setProblem({ message: `Not valid JSON: ${e instanceof Error ? e.message : String(e)}`, defects: [] });
      return;
    }
    const defects = validateOverlay(parsed);
    if (defects.length > 0) {
      setProblem({ message: `Nothing imported: ${defects.length} defect(s).`, defects });
      return;
    }
    const next = prune(parsed as OverlayPut);
    const c = counts(next);
    const ok = await confirm({
      title: "Replace the overlay with this file?",
      body: `This replaces your whole overlay with ${c.items} item(s) in ${c.groups} group(s) and ${c.hidden} hidden default(s).`,
      confirmLabel: "Import",
      danger: false,
    });
    if (!ok) return;
    await save(next, data.rev);
  };

  if (q.isPending) return <p className="tcs__muted">Loading the command catalogue…</p>;
  if (q.isError || !data) {
    return (
      <div className="tcs__alert" role="alert">
        Couldn't load the command catalogue: {q.error instanceof Error ? q.error.message : "unknown error"}
        <button type="button" className="tcs__btn" onClick={() => void q.refetch()}>
          Retry
        </button>
      </div>
    );
  }

  const problemBlock = problem && (
    <div className="tcs__problem" data-testid="tcs-problem">
      {problem.defects.length > 0 ? (
        <Defects defects={problem.defects} title={problem.message} />
      ) : (
        <div className="tcs__alert" role="alert">
          {problem.message}
        </div>
      )}
    </div>
  );

  return (
    <section className="settings__section tcs" id="terminal-commands" aria-label="terminal commands">
      <h2 className="settings__h2">Terminal commands</h2>
      <p className="settings__hint">
        The commands, replies and key chords offered for each harness. Your edits are stored by the daemon, so every
        device sees them. A default you edit shows as <em>changed</em>; one you hide stays listed under Hidden defaults.
        Default entries keep their shipped order; entries you add can be moved.
      </p>

      {stale && (
        <div className="tcs__alert" role="alert" data-testid="tcs-stale">
          Changed elsewhere, reload before saving. Your open draft is kept.
          <button type="button" className="tcs__btn" onClick={() => void reload()}>
            Reload
          </button>
        </div>
      )}
      {notice && (
        <p className="tcs__notice" role="status" data-testid="tcs-notice">
          {notice}
        </p>
      )}
      {!draft && problemBlock}

      <div className="tcs__bar" role="group" aria-label="harness">
        {EDITOR_HARNESSES.map((h) => (
          <button
            key={h}
            type="button"
            className={`tcs__btn${h === harness ? " is-active" : ""}`}
            aria-pressed={h === harness}
            onClick={() => setHarness(h)}
          >
            {h}
          </button>
        ))}
        <button type="button" className="tcs__btn tcs__btn--primary" disabled={busy || draft !== null} onClick={openAdd}>
          Add item
        </button>
      </div>

      {draft && (
        <>
          {problemBlock}
          <ItemForm
            draft={draft}
            groups={groups.map((g) => ({ id: g.id, label: g.label }))}
            busy={busy}
            errors={draftErrors}
            onChange={setDraft}
            onSave={() => void saveDraft()}
            onDiscard={() => {
              setDraft(null);
              setDraftErrors([]);
            }}
          />
        </>
      )}

      {groups.length === 0 && <p className="tcs__muted">No entries for {harness}.</p>}
      {groups.map((g) => (
        <section key={g.id} className="tcs__group" aria-label={`${g.label} group`}>
          <h3 className="tcs__h3">{g.label}</h3>
          <ul className="tcs__list">
            {g.items.map((i) => {
              const origin = originOf(harness, i.id, i.origin);
              const canUp = moveItem(overlay, harness, i.id, -1) !== null;
              const canDown = moveItem(overlay, harness, i.id, 1) !== null;
              return (
                <li key={i.id} className="tcs__item" data-testid={`tcs-item-${i.id}`}>
                  <div className="tcs__main">
                    <span className="tcs__label">{i.label}</span>
                    <span className={`tcs__badge tcs__badge--${origin}`}>{ORIGIN_TEXT[origin]}</span>
                    {i.unverified === true && <span className="tcs__badge tcs__badge--unverified">unverified</span>}
                    <span className="tcs__kind">
                      {i.kind}
                      {i.kind !== "chord" ? ` · ${i.mode ?? "paste"}` : ""}
                    </span>
                    <span className="tcs__preview">{preview(i)}</span>
                  </div>
                  <div className="tcs__actions">
                    <button type="button" className="tcs__btn" disabled={busy || draft !== null} aria-label={`Edit ${i.label}`} onClick={() => openEdit(g.id, i)}>
                      Edit
                    </button>
                    <button type="button" className="tcs__btn" disabled={busy || !canUp} aria-label={`Move ${i.label} up`} onClick={() => apply((o) => moveItem(o, harness, i.id, -1))}>
                      Up
                    </button>
                    <button type="button" className="tcs__btn" disabled={busy || !canDown} aria-label={`Move ${i.label} down`} onClick={() => apply((o) => moveItem(o, harness, i.id, 1))}>
                      Down
                    </button>
                    {origin === "changed" && (
                      <button type="button" className="tcs__btn" disabled={busy} aria-label={`Reset ${i.label}`} onClick={() => apply((o) => resetItem(o, harness, i.id))}>
                        Reset
                      </button>
                    )}
                    {origin === "added" ? (
                      <button type="button" className="tcs__btn tcs__btn--danger" disabled={busy} aria-label={`Delete ${i.label}`} onClick={() => apply((o) => removeItem(o, harness, i.id))}>
                        Delete
                      </button>
                    ) : (
                      <button type="button" className="tcs__btn" disabled={busy} aria-label={`Hide ${i.label}`} onClick={() => apply((o) => hideDefault(o, harness, i.id))}>
                        Hide
                      </button>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      ))}

      <section className="tcs__group" aria-label="hidden defaults">
        <h3 className="tcs__h3">Hidden defaults</h3>
        {hiddenIds.length === 0 ? (
          <p className="tcs__muted">Nothing hidden.</p>
        ) : (
          <ul className="tcs__list">
            {hiddenIds.map((id) => {
              const label = defaultItem(harness, id)?.label ?? id;
              return (
                <li key={id} className="tcs__item" data-testid={`tcs-hidden-${id}`}>
                  <div className="tcs__main">
                    <span className="tcs__label">{label}</span>
                    <span className="tcs__badge tcs__badge--hidden">hidden</span>
                  </div>
                  <div className="tcs__actions">
                    <button type="button" className="tcs__btn" disabled={busy} aria-label={`Unhide ${label}`} onClick={() => apply((o) => unhide(o, harness, id))}>
                      Unhide
                    </button>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      <section className="tcs__group" aria-label="import and export">
        <h3 className="tcs__h3">Import / export</h3>
        <p className="settings__hint">
          The overlay as JSON ({`{harnesses, hidden}`}). Import checks every entry first and replaces the whole overlay
          in one save, or changes nothing.
        </p>
        <label htmlFor="tcs-json" className="tcs__sr">
          Overlay JSON
        </label>
        <textarea id="tcs-json" className="tcs__json" rows={6} value={importText} onChange={(e) => setImportText(e.target.value)} />
        <div className="tcs__row">
          <button type="button" className="tcs__btn" onClick={exportJson}>
            Export
          </button>
          <button type="button" className="tcs__btn" onClick={() => fileRef.current?.click()}>
            Choose file…
          </button>
          <input
            ref={fileRef}
            type="file"
            accept="application/json,.json"
            className="tcs__sr"
            aria-label="Overlay JSON file"
            onChange={(e) => {
              const file = e.target.files?.[0];
              if (file) void file.text().then(setImportText);
              e.target.value = "";
            }}
          />
          <button type="button" className="tcs__btn" disabled={busy || importText.trim() === ""} onClick={() => void importJson()}>
            Import
          </button>
        </div>
      </section>

      <div className="tcs__row">
        <button type="button" className="tcs__btn tcs__btn--danger" disabled={busy} onClick={() => void resetAll()}>
          Reset everything to shipped defaults
        </button>
      </div>
    </section>
  );
}

export default TerminalCommandsSection;
