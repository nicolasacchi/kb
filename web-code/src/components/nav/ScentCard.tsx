// V70-A6 — the scent card (§P7's hover rung, the Ramp's zero rung).
//
// Information Foraging Theory is the argument for this component, and it is a
// measured one: Piorkowski et al. (FSE 2016) found over 50% of navigation
// choices produce LESS value than the developer predicted and nearly 40% cost
// MORE. The design consequence the research draws is "make cost visible
// before the hop", and this card is that — signature, kind, trust, size, how
// often this trail has already been here, and who last touched it.
//
// THE HONESTY RULE, and it is the whole reason this is a component rather
// than a tooltip string: **a scent card never renders silently empty.** Every
// field it does not have says so by name ("no signature", "size unknown"),
// because a blank card and a card about a symbol with no signature look
// identical, and the reader would learn to distrust both. Sourcegraph's own
// issue tracker records the failure mode this avoids — a hover that silently
// vanishes when nothing comes back, leaving "guess and check".
//
// TIMING lives in the caller (`useScentIntent`) and not here: this component
// is a pure render of whatever the row already knew, so it costs nothing and
// fetches nothing.

import TrustBadge from "../TrustBadge";
import type { RampTarget } from "../../nav/ramp";
import { VIA_LABEL } from "../../lib/trail";

export interface ScentCardProps {
  target: RampTarget;
  /// How many times the current trail has already landed on this file.
  visits: number;
  /// Rendered inline on the keyboard-focused row rather than floating at the
  /// pointer — same card, two placements (§P7: "keyboard users get the same
  /// card inline on the focused row").
  inline?: boolean;
  /// Absolute placement for the hover form.
  style?: React.CSSProperties;
}

function Field({ label, value, absent }: { label: string; value?: string | null; absent: string }) {
  const has = value !== undefined && value !== null && value !== "";
  return (
    <div className={"kbc-scent__field" + (has ? "" : " kbc-scent__field--absent")}>
      <span className="kbc-scent__label">{label}</span>
      <span className="kbc-scent__value">{has ? value : absent}</span>
    </div>
  );
}

export default function ScentCard({ target, visits, inline, style }: ScentCardProps) {
  const where = target.line ? `${target.path}:${target.line}` : target.path;
  return (
    <div
      className={"kbc-scent" + (inline ? " kbc-scent--inline" : "")}
      style={style}
      role="note"
      aria-label={`what is at ${where}`}
      data-kbc-scent
      data-kbc-scent-inline={inline ? "1" : undefined}
    >
      <div className="kbc-scent__head">
        <span className="kbc-scent__where">{where}</span>
        {target.trust && <TrustBadge cls={target.trust} />}
      </div>
      <code className="kbc-scent__sig" data-kbc-scent-sig>
        {target.signature ?? target.subject ?? "no signature — this row names a location, not a symbol"}
      </code>
      <div className="kbc-scent__grid">
        <Field
          label="kind"
          value={[target.kind, target.container ? `in ${target.container}` : ""].filter(Boolean).join(" ")}
          absent="not a declared symbol"
        />
        <Field label="size" value={target.sizeLabel} absent="size unknown" />
        <Field label="edge" value={VIA_LABEL[target.via]} absent="—" />
        <Field
          label="in this trail"
          value={visits > 0 ? `visited ${visits}×` : "not yet visited"}
          absent="no trail yet"
        />
        <Field label="last touched" value={target.lastTouched} absent="no session or commit on this row" />
      </div>
      <div className="kbc-scent__keys" aria-hidden="true">
        <kbd>K</kbd> peek <kbd>↵</kbd> here <kbd>⇧↵</kbd> other pane <kbd>o</kbd> tab <kbd>O</kbd> window
      </div>
    </div>
  );
}
