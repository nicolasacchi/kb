// V4.U5 — the ONE destination catalog for web-code's TopBar. Desktop review
// chips, the Explore popover, and the mobile nav sheet all consume this
// array so the three surfaces cannot drift. Each `url` builder is a real
// export from `codeUrl` / `setsUrl` (repo-scoped `~` sentinels).

import { Icon } from "../components/icons";
import {
  branchesUrl,
  browserPageUrl,
  canvasPageUrl,
  commentsUrl,
  hotspotsUrl,
  prsUrl,
  recipesPageUrl,
  reviewsUrl,
  stacksPageUrl,
  todosUrl,
} from "./codeUrl";
import { setsUrl, workspacesUrl } from "./setsUrl";

// ── S2-A: unified inbox — kb-code v6.0 "One Inbox" (design-s2.md §S2-A) ───
// `/~inbox` is a top-level, repo-LESS route (kb items in its "From kb" lane
// aren't scoped to any one configured repo) — every other NAV_ITEMS entry
// is repo-scoped, so this is the one `url` that ignores its `repo`
// argument. Kept in the catalog anyway (rather than a bespoke TopBar link)
// so the Explore popover + mobile nav sheet + active-item highlighting all
// pick it up for free, same "one destination catalog" rationale this
// file's header doc states.
export function inboxUrl(_repo: string): string {
  return "/~inbox";
}
// ── /S2-A ──

type IconComp = (typeof Icon)[keyof typeof Icon];

export type NavGroup = "review" | "explore";

export type NavKey =
  | "branches"
  | "prs"
  | "reviews"
  | "browser"
  | "sets"
  | "workspaces"
  | "hotspots"
  | "todos"
  | "comments"
  | "recipes"
  | "stacks"
  | "canvas"
  | "inbox";

export interface NavItem {
  key: NavKey;
  label: string;
  icon: IconComp;
  group: NavGroup;
  url: (repo: string) => string;
}

export const NAV_ITEMS: readonly NavItem[] = [
  { key: "branches", label: "Branches", icon: Icon.Branch, group: "review", url: branchesUrl },
  { key: "prs", label: "PRs", icon: Icon.PullRequest, group: "review", url: prsUrl },
  { key: "reviews", label: "Reviews", icon: Icon.ClipboardCheck, group: "review", url: reviewsUrl },
  { key: "browser", label: "Browser", icon: Icon.Grid, group: "explore", url: browserPageUrl },
  { key: "sets", label: "Sets", icon: Icon.Bookmark, group: "explore", url: setsUrl },
  // V70-A10 — `workspacesUrl` takes an optional `ref` second arg (the
  // `~branches` chip); the catalog's own `url: (repo: string) => string`
  // signature only ever calls it with one, which is exactly `workspacesUrl`
  // with no `ref` — the full list, same as every other entry here.
  { key: "workspaces", label: "Workspaces", icon: Icon.Layers, group: "explore", url: workspacesUrl },
  { key: "hotspots", label: "Hotspots", icon: Icon.Flame, group: "explore", url: hotspotsUrl },
  { key: "todos", label: "TODOs", icon: Icon.Tasks, group: "explore", url: todosUrl },
  // V72-J2 (D8) — comments/1's dashboard: the richer, kind-aware surface
  // TODOs above now links forward to.
  { key: "comments", label: "Comments", icon: Icon.Comment, group: "explore", url: commentsUrl },
  { key: "recipes", label: "Recipes", icon: Icon.Terminal, group: "explore", url: recipesPageUrl },
  { key: "stacks", label: "Stacks", icon: Icon.Layers, group: "explore", url: stacksPageUrl },
  { key: "canvas", label: "Canvas", icon: Icon.Graph, group: "explore", url: canvasPageUrl },
  // S2-A — unified inbox (design-s2.md §S2-A). Repo-less; see `inboxUrl`'s
  // own doc above for why its `url` ignores `repo`.
  { key: "inbox", label: "Inbox", icon: Icon.List, group: "explore", url: inboxUrl },
];

/// Decode both sides so an encoded `item.url` still matches react-router's
/// (typically decoded) `location.pathname`. Query strings are ignored —
/// sentinels don't put routing state in the path's `?`.
function pathOnly(s: string): string {
  const noQuery = s.split("?")[0] ?? s;
  try {
    return decodeURI(noQuery);
  } catch {
    return noQuery;
  }
}

/// Pure: does `pathname` sit on `itemUrl` or a nested child
/// (`/r/{repo}/~reviews/12` matches `reviewsUrl(repo)`).
export function navItemMatches(pathname: string, itemUrl: string): boolean {
  const path = pathOnly(pathname);
  const base = pathOnly(itemUrl);
  return path === base || path.startsWith(`${base}/`);
}

/// First NAV_ITEMS entry whose url-shape matches `pathname` for `repo`,
/// or `null` when the location is not a catalog destination (reader, home).
export function matchActiveNavItem(pathname: string, repo: string): NavItem | null {
  return NAV_ITEMS.find((item) => navItemMatches(pathname, item.url(repo))) ?? null;
}

export function navTestAttr(key: NavKey): string {
  return `data-kbc-topbar-${key}`;
}
