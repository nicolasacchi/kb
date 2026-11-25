// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useNotifyTerms } from "./useNotifyTerms";

function Probe({ admitted }: { admitted: boolean }) {
  useNotifyTerms(admitted);
  return null;
}
function mount(admitted: boolean) {
  const qc = new QueryClient();
  return render(
    <QueryClientProvider client={qc}>
      <Probe admitted={admitted} />
    </QueryClientProvider>,
  );
}
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("useNotifyTerms", () => {
  it("never fetches /api/terms when the caller is not admitted (or identity unknown)", async () => {
    const f = vi.fn(async () => new Response("{}", { status: 403 }));
    vi.stubGlobal("fetch", f);
    mount(false);
    window.dispatchEvent(new Event("focus"));
    await new Promise((r) => setTimeout(r, 30));
    expect(f).not.toHaveBeenCalled();
  });
  it("fetches /api/terms once when admitted", async () => {
    const f = vi.fn(async () => new Response(JSON.stringify({ terms: [] }), { status: 200 }));
    vi.stubGlobal("fetch", f);
    mount(true);
    await waitFor(() => expect(f).toHaveBeenCalledTimes(1));
    expect(String((f.mock.calls[0] as unknown[])[0])).toContain("/api/terms");
  });
});
