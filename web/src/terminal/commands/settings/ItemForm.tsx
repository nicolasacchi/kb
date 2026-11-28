// CMDSETTINGS: the add/edit form for one catalogue item. Controlled: the
// parent owns the draft (so a refetch never touches it) and the save.
import { useId, useState } from "react";
import type { CmdError, ItemKind } from "../schema";
import { KEY_CHOICES, pasteNewlineWarning, type ItemFields } from "./overlayOps";

export const NEW_GROUP = "__new__";

export interface Draft {
  mode: "add" | "edit";
  harness: string;
  /** The revision the draft was opened against (sent as If-Match on save). */
  baseRev: number;
  /** The id being edited; a fresh item gets one at save time. */
  itemId: string | null;
  groupId: string;
  newGroupLabel: string;
  fields: ItemFields;
}

const KIND_LABELS: Record<ItemKind, string> = {
  command: "Slash command",
  reply: "Text reply",
  chord: "Key chord",
};

export function ItemForm({
  draft,
  groups,
  busy,
  errors,
  onChange,
  onSave,
  onDiscard,
}: {
  draft: Draft;
  groups: { id: string; label: string }[];
  busy: boolean;
  errors: CmdError[];
  onChange: (next: Draft) => void;
  onSave: () => void;
  onDiscard: () => void;
}) {
  const uid = useId();
  const [pick, setPick] = useState<string>(KEY_CHOICES[0]);
  const f = draft.fields;
  const set = (patch: Partial<ItemFields>) => onChange({ ...draft, fields: { ...f, ...patch } });
  const id = (n: string) => `${uid}-${n}`;
  const editing = draft.mode === "edit";
  return (
    <form
      className="tcs__form"
      aria-label={editing ? "Edit item" : "Add item"}
      data-testid="tcs-form"
      onSubmit={(e) => {
        e.preventDefault();
        onSave();
      }}
    >
      <h3 className="tcs__h3">{editing ? "Edit item" : "Add item"}</h3>

      <div className="tcs__field">
        <label htmlFor={id("label")}>Label</label>
        <input id={id("label")} value={f.label} maxLength={80} onChange={(e) => set({ label: e.target.value })} />
      </div>

      <div className="tcs__field">
        <label htmlFor={id("kind")}>Kind</label>
        <select
          id={id("kind")}
          value={f.kind}
          onChange={(e) => set({ kind: e.target.value as ItemKind })}
        >
          {(Object.keys(KIND_LABELS) as ItemKind[]).map((k) => (
            <option key={k} value={k}>
              {KIND_LABELS[k]}
            </option>
          ))}
        </select>
      </div>

      {!editing && (
        <div className="tcs__field">
          <label htmlFor={id("group")}>Group</label>
          <select
            id={id("group")}
            value={draft.groupId}
            onChange={(e) => onChange({ ...draft, groupId: e.target.value })}
          >
            {groups.map((g) => (
              <option key={g.id} value={g.id}>
                {g.label}
              </option>
            ))}
            <option value={NEW_GROUP}>New group…</option>
          </select>
        </div>
      )}
      {!editing && draft.groupId === NEW_GROUP && (
        <div className="tcs__field">
          <label htmlFor={id("newgroup")}>New group label</label>
          <input
            id={id("newgroup")}
            value={draft.newGroupLabel}
            maxLength={40}
            onChange={(e) => onChange({ ...draft, newGroupLabel: e.target.value })}
          />
        </div>
      )}

      {f.kind !== "chord" && (
        <>
          <div className="tcs__field">
            <label htmlFor={id("text")}>Text</label>
            <textarea id={id("text")} rows={3} value={f.text} onChange={(e) => set({ text: e.target.value })} />
          </div>
          <div className="tcs__field">
            <label htmlFor={id("mode")}>Mode</label>
            <select
              id={id("mode")}
              value={f.mode}
              onChange={(e) => set({ mode: e.target.value as "paste" | "run" })}
            >
              <option value="paste">paste (no Enter)</option>
              <option value="run">run (paste, then Enter)</option>
            </select>
          </div>
          {pasteNewlineWarning(f) && (
            <p className="tcs__warn" role="status" data-testid="tcs-newline-warning">
              A newline can press Enter when bracketed paste is off.
            </p>
          )}
        </>
      )}

      {f.kind === "chord" && (
        <fieldset className="tcs__fieldset">
          <legend>Keys (sent in order)</legend>
          <ul className="tcs__chips" aria-label="chosen keys">
            {f.keys.map((k, i) => (
              <li key={`${k}-${i}`} className="tcs__chip">
                <span>{k}</span>
                <button
                  type="button"
                  className="tcs__btn tcs__btn--icon"
                  aria-label={`Remove key ${k}`}
                  onClick={() => set({ keys: f.keys.filter((_, j) => j !== i) })}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
          <div className="tcs__row">
            <label htmlFor={id("addkey")} className="tcs__sr">
              Key to add
            </label>
            <select id={id("addkey")} value={pick} onChange={(e) => setPick(e.target.value)}>
              {KEY_CHOICES.map((k) => (
                <option key={k} value={k}>
                  {k}
                </option>
              ))}
            </select>
            <button
              type="button"
              className="tcs__btn"
              disabled={f.keys.length >= 8}
              onClick={() => set({ keys: [...f.keys, pick] })}
            >
              Add key
            </button>
          </div>
        </fieldset>
      )}

      <div className="tcs__field">
        <label htmlFor={id("desc")}>Description (optional)</label>
        <input id={id("desc")} value={f.description} onChange={(e) => set({ description: e.target.value })} />
      </div>

      {f.kind !== "chord" && (
        <fieldset className="tcs__fieldset">
          <legend>Arguments (optional, used as {"{name}"} in the text)</legend>
          {f.args.map((a, i) => (
            <div className="tcs__arg" key={i}>
              <div className="tcs__field">
                <label htmlFor={id(`an${i}`)}>Argument {i + 1} name</label>
                <input
                  id={id(`an${i}`)}
                  value={a.name}
                  onChange={(e) => set({ args: f.args.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)) })}
                />
              </div>
              <div className="tcs__field">
                <label htmlFor={id(`ah${i}`)}>Argument {i + 1} hint</label>
                <input
                  id={id(`ah${i}`)}
                  value={a.hint}
                  onChange={(e) => set({ args: f.args.map((x, j) => (j === i ? { ...x, hint: e.target.value } : x)) })}
                />
              </div>
              <div className="tcs__field">
                <label htmlFor={id(`ad${i}`)}>Argument {i + 1} default</label>
                <input
                  id={id(`ad${i}`)}
                  value={a.default}
                  onChange={(e) => set({ args: f.args.map((x, j) => (j === i ? { ...x, default: e.target.value } : x)) })}
                />
              </div>
              <button
                type="button"
                className="tcs__btn"
                aria-label={`Remove argument ${i + 1}`}
                onClick={() => set({ args: f.args.filter((_, j) => j !== i) })}
              >
                Remove argument
              </button>
            </div>
          ))}
          <button
            type="button"
            className="tcs__btn"
            disabled={f.args.length >= 4}
            onClick={() => set({ args: [...f.args, { name: "", hint: "", default: "" }] })}
          >
            Add argument
          </button>
        </fieldset>
      )}

      <label className="tcs__check">
        <input type="checkbox" checked={f.unverified} onChange={(e) => set({ unverified: e.target.checked })} />
        <span>Mark as unverified</span>
      </label>

      {errors.length > 0 && <Defects defects={errors} title="This item can't be saved yet" />}

      <div className="tcs__row">
        <button type="submit" className="tcs__btn tcs__btn--primary" disabled={busy}>
          Save item
        </button>
        <button type="button" className="tcs__btn" onClick={onDiscard}>
          Discard draft
        </button>
      </div>
    </form>
  );
}

/** Every defect, in order: the machine code and the JSON pointer. */
export function Defects({ defects, title }: { defects: CmdError[]; title: string }) {
  return (
    <div className="tcs__alert" role="alert" data-testid="tcs-defects">
      <strong>{title}</strong>
      <ul>
        {defects.map((d, i) => (
          <li key={`${d.code}-${d.path}-${i}`}>
            <code>{d.code}</code> at <code>{d.path === "" ? "(root)" : d.path}</code>
          </li>
        ))}
      </ul>
    </div>
  );
}
