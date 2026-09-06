// V70-A5 — day one (design D24), the whole of it.
//
//   "A repo opens to the reader with a working-tree tree and git status, the
//    last file or the Home card, the rail on All, no drawer, dark theme, vim
//    preset. The only teaching chrome above the fold is a Space hint and ?.
//    The first gd shows a one-time dismissible coach-mark for the Ramp; the
//    first mouse action with a key shows one learn-mode toast. Nothing else
//    is offered until asked."
//
// Three pieces, deliberately small:
//
//   * `SpaceHint`  — one chip in the chrome. Not a tour, not a modal.
//   * `GdCoachMark` — shown at most ONCE per browser profile, the first time
//     `gd` resolves, naming the commitment Ramp (hover · K · Enter ·
//     Shift-Enter · Ctrl-Enter · O) that `gd` is the entry point to.
//   * `useLearnMode` — the first mouse click on a control that HAS a key
//     earns ONE toast naming it. Once per command, ever.
//
// All three counters are BROWSER-LOCAL (`kbc:prefs`) and are never read by
// anything that ranks — §P2's "learn mode (local-first, never feeds
// ranking)". There is no server noun here, and there must not be one: what a
// person has already learned is not a fact about the corpus.

import { Fragment, useEffect, useState } from "react";
import { claimGdCoachMark, claimLearnToast, loadKeyPreset } from "../lib/prefs";
import { toast } from "../lib/toast";
import { displayKey } from "./dispatch";
import { KBC_COMMANDS, KBC_LEADER } from "./registry.gen";

/// The one always-visible teaching chip. `?` is the other half and is a real
/// binding, so it is named rather than rendered as a button — the sheet it
/// opens is one keystroke away and does not need a second door.
export function SpaceHint() {
  return (
    <span className="kbc-spacehint" data-kbc-spacehint>
      <kbd>{KBC_LEADER}</kbd> for everything · <kbd>?</kbd> for keys
    </span>
  );
}

/// The Ramp, in the order a reader commits to a result: look, peek, open,
/// open beside, open in the drawer, open standalone. Named here rather than
/// in the registry because it is a SEQUENCE of six existing commands, not a
/// seventh command.
const RAMP: Array<[string, string]> = [
  ["hover", "see it"],
  ["K", "peek without leaving"],
  ["Enter", "open it here"],
  ["Shift-Enter", "open it beside"],
  ["Ctrl-Enter", "keep it in the drawer"],
  ["O", "open it on its own"],
];

/// Rendered by the reader the first time `gd` resolves. Dismissible, and the
/// counter is claimed on MOUNT (not on dismiss), so a double render or a
/// refresh mid-read cannot show it twice.
export function GdCoachMark({ onClose }: { onClose: () => void }) {
  return (
    <aside className="kbc-coachmark" data-kbc-coachmark role="note" aria-label="how far to commit">
      <div className="kbc-coachmark__head">
        Jumped to the definition
        <button
          type="button"
          className="kbc-coachmark__close"
          data-kbc-coachmark-close
          onClick={onClose}
          aria-label="dismiss"
        >
          ✕
        </button>
      </div>
      <p>You do not have to jump. Every result answers to how much you want to commit:</p>
      <div className="kbc-coachmark__ramp">
        {RAMP.map(([key, what]) => (
          <Fragment key={key}>
            <kbd>{key}</kbd>
            <span>{what}</span>
          </Fragment>
        ))}
      </div>
    </aside>
  );
}

/// The reader's hook: returns whether to render the coach-mark, and the
/// closer. Call `noteGotoDefinition()` from the `gd` handler.
export function useGdCoachMark(): { show: boolean; noteGotoDefinition: () => void; close: () => void } {
  const [show, setShow] = useState(false);
  return {
    show,
    noteGotoDefinition: () => {
      if (claimGdCoachMark()) setShow(true);
    },
    close: () => setShow(false),
  };
}

/// Learn mode. Mounts one delegated click listener that looks for the nearest
/// `[data-cmd]` ancestor of whatever was clicked, maps it to the registry row
/// that names it as its `affordance_anchor`, and — if that row has a key in
/// the active preset, and this is the first time — says so once.
///
/// Delegated rather than per-button on purpose: every affordance already
/// carries `data-cmd` (the Desk's stripes, the drawer's pin/close, the rail's
/// pin), so learn mode costs those components exactly nothing and cannot
/// drift out of sync with them.
export function useLearnMode(): void {
  useEffect(() => {
    function onClick(e: MouseEvent) {
      const target = e.target as HTMLElement | null;
      if (!target || typeof target.closest !== "function") return;
      const el = target.closest<HTMLElement>("[data-cmd]");
      const anchor = el?.dataset.cmd;
      if (!anchor) return;
      const cmd = KBC_COMMANDS.find((c) => c.affordanceAnchor === anchor);
      if (!cmd) return;
      const key = displayKey(cmd, loadKeyPreset());
      if (!key) return; // no key in this preset — nothing to teach
      if (!claimLearnToast(cmd.id)) return; // already taught
      toast.ok(`${cmd.title} — ${key}`);
    }
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, []);
}
