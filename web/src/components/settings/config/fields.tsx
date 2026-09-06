// CE — controlled form primitives for the daemon-config editor. All
// presentational: they take a value + onChange + optional inline error and
// reuse the existing `.settings__*` classes plus a small `.cfg__*` set
// (see app.css). No data fetching here.

import { useEffect, useState, type ReactNode } from "react";
import { Icon } from "../../icons";

export function FieldRow({
  label,
  htmlFor,
  hint,
  error,
  children,
}: {
  label: string;
  htmlFor?: string;
  hint?: string;
  error?: string;
  children: ReactNode;
}) {
  return (
    <div className="settings__field cfg__field">
      <label className="settings__label" htmlFor={htmlFor}>
        {label}
      </label>
      <div className="cfg__control">
        {children}
        {hint && !error && <span className="settings__hint cfg__hint">{hint}</span>}
        {error && (
          <span className="cfg__field-error" role="alert">
            {error}
          </span>
        )}
      </div>
    </div>
  );
}

let _idSeq = 0;
function nextId(prefix: string): string {
  _idSeq += 1;
  return `${prefix}-${_idSeq}`;
}

export function TextField({
  label,
  value,
  onChange,
  placeholder,
  hint,
  error,
  mono = false,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  hint?: string;
  error?: string;
  mono?: boolean;
}) {
  const id = nextId("cfg-text");
  return (
    <FieldRow label={label} htmlFor={id} hint={hint} error={error}>
      <input
        id={id}
        type="text"
        className={`cfg__input ${mono ? "cfg__input--mono" : ""}`}
        value={value}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => onChange(e.target.value)}
      />
    </FieldRow>
  );
}

export function NumberField({
  label,
  value,
  onChange,
  min = 0,
  placeholder,
  hint,
  error,
}: {
  label: string;
  value: number | null | undefined;
  onChange: (v: number | null) => void;
  min?: number;
  placeholder?: string;
  hint?: string;
  error?: string;
}) {
  const id = nextId("cfg-num");
  return (
    <FieldRow label={label} htmlFor={id} hint={hint} error={error}>
      <input
        id={id}
        type="number"
        min={min}
        className="cfg__input cfg__input--num"
        value={value ?? ""}
        placeholder={placeholder ?? "(default)"}
        onChange={(e) => {
          const raw = e.target.value.trim();
          onChange(raw === "" ? null : Number(raw));
        }}
      />
    </FieldRow>
  );
}

export function ToggleField({
  label,
  value,
  onChange,
  hint,
}: {
  label: string;
  value: boolean;
  onChange: (v: boolean) => void;
  hint?: string;
}) {
  return (
    <FieldRow label={label} hint={hint}>
      <div className="settings__buttons" role="radiogroup" aria-label={label}>
        <button
          type="button"
          role="radio"
          aria-checked={!value}
          className={`settings__btn ${!value ? "is-active" : ""}`}
          onClick={() => onChange(false)}
        >
          off
        </button>
        <button
          type="button"
          role="radio"
          aria-checked={value}
          className={`settings__btn ${value ? "is-active" : ""}`}
          onClick={() => onChange(true)}
        >
          on
        </button>
      </div>
    </FieldRow>
  );
}

export function SelectField({
  label,
  value,
  options,
  onChange,
  allowEmpty = true,
  emptyLabel = "(default)",
  hint,
  error,
}: {
  label: string;
  value: string | null | undefined;
  options: readonly string[];
  onChange: (v: string | null) => void;
  allowEmpty?: boolean;
  emptyLabel?: string;
  hint?: string;
  error?: string;
}) {
  const id = nextId("cfg-sel");
  return (
    <FieldRow label={label} htmlFor={id} hint={hint} error={error}>
      <select
        id={id}
        value={value ?? ""}
        onChange={(e) => onChange(e.target.value === "" ? null : e.target.value)}
      >
        {allowEmpty && <option value="">{emptyLabel}</option>}
        {options.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </select>
    </FieldRow>
  );
}

/// Editor for an ordered list of free strings (trusted_proxies,
/// skip_patterns). Each row has a remove button; a trailing add row
/// appends. Empty rows are dropped on change.
export function StringListField({
  label,
  values,
  onChange,
  placeholder,
  hint,
}: {
  label: string;
  values: string[];
  onChange: (next: string[]) => void;
  placeholder?: string;
  hint?: string;
}) {
  const set = (i: number, v: string) => {
    const next = values.slice();
    next[i] = v;
    onChange(next);
  };
  const remove = (i: number) => onChange(values.filter((_, j) => j !== i));
  const add = () => onChange([...values, ""]);
  return (
    <FieldRow label={label} hint={hint}>
      <div className="cfg__list">
        {values.map((v, i) => (
          <div className="cfg__list-row" key={i}>
            <input
              type="text"
              className="cfg__input cfg__input--mono"
              value={v}
              placeholder={placeholder}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => set(i, e.target.value)}
            />
            <button
              type="button"
              className="settings__btn settings__btn--sm"
              aria-label="remove"
              onClick={() => remove(i)}
            >
              <Icon.X />
            </button>
          </div>
        ))}
        <button type="button" className="settings__btn settings__btn--sm cfg__list-add" onClick={add}>
          + add
        </button>
      </div>
    </FieldRow>
  );
}

export type Pair = { a: string; b: string };

/// Record form of a pairs list: empty keys dropped, duplicate keys last-wins.
/// Used by section editors that store `Record<string, string>` (templates).
export function pairsToRecord(pairs: Pair[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const { a, b } of pairs) if (a.trim() !== "") out[a] = b;
  return out;
}

export function recordToPairs(rec: Record<string, string>): Pair[] {
  return Object.entries(rec).map(([a, b]) => ({ a, b }));
}

function recordsEqual(a: Record<string, string>, b: Record<string, string>): boolean {
  const ak = Object.keys(a);
  const bk = Object.keys(b);
  if (ak.length !== bk.length) return false;
  for (const k of ak) if (a[k] !== b[k]) return false;
  return true;
}

/// Editor for a list of two-field rows (redaction pattern/replacement,
/// template name/path). Section code converts to/from its native shape.
///
/// Local draft state keeps empty-key and duplicate-key rows visible while
/// typing: parent stores often round-trip through `pairsToRecord` (drops
/// empties, collapses dup keys), which would otherwise make "+ add" a no-op.
export function PairListField({
  label,
  pairs,
  onChange,
  aPlaceholder,
  bPlaceholder,
  hint,
}: {
  label: string;
  pairs: Pair[];
  onChange: (next: Pair[]) => void;
  aPlaceholder?: string;
  bPlaceholder?: string;
  hint?: string;
}) {
  const [draft, setDraft] = useState<Pair[]>(pairs);
  // Resync only when the parent's committed record diverges from ours
  // (external load / kb switch) — not when the parent re-passes the
  // empty-stripped form of the same draft.
  useEffect(() => {
    setDraft((d) => (recordsEqual(pairsToRecord(d), pairsToRecord(pairs)) ? d : pairs));
  }, [pairs]);

  const commit = (next: Pair[]) => {
    setDraft(next);
    onChange(next);
  };
  const set = (i: number, key: "a" | "b", v: string) => {
    commit(draft.map((p, j) => (j === i ? { ...p, [key]: v } : p)));
  };
  const remove = (i: number) => commit(draft.filter((_, j) => j !== i));
  const add = () => commit([...draft, { a: "", b: "" }]);
  return (
    <FieldRow label={label} hint={hint}>
      <div className="cfg__list">
        {draft.map((p, i) => (
          <div className="cfg__kv-row" key={i}>
            <input
              type="text"
              className="cfg__input cfg__input--mono"
              value={p.a}
              placeholder={aPlaceholder}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => set(i, "a", e.target.value)}
            />
            <input
              type="text"
              className="cfg__input cfg__input--mono"
              value={p.b}
              placeholder={bPlaceholder}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => set(i, "b", e.target.value)}
            />
            <button
              type="button"
              className="settings__btn settings__btn--sm"
              aria-label="remove"
              onClick={() => remove(i)}
            >
              <Icon.X />
            </button>
          </div>
        ))}
        <button type="button" className="settings__btn settings__btn--sm cfg__list-add" onClick={add}>
          + add
        </button>
      </div>
    </FieldRow>
  );
}
