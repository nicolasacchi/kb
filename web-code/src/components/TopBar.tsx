import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { Link, useLocation } from "react-router";
import RepoPill from "./RepoPill";
import CurrentReviewChip from "./CurrentReviewChip";
import NavSheet from "./NavSheet";
import { Icon } from "./iconSet";
import TrailIndicator from "./trail/TrailIndicator";
import { SpaceHint } from "../commands/learn";
import { useExplicitRepo } from "../hooks/useActiveRepo";
import { useIsMobile } from "../hooks/useIsMobile";
import { loadTheme, loadThemeFamily } from "../lib/prefs";
import { Spine } from "@kb/ui/Spine";
import {
  NAV_ITEMS,
  inboxUrl,
  matchActiveNavItem,
  navTestAttr,
} from "../lib/navItems";
import RefChip from "./RefChip";
import { ScopeSlot } from "./ScopeSlot";

// F1 — the ONE chrome bar shared by every route, mounted once at the app
// root (app.tsx), mirroring kb's own `Header` (also mounted once above
// `<Routes>` in `web/src/app.tsx`). OL-C1: the bar is `@kb/ui/Spine` —
// RepoPill + RefChip + four labelled doors (Code · Search · Review ·
// Collections) on the left, the omnibox button in the middle, status on the
// right; mobile (≤860px) puts every destination in a `@kb/ui/Sheet`.
// V70-A7 — the theme control is no longer a three-way cycle. `data-kbc-theme
// -toggle` keeps its name (and its place in the bar) but now opens the
// ThemePicker popover: a family catalogue is not a cycle, and reaching
// `light` from `system` used to cost two blind clicks. Two recon defects are
// fixed on the way: the glyph is now STATE-BEARING (Sun/Moon/Contrast, R11)
// and the button carries `aria-pressed` + `aria-expanded` instead of
// advertising its state only through a `title` attribute.

export default function TopBar() {
  const explicitRepo = useExplicitRepo();
  const isMobile = useIsMobile();
  const { pathname } = useLocation();
  const [sheetOpen, setSheetOpen] = useState(false);

  const active = explicitRepo ? matchActiveNavItem(pathname, explicitRepo) : null;
  const inboxOn = pathname === "/~inbox";

  useEffect(() => {
    setSheetOpen(false);
  }, [pathname]);

  // OL-C1 — `<Spine>` is the one 44px bar. The `data-region="topbar"` wrapper
  // is what regions.spec / visual.spec settle on; the sheet is mounted OUTSIDE
  // it so its fixed scrim is not trapped in the bar's stacking context.
  return (
    <>
      <div className="kbc-topbar" data-region="topbar">
        <Spine
          scope={<ScopeSlot />}
          switcher={
            <>
              <RepoPill />
              <RefChip />
              {explicitRepo && !isMobile && (
                <nav className="kbc-topbar__doors" aria-label="Sections">
                  {NAV_ITEMS.map((item) => {
                    const on = active?.key === item.key;
                    const ItemIcon = item.icon;
                    return (
                      <Link
                        key={item.key}
                        to={item.url(explicitRepo)}
                        className={
                          "kbc-topbar__chip kbc-topbar__door kbc-topbar__" +
                          item.key +
                          (on ? " is-active" : "")
                        }
                        aria-current={on ? "page" : undefined}
                        {...{ [navTestAttr(item.key)]: true }}
                      >
                        <ItemIcon aria-hidden />
                        {item.label}
                      </Link>
                    );
                  })}
                </nav>
              )}
            </>
          }
          search={
            <button
              type="button"
              className="kbc-search-mini kbc-topbar__search"
              onClick={() => window.dispatchEvent(new CustomEvent("kbc:omnibox.open"))}
              aria-label="open search (cmd-k)"
              title="search (⌘K)"
            >
              {/* F5 — mobile (≤860px) hides the label + `⌘K` hint (a shortcut
                  that's meaningless without a physical keyboard) and keeps just
                  this icon, so the button stays a recognizable, ≥40px tap target
                  instead of collapsing to a bare, unlabeled kbd. */}
              <Icon.Search />
              <span className="kbc-search-mini__label">Search…</span>
              <kbd>⌘K</kbd>
            </button>
          }
          right={
            !isMobile && (
              <>
                {/* V80-M3 — "which review am I working," hidden entirely when
                    unset. ≤860px it moves into the NavSheet instead. */}
                {explicitRepo && <CurrentReviewChip />}
                {/* D24 — "the only teaching chrome above the fold is a Space
                    hint and `?`". Hidden on mobile, where there is no
                    keyboard to teach (recon R9). */}
                <SpaceHint />
                {/* V74-L3b — the kbc-trail/1 opt-in's visible indicator; it
                    renders NOTHING on a daemon where `[trails] enabled` is
                    false (see the component's own rule 1). */}
                <TrailIndicator />
                <Link
                  to={inboxUrl("")}
                  className="kbc-iconbtn kbc-topbar__inbox"
                  aria-label="Inbox"
                  title="Inbox"
                  aria-current={inboxOn ? "page" : undefined}
                  {...{ [navTestAttr("inbox")]: true }}
                >
                  <Icon.List />
                </Link>
                <ThemeControl />
              </>
            )
          }
          onMenu={isMobile ? () => setSheetOpen((o) => !o) : undefined}
          menuOpen={sheetOpen}
          menuLabel="Open navigation"
          menuControls="kbc-navsheet"
          menuIcon={<Icon.List aria-hidden />}
        />
      </div>
      {isMobile && (
        <NavSheet repo={explicitRepo} open={sheetOpen} onClose={() => setSheetOpen(false)} />
      )}
    </>
  );
}

// Code-split: the picker pulls in the whole registry's metadata plus its own
// stylesheet, and the overwhelming majority of sessions never open it.
const ThemePicker = lazy(() => import("./ThemePicker"));

function ThemeControl() {
  const [theme, setTheme] = useState(() => loadTheme());
  const [family, setFamily] = useState(() => loadThemeFamily());
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);

  // V4.U5 — the mobile NavSheet row cycles the same appearance axis, so a
  // change from there must refresh this button (each control holds local
  // state; `kbc:theme.changed` is the one notification channel).
  useEffect(() => {
    const on = () => {
      setTheme(loadTheme());
      setFamily(loadThemeFamily());
    };
    window.addEventListener("kbc:theme.changed", on);
    return () => window.removeEventListener("kbc:theme.changed", on);
  }, []);

  // An outside click dismisses AND reverts (ThemePicker's own unmount effect
  // puts the persisted theme back), so a staged preview can never leak.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  // R11 — the glyph now carries the state instead of only the title string.
  const Glyph = theme === "light" ? Icon.Sun : theme === "dark" ? Icon.Moon : Icon.Contrast;

  return (
    <div className="kbc-topbar__theme" ref={wrapRef}>
      <button
        type="button"
        className="kbc-iconbtn"
        data-kbc-theme-toggle
        onClick={() => setOpen((v) => !v)}
        title={`theme: ${family} · ${theme} — click to choose`}
        aria-label="choose theme"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-pressed={open}
      >
        <Glyph />
      </button>
      {open && (
        <Suspense fallback={null}>
          <ThemePicker onClose={() => setOpen(false)} />
        </Suspense>
      )}
    </div>
  );
}
