import type { ReactNode } from "react";
import { Link } from "react-router-dom";

// kb2 redesign — one on-brand empty-state pattern: an icon, a one-line
// title, an optional hint, an optional primary action (button or link),
// and an optional CLI hint. Replaces the ad-hoc per-view empty blocks so
// every "nothing here yet" reads the same. `variant` sizes it for a full
// view, a side rail, or an inline row.
export type EmptyStateProps = {
  icon?: ReactNode;
  title: string;
  hint?: ReactNode;
  action?: { label: string; onClick?: () => void; to?: string };
  cli?: string;
  variant?: "view" | "rail" | "inline";
};

export default function EmptyState({
  icon,
  title,
  hint,
  action,
  cli,
  variant = "view",
}: EmptyStateProps) {
  return (
    <div className={`kb-empty kb-empty--${variant}`} role="status">
      {icon && <div className="kb-empty__icon" aria-hidden>{icon}</div>}
      <p className="kb-empty__title">{title}</p>
      {hint && <p className="kb-empty__hint">{hint}</p>}
      {action &&
        (action.to ? (
          <Link className="kb-empty__action" to={action.to}>
            {action.label}
          </Link>
        ) : (
          <button
            type="button"
            className="kb-empty__action"
            onClick={action.onClick}
          >
            {action.label}
          </button>
        ))}
      {cli && <code className="kb-empty__cli">{cli}</code>}
    </div>
  );
}
