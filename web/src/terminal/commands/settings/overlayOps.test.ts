import { describe, expect, it } from "vitest";
import type { OverlayPut } from "../../../api/generated/OverlayPut";
import { effectiveCatalogue, forHarness } from "../catalog";
import {
  BLANK_FIELDS,
  EMPTY_OVERLAY,
  counts,
  hideDefault,
  itemFromFields,
  moveItem,
  newGroupId,
  newItemId,
  originOf,
  pasteNewlineWarning,
  prune,
  removeItem,
  resetItem,
  sameOverlay,
  unhide,
  upsertItem,
  validateOverlay,
} from "./overlayOps";

const item = (id: string, label = id) => ({ id, kind: "reply", label, text: label });
const ids = (o: OverlayPut, h = "claude") => forHarness(effectiveCatalogue(o), h).map((e) => e.item.id);

describe("overlay builders", () => {
  it("never emit an effective-only field", () => {
    let o = upsertItem(EMPTY_OVERLAY, "claude", "slash", "Slash commands", { ...item("help", "Mine") });
    o = hideDefault(o, "claude", "compact");
    o = upsertItem(o, "claude", "g-x", "X", item("u-a"));
    expect(JSON.stringify(o)).not.toContain("origin");
    expect(Object.keys(o).sort()).toEqual(["harnesses", "hidden"]);
  });

  it("upsert adds to a named group, replaces in place, and creates the group", () => {
    let o = upsertItem(EMPTY_OVERLAY, "claude", "g-mine", "Mine", item("u-a"));
    expect(o.harnesses.claude.groups).toEqual([{ id: "g-mine", label: "Mine", items: [item("u-a")] }]);
    o = upsertItem(o, "claude", "other", "Other", item("u-a", "renamed"));
    expect(o.harnesses.claude.groups).toHaveLength(1);
    expect(o.harnesses.claude.groups[0].items[0].label).toBe("renamed");
  });

  it("a default edited becomes an overlay item with the same id: changed, in its default slot", () => {
    const before = ids(EMPTY_OVERLAY);
    const o = upsertItem(EMPTY_OVERLAY, "claude", "slash", "Slash commands", item("help", "Mine"));
    expect(ids(o)).toEqual(before);
    expect(originOf("claude", "help", "overlay")).toBe("changed");
    expect(originOf("claude", "u-a", "overlay")).toBe("added");
    expect(originOf("claude", "help", "default")).toBe("default");
  });

  it("hide drops the replacement and lists the id; unhide and reset undo it", () => {
    let o = upsertItem(EMPTY_OVERLAY, "claude", "slash", "Slash commands", item("help", "Mine"));
    o = hideDefault(o, "claude", "help");
    expect(o.harnesses.claude).toBeUndefined();
    expect(o.hidden.claude).toEqual(["help"]);
    expect(ids(o)).not.toContain("help");
    expect(sameOverlay(unhide(o, "claude", "help"), EMPTY_OVERLAY)).toBe(true);
    expect(sameOverlay(resetItem(o, "claude", "help"), EMPTY_OVERLAY)).toBe(true);
  });

  it("removeItem prunes an emptied group and harness row", () => {
    const o = upsertItem(EMPTY_OVERLAY, "omp", "g", "G", item("u-a"));
    expect(removeItem(o, "omp", "u-a")).toEqual(EMPTY_OVERLAY);
  });

  it("move reorders only overlay-only items and the effective order follows", () => {
    let o = upsertItem(EMPTY_OVERLAY, "claude", "slash", "Slash commands", item("u-a"));
    o = upsertItem(o, "claude", "slash", "Slash commands", item("u-b"));
    o = upsertItem(o, "claude", "slash", "Slash commands", item("help", "Mine"));
    const order = ids(o);
    expect(order.indexOf("u-a")).toBeLessThan(order.indexOf("u-b"));
    const moved = moveItem(o, "claude", "u-b", -1);
    expect(moved).not.toBeNull();
    const after = ids(moved as OverlayPut);
    expect(after.indexOf("u-b")).toBeLessThan(after.indexOf("u-a"));
    // First added item cannot move up; a shipped id never moves; unknown id neither.
    expect(moveItem(o, "claude", "u-a", -1)).toBeNull();
    expect(moveItem(o, "claude", "u-b", 1)).toBeNull();
    expect(moveItem(o, "claude", "help", 1)).toBeNull();
    expect(moveItem(o, "claude", "nope", 1)).toBeNull();
  });

  it("ordering is stable: hidden is sorted and unique, equal overlays compare equal", () => {
    let o = hideDefault(EMPTY_OVERLAY, "claude", "help");
    o = hideDefault(o, "claude", "clear");
    o = hideDefault(o, "claude", "help");
    expect(o.hidden.claude).toEqual(["clear", "help"]);
    expect(sameOverlay(o, prune(o))).toBe(true);
  });

  it("new ids are valid and never collide with a shipped or overlay id", () => {
    const o = upsertItem(EMPTY_OVERLAY, "claude", "g", "G", item("u-help"));
    const a = newItemId("claude", o, "help");
    expect(a).toBe("u-help-2");
    expect(newItemId("claude", EMPTY_OVERLAY, "  !!  ")).toBe("u-item");
    expect(newGroupId(["g-mine"], "Mine")).toBe("g-mine-2");
    expect(validateOverlay(upsertItem(EMPTY_OVERLAY, "claude", newGroupId([], "A b"), "A b", item(a)))).toEqual([]);
  });

  it("counts the overlay", () => {
    const o = hideDefault(upsertItem(EMPTY_OVERLAY, "claude", "g", "G", item("u-a")), "omp", "tree");
    expect(counts(o)).toEqual({ groups: 1, items: 1, hidden: 1 });
  });
});

describe("item form helpers", () => {
  it("emit only the fields a kind allows", () => {
    const chord = itemFromFields("u-c", { ...BLANK_FIELDS, label: "Esc", kind: "chord", keys: ["Esc"], text: "stale" });
    expect(chord).toEqual({ id: "u-c", kind: "chord", label: "Esc", keys: ["Esc"] });
    const cmd = itemFromFields("u-x", {
      ...BLANK_FIELDS,
      label: "/x",
      text: "/x {n}",
      args: [{ name: "n", hint: "count", default: "" }],
    });
    expect(cmd).toEqual({ id: "u-x", kind: "command", label: "/x", text: "/x {n}", mode: "paste", args: [{ name: "n", hint: "count" }] });
  });

  it("warns on a newline only for paste text", () => {
    expect(pasteNewlineWarning({ ...BLANK_FIELDS, text: "a\nb" })).toBe(true);
    expect(pasteNewlineWarning({ ...BLANK_FIELDS, text: "a\nb", mode: "run" })).toBe(false);
    expect(pasteNewlineWarning({ ...BLANK_FIELDS, text: "ab" })).toBe(false);
  });

  it("validateOverlay lists every defect, hidden included", () => {
    const e = validateOverlay({
      harnesses: { claude: { groups: [{ id: "g", label: "x", items: [{ id: "a", kind: "command", label: "y", text: "a\nb", mode: "run" }] }] } },
      hidden: { nope: [], claude: ["BAD"] },
      extra: 1,
    });
    expect(e.map((d) => d.code)).toEqual(["run-multiline", "bad-id", "unknown-harness", "unknown-field"]);
    expect(validateOverlay([])).toEqual([{ code: "bad-type", path: "" }]);
  });
});
