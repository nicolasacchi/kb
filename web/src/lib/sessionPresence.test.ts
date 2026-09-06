import { describe, expect, it } from "vitest";
import {
  ACTIVE_WINDOW_SECS,
  presenceLiveSet,
  sessionPresence,
  sessionPresenceStatus,
  presenceSummary,
  formatPresenceSummary,
} from "./sessionPresence";

const NOW_MS = 1_800_000_000_000; // fixed instant
const NOW_SECS = Math.floor(NOW_MS / 1000);

describe("sessionPresence", () => {
  it("is active just under the window", () => {
    const row = { ended_at: NOW_SECS - (ACTIVE_WINDOW_SECS - 1) };
    const p = sessionPresence(row, NOW_MS);
    expect(p.status).toBe("active");
    expect(p.copy).toContain("active — as of capture");
    expect(p.copy).toContain("ago");
  });

  it("is idle exactly at and past the window", () => {
    expect(sessionPresence({ ended_at: NOW_SECS - ACTIVE_WINDOW_SECS }, NOW_MS).status).toBe(
      "idle",
    );
    expect(
      sessionPresence({ ended_at: NOW_SECS - ACTIVE_WINDOW_SECS - 1 }, NOW_MS).status,
    ).toBe("idle");
  });

  it("idle copy never claims 'active'", () => {
    const p = sessionPresence({ ended_at: NOW_SECS - 3600 }, NOW_MS);
    expect(p.status).toBe("idle");
    expect(p.copy).toBe("as of capture · 1h ago");
    expect(p.copy).not.toContain("active");
  });

  it("uses the honest 'as of capture' phrase, never implying liveness", () => {
    const p = sessionPresence({ ended_at: NOW_SECS - 30 }, NOW_MS);
    expect(p.copy).toContain("as of capture");
    expect(p.copy).not.toMatch(/live|writing now/i);
  });

  it("a just-landed capture (<5s) reads 'just now', not '0s ago'", () => {
    const p = sessionPresence({ ended_at: NOW_SECS - 1 }, NOW_MS);
    expect(p.copy).toBe("active — as of capture · just now");
  });

  it("a future ended_at (clock skew) clamps to non-negative age and stays active", () => {
    const p = sessionPresence({ ended_at: NOW_SECS + 1000 }, NOW_MS);
    expect(p.status).toBe("active");
    expect(p.copy).toContain("just now");
  });

  it("sessionPresenceStatus returns just the status", () => {
    expect(sessionPresenceStatus({ ended_at: NOW_SECS - 10 }, NOW_MS)).toBe("active");
    expect(sessionPresenceStatus({ ended_at: NOW_SECS - 7200 }, NOW_MS)).toBe("idle");
  });

  it("defaults `now` to the real clock when omitted", () => {
    const recent = Math.floor(Date.now() / 1000);
    const p = sessionPresence({ ended_at: recent });
    expect(p.status).toBe("active");
  });
});

// W7/LF-1 — Tier-1 (evidence-based) upgrades Tier-0.
describe("sessionPresence — Tier-1 presence-set upgrade", () => {
  it("a presence-set hit upgrades a Tier-0 idle row to live", () => {
    const row = { session_id: "sid-1", ended_at: NOW_SECS - 7200 }; // Tier-0: idle
    const set = presenceLiveSet([{ session_id: "sid-1" }]);
    const p = sessionPresence(row, NOW_MS, set);
    expect(p.status).toBe("live");
    expect(p.copy).toBe("live — writing now");
  });

  it("a presence-set hit upgrades a Tier-0 active row to live too", () => {
    const row = { session_id: "sid-1", ended_at: NOW_SECS - 10 }; // Tier-0: active
    const set = presenceLiveSet([{ session_id: "sid-1" }]);
    expect(sessionPresence(row, NOW_MS, set).status).toBe("live");
  });

  it("a row NOT in the presence set is unaffected (falls back to Tier-0)", () => {
    const row = { session_id: "sid-2", ended_at: NOW_SECS - 10 };
    const set = presenceLiveSet([{ session_id: "sid-1" }]);
    const p = sessionPresence(row, NOW_MS, set);
    expect(p.status).toBe("active");
    expect(p.copy).toContain("as of capture");
  });

  it("a row with no session_id can never match the presence set", () => {
    const row = { ended_at: NOW_SECS - 10 };
    const set = presenceLiveSet([{ session_id: "sid-1" }]);
    expect(sessionPresence(row, NOW_MS, set).status).toBe("active");
  });

  it("an empty/absent presence set never promotes anything to live", () => {
    const row = { session_id: "sid-1", ended_at: NOW_SECS - 10 };
    expect(sessionPresence(row, NOW_MS).status).toBe("active");
    expect(sessionPresence(row, NOW_MS, presenceLiveSet([])).status).toBe(
      "active",
    );
    expect(sessionPresence(row, NOW_MS, presenceLiveSet(undefined)).status).toBe(
      "active",
    );
  });

  it("sessionPresenceStatus also honors the presence set", () => {
    const row = { session_id: "sid-1", ended_at: NOW_SECS - 7200 };
    const set = presenceLiveSet([{ session_id: "sid-1" }]);
    expect(sessionPresenceStatus(row, NOW_MS, set)).toBe("live");
  });

  it("pre-W7 call sites (no presence set arg) are byte-unchanged", () => {
    // Every existing caller — SessionListRow, SessionContextCard,
    // PreviewInspector — calls sessionPresence(row) with no third arg.
    const row = { ended_at: NOW_SECS - 10 };
    expect(sessionPresence(row, NOW_MS).status).toBe("active");
  });
});

// LF-1/S2 — Presence summary for strip counts and project cards
describe("presenceSummary", () => {
  it("counts live and active rows correctly", () => {
    const rows = [
      { session_id: "sid-1", ended_at: NOW_SECS - 10 }, // tier-0: active
      { session_id: "sid-2", ended_at: NOW_SECS - 7200 }, // tier-0: idle
      { session_id: "sid-3", ended_at: NOW_SECS - 100 }, // tier-0: active
    ];
    const set = presenceLiveSet([{ session_id: "sid-1" }]);
    const summary = presenceSummary(rows, NOW_MS, set);
    expect(summary.live).toBe(1); // sid-1 is in the presence set
    expect(summary.active).toBe(1); // sid-3 is tier-0 active
  });

  it("handles empty rows", () => {
    const summary = presenceSummary([]);
    expect(summary.live).toBe(0);
    expect(summary.active).toBe(0);
  });

  it("handles all idle rows", () => {
    const rows = [
      { ended_at: NOW_SECS - 7200 },
      { ended_at: NOW_SECS - 3600 },
    ];
    const summary = presenceSummary(rows, NOW_MS);
    expect(summary.live).toBe(0);
    expect(summary.active).toBe(0);
  });

  it("counts all live when all rows are in the presence set", () => {
    const rows = [
      { session_id: "sid-1", ended_at: NOW_SECS - 7200 }, // would be idle without presence
      { session_id: "sid-2", ended_at: NOW_SECS - 10 }, // would be active
    ];
    const set = presenceLiveSet([
      { session_id: "sid-1" },
      { session_id: "sid-2" },
    ]);
    const summary = presenceSummary(rows, NOW_MS, set);
    expect(summary.live).toBe(2);
    expect(summary.active).toBe(0);
  });
});

describe("formatPresenceSummary", () => {
  it("formats both live and active", () => {
    const summary = { live: 3, active: 2 };
    expect(formatPresenceSummary(summary)).toBe("3 live · 2 active");
  });

  it("formats live only", () => {
    const summary = { live: 2, active: 0 };
    expect(formatPresenceSummary(summary)).toBe("2 live");
  });

  it("formats active only", () => {
    const summary = { live: 0, active: 5 };
    expect(formatPresenceSummary(summary)).toBe("5 active");
  });

  it("returns 'idle' when both are zero", () => {
    const summary = { live: 0, active: 0 };
    expect(formatPresenceSummary(summary)).toBe("idle");
  });
});
