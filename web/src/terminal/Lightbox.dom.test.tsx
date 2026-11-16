// @vitest-environment jsdom
//
// T15 - the picture lightbox (jsdom). jsdom has no CloseWatcher, so the Esc
// tests below prove the explicit keydown handler works on its own.
import { afterEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { renderToString } from "react-dom/server";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Lightbox } from "./Lightbox";
import { MediaCache } from "./gfx/media";
import type { OpenPicture } from "./gfx/attach";

const SHA = "c".repeat(64);
const PIC: OpenPicture = { id: 5, sha: SHA, mime: "image/png", w: 40, h: 20 };

function cache(): MediaCache {
  return new MediaCache({
    fetchImpl: (() => Promise.resolve(new Response(new Uint8Array(4), { status: 200 }))) as typeof fetch,
    createObjectURL: () => "blob:test/1",
    revokeObjectURL: () => {},
    decode: () => Promise.resolve(),
    sleep: () => Promise.resolve(),
  });
}

function Harness({ media }: { media: MediaCache }) {
  const [pic, setPic] = useState<OpenPicture | null>(null);
  return (
    <div>
      <button type="button" data-testid="opener" onClick={() => setPic(PIC)}>
        open
      </button>
      <Lightbox picture={pic} media={media} onClose={() => setPic(null)} />
    </div>
  );
}

afterEach(() => cleanup());

describe("Lightbox", () => {
  it("opens with focus on the dialog and shows the decoded image", async () => {
    const media = cache();
    render(<Harness media={media} />);
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(screen.getByTestId("opener"));
    const dialog = screen.getByRole("dialog");
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-label")).toBe("Picture");
    expect(document.activeElement).toBe(dialog);
    await waitFor(() => expect(screen.getByAltText("Terminal picture")).toBeTruthy());
    expect(screen.getByAltText("Terminal picture").getAttribute("src")).toBe("blob:test/1");
  });

  it("Esc closes and restores focus", async () => {
    expect("CloseWatcher" in window).toBe(false);
    const media = cache();
    render(<Harness media={media} />);
    const opener = screen.getByTestId("opener");
    opener.focus();
    fireEvent.click(opener);
    const dialog = screen.getByRole("dialog");
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(opener);
  });

  it("backdrop click closes, a click inside the image does not", async () => {
    const media = cache();
    render(<Harness media={media} />);
    fireEvent.click(screen.getByTestId("opener"));
    await waitFor(() => expect(screen.getByAltText("Terminal picture")).toBeTruthy());
    fireEvent.click(screen.getByAltText("Terminal picture"));
    expect(screen.queryByRole("dialog")).not.toBeNull();
    fireEvent.click(screen.getByRole("dialog"));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("the close button closes", () => {
    const media = cache();
    render(<Harness media={media} />);
    fireEvent.click(screen.getByTestId("opener"));
    fireEvent.click(screen.getByLabelText("Close picture"));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("unpins the cache entry on close", () => {
    const media = cache();
    const pin = vi.spyOn(media, "pin");
    const unpin = vi.spyOn(media, "unpin");
    render(<Harness media={media} />);
    fireEvent.click(screen.getByTestId("opener"));
    expect(pin).toHaveBeenCalledWith(SHA);
    expect(unpin).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(unpin).toHaveBeenCalledWith(SHA);
    expect(unpin).toHaveBeenCalledTimes(1);
  });

  it("renders to a string without a picture and with one", () => {
    const media = cache();
    expect(renderToString(<Lightbox picture={null} media={media} onClose={() => {}} />)).toBe("");
    const html = renderToString(<Lightbox picture={PIC} media={media} onClose={() => {}} />);
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toContain("kbgfx-lightbox");
  });
});
