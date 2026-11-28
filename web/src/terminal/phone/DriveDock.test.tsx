// @vitest-environment jsdom
// T18 — the dock: "Fit to phone" is the ONLY thing that resizes the pty, and it
// asks first only when another viewer owns the grid.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TermRow } from "../../api/terms";
import { DriveDock } from "./DriveDock";
import type { ConfirmFn } from "./driveApi";
import type { PhoneMeasure } from "./fitPhone";
import { byId, click, maybeById, mount, rigFetch, settle, typeInto, type FetchRig, type Mounted } from "./driveTestkit";

const confirmMock = vi.fn<ConfirmFn>(async () => true);
vi.mock("../../components/ConfirmProvider", () => ({ useConfirm: () => confirmMock }));
vi.mock("../../hooks/useLiveSessions", () => ({ useLiveSessions: () => ({ rows: [], loading: false }) }));

let m: Mounted | null = null;
let rig: FetchRig;

beforeEach(() => {
  rig = rigFetch();
  confirmMock.mockReset();
  confirmMock.mockResolvedValue(true);
});

afterEach(() => {
  m?.unmount();
  m = null;
  vi.unstubAllGlobals();
});

const MEASURE: PhoneMeasure = { visibleWidth: 390, visibleHeight: 400, cellWidth: 7.2, cellHeight: 17 };

const row = (over: Partial<TermRow> = {}): TermRow =>
  ({
    id: "abcd1234",
    harness: "omp",
    bracket_paste_flag: true,
    grid_owner: null,
    session_id: "sid-1",
    ...over,
  }) as TermRow;

function dock(
  r: TermRow | undefined,
  measure: () => PhoneMeasure | null = () => MEASURE,
  extra: {
    ownerHint?: { owner: string | null } | null;
    onOwner?: (o: string | null) => void;
    grid?: { cols: number; rows: number } | null;
    compact?: boolean;
    ownerKnown?: boolean;
  } = {},
) {
  const qc = new QueryClient();
  m = mount(
    <QueryClientProvider client={qc}>
      <DriveDock termId="abcd1234" harness="omp" viewer="ws-abc1234567" row={r} measure={measure} {...extra} />
    </QueryClientProvider>,
  );
}

const fit = async (): Promise<void> => {
  click(byId(m!.host, "kb-drive-fit"));
  await settle(10);
};

describe("DriveDock: Fit to phone", () => {
  it("sends exactly one take-size with the measured cols and rows and the viewer", async () => {
    rig.reply(200, { cols: 54, rows: 23, owner: "kb-term" });
    dock(row());
    await fit();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
    expect(rig.calls[0].json).toEqual({ cols: 54, rows: 23, viewer: "ws-abc1234567" });
    expect(confirmMock).not.toHaveBeenCalled();
    expect(byId(m!.host, "kb-drive-caption").textContent).toBe("Grid set to 54x23.");
  });

  it("asks first when another viewer owns the grid, and a refusal sends nothing", async () => {
    confirmMock.mockResolvedValue(false);
    dock(row({ grid_owner: "desk-1" }));
    await fit();
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(confirmMock.mock.calls[0][0]).toMatchObject({ body: "Resizing rebuilds the screen for every viewer." });
    expect(rig.calls).toHaveLength(0);
  });

  it("goes ahead after the operator confirms, and shows a 409 grid-owned sentence verbatim", async () => {
    rig.reply(409, { type: "urn:kb:errors:terminal-conflict", detail: "grid is owned by desk-1" });
    dock(row({ grid_owner: "desk-1" }));
    await fit();
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
    expect(byId(m!.host, "kb-drive-caption").textContent).toBe("grid is owned by desk-1");
  });

  it("does not ask when the web already owns the grid, and the chip names the REAL grid and owner", async () => {
    dock(row({ grid_owner: "kb-term", cols: 120, rows: 40 }), () => MEASURE, { grid: { cols: 120, rows: 40 } });
    expect(byId(m!.host, "kb-drive-grid-chip").textContent).toBe("120x40 · web");
    await fit();
    expect(confirmMock).not.toHaveBeenCalled();
    expect(rig.calls).toHaveLength(1);
  });

  it("owner local:9 — no confirm (the web outranks a local attach), take-size posted, and it says so", async () => {
    rig.reply(200, { cols: 54, rows: 23, owner: "kb-term" });
    dock(row({ grid_owner: "local:9" }));
    await fit();
    expect(confirmMock).not.toHaveBeenCalled();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
    expect(byId(m!.host, "kb-drive-caption").textContent).toBe(
      "Took the grid from the local attach; it now watches (54x23).",
    );
  });

  it("Release is shown only for owner kb-term, next to Fit, and posts release-size with an empty body", async () => {
    dock(row({ grid_owner: "local:9" }));
    expect(maybeById(m!.host, "kb-drive-release")).toBeNull();
    m!.unmount();
    dock(row({ grid_owner: "kb-term" }));
    click(byId(m!.host, "kb-drive-release"));
    await settle(10);
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/release-size"]);
    expect(rig.calls[0].json).toEqual({});
    expect(byId(m!.host, "kb-drive-caption").textContent).toBe("Released the grid.");
    expect(confirmMock).not.toHaveBeenCalled();
  });

  it("the runner's named owner outranks a list that cannot say (no row): Release shows, and a take reports it", async () => {
    dock(undefined, () => MEASURE, { ownerHint: { owner: "kb-term" } });
    expect(maybeById(m!.host, "kb-drive-release")).not.toBeNull();
    m!.unmount();
    const seen: Array<string | null> = [];
    rig.reply(200, { cols: 54, rows: 23, owner: "kb-term" });
    dock(row(), () => MEASURE, { onOwner: (o) => seen.push(o) });
    await fit();
    expect(seen).toEqual(["kb-term"]);
  });

  it("W6b: the chip is the desktop chip's words (one helper): free, a local attach, and no suffix while the row is behind the live grid", () => {
    dock(row({ cols: 120, rows: 40 }), () => MEASURE, { grid: { cols: 120, rows: 40 } });
    expect(byId(m!.host, "kb-drive-grid-chip").textContent).toBe("120x40 · free");
    m!.unmount();
    dock(row({ grid_owner: "local:9", cols: 120, rows: 40 }), () => MEASURE, { grid: { cols: 120, rows: 40 } });
    expect(byId(m!.host, "kb-drive-grid-chip").textContent).toBe("120x40 · local attach");
    m!.unmount();
    // Another viewer took 54x23; the list row still describes 120x40.
    dock(row({ grid_owner: null, cols: 120, rows: 40 }), () => MEASURE, { grid: { cols: 54, rows: 23 } });
    expect(byId(m!.host, "kb-drive-grid-chip").textContent).toBe("54x23 · checking");
  });

  it("FUMINPHONE: while the owner is unknown the chip says so and Release stays hidden until the row is vouched", () => {
    // The row still describes 120x40 and says the web owns it; the live grid is 54x23.
    dock(row({ grid_owner: "kb-term", cols: 120, rows: 40 }), () => MEASURE, { grid: { cols: 54, rows: 23 } });
    const chip = byId(m!.host, "kb-drive-grid-chip");
    expect(chip.textContent).toBe("54x23 · checking");
    expect(chip.getAttribute("data-owner")).toBe("checking");
    expect(maybeById(m!.host, "kb-drive-release")).toBeNull();
    m!.unmount();
    // No row at all (the list has not loaded): unknown as well.
    dock(undefined, () => MEASURE, { grid: { cols: 54, rows: 23 } });
    expect(byId(m!.host, "kb-drive-grid-chip").textContent).toBe("54x23 · checking");
    expect(maybeById(m!.host, "kb-drive-release")).toBeNull();
    m!.unmount();
    // Vouched: the row matches the live grid, the chip names the owner and Release shows.
    dock(row({ grid_owner: "kb-term", cols: 54, rows: 23 }), () => MEASURE, { grid: { cols: 54, rows: 23 } });
    expect(byId(m!.host, "kb-drive-grid-chip").textContent).toBe("54x23 · web");
    expect(maybeById(m!.host, "kb-drive-release")).not.toBeNull();
  });

  it("no chip before the live grid is known", () => {
    dock(row());
    expect(maybeById(m!.host, "kb-drive-grid-chip")).toBeNull();
  });

  it("W6f: in full screen the grid-controls row folds away; keys and composer stay", () => {
    dock(row({ grid_owner: "kb-term", cols: 120, rows: 40 }), () => MEASURE, { grid: { cols: 120, rows: 40 }, compact: true });
    expect(maybeById(m!.host, "kb-drive-fit")).toBeNull();
    expect(maybeById(m!.host, "kb-drive-release")).toBeNull();
    expect(maybeById(m!.host, "kb-drive-grid-chip")).toBeNull();
    expect(maybeById(m!.host, "kb-drive-keybar")).not.toBeNull();
    m!.unmount();
    dock(row(), () => MEASURE, { compact: false });
    expect(maybeById(m!.host, "kb-drive-fit")).not.toBeNull();
  });

  it("sends nothing when the screen cannot be measured", async () => {
    dock(row(), () => null);
    await fit();
    expect(rig.calls).toHaveLength(0);
    expect(byId(m!.host, "kb-drive-caption").textContent).toBe("Cannot measure the screen yet.");
  });

  it("nothing but the explicit action resizes: mounting, focusing and typing send no take-size or resize", async () => {
    dock(row());
    const ta = byId(m!.host, "kb-drive-input") as HTMLTextAreaElement;
    ta.focus();
    ta.dispatchEvent(new FocusEvent("focus"));
    ta.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    typeInto(ta, "half a message");
    window.dispatchEvent(new Event("resize"));
    (window.visualViewport ?? window).dispatchEvent(new Event("resize"));
    await settle();
    expect(rig.calls).toHaveLength(0);
  });
});

describe("DriveDock: composition", () => {
  it("passes the pane's bracketed-paste flag to the composer", async () => {
    dock(row({ bracket_paste_flag: false }));
    typeInto(byId(m!.host, "kb-drive-input") as HTMLTextAreaElement, "a\nb");
    click(byId(m!.host, "kb-drive-send"));
    await settle();
    expect(rig.calls).toHaveLength(0);
  });

  it("an unbound terminal says Enter is disabled", () => {
    dock(row({ session_id: null }));
    expect(byId(m!.host, "kb-drive-caption").textContent).toBe("No session bound: Enter is disabled");
  });

  it("has no choice bar while nothing is blocked", () => {
    dock(row());
    expect(maybeById(m!.host, "kb-drive-choicebar")).toBeNull();
    expect(maybeById(m!.host, "kb-drive-keybar")).not.toBeNull();
  });
});

// BPHONE (e): while the stream is not live nobody can vouch for an owner.
describe("DriveDock: the owner chip while not live", () => {
  const GRID = { cols: 120, rows: 40 };

  it("ownerKnown=false: the chip says the size only, marked unknown, and Release is not offered", () => {
    dock(row({ grid_owner: "kb-term", cols: 120, rows: 40 } as Partial<TermRow>), () => MEASURE, { grid: GRID, ownerKnown: false });
    const chip = byId(m!.host, "kb-drive-grid-chip");
    expect(chip.textContent).toBe("120x40");
    expect(chip.getAttribute("data-owner")).toBe("unknown");
    expect(maybeById(m!.host, "kb-drive-release")).toBeNull();
  });

  it("ownerKnown defaults to true: a web-owned grid still offers Release and names its owner", () => {
    dock(row({ grid_owner: "kb-term", cols: 120, rows: 40 } as Partial<TermRow>), () => MEASURE, { grid: GRID });
    expect(byId(m!.host, "kb-drive-grid-chip").getAttribute("data-owner")).toBe("web");
    expect(maybeById(m!.host, "kb-drive-release")).not.toBeNull();
  });
});
