// T18 — what Send does, rule by rule, in rule order.

import { describe, expect, it } from "vitest";
import { CAPTION, planSend, type SendCtx } from "./planSend";

const ctx = (over: Partial<SendCtx> = {}): SendCtx => ({
  harness: "omp",
  bracketPaste: true,
  blocked: false,
  ...over,
});

describe("planSend", () => {
  it("rule 1: blank text is empty", () => {
    for (const t of ["", " ", "\n", " \t\r\n "]) expect(planSend(t, ctx())).toEqual({ kind: "empty" });
  });

  it("rule 5: a plain message is paste then Enter", () => {
    expect(planSend("fix the build", ctx())).toEqual({ kind: "paste-enter", text: "fix the build" });
    // Unicode is just text.
    expect(planSend("déjà vu 日本語 🚀", ctx())).toEqual({ kind: "paste-enter", text: "déjà vu 日本語 🚀" });
  });

  it("a single trailing newline does not make a message multi-line", () => {
    expect(planSend("one line\n", ctx({ bracketPaste: false }))).toEqual({
      kind: "paste-enter",
      text: "one line\n",
    });
    expect(planSend("one line\r\n", ctx({ bracketPaste: false })).kind).toBe("paste-enter");
    // Two trailing newlines leave one: that IS multi-line.
    expect(planSend("one line\n\n", ctx({ bracketPaste: false }))).toEqual({
      kind: "refuse",
      reason: "multiline-no-bracketed-paste",
    });
  });

  it("rule 2: multi-line is refused when the pane did not enable bracketed paste", () => {
    expect(planSend("a\nb", ctx({ bracketPaste: false }))).toEqual({
      kind: "refuse",
      reason: "multiline-no-bracketed-paste",
    });
    expect(planSend("a\r\nb", ctx({ bracketPaste: false })).kind).toBe("refuse");
  });

  it("bracketPaste=true or unknown (null) lets a multi-line message through (the server is authoritative)", () => {
    expect(planSend("a\nb", ctx({ bracketPaste: true }))).toEqual({ kind: "paste-enter", text: "a\nb" });
    expect(planSend("a\nb", ctx({ bracketPaste: null }))).toEqual({ kind: "paste-enter", text: "a\nb" });
  });

  it("rule 3: a leading / or @ is paste-only, never Enter", () => {
    expect(planSend("/compact", ctx())).toEqual({ kind: "paste-only", text: "/compact", why: "slash" });
    expect(planSend("  /help", ctx()).kind).toBe("paste-only");
    expect(planSend("@src/main.rs", ctx())).toEqual({ kind: "paste-only", text: "@src/main.rs", why: "at" });
    // A slash inside the text is nothing special.
    expect(planSend("see /tmp/x", ctx()).kind).toBe("paste-enter");
  });

  it("rule 3 holds for a multi-line message even where multi-line is allowed", () => {
    expect(planSend("/x\ny", ctx({ bracketPaste: true })).kind).toBe("paste-only");
    expect(planSend("@x\ny", ctx({ bracketPaste: null })).kind).toBe("paste-only");
  });

  it("rule 4: a blocked session pastes then holds", () => {
    expect(planSend("yes please", ctx({ blocked: true }))).toEqual({
      kind: "paste-then-hold",
      text: "yes please",
    });
  });

  it("rule order 2 < 3 < 4", () => {
    // 2 beats 3 and 4: a multi-line refusal wins over a slash and over blocked.
    expect(planSend("/a\nb", ctx({ bracketPaste: false, blocked: true })).kind).toBe("refuse");
    // 3 beats 4: a slash while blocked is still paste-only.
    expect(planSend("/a", ctx({ blocked: true })).kind).toBe("paste-only");
    // 1 beats everything.
    expect(planSend("  ", ctx({ bracketPaste: false, blocked: true })).kind).toBe("empty");
  });

  it("the captions are the contract", () => {
    expect(CAPTION.pastedOnly).toBe("Pasted, not sent (starts with / or @). Press Enter on the key bar.");
    expect(CAPTION.holding).toBe("Session is waiting on you; use the choice bar.");
    expect(CAPTION.unbound).toBe("No session bound: Enter is disabled");
    expect(CAPTION.holdCtrlC).toBe("Hold to send Ctrl-C");
    expect(CAPTION.pastedNotSent("the session is blocked")).toBe("Pasted, not sent: the session is blocked");
    expect(CAPTION.refuse).toMatch(/^This pane did not enable bracketed paste/);
  });
});
