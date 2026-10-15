import { useEffect, type ReactNode } from "react";
import { Icon } from "../icons";
import type { KeyProfile, KeyRow } from "./types";
import "./keys.css";

export type HelpGroup = { title: string; rows: KeyRow[] };

/** Bucket rows by `group`, preserving first-seen order. */
export function groupRows(rows: readonly KeyRow[]): HelpGroup[] {
  const order: string[] = [];
  const by = new Map<string, KeyRow[]>();
  for (const r of rows) {
    const g = r.group ?? "";
    if (!by.has(g)) {
      by.set(g, []);
      order.push(g);
    }
    by.get(g)!.push(r);
  }
  return order.map((title) => ({ title, rows: by.get(title)! }));
}

export type KeyboardHelpProps = {
  open: boolean;
  onClose: () => void;
  /** Rows for the current surface, listed first and emphasised when `globalRows` is non-empty. */
  rows: KeyRow[];
  /** Rows available everywhere, collapsed under an "Everywhere" divider. */
  globalRows?: KeyRow[];
  /** Name of the surface, shown after the title. */
  scopeName?: string;
  title?: string;
  /** Key-profile switch. Rendered only when `profiles` is given. */
  profile?: string;
  profiles?: KeyProfile[];
  onProfileChange?: (id: string) => void;
  /** false: render the panel without the scrim and dialog semantics. */
  asSheet?: boolean;
  footer?: ReactNode;
  /** Close glyph; defaults to the shared X icon. */
  closeIcon?: ReactNode;
};

export function KeyboardHelp({
  open,
  onClose,
  rows,
  globalRows = [],
  scopeName,
  title = "Keyboard",
  profile,
  profiles,
  onProfileChange,
  asSheet = true,
  footer,
  closeIcon,
}: KeyboardHelpProps) {
  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    }
    // Capture phase so this Esc wins over the surface's own handlers while
    // the overlay is up.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  if (!open) return null;

  const primary = groupRows(rows);
  const global = groupRows(globalRows);

  function renderGroup(g: HelpGroup, emphasize: boolean) {
    return (
      <section
        key={g.title}
        className={"kbc-kbdhelp__group" + (emphasize ? " kbc-kbdhelp__group--primary" : "")}
      >
        <h3>{g.title}</h3>
        <dl>
          {g.rows.map((r, i) => {
            const key = r.keys.join(" ");
            return (
              <div
                key={r.id ?? `${g.title}:${i}`}
                className={"kbc-kbdhelp__row" + (r.planned ? " is-planned" : "")}
                data-kbc-cmd={r.id}
              >
                <dt>{key ? <kbd>{key}</kbd> : <span className="kbc-kbdhelp__nokey">—</span>}</dt>
                <dd>
                  {r.verb}
                  {r.planned && <span className="kbc-kbdhelp__planned"> planned</span>}
                </dd>
              </div>
            );
          })}
        </dl>
      </section>
    );
  }

  const panel = (
    <>
      <header className="kbc-kbdhelp__head">
        <h2>
          {title}
          {scopeName !== undefined && (
            <>
              {" "}
              <span className="kbc-kbdhelp__scopename">{scopeName}</span>
            </>
          )}
        </h2>
        {profiles && (
          <label className="kbc-kbdhelp__preset">
            Keys
            <select
              value={profile}
              data-kbc-preset
              aria-label="key preset"
              onChange={(e) => onProfileChange?.(e.target.value)}
            >
              {profiles.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.title}
                </option>
              ))}
            </select>
          </label>
        )}
        <button type="button" className="kbc-kbdhelp__close" onClick={onClose} aria-label="close">
          {closeIcon ?? <Icon.X />}
        </button>
      </header>
      <div className="kbc-kbdhelp__cols">{primary.map((g) => renderGroup(g, global.length > 0))}</div>
      {global.length > 0 && (
        <>
          <div className="kbc-kbdhelp__break" role="separator">
            <span>Everywhere</span>
          </div>
          <div className="kbc-kbdhelp__cols kbc-kbdhelp__cols--rest">
            {global.map((g) => renderGroup(g, false))}
          </div>
        </>
      )}
      {footer !== undefined && <footer className="kbc-kbdhelp__foot">{footer}</footer>}
    </>
  );

  if (!asSheet) return <div className="kbc-kbdhelp kbc-kbdhelp--inline">{panel}</div>;

  return (
    <div className="kbc-kbdhelp__scrim" onClick={onClose} data-kbc-kbdhelp>
      <div
        className="kbc-kbdhelp"
        role="dialog"
        aria-modal="true"
        aria-label="keyboard shortcuts"
        onClick={(e) => e.stopPropagation()}
      >
        {panel}
      </div>
    </div>
  );
}

export default KeyboardHelp;
