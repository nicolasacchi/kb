import type { FunctionComponent } from "react";
import type { Location } from "react-router-dom";
import { Icon } from "../components/icons";

// Single source of truth for the SPA's primary view destinations. Consumed
// by the desktop Header's view-toggle strip and the mobile hamburger
// drawer's NavList so the two never drift. (Settings / theme / saved-queries
// are chrome-level actions each consumer renders separately.)

export type NavId =
  | "grid"
  | "list"
  | "atlas"
  | "press"
  | "search"
  | "memory"
  | "lists"
  | "notes"
  | "sessions"
  | "slates"
  | "history";

// Context handed to every href builder: the current query params (so gallery
// view-switches keep the active filters) and the EXPLICIT active kb — the
// path/query kb with no first-kb fallback (see useExplicitKb). A non-null kb
// is carried into the destination so navigating between sections keeps the
// user's space; a null kb leaves the URL kb-less so an unscoped view stays
// clean.
export type NavHrefCtx = { params: URLSearchParams; kb: string | null };

// Append ?kb= (or &kb= when the path already carries a query) to a
// destination, URI-encoding the value. A null kb returns the path untouched.
export function withKb(path: string, kb: string | null): string {
  if (!kb) return path;
  const sep = path.includes("?") ? "&" : "?";
  return `${path}${sep}kb=${encodeURIComponent(kb)}`;
}

export type NavItem = {
  id: NavId;
  label: string;
  Glyph: FunctionComponent;
  // Canonical destination. Gallery views (grid/list/atlas/history) live at
  // "/" behind `?view=` and preserve the current query params (filters) plus
  // the active kb; the standalone routes carry the active kb as `?kb=`.
  href: (ctx: NavHrefCtx) => string;
  isActive: (loc: Location, params: URLSearchParams) => boolean;
  // Preserve existing e2e selectors when a tab grows a test hook.
  testid?: string;
};

// Gallery `?view=` href that keeps every other param (tags, folder, since, …)
// and pins the active kb. `null` = the default Grid view (no `view` param).
const galleryHref =
  (
    view: Exclude<
      NavId,
      "memory" | "lists" | "sessions" | "notes" | "search" | "slates"
    > | null,
  ) =>
  ({ params, kb }: NavHrefCtx): string => {
    const p = new URLSearchParams(params);
    if (view === null || view === "grid") p.delete("view");
    else p.set("view", view);
    if (kb) p.set("kb", kb);
    const qs = p.toString();
    return qs ? `/?${qs}` : "/";
  };

const galleryActive =
  (view: NavId | null) =>
  (loc: Location, params: URLSearchParams): boolean =>
    loc.pathname === "/" && (params.get("view") || null) === view;

const routeActive =
  (path: string) =>
  (loc: Location): boolean =>
    loc.pathname === path;

export const NAV_ITEMS: NavItem[] = [
  {
    id: "grid",
    label: "Grid",
    Glyph: Icon.Grid,
    href: galleryHref(null),
    isActive: galleryActive(null),
  },
  {
    id: "list",
    label: "List",
    Glyph: Icon.List,
    href: galleryHref("list"),
    isActive: galleryActive("list"),
  },
  {
    id: "atlas",
    label: "Atlas",
    Glyph: Icon.Atlas,
    href: galleryHref("atlas"),
    isActive: galleryActive("atlas"),
  },
  {
    // W2.5 — the broadsheet: the current filter set as a deterministic
    // front page (lead/features/briefs/coverage + issue masthead).
    id: "press",
    label: "Press",
    Glyph: Icon.Press,
    href: galleryHref("press"),
    isActive: galleryActive("press"),
  },
  {
    // Track F — the full search page (deeper + customizable than the
    // Cmd+K popup). Standalone route; preserves the active kb so the page
    // opens scoped to the corpus the user was browsing.
    id: "search",
    label: "Search",
    Glyph: Icon.Search,
    href: ({ kb }) => withKb("/search", kb),
    isActive: routeActive("/search"),
    testid: "header-search-tab",
  },
  {
    id: "memory",
    label: "Memory",
    Glyph: Icon.Brain,
    href: ({ kb }) => withKb("/memory", kb),
    isActive: routeActive("/memory"),
  },
  {
    id: "lists",
    label: "Lists",
    Glyph: Icon.Tasks,
    href: ({ kb }) => withKb("/lists", kb),
    // Detail pages (/lists/:kb/:id) keep the tab lit — prefix match,
    // unlike the exact-path routeActive the flat views use.
    isActive: (loc) =>
      loc.pathname === "/lists" || loc.pathname.startsWith("/lists/"),
  },
  {
    id: "notes",
    label: "Notes",
    Glyph: Icon.Note,
    href: ({ kb }) => withKb("/notes", kb),
    isActive: routeActive("/notes"),
    testid: "header-notes-tab",
  },
  {
    id: "sessions",
    label: "Sessions",
    Glyph: Icon.Terminal,
    href: ({ kb }) => withKb("/sessions", kb),
    isActive: routeActive("/sessions"),
    testid: "header-sessions-tab",
  },
  {
    // SL4 — the slate board. Standalone route like Search/Memory/Lists; the
    // active kb is deliberately NOT carried: a slate is per-PROJECT
    // coordination keyed on a slug, not per-corpus, so a `?kb=` here would
    // suggest a scoping the route does not have.
    id: "slates",
    label: "Slates",
    Glyph: Icon.Slate,
    href: () => "/slates",
    // Board pages (/slates/:slug) keep the tab lit — prefix match, like
    // Lists' detail pages.
    isActive: (loc) =>
      loc.pathname === "/slates" || loc.pathname.startsWith("/slates/"),
    testid: "header-slates-tab",
  },
  {
    id: "history",
    label: "History",
    Glyph: Icon.History,
    href: galleryHref("history"),
    isActive: galleryActive("history"),
  },
];
