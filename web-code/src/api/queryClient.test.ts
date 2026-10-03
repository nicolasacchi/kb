// @vitest-environment jsdom
//
// v0.44 X2 (K4 carry-over, A9.f2) - the identity probe is refreshed when the
// SSE stream reconnects: `review_mutations_admitted` is computed per request,
// and `["identity"]` is `staleTime: Infinity`, so without this a caller whose
// admission flipped stays disabled (or enabled) until a full reload.
import { describe, expect, it, vi } from "vitest";

class FakeEventSource {
  static last: FakeEventSource | null = null;
  onopen: (() => void) | null = null;
  constructor(public url: string) {
    FakeEventSource.last = this;
  }
  addEventListener() {}
  close() {}
}

describe("SSE bridge reconnect", () => {
  it("marks the identity query stale so it is re-fetched", async () => {
    vi.stubGlobal("EventSource", FakeEventSource);
    const { queryClient, startSseInvalidationBridge } = await import("./queryClient");
    queryClient.setQueryData(["identity"], { review_mutations_admitted: false });
    expect(queryClient.getQueryState(["identity"])?.isInvalidated).toBe(false);

    startSseInvalidationBridge();
    const src = FakeEventSource.last;
    expect(src?.onopen).toBeTypeOf("function");
    src!.onopen!();

    expect(queryClient.getQueryState(["identity"])?.isInvalidated).toBe(true);
    vi.unstubAllGlobals();
  });
});
