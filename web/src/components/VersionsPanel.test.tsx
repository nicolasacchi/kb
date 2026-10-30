import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import VersionsPanel from "./VersionsPanel";

function render(props: Partial<React.ComponentProps<typeof VersionsPanel>>) {
  const qc = new QueryClient();
  return renderToString(
    <QueryClientProvider client={qc}>
      <VersionsPanel kb="k" id="abc123abc123" {...props} />
    </QueryClientProvider>,
  );
}

describe("VersionsPanel — Open as of…", () => {
  it("renders the date control when the route can act on it", () => {
    const html = render({ onOpenAsOf: () => {} });
    expect(html).toContain('data-testid="versions-open-as-of"');
    expect(html).toContain('type="datetime-local"');
    expect(html).toContain("Open as of");
  });

  it("is absent without a handler (a pane with no ?at= of its own)", () => {
    expect(render({})).not.toContain("versions-open-as-of");
  });

  it("seeds the input from the current ?at=", () => {
    const html = render({ onOpenAsOf: () => {}, at: 1_700_000_000 });
    // 2023-11-14 in every timezone's local date near that instant.
    expect(html).toMatch(/value="2023-11-1[345]T\d\d:\d\d"/);
  });
});
