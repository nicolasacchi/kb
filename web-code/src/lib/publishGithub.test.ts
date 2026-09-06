import { describe, expect, it } from "vitest";
import { composeGhCommands, composeGhCommandsText, shellQuote, type GhExportInput } from "./publishGithub";

describe("shellQuote", () => {
  it("wraps a plain string in single quotes", () => {
    expect(shellQuote("hello")).toBe("'hello'");
  });

  it("escapes an embedded single quote", () => {
    expect(shellQuote("it's broken")).toBe("'it'\\''s broken'");
  });

  it("leaves double quotes and $ untouched (single-quoted, no shell expansion)", () => {
    expect(shellQuote(`say "$HOME"`)).toBe(`'say "$HOME"'`);
  });

  it("round-trips an empty string", () => {
    expect(shellQuote("")).toBe("''");
  });
});

function baseInput(overrides: Partial<GhExportInput> = {}): GhExportInput {
  return {
    ownerRepoSlug: "acme/widget",
    prNumber: 15533,
    commitId: "8f21bb0",
    event: "COMMENT",
    verdictBody: null,
    comments: [],
    generalComments: [],
    ...overrides,
  };
}

describe("composeGhCommands — golden strings", () => {
  it("composes one gh api command per single-line inline comment, exact bytes", () => {
    const input = baseInput({
      comments: [
        {
          path: "app/helpers/pagy_helper.rb",
          line: 225,
          line_end: null,
          side: "RIGHT",
          body: "**f-pagy-absolute-ignored**\n\ncustom_routing ignored.",
          finding_slug: "f-pagy-absolute-ignored",
          orphaned: false,
        },
      ],
      event: null,
    });
    const cmds = composeGhCommands(input);
    expect(cmds).toEqual([
      "gh api repos/acme/widget/pulls/15533/comments " +
        "-f body='**f-pagy-absolute-ignored**\n\ncustom_routing ignored.' " +
        "-f commit_id='8f21bb0' " +
        "-f path='app/helpers/pagy_helper.rb' " +
        "-F line=225 " +
        "-f side='RIGHT'",
    ]);
  });

  it("uses start_line/start_side for a range (line_end set), line names the END", () => {
    const input = baseInput({
      comments: [
        {
          path: "a.rb",
          line: 5,
          line_end: 9,
          side: "RIGHT",
          body: "body",
          finding_slug: "f-x",
          orphaned: false,
        },
      ],
      event: null,
    });
    expect(composeGhCommands(input)[0]).toBe(
      "gh api repos/acme/widget/pulls/15533/comments " +
        "-f body='body' -f commit_id='8f21bb0' -f path='a.rb' " +
        "-F start_line=5 -f start_side='RIGHT' -F line=9 -f side='RIGHT'",
    );
  });

  it("composes a plain gh pr comment for a general (file-level) comment", () => {
    const input = baseInput({
      generalComments: [{ body: "please add a base_url spec", finding_slug: "f-y", reason: "whole_file" }],
      event: null,
    });
    expect(composeGhCommands(input)).toEqual([
      "gh pr comment 15533 --body 'please add a base_url spec'",
    ]);
  });

  it("appends gh pr review LAST, after every comment", () => {
    const input = baseInput({
      comments: [
        { path: "a.rb", line: 1, line_end: null, side: "RIGHT", body: "b1", finding_slug: "f-a", orphaned: false },
      ],
      generalComments: [{ body: "b2", finding_slug: "f-b", reason: "whole_file" }],
      event: "REQUEST_CHANGES",
      verdictBody: "please fix these",
    });
    const cmds = composeGhCommands(input);
    expect(cmds).toHaveLength(3);
    expect(cmds[2]).toBe("gh pr review 15533 --request-changes -b 'please fix these'");
  });

  it("maps every event to its exact gh pr review flag", () => {
    expect(composeGhCommands(baseInput({ event: "APPROVE", verdictBody: null }))[0]).toBe(
      "gh pr review 15533 --approve",
    );
    expect(composeGhCommands(baseInput({ event: "REQUEST_CHANGES", verdictBody: null }))[0]).toBe(
      "gh pr review 15533 --request-changes",
    );
    expect(composeGhCommands(baseInput({ event: "COMMENT", verdictBody: null }))[0]).toBe(
      "gh pr review 15533 --comment",
    );
  });

  it("omits -b when the verdict body is null or blank", () => {
    expect(composeGhCommands(baseInput({ event: "APPROVE", verdictBody: "" }))[0]).toBe(
      "gh pr review 15533 --approve",
    );
    expect(composeGhCommands(baseInput({ event: "APPROVE", verdictBody: "   " }))[0]).toBe(
      "gh pr review 15533 --approve",
    );
  });

  it("emits no gh pr review command when event is null (no verdict set)", () => {
    const cmds = composeGhCommands(baseInput({ event: null }));
    expect(cmds).toEqual([]);
  });

  it("degrades to an empty list when ownerRepoSlug doesn't parse as owner/repo", () => {
    expect(composeGhCommands(baseInput({ ownerRepoSlug: "not-a-slug", event: "APPROVE" }))).toEqual([]);
    expect(composeGhCommands(baseInput({ ownerRepoSlug: "", event: "APPROVE" }))).toEqual([]);
  });

  it("composeGhCommandsText joins with newlines", () => {
    const input = baseInput({ event: "APPROVE", verdictBody: null });
    expect(composeGhCommandsText(input)).toBe(composeGhCommands(input).join("\n"));
  });
});
