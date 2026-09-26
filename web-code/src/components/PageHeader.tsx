import type { ReactNode } from "react";
import "../styles/page.css";

// V80-R0 — the ONE shared page-header shape (item 5 of the ramp/density
// unit's brief): title + optional lede/kicker/actions, so a future page
// doesn't hand-roll another `__head`/`__title` block the way Reviews/
// Todos/Sets/Inbox/Branches each used to (their own, near-identical,
// per-page CSS — see `styles/page.css`'s own header doc). `title` and
// `lede` take `ReactNode` (not just `string`) because at least one adopter
// (Inbox) nests a live badge INSIDE the `<h1>`, not beside it.
export interface PageHeaderProps {
  /// Rendered inside the `<h1>`. A caller that needs a badge/icon next to
  /// the text (e.g. Inbox's unread count) puts it here, not in `actions`.
  title: ReactNode;
  /// V80-R5 — an extra class on the `<h1>` ITSELF (not the outer
  /// `<header>` — that's `className` below). Only for a caller with a
  /// pre-existing test-selected class on its title (e.g. Commit's
  /// `.kbc-commit__subject`, asserted by `time.spec.ts`) that must keep
  /// resolving after adopting this component; a fresh page never needs it.
  titleClassName?: string;
  /// A one-line (or short) explanation under the title, `--fs-sm`/muted,
  /// bounded to the reading contract's `--measure` so a long lede still
  /// wraps at a sane column count.
  lede?: ReactNode;
  /// Right-aligned slot beside the title (buttons/links) — same row on
  /// desktop, wraps below on narrow viewports (`flex-wrap`).
  actions?: ReactNode;
  /// A small caption ABOVE the title (e.g. a breadcrumb-ish label). Rare;
  /// none of this unit's five adopters use it yet.
  kicker?: ReactNode;
  /// Extra class(es) for the outer `<header>`, so a caller can still hang
  /// its own `data-kbc-*`/layout hook off the header element itself.
  className?: string;
}

export default function PageHeader({
  title,
  titleClassName,
  lede,
  actions,
  kicker,
  className,
}: PageHeaderProps) {
  return (
    <header className={className ? `kbc-page-head ${className}` : "kbc-page-head"}>
      <div className="kbc-page-head__row">
        <div className="kbc-page-head__titlewrap">
          {kicker !== undefined && kicker !== null && (
            <div className="kbc-page-head__kicker">{kicker}</div>
          )}
          <h1 className={titleClassName ? `kbc-page-head__title ${titleClassName}` : "kbc-page-head__title"}>
            {title}
          </h1>
        </div>
        {actions !== undefined && actions !== null && (
          <div className="kbc-page-head__actions">{actions}</div>
        )}
      </div>
      {lede !== undefined && lede !== null && <p className="kbc-page-head__lede">{lede}</p>}
    </header>
  );
}
