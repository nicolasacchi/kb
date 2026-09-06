import type { ReactNode } from "react";

// SH.D.5 — one loading idiom, mirroring EmptyState.tsx's shape (icon +
// title + optional hint) so "nothing here yet" and "fetching, nothing to
// show YET" read as siblings instead of two different vocabularies —
// replaces the ad-hoc `<div className="...">loading…</div>` rows that grew
// independently across gallery.tsx/lists.tsx/notes.tsx (design-audit P3).
//
// Static — no spinner. Matches the calm-computing posture other
// in-progress surfaces in this codebase already follow (chrome.css's
// SessionProducedSection comment: "static, honest, never a spinner");
// `aria-busy` carries the "in progress" signal to assistive tech instead of
// an animation. Reuses the existing `.kb-empty` family of classes (app.css)
// so no new CSS is needed for the base shape — `variant` picks the same
// view/rail/inline sizing EmptyState already offers.
export type LoadingProps = {
  icon?: ReactNode;
  label?: string;
  hint?: ReactNode;
  variant?: "view" | "rail" | "inline";
};

export default function Loading({
  icon,
  label = "loading…",
  hint,
  variant = "view",
}: LoadingProps) {
  return (
    <div
      className={`kb-empty kb-empty--${variant} kb-loading`}
      role="status"
      aria-busy="true"
    >
      {icon && (
        <div className="kb-empty__icon" aria-hidden>
          {icon}
        </div>
      )}
      <p className="kb-empty__title">{label}</p>
      {hint && <p className="kb-empty__hint">{hint}</p>}
    </div>
  );
}
