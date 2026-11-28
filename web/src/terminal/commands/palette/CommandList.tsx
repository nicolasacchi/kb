// CMDPALETTE: the searchable, grouped command list. ONE component behind the
// desktop dialog (`CommandPalette`) and the rail's `commands` tab
// (`CommandsRail`), so both send through the same paths. Search is a combobox
// over a listbox; an entry with args swaps the list for a small inline form.

import { useId, useMemo, useState, type KeyboardEvent, type ReactElement } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { useIdentity } from "../../../hooks/useArtifactHost";
import { useConfirm } from "../../../components/ConfirmProvider";
import { toast } from "../../../lib/toast";
import { CAPTION } from "../../phone/planSend";
import type { Drive } from "../../phone/driveApi";
import { termCommandsQueryOptions } from "../settings/useTermCommands";
import { effectiveCatalogue, forHarness, type Effective, groupEntries, searchEntries, toKeystrokes, type Entry, type Keystrokes } from "../catalog";
import { sendEntry, unsendable } from "./sendEntry";
import "./palette.css";

export interface CommandListProps {
  termId: string;
  harness: string;
  variant: "dialog" | "rail" | "sheet";
  /** CMDSHEET: the phone dock's viewer id, so a send carries the same `viewer` the key bar does (absent = none, as before). */
  viewer?: string | null;
  /** The text reached the pane (sent, or pasted with the Enter refused). */
  onSent?: () => void;
  /** Called when the Edit commands link is followed. */
  onNavigate?: () => void;
  autoFocus?: boolean;
}

/** Plain-text preview of what an entry sends. */
export function previewOf(e: Entry): string {
  const ks = toKeystrokes(e);
  if (ks.kind === "text") return ks.text;
  if (ks.kind === "keys") return ks.keys.join(" ");
  if (ks.kind === "incomplete") return e.item.text ?? "";
  return "(invalid entry)";
}

function modeBadge(e: Entry): "paste" | "run" | "keys" {
  if (e.item.kind === "chord") return "keys";
  return e.item.mode === "run" ? "run" : "paste";
}

function blank(v: string | undefined): boolean {
  return v === undefined || v.trim() === "";
}

/** The effective catalogue (the settings unit's query, gated on the terminal tier); shipped defaults while it has no data. */
function useCatalogue(): { catalogue: Effective; fallback: boolean } {
  const identity = useIdentity();
  const q = useQuery({ ...termCommandsQueryOptions, enabled: identity?.terminal_admitted === true });
  const shipped = useMemo(() => effectiveCatalogue(null), []);
  if (q.data === undefined || q.isError) return { catalogue: shipped, fallback: true };
  return { catalogue: q.data as unknown as Effective, fallback: false };
}

export function CommandList(props: CommandListProps): ReactElement {
  const { termId, harness, variant, viewer = null, onSent, onNavigate, autoFocus } = props;
  const uid = useId();
  const confirm = useConfirm();
  const { catalogue, fallback } = useCatalogue();
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [pending, setPending] = useState<Entry | null>(null);
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const entries = useMemo(() => forHarness(catalogue, harness), [catalogue, harness]);
  const searching = query.trim() !== "";
  const rows = useMemo(() => (searching ? searchEntries(entries, query) : entries), [entries, query, searching]);
  const groups = useMemo(() => (searching ? [{ id: "results", label: "Results", entries: rows }] : groupEntries(rows)), [rows, searching]);
  const activeIdx = rows.length === 0 ? -1 : Math.min(active, rows.length - 1);
  const optId = (i: number): string => `${uid}-opt-${i}`;

  async function doSend(ks: Keystrokes): Promise<void> {
    if (busy) return;
    setBusy(true);
    const drive: Drive = { termId, harness, viewer, confirm };
    const r = await sendEntry(drive, ks);
    setBusy(false);
    if (r.kind === "sent") onSent?.();
    else if (r.kind === "refused") {
      toast.err(CAPTION.enterRefused(r.detail));
      onSent?.();
    } else if (r.kind === "error") toast.err(r.detail);
  }

  function activate(e: Entry): void {
    if (busy) return;
    if ((e.item.args ?? []).length > 0) {
      const init: Record<string, string> = {};
      for (const a of e.item.args ?? []) init[a.name] = a.default ?? "";
      setValues(init);
      setPending(e);
      return;
    }
    void doSend(toKeystrokes(e));
  }

  function onSearchKey(ev: KeyboardEvent<HTMLInputElement>): void {
    if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
      ev.preventDefault();
      if (rows.length === 0) return;
      const d = ev.key === "ArrowDown" ? 1 : -1;
      setActive((activeIdx + d + rows.length) % rows.length);
    } else if (ev.key === "Enter" && !ev.nativeEvent.isComposing) {
      ev.preventDefault();
      if (activeIdx >= 0) activate(rows[activeIdx]);
    }
  }

  const editLink = (
    <Link className="kb-cmdp__edit" to="/settings#terminal-commands" onClick={onNavigate}>
      Edit commands
    </Link>
  );

  if (pending !== null) {
    const args = pending.item.args ?? [];
    const given: Record<string, string> = {};
    for (const a of args) if (!blank(values[a.name])) given[a.name] = values[a.name];
    const ks = toKeystrokes(pending, given);
    const reason = ks.kind === "invalid" ? unsendable(ks) : null;
    const missing = args.some((a) => blank(values[a.name]));
    const disabled = busy || missing || ks.kind !== "text";
    const submit = (): void => {
      if (!disabled) void doSend(ks);
    };
    return (
      <div className={`kb-cmdp kb-cmdp--${variant}`} data-testid="kb-cmdp">
        <form
          className="kb-cmdp__form"
          aria-label={`Arguments for ${pending.item.label}`}
          onSubmit={(ev) => {
            ev.preventDefault();
            submit();
          }}
        >
          <p className="kb-cmdp__formtitle">{pending.item.label}</p>
          {args.map((a, i) => (
            <label key={a.name} className="kb-cmdp__field">
              <span>{a.name}</span>
              <textarea
                rows={1}
                value={values[a.name] ?? ""}
                placeholder={a.hint}
                // eslint-disable-next-line jsx-a11y/no-autofocus
                autoFocus={i === 0}
                onChange={(ev) => setValues({ ...values, [a.name]: ev.target.value })}
                onKeyDown={(ev) => {
                  if (ev.key === "Enter" && !ev.shiftKey && !ev.nativeEvent.isComposing) {
                    ev.preventDefault();
                    submit();
                  }
                }}
              />
            </label>
          ))}
          <p className="kb-cmdp__preview" data-testid="kb-cmdp-formpreview">
            {ks.kind === "text" ? ks.text : ""}
          </p>
          {reason !== null && (
            <p role="alert" className="kb-cmdp__alert">
              {reason}
            </p>
          )}
          <div className="kb-cmdp__formbtns">
            <button type="button" className="kb-cmdp__btn" onClick={() => setPending(null)}>
              Back
            </button>
            <button type="submit" className="kb-cmdp__btn kb-cmdp__btn--go" disabled={disabled}>
              Send
            </button>
          </div>
        </form>
      </div>
    );
  }

  let offset = 0;
  return (
    <div className={`kb-cmdp kb-cmdp--${variant}`} data-testid="kb-cmdp">
      <input
        className="kb-cmdp__search"
        type="search"
        role="combobox"
        aria-label="Search commands"
        aria-expanded="true"
        aria-controls={`${uid}-list`}
        aria-activedescendant={activeIdx >= 0 ? optId(activeIdx) : undefined}
        aria-autocomplete="list"
        placeholder="Search commands"
        value={query}
        // eslint-disable-next-line jsx-a11y/no-autofocus
        autoFocus={autoFocus}
        onChange={(ev) => {
          setQuery(ev.target.value);
          setActive(0);
        }}
        onKeyDown={onSearchKey}
      />
      {fallback && <p className="kb-cmdp__caption">Showing shipped commands</p>}
      <div className="kb-cmdp__scroll">
        <div id={`${uid}-list`} role="listbox" aria-label="Commands" className="kb-cmdp__list">
          {groups.map((g) => {
            const base = offset;
            offset += g.entries.length;
            return (
              <div key={g.id} role="group" aria-labelledby={`${uid}-g-${g.id}`} className="kb-cmdp__group">
                <p id={`${uid}-g-${g.id}`} className="kb-cmdp__ghead">
                  {g.label}
                </p>
                {g.entries.map((e, j) => {
                  const i = base + j;
                  const badge = modeBadge(e);
                  return (
                    <div
                      key={e.item.id}
                      id={optId(i)}
                      role="option"
                      aria-selected={i === activeIdx}
                      className="kb-cmdp__row"
                      data-active={i === activeIdx ? "" : undefined}
                      data-testid="kb-cmdp-row"
                      onMouseDown={(ev) => ev.preventDefault()}
                      onClick={() => {
                        setActive(i);
                        activate(e);
                      }}
                    >
                      <span className="kb-cmdp__label">{e.item.label}</span>
                      <span className={`kb-cmdp__badge kb-cmdp__badge--${badge}`}>{badge}</span>
                      {e.item.unverified === true && <span className="kb-cmdp__badge kb-cmdp__badge--unv">unverified</span>}
                      {e.item.description !== undefined && <span className="kb-cmdp__desc">{e.item.description}</span>}
                      <code className="kb-cmdp__preview">{previewOf(e)}</code>
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>
        {rows.length === 0 && (
          <p className="kb-cmdp__empty">{searching ? "No match" : `No commands for ${harness}`}</p>
        )}
      </div>
      <div className="kb-cmdp__foot">
        {editLink}
        {variant === "dialog" && <span className="kb-cmdp__hint">Alt+K</span>}
      </div>
    </div>
  );
}
