import { Link, useLocation, useSearchParams } from "react-router-dom";
import { Icon } from "../icons";
import { NAV_ITEMS, withKb } from "../../lib/navItems";
import { useExplicitKb } from "../../hooks/useActiveKb";
import { useAnchors } from "../../hooks/useAnchors";
import { useInbox } from "../../hooks/useInbox";
import { useSlatesAttention } from "../../hooks/useSlates";
import { cycleTheme } from "../../api/prefs";

// Destination list shown inside the mobile hamburger drawer. Reuses the
// shared NAV_ITEMS so it never drifts from the desktop header's view strip,
// and folds in the chrome actions the compact mobile header drops (Settings
// + theme toggle, plus — W3 — the Inbox/Anchors pills the ≤860px header
// hides: at 390px the two pills overflow the fixed-height no-wrap command
// bar, so THESE rows are mobile's one home for /inbox + /anchors; the
// bottom bar homes Recent/Search/Memory/Lists/Capture (C2) instead). Badge
// counts read the same TanStack cache entries the header pills do
// (["inbox"]/["anchors"], invariant #23 — no new fetch plumbing). Calls
// onNavigate after any pick so the drawer closes.
export default function NavList({ onNavigate }: { onNavigate: () => void }) {
  const loc = useLocation();
  const [params] = useSearchParams();
  const kb = useExplicitKb();
  const onSettings = loc.pathname === "/settings";
  const onInbox = loc.pathname === "/inbox";
  const onAnchors = loc.pathname === "/anchors";
  const { totalOpen } = useInbox();
  const { count: anchorCount } = useAnchors();
  // SL4 — unacknowledged hands + open asks + contested takes + stale takes
  // across every slate (lib/slateLanes.attentionCount, summed client-side
  // off GET /api/slates' per-section counts — the daemon reports counts,
  // never a score). Hidden at zero by the render guard below, which is what
  // stops the chip flashing a `0` between mount and the first response: the
  // hook returns 0 while loading and 0 when there is nothing to attend to,
  // and `> 0` treats both the same.
  const slateAttention = useSlatesAttention();

  return (
    <nav className="kb-navlist" aria-label="views">
      <div className="kb-navlist__sect">Navigate</div>
      {NAV_ITEMS.map((item) => {
        const active = item.isActive(loc, params);
        return (
          <Link
            key={item.id}
            to={item.href({ params, kb })}
            className={`kb-navlist__row ${active ? "is-active" : ""}`}
            aria-current={active ? "page" : undefined}
            onClick={onNavigate}
          >
            <span className="kb-navlist__glyph" aria-hidden="true">
              <item.Glyph />
            </span>
            <span className="kb-navlist__label">{item.label}</span>
            {item.id === "slates" && slateAttention > 0 && (
              <span className="kb-navlist__badge kb-navlist__badge--slate">
                {slateAttention}
              </span>
            )}
          </Link>
        );
      })}
      {/* W3 — homes for the pills the compact header hides. Always present
          (a nav destination shouldn't vanish at count 0); the badge mirrors
          the pill's hide-at-zero semantics. Inbox is fleet-wide (no kb, like
          the header pill); Anchors carries the active space like AnchorPill. */}
      <Link
        to="/inbox"
        className={`kb-navlist__row ${onInbox ? "is-active" : ""}`}
        aria-current={onInbox ? "page" : undefined}
        data-testid="drawer-inbox"
        onClick={onNavigate}
      >
        <span className="kb-navlist__glyph" aria-hidden="true">
          <Icon.Comment />
        </span>
        <span className="kb-navlist__label">Inbox</span>
        {totalOpen > 0 && (
          <span className="kb-navlist__badge kb-navlist__badge--inbox">
            {totalOpen}
          </span>
        )}
      </Link>
      <Link
        to={withKb("/anchors", kb)}
        className={`kb-navlist__row ${onAnchors ? "is-active" : ""}`}
        aria-current={onAnchors ? "page" : undefined}
        data-testid="drawer-anchors"
        onClick={onNavigate}
      >
        <span className="kb-navlist__glyph" aria-hidden="true">
          <Icon.Anchor />
        </span>
        <span className="kb-navlist__label">Anchors</span>
        {anchorCount > 0 && (
          <span className="kb-navlist__badge kb-navlist__badge--anchor">
            {anchorCount}
          </span>
        )}
      </Link>
      <Link
        to="/settings"
        className={`kb-navlist__row ${onSettings ? "is-active" : ""}`}
        aria-current={onSettings ? "page" : undefined}
        onClick={onNavigate}
      >
        <span className="kb-navlist__glyph" aria-hidden="true">
          <Icon.Settings />
        </span>
        <span className="kb-navlist__label">Settings</span>
      </Link>
      <button
        type="button"
        className="kb-navlist__row kb-navlist__row--btn"
        onClick={() => {
          cycleTheme();
          onNavigate();
        }}
      >
        <span className="kb-navlist__glyph" aria-hidden="true">
          <Icon.Sun />
        </span>
        <span className="kb-navlist__label">Toggle theme</span>
      </button>
    </nav>
  );
}
