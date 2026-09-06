// @vitest-environment jsdom
//
// A-SPA — verifies the durable-draft wiring (lib/drafts.ts) actually reaches
// CommentsPanel's composers: type→"reload"(remount)→restored for the
// file-scope box, and the anchor-switch round trip for the routed composer.
// CodeMirror (LazyMarkdownEditor) is stubbed to a plain textarea — its own
// behaviour isn't this suite's concern, only that CommentsPanel plumbs
// `useDraft`'s text/setText through to whatever editor is mounted.
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { forwardRef, useImperativeHandle } from "react";
import CommentsPanel, { type CommentsPanelProps } from "./CommentsPanel";
import ConfirmProvider from "./ConfirmProvider";
import { emptyReview, type Anchor } from "../api/client";
import { __resetDraftsForTests, readDraft } from "../lib/drafts";

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

vi.mock("../hooks/useReview", () => ({
  useReview: () => ({ setVerdict: vi.fn() }),
}));

vi.mock("../hooks/useArtifactHost", () => ({
  useIdentity: () => null,
}));

// jsdom doesn't implement <dialog>.showModal() — stub it (same pattern as
// LineageViewer.test.tsx / ProvenanceThread.test.tsx) so ConfirmProvider's
// modal can open in the discard-flow test below.
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

function baseProps(overrides: Partial<CommentsPanelProps> = {}): CommentsPanelProps {
  return {
    kb: "kb1",
    artifactId: "art1",
    sourceRelative: "doc.html",
    file: emptyReview("kb1", "art1", "Doc Title"),
    loading: false,
    error: null,
    staleCommentIds: new Set(),
    onAddComment: vi.fn().mockResolvedValue({}),
    onAddReply: vi.fn().mockResolvedValue({}),
    onResolveComment: vi.fn().mockResolvedValue(undefined),
    onUnresolveComment: vi.fn().mockResolvedValue(undefined),
    onEditComment: vi.fn().mockResolvedValue(undefined),
    onDeleteComment: vi.fn().mockResolvedValue(undefined),
    onDetachAttachment: vi.fn(),
    onDetachReplyAttachment: vi.fn(),
    activeCommentId: null,
    ...overrides,
  };
}

function renderPanel(overrides: Partial<CommentsPanelProps> = {}) {
  const props = baseProps(overrides);
  return render(
    <ConfirmProvider>
      <CommentsPanel {...props} />
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

describe("CommentsPanel — file-scope draft (type→reload→restored)", () => {
  it("persists what's typed and restores it on a fresh mount", () => {
    vi.useFakeTimers();
    const { unmount } = renderPanel();
    const box = screen.getByLabelText("file-scope comment") as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "an unsent file comment" } });
    vi.advanceTimersByTime(400);
    expect(readDraft("kb1", "art1", "file")).toBe("an unsent file comment");

    unmount();
    renderPanel();
    expect(
      (screen.getByLabelText("file-scope comment") as HTMLTextAreaElement).value,
    ).toBe("an unsent file comment");
  });

  it("clears the persisted draft once the comment is actually added", () => {
    vi.useFakeTimers();
    const onAddComment = vi.fn().mockResolvedValue({});
    renderPanel({ onAddComment });
    const box = screen.getByLabelText("file-scope comment") as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "ship it" } });
    vi.advanceTimersByTime(400);
    expect(readDraft("kb1", "art1", "file")).toBe("ship it");

    fireEvent.click(screen.getByText("add file-scope"));
    expect(onAddComment).toHaveBeenCalled();
    expect(readDraft("kb1", "art1", "file")).toBe("");
  });
});

describe("CommentsPanel — routed-anchor composer (anchor-switch round trip)", () => {
  const anchorA: Anchor = { kind: "section", id: "h1", tag: null, snippet: null };
  const anchorB: Anchor = { kind: "section", id: "h2", tag: null, snippet: null };

  it("switching composeAnchor away and back restores the outgoing anchor's draft", () => {
    vi.useFakeTimers();
    const { rerender } = render(
      <ConfirmProvider>
        <CommentsPanel {...baseProps({ composeAnchor: anchorA, onCloseCompose: vi.fn() })} />
      </ConfirmProvider>,
    );
    const box = () => screen.getByLabelText("new comment body") as HTMLTextAreaElement;
    fireEvent.change(box(), { target: { value: "thoughts on section A" } });

    rerender(
      <ConfirmProvider>
        <CommentsPanel {...baseProps({ composeAnchor: anchorB, onCloseCompose: vi.fn() })} />
      </ConfirmProvider>,
    );
    expect(box().value).toBe(""); // B has no draft yet
    expect(readDraft("kb1", "art1", "compose:section:h1::")).toBe(
      "thoughts on section A",
    );

    rerender(
      <ConfirmProvider>
        <CommentsPanel {...baseProps({ composeAnchor: anchorA, onCloseCompose: vi.fn() })} />
      </ConfirmProvider>,
    );
    expect(box().value).toBe("thoughts on section A");
  });

  it("a confirmed discard clears the persisted draft for that anchor", async () => {
    // Real timers here — the confirm() promise resolves off a user click,
    // not a debounce, and testing-library's `waitFor` polls with real
    // timers by default.
    renderPanel({ composeAnchor: anchorA, onCloseCompose: vi.fn() });
    const box = screen.getByLabelText("new comment body") as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "never mind" } });
    await waitFor(() =>
      expect(readDraft("kb1", "art1", "compose:section:h1::")).toBe("never mind"),
    );

    fireEvent.click(screen.getByText("cancel")); // discardCompose()
    await waitFor(() => expect(screen.getByText("Discard")).toBeTruthy());
    fireEvent.click(screen.getByText("Discard")); // ConfirmModal's confirm button

    await waitFor(() =>
      expect(readDraft("kb1", "art1", "compose:section:h1::")).toBe(""),
    );
  });
});
