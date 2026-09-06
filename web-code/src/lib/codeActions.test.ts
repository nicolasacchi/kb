import { describe, expect, it } from "vitest";
import type { CodeActionEdit, CodeActionFileEdit, CodeActionRow, CodeActionsOut } from "../api/types";
import {
  actionToAnnotationOps,
  anchorForEdit,
  buildCodeActionsRequest,
  buildCodeActionsView,
  bytePrefix,
  byteSlice,
  byteSuffix,
  droppedTotal,
  editFileCount,
  editToAnnotationOp,
  editTotalCount,
  originalRangeText,
  rangeFromDiagnostic,
  spliceEditReplacement,
  totalActionCount,
  unavailableCodeActionsReasonLabel,
} from "./codeActions";

function edit(overrides: Partial<CodeActionEdit> = {}): CodeActionEdit {
  return { start_line: 3, start_col: 4, end_line: 3, end_col: 9, new_text: "fixed", ...overrides };
}

function fileEdit(overrides: Partial<CodeActionFileEdit> = {}): CodeActionFileEdit {
  return { path: "src/x.rs", edits: [edit()], ...overrides };
}

function action(overrides: Partial<CodeActionRow> = {}): CodeActionRow {
  return {
    title: "Add missing import",
    kind: "quickfix",
    is_preferred: false,
    edits: [fileEdit()],
    ...overrides,
  };
}

function out(overrides: Partial<CodeActionsOut> = {}): CodeActionsOut {
  return {
    schema: "code-actions/1",
    available: true,
    verified: true,
    reason: null,
    provider: "rust-analyzer-local",
    actions: [],
    dropped: { command_only: 0, unsupported: 0 },
    ...overrides,
  };
}

describe("buildCodeActionsRequest", () => {
  it("omits end_* for a point range", () => {
    expect(buildCodeActionsRequest("kb", "src/x.rs", { start_line: 10, start_col: 0 })).toEqual({
      repo: "kb",
      path: "src/x.rs",
      start_line: 10,
      start_col: 0,
    });
  });

  it("includes end_* when end_line >= start_line, defaulting end_col to start_col", () => {
    expect(
      buildCodeActionsRequest("kb", "src/x.rs", { start_line: 10, start_col: 0, end_line: 12 }),
    ).toEqual({
      repo: "kb",
      path: "src/x.rs",
      start_line: 10,
      start_col: 0,
      end_line: 12,
      end_col: 0,
    });
  });

  it("drops an end_line BEFORE start_line back to a point range", () => {
    expect(
      buildCodeActionsRequest("kb", "src/x.rs", { start_line: 10, start_col: 0, end_line: 5, end_col: 2 }),
    ).toEqual({
      repo: "kb",
      path: "src/x.rs",
      start_line: 10,
      start_col: 0,
    });
  });

  it("forwards a non-empty kinds list, dropping empty", () => {
    expect(buildCodeActionsRequest("kb", "x", { start_line: 1, start_col: 0 }, ["quickfix"]).kinds).toEqual([
      "quickfix",
    ]);
    expect(buildCodeActionsRequest("kb", "x", { start_line: 1, start_col: 0 }, []).kinds).toBeUndefined();
  });
});

describe("rangeFromDiagnostic", () => {
  it("treats end_line 0 as unreported (point range)", () => {
    expect(rangeFromDiagnostic({ line: 5, col: 2, end_line: 0, end_col: 0 })).toEqual({
      start_line: 5,
      start_col: 2,
      end_line: undefined,
      end_col: undefined,
    });
  });

  it("treats end_line < line as unreported", () => {
    expect(rangeFromDiagnostic({ line: 5, col: 2, end_line: 3, end_col: 9 })).toEqual({
      start_line: 5,
      start_col: 2,
      end_line: undefined,
      end_col: undefined,
    });
  });

  it("carries a real range through", () => {
    expect(rangeFromDiagnostic({ line: 5, col: 2, end_line: 7, end_col: 9 })).toEqual({
      start_line: 5,
      start_col: 2,
      end_line: 7,
      end_col: 9,
    });
  });
});

describe("buildCodeActionsView", () => {
  it("absent before the user has ever requested", () => {
    expect(buildCodeActionsView(false, false, null)).toEqual({ kind: "absent" });
  });

  it("loading while requested + in flight, or requested with no data yet", () => {
    expect(buildCodeActionsView(true, true, null)).toEqual({ kind: "loading" });
    expect(buildCodeActionsView(true, false, undefined)).toEqual({ kind: "loading" });
  });

  it("reason for available:false, mapping the closed vocabulary", () => {
    const view = buildCodeActionsView(true, false, out({ available: false, reason: "capability_absent", provider: null }));
    expect(view.kind).toBe("reason");
    expect(view.reason).toBe("provider doesn't support code actions");
  });

  it("an unrecognized reason degrades to its own verbatim text", () => {
    const view = buildCodeActionsView(true, false, out({ available: false, reason: "some_future_reason" }));
    expect(view.reason).toBe("some_future_reason");
  });

  it("empty for a zero-action available response", () => {
    const view = buildCodeActionsView(true, false, out({ actions: [] }));
    expect(view.kind).toBe("empty");
    expect(view.provider).toBe("rust-analyzer-local");
  });

  it("actions for a non-empty response", () => {
    const a = action();
    const view = buildCodeActionsView(true, false, out({ actions: [a] }));
    expect(view.kind).toBe("actions");
    expect(view.actions).toEqual([a]);
  });
});

describe("unavailableCodeActionsReasonLabel", () => {
  it("falls back to a generic label when reason is null/undefined", () => {
    expect(unavailableCodeActionsReasonLabel(null)).toBe("quick fixes unavailable");
    expect(unavailableCodeActionsReasonLabel(undefined)).toBe("quick fixes unavailable");
  });
  it("maps every closed-vocabulary reason including capability_absent", () => {
    expect(unavailableCodeActionsReasonLabel("blob_stale")).toContain("changed while fetching");
    expect(unavailableCodeActionsReasonLabel("capability_absent")).toBe("provider doesn't support code actions");
  });
});

describe("byte-column slice math", () => {
  it("bytePrefix/byteSuffix/byteSlice on plain ASCII", () => {
    const line = "let x = 1;";
    expect(bytePrefix(line, 4)).toBe("let ");
    expect(byteSuffix(line, 4)).toBe("x = 1;");
    expect(byteSlice(line, 4, 5)).toBe("x");
  });

  it("clamps out-of-range columns rather than throwing", () => {
    const line = "abc";
    expect(bytePrefix(line, 100)).toBe("abc");
    expect(byteSuffix(line, 100)).toBe("");
    expect(byteSlice(line, 5, 2)).toBe("");
  });

  it("slices on UTF-8 byte boundaries, not UTF-16 code units", () => {
    // "é" is 1 UTF-16 code unit but 2 UTF-8 bytes (0xC3 0xA9).
    const line = "café bar";
    // bytes: c(1) a(1) f(1) é(2) space(1) b(1) a(1) r(1) => "café" is 5 bytes.
    expect(bytePrefix(line, 5)).toBe("café");
    expect(byteSuffix(line, 5)).toBe(" bar");
  });
});

describe("originalRangeText / spliceEditReplacement", () => {
  const content = "fn main() {\n    let x = old_call();\n    println!(\"{}\", x);\n}\n";

  it("single-line: extracts and splices within one line", () => {
    const e = edit({ start_line: 2, start_col: 12, end_line: 2, end_col: 20, new_text: "new_call" });
    expect(originalRangeText(content, e)).toBe("old_call");
    expect(spliceEditReplacement(content, e)).toBe("    let x = new_call();");
  });

  it("multi-line: joins prefix/middle/suffix with newlines", () => {
    const multi = "AAAA\nBBBB\nCCCC\n";
    const e = edit({ start_line: 1, start_col: 2, end_line: 3, end_col: 2, new_text: "XX" });
    expect(originalRangeText(multi, e)).toBe("AA\nBBBB\nCC");
    expect(spliceEditReplacement(multi, e)).toBe("AAXXCC");
  });

  it("returns null when the range falls outside the file's line count", () => {
    const e = edit({ start_line: 50, end_line: 50 });
    expect(originalRangeText(content, e)).toBeNull();
    expect(spliceEditReplacement(content, e)).toBeNull();
  });
});

describe("anchorForEdit", () => {
  it("single-line edit anchors as 'line' with no line_end", () => {
    expect(anchorForEdit(edit({ start_line: 3, end_line: 3 }))).toEqual({ anchor_kind: "line", line: 3 });
  });
  it("multi-line edit anchors as 'range' with line_end", () => {
    expect(anchorForEdit(edit({ start_line: 3, end_line: 5 }))).toEqual({
      anchor_kind: "range",
      line: 3,
      line_end: 5,
    });
  });
});

describe("editToAnnotationOp / actionToAnnotationOps", () => {
  it("builds a note-intent add_comment op with the Quick fix body + provenance line", () => {
    const fe = fileEdit();
    const op = editToAnnotationOp("Add missing import", fe, fe.edits[0], { provider: "rust-analyzer-local" });
    expect(op.op).toBe("add_comment");
    expect(op.path).toBe("src/x.rs");
    expect(op.intent).toBe("note");
    expect(op.anchor_kind).toBe("line");
    expect(op.line).toBe(3);
    expect(op.line_end).toBeUndefined();
    expect(op.body).toBe("Quick fix: Add missing import\n\nvia rust-analyzer-local (lsp-live)");
  });

  it("falls back to a bare 'via lsp-live' provenance line when provider is absent", () => {
    const fe = fileEdit();
    const op = editToAnnotationOp("Fix", fe, fe.edits[0], {});
    expect(op.body).toContain("via lsp-live");
  });

  it("uses new_text verbatim as the suggestion replacement when no file content is supplied", () => {
    const fe = fileEdit();
    const op = editToAnnotationOp("Fix", fe, fe.edits[0]);
    expect(op.suggestion).toEqual({ replacement: "fixed" });
  });

  it("splices a real full-line replacement when file content IS supplied", () => {
    const fe: CodeActionFileEdit = {
      path: "src/x.rs",
      edits: [{ start_line: 1, start_col: 3, end_line: 1, end_col: 3, new_text: "!" }],
    };
    const op = editToAnnotationOp("Fix", fe, fe.edits[0], { fileContents: { "src/x.rs": "abc\ndef" } });
    expect(op.suggestion).toEqual({ replacement: "abc!" });
  });

  it("threads review scope fields through only when provided", () => {
    const fe = fileEdit();
    const op = editToAnnotationOp("Fix", fe, fe.edits[0], { reviewId: 7, ps: 2, side: "new" });
    expect(op.review_id).toBe(7);
    expect(op.ps).toBe(2);
    expect(op.side).toBe("new");
    const bare = editToAnnotationOp("Fix", fe, fe.edits[0]);
    expect(bare.review_id).toBeUndefined();
  });

  it("actionToAnnotationOps expands one op per (file, edit) pair in relay order", () => {
    const a = action({
      edits: [
        { path: "a.rs", edits: [edit({ start_line: 1 }), edit({ start_line: 2 })] },
        { path: "b.rs", edits: [edit({ start_line: 9 })] },
      ],
    });
    const ops = actionToAnnotationOps(a);
    expect(ops.map((o) => [o.path, o.line])).toEqual([
      ["a.rs", 1],
      ["a.rs", 2],
      ["b.rs", 9],
    ]);
  });
});

describe("count/badge helpers", () => {
  it("totalActionCount / droppedTotal", () => {
    expect(totalActionCount(null)).toBe(0);
    expect(totalActionCount(out({ actions: [action()] }))).toBe(1);
    expect(droppedTotal(null)).toBe(0);
    expect(droppedTotal({ command_only: 2, unsupported: 3 })).toBe(5);
  });

  it("editFileCount / editTotalCount", () => {
    const a = action({
      edits: [
        { path: "a.rs", edits: [edit(), edit()] },
        { path: "b.rs", edits: [edit()] },
      ],
    });
    expect(editFileCount(a)).toBe(2);
    expect(editTotalCount(a)).toBe(3);
  });
});
