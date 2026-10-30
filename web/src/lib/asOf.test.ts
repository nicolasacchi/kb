import { describe, it, expect } from "vitest";
import {
  artifactSrcAt,
  asOfCaption,
  atFromLocalInput,
  clampAt,
  localInputFromAt,
} from "./asOf";
import { artifactHref } from "./artifactHref";
import { parseAtParam, resolveMemento } from "./memento";
import type { Version } from "../api/versions";

const v = (ref: string, source: Version["source"], ts_unix: number): Version => ({
  ref,
  source,
  label: "",
  author: "",
  ts_unix,
  short: ref,
});
const fmt = (ts: number) => `T${ts}`;

describe("clampAt", () => {
  it("never asks for the future, and truncates a float", () => {
    expect(clampAt(2_000, 1_000)).toBe(1_000);
    expect(clampAt(500.9, 1_000)).toBe(500);
  });
});

describe("datetime-local <-> unix", () => {
  it("round-trips a local minute", () => {
    const at = atFromLocalInput("2026-07-30T09:15", 4_000_000_000)!;
    expect(localInputFromAt(at)).toBe("2026-07-30T09:15");
  });
  it("clamps a picked instant in the future to now", () => {
    expect(atFromLocalInput("2999-01-01T00:00", 1_700_000_000)).toBe(1_700_000_000);
  });
  it("is total: malformed and impossible dates are null", () => {
    for (const bad of ["", "yesterday", "2026-07-30", "2026-02-30T10:00", "2026-13-01T10:00"]) {
      expect(atFromLocalInput(bad, 4_000_000_000)).toBeNull();
    }
  });
});

describe("artifactSrcAt", () => {
  const base = "http://abc.artifacts.localhost:4000/";
  it("appends only ?at= (never cm=on) for a historical entry", () => {
    expect(artifactSrcAt(base, 1234.7, "")).toBe(`${base}?at=1234`);
    expect(artifactSrcAt(base, 1234, "")).not.toContain("cm=on");
  });
  it("leaves the plain URL when not historical or on a sub-page", () => {
    expect(artifactSrcAt(base, null, "")).toBe(base);
    expect(artifactSrcAt(`${base}p2.html`, 1234, "p2.html")).toBe(`${base}p2.html`);
  });
});

describe("the ?at= round trip through the reader URL", () => {
  it("artifactHref writes what parseAtParam reads, before pane2", () => {
    const href = artifactHref("kb", "a/b.html", { at: 1700000000, pane2: "kb:a%2Fb.html" });
    const qs = new URLSearchParams(href.split("?")[1]);
    expect(parseAtParam(qs.get("at"))).toBe(1700000000);
    expect(href.indexOf("at=")).toBeLessThan(href.indexOf("pane2="));
  });
  it("a malformed at degrades to the ordinary reader", () => {
    expect(parseAtParam("yesterday")).toBeNull();
  });
});

describe("asOfCaption", () => {
  const timeline = [v("WORKING", "working", 300), v("index:2", "index", 200), v("index:1", "index", 100)];
  it("names the nearest-earlier relation and the sibling caveat", () => {
    const c = asOfCaption(resolveMemento(timeline, 250), fmt);
    expect(c).toBe(
      "as of T250 — nearest earlier version (T200); relative links resolve to today's siblings",
    );
  });
  it("calls an exact second exact", () => {
    expect(asOfCaption(resolveMemento(timeline, 200), fmt)).toContain("exact version");
  });
  it("says so when the instant resolves to the working tree", () => {
    expect(asOfCaption(resolveMemento(timeline, 9999), fmt)).toContain("today's file");
  });
  it("a miss names the floor and never degrades to the oldest", () => {
    const c = asOfCaption(resolveMemento(timeline, 50), fmt);
    expect(c).toBe("nothing is that old: no version before T100");
    expect(c).not.toContain("relative links");
  });
  it("no versions at all", () => {
    expect(asOfCaption(resolveMemento([], 50), fmt)).toContain("no recorded versions");
  });
});
