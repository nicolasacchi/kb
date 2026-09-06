import { useEffect, useMemo, useState } from "react";
import { Icon } from "./icons";
import { useCommands } from "../commands/CommandRoot";
import { displayKey } from "../commands/dispatch";
import {
  KBC_COMMANDS,
  KBC_PRESETS,
  KBC_SCOPES,
  type KbcCommand,
  type KbcPreset,
  type KbcScope,
} from "../commands/registry.gen";
import { loadKeyPreset, saveKeyPreset } from "../lib/prefs";

// The `?` keyboard sheet.
//
// V70-A5 — this file no longer CONTAINS a keymap. It used to hold a
// hand-maintained literal `GROUPS` array: 5 groups, 47 rows, each a
// `[keys, what]` string pair with nothing test-pinning it against a real
// binding. kb's own SPA had already written down why that is fatal
// (`web/src/lib/keymap.ts`): "a binding that isn't in REGISTRY doesn't exist.
// The `?` cheat sheet is GENERATED from this table, never hand-copied —
// that's what killed the two lying rows." web-code's sheet WAS that
// hand-copied second home, and it had already drifted (it promised
// `Ctrl-w v` with no caveat, and on 25 routes `?` did nothing at all).
//
// Now every row is rendered from `kbc-cmd/1`, for the CURRENT SCOPE, in the
// registry's own reading order, with the key for the CURRENT PRESET. Global
// rows are collapsed at the bottom rather than hidden: knowing that ⌘K and
// the leader exist everywhere is a large part of the point of the sheet.

/// Kept as an exported alias so pre-A5 callers still compile: a "context" is
/// now simply a scope. `"review-diff"` was the old spelling of `"diff"`.
export type KeyboardHelpContext = KbcScope | "all" | "review-diff";

export interface HelpGroup {
  title: string;
  rows: KbcCommand[];
}

/// Bucket commands by `group`, preserving registry order. Pure — the sheet
/// iterates exactly this, and so does `kb-code commands cheatsheet`, which is
/// why the printed sheet and the on-screen one cannot disagree.
export function groupCommands(commands: readonly KbcCommand[]): HelpGroup[] {
  const order: string[] = [];
  const byGroup = new Map<string, KbcCommand[]>();
  for (const c of commands) {
    if (!byGroup.has(c.group)) {
      byGroup.set(c.group, []);
      order.push(c.group);
    }
    byGroup.get(c.group)!.push(c);
  }
  return order.map((title) => ({ title, rows: byGroup.get(title)! }));
}

/// Normalise a caller's context to a real scope id.
export function normaliseContext(context: KeyboardHelpContext): KbcScope | "all" {
  if (context === "review-diff") return "diff";
  return context;
}

/// Split the registry into "this surface" and "everywhere else" for a scope.
/// `"all"` keeps everything primary (the pre-A5 flat listing, and what an
/// unknown/stale context degrades to — never a blank sheet).
///
/// A row with NO key in the active preset is still listed, with an empty key
/// cell: "reachable from the palette, not from the keyboard here" is a real
/// answer, and silently dropping it would recreate the lying-sheet problem
/// from the other direction.
export function scopeSections(context: KeyboardHelpContext): {
  primary: HelpGroup[];
  global: HelpGroup[];
} {
  const scope = normaliseContext(context);
  if (scope === "all") {
    return { primary: groupCommands(KBC_COMMANDS), global: [] };
  }
  const local = KBC_COMMANDS.filter((c) => c.scope === scope);
  const global = KBC_COMMANDS.filter((c) => c.scope === "global");
  if (local.length === 0) {
    // A scope with no rows of its own is still entitled to the global sheet
    // rather than an empty dialog.
    return { primary: groupCommands(global), global: [] };
  }
  return { primary: groupCommands(local), global: groupCommands(global) };
}

export default function KeyboardHelp({
  open,
  onClose,
  context = "all",
}: {
  open: boolean;
  onClose: () => void;
  /// Which surface opened the sheet. Defaults to `"all"`, which then follows
  /// whatever scope the mounted surface published.
  context?: KeyboardHelpContext;
}) {
  const bus = useCommands();
  const [preset, setPreset] = useState<KbcPreset>(() => loadKeyPreset());

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    }
    // Capture phase so this Esc wins over the surface's own handlers while
    // the overlay is up — `dismiss.help` is dismiss_order 2, the second
    // innermost thing that can be on screen.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  // An explicit `context` wins; otherwise follow whatever surface last
  // published its scope, so `?` on a route that never passed a context is
  // still scoped rather than dumping all 181 rows.
  const scope: KeyboardHelpContext = context === "all" ? bus.scope : context;
  const { primary, global } = useMemo(() => scopeSections(scope), [scope]);
  const scopeId = normaliseContext(scope);
  const scopeTitle =
    scopeId === "all" ? "Everywhere" : (KBC_SCOPES.find((s) => s.id === scopeId)?.title ?? scopeId);

  if (!open) return null;

  function renderGroup(g: HelpGroup, emphasize: boolean) {
    return (
      <section
        key={g.title}
        className={"kbc-kbdhelp__group" + (emphasize ? " kbc-kbdhelp__group--primary" : "")}
      >
        <h3>{g.title}</h3>
        <dl>
          {g.rows.map((c) => {
            const key = displayKey(c, preset);
            return (
              <div
                key={c.id}
                className={"kbc-kbdhelp__row" + (c.lifecycle === "shipped" ? "" : " is-planned")}
                data-kbc-cmd={c.id}
              >
                <dt>{key ? <kbd>{key}</kbd> : <span className="kbc-kbdhelp__nokey">—</span>}</dt>
                <dd>
                  {c.title}
                  {c.lifecycle !== "shipped" && (
                    <span className="kbc-kbdhelp__planned"> planned</span>
                  )}
                </dd>
              </div>
            );
          })}
        </dl>
      </section>
    );
  }

  return (
    <div className="kbc-kbdhelp__scrim" onClick={onClose} data-kbc-kbdhelp>
      <div
        className="kbc-kbdhelp"
        role="dialog"
        aria-modal="true"
        aria-label="keyboard shortcuts"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="kbc-kbdhelp__head">
          <h2>
            Keyboard <span className="kbc-kbdhelp__scopename">{scopeTitle}</span>
          </h2>
          <label className="kbc-kbdhelp__preset">
            Keys
            <select
              value={preset}
              data-kbc-preset
              aria-label="key preset"
              onChange={(e) => {
                const next = e.target.value as KbcPreset;
                setPreset(next);
                saveKeyPreset(next);
                bus.setPreset(next);
              }}
            >
              {KBC_PRESETS.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.title}
                </option>
              ))}
            </select>
          </label>
          <button type="button" className="kbc-kbdhelp__close" onClick={onClose} aria-label="close">
            <Icon.X />
          </button>
        </header>
        <div className="kbc-kbdhelp__cols">
          {primary.map((g) => renderGroup(g, global.length > 0))}
        </div>
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
        <footer className="kbc-kbdhelp__foot">
          Printable cheatsheet: <code>kb-code commands cheatsheet --scope {scopeId} --md</code>
        </footer>
      </div>
    </div>
  );
}
