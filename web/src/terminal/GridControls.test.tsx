// @vitest-environment jsdom
// FUTERMFIT — the desktop Fit / Release controls: the web outranks a local
// attach (no confirm), a confirm survives only for any other owner class, the
// runner's own sentence reaches the status line, and nothing but a click posts.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { GridControls } from "./GridControls";
import type { ConfirmFn } from "./phone/driveApi";
import { byId, click, maybeById, mount, rigFetch, settle, type FetchRig, type Mounted } from "./phone/driveTestkit";

const confirmMock = vi.fn<ConfirmFn>(async () => true);
vi.mock("../components/ConfirmProvider", () => ({ useConfirm: () => confirmMock }));
const toastErr = vi.fn();
vi.mock("../lib/toast", () => ({ toast: { err: (m: string) => toastErr(m), ok: vi.fn(), info: vi.fn() } }));

let m: Mounted | null = null;
let rig: FetchRig;
let qc: QueryClient;
const said: string[] = [];

beforeEach(() => {
  rig = rigFetch();
  confirmMock.mockReset();
  confirmMock.mockResolvedValue(true);
  toastErr.mockReset();
  said.length = 0;
  owners.length = 0;
  qc = new QueryClient();
});

afterEach(() => {
  m?.unmount();
  m = null;
  vi.unstubAllGlobals();
});

// 0.6 x 1.3 cell: 1400 / 8.4 = 166 cols, 800 / 18.2 = 43 rows at the 14px reference.
const MEASURE = { width: 1400, height: 800, cellW: 0.6, cellH: 1.3 };

const owners: Array<string | null> = [];

function controls(owner: string | null, measure: () => typeof MEASURE | null = () => MEASURE) {
  m = mount(
    <QueryClientProvider client={qc}>
      <GridControls
        termId="abcd1234"
        harness="omp"
        viewer="ws-abc1234567"
        owner={owner}
        measureWindow={measure}
        say={(s) => said.push(s)}
        onOwner={(o) => owners.push(o)}
      />
    </QueryClientProvider>,
  );
}

const fit = async (): Promise<void> => {
  click(byId(m!.host, "kb-term-fit"));
  await settle(10);
};

describe("Fit to window", () => {
  it("posts one take-size with the measured cols and rows and the viewer, and never asks, for a free grid", async () => {
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    controls(null);
    await fit();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
    expect(rig.calls[0].json).toEqual({ cols: 166, rows: 43, viewer: "ws-abc1234567" });
    expect(confirmMock).not.toHaveBeenCalled();
    expect(said).toEqual(["Grid set to 166x43."]);
  });

  it("owner local:9 — no confirm, a take-size is posted, and the status line says it took the grid", async () => {
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    controls("local:9");
    await fit();
    expect(confirmMock).not.toHaveBeenCalled();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
    expect(said).toEqual(["Took the grid from the local attach; it now watches (166x43)."]);
  });

  it("owner kb-term (another web client) — the latest explicit Fit wins, no confirm", async () => {
    controls("kb-term");
    await fit();
    expect(confirmMock).not.toHaveBeenCalled();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
  });

  it("an unknown owner class asks first; cancelling posts nothing", async () => {
    confirmMock.mockResolvedValue(false);
    controls("desk-1");
    await fit();
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(confirmMock.mock.calls[0][0]).toMatchObject({ body: "Resizing rebuilds the screen for every viewer." });
    expect(rig.calls).toHaveLength(0);
    expect(said).toEqual([]);
  });

  it("an unknown owner class goes ahead once confirmed", async () => {
    controls("desk-1");
    await fit();
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
  });

  it("a 409 puts the runner's sentence on the status line verbatim, with no toast", async () => {
    rig.reply(409, { type: "urn:kb:errors:terminal-conflict", detail: "grid is owned by desk-1" });
    controls("kb-term");
    await fit();
    expect(said).toEqual(["grid is owned by desk-1"]);
    expect(toastErr).not.toHaveBeenCalled();
  });

  it("a network error is said on the status line AND raised as a toast", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new TypeError("offline"))));
    controls(null);
    await fit();
    expect(said).toEqual(["Could not reach the terminal"]);
    expect(toastErr).toHaveBeenCalledWith("Could not reach the terminal");
  });

  it("reports the runner's named owner after a take (the list may be unavailable), and none after a refusal", async () => {
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    controls(null);
    await fit();
    expect(owners).toEqual(["kb-term"]);
    m!.unmount();
    m = null;
    owners.length = 0;
    rig.reply(409, { type: "urn:kb:errors:terminal-conflict", detail: "grid is owned by desk-1" });
    controls(null);
    await fit();
    expect(owners).toEqual([]);
  });

  it("refreshes the terms list after a take", async () => {
    const spy = vi.spyOn(qc, "invalidateQueries");
    controls(null);
    await fit();
    expect(spy).toHaveBeenCalledWith({ queryKey: ["terms"] });
  });

  it("an unmeasurable window posts nothing and says so", async () => {
    controls(null, () => null);
    await fit();
    expect(rig.calls).toHaveLength(0);
    expect(said).toEqual(["Cannot measure the window yet."]);
  });

  it("mounting and re-rendering send nothing: only the click posts", async () => {
    controls("local:9");
    await settle();
    expect(rig.calls).toHaveLength(0);
  });
});

describe("Release grid", () => {
  it("appears only while the web owns the grid", () => {
    for (const owner of [null, "", "local:9", "desk-1"]) {
      controls(owner);
      expect(maybeById(m!.host, "kb-term-release")).toBeNull();
      m!.unmount();
      m = null;
    }
    controls("kb-term");
    expect(maybeById(m!.host, "kb-term-release")).not.toBeNull();
  });

  it("posts release-size with an empty body (the route rejects unknown fields) and says so", async () => {
    controls("kb-term");
    click(byId(m!.host, "kb-term-release"));
    await settle(10);
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/release-size"]);
    expect(rig.calls[0].json).toEqual({});
    expect(said).toEqual(["Released the grid."]);
    expect(owners).toEqual([null]);
    expect(confirmMock).not.toHaveBeenCalled();
  });

  it("a refused release shows the runner's sentence", async () => {
    rig.reply(409, { type: "urn:kb:errors:terminal-conflict", detail: "grid is owned by local:9" });
    controls("kb-term");
    click(byId(m!.host, "kb-term-release"));
    await settle(10);
    expect(said).toEqual(["grid is owned by local:9"]);
  });
});
