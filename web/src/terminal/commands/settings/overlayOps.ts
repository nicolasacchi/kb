// CMDSETTINGS: pure builders over the stored OVERLAY (never over the
// effective list). Every operation takes the overlay the server returned
// (`data.overlay`) and gives back a new one; nothing here touches the DOM or
// the network, and nothing emits an effective-only field (`origin`).
//
// The merge rule the editor must respect (command_store.rs `merge`, mirrored
// by catalog.ts): a default item is replaced IN PLACE (at its default
// position) by the overlay item with the same id, wherever that overlay item
// sits; overlay-only items are appended after the defaults of the group with
// their group id, in overlay order. So only overlay-only items can change
// position; a default item always keeps its shipped slot.

import type { OvGroup } from "../../../api/generated/OvGroup";
import type { OvItem } from "../../../api/generated/OvItem";
import type { OverlayPut } from "../../../api/generated/OverlayPut";
import { KEY_NAMES } from "../../phone/keys";
import { DEFAULT_CATALOGUE } from "../defaults";
import { CMD_SCHEMA, HARNESS_NAMES, validateCatalogue, type CmdError, type ItemKind } from "../schema";

/** The harnesses the editor offers (the shipped catalogue's own set). */
export const EDITOR_HARNESSES = ["claude", "omp"] as const;

export const MAX_HIDDEN = 200;
const ID_RE = /^[a-z0-9][a-z0-9-]{0,47}$/;

export type ItemOrigin = "default" | "changed" | "added";

export const EMPTY_OVERLAY: OverlayPut = { harnesses: {}, hidden: {} };

export function cloneOverlay(o: OverlayPut): OverlayPut {
  return JSON.parse(JSON.stringify({ harnesses: o.harnesses ?? {}, hidden: o.hidden ?? {} })) as OverlayPut;
}

/** Drop empty groups, harness rows with no groups, and empty hidden lists (what the server's normalize does). */
export function prune(o: OverlayPut): OverlayPut {
  const out = cloneOverlay(o);
  for (const [h, row] of Object.entries(out.harnesses)) {
    row.groups = row.groups.filter((g) => g.items.length > 0);
    if (row.groups.length === 0) delete out.harnesses[h];
  }
  for (const [h, ids] of Object.entries(out.hidden)) {
    const uniq = [...new Set(ids)].sort();
    if (uniq.length === 0) delete out.hidden[h];
    else out.hidden[h] = uniq;
  }
  return out;
}

export function defaultItemIds(harness: string): Set<string> {
  const h = DEFAULT_CATALOGUE.harnesses[harness];
  return new Set((h?.groups ?? []).flatMap((g) => g.items.map((i) => i.id)));
}

export function defaultItem(harness: string, id: string): OvItem | undefined {
  const h = DEFAULT_CATALOGUE.harnesses[harness];
  for (const g of h?.groups ?? []) for (const i of g.items) if (i.id === id) return i as unknown as OvItem;
  return undefined;
}

/** `default` untouched; `changed` an overlay item replacing a shipped id; `added` overlay-only. */
export function originOf(harness: string, id: string, effectiveOrigin: "default" | "overlay"): ItemOrigin {
  if (effectiveOrigin === "default") return "default";
  return defaultItemIds(harness).has(id) ? "changed" : "added";
}

function overlayIds(o: OverlayPut, harness: string): Set<string> {
  return new Set((o.harnesses[harness]?.groups ?? []).flatMap((g) => g.items.map((i) => i.id)));
}

function slug(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

/** A fresh, valid item id: unique among the shipped ids and the overlay's. */
export function newItemId(harness: string, o: OverlayPut, label: string): string {
  const taken = new Set([...defaultItemIds(harness), ...overlayIds(o, harness)]);
  const base = `u-${slug(label).slice(0, 36) || "item"}`;
  let id = base;
  for (let n = 2; taken.has(id); n++) id = `${base}-${n}`;
  return id;
}

/** A fresh overlay group id, unique among `taken` (the effective group ids). */
export function newGroupId(taken: Iterable<string>, label: string): string {
  const set = new Set(taken);
  const base = `g-${slug(label).slice(0, 36) || "group"}`;
  let id = base;
  for (let n = 2; set.has(id); n++) id = `${base}-${n}`;
  return id;
}

function findItem(o: OverlayPut, harness: string, id: string): { group: OvGroup; index: number } | null {
  for (const group of o.harnesses[harness]?.groups ?? []) {
    const index = group.items.findIndex((i) => i.id === id);
    if (index >= 0) return { group, index };
  }
  return null;
}

/**
 * Put `item` into the overlay. An overlay item with the same id is replaced
 * where it sits; otherwise it is appended to the overlay group `groupId`
 * (created with `groupLabel` when absent).
 */
export function upsertItem(
  o: OverlayPut,
  harness: string,
  groupId: string,
  groupLabel: string,
  item: OvItem,
): OverlayPut {
  const out = cloneOverlay(o);
  const hit = findItem(out, harness, item.id);
  if (hit) {
    hit.group.items[hit.index] = item;
    return prune(out);
  }
  const row = (out.harnesses[harness] ??= { groups: [] });
  let g = row.groups.find((x) => x.id === groupId);
  if (!g) {
    g = { id: groupId, label: groupLabel, items: [] };
    row.groups.push(g);
  }
  g.items.push(item);
  return prune(out);
}

/** Drop the overlay copy of `id` (a `changed` item reverts to its default, an `added` one is deleted). */
export function removeItem(o: OverlayPut, harness: string, id: string): OverlayPut {
  const out = cloneOverlay(o);
  const hit = findItem(out, harness, id);
  if (hit) hit.group.items.splice(hit.index, 1);
  return prune(out);
}

/** Hide a shipped item: its overlay replacement (if any) goes, then the id joins `hidden`. */
export function hideDefault(o: OverlayPut, harness: string, id: string): OverlayPut {
  const out = cloneOverlay(removeItem(o, harness, id));
  const ids = out.hidden[harness] ?? [];
  if (!ids.includes(id)) ids.push(id);
  out.hidden[harness] = ids;
  return prune(out);
}

export function unhide(o: OverlayPut, harness: string, id: string): OverlayPut {
  const out = cloneOverlay(o);
  if (out.hidden[harness]) out.hidden[harness] = out.hidden[harness].filter((x) => x !== id);
  return prune(out);
}

/** Reset one item: drop its overlay copy and unhide it. */
export function resetItem(o: OverlayPut, harness: string, id: string): OverlayPut {
  return unhide(removeItem(o, harness, id), harness, id);
}

/**
 * Move an overlay-only item one slot among its group's overlay-only items.
 * `null` when it cannot move: a shipped id (its slot is the default's), the
 * first/last overlay-only item of its group, or an unknown id.
 */
export function moveItem(o: OverlayPut, harness: string, id: string, dir: -1 | 1): OverlayPut | null {
  if (defaultItemIds(harness).has(id)) return null;
  const out = cloneOverlay(o);
  const hit = findItem(out, harness, id);
  if (!hit) return null;
  const shipped = defaultItemIds(harness);
  const items = hit.group.items;
  let j = hit.index + dir;
  while (j >= 0 && j < items.length && shipped.has(items[j].id)) j += dir;
  if (j < 0 || j >= items.length) return null;
  [items[hit.index], items[j]] = [items[j], items[hit.index]];
  return out;
}

export function counts(o: OverlayPut): { groups: number; items: number; hidden: number } {
  let groups = 0;
  let items = 0;
  for (const row of Object.values(o.harnesses)) {
    groups += row.groups.length;
    for (const g of row.groups) items += g.items.length;
  }
  let hidden = 0;
  for (const ids of Object.values(o.hidden)) hidden += ids.length;
  return { groups, items, hidden };
}

/** True when two overlays are equal after normalisation (the server's no-op test). */
export function sameOverlay(a: OverlayPut, b: OverlayPut): boolean {
  return JSON.stringify(prune(a)) === JSON.stringify(prune(b));
}

// ---------------------------------------------------------------------------
// validation (the shared validator + the `hidden` shape the server also checks)
// ---------------------------------------------------------------------------

function hiddenErrors(v: unknown): CmdError[] {
  const out: CmdError[] = [];
  if (typeof v !== "object" || v === null || Array.isArray(v)) {
    out.push({ code: "bad-type", path: "/hidden" });
    return out;
  }
  for (const k of Object.keys(v).sort()) {
    const path = `/hidden/${k.replace(/~/g, "~0").replace(/\//g, "~1")}`;
    if (!(HARNESS_NAMES as readonly string[]).includes(k)) {
      out.push({ code: "unknown-harness", path });
      continue;
    }
    const arr = (v as Record<string, unknown>)[k];
    if (!Array.isArray(arr)) {
      out.push({ code: "bad-type", path });
      continue;
    }
    if (arr.length > MAX_HIDDEN) out.push({ code: "too-many", path });
    arr.forEach((id, i) => {
      if (typeof id !== "string") out.push({ code: "bad-type", path: `${path}/${i}` });
      else if (!ID_RE.test(id)) out.push({ code: "bad-id", path: `${path}/${i}` });
    });
  }
  return out;
}

/** Every defect of an overlay-shaped value, in the validator's order; `[]` = valid. */
export function validateOverlay(v: unknown): CmdError[] {
  if (typeof v !== "object" || v === null || Array.isArray(v)) return [{ code: "bad-type", path: "" }];
  const root = v as Record<string, unknown>;
  const errs = validateCatalogue({ schema: CMD_SCHEMA, harnesses: root.harnesses ?? {} });
  errs.push(...hiddenErrors(root.hidden ?? {}));
  for (const k of Object.keys(root).filter((x) => x !== "harnesses" && x !== "hidden").sort()) {
    errs.push({ code: "unknown-field", path: `/${k.replace(/~/g, "~0").replace(/\//g, "~1")}` });
  }
  return errs;
}

// ---------------------------------------------------------------------------
// the item form
// ---------------------------------------------------------------------------

export interface ArgField {
  name: string;
  hint: string;
  default: string;
}

export interface ItemFields {
  label: string;
  kind: ItemKind;
  text: string;
  mode: "paste" | "run";
  description: string;
  keys: string[];
  args: ArgField[];
  unverified: boolean;
}

export const BLANK_FIELDS: ItemFields = {
  label: "",
  kind: "command",
  text: "",
  mode: "paste",
  description: "",
  keys: [],
  args: [],
  unverified: false,
};

export function fieldsFromItem(i: OvItem): ItemFields {
  return {
    label: i.label,
    kind: (i.kind === "chord" || i.kind === "reply" ? i.kind : "command") as ItemKind,
    text: i.text ?? "",
    mode: i.mode === "run" ? "run" : "paste",
    description: i.description ?? "",
    keys: [...(i.keys ?? [])],
    args: (i.args ?? []).map((a) => ({ name: a.name, hint: a.hint, default: a.default ?? "" })),
    unverified: i.unverified === true,
  };
}

/** The item the form describes. Only fields the kind allows are emitted. */
export function itemFromFields(id: string, f: ItemFields): OvItem {
  const item: OvItem = { id, kind: f.kind, label: f.label };
  if (f.description !== "") item.description = f.description;
  if (f.kind === "chord") {
    item.keys = [...f.keys];
  } else {
    item.text = f.text;
    item.mode = f.mode;
    if (f.args.length > 0) {
      item.args = f.args.map((a) => (a.default !== "" ? { name: a.name, hint: a.hint, default: a.default } : { name: a.name, hint: a.hint }));
    }
  }
  if (f.unverified) item.unverified = true;
  return item;
}

/** A newline in paste text can press Enter when bracketed paste is off. */
export function pasteNewlineWarning(f: ItemFields): boolean {
  return f.kind !== "chord" && f.mode === "paste" && /[\r\n]/.test(f.text);
}

export const KEY_CHOICES: readonly string[] = KEY_NAMES;
