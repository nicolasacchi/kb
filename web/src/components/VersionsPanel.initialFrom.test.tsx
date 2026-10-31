// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import type { Version } from "../api/versions";

const versions: Version[] = [
  { ref: "WORKING", source: "working", label: "", author: "", ts_unix: 300, short: "wt" },
  { ref: "index:3", source: "index", label: "", author: "", ts_unix: 200, short: "#3" },
  { ref: "index:2", source: "index", label: "", author: "", ts_unix: 100, short: "#2" },
];

vi.mock("../hooks/useVersions", () => ({
  useVersions: () => ({ versions, mode: "index", loading: false, error: null }),
}));
vi.mock("../api/versions", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/versions")>();
  return { ...actual, fetchDiff: vi.fn(async () => ({ from: "", to: "WORKING", mode: "text", hunks: [] })) };
});
vi.mock("../api/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api/client")>();
  return { ...actual, fetchArtifactHtml: vi.fn(async () => "") };
});

import { fetchDiff } from "../api/versions";
import VersionsPanel from "./VersionsPanel";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const lastFrom = () => {
  const calls = vi.mocked(fetchDiff).mock.calls;
  return calls[calls.length - 1]?.[2];
};

describe("VersionsPanel initialFrom", () => {
  it("diffs from initialFrom.ref when it names a known version", async () => {
    render(<VersionsPanel kb="k" id="a" initialFrom={{ ref: "index:2", nonce: 1 }} />);
    await waitFor(() => expect(fetchDiff).toHaveBeenCalled());
    expect(lastFrom()).toBe("index:2");
  });

  it("falls back to the newest non-working version for an unknown ref", async () => {
    render(<VersionsPanel kb="k" id="a" initialFrom={{ ref: "index:999", nonce: 1 }} />);
    await waitFor(() => expect(fetchDiff).toHaveBeenCalled());
    expect(lastFrom()).toBe("index:3");
  });

  it("re-applies the selection when the nonce changes", async () => {
    const { rerender } = render(
      <VersionsPanel kb="k" id="a" initialFrom={{ ref: "index:2", nonce: 1 }} />,
    );
    await waitFor(() => expect(lastFrom()).toBe("index:2"));
    rerender(<VersionsPanel kb="k" id="a" initialFrom={{ ref: "index:3", nonce: 2 }} />);
    await waitFor(() => expect(lastFrom()).toBe("index:3"));
  });
});
