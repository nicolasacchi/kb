import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { Link, useLocation } from "react-router";
import { Sheet, type Detent } from "@kb/ui/Sheet";
import { settingsUrl } from "../lib/codeUrl";
import CurrentReviewChip from "./CurrentReviewChip";
import { Icon } from "./iconSet";
import { cycleTheme, loadTheme, loadThemeFamily } from "../lib/prefs";
import { NAV_ITEMS, inboxUrl, matchActiveNavItem, navTestAttr } from "../lib/navItems";

// OL-C1 — the mobile (≤860px) navigation, rebuilt on the shared `@kb/ui/Sheet`
// (scrim, drag handle, Esc, focus trap and body-scroll lock all come from it).
// Mounted only when `useIsMobile` is true (TopBar's gate). It lists the four
// doors (Code · Search · Review · Collections), Inbox, the current-review chip
// and the two theme rows. Route change closes it (parent + local).

// V70-A7 — the same picker the TopBar opens, rendered as a bottom sheet.
const ThemePicker = lazy(() => import("./ThemePicker"));

export default function NavSheet({
  repo,
  open,
  onClose,
}: {
  /// The explicit repo, or `null` on a repo-less route (Home, Inbox): the
  /// doors need a repo, Inbox and the theme rows do not.
  repo: string | null;
  open: boolean;
  onClose: () => void;
}) {
  const { pathname } = useLocation();
  const [detent, setDetent] = useState<Detent>("peek");
  const [theme, setTheme] = useState(() => loadTheme());
  const [family, setFamily] = useState(() => loadThemeFamily());
  const [pickerOpen, setPickerOpen] = useState(false);

  // V4.U5 — mirror cycles made elsewhere while the sheet is open (each theme
  // control holds local state; `kbc:theme.changed` is the one channel).
  useEffect(() => {
    const on = () => {
      setTheme(loadTheme());
      setFamily(loadThemeFamily());
    };
    window.addEventListener("kbc:theme.changed", on);
    return () => window.removeEventListener("kbc:theme.changed", on);
  }, []);

  // Every open starts at the peek detent, not wherever the last one ended.
  useEffect(() => {
    if (open) setDetent("peek");
    else setPickerOpen(false);
  }, [open]);

  const prevPath = useRef(pathname);
  useEffect(() => {
    if (prevPath.current === pathname) return;
    prevPath.current = pathname;
    onClose();
  }, [pathname, onClose]);

  const active = repo ? matchActiveNavItem(pathname, repo) : null;
  const inboxOn = pathname === "/~inbox";
  const settingsOn = pathname === settingsUrl();

  return (
    <Sheet
      open={open}
      detent={detent}
      onDetentChange={setDetent}
      onClose={onClose}
      title="Navigate"
      id="kbc-navsheet"
      peek="70vh"
      full="92vh"
    >
      <div data-kbc-navsheet>
        <section>
          {repo && (
            <>
              {/* V80-M3 — the current-review chip's mobile home (≤860px moves
                  it out of the TopBar into this sheet, same rule). */}
              <CurrentReviewChip variant="sheet" onNavigate={onClose} />
              {NAV_ITEMS.map((item) => {
                const on = active?.key === item.key;
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
                    <ItemIcon aria-hidden />
                    {item.label}
                  </Link>
                );
              })}
            </>
          )}
          <Link
            to={inboxUrl("")}
            className={"kbc-navsheet__row" + (inboxOn ? " is-active" : "")}
            aria-current={inboxOn ? "page" : undefined}
            {...{ [navTestAttr("inbox")]: true }}
            data-kbc-nav-row="inbox"
            onClick={onClose}
          >
            <Icon.List aria-hidden />
            Inbox
          </Link>
          <Link
            to={settingsUrl()}
            className={"kbc-navsheet__row" + (settingsOn ? " is-active" : "")}
            aria-current={settingsOn ? "page" : undefined}
            data-kbc-nav-row="settings"
            onClick={onClose}
          >
            <Icon.Settings aria-hidden />
            Settings
          </Link>
        </section>
        {/* V70-A7 — the APPEARANCE row cycles the light/dark/system axis (the
            one control that has to work with a thumb and no popover); the
            family catalogue gets its own row, opening the same ThemePicker
            the TopBar uses, rendered as a bottom sheet. */}
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
    </Sheet>
  );
}
