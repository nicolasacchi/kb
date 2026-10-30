// OL IDEA3 — the pure copy and layout helpers around the server's evidence
// sentences.
import { describe, expect, it } from "vitest";
import type { SessionEvidenceOut } from "../api/sessions";
import {
  CHECKS_CAPTION,
  checkWhen,
  ignoredEditsNote,
  notNamedLine,
  resumeEvidenceLines,
  rowEvidence,
  segmentCaption,
} from "./sessionEvidence";

const ev = (over: Partial<SessionEvidenceOut>): SessionEvidenceOut => ({
  verdict: "verified",
  phrase: "cargo test passed after the last edit",
  deciding: {
    label: "cargo test",
    cmd: "cargo test -p widget",
    runners: ["cargo-test"],
    outcome: "pass",
    text: "passed",
  },
  checks_total: 1,
  checks_after_edit: 1,
  edits_ignored: 0,
  uncommitted: false,
  ...over,
});

describe("rowEvidence", () => {
  it("carries the server's phrase and a `$ cmd` title for every row verdict", () => {
    const r = rowEvidence(ev({}));
    expect(r).toEqual({
      verdict: "verified",
      phrase: "cargo test passed after the last edit",
      title: "$ cargo test -p widget",
    });
    for (const verdict of ["failing", "unconfirmed", "stale", "unchecked"] as const) {
      expect(rowEvidence(ev({ verdict }))?.verdict).toBe(verdict);
    }
  });

  it("is silent for no_edits, not_extracted, a missing phrase and no evidence", () => {
    expect(rowEvidence(ev({ verdict: "no_edits" }))).toBeNull();
    expect(rowEvidence(ev({ verdict: "not_extracted" }))).toBeNull();
    expect(rowEvidence(ev({ phrase: undefined }))).toBeNull();
    expect(rowEvidence(ev({ phrase: "   " }))).toBeNull();
    expect(rowEvidence(undefined)).toBeNull();
    expect(rowEvidence(null)).toBeNull();
  });

  it("has no title when the verdict rests on no single check", () => {
    const r = rowEvidence(
      ev({ verdict: "unchecked", phrase: "edited, no checks ran", deciding: undefined }),
    );
    expect(r?.title).toBeUndefined();
  });
});

describe("captions and lines", () => {
  it("states what the evidence can and cannot see", () => {
    expect(CHECKS_CAPTION).toBe(
      "From this session's main transcript: edits are Edit/Write tool calls (edits made through Bash or by subagents are not seen); checks are a fixed list of test, lint and build commands.",
    );
  });

  it("counts the edits it ignored", () => {
    expect(ignoredEditsNote(0)).toBe("");
    expect(ignoredEditsNote(1)).toBe(" · 1 edit to agent or scratch files not counted");
    expect(ignoredEditsNote(4)).toBe(" · 4 edits to agent or scratch files not counted");
    expect(ignoredEditsNote(Number.NaN)).toBe("");
  });

  it("captions a segmented session's part, and only that", () => {
    expect(segmentCaption(2, 3)).toBe("part 2 of 3: this part's checks and edits only");
    expect(segmentCaption(undefined, 2)).toBe("part 1 of 2: this part's checks and edits only");
    expect(segmentCaption(undefined, undefined)).toBeNull();
    expect(segmentCaption(1, 1)).toBeNull();
  });

  it("names the edits the closing text never mentions", () => {
    expect(notNamedLine(["a.rs", "b.ts"], 2)).toBe("Not named in the closing text: a.rs, b.ts");
    expect(notNamedLine(["a.rs", "b.ts"], 5)).toBe(
      "Not named in the closing text: a.rs, b.ts (+3 more)",
    );
    expect(notNamedLine([], 0)).toBeNull();
    expect(notNamedLine(undefined, undefined)).toBeNull();
  });

  it("places a check before or after the last edit", () => {
    expect(checkWhen({ after_last_edit: true })).toBe("after the last edit");
    expect(checkWhen({ after_last_edit: false })).toBe("before the last edit");
    expect(checkWhen({})).toBe("");
  });
});

describe("resumeEvidenceLines", () => {
  it("mirrors the CLI resume block", () => {
    expect(resumeEvidenceLines(ev({ uncommitted: true }))).toEqual([
      "Checks: cargo test passed after the last edit",
      "  $ cargo test -p widget",
      "Not committed: no commit detected in this session",
    ]);
    expect(resumeEvidenceLines(ev({}))).toEqual([
      "Checks: cargo test passed after the last edit",
      "  $ cargo test -p widget",
    ]);
  });

  it("is silent without edits or evidence, and honest about a missing extraction", () => {
    expect(resumeEvidenceLines(undefined)).toEqual([]);
    expect(resumeEvidenceLines(ev({ verdict: "no_edits", phrase: undefined }))).toEqual([]);
    expect(
      resumeEvidenceLines(ev({ verdict: "not_extracted", phrase: undefined, deciding: undefined })),
    ).toEqual(["Checks: not extracted for this capture"]);
    expect(
      resumeEvidenceLines(
        ev({ verdict: "unchecked", phrase: "edited, no checks ran", deciding: undefined }),
      ),
    ).toEqual(["Checks: edited, no checks ran"]);
  });
});
