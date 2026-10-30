// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

const recordOpen = vi.fn(() =>
  Promise.resolve({ visit_id: 7, scroll_y: 0, reading: null }),
);
vi.mock("../../api/history", async () => {
  const actual =
    await vi.importActual<typeof import("../../api/history")>(
      "../../api/history",
    );
  return { ...actual, recordOpen: (...a: unknown[]) => recordOpen(...(a as [])) };
});

import { useArtifactVisit } from "./ArtifactPane";

function Probe({ enabled }: { enabled?: boolean }) {
  useArtifactVisit("k", "a1", enabled);
  return null;
}

afterEach(() => {
  cleanup();
  recordOpen.mockClear();
});

describe("useArtifactVisit enabled flag (OL TIME2)", () => {
  it("never calls history.open when enabled=false (a historical render is not a read)", () => {
    render(<Probe enabled={false} />);
    expect(recordOpen).not.toHaveBeenCalled();
  });

  it("calls history.open once when enabled (default)", () => {
    render(<Probe />);
    expect(recordOpen).toHaveBeenCalledTimes(1);
  });
});
