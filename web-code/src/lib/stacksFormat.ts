// Pure helpers for the stacks SPA surface (V3.3-S2).

import type { StackLayer } from "../api/types";
import { codeBasePath } from "./codeUrl";
import { shortSha } from "./format";

/// `/r/{repo}/~stacks[?all=1][&branch=]`.
export function stacksUrl(
  repo: string,
  opts: { all?: boolean; branch?: string } = {},
): string {
  const base = `${codeBasePath(repo, "")}/~stacks`;
  const params = new URLSearchParams();
  if (opts.all) params.set("all", "1");
  if (opts.branch) params.set("branch", opts.branch);
  const qs = params.toString();
  return qs ? `${base}?${qs}` : base;
}

export interface LayerTag {
  kind: "stale" | "tip_shared" | "unresolved";
  /** Neutral UI copy — never shame language. */
  label: string;
}

/// Visible tags for a stack layer (design law: stale = "base moved",
/// unresolved = "walk bound exceeded").
export function layerTags(layer: StackLayer): LayerTag[] {
  const tags: LayerTag[] = [];
  if (layer.stale) tags.push({ kind: "stale", label: "base moved" });
  if (layer.tip_shared) tags.push({ kind: "tip_shared", label: "tip shared" });
  if (layer.unresolved) tags.push({ kind: "unresolved", label: "walk bound exceeded" });
  return tags;
}

/// Header line for a layer-diff panel: "diff vs base <base> @ <short tip>".
export function layerDiffHeader(base: string, baseTip: string, stale: boolean): string {
  const tip = shortSha(baseTip) || baseTip.slice(0, 7);
  const core = `diff vs base ${base} @ ${tip}`;
  return stale ? `${core} · base moved since cut` : core;
}

/// Basename of a path for node chips (last segment; empty → full path).
export function pathBasename(path: string): string {
  const p = path.replace(/\\/g, "/");
  const i = p.lastIndexOf("/");
  return i >= 0 ? p.slice(i + 1) : p || path;
}
