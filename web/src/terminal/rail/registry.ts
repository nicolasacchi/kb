// BSHELL — the rail/sheet tab registry. Tracks register a tab for one of the
// seven fixed ids and never touch the shell's layout. Pure: no xterm import, no
// fetching, no TanStack (the shell's own rail state is local, #23).
//
// Order is fixed (Now, Conversation, Changes, Files, Knowledge, Board, Commands) whatever the registration
// order. A duplicate id REPLACES the earlier tab; an unknown id throws.

import { createElement, type ReactNode } from "react";

export type RailTabId =
  | "now"
  | "conversation"
  | "changes"
  | "files"
  | "knowledge"
  | "board"
  | "commands";

export interface RailTab {
  id: RailTabId;
  label: string;
  badge?: number;
  /** A live badge: a hook the shell calls inside a tiny component (never in a loop), so a tab can badge from its own query. Wins over `badge`. */
  useBadge?: () => number | undefined;
  node: ReactNode;
}

export const RAIL_TAB_ORDER: readonly RailTabId[] = ["now", "conversation", "changes", "files", "knowledge", "board", "commands"];

export function isRailTabId(v: unknown): v is RailTabId {
  return typeof v === "string" && (RAIL_TAB_ORDER as readonly string[]).includes(v);
}

function placeholder(id: RailTabId, label: string): RailTab {
  return {
    id,
    label,
    node: createElement(
      "p",
      { "data-testid": `rail-placeholder-${id}`, className: "kb-termshell__soon" },
      "Coming soon",
    ),
  };
}

function defaults(): Map<RailTabId, RailTab> {
  return new Map<RailTabId, RailTab>([
    ["now", placeholder("now", "Now")],
    ["conversation", placeholder("conversation", "Conversation")],
    ["changes", placeholder("changes", "Changes")],
    ["files", placeholder("files", "Files")],
    ["knowledge", placeholder("knowledge", "Knowledge")],
    ["board", placeholder("board", "Board")],
    ["commands", placeholder("commands", "Commands")],
  ]);
}

let tabs = defaults();

/** Register (or replace) the tab for `tab.id`. Throws on an id outside the fixed seven. */
export function registerRailTab(tab: RailTab): void {
  if (!isRailTabId(tab.id)) throw new Error(`unknown rail tab id: ${String(tab.id)}`);
  tabs.set(tab.id, tab);
}

/** The registered tabs in the fixed order. */
export function railTabs(): RailTab[] {
  return RAIL_TAB_ORDER.flatMap((id) => {
    const t = tabs.get(id);
    return t ? [t] : [];
  });
}

/** Test seam: back to the seven placeholders. */
export function resetRailTabs(): void {
  tabs = defaults();
}
