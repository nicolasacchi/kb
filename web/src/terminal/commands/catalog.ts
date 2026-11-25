// CMDPURE: the pure command-catalogue logic. DOM-free, React-free, fetch-free.
//
//   mergeCatalogue   defaults + overlay -> effective catalogue, mirroring the
//                    server's `merge()` (kb-server terminal/command_store.rs)
//                    rule for rule; both sides are pinned by the shared
//                    merge.golden.json.
//   forHarness / groupEntries / searchEntries   filter, group, rank.
//   toKeystrokes     exactly what an entry sends to the terminal.
//
// `toKeystrokes` does NOT call `planSend`: it reports the payload and an
// `enter` flag, nothing more. The server still refuses Enter after a
// paste-only paste and while a session is blocked; the consumer (CMDSHEET)
// wires `planSend`. No trailing newline is ever added to a text payload.

import type { EffGroup } from "../../api/generated/EffGroup";
import type { EffItem } from "../../api/generated/EffItem";
import type { OvHarness } from "../../api/generated/OvHarness";
import type { OvItem } from "../../api/generated/OvItem";
import type { OverlayPut } from "../../api/generated/OverlayPut";
import { DEFAULT_CATALOGUE } from "./defaults";
import { isKeyName, type KeyName } from "../phone/keys";

type Origin = "default" | "overlay";

/** The shipped defaults shape (`DEFAULT_CATALOGUE` fits structurally). */
export interface DefaultsDoc {
  harnesses: Record<string, OvHarness>;
}
export type OverlayDoc = Pick<OverlayPut, "harnesses" | "hidden">;
export interface Effective {
  harnesses: Record<string, { groups: EffGroup[] }>;
}
export interface Entry {
  harness: string;
  groupId: string;
  groupLabel: string;
  groupOrigin: Origin;
  item: EffItem;
}
export type Keystrokes =
  | { kind: "keys"; keys: KeyName[]; unverified: boolean }
  | { kind: "text"; text: string; enter: boolean; unverified: boolean }
  | { kind: "incomplete"; missing: string[] }
  | { kind: "invalid"; reason: "arg-control-char" | "bad-key" | "bad-item" };

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

function own<T>(o: Record<string, T> | undefined, k: string): T | undefined {
  return o !== undefined && Object.prototype.hasOwnProperty.call(o, k) ? o[k] : undefined;
}

function toEff(item: OvItem, origin: Origin): EffItem {
  const out: EffItem = { id: item.id, kind: item.kind, label: item.label, origin };
  if (item.description !== undefined) out.description = item.description;
  if (item.text !== undefined) out.text = item.text;
  if (item.mode !== undefined) out.mode = item.mode;
  if (item.args !== undefined) out.args = item.args.map((a) => ({ ...a }));
  if (item.keys !== undefined) out.keys = [...item.keys];
  if (item.unverified !== undefined) out.unverified = item.unverified;
  return out;
}

/** The effective catalogue: the server's merge rule (see command_store.rs `merge`). */
export function mergeCatalogue(defaults: DefaultsDoc, overlay: OverlayDoc | null): Effective {
  const ovH = overlay?.harnesses ?? {};
  const hiddenMap = overlay?.hidden ?? {};
  const names = [...new Set([...Object.keys(defaults.harnesses), ...Object.keys(ovH)])].sort();
  const out: Effective = { harnesses: {} };
  for (const name of names) {
    const dgroups = own(defaults.harnesses, name)?.groups ?? [];
    const ogroups = own(ovH, name)?.groups ?? [];
    const hidden = new Set(own(hiddenMap, name) ?? []);
    const oitems = new Map<string, OvItem>();
    for (const g of ogroups) for (const it of g.items) if (!oitems.has(it.id)) oitems.set(it.id, it);
    const defaultIds = new Set<string>();
    for (const g of dgroups) for (const it of g.items) defaultIds.add(it.id);
    const dgroupIds = new Set(dgroups.map((g) => g.id));

    const groups: EffGroup[] = [];
    for (const dg of dgroups) {
      const og = ogroups.find((g) => g.id === dg.id);
      const items: EffItem[] = [];
      for (const it of dg.items) {
        const o = oitems.get(it.id);
        if (o !== undefined) items.push(toEff(o, "overlay"));
        else if (!hidden.has(it.id)) items.push(toEff(it, "default"));
      }
      if (og !== undefined) {
        for (const it of og.items) if (!defaultIds.has(it.id)) items.push(toEff(it, "overlay"));
      }
      if (items.length > 0) {
        groups.push({
          id: dg.id,
          label: og !== undefined ? og.label : dg.label,
          origin: og !== undefined ? "overlay" : "default",
          items,
        });
      }
    }
    for (const og of ogroups) {
      if (dgroupIds.has(og.id)) continue;
      const items = og.items.filter((i) => !defaultIds.has(i.id)).map((i) => toEff(i, "overlay"));
      if (items.length > 0) groups.push({ id: og.id, label: og.label, origin: "overlay", items });
    }
    out.harnesses[name] = { groups };
  }
  return out;
}

/** Convenience: the shipped defaults merged with `overlay`. */
export function effectiveCatalogue(overlay: OverlayDoc | null): Effective {
  return mergeCatalogue(DEFAULT_CATALOGUE as unknown as DefaultsDoc, overlay);
}

// ---------------------------------------------------------------------------
// filter / group
// ---------------------------------------------------------------------------

/** Flatten one harness's catalogue in order; an unknown harness is `[]`. */
export function forHarness(c: Effective, harness: string): Entry[] {
  const h = own(c.harnesses, harness);
  if (h === undefined || !Array.isArray(h.groups)) return [];
  const out: Entry[] = [];
  for (const g of h.groups) {
    for (const item of g.items) {
      out.push({ harness, groupId: g.id, groupLabel: g.label, groupOrigin: g.origin, item });
    }
  }
  return out;
}

/** Group entries by group id, first-seen order; empty groups never appear. */
export function groupEntries(entries: readonly Entry[]): { id: string; label: string; entries: Entry[] }[] {
  const out: { id: string; label: string; entries: Entry[] }[] = [];
  const at = new Map<string, number>();
  for (const e of entries) {
    const i = at.get(e.groupId);
    if (i === undefined) {
      at.set(e.groupId, out.length);
      out.push({ id: e.groupId, label: e.groupLabel, entries: [e] });
    } else out[i].entries.push(e);
  }
  return out;
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

function norm(s: string): string {
  return s.normalize("NFKC").toLowerCase().trim();
}
function stripSlash(s: string): string {
  return s.startsWith("/") ? s.slice(1) : s;
}
function cpLen(s: string): number {
  return [...s].length;
}
function isWordBoundary(c: string): boolean {
  return c === " " || c === "-" || c === "_" || c === "/";
}
function wordStart(label: string, q: string): boolean {
  let from = 0;
  for (;;) {
    const i = label.indexOf(q, from);
    if (i < 0) return false;
    if (i === 0 || isWordBoundary(label[i - 1])) return true;
    from = i + 1;
  }
}
function subsequence(label: string, q: string): boolean {
  const l = [...label];
  let j = 0;
  for (const c of q) {
    while (j < l.length && l[j] !== c) j++;
    if (j >= l.length) return false;
    j++;
  }
  return true;
}

/** Lowest tier = best; -1 = no match. */
function tierOf(e: Entry, q: string): number {
  const label = stripSlash(norm(e.item.label));
  if (label === q) return 0;
  if (label.startsWith(q)) return 1;
  if (wordStart(label, q)) return 2;
  if (label.includes(q)) return 3;
  const id = norm(e.item.id);
  if (id.startsWith(q) || id.includes(q)) return 4;
  if (e.item.description !== undefined && norm(e.item.description).includes(q)) return 5;
  if (subsequence(label, q)) return 6;
  return -1;
}

/**
 * Rank entries for `query`. Tiers (best first): exact label, label prefix,
 * label word start, label substring, id, description, label subsequence.
 * Ties: shorter label (code points), then original order. Empty query keeps
 * the input order. Deterministic: no locale compare, no cache.
 */
export function searchEntries(entries: readonly Entry[], query: string): Entry[] {
  const q = stripSlash(norm(query));
  if (q === "") return [...entries];
  const scored: { e: Entry; tier: number; len: number; idx: number }[] = [];
  entries.forEach((e, idx) => {
    const tier = tierOf(e, q);
    if (tier >= 0) scored.push({ e, tier, len: cpLen(stripSlash(norm(e.item.label))), idx });
  });
  scored.sort((a, b) => a.tier - b.tier || a.len - b.len || a.idx - b.idx);
  return scored.map((s) => s.e);
}

// ---------------------------------------------------------------------------
// keystrokes
// ---------------------------------------------------------------------------

const CONTROL_RE = /\p{Cc}/u;

/** Is `v` fit to be substituted into a payload of this mode? */
function valueOk(v: string, run: boolean): boolean {
  for (const c of v) {
    if (!CONTROL_RE.test(c)) continue;
    if (!run && (c === "\n" || c === "\t")) continue;
    return false;
  }
  return true;
}

/**
 * Exactly what an entry sends. Chords become key names; commands and replies
 * become a text payload plus an `enter` flag (true only for `mode: "run"`).
 * The flag is sent by the consumer as the named key `Enter`; no newline is
 * ever appended to `text`.
 */
export function toKeystrokes(x: Entry | EffItem, args?: Record<string, string>): Keystrokes {
  const item: EffItem = "item" in x ? x.item : x;
  const unverified = item.unverified === true;
  if (item.kind === "chord") {
    const keys = item.keys;
    if (!Array.isArray(keys) || keys.length === 0 || !keys.every((k) => isKeyName(k))) {
      return { kind: "invalid", reason: "bad-key" };
    }
    return { kind: "keys", keys: [...keys] as KeyName[], unverified };
  }
  if ((item.kind !== "command" && item.kind !== "reply") || typeof item.text !== "string") {
    return { kind: "invalid", reason: "bad-item" };
  }
  const run = item.mode === "run";
  const values = new Map<string, string>();
  const missing: string[] = [];
  for (const a of item.args ?? []) {
    const given = own(args, a.name);
    const v = given !== undefined ? given : a.default;
    if (v === undefined) missing.push(a.name);
    else values.set(a.name, v);
  }
  if (missing.length > 0) return { kind: "incomplete", missing };

  const chars = [...item.text];
  let text = "";
  for (let i = 0; i < chars.length; i++) {
    const c = chars[i];
    if (c === "{" && chars[i + 1] === "{") {
      text += "{";
      i++;
    } else if (c === "}" && chars[i + 1] === "}") {
      text += "}";
      i++;
    } else if (c === "{") {
      const end = chars.indexOf("}", i + 1);
      const name = end < 0 ? undefined : chars.slice(i + 1, end).join("");
      const v = name === undefined ? undefined : values.get(name);
      if (end >= 0 && v !== undefined) {
        if (!valueOk(v, run)) return { kind: "invalid", reason: "arg-control-char" };
        text += v;
        i = end;
      } else text += c;
    } else text += c;
  }
  return { kind: "text", text, enter: run, unverified };
}
