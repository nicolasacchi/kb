import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Probe } from "./Probe";

describe("ui Probe component", () => {
  it("renders under the importing app's React via react-dom/server", () => {
    const html = renderToString(<Probe label="hello" />);
    expect(html).toContain("data-kb-ui-probe");
    expect(html).toContain(">hello</span>");
  });
});
