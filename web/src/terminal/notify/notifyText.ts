// TERMNOTIFY - the one place notification wording is built. Inputs are the
// scrubbed live title and a project basename only.

import type { AttnEvent } from "./derive";
import { buildHubUrl } from "../hub/termHub";

export const TITLE_MAX = 60;

function clip(s: string): string {
  const a = Array.from(s);
  return a.length <= TITLE_MAX ? s : a.slice(0, TITLE_MAX - 1).join("") + "…";
}

export function eventText(e: AttnEvent): string {
  if (e.kind === "summary") {
    if (e.finished === 0) return `${e.count} terminals need attention`;
    if (e.asks === 0) return `${e.count} terminals finished`;
    return `${e.count} terminals need attention or finished`;
  }
  const who = `${clip(e.title)} (${clip(e.project)})`;
  return e.kind === "asks" ? `${who} needs you` : `${who} finished`;
}

export function eventLabel(e: AttnEvent): string {
  return e.kind === "summary" ? "Terminals" : "Go";
}

export function eventUrl(e: AttnEvent): string {
  return e.kind === "summary" ? buildHubUrl() : buildHubUrl({ id: e.termId });
}

export function eventTag(e: AttnEvent): string {
  return e.kind === "summary" ? "kb-term-summary" : `kb-term-${e.termId}`;
}
