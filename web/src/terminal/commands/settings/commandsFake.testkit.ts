// CMDSETTINGS - an in-memory stand-in for `/api/terms/commands` for jsdom
// tests (not a test file). It merges with the real `mergeCatalogue`, validates
// with the real validator, and honours If-Match like the daemon does.
import { vi } from "vitest";
import type { EffectiveCatalogue } from "../../../api/generated/EffectiveCatalogue";
import type { OverlayPut } from "../../../api/generated/OverlayPut";
import { effectiveCatalogue } from "../catalog";
import { prune, validateOverlay } from "./overlayOps";

export interface FakeCall {
  method: string;
  ifMatch: string | null;
  intent: string | null;
  body: unknown;
}

export interface Fake {
  calls: FakeCall[];
  state: { rev: number; overlay: OverlayPut };
  /** Another client changed the overlay (bumps the rev, no event). */
  external(overlay: OverlayPut): void;
  /** Answer the next mutation with this response instead. */
  force(status: number, body: unknown): void;
  puts(): FakeCall[];
  deletes(): FakeCall[];
}

export function effectiveBody(rev: number, overlay: OverlayPut): EffectiveCatalogue {
  const eff = effectiveCatalogue(overlay);
  return { rev, harnesses: eff.harnesses, overlay, hidden: overlay.hidden } as unknown as EffectiveCatalogue;
}

const json = (status: number, body: unknown, type = "application/json") =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": type } });

export function installFakeCommands(init?: { rev?: number; overlay?: OverlayPut }): Fake {
  const fake: Fake = {
    calls: [],
    state: { rev: init?.rev ?? 0, overlay: init?.overlay ?? { harnesses: {}, hidden: {} } },
    external(overlay) {
      fake.state = { rev: fake.state.rev + 1, overlay: prune(overlay) };
    },
    force(status, body) {
      forced = { status, body };
    },
    puts: () => fake.calls.filter((c) => c.method === "PUT"),
    deletes: () => fake.calls.filter((c) => c.method === "DELETE"),
  };
  let forced: { status: number; body: unknown } | null = null;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: string | URL, init2?: RequestInit) => {
      const url = String(input);
      if (!url.endsWith("/api/terms/commands")) return json(404, {});
      const method = init2?.method ?? "GET";
      const h = new Headers(init2?.headers);
      const body = init2?.body ? JSON.parse(String(init2.body)) : null;
      fake.calls.push({ method, ifMatch: h.get("If-Match"), intent: h.get("X-Kb-Intent"), body });
      if (method === "GET") return json(200, effectiveBody(fake.state.rev, fake.state.overlay));
      if (forced) {
        const f = forced;
        forced = null;
        return json(f.status, f.body, "application/problem+json");
      }
      const seen = Number((h.get("If-Match") ?? "").replace(/"/g, ""));
      if (seen !== fake.state.rev) {
        return json(
          412,
          { type: "urn:kb:errors:commands-stale", title: "Precondition Failed", status: 412, detail: "stale", rev: fake.state.rev },
          "application/problem+json",
        );
      }
      if (method === "DELETE") {
        const empty = Object.keys(fake.state.overlay.harnesses).length + Object.keys(fake.state.overlay.hidden).length === 0;
        if (!empty) fake.state = { rev: fake.state.rev + 1, overlay: { harnesses: {}, hidden: {} } };
        return json(200, effectiveBody(fake.state.rev, fake.state.overlay));
      }
      const errs = validateOverlay(body);
      if (errs.length > 0) {
        return json(
          400,
          { type: "urn:kb:errors:bad-catalogue", title: "Bad Request", status: 400, detail: "defects", errors: errs },
          "application/problem+json",
        );
      }
      const next = prune(body as OverlayPut);
      if (JSON.stringify(next) !== JSON.stringify(prune(fake.state.overlay))) {
        fake.state = { rev: fake.state.rev + 1, overlay: next };
      }
      return json(200, effectiveBody(fake.state.rev, fake.state.overlay));
    }),
  );
  return fake;
}
