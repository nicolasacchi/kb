import { Link } from "react-router-dom";
import type { ResolvedNavItem } from "../../lib/navItems";

// The desktop door strip (Spine switcher slot). Renders an already-resolved
// list, so hidden doors are absent and badges are normalised integers.
export default function DoorStrip({
  items,
  pathname,
  kb,
}: {
  items: ResolvedNavItem[];
  pathname: string;
  kb: string | null;
}) {
  return (
    <nav className="kb-doors" aria-label="Primary">
      {items.map((item) => {
        const active = item.isActive(pathname);
        return (
          <Link
            key={item.id}
            to={item.href({ kb })}
            className={"kb-door" + (active ? " is-active" : "")}
            aria-current={active ? "page" : undefined}
            title={item.label}
            data-testid={item.testid}
          >
            <item.Glyph />
            <span className="kb-door__label">{item.label}</span>
            {item.badge > 0 && (
              <span
                className="kb-door__badge"
                data-testid={`door-badge-${item.id}`}
                aria-label={`${item.badge} need attention`}
              >
                {item.badge}
              </span>
            )}
          </Link>
        );
      })}
    </nav>
  );
}
