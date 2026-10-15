import { useEffect, useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import "./viewbar.css";

export type ControlOption = { value: string; label: string };

export type Control = {
  id: string;
  /** The verb shown on the control. Always comes from the caller's registry row. */
  label: string;
  key?: string;
  kind: "button" | "toggle" | "select" | "segment";
  active?: boolean;
  disabled?: boolean;
  /** Why the control is disabled; rendered as a visible caption, never hidden. */
  reason?: string;
  options?: ControlOption[];
  value?: string;
  onSelect?: (value?: string) => void;
  icon?: ReactNode;
  /** Extra `data-*` attributes spread onto the rendered control (e2e selectors). */
  attrs?: Record<string, string>;
};

export type Cluster = { label: string; controls: Control[] };

export type ViewBarProps = {
  title?: string;
  count?: number | string;
  clusters: Cluster[];
  maxVisible?: number;
  moreLabel?: string;
  /** Replace how a control is drawn in the bar; `node` is the default rendering. */
  renderControl?: (control: Control, node: ReactNode) => ReactNode;
};

function Keycap({ k }: { k?: string }) {
  return k ? <kbd className="kb-viewbar__kbd">{k}</kbd> : null;
}

function BarControl({ c, uid }: { c: Control; uid: string }) {
  const base = `${uid}-${c.id}`;
  const descId = `${base}-d`;
  const reasonId = `${base}-r`;
  const hasReason = !!(c.disabled && c.reason);
  const described = [c.key ? descId : "", hasReason ? reasonId : ""].filter(Boolean).join(" ") || undefined;
  const title = [c.label, c.key ? `(${c.key})` : "", hasReason ? `- ${c.reason}` : ""].filter(Boolean).join(" ");
  const attrs = c.attrs ?? {};
  let control: ReactNode;
  if (c.kind === "select") {
    control = (
      <label className="kb-viewbar__field">
        <span className="kb-viewbar__field-label">{c.label}</span>
        <select
          className="kb-viewbar__select"
          data-kb-viewbar-control={c.id}
          {...attrs}
          value={c.value ?? ""}
          disabled={c.disabled}
          title={title}
          aria-describedby={described}
          onChange={(e) => c.onSelect?.(e.target.value)}
        >
          {(c.options ?? []).map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      </label>
    );
  } else if (c.kind === "segment") {
    control = (
      <div
        role="group"
        aria-label={c.label}
        className="kb-viewbar__segment"
        data-kb-viewbar-control={c.id}
        {...attrs}
        title={title}
        aria-describedby={described}
      >
        {(c.options ?? []).map((o) => (
          <button
            key={o.value}
            type="button"
            className="kb-viewbar__btn"
            aria-pressed={c.value === o.value}
            disabled={c.disabled}
            onClick={() => c.onSelect?.(o.value)}
          >
            {o.label}
          </button>
        ))}
      </div>
    );
  } else {
    control = (
      <button
        type="button"
        className="kb-viewbar__btn"
        data-kb-viewbar-control={c.id}
        {...attrs}
        aria-pressed={c.kind === "toggle" ? !!c.active : undefined}
        disabled={c.disabled}
        title={title}
        aria-describedby={described}
        onClick={() => c.onSelect?.()}
      >
        {c.icon}
        <span>{c.label}</span>
      </button>
    );
  }
  return (
    <span className="kb-viewbar__item">
      {control}
      {c.key ? (
        <span id={descId} className="kb-viewbar__sr">
          {c.label} <Keycap k={c.key} />
        </span>
      ) : null}
      {hasReason ? (
        <span id={reasonId} className="kb-viewbar__reason">
          {c.reason}
        </span>
      ) : null}
    </span>
  );
}

type Overflow = { cluster: string; control: Control };

function MenuRows({ o, close }: { o: Overflow; close: () => void }) {
  const c = o.control;
  const cap = <span className="kb-viewbar__menu-cluster">{o.cluster}</span>;
  const attrs = c.attrs ?? {};
  const reason = c.disabled && c.reason ? <span className="kb-viewbar__reason">{c.reason}</span> : null;
  if (c.kind === "select") {
    return (
      <div role="none" className="kb-viewbar__menu-row" {...attrs}>
        {cap}
        <label className="kb-viewbar__menu-label">
          {c.label}
          <select
            className="kb-viewbar__select"
            value={c.value ?? ""}
            disabled={c.disabled}
            onChange={(e) => {
              c.onSelect?.(e.target.value);
              close();
            }}
          >
            {(c.options ?? []).map((opt) => (
              <option key={opt.value} value={opt.value}>
                {opt.label}
              </option>
            ))}
          </select>
        </label>
        {reason}
      </div>
    );
  }
  if (c.kind === "segment") {
    return (
      <>
        {(c.options ?? []).map((opt) => (
          <button
            key={opt.value}
            type="button"
            role="menuitemcheckbox"
            aria-checked={c.value === opt.value}
            className="kb-viewbar__menu-row"
            disabled={c.disabled}
            {...attrs}
            onClick={() => {
              c.onSelect?.(opt.value);
              close();
            }}
          >
            {cap}
            <span className="kb-viewbar__menu-label">
              {c.label}: {opt.label}
            </span>
          </button>
        ))}
        {reason}
      </>
    );
  }
  const toggle = c.kind === "toggle";
  return (
    <button
      type="button"
      role={toggle ? "menuitemcheckbox" : "menuitem"}
      aria-checked={toggle ? !!c.active : undefined}
      className="kb-viewbar__menu-row"
      disabled={c.disabled}
      {...attrs}
      onClick={() => {
        c.onSelect?.();
        close();
      }}
    >
      {cap}
      <span className="kb-viewbar__menu-label">{c.label}</span>
      <Keycap k={c.key} />
      {reason}
    </button>
  );
}

export function ViewBar({ title, count, clusters, maxVisible = 9, moreLabel = "More", renderControl }: ViewBarProps) {
  const uid = useId();
  const menuId = `${uid}-menu`;
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement | null>(null);

  let seen = 0;
  const visible: Cluster[] = [];
  const overflow: Overflow[] = [];
  for (const cl of clusters) {
    const shown: Control[] = [];
    for (const control of cl.controls) {
      if (seen < maxVisible) shown.push(control);
      else overflow.push({ cluster: cl.label, control });
      seen += 1;
    }
    if (shown.length > 0) visible.push({ label: cl.label, controls: shown });
  }
  const hasOverflow = overflow.length > 0;

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    const onDown = (e: Event) => {
      const el = wrapRef.current;
      if (el && e.target instanceof Node && !el.contains(e.target)) setOpen(false);
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onDown);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("pointerdown", onDown);
    };
  }, [open]);

  return (
    <div data-kb-viewbar role="toolbar" aria-label={title ?? "View"} className="kb-viewbar">
      <div className="kb-viewbar__lead">
        {title ? <span className="kb-viewbar__title">{title}</span> : null}
        {count !== undefined ? <span className="kb-viewbar__count">{count}</span> : null}
      </div>
      <div className="kb-viewbar__clusters">
        {visible.map((cl) => (
          <fieldset key={cl.label} data-kb-viewbar-cluster className="kb-viewbar__cluster">
            <legend className="kb-viewbar__legend">{cl.label}</legend>
            {cl.controls.map((c) => {
              const node = <BarControl c={c} uid={uid} />;
              return <span key={c.id}>{renderControl ? renderControl(c, node) : node}</span>;
            })}
          </fieldset>
        ))}
        {hasOverflow ? (
          <span className="kb-viewbar__more" ref={wrapRef}>
            <button
              type="button"
              className="kb-viewbar__btn"
              data-kb-viewbar-more
              aria-haspopup="menu"
              aria-expanded={open}
              aria-controls={open ? menuId : undefined}
              onClick={() => setOpen((v) => !v)}
            >
              {moreLabel}
            </button>
            {open ? (
              <div id={menuId} role="menu" aria-label={moreLabel} className="kb-viewbar__menu">
                {overflow.map((o) => (
                  <MenuRows key={o.control.id} o={o} close={() => setOpen(false)} />
                ))}
              </div>
            ) : null}
          </span>
        ) : null}
      </div>
    </div>
  );
}
