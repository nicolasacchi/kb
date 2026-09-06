import { useEffect, useMemo, useRef, useState } from "react";
import { Link, matchPath, useLocation, useNavigate } from "react-router-dom";
import { type KbSummary } from "../../api/client";
import { useAnchors } from "../../hooks/useAnchors";
import { useIdentity } from "../../hooks/useArtifactHost";
import { useInbox } from "../../hooks/useInbox";
import { useKbs } from "../../hooks/useKbs";
import { useActiveKb, useExplicitKb } from "../../hooks/useActiveKb";
import { useSavedQueries } from "../../hooks/useSavedQueries";
import { useUrl } from "../../hooks/useUrl";
import { cycleTheme, loadPrefs } from "../../api/prefs";
import { Icon } from "../icons";
import { NAV_ITEMS, withKb } from "../../lib/navItems";
import WaitingChip from "./WaitingChip";
import DeskPill from "./DeskPill";

// kb2 redesign — Band-1 command bar.
//
// Slot order matches the redesign canvas: workspace pill (with kb
// dropdown), anchor pill, view toggle (8-icon segmented strip), then the
// flex-grow search launcher that fills the gap and pushes the right
// cluster (saved-queries, theme, settings) to the edge.
//
// Replaces the v0.6 TopBar. K3 will:
//   - light up the anchor pill from /api/anchors (cross-kb count)
//   - retire the standalone StaleAnchorsBadge once stale anchors
//     fold into the /anchors view's "stale" filter tab.
// Until then the pill mirrors the stale-anchors count (visible only
// when > 0) so the warm-amber affordance reads in the static screenshot
// without inventing a feature.
export default function Header({
  onOpenCmdk,
  onToggleDrawer,
}: {
  onOpenCmdk: () => void;
  onToggleDrawer: () => void;
}) {
  const loc = useLocation();
  const { params, set, navigate } = useUrl();

  const { data: kbs = [] } = useKbs();
  // Resolved kb drives the workspace pill (never blank once kbs load); the
  // explicit kb (no first-kb fallback) is what the nav links carry, so an
  // unscoped view stays kb-less.
  const activeKb = useActiveKb() || "";
  const explicitKb = useExplicitKb();
  // The reader (/a/:kb/*) and list detail (/lists/:kb/:id) own their kb in
  // the path, so switching the workspace there must LEAVE to the gallery —
  // the open item belongs to the old kb. Everywhere else we re-scope the
  // current section in place.
  const onPathKbRoute =
    !!matchPath({ path: "/a/:kb/*" }, loc.pathname) ||
    !!matchPath({ path: "/lists/:kb/:id" }, loc.pathname);

  return (
    <header className="kb-head" role="banner">
      <button
        type="button"
        className="kb-burger"
        onClick={onToggleDrawer}
        aria-label="open menu"
        title="menu"
      >
        <Icon.List aria-hidden="true" />
      </button>
      <KbSelector
        kb={activeKb || "—"}
        kbs={kbs}
        scoped={explicitKb !== null}
        onChange={(name) => {
          if (onPathKbRoute) navigate(`/?kb=${encodeURIComponent(name)}`);
          else set("kb", name);
        }}
      />

      {/* v0.34 W — current attribution identity (edge/token/loopback).
          No login/logout affordance — the reverse proxy owns auth. */}
      <IdentityChip />

      <AnchorPill />

      <InboxPill />

      {/* LSC-4 — "N waiting" — the piece that makes the live-sessions
          cockpit worth building: the operator shouldn't have to go look.
          Same hidden-at-zero geometry as the two pills above. */}
      <WaitingChip />

      <DeskPill />

      <div className="kb-viewtoggle" role="tablist" aria-label="view mode">
        {NAV_ITEMS.map((item) => {
          const active = item.isActive(loc, params);
          return (
            <Link
              key={item.id}
              to={item.href({ params, kb: explicitKb })}
              role="tab"
              aria-selected={active}
              className={active ? "on" : ""}
              title={item.label}
              data-testid={item.testid}
            >
              <item.Glyph />
            </Link>
          );
        })}
      </div>

      <button
        className="kb-search-mini"
        onClick={onOpenCmdk}
        aria-label="open search (cmd-k)"
        title="search (⌘K)"
      >
        <Icon.Search />
        <span className="kb-search-mini__placeholder">Search artifacts…</span>
        <kbd className="kbd">⌘K</kbd>
      </button>

      {/* C2 — first-class capture entry point (was palette + drawer row
          only). Same event the palette command + BottomTabBar button
          dispatch; App.tsx owns the CaptureSheet's open state. Hidden
          ≤860px by mobile.css — the mobile home is the tab bar button. */}
      <button
        type="button"
        className="kb-iconbtn"
        data-kb-act="capture"
        title="Capture files or text"
        aria-label="capture"
        onClick={() => window.dispatchEvent(new CustomEvent("kb:capture.open"))}
      >
        <Icon.Plus />
      </button>

      <SavedQueriesButton />

      {/* SH.D.4 — the keyboard system (chords, marks, registers, hints) had
          ZERO UI entry point beyond the bare '?' keypress (design audit).
          A plain text '?' inside the existing .kb-iconbtn shell — no drawn
          glyph exists for this and a literal question mark is typographically
          fine here. Fires the same helpOpen toggle HotkeyRoot's own '?' key
          uses, via the window event that component listens for. */}
      <button
        type="button"
        className="kb-iconbtn"
        data-kb-act="keyhelp"
        title="keyboard shortcuts (?)"
        aria-label="keyboard shortcuts"
        onClick={() => window.dispatchEvent(new CustomEvent("kb:keyhelp.toggle"))}
      >
        ?
      </button>

      <ThemeToggle />

      <Link
        to="/settings"
        className={`kb-iconbtn ${loc.pathname === "/settings" ? "on" : ""}`}
        title="Settings"
        aria-label="Settings"
      >
        <Icon.Settings />
      </Link>
    </header>
  );
}

// v0.34 W — subtle current-user chip next to the workspace pill.
// `data-kb-identity` is the e2e lock-step selector (spa-multiuser.spec).
function IdentityChip() {
  const identity = useIdentity();
  if (!identity?.user) return null;
  const source = identity.identity_source || "unknown";
  return (
    <span
      className="kb-identity"
      data-kb-identity
      title={`identity source: ${source}`}
      aria-label={`signed in as ${identity.user}, identity source: ${source}`}
    >
      {identity.user}
    </span>
  );
}

// Anchor pill — warm-amber chip with ⚓ + count. Wired to the
// corkboard count via useAnchors (live-synced through anchor.added /
// anchor.removed SSE). The /anchors view's "Stale" tab handles stale
// comment anchors as a secondary concern; we no longer surface them
// in the Header to avoid the warm-amber pill flickering on indexer
// activity that the user doesn't need to react to right now.
function AnchorPill() {
  const { count } = useAnchors();
  // Carry the active space so the corkboard keeps the workspace pill put,
  // even though /anchors itself renders the cross-kb count.
  const kb = useExplicitKb();
  if (count === 0) return null;
  return (
    <Link
      to={withKb("/anchors", kb)}
      className="kb-anchor"
      title={`${count} anchored artifact${count === 1 ? "" : "s"}`}
    >
      <Icon.Anchor />
      <span>{count}</span>
    </Link>
  );
}

// Z4 — fleet-wide open-comments inbox pill. Speech-bubble glyph + the
// total open-comment count across every corpus, live-synced through the
// same ["inbox"] query the /inbox route reads (the SSE bridge invalidates
// it on comments.updated). Hidden at 0 (mirrors AnchorPill) so it only
// draws attention when there's something to triage.
function InboxPill() {
  const { totalOpen } = useInbox();
  if (totalOpen === 0) return null;
  return (
    <Link
      to="/inbox"
      className="kb-inbox"
      data-testid="header-inbox"
      title={`${totalOpen} open comment${totalOpen === 1 ? "" : "s"} across the fleet`}
      aria-label={`comments inbox: ${totalOpen} open`}
    >
      <Icon.Comment />
      <span>{totalOpen}</span>
    </Link>
  );
}

// v0.12 Q4 — saved-queries bookmark button + drop-down. Click the
// icon to open a popover: lists every saved query with a click-to-
// restore link + remove ✕; bottom row "+ save current" prompts for
// a name and stores the active URL.
function SavedQueriesButton() {
  const { queries, save, remove, matchesCurrent } = useSavedQueries();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const navigate = useNavigate();
  const here = matchesCurrent();

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

  const promptSave = () => {
    const name = window.prompt("Name this query:");
    if (name && save(name)) setOpen(false);
  };

  return (
    <div className="kb-saved-wrap" ref={wrapRef}>
      <button
        className={`kb-iconbtn ${here ? "on" : ""}`}
        title={
          here
            ? `saved query: ${here.name}`
            : queries.length
              ? `${queries.length} saved query${queries.length === 1 ? "" : "ies"}`
              : "saved queries"
        }
        aria-label="saved queries"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <Icon.Bookmark />
      </button>
      {open && (
        <div className="kb-saved-pop" role="dialog" aria-label="saved queries">
          <div className="kb-saved-pop__head">Saved queries</div>
          {queries.length === 0 ? (
            <div className="kb-saved-pop__empty">none yet</div>
          ) : (
            <ul className="kb-saved-pop__list">
              {queries.map((q) => (
                <li key={q.name} className="kb-saved-pop__row">
                  <button
                    type="button"
                    className="kb-saved-pop__open"
                    onClick={() => {
                      navigate(q.path + q.search);
                      setOpen(false);
                    }}
                    title={`${q.path}${q.search}`}
                  >
                    {q.name}
                  </button>
                  <button
                    type="button"
                    className="kb-saved-pop__x"
                    onClick={(e) => {
                      e.stopPropagation();
                      remove(q.name);
                    }}
                    title="remove"
                    aria-label={`remove ${q.name}`}
                  >
                    <Icon.X />
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            className="kb-saved-pop__add"
            onClick={promptSave}
          >
            + save current query
          </button>
          <div className="kb-saved-pop__hint">
            Browser-local (localStorage). Daemon-side sync is v0.13.
          </div>
        </div>
      )}
    </div>
  );
}

function ThemeToggle() {
  const [theme, setTheme] = useState<string>(() => loadPrefs().theme);
  return (
    <button
      className="kb-iconbtn"
      onClick={() => {
        const next = cycleTheme();
        setTheme(next);
      }}
      title={`theme: ${theme} — click to cycle`}
      aria-label="cycle theme"
    >
      <Icon.Sun />
    </button>
  );
}

function KbSelector({
  kb,
  kbs,
  scoped,
  onChange,
}: {
  kb: string;
  kbs: KbSummary[];
  // K1 — false on a section view with no explicit kb: the pill is showing the
  // DEFAULT corpus, not a hard selection. Render it dimmed + "click to pin" so
  // the selector is honest about where you are, instead of silently implying
  // you picked the first kb. The displayed name stays the fallback (unchanged).
  scoped: boolean;
  onChange: (name: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const sorted = useMemo(
    () => [...kbs].sort((a, b) => a.name.localeCompare(b.name)),
    [kbs],
  );

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

  return (
    <div className="kb-selector-wrap" ref={wrapRef}>
      <button
        className={`kb-ws${scoped ? "" : " kb-ws--unscoped"}`}
        data-scope={scoped ? "one" : "unscoped"}
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={scoped ? undefined : "default scope — click to pin a corpus"}
      >
        <span className="kb-ws-k" aria-hidden="true">
          K
        </span>
        <span className="kb-ws-name">{kb}</span>
        <Icon.ChevDown />
      </button>
      {open && sorted.length > 0 && (
        <ul className="kb-popover" role="listbox">
          {sorted.map((k) => (
            <li key={k.name}>
              <button
                role="option"
                aria-selected={k.name === kb}
                className={`kb-popover__item ${k.name === kb ? "is-on" : ""}`}
                onClick={() => {
                  onChange(k.name);
                  setOpen(false);
                }}
              >
                <span className="kb-popover__name">{k.name}</span>
                <span className="kb-popover__count">{k.doc_count}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
