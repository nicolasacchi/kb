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
