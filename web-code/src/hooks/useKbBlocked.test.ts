import { describe, expect, it } from "vitest";
import { KB_BLOCKED_KEY, KB_BLOCKED_POLL_MS, kbBlockedQueryOptions } from "./useKbBlocked";

describe("useKbBlocked polling contract (OL IDEA1)", () => {
  it("keys under [kb, blocked] and polls every 15 s with a matching staleTime", () => {
    expect(KB_BLOCKED_KEY).toEqual(["kb", "blocked"]);
    expect(KB_BLOCKED_POLL_MS).toBe(15_000);
    expect(kbBlockedQueryOptions.queryKey).toEqual(["kb", "blocked"]);
    expect(kbBlockedQueryOptions.staleTime).toBe(15_000);
    expect(kbBlockedQueryOptions.refetchInterval).toBe(15_000);
  });

  it("never opts into background polling (TanStack's default is off)", () => {
    expect("refetchIntervalInBackground" in kbBlockedQueryOptions).toBe(false);
  });
});
