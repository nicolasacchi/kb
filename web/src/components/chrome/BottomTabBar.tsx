import { Link, useLocation } from "react-router-dom";
import { Icon } from "../icons";
import { useExplicitKb } from "../../hooks/useActiveKb";
import { withKb } from "../../lib/navItems";

// kb2 redesign — mobile bottom tab bar. Five high-traffic destinations as
// a thumb-reachable fixed bar; the full nav (all 8 views + settings +
// filters) still lives in the hamburger drawer. "Search" is not a route —
// it opens the command palette (full-screen on mobile). C2 — "Capture" is
// likewise not a route: it dispatches the same kb:capture.open event as
// the Header iconbtn and Cmdk command, opening the App-level CaptureSheet.
// Mounted only on mobile and never on the detail route (see app.tsx); the
// immersive CSS also hides it defensively.
export default function BottomTabBar({
  cmdkOpen,
  onOpenCmdk,
}: {
  cmdkOpen: boolean;
  onOpenCmdk: () => void;
}) {
  const loc = useLocation();
  const kb = useExplicitKb();
  const onGallery = loc.pathname === "/" && !cmdkOpen;
  const onMemory = loc.pathname === "/memory";
  const onLists =
    loc.pathname === "/lists" || loc.pathname.startsWith("/lists/");

  return (
    <nav className="kb-tabbar" aria-label="primary">
      <Link
        to={withKb("/", kb)}
        className={`kb-tabbar__tab ${onGallery ? "is-active" : ""}`}
        aria-current={onGallery ? "page" : undefined}
      >
        <Icon.Grid />
        <span>Recent</span>
      </Link>
      <button
        type="button"
        className={`kb-tabbar__tab ${cmdkOpen ? "is-active" : ""}`}
        onClick={onOpenCmdk}
        aria-label="search"
      >
        <Icon.Search />
        <span>Search</span>
      </button>
      <Link
        to={withKb("/memory", kb)}
        className={`kb-tabbar__tab ${onMemory ? "is-active" : ""}`}
        aria-current={onMemory ? "page" : undefined}
      >
        <Icon.Brain />
        <span>Memory</span>
      </Link>
      <Link
        to={withKb("/lists", kb)}
        className={`kb-tabbar__tab ${onLists ? "is-active" : ""}`}
        aria-current={onLists ? "page" : undefined}
      >
        <Icon.Tasks />
        <span>Lists</span>
      </Link>
      <button
        type="button"
        className="kb-tabbar__tab"
        data-kb-act="capture"
        aria-label="capture"
        onClick={() =>
          window.dispatchEvent(new CustomEvent("kb:capture.open"))
        }
      >
        <Icon.Plus />
        <span>Capture</span>
      </button>
    </nav>
  );
}
