import type { FunctionComponent } from "react";
import { Icon } from "@kb/ui/icons";

// OL-L2 — the ONE destination catalogue for the SPA's primary nav: five
// labelled DOORS (Library · Search · Memory · Worklog · Lists). Consumed by the
// desktop Header's door strip and the mobile sheet's NavList so the two never
// drift. (Settings / theme / saved-queries are chrome-level actions each
// consumer renders separately.)
//
// A door owns a FAMILY of routes: `isActive` lights it on every route of its
// family, and Worklog / Lists additionally expose a sub-nav row (`WORKLOG_TABS`
// / `LISTS_TABS`) so the family's members stay one click apart. The Library's
// views (grid, list, atlas, press, history, canvas) are not doors; they are
// chosen with `?view=` (and the `g` chords) inside the Library.

export type NavId = "library" | "search" | "memory" | "worklog" | "lists";

// Context handed to every href builder: the EXPLICIT active kb — the
// path/query kb with no first-kb fallback (see useExplicitKb). A non-null kb
// is carried into the destination so navigating between sections keeps the
// user's space; a null kb leaves the URL kb-less so an unscoped view stays
// clean.
export type NavHrefCtx = { kb: string | null };

// Append ?kb= (or &kb= when the path already carries a query) to a
// destination, URI-encoding the value. A null kb returns the path untouched.
export function withKb(path: string, kb: string | null): string {
  if (!kb) return path;
  const sep = path.includes("?") ? "&" : "?";
  return `${path}${sep}kb=${encodeURIComponent(kb)}`;
}

/// True when `pathname` is `base` or lies beneath it (`/lists` matches
/// `/lists` and `/lists/kb/id`, never `/listsx`).
export function underPath(pathname: string, base: string): boolean {
  return pathname === base || pathname.startsWith(`${base}/`);
}

export type NavItem = {
  id: NavId;
  label: string;
  Glyph: FunctionComponent;
  href: (ctx: NavHrefCtx) => string;
  // Pure on the pathname: no door depends on query params.
  isActive: (pathname: string) => boolean;
  // Preserve existing e2e selectors when a door grows a test hook.
  testid?: string;
};

export const NAV_ITEMS: NavItem[] = [
  {
    // The gallery root. The href is the bare root (+ kb): it drops `?view=`
    // and every filter, so the door always lands on the default Library; a
    // view is picked inside it. The reader (/a/...) belongs to the Library.
    id: "library",
    label: "Library",
    Glyph: Icon.Grid,
    href: ({ kb }) => withKb("/", kb),
    isActive: (p) => p === "/" || underPath(p, "/a"),
  },
  {
    // The full search page (deeper than the Cmd+K popup).
    id: "search",
    label: "Search",
    Glyph: Icon.Search,
    href: ({ kb }) => withKb("/search", kb),
    isActive: (p) => p === "/search",
    testid: "header-search-tab",
  },
  {
    id: "memory",
    label: "Memory",
    Glyph: Icon.Brain,
    href: ({ kb }) => withKb("/memory", kb),
    isActive: (p) => p === "/memory",
  },
  {
    // Sessions, Slates and session replays: the record of work done.
    id: "worklog",
    label: "Worklog",
    Glyph: Icon.Terminal,
    href: ({ kb }) => withKb("/sessions", kb),
    isActive: (p) =>
      underPath(p, "/sessions") ||
      underPath(p, "/slates") ||
      underPath(p, "/replay"),
    testid: "header-sessions-tab",
  },
  {
    // Reading lists, their boards and Notes.
    id: "lists",
    label: "Lists",
    Glyph: Icon.Tasks,
    href: ({ kb }) => withKb("/lists", kb),
    isActive: (p) =>
      underPath(p, "/lists") || underPath(p, "/notes") || underPath(p, "/board"),
  },
];

/// The door that lights for `pathname`, or null (settings, inbox, anchors…).
export function matchActiveNavItem(pathname: string): NavItem | null {
  return NAV_ITEMS.find((item) => item.isActive(pathname)) ?? null;
}

// ── sub-nav rows ──────────────────────────────────────────────────────────
// One row at the top of each family page keeps the family's members together.
export type SectionTab = {
  id: string;
  label: string;
  href: (ctx: NavHrefCtx) => string;
  isActive: (pathname: string) => boolean;
  testid: string;
};

export const WORKLOG_TABS: SectionTab[] = [
  {
    id: "sessions",
    label: "Sessions",
    href: ({ kb }) => withKb("/sessions", kb),
    isActive: (p) => underPath(p, "/sessions") || underPath(p, "/replay"),
    testid: "worklog-tab-sessions",
  },
  {
    // A slate is per-PROJECT coordination keyed on a slug, not per-corpus, so
    // the active kb is deliberately NOT carried (a `?kb=` would suggest a
    // scoping the route does not have).
    id: "slates",
    label: "Slates",
    href: () => "/slates",
    isActive: (p) => underPath(p, "/slates"),
    testid: "header-slates-tab",
  },
];

export const LISTS_TABS: SectionTab[] = [
  {
    id: "lists",
    label: "Lists",
    href: ({ kb }) => withKb("/lists", kb),
    isActive: (p) => underPath(p, "/lists") || underPath(p, "/board"),
    testid: "lists-tab-lists",
  },
  {
    id: "notes",
    label: "Notes",
    href: ({ kb }) => withKb("/notes", kb),
    isActive: (p) => underPath(p, "/notes"),
    testid: "header-notes-tab",
  },
];
