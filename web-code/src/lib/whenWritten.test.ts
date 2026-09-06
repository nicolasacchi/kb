import { describe, expect, it } from "vitest";
import type { CodeLensRef, CodeLensWhenWritten } from "../api/types";
import { whenWrittenBadge } from "./whenWritten";

function ref(overrides: Partial<CodeLensRef>): CodeLensRef {
  return {
    ordinal: 1,
    group: null,
    kind: "path_line",
    raw: "a.rb:1",
    declared: false,
    path_hint: "a.rb",
    line_hint: 1,
    line_hint_end: null,
    symbol_container: null,
    symbol_member: null,
    context: null,
    path_state: "present",
    resolved_path: "a.rb",
    candidate_count: 1,
    candidates: ["a.rb"],
    issue: null,
    line_state: "confirmed",
    line_evidence: "context_token",
    confirm_token: null,
    token_line: null,
    resolved_line: null,
    resolved_line_end: null,
    line_hint_delta: null,
    file_lines: 10,
    line_reason: null,
    remap: null,
    symbol_state: "no_symbol",
    symbol_hit_count: 0,
    symbol_hits: [],
    spans: [],
    reader: null,
    search: null,
    note: null,
    when_written: null,
    ...overrides,
  };
}

function ww(overrides: Partial<CodeLensWhenWritten>): CodeLensWhenWritten {
  return { path_state_at_rev: "present", line_state_at_rev: "confirmed", ...overrides };
}

describe("whenWrittenBadge", () => {
  it("null when when_written is absent — the caller never opted in, or the doc has no usable rev", () => {
    expect(whenWrittenBadge(ref({ when_written: null }))).toBe(null);
  });

  it("'wrong when written' when the path never existed at the declared rev, even if it confirms NOW", () => {
    expect(
      whenWrittenBadge(
        ref({
          path_state: "present",
          line_state: "confirmed",
          when_written: ww({ path_state_at_rev: "absent", line_state_at_rev: "absent" }),
        }),
      ),
    ).toBe("wrong-when-written");
  });

  it("'rotted since' when it was confirmed at the declared rev but the current tree honestly drifted", () => {
    expect(
      whenWrittenBadge(
        ref({
          line_state: "drifted",
          when_written: ww({ path_state_at_rev: "present", line_state_at_rev: "confirmed" }),
        }),
      ),
    ).toBe("rotted-since");
  });

  it("no badge when both verdicts agree — confirmed then, confirmed now", () => {
    expect(
      whenWrittenBadge(
        ref({
          line_state: "confirmed",
          when_written: ww({ path_state_at_rev: "present", line_state_at_rev: "confirmed" }),
        }),
      ),
    ).toBe(null);
  });

  it("no THIRD label is invented for combinations outside the two named cases", () => {
    // Present at the rev but unverifiable there (an imprecise citation from
    // the start) — the design calls for exactly two derived labels, not a
    // "was already unverifiable" one.
    expect(
      whenWrittenBadge(
        ref({
          line_state: "unverifiable",
          when_written: ww({ path_state_at_rev: "present", line_state_at_rev: "unverifiable" }),
        }),
      ),
    ).toBe(null);
    // Confirmed at the rev, still confirmed now, but via a different
    // evidence path than the badge cares about — no signal to add.
    expect(
      whenWrittenBadge(
        ref({
          line_state: "absent",
          when_written: ww({ path_state_at_rev: "present", line_state_at_rev: "unverifiable" }),
        }),
      ),
    ).toBe(null);
  });
});
