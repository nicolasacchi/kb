// @vitest-environment jsdom
//
// A-SPA — the modal's reply composer shares its durable-draft slot
// (`reply:<commentId>`, lib/drafts.ts) with CommentsPanel's inline
// CommentRow reply box for the SAME comment. Verified here at the
// storage level (round trip + shared-slot) since mounting both composers
// simultaneously would need the full panel; CommentsPanel.test.tsx
// already covers the file/compose slots' wiring the identical way.
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { forwardRef, useImperativeHandle } from "react";
import CommentModal from "./CommentModal";
import ConfirmProvider from "./ConfirmProvider";
import type { Comment } from "../api/client";
import { __resetDraftsForTests, readDraft, writeDraft } from "../lib/drafts";

vi.mock("./LazyMarkdownEditor", () => ({
  default: forwardRef(function StubEditor(
    props: {
      ariaLabel?: string;
      value: string;
      onChange: (v: string) => void;
    },
    ref,
  ) {
    useImperativeHandle(ref, () => ({ focusWrite: () => {}, insertAtCursor: () => {} }));
    return (
      <textarea
        aria-label={props.ariaLabel}
        value={props.value}
        onChange={(e) => props.onChange(e.target.value)}
      />
    );
  }),
}));

vi.mock("./CommentBody", () => ({
  default: ({ body }: { body: string }) => <div>{body}</div>,
}));

vi.mock("../hooks/useArtifactHost", () => ({
  useIdentity: () => null,
}));

beforeAll(() => {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
      this.open = true;
    };
  }
});

function stubLocalStorage() {
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    setItem: (k: string, v: string) => {
      store.set(k, v);
    },
    removeItem: (k: string) => {
      store.delete(k);
    },
    get length() {
      return store.size;
    },
    key: (i: number) => Array.from(store.keys())[i] ?? null,
    clear: () => store.clear(),
  });
}

function makeComment(overrides: Partial<Comment> = {}): Comment {
  return {
    id: "c_deadbeef0001",
    status: "open",
    file: "art1",
    fileLabel: "art1",
    anchor: { kind: "file" },
    author: "you",
    body: "the original comment body",
    createdAt: "2026-08-01T00:00:00Z",
    editedAt: null,
    replies: [],
    choices: [],
    attachments: [],
    ...overrides,
  };
}

function renderModal(overrides: Partial<Comment> = {}, extra: Partial<Parameters<typeof CommentModal>[0]> = {}) {
  return render(
    <ConfirmProvider>
      <CommentModal
        kb="kb1"
        artifactId="art1"
        comment={makeComment(overrides)}
        onClose={vi.fn()}
        onEditBody={vi.fn()}
        onReply={vi.fn()}
        onChoose={vi.fn()}
        {...extra}
      />
    </ConfirmProvider>,
  );
}

beforeEach(() => {
  __resetDraftsForTests();
  stubLocalStorage();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("CommentModal — reply draft (type→reload→restored)", () => {
  it("persists the reply box text and restores it on a fresh mount", () => {
    vi.useFakeTimers();
    const { unmount } = renderModal();
    const box = screen.getByLabelText("reply body") as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "a half-typed reply" } });
    vi.advanceTimersByTime(400);
    expect(readDraft("kb1", "art1", "reply:c_deadbeef0001")).toBe(
      "a half-typed reply",
    );

    unmount();
    renderModal();
    expect(
      (screen.getByLabelText("reply body") as HTMLTextAreaElement).value,
    ).toBe("a half-typed reply");
  });

  it("clears the persisted draft once the reply is actually sent", async () => {
    const onReply = vi.fn();
    renderModal({}, { onReply });
    const box = screen.getByLabelText("reply body") as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "posting this" } });
    await waitFor(() =>
      expect(readDraft("kb1", "art1", "reply:c_deadbeef0001")).toBe(
        "posting this",
      ),
    );

    fireEvent.click(screen.getByText("send reply"));
    expect(onReply).toHaveBeenCalledWith("you", "posting this");
    expect(readDraft("kb1", "art1", "reply:c_deadbeef0001")).toBe("");
  });

  it("hydrates from a draft left by CommentsPanel's inline reply box for the same comment", () => {
    // Simulates: the user half-typed a reply in CommentRow's inline box
    // (same slot grammar, `reply:<commentId>`), then opened the modal
    // ("⤢ expand") on the same comment WITHOUT sending.
    writeDraft("kb1", "art1", "reply:c_deadbeef0001", "typed in the row first");
    renderModal();
    expect(
      (screen.getByLabelText("reply body") as HTMLTextAreaElement).value,
    ).toBe("typed in the row first");
  });

  it("keeps each comment's reply draft independent", () => {
    writeDraft("kb1", "art1", "reply:c_aaaaaaaaaaaa", "reply to A");
    writeDraft("kb1", "art1", "reply:c_bbbbbbbbbbbb", "reply to B");
    renderModal({ id: "c_aaaaaaaaaaaa" });
    expect(
      (screen.getByLabelText("reply body") as HTMLTextAreaElement).value,
    ).toBe("reply to A");
  });
});
