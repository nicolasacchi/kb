// V70-A4 — the two icon stripes: a collapsed region is a VISIBLE state.
//
// docs/research/kb-code-v7-continuum-2026-09.html §P1: "Regions collapse
// to a badged stripe and never vanish." The research report gives the
// reason (panel-layout-system.md §3.1): "Collapse is not 'gone' — it's
// the icon rail, so scent survives (IFT) and NN/g's recall tax is paid
// in icons, not memory."
//
// The stripes are OUTSIDE the resizable group and never resize — they
// are fixed-width columns flanking it, JetBrains' tool-window stripes.
// That is also why the landmark golden holds trivially: a stripe cannot
// move, because nothing about it is negotiable.

import type { ReactNode } from "react";

export interface StripeButton {
  id: string;
  label: string;
  icon: ReactNode;
  /// Rendered as a small count pill. `0`/absent renders nothing — a
  /// badge that says "0" is noise, and the rail's own always-visible
  /// cards already self-suppress on the same principle.
  badge?: number;
  active?: boolean;
  /// The command id this button is the mouse form of. Unit A5's `cmd/1`
  /// registry will bind keys to these same ids; exposing the attribute
  /// now means the e2e can address a button by INTENT rather than by
  /// icon or position.
  cmd?: string;
  onClick: () => void;
}

export interface StripeProps {
  side: "left" | "right";
  ariaLabel: string;
  /// Rendered at the top, reading downward.
  buttons: StripeButton[];
  /// Rendered at the bottom, pinned to the foot of the stripe (the
  /// drawer toggle lives here — a bottom dock's switch belongs at the
  /// bottom).
  footButtons?: StripeButton[];
}

/// Badges are counts of things a human might want to come back to, so a
/// four-digit count is never information — it is a pill that breaks the
/// stripe's width. Capped, with the true value in the title.
export function badgeText(n: number): string {
  return n > 99 ? "99+" : String(n);
}

function StripeBtn({ b }: { b: StripeButton }) {
  const title = b.badge && b.badge > 0 ? `${b.label} (${b.badge})` : b.label;
  return (
    <button
      type="button"
      className={"kbc-desk__stripe-btn" + (b.active ? " is-on" : "")}
      onClick={b.onClick}
      title={title}
      aria-label={title}
      aria-pressed={b.active ?? false}
      data-desk-stripe-btn={b.id}
      {...(b.cmd ? { "data-cmd": b.cmd } : {})}
    >
      {b.icon}
      {b.badge !== undefined && b.badge > 0 && (
        <span className="kbc-desk__stripe-badge" data-desk-stripe-badge={b.id}>
          {badgeText(b.badge)}
        </span>
      )}
    </button>
  );
}

export default function Stripe({ side, ariaLabel, buttons, footButtons = [] }: StripeProps) {
  return (
    <nav
      className={`kbc-desk__stripe kbc-desk__stripe--${side}`}
      data-region={side === "left" ? "stripe-left" : "stripe-right"}
      aria-label={ariaLabel}
    >
      <div className="kbc-desk__stripe-group">
        {buttons.map((b) => (
          <StripeBtn key={b.id} b={b} />
        ))}
      </div>
      {footButtons.length > 0 && (
        <div className="kbc-desk__stripe-group kbc-desk__stripe-group--foot">
          {footButtons.map((b) => (
            <StripeBtn key={b.id} b={b} />
          ))}
        </div>
      )}
    </nav>
  );
}
