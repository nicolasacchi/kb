import type { ReactNode } from "react";
import "./spine.css";

export interface SpineProps {
  /** Left: the app switcher / brand. */
  switcher?: ReactNode;
  /** Scope crumb (the only slot that may shrink). */
  scope?: ReactNode;
  /** Centred search. */
  search?: ReactNode;
  /**
   * Optional crossing slot (OL-SIBL1): where the current thing crosses to its
   * sibling app (e.g. the citing doc). Rendered between `search` and `right`,
   * only when defined, and NOT hidden at <=860px (the chip collapses itself).
   */
  crossing?: ReactNode;
  /** Right: status and identity. View controls never live here (ViewBar). */
  right?: ReactNode;
  /** When given, a menu button is rendered (visible only at <=860px). */
  onMenu?: () => void;
  menuOpen?: boolean;
  menuLabel?: string;
  /** id of the element the menu button controls (aria-controls). */
  menuControls?: string;
  /** Glyph for the menu button; defaults to a text hamburger. */
  menuIcon?: ReactNode;
}

export function Spine({
  switcher,
  scope,
  search,
  crossing,
  right,
  onMenu,
  menuOpen,
  menuLabel = "Menu",
  menuControls,
  menuIcon,
}: SpineProps) {
  return (
    <header data-kb-spine role="banner" className="kb-spine">
      <div data-kb-spine-slot="switcher" className="kb-spine__slot kb-spine__switcher">
        {switcher}
      </div>
      <div data-kb-spine-slot="scope" className="kb-spine__slot kb-spine__scope">
        {scope}
      </div>
      <div data-kb-spine-slot="search" className="kb-spine__slot kb-spine__search">
        {search}
      </div>
      {crossing !== undefined ? (
        <div data-kb-spine-slot="crossing" className="kb-spine__slot kb-spine__crossing">
          {crossing}
        </div>
      ) : null}
      <div data-kb-spine-slot="right" className="kb-spine__slot kb-spine__right">
        {right}
      </div>
      {onMenu ? (
        <button
          type="button"
          data-kb-spine-menu
          className="kb-spine__menu"
          aria-label={menuLabel}
          aria-expanded={menuOpen ? true : false}
          aria-controls={menuControls}
          onClick={onMenu}
        >
          {menuIcon ?? <span aria-hidden="true">☰</span>}
        </button>
      ) : null}
    </header>
  );
}

export interface SpineCrumb {
  label: ReactNode;
  href?: string;
  current?: boolean;
}

export function SpineCrumbs({
  crumbs,
  renderLink,
}: {
  crumbs: SpineCrumb[];
  renderLink?: (href: string, children: ReactNode) => ReactNode;
}) {
  const last = crumbs.length - 1;
  return (
    <ol data-kb-spine-crumbs className="kb-spine-crumbs">
      {crumbs.map((c, i) => {
        // Middle crumbs (neither first nor last) collapse to "…" beyond 4 at <=1100px.
        const middle = i > 0 && i < last;
        let body: ReactNode;
        if (c.current || !c.href) {
          body = c.current ? <span aria-current="page">{c.label}</span> : <span>{c.label}</span>;
        } else {
          body = renderLink ? renderLink(c.href, c.label) : <a href={c.href}>{c.label}</a>;
        }
        return (
          <li
            key={i}
            className="kb-spine-crumbs__item"
            data-kb-crumb-middle={middle ? "" : undefined}
          >
            {body}
          </li>
        );
      })}
    </ol>
  );
}
