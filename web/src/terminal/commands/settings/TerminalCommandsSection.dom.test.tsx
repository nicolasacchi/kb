// @vitest-environment jsdom
//
// CMDSETTINGS - the Terminal commands editor against an in-memory daemon
// stand-in: add / edit / hide / unhide / reorder / reset, import, the 412 and
// 400 paths, the newline warning and the SSE-echo-vs-draft rule.
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import ConfirmProvider from "../../../components/ConfirmProvider";
import { TerminalCommandsSection } from "./TerminalCommandsSection";
import { installFakeCommands, type Fake } from "./commandsFake.testkit";
import { TERM_COMMANDS_KEY } from "./useTermCommands";

let fake: Fake;
let qc: QueryClient;

beforeAll(() => {
  // jsdom has no <dialog>.showModal; the confirm host needs one.
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
      this.open = true;
    };
  }
});

beforeEach(() => {
  fake = installFakeCommands();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

async function mount() {
  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const view = render(
    <QueryClientProvider client={qc}>
      <ConfirmProvider>
        <TerminalCommandsSection />
      </ConfirmProvider>
    </QueryClientProvider>,
  );
  await screen.findByRole("heading", { name: "Terminal commands" });
  return view;
}

const item = (id: string) => screen.getByTestId(`tcs-item-${id}`);
const btn = (name: string) => screen.getByRole("button", { name });
const confirmGo = () => fireEvent.click(document.querySelector(".confirm__go") as HTMLElement);
const confirmCancel = () => fireEvent.click(document.querySelector(".confirm__cancel") as HTMLElement);
const lastPut = () => fake.puts()[fake.puts().length - 1].body as {
  harnesses: Record<string, { groups: { id: string; label: string; items: Record<string, unknown>[] }[] }>;
  hidden: Record<string, string[]>;
};

async function addItem(label: string, text: string) {
  fireEvent.click(btn("Add item"));
  fireEvent.change(screen.getByLabelText("Label"), { target: { value: label } });
  fireEvent.change(screen.getByLabelText("Text"), { target: { value: text } });
  fireEvent.click(btn("Save item"));
  await waitFor(() => expect(screen.queryByTestId("tcs-form")).toBeNull());
}

describe("listing", () => {
  it("shows default badges and sends the intent header with GET", async () => {
    await mount();
    expect(within(item("help")).getByText("default")).toBeTruthy();
    expect(fake.calls[0]).toMatchObject({ method: "GET", intent: "terminal" });
  });

  it("flags unverified entries and switches harness", async () => {
    await mount();
    expect(within(item("help")).getByText("unverified")).toBeTruthy();
    expect(within(item("reply-continue")).queryByText("unverified")).toBeNull();
    fireEvent.click(btn("omp"));
    expect(within(item("shake")).getByText("unverified")).toBeTruthy();
    expect(screen.getByTestId("tcs-item-kb-context")).toBeTruthy();
  });
});

describe("add / edit", () => {
  it("adds an item, builds the PUT from the overlay only, and shows it as added", async () => {
    await mount();
    await addItem("/zz test", "/zz test");
    expect(fake.puts()).toHaveLength(1);
    expect(fake.puts()[0].ifMatch).toBe('"0"');
    expect(fake.puts()[0].intent).toBe("terminal");
    const body = lastPut();
    expect(JSON.stringify(body)).not.toContain("origin");
    expect(body.harnesses.claude.groups).toHaveLength(1);
    expect(body.harnesses.claude.groups[0].items[0]).toMatchObject({ kind: "command", label: "/zz test", text: "/zz test", mode: "paste" });
    expect(within(item("u-zz-test")).getByText("added")).toBeTruthy();
    expect(screen.getByTestId("tcs-notice").textContent).toContain("revision 1");
  });

  it("adds into a new group", async () => {
    await mount();
    fireEvent.click(btn("Add item"));
    fireEvent.change(screen.getByLabelText("Group"), { target: { value: "__new__" } });
    fireEvent.change(screen.getByLabelText("New group label"), { target: { value: "Mine" } });
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "hi" } });
    fireEvent.change(screen.getByLabelText("Text"), { target: { value: "hi" } });
    fireEvent.click(btn("Save item"));
    await waitFor(() => expect(screen.getByRole("heading", { name: "Mine" })).toBeTruthy());
    expect(lastPut().harnesses.claude.groups[0]).toMatchObject({ id: "g-mine", label: "Mine" });
  });

  it("adds a chord from the key list", async () => {
    await mount();
    fireEvent.click(btn("Add item"));
    fireEvent.change(screen.getByLabelText("Kind"), { target: { value: "chord" } });
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Back out" } });
    fireEvent.change(screen.getByLabelText("Key to add"), { target: { value: "C-c" } });
    fireEvent.click(btn("Add key"));
    fireEvent.click(btn("Save item"));
    await waitFor(() => expect(screen.queryByTestId("tcs-form")).toBeNull());
    expect(lastPut().harnesses.claude.groups[0].items[0]).toEqual({ id: "u-back-out", kind: "chord", label: "Back out", keys: ["C-c"] });
  });

  it("editing a default makes an overlay item with the same id, labelled changed", async () => {
    await mount();
    fireEvent.click(btn("Edit /help"));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "/help me" } });
    fireEvent.click(btn("Save item"));
    await waitFor(() => expect(within(item("help")).getByText("changed")).toBeTruthy());
    expect(lastPut().harnesses.claude.groups[0].items[0]).toMatchObject({ id: "help", label: "/help me" });
  });

  it("resetting a changed item drops its overlay copy", async () => {
    await mount();
    fireEvent.click(btn("Edit /help"));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "/help me" } });
    fireEvent.click(btn("Save item"));
    await waitFor(() => within(item("help")).getByText("changed"));
    fireEvent.click(btn("Reset /help me"));
    await waitFor(() => expect(within(item("help")).getByText("default")).toBeTruthy());
    expect(lastPut()).toEqual({ harnesses: {}, hidden: {} });
  });

  it("warns inline about a newline in paste text and clears it for run", async () => {
    await mount();
    fireEvent.click(btn("Add item"));
    fireEvent.change(screen.getByLabelText("Text"), { target: { value: "a\nb" } });
    expect(screen.getByTestId("tcs-newline-warning").textContent).toContain("bracketed paste");
    fireEvent.change(screen.getByLabelText("Mode"), { target: { value: "run" } });
    expect(screen.queryByTestId("tcs-newline-warning")).toBeNull();
  });

  it("lists every client-side defect and sends nothing for an invalid form", async () => {
    await mount();
    fireEvent.click(btn("Add item"));
    fireEvent.change(screen.getByLabelText("Text"), { target: { value: "a\nb" } });
    fireEvent.change(screen.getByLabelText("Mode"), { target: { value: "run" } });
    fireEvent.click(btn("Save item"));
    const list = await screen.findByTestId("tcs-defects");
    expect(list.textContent).toContain("empty");
    expect(list.textContent).toContain("run-multiline");
    expect(fake.puts()).toHaveLength(0);
    expect(screen.getByTestId("tcs-form")).toBeTruthy();
  });
});

describe("hide / reorder / reset", () => {
  it("hides a default then unhides it from the hidden list", async () => {
    await mount();
    fireEvent.click(btn("Hide /compact"));
    await waitFor(() => expect(screen.queryByTestId("tcs-item-compact")).toBeNull());
    expect(lastPut().hidden).toEqual({ claude: ["compact"] });
    fireEvent.click(btn("Unhide /compact"));
    await waitFor(() => expect(screen.getByTestId("tcs-item-compact")).toBeTruthy());
    expect(lastPut()).toEqual({ harnesses: {}, hidden: {} });
  });

  it("moves an added item up, and offers no move for a shipped one", async () => {
    await mount();
    await addItem("first", "first");
    await addItem("second", "second");
    const order = () => screen.getAllByTestId(/^tcs-item-u-/).map((n) => n.getAttribute("data-testid"));
    expect(order()).toEqual(["tcs-item-u-first", "tcs-item-u-second"]);
    fireEvent.click(btn("Move second up"));
    await waitFor(() => expect(order()).toEqual(["tcs-item-u-second", "tcs-item-u-first"]));
    expect(lastPut().harnesses.claude.groups[0].items.map((i) => i.id)).toEqual(["u-second", "u-first"]);
    expect((btn("Move /help up") as HTMLButtonElement).disabled).toBe(true);
    expect((btn("Move /help down") as HTMLButtonElement).disabled).toBe(true);
  });

  it("deletes an added item", async () => {
    await mount();
    await addItem("gone", "gone");
    fireEvent.click(btn("Delete gone"));
    await waitFor(() => expect(screen.queryByTestId("tcs-item-u-gone")).toBeNull());
  });

  it("reset everything asks the confirm host and DELETEs only on accept", async () => {
    fake = installFakeCommands({ rev: 4, overlay: { harnesses: {}, hidden: { claude: ["help"] } } });
    await mount();
    fireEvent.click(btn("Reset everything to shipped defaults"));
    await screen.findByText("Reset all terminal commands?");
    confirmCancel();
    await waitFor(() => expect(screen.queryByText("Reset all terminal commands?")).toBeNull());
    expect(fake.deletes()).toHaveLength(0);
    fireEvent.click(btn("Reset everything to shipped defaults"));
    await screen.findByText("Reset all terminal commands?");
    confirmGo();
    await waitFor(() => expect(fake.deletes()).toHaveLength(1));
    expect(fake.deletes()[0].ifMatch).toBe('"4"');
    await waitFor(() => expect(screen.getByTestId("tcs-item-help")).toBeTruthy());
    expect(screen.getByText("Nothing hidden.")).toBeTruthy();
  });
});

describe("import / export", () => {
  const invalid = JSON.stringify({
    harnesses: { claude: { groups: [{ id: "g", label: "x", items: [{ id: "a", kind: "command", label: "y", text: "a\nb", mode: "run" }, { id: "a", kind: "reply", label: "z", text: "ok" }] }] } },
  });

  it("lists every error and makes no PUT for an invalid file", async () => {
    await mount();
    fireEvent.change(screen.getByLabelText("Overlay JSON"), { target: { value: invalid } });
    fireEvent.click(btn("Import"));
    const defects = await screen.findByTestId("tcs-defects");
    expect(defects.textContent).toContain("run-multiline");
    expect(defects.textContent).toContain("duplicate-id");
    expect(fake.puts()).toHaveLength(0);
  });

  it("reports unparseable JSON without a PUT", async () => {
    await mount();
    fireEvent.change(screen.getByLabelText("Overlay JSON"), { target: { value: "{nope" } });
    fireEvent.click(btn("Import"));
    expect((await screen.findByTestId("tcs-problem")).textContent).toContain("Not valid JSON");
    expect(fake.puts()).toHaveLength(0);
  });

  it("a valid import is one PUT after the confirm summary", async () => {
    await mount();
    const ok = { harnesses: { omp: { groups: [{ id: "g", label: "Mine", items: [{ id: "a", kind: "reply", label: "y", text: "yes" }] }] } }, hidden: { claude: ["help"] } };
    fireEvent.change(screen.getByLabelText("Overlay JSON"), { target: { value: JSON.stringify(ok) } });
    fireEvent.click(btn("Import"));
    expect((await screen.findByText(/1 item\(s\) in 1 group\(s\) and 1 hidden default\(s\)/)).textContent).toContain("replaces your whole overlay");
    expect(fake.puts()).toHaveLength(0);
    confirmGo();
    await waitFor(() => expect(fake.puts()).toHaveLength(1));
    expect(lastPut()).toEqual(ok);
  });

  it("export fills the text area with the stored overlay", async () => {
    fake = installFakeCommands({ rev: 1, overlay: { harnesses: {}, hidden: { omp: ["tree"] } } });
    await mount();
    fireEvent.click(btn("Export"));
    expect(JSON.parse((screen.getByLabelText("Overlay JSON") as HTMLTextAreaElement).value)).toEqual({ harnesses: {}, hidden: { omp: ["tree"] } });
  });
});

describe("server answers", () => {
  it("a 412 shows the reload banner, keeps the draft, and re-saves after Reload", async () => {
    await mount();
    fireEvent.click(btn("Add item"));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "mine" } });
    fireEvent.change(screen.getByLabelText("Text"), { target: { value: "mine" } });
    fake.external({ harnesses: {}, hidden: { claude: ["clear"] } });
    fireEvent.click(btn("Save item"));
    expect(await screen.findByTestId("tcs-stale")).toBeTruthy();
    expect((screen.getByLabelText("Label") as HTMLInputElement).value).toBe("mine");
    expect(fake.puts()).toHaveLength(1);
    fireEvent.click(btn("Reload"));
    await waitFor(() => expect(screen.queryByTestId("tcs-stale")).toBeNull());
    expect(screen.queryByTestId("tcs-item-clear")).toBeNull();
    fireEvent.click(btn("Save item"));
    await waitFor(() => expect(screen.queryByTestId("tcs-form")).toBeNull());
    expect(fake.puts()[1].ifMatch).toBe('"1"');
    expect(lastPut().hidden).toEqual({ claude: ["clear"] });
    expect(within(item("u-mine")).getByText("added")).toBeTruthy();
  });

  it("a 400 from the server lists its errors", async () => {
    await mount();
    fake.force(400, {
      type: "urn:kb:errors:bad-catalogue",
      title: "Bad Request",
      detail: "the overlay has 2 defect(s)",
      errors: [
        { code: "too-long", path: "/harnesses/claude/groups/0/items/0/label" },
        { code: "bad-key", path: "/harnesses/claude/groups/0/items/1/keys/0" },
      ],
    });
    fireEvent.click(btn("Hide /help"));
    const box = await screen.findByTestId("tcs-defects");
    expect(box.textContent).toContain("too-long");
    expect(box.textContent).toContain("/harnesses/claude/groups/0/items/1/keys/0");
  });

  it("a 5xx shows the problem detail", async () => {
    await mount();
    fake.force(500, { type: "urn:kb:errors:commands-io", title: "Internal Server Error", detail: "could not be written" });
    fireEvent.click(btn("Hide /help"));
    expect((await screen.findByTestId("tcs-problem")).textContent).toContain("could not be written");
  });

  it("an unchanged save says no change", async () => {
    await mount();
    // Another client already stored exactly what this click will send (same rev).
    fake.state.overlay = { harnesses: {}, hidden: { claude: ["help"] } };
    fireEvent.click(btn("Hide /help"));
    await waitFor(() => expect(screen.getByTestId("tcs-notice").textContent).toBe("No change."));
  });

  it("the SSE echo (an invalidation) never clobbers an open draft", async () => {
    await mount();
    fireEvent.click(btn("Add item"));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "typing" } });
    fake.external({ harnesses: {}, hidden: { claude: ["clear"] } });
    await qc.invalidateQueries({ queryKey: TERM_COMMANDS_KEY });
    await waitFor(() => expect(screen.queryByTestId("tcs-item-clear")).toBeNull());
    expect((screen.getByLabelText("Label") as HTMLInputElement).value).toBe("typing");
  });
});
