// BSHELL — the rail/sheet tab registry. Tracks register a tab for one of the
// three fixed ids and never touch the shell's layout. Pure: no xterm import, no
// fetching, no TanStack (the shell's own rail state is local, #23).
//
// Order is fixed (Conversation, Files, Commands) whatever the registration
// order. A duplicate id REPLACES the earlier tab; an unknown id throws.

import { createElement, type ReactNode } from "react";

export type RailTabId = "conversation" | "files" | "commands";

export interface RailTab {
  id: RailTabId;
  label: string;
  badge?: number;
  node: ReactNode;
}

export const RAIL_TAB_ORDER: readonly RailTabId[] = ["conversation", "files", "commands"];

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
    ["conversation", placeholder("conversation", "Conversation")],
    ["files", placeholder("files", "Files")],
    ["commands", placeholder("commands", "Commands")],
  ]);
}

let tabs = defaults();

/** Register (or replace) the tab for `tab.id`. Throws on an id outside the fixed three. */
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

/** Test seam: back to the three placeholders. */
export function resetRailTabs(): void {
  tabs = defaults();
}
