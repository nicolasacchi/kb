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
// v0.40 TN — the meta PATCH path, mocked so a test can prove it is NOT
// taken on the create (see the note-meta describe at the bottom).
import { patchCommentMeta } from "../api/reviewNotes";

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

vi.mock("../api/reviewNotes", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api/reviewNotes")>()),
  patchCommentMeta: vi.fn().mockResolvedValue({ ok: true, changed: true, tags: [], private: false }),
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

// v0.40 TN — a note's flag and tags must ride the CREATE request. A
// create-then-PATCH order leaves the comment public for a round trip, and
// the create records a history row, so the note's EXISTENCE would stay
// countable by an agent in the daycard activity bar and the calendar's
// per-day comment count — the one thing the flag promises it can never
// see. The two composers are a ternary, so exactly one is mounted and the
// shared label queries are unambiguous.
describe("CommentsPanel — note meta rides the create (v0.40 TN)", () => {
  const anchor: Anchor = { kind: "section", id: "h1", tag: null, snippet: null };

  beforeEach(() => {
    vi.mocked(patchCommentMeta).mockClear();
  });

  it("file-scope: 🔒 + tags go out on the create, and no meta PATCH follows", () => {
    vi.useFakeTimers();
    const onAddComment = vi.fn().mockResolvedValue({ id: "c1" });
    renderPanel({ onAddComment });
    fireEvent.change(screen.getByLabelText("file-scope comment"), {
      target: { value: "an agent-blind note" },
    });
    vi.advanceTimersByTime(400);
    fireEvent.change(screen.getByLabelText("tags for this comment"), {
      target: { value: "fleet-doc, wording" },
    });
    fireEvent.click(screen.getByLabelText(/private note/));
    fireEvent.click(screen.getByText("add file-scope"));

    expect(onAddComment).toHaveBeenCalledTimes(1);
    const opts = onAddComment.mock.calls[0][2];
    expect(opts.private).toBe(true);
    // Split RAW, never slugified here — the daemon's
    // `normalize_comment_tags` is the one normaliser, and it runs on the
    // create route.
    expect(opts.tags).toEqual(["fleet-doc", "wording"]);
    expect(patchCommentMeta).not.toHaveBeenCalled();
  });

  it("routed-anchor: its own tags + note flag reach the create", () => {
    vi.useFakeTimers();
    const onAddComment = vi.fn().mockResolvedValue({ id: "c2" });
    renderPanel({ composeAnchor: anchor, onCloseCompose: vi.fn(), onAddComment });
    fireEvent.change(screen.getByLabelText("new comment body"), {
      target: { value: "note on the section" },
    });
    vi.advanceTimersByTime(400);
    fireEvent.change(screen.getByLabelText("tags for this comment"), {
      target: { value: "wording" },
    });
    fireEvent.click(screen.getByText("add comment"));

    const opts = onAddComment.mock.calls[0][2];
    expect("private" in opts).toBe(false); // unchecked 🔒 ⇒ key absent, not false
    expect(opts.tags).toEqual(["wording"]);
    expect(patchCommentMeta).not.toHaveBeenCalled();
  });

  it("a plain comment puts NEITHER key on the wire (body byte-unchanged)", () => {
    vi.useFakeTimers();
    const onAddComment = vi.fn().mockResolvedValue({ id: "c3" });
    renderPanel({ onAddComment });
    fireEvent.change(screen.getByLabelText("file-scope comment"), {
      target: { value: "an ordinary comment" },
    });
    vi.advanceTimersByTime(400);
    fireEvent.click(screen.getByText("add file-scope"));

    const opts = onAddComment.mock.calls[0][2];
    // Not merely falsy: the keys must be ABSENT, so a plain add's JSON
    // body is byte-identical to the pre-TN one.
    expect("tags" in opts).toBe(false);
    expect("private" in opts).toBe(false);
  });
});

// v0.44 X2 (P2 carry-over) — the tag editor sends add_tags/remove_tags
// DELTAS like the CLI, never the full-replace `tags` key (a read-merge-write
// client erases whatever another writer landed in between).
describe("CommentsPanel — tag editor sends deltas (v0.44 X2)", () => {
  beforeEach(() => {
    vi.mocked(patchCommentMeta).mockClear();
  });

  it("editing tags PATCHes add_tags/remove_tags and no `tags` key", async () => {
    const file = emptyReview("kb1", "art1", "Doc Title");
    file.comments = [
      {
        id: "c1",
        status: "open",
        body: "tagged comment",
        file: "art1",
        fileLabel: "art1",
        anchor: { kind: "file" },
        author: "you",
        createdAt: "2026-01-01T00:00:00Z",
        editedAt: null,
        replies: [],
        tags: ["alpha", "beta"],
      },
    ] as unknown as typeof file.comments;
    renderPanel({ file });
    fireEvent.click(screen.getByText(/🏷 tags/));
    fireEvent.change(screen.getByLabelText("comment tags"), {
      target: { value: "beta, gamma" },
    });
    fireEvent.click(screen.getByText("save"));
    await waitFor(() => expect(patchCommentMeta).toHaveBeenCalledTimes(1));
    const patch = vi.mocked(patchCommentMeta).mock.calls[0][3];
    expect(patch).toEqual({ add_tags: ["gamma"], remove_tags: ["alpha"] });
    expect("tags" in patch).toBe(false);
  });
});
