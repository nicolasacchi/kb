// @vitest-environment jsdom
//
// v0.44 X2 — the in-iframe annotator posts to the parent's exact origin,
// never "*" when it can know better: the configured __KB_PARENT_ORIGIN first,
// else location.ancestorOrigins[0], "*" only as the Firefox fallback.
import { afterEach, describe, expect, it, vi } from "vitest";

const w = window as unknown as Record<string, unknown>;

function envelope() {
  w.__KB_COMMENTS = {
    v: 1,
    etag: null,
    file: {
      schema: "kb-comments/1",
      artifact: { id: "art1", title: "", kb: "k" },
      generatedAt: "2026-01-01T00:00:00Z",
      comments: [],
    },
  };
}

afterEach(() => {
  document.body.innerHTML = "";
  delete w.__KB_COMMENTS;
  delete w.__KB_PARENT_ORIGIN;
  delete (window.location as unknown as Record<string, unknown>)
    .ancestorOrigins;
  vi.restoreAllMocks();
  vi.resetModules();
});

async function probeTarget(): Promise<unknown> {
  const spy = vi.spyOn(window.parent, "postMessage");
  envelope();
  vi.resetModules();
  await import("./annotate");
  const call = spy.mock.calls.find(
    (c) => (c[0] as { type?: string }).type === "cm:probe",
  );
  expect(call).toBeDefined();
  return call![1];
}

describe("annotate.ts post() targetOrigin", () => {
  it("uses the configured parent origin", async () => {
    w.__KB_PARENT_ORIGIN = "https://spa.example";
    expect(await probeTarget()).toBe("https://spa.example");
  });

  it("uses ancestorOrigins[0] when the configured target is '*'", async () => {
    w.__KB_PARENT_ORIGIN = "*";
    Object.defineProperty(window.location, "ancestorOrigins", {
      value: ["https://spa.example"],
      configurable: true,
    });
    expect(await probeTarget()).toBe("https://spa.example");
  });

  it("falls back to '*' only when nothing better is known", async () => {
    expect(await probeTarget()).toBe("*");
  });
});
