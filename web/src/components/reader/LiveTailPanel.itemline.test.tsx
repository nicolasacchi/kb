import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { ItemLine } from "./LiveTailPanel";
import type { Item } from "../../api/generated/Item";

describe("ItemLine Image", () => {
  it("renders the blob label form", () => {
    const html = renderToString(
      <ItemLine
        item={{
          item: "Image",
          mime: "image/png",
          sha256:
            "abababababababababababababababababababababababababababababababab",
          inline_bytes: null,
          width: null,
          height: null,
        }}
      />,
    );
    expect(html).toContain("[image image/png blob:abababababab]");
    expect(html).toContain("kb-livetail__image");
  });

  it("renders the inline label form", () => {
    const html = renderToString(
      <ItemLine
        item={{
          item: "Image",
          mime: "image/png",
          sha256: null,
          inline_bytes: 4,
          width: null,
          height: null,
        }}
      />,
    );
    expect(html).toContain("[image image/png inline 4 b64 chars]");
  });

  it("renders the bare mime-only label form", () => {
    const html = renderToString(
      <ItemLine
        item={{
          item: "Image",
          mime: "image/webp",
          sha256: null,
          inline_bytes: null,
          width: null,
          height: null,
        }}
      />,
    );
    expect(html).toContain("[image image/webp]");
  });

  it("renders nothing for an unknown item kind", () => {
    const item = { item: "Bogus" } as unknown as Item;
    expect(renderToString(<ItemLine item={item} />)).toBe("");
  });
});
