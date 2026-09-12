import {
  lazy,
  Suspense,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { Link, useLocation } from "react-router";
import { Icon } from "./icons";
import { cycleTheme, loadTheme, loadThemeFamily } from "../lib/prefs";
import {
  NAV_ITEMS,
  navItemMatches,
  navTestAttr,
  type NavGroup,
} from "../lib/navItems";

// V4.U5 — mobile (≤860px) nav bottom sheet. Mounted only when
// `useIsMobile` is true (TopBar's gate, same as MobileDrawer). Slide +
// scrim reuse the house sheet recipe: --z-scrim / --z-drawer, translateY,
// --ease, reduced-motion override, env(safe-area-inset-bottom). Dismiss
// = X + scrim tap + Esc; route change closes (parent + local).

// V70-A7 — the same picker the TopBar opens, rendered as a bottom sheet.
const ThemePicker = lazy(() => import("./ThemePicker"));

const GROUPS: { group: NavGroup; label: string }[] = [
  { group: "review", label: "Review" },
  { group: "explore", label: "Explore" },
];

export default function NavSheet({
  repo,
  open,
  onClose,
}: {
  repo: string;
  open: boolean;
  onClose: () => void;
}) {
  const { pathname } = useLocation();
  const [render, setRender] = useState(open);
  const [shown, setShown] = useState(false);
  const [theme, setTheme] = useState(() => loadTheme());
  const [family, setFamily] = useState(() => loadThemeFamily());
  const [pickerOpen, setPickerOpen] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);

  // V4.U5 — mirror cycles made from the TopBar ThemeToggle while the
  // sheet is open (each theme control holds local state).
  useEffect(() => {
    const on = () => {
      setTheme(loadTheme());
      setFamily(loadThemeFamily());
    };
    window.addEventListener("kbc:theme.changed", on);
    return () => window.removeEventListener("kbc:theme.changed", on);
  }, []);

  useEffect(() => {
    if (open) {
      setRender(true);
      const raf = requestAnimationFrame(() => setShown(true));
      return () => cancelAnimationFrame(raf);
    }
    setShown(false);
    const t = setTimeout(() => setRender(false), 240);
    return () => clearTimeout(t);
  }, [open]);

  useEffect(() => {
    if (!render) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey);
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = prevOverflow;
    };
  }, [render, onClose]);

  useEffect(() => {
    if (shown) panelRef.current?.focus();
  }, [shown]);

  const prevPath = useRef(pathname);
  useEffect(() => {
    if (prevPath.current === pathname) return;
    prevPath.current = pathname;
    onClose();
  }, [pathname, onClose]);

  if (!render) return null;

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Tab") return;
    const focusables = panelRef.current?.querySelectorAll<HTMLElement>(
      'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
    if (!focusables || focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <div className="kbc-navsheet-root">
      <div
        className={"kbc-navsheet-scrim" + (shown ? " is-open" : "")}
        onClick={onClose}
        aria-hidden="true"
        data-kbc-navsheet-scrim
      />
      <div
        ref={panelRef}
        id="kbc-navsheet"
        className={"kbc-navsheet" + (shown ? " is-open" : "")}
        role="dialog"
        aria-modal="true"
        aria-label="Navigate"
        tabIndex={-1}
        onKeyDown={onKeyDown}
        data-kbc-navsheet
      >
        <div className="kbc-navsheet__head">
          <span className="kbc-navsheet__title">Navigate</span>
          <button
            type="button"
            className="kbc-navsheet__close"
            onClick={onClose}
            aria-label="close navigation"
            data-kbc-nav-close
          >
            <Icon.X />
          </button>
        </div>
        <div className="kbc-navsheet__body">
          {GROUPS.map(({ group, label }) => (
            <section key={group} className="kbc-navsheet__sec">
              <h2 className="kbc-navsheet__h">{label}</h2>
              {NAV_ITEMS.filter((item) => item.group === group).map((item) => {
                const on = navItemMatches(pathname, item.url(repo));
                const ItemIcon = item.icon;
                return (
                  <Link
                    key={item.key}
                    to={item.url(repo)}
                    className={"kbc-navsheet__row" + (on ? " is-active" : "")}
                    aria-current={on ? "page" : undefined}
                    {...{ [navTestAttr(item.key)]: true }}
                    data-kbc-nav-row={item.key}
                    onClick={onClose}
                  >
                    <ItemIcon />
                    {item.label}
                  </Link>
                );
              })}
            </section>
          ))}
          {/* V70-A7 — the APPEARANCE row is unchanged (it cycles the same
              light/dark/system axis it always did, which is the one control
              that has to work with a thumb and no popover); the family
              catalogue gets its own row, opening the same ThemePicker the
              TopBar uses, rendered as a bottom sheet. */}
          <button
            type="button"
            className="kbc-navsheet__theme"
            data-kbc-nav-theme
            onClick={() => {
              const next = cycleTheme();
              setTheme(next);
            }}
          >
            Theme: {theme} — tap to cycle
          </button>
          <button
            type="button"
            className="kbc-navsheet__theme"
            data-kbc-nav-theme-picker
            aria-haspopup="dialog"
            aria-expanded={pickerOpen}
            onClick={() => setPickerOpen(true)}
          >
            Colour scheme: {family} — tap to choose
          </button>
          {pickerOpen && (
            <Suspense fallback={null}>
              <ThemePicker asSheet onClose={() => setPickerOpen(false)} />
            </Suspense>
          )}
        </div>
      </div>
    </div>
  );
}
