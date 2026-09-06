import { describe, it, expect } from "vitest";
import { flipTask } from "./flipTask";

// Pins the optimistic client mirror to the server's task semantics
// (`kb_core::notes::scan_tasks`, comrak-based) across the SAME divergent
// fixtures the Rust suite uses. flipTask is optimistic-only (the server
// response is authoritative), so these cover the common GFM forms — and the
// two historical bugs: fence run-length and no-whitespace-after-`]`.

describe("flipTask", () => {
  it("flips the nth bullet task in document order", () => {
    const body = "- [ ] a\n- [ ] b\n- [x] c\n";
    expect(flipTask(body, 1, true)).toBe("- [ ] a\n- [x] b\n- [x] c\n");
    expect(flipTask(body, 2, false)).toBe("- [ ] a\n- [ ] b\n- [ ] c\n");
  });

  it("counts nested + star/plus markers", () => {
    expect(flipTask("- [ ] a\n  - [ ] nested\n", 1, true)).toBe(
      "- [ ] a\n  - [x] nested\n",
    );
    expect(flipTask("* [ ] star\n+ [ ] plus\n", 1, true)).toBe(
      "* [ ] star\n+ [x] plus\n",
    );
  });

  it("handles blockquoted tasks (index not skipped)", () => {
    expect(flipTask("> - [ ] x\n> - [ ] y\n", 1, true)).toBe(
      "> - [ ] x\n> - [x] y\n",
    );
  });

  it("handles ordered-list tasks (dot and paren)", () => {
    expect(flipTask("1. [ ] one\n2. [ ] two\n", 0, true)).toBe(
      "1. [x] one\n2. [ ] two\n",
    );
    expect(flipTask("1) [ ] one\n2) [ ] two\n", 1, true)).toBe(
      "1) [ ] one\n2) [x] two\n",
    );
  });

  it("skips fenced tasks and respects fence run-length", () => {
    // plain ``` fence
    expect(
      flipTask("- [ ] real\n```\n- [ ] fenced\n```\n- [ ] real2\n", 1, true),
    ).toBe("- [ ] real\n```\n- [ ] fenced\n```\n- [x] real2\n");
    // a 4-backtick fence is NOT closed by a 3-backtick line (the old bug);
    // the inner task stays fenced, so index 1 is `real2`.
    expect(
      flipTask(
        "- [ ] real\n````\n```\n- [ ] fenced\n````\n- [ ] real2\n",
        1,
        true,
      ),
    ).toBe("- [ ] real\n````\n```\n- [ ] fenced\n````\n- [x] real2\n");
    // tilde fence
    expect(flipTask("~~~~\n- [ ] fenced\n~~~~\n- [x] real\n", 0, false)).toBe(
      "~~~~\n- [ ] fenced\n~~~~\n- [ ] real\n",
    );
  });

  it("ignores `- [ ]x` (no whitespace after `]`) and out-of-range index", () => {
    // not a GFM task → nothing flips, body unchanged.
    expect(flipTask("- [ ]x not a task\n", 0, true)).toBe(
      "- [ ]x not a task\n",
    );
    // out of range → unchanged.
    expect(flipTask("- [ ] only\n", 5, true)).toBe("- [ ] only\n");
  });
});
