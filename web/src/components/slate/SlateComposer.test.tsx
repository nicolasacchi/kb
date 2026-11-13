// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import SlateComposer from "./SlateComposer";

vi.mock("../LazyMarkdownEditor", () => ({ default: () => null }));

async function submitAsk(pick?: string) {
  const onSubmit = vi.fn().mockResolvedValue(undefined);
  render(
    <SlateComposer open seed={{ kind: "ask" }} topics={[]} onClose={() => {}} onSubmit={onSubmit} />,
  );
  if (pick) fireEvent.change(screen.getByLabelText("To"), { target: { value: pick } });
  fireEvent.change(screen.getByLabelText("Line"), { target: { value: "which db?" } });
  fireEvent.click(screen.getByRole("button", { name: "Post" }));
  await waitFor(() => expect(onSubmit).toHaveBeenCalled());
  return onSubmit.mock.calls[0][0];
}

afterEach(cleanup);

describe("SlateComposer to select", () => {
  it("defaults to any and sends no to", async () => {
    const body = await submitAsk();
    expect("to" in body).toBe(false);
  });

  it("sends to: you", async () => {
    const body = await submitAsk("you");
    expect(body.to).toBe("you");
  });

  it("sends a harness", async () => {
    const body = await submitAsk("omp");
    expect(body.to).toBe("omp");
  });

  it("offers every addressee", () => {
    render(<SlateComposer open seed={{ kind: "ask" }} topics={[]} onClose={() => {}} onSubmit={vi.fn()} />);
    const opts = Array.from(screen.getByLabelText("To").querySelectorAll("option")).map((o) => o.value);
    expect(opts).toEqual(["any", "you", "claude", "codex", "opencode", "grok", "kimi", "omp"]);
  });

  it("is absent for other kinds", () => {
    render(<SlateComposer open seed={{ kind: "idea" }} topics={[]} onClose={() => {}} onSubmit={vi.fn()} />);
    expect(screen.queryByLabelText("To")).toBeNull();
  });
});

function mountC(seed: object, mobile = false) {
  const onSubmit = vi.fn().mockResolvedValue(undefined);
  const r = render(
    <SlateComposer open seed={seed} topics={[]} mobile={mobile} onClose={() => {}} onSubmit={onSubmit} />,
  );
  return { onSubmit, r };
}

async function post(onSubmit: ReturnType<typeof vi.fn>) {
  fireEvent.click(screen.getByRole("button", { name: /^(Post|Rewrite)$/ }));
  await waitFor(() => expect(onSubmit).toHaveBeenCalled());
  return onSubmit.mock.calls[onSubmit.mock.calls.length - 1][0];
}

describe("SlateComposer to edge cases", () => {
  it("switching ask+you to idea posts no to", async () => {
    const { onSubmit } = mountC({ kind: "ask" });
    fireEvent.change(screen.getByLabelText("To"), { target: { value: "you" } });
    fireEvent.change(screen.getByLabelText("Kind"), { target: { value: "idea" } });
    fireEvent.change(screen.getByLabelText("Line"), { target: { value: "try x" } });
    const body = await post(onSubmit);
    expect(body.kind).toBe("idea");
    expect("to" in body).toBe(false);
  });

  it("edit seed preselects you; any omits to; reopening with another seed resets", async () => {
    const onSubmit = vi.fn().mockResolvedValue(undefined);
    const ui = (seed: object) => (
      <SlateComposer open seed={seed} topics={[]} onClose={() => {}} onSubmit={onSubmit} />
    );
    const { rerender } = render(ui({ kind: "ask", line: "q?", to: "you", supersedes: 3 }));
    expect((screen.getByLabelText("To") as HTMLSelectElement).value).toBe("you");
    fireEvent.change(screen.getByLabelText("To"), { target: { value: "any" } });
    expect("to" in (await post(onSubmit))).toBe(false);
    rerender(ui({ kind: "ask", line: "q2?", to: "omp", supersedes: 4 }));
    expect((screen.getByLabelText("To") as HTMLSelectElement).value).toBe("omp");
  });

  it("mobile has no to select and sends none", async () => {
    const { onSubmit } = mountC({ kind: "ask", to: "you" }, true);
    expect(screen.queryByLabelText("To")).toBeNull();
    fireEvent.change(screen.getByLabelText("Line"), { target: { value: "q?" } });
    const body = await post(onSubmit);
    expect("to" in body).toBe(false);
  });

  it("editing a hand keeps its to", async () => {
    const { onSubmit } = mountC({ kind: "hand", line: "pass it", subject: "s", to: "omp", supersedes: 5 });
    expect(screen.queryByLabelText("To")).toBeNull();
    expect((await post(onSubmit)).to).toBe("omp");
  });

  it("an unknown seeded to appears as an option", () => {
    mountC({ kind: "ask", line: "q?", to: "newharness", supersedes: 5 });
    const sel = screen.getByLabelText("To") as HTMLSelectElement;
    expect(sel.value).toBe("newharness");
    expect(Array.from(sel.options).map((o) => o.value)).toContain("newharness");
  });
});
