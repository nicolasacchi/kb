// @vitest-environment jsdom
//
// T21 - the Images strip: chronological merge without sha dedupe, the
// unadmitted tier (no media request, caption tiles), a 404 degrading to the
// caption, the lightbox, and blob-URL revocation on unmount.
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  buildTiles,
  ImagesStripView,
  NOT_STORED_CAPTION,
  type TerminalImage,
} from "./ImagesStrip";
import type { TranscriptImageRef } from "./sessionImages";

const SHA_A = "a".repeat(64);
const SHA_B = "b".repeat(64);

const ref = (
  sha: string | null,
  turn: string,
  ts: string | null,
): TranscriptImageRef => ({ mime: "image/png", sha256: sha, turnId: turn, ts });

const term = (sha: string, seenAt: number): TerminalImage => ({
  sha256: sha,
  w: 10,
  h: 10,
  seenAt,
});

function opts(fetchImpl: typeof fetch, revoked: string[] = []) {
  let n = 0;
  return {
    fetchImpl,
    createObjectURL: () => `blob:test/${(n += 1)}`,
    revokeObjectURL: (u: string) => {
      revoked.push(u);
    },
    decode: () => Promise.resolve(),
    sleep: () => Promise.resolve(),
  };
}

const ok = (): Response => new Response(new Uint8Array(8), { status: 200 });
const gone = (): Response => new Response(null, { status: 404 });

afterEach(() => cleanup());

describe("buildTiles", () => {
  it("merges both sources chronologically and keeps two entries with one sha", () => {
    const t1 = Date.parse("2026-07-01T09:00:00Z");
    const t2 = Date.parse("2026-07-01T09:10:00Z");
    const t3 = Date.parse("2026-07-01T09:20:00Z");
    const tiles = buildTiles(
      [term(SHA_A, t2), term(SHA_A, t3)],
      [ref(SHA_A, "t-000000000001", "2026-07-01T09:00:00Z"), ref(null, "t-000000000002", null)],
      true,
    );
    expect(tiles.map((t) => [t.source, t.at])).toEqual([
      ["transcript", t1],
      ["terminal", t2],
      ["terminal", t3],
      ["transcript", null],
    ]);
    expect(tiles.filter((t) => t.sha === SHA_A)).toHaveLength(3);
    expect(new Set(tiles.map((t) => t.key)).size).toBe(4);
  });

  it("drops terminal pictures when the tier is not admitted", () => {
    const tiles = buildTiles([term(SHA_A, 1)], [ref(SHA_B, "t-000000000001", null)], false);
    expect(tiles.map((t) => t.source)).toEqual(["transcript"]);
  });
});

describe("ImagesStripView", () => {
  it("not admitted: no media request, caption tiles with turn links, terminal source absent", () => {
    const fetchImpl = vi.fn(() => Promise.resolve(ok()));
    render(
      <ImagesStripView
        admitted={false}
        refs={[ref(SHA_A, "t-0123456789ab", "2026-07-01T09:00:00Z"), ref(null, "t-ba9876543210", null)]}
        terminalImages={[term(SHA_B, 5)]}
        mediaOptions={opts(fetchImpl as unknown as typeof fetch)}
      />,
    );
    const tiles = screen.getAllByTestId("kb-imgs-tile");
    expect(tiles).toHaveLength(2);
    expect(screen.getAllByTestId("kb-imgs-caption").map((c) => c.textContent)).toEqual([
      NOT_STORED_CAPTION,
      NOT_STORED_CAPTION,
    ]);
    expect(tiles.every((t) => t.getAttribute("data-source") === "transcript")).toBe(true);
    expect(screen.getAllByTestId("kb-imgs-turn").map((a) => a.getAttribute("href"))).toEqual([
      "#t-0123456789ab",
      "#t-ba9876543210",
    ]);
    expect(document.querySelector("img")).toBeNull();
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("admitted and the store answers 404: the caption tile, never an image", async () => {
    const fetchImpl = vi.fn(() => Promise.resolve(gone()));
    render(
      <ImagesStripView
        admitted
        refs={[ref(SHA_A, "t-0123456789ab", null)]}
        mediaOptions={opts(fetchImpl as unknown as typeof fetch)}
      />,
    );
    await waitFor(() => expect(screen.getByTestId("kb-imgs-caption")).toBeTruthy());
    expect(document.querySelector("img")).toBeNull();
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    const [url, init] = fetchImpl.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe(`/api/terms/media/${SHA_A}`);
    expect((init.headers as Record<string, string>)["X-Kb-Intent"]).toBe("terminal");
  });

  it("a transcript ref without a sha never asks the store", async () => {
    const fetchImpl = vi.fn(() => Promise.resolve(ok()));
    render(
      <ImagesStripView
        admitted
        refs={[ref(null, "t-0123456789ab", null)]}
        mediaOptions={opts(fetchImpl as unknown as typeof fetch)}
      />,
    );
    expect(screen.getByTestId("kb-imgs-caption")).toBeTruthy();
    await Promise.resolve();
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("admitted with bytes: a thumbnail whose tap opens the lightbox", async () => {
    const fetchImpl = vi.fn(() => Promise.resolve(ok()));
    render(
      <ImagesStripView
        admitted
        refs={[ref(SHA_A, "t-0123456789ab", null)]}
        terminalImages={[term(SHA_B, 1)]}
        mediaOptions={opts(fetchImpl as unknown as typeof fetch)}
      />,
    );
    await waitFor(() => expect(screen.getAllByTestId("kb-imgs-open")).toHaveLength(2));
    expect(screen.queryByTestId("kb-term-lightbox")).toBeNull();
    fireEvent.click(screen.getAllByTestId("kb-imgs-open")[0]);
    await waitFor(() => expect(screen.getByTestId("kb-term-lightbox")).toBeTruthy());
  });

  it("unmount revokes every blob URL it minted", async () => {
    const revoked: string[] = [];
    const fetchImpl = vi.fn(() => Promise.resolve(ok()));
    const { unmount } = render(
      <ImagesStripView
        admitted
        refs={[ref(SHA_A, "t-0123456789ab", null), ref(SHA_B, "t-ba9876543210", null)]}
        mediaOptions={opts(fetchImpl as unknown as typeof fetch, revoked)}
      />,
    );
    await waitFor(() => expect(screen.getAllByTestId("kb-imgs-open")).toHaveLength(2));
    expect(revoked).toEqual([]);
    unmount();
    expect([...revoked].sort()).toEqual(["blob:test/1", "blob:test/2"]);
  });

  it("an empty strip says so", () => {
    render(<ImagesStripView admitted={false} refs={[]} />);
    expect(screen.getByTestId("kb-imgs-empty")).toBeTruthy();
  });

  it("renders on the server without touching the browser", () => {
    const html = renderToString(
      <ImagesStripView admitted={false} refs={[ref(SHA_A, "t-0123456789ab", null)]} />,
    );
    expect(html).toContain(NOT_STORED_CAPTION);
  });
});
