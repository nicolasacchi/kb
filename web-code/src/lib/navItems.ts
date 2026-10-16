// OL-C1 — the ONE destination catalogue for web-code's TopBar, NavSheet and
// the Collections page. Four labelled DOORS (Code · Search · Review ·
// Collections) replace the old Branches/PRs/Reviews chips + the 12-item
// Explore popover; Inbox stays an icon link in the Spine's right slot.
//
// `matchActiveNavItem` maps EVERY former route onto its door (table-tested in
// `navItems.test.ts` against each path in `app.tsx`), so a destination that
// has no chip of its own still lights the door that contains it. Every `url`
// is a real export from `codeUrl` / `setsUrl` / `searchLanes`.

import { Icon } from "../components/iconSet";
import { readerUrl } from "./breadcrumbs";
import {
  boardsPageUrl,
  canvasPageUrl,
  collectionsUrl,
  commentsUrl,
  hotspotsUrl,
  lanesUrl,
  railsUrl,
  recipesPageUrl,
  reviewsUrl,
  stacksPageUrl,
  todosUrl,
  toursPageUrl,
} from "./codeUrl";
import { fullSearchUrl } from "./searchLanes";
import { setsUrl, workspacesUrl } from "./setsUrl";

// ── S2-A: unified inbox ───────────────────────────────────────────────────
// `/~inbox` is top-level and repo-LESS, so this one `url` ignores its `repo`.
export function inboxUrl(_repo: string): string {
  return "/~inbox";
}

type IconComp = (typeof Icon)[keyof typeof Icon];

export type DoorKey = "code" | "search" | "review" | "collections";

export interface NavItem {
  key: DoorKey;
  label: string;
  icon: IconComp;
  url: (repo: string) => string;
}

/// The four doors, in bar order. `code` lands on the reader root — the same
/// target the Home repo cards link to (`readerUrl(repo, "")`).
export const NAV_ITEMS: readonly NavItem[] = [
  { key: "code", label: "Code", icon: Icon.Code, url: (repo) => readerUrl(repo, "") },
  { key: "search", label: "Search", icon: Icon.Search, url: (repo) => fullSearchUrl("", repo) },
  { key: "review", label: "Review", icon: Icon.ClipboardCheck, url: reviewsUrl },
  { key: "collections", label: "Collections", icon: Icon.Layers, url: collectionsUrl },
];

export type CollectionGroup = "sets" | "boards" | "instruments";

export type CollectionKey =
  | "sets"
  | "stacks"
  | "workspaces"
  | "tours"
  | "boards"
  | "canvas"
  | "rails"
  | "lanes"
  | "recipes"
  | "todos"
  | "hotspots"
  | "comments";

/// A kbc-seq/1 count: which projection (+ which physical table, when a
/// projection spans two) a card's number is read from. Absent = no count.
export interface SeqCount {
  projection: "set" | "workspace" | "tour" | "board";
  source?: "reading_sets" | "canvas_boards" | "canvas_sets";
}

export interface CollectionItem {
  key: CollectionKey;
  label: string;
  group: CollectionGroup;
  icon: IconComp;
  purpose: string;
  url: (repo: string) => string;
  count?: SeqCount;
}

export const COLLECTION_GROUPS: readonly { group: CollectionGroup; label: string }[] = [
  { group: "sets", label: "Sets" },
  { group: "boards", label: "Boards" },
  { group: "instruments", label: "Instruments" },
];

/// The 12 former Explore items (Browser lives under the Code door) plus the
/// Boards and Lanes surfaces that no menu used to reach.
export const COLLECTION_ITEMS: readonly CollectionItem[] = [
  {
    key: "sets",
    label: "Sets",
    group: "sets",
    icon: Icon.Bookmark,
    purpose: "Ordered file and span walkthroughs.",
    url: setsUrl,
    count: { projection: "set" },
  },
  {
    key: "stacks",
    label: "Stacks",
    group: "sets",
    icon: Icon.Layers,
    purpose: "Dependent-branch stacks with each layer's own diff.",
    url: stacksPageUrl,
  },
  {
    key: "workspaces",
    label: "Workspaces",
    group: "sets",
    icon: Icon.Panel,
    purpose: "Saved open-file sets from the reader.",
    url: workspacesUrl,
    count: { projection: "workspace" },
  },
  {
    key: "tours",
    label: "Tours",
    group: "sets",
    icon: Icon.Spark,
    purpose: "Walks through references, re-resolved on every read.",
    url: toursPageUrl,
    count: { projection: "tour", source: "canvas_boards" },
  },
  {
    key: "boards",
    label: "Boards",
    group: "boards",
    icon: Icon.Grid,
    purpose: "Ordered sets of references into this repo, never copies.",
    url: boardsPageUrl,
    count: { projection: "board", source: "canvas_boards" },
  },
  {
    key: "canvas",
    label: "Canvas",
    group: "boards",
    icon: Icon.Graph,
    purpose: "Symbol fragments on a pan-zoom surface.",
    url: canvasPageUrl,
    count: { projection: "board", source: "canvas_sets" },
  },
  {
    key: "rails",
    label: "Rails",
    group: "instruments",
    icon: Icon.Entity,
    purpose: "Convention edges joined to the entity index.",
    url: (repo) => railsUrl(repo),
  },
  {
    key: "lanes",
    label: "Lanes",
    group: "instruments",
    icon: Icon.Table,
    purpose: "The augmentation-lane registry.",
    url: lanesUrl,
  },
  {
    key: "recipes",
    label: "Recipes",
    group: "instruments",
    icon: Icon.Terminal,
    purpose: "Named, parameterised questions over the repo's indexes.",
    url: recipesPageUrl,
  },
  {
    key: "todos",
    label: "TODOs",
    group: "instruments",
    icon: Icon.Tasks,
    purpose: "Comment markers from the live index.",
    url: todosUrl,
  },
  {
    key: "hotspots",
    label: "Hotspots",
    group: "instruments",
    icon: Icon.Flame,
    purpose: "Where to look first, from change history.",
    url: hotspotsUrl,
  },
  {
    key: "comments",
    label: "Comments",
    group: "instruments",
    icon: Icon.Comment,
    purpose: "Every kind of comment, with drift and age state.",
    url: commentsUrl,
  },
];

/// Pure: read a kbc-seq/1 list into per-card counts. `size`-less rows still
/// count as rows (a row is a row); a card whose projection is absent from
/// the list gets no entry, never a 0.
export function seqCounts(
  rows: readonly { projection: string; source: string }[],
): Partial<Record<CollectionKey, number>> {
  const out: Partial<Record<CollectionKey, number>> = {};
  for (const item of COLLECTION_ITEMS) {
    const c = item.count;
    if (!c) continue;
    out[item.key] = rows.filter(
      (r) => r.projection === c.projection && (c.source === undefined || r.source === c.source),
    ).length;
  }
  return out;
}

/// Decode so an encoded builder URL still matches react-router's decoded
/// `location.pathname`; the query string is ignored.
function pathOnly(s: string): string {
  const noQuery = s.split("?")[0] ?? s;
  try {
    return decodeURI(noQuery);
  } catch {
    return noQuery;
  }
}

/// Pure: does `pathname` sit on `itemUrl` or a nested child.
export function navItemMatches(pathname: string, itemUrl: string): boolean {
  const path = pathOnly(pathname);
  const base = pathOnly(itemUrl);
  return path === base || path.startsWith(`${base}/`);
}

/// The `~sentinel` segment (first one after `/r/{repo}`) → the door that
/// contains it. A `~` segment not listed here (`~commit`, `~story`,
/// `~diff`, `~lens`, `~browser`, …) is reader territory and falls through
/// to Code, exactly like a file path under the repo.
const SENTINEL_DOOR: Record<string, DoorKey> = {
  "~reviews": "review",
  "~prs": "review",
  "~branches": "review",
  "~compare": "review",
  "~range-diff": "review",
  "~collections": "collections",
  "~sets": "collections",
  "~stacks": "collections",
  "~workspaces": "collections",
  "~tours": "collections",
  "~boards": "collections",
  "~canvas": "collections",
  "~rails": "collections",
  "~lanes": "collections",
  "~recipes": "collections",
  "~todos": "collections",
  "~hotspots": "collections",
  "~comments": "collections",
};

/// The door containing `pathname` for `repo`, or `null` off every door
/// (Home, Inbox, another repo, a repo-less route).
export function matchActiveNavItem(pathname: string, repo: string): NavItem | null {
  const path = pathOnly(pathname);
  let key: DoorKey | null = null;
  if (path === "/search") {
    key = "search";
  } else {
    const root = pathOnly(`/r/${encodeURIComponent(repo)}`);
    if (path === root) key = "code";
    else if (path.startsWith(`${root}/`)) {
      const seg = path.slice(root.length + 1).split("/")[0] ?? "";
      key = SENTINEL_DOOR[seg] ?? "code";
    }
  }
  return key ? (NAV_ITEMS.find((i) => i.key === key) ?? null) : null;
}

/// OL-P5 — which repo the doors link to when the current view is not scoped
/// to one (Home, `/~inbox`, `/~settings`, an unscoped `/search`): the explicit
/// repo, else the last repo the user was in (only if the daemon still lists
/// it), else the first configured repo, else `null` (no repos known yet).
export function resolveDoorRepo(args: {
  explicit: string | null;
  last: string | null;
  repos: readonly string[];
}): string | null {
  if (args.explicit) return args.explicit;
  if (args.last && args.repos.includes(args.last)) return args.last;
  return args.repos[0] ?? null;
}

export function navTestAttr(key: DoorKey | "inbox"): string {
  return `data-kbc-topbar-${key}`;
}
