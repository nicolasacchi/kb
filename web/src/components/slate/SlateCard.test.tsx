// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import SlateCard from "./SlateCard";
import type { SlateBoardCard, SlateWho } from "../../api/slateTypes";

const agent: SlateWho = {
  origin: "agent",
  harness: "omp",
  session_short: "ab12",
  tag: "omp/ab12",
};

function ask(extra: Partial<SlateBoardCard> = {}): SlateBoardCard {
  return {
    seq: 7,
    id: "x",
    kind: "ask",
    line: "which db?",
    who: agent,
    age_secs: 120,
    tier: "whole",
    marks: 0,
    pinned: false,
    contested: false,
    body: null,
    marks_by: [],
    has_sketch: false,
    ...extra,
  };
}

function mount(card: SlateBoardCard, mobile = false, onAnswer?: () => void) {
  const qc = new QueryClient();
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <SlateCard card={card} mobile={mobile} actions={onAnswer ? { onAnswer } : {}} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(cleanup);

describe("SlateCard addressed asks", () => {
  it("emphasizes → you", () => {
    mount(ask({ to: "you" }));
    const chip = screen.getByText("→ you");
    expect(chip.classList.contains("is-you")).toBe(true);
  });

  it("shows a neutral chip for a harness addressee", () => {
    mount(ask({ to: "omp" }));
    const chip = screen.getByText("→ omp");
    expect(chip.classList.contains("is-you")).toBe(false);
  });

  it("shows no addressee chip when absent or any", () => {
    mount(ask());
    expect(document.querySelector(".slate-card__to")).toBeNull();
    cleanup();
    mount(ask({ to: "any" }));
    expect(document.querySelector(".slate-card__to")).toBeNull();
  });

  it("state chip is open at 0 answers", () => {
    mount(ask({ answers: 0 }));
    expect(screen.getByText("open")).toBeTruthy();
    expect(screen.queryByText(/waiting for asker/)).toBeNull();
  });

  it("state chip says answered — waiting for asker above 0 answers, with the last answer", () => {
    mount(
      ask({
        answers: 2,
        last_answer: {
          seq: 9,
          line: "use sqlite",
          who: { origin: "human", harness: "spa", session_short: "", tag: "you" },
          age_secs: 3700,
        },
      }),
    );
    expect(screen.getByText("answered — waiting for asker")).toBeTruthy();
    const la = screen.getByTestId("slate-last-answer");
    expect(la.textContent).toContain("[you]");
    expect(la.textContent).toContain("use sqlite");
    expect(la.textContent).toContain("1h");
  });

  it("renders an agent last answer by its tag", () => {
    mount(
      ask({
        answers: 1,
        last_answer: { seq: 9, line: "pg", who: agent, age_secs: 30 },
      }),
    );
    expect(screen.getByTestId("slate-last-answer").textContent).toContain("omp/ab12");
  });

  it("offers answer on desktop only", () => {
    mount(ask(), false, () => {});
    expect(document.querySelector('[data-act="answer"]')).toBeTruthy();
    cleanup();
    mount(ask(), true, () => {});
    expect(document.querySelector('[data-act="answer"]')).toBeNull();
  });

  it("non-ask cards get no ask chips", () => {
    mount(ask({ kind: "hand", to: "omp", answers: undefined }));
    expect(document.querySelector(".slate-card__askstate")).toBeNull();
  });

  it("a hand with to and last_answer gets no ask chips or answer block", () => {
    mount(
      ask({
        kind: "hand",
        to: "omp",
        answers: 1,
        last_answer: { seq: 9, line: "pg", who: agent, age_secs: 30 },
      }),
    );
    expect(document.querySelector(".slate-card__to")).toBeNull();
    expect(document.querySelector(".slate-card__askstate")).toBeNull();
    expect(screen.queryByTestId("slate-last-answer")).toBeNull();
  });

  it("author_ended shows the asker-ended chip and text", () => {
    mount(ask({ answers: 1, author_ended: true }));
    expect(screen.getByText("answered — asker ended")).toBeTruthy();
    expect(screen.getByText("asker ended")).toBeTruthy();
    expect(screen.queryByText(/waiting for asker/)).toBeNull();
  });

  it("a human asker says waiting for you", () => {
    mount(
      ask({
        answers: 1,
        who: { origin: "human", harness: "spa", session_short: "", tag: "you" },
      }),
    );
    expect(screen.getByText("answered — waiting for you")).toBeTruthy();
    expect(screen.queryByText("asker ended")).toBeNull();
  });

  it("the addressee chip has an accessible name", () => {
    mount(ask({ to: "omp" }));
    expect(screen.getByLabelText("addressed to omp")).toBeTruthy();
  });
});
