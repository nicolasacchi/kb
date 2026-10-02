// @vitest-environment jsdom
//
// v0.44 P1 — private notes never cross the operator boundary. The SPA's
// review file is the `?visibility=all` one; the iframe bridge and every
// export must project it through publicView first.
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { createRef } from "react";
import AnnotatorBridge, { type BridgeApi, expectedIframeOrigin } from "./AnnotatorBridge";
import { ExportModal, buildClaudePrompt, buildMarkdown } from "./CommentsPanel";
import { emptyReview, type ReviewFile } from "../api/client";

const SECRET = "SECRET-NOTE-BODY";

function fileWithNote(): ReviewFile {
  const f = emptyReview("k", "art1");
  const base = {
    status: "open",
    file: "art1",
    fileLabel: "art1",
    anchor: { kind: "file" },
    author: "you",
    createdAt: "2026-01-01T00:00:00Z",
    editedAt: null,
    replies: [],
    choices: [],
    attachments: [],
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  f.comments = [
    { ...base, id: "pub1", body: "public body" },
    { ...base, id: "priv1", body: SECRET, private: true },
  ] as any;
  return f;
}

beforeAll(() => {
  if (!HTMLDialogElement.prototype.show) {
    HTMLDialogElement.prototype.show = function show() {
      this.open = true;
    };
  }
});
afterEach(cleanup);

describe("AnnotatorBridge.refresh", () => {
  it("posts the public projection, never the note", () => {
    const iframe = document.createElement("iframe");
    document.body.appendChild(iframe);
    const post = vi.fn();
    Object.defineProperty(iframe, "contentWindow", { value: { postMessage: post } });
    const iframeRef = { current: iframe };
    const api = createRef<BridgeApi>();
    render(
      <AnnotatorBridge
        ref={api}
        iframeRef={iframeRef}
        artifactId="art1"
        kb="k"
        hostSuffix=".artifacts.example.test"
        annotateMode={false}
        onComposeAnchor={() => {}}
        onFocusComment={() => {}}
        onExitAnnotate={() => {}}
        onSelection={() => {}}
        onSelectionClear={() => {}}
      />,
    );
    api.current!.refresh(fileWithNote());
    expect(post).toHaveBeenCalledTimes(1);
    const [payload, origin] = post.mock.calls[0];
    expect(origin).toBe(expectedIframeOrigin("art1", "k", ".artifacts.example.test"));
    expect(payload.type).toBe("cm:refresh");
    expect(JSON.stringify(payload)).not.toContain(SECRET);
    expect(payload.file.comments.map((c: { id: string }) => c.id)).toEqual(["pub1"]);
  });
});

describe("export builders + ExportModal", () => {
  it("ExportModal renders no section containing the note", () => {
    render(<ExportModal file={fileWithNote()} kb="k" onClose={() => {}} />);
    expect(screen.queryAllByText(new RegExp(SECRET))).toHaveLength(0);
    expect(document.body.textContent).not.toContain(SECRET);
    expect(document.body.textContent).toContain("public body");
  });

  it("the prompt and Markdown builders drop notes on their own too", () => {
    const prompt = buildClaudePrompt(fileWithNote(), "k");
    const md = buildMarkdown(fileWithNote());
    expect(prompt).not.toContain(SECRET);
    expect(md).not.toContain(SECRET);
    expect(prompt).toContain("public body");
    expect(md).toContain("public body");
  });
});
