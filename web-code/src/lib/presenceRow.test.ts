import { describe, expect, it } from "vitest";
import { PROV_HUES } from "@kb/ui/themes/derive";
import { attribute, buildMarks, rollup, type PresenceFileWire, type PresenceSnapshot } from "./presence";
import { rowPresence } from "./presenceRow";
import { provHueIndex, provHueStyle } from "./provHue";

const f = (o: Partial<PresenceFileWire>): PresenceFileWire => ({
  repo: "r",
  path: "src/a.rs",
  session_id: "3f2a9c",
  harness: "claude",
  state: "working",
  age_secs: 0,
  ...o,
});
const snap = (files: PresenceFileWire[]): PresenceSnapshot => ({
  schema: "kbc-presence/1",
  available: true,
  now_unix: 0,
  files,
  fetchedPerf: 100_000,
});
const marks = (paths: string[], files: PresenceFileWire[]) =>
  buildMarks(
    attribute(
      paths.map((path) => ({ repo: "r", path, at: 100_000 })),
      snap(files),
    ),
    () => undefined,
    110_000,
  );

describe("rowPresence", () => {
  it("an exact file row: solid class, level, sid, aria text and the session hue", () => {
    const m = marks(["src/a.rs"], [f({})]);
    const r = rowPresence(m.get("src/a.rs"), undefined, false)!;
    expect(r.className).toBe("kbc-tree__row--presence kbc-tree__row--presence-exact");
    expect(r.level).toBe(3);
    expect(r.sid).toBe("3f2a9c");
    expect(r.label).toBe("Edited by claude session 3f2a 10 s ago");
    expect(r.count).toBeUndefined();
    expect(r.style).toEqual(provHueStyle("3f2a9c"));
    expect(provHueIndex("3f2a9c")).toBeLessThan(PROV_HUES);
  });
  it("a candidate row is dotted-class and says 'probably'", () => {
    const m = marks(["src/z.rs"], [f({})]);
    const r = rowPresence(m.get("src/z.rs"), undefined, false)!;
    expect(r.className).toContain("presence-candidate");
    expect(r.label).toMatch(/^Probably claude session 3f2a/);
  });
  it("a neutral row claims no session and sets no hue", () => {
    const m = marks(["src/z.rs"], []);
    const r = rowPresence(m.get("src/z.rs"), undefined, false)!;
    expect(r.trust).toBe("neutral");
    expect(r.sid).toBeUndefined();
    expect(r.style).toEqual({});
    expect(r.label).toBe("Changed 10 s ago");
  });
  it("a collapsed dir shows the roll-up with a count; an expanded one nothing", () => {
    const m = marks(["d/a.rs", "d/b.rs"], [f({ path: "d/a.rs" })]);
    const dirs = rollup(m);
    const closed = rowPresence(undefined, dirs.get("d"), false)!;
    expect(closed.count).toBe(2);
    expect(rowPresence(undefined, dirs.get("d"), true)).toBeNull();
  });
  it("no mark, no decoration", () => {
    expect(rowPresence(undefined, undefined, false)).toBeNull();
  });
});
