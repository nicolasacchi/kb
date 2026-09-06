import type { ReactNode } from "react";
import { Link } from "react-router-dom";

// F6 — kb-code's own on-brand empty-state pattern, mirroring kb's own
// `web/src/components/EmptyState.tsx` shape (icon + title + hint + optional
// action) under a `kbc-` prefix — kb-code has no cross-app import of that
// component (see `components/icons.tsx`'s own header doc on the crate
// boundary), so this is a small, deliberate re-implementation, not a port.
// Replaces the ad-hoc per-view "nothing here" `<div>`s that used to be
// scattered across Home/Search/Branches/Commit/Reader/PeekPanel/
// HistoryPanel — `.kbc-reader__hint` (and its sibling `.kbc-*__hint`
// classes) stays reserved for pure LOADING/ERROR lines; this is for a state
// worth explaining (nothing configured, nothing found, nothing committed
// yet), and should carry a useful next action whenever one exists.
export interface EmptyStateAction {
  label: string;
  onClick?: () => void;
  to?: string;
}

export interface EmptyStateProps {
  icon?: ReactNode;
  title: string;
  hint?: ReactNode;
  action?: EmptyStateAction;
  /// Sizes the block for its context: a full route view, an inspector-rail
  /// column, or a compact inline note inside a smaller panel (the peek
  /// panel's own floating card) — mirrors kb's own three variants.
  variant?: "view" | "rail" | "inline";
}

export default function EmptyState({ icon, title, hint, action, variant = "view" }: EmptyStateProps) {
  return (
    <div className={`kbc-empty kbc-empty--${variant}`} role="status" data-kbc-empty>
      {icon && (
        <div className="kbc-empty__icon" aria-hidden="true">
          {icon}
        </div>
      )}
      <p className="kbc-empty__title">{title}</p>
      {hint && <p className="kbc-empty__hint">{hint}</p>}
      {action &&
        (action.to ? (
          <Link className="kbc-empty__action" to={action.to}>
            {action.label}
          </Link>
        ) : (
          <button type="button" className="kbc-empty__action" onClick={action.onClick}>
            {action.label}
          </button>
        ))}
    </div>
  );
}
