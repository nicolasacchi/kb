import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { Link, useLocation } from "react-router";
import { Icon } from "./icons";
import { navItemMatches, navTestAttr, type NavItem } from "../lib/navItems";

// V4.U5 — desktop Explore popover. Outside-click + Escape mirror
// RepoPill's document listeners (RefPicker has no such handling — it
// only closes on item click). Roving arrow keys + Enter (native Link
// activation) + route-change close sit on top of that.

export default function NavMenu({
  repo,
  items,
}: {
  repo: string;
  items: readonly NavItem[];
}) {
  const { pathname } = useLocation();
  const active = items.find((it) => navItemMatches(pathname, it.url(repo))) ?? null;
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<Array<HTMLAnchorElement | null>>([]);
  const [idx, setIdx] = useState(0);

  useEffect(() => {
    setOpen(false);
  }, [pathname]);

  useEffect(() => {
    if (!open) return;
    function onDocClick(e: MouseEvent) {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const start = items.findIndex((it) => navItemMatches(pathname, it.url(repo)));
    setIdx(start >= 0 ? start : 0);
  }, [open, items, pathname, repo]);

  useEffect(() => {
    if (!open) return;
    itemRefs.current[idx]?.focus();
  }, [idx, open]);

  function onMenuKey(e: ReactKeyboardEvent<HTMLDivElement>) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setIdx((i) => (i + 1) % items.length);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setIdx((i) => (i - 1 + items.length) % items.length);
    } else if (e.key === "Home") {
      e.preventDefault();
      setIdx(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setIdx(items.length - 1);
    } else if (e.key === "Escape") {
      e.preventDefault();
      setOpen(false);
    }
  }

  const label = active ? `Explore: ${active.label}` : "Explore";

  return (
    <div className="kbc-navmenu-wrap" ref={wrapRef}>
      <button
        type="button"
        className={"kbc-topbar__chip" + (active ? " is-active" : "")}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-current={active ? "page" : undefined}
        data-kbc-nav-explore
        onClick={() => setOpen((o) => !o)}
      >
        <Icon.Grid />
        <span>{label}</span>
        <Icon.ChevDown />
      </button>
      {open && (
        <div
          className="kbc-navmenu"
          role="menu"
          aria-label="Explore"
          data-kbc-navmenu
          onKeyDown={onMenuKey}
        >
          {items.map((item, i) => {
            const on = active?.key === item.key;
            const ItemIcon = item.icon;
            return (
              <Link
                key={item.key}
                role="menuitem"
                to={item.url(repo)}
                className={"kbc-navmenu__item" + (on ? " is-active" : "")}
                aria-current={on ? "page" : undefined}
                tabIndex={i === idx ? 0 : -1}
                {...{ [navTestAttr(item.key)]: true }}
                data-kbc-navmenu-item={item.key}
                ref={(el) => {
                  itemRefs.current[i] = el;
                }}
                onClick={() => setOpen(false)}
              >
                <ItemIcon />
                {item.label}
              </Link>
            );
          })}
        </div>
      )}
    </div>
  );
}
