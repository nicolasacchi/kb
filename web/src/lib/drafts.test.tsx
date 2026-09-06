// @vitest-environment jsdom
//
// `useDraft` is the exact mechanism CommentsPanel/CommentModal wire their
// composers through — tested directly (a small harness component) rather
// than via a full CommentsPanel render, so these assert the DEBOUNCE and
// KEY-SWITCH mechanics precisely with fake timers.
//
// No jest-dom in this repo (no `toHaveValue`) — assert the DOM `.value`
// property directly, same as every other component test here.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { __resetDraftsForTests, readDraft, useDraft } from "./drafts";

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
  return store;
}

function Harness({
  kb,
  artifactId,
  slot,
}: {
  kb: string;
  artifactId: string;
  slot: string | null;
}) {
  const draft = useDraft(kb, artifactId, slot);
  return (
    <div>
      <textarea
        aria-label="draft"
        value={draft.text}
        onChange={(e) => draft.setText(e.target.value)}
      />
      <button onClick={draft.clear}>clear</button>
    </div>
  );
}

function box(): HTMLTextAreaElement {
  return screen.getByLabelText("draft") as HTMLTextAreaElement;
}

function type(text: string) {
  fireEvent.change(box(), { target: { value: text } });
}

beforeEach(() => {
  __resetDraftsForTests();
  stubLocalStorage();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("useDraft — hydration", () => {
  it("hydrates from a pre-existing storage value on mount", () => {
    localStorage.setItem(
      "kb-draft/1:kb1:art1:file",
      JSON.stringify({ text: "left off here", savedAt: Date.now() }),
    );
    render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    expect(box().value).toBe("left off here");
  });

  it("starts blank with no persisted draft", () => {
    render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    expect(box().value).toBe("");
  });
});

describe("useDraft — type→reload→restored (the debounced write + flush-on-unmount)", () => {
  it("does not write before the debounce elapses", () => {
    vi.useFakeTimers();
    render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    type("typing…");
    act(() => {
      vi.advanceTimersByTime(399);
    });
    expect(readDraft("kb1", "art1", "file")).toBe("");
  });

  it("persists after the debounce elapses, and a fresh mount ('reload') restores it", () => {
    vi.useFakeTimers();
    const { unmount } = render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    type("hello world");
    act(() => {
      vi.advanceTimersByTime(400);
    });
    expect(readDraft("kb1", "art1", "file")).toBe("hello world");

    unmount();
    render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    expect(box().value).toBe("hello world");
  });

  it("flushes an un-debounced write on unmount — a fast close doesn't drop it", () => {
    vi.useFakeTimers();
    const { unmount } = render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    type("no time to debounce");
    // No vi.advanceTimersByTime at all — unmount immediately.
    unmount();
    expect(readDraft("kb1", "art1", "file")).toBe("no time to debounce");
  });
});

describe("useDraft — anchor-switch round trip", () => {
  it("switching slots flushes the outgoing draft and hydrates the incoming one; switching back restores it", () => {
    vi.useFakeTimers();
    const { rerender } = render(
      <Harness kb="kb1" artifactId="art1" slot="compose:section-a" />,
    );
    type("draft on A");
    // Switch to a different anchor's slot BEFORE the debounce would have
    // fired — the switch itself must flush A's pending write.
    rerender(<Harness kb="kb1" artifactId="art1" slot="compose:section-b" />);
    expect(box().value).toBe(""); // B has never been composed before
    expect(readDraft("kb1", "art1", "compose:section-a")).toBe("draft on A");

    type("draft on B");
    rerender(<Harness kb="kb1" artifactId="art1" slot="compose:section-a" />);
    // A's draft, written before B was ever opened, re-hydrates.
    expect(box().value).toBe("draft on A");
    expect(readDraft("kb1", "art1", "compose:section-b")).toBe("draft on B");
  });

  it("switching artifacts (kb/artifactId) keeps drafts separate — no cross-artifact leak", () => {
    vi.useFakeTimers();
    const { rerender } = render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    type("art1's draft");
    rerender(<Harness kb="kb1" artifactId="art2" slot="file" />);
    expect(box().value).toBe(""); // a fresh artifact, never composed on
    expect(readDraft("kb1", "art1", "file")).toBe("art1's draft");
    expect(readDraft("kb1", "art2", "file")).toBe("");
  });
});

describe("useDraft — clear()", () => {
  it("resets to '' and removes the persisted copy immediately", () => {
    vi.useFakeTimers();
    render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    type("about to submit");
    act(() => {
      vi.advanceTimersByTime(400);
    });
    expect(readDraft("kb1", "art1", "file")).toBe("about to submit");

    act(() => {
      screen.getByText("clear").click();
    });
    expect(box().value).toBe("");
    expect(readDraft("kb1", "art1", "file")).toBe("");
  });

  it("cancels a pending debounced write — clear() right after typing doesn't resurrect the draft", () => {
    vi.useFakeTimers();
    render(<Harness kb="kb1" artifactId="art1" slot="file" />);
    type("typed then cleared");
    act(() => {
      screen.getByText("clear").click();
    });
    act(() => {
      vi.advanceTimersByTime(1000); // well past the debounce window
    });
    expect(readDraft("kb1", "art1", "file")).toBe("");
  });
});

describe("useDraft — slot === null (disabled persistence)", () => {
  it("still behaves as plain in-memory state, but never touches storage", () => {
    vi.useFakeTimers();
    render(<Harness kb="kb1" artifactId="art1" slot={null} />);
    type("no target yet");
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(box().value).toBe("no target yet");
    expect(localStorage.length).toBe(0);
  });
});
