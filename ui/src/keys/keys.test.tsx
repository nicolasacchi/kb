import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { KeyboardHelp, groupRows } from "./KeyboardHelp";
import { WhichKey } from "./WhichKey";
import type { KeyRow, KeyScopeView } from "./types";

const view: KeyScopeView = {
  prefix: ["Space"],
  scope: "reader",
  rows: [
    { keys: ["d"], verb: "Definition", group: "Go" },
    { keys: ["g"], verb: "3 more…", group: "Go", isGroup: true },
    { keys: ["f"], verb: "Find file", group: "Search" },
  ],
};

describe("WhichKey", () => {
  it("renders nothing without a view", () => {
    expect(renderToString(<WhichKey view={null} />)).toBe("");
  });

  it("renders the prefix, scope, one cell per next key and a heading per group change", () => {
    const html = renderToString(<WhichKey view={view} />);
    expect(html).toContain("data-kbc-whichkey");
    expect(html).toContain('class="kbc-whichkey__pending">Space</kbd>');
    expect(html).toContain('class="kbc-whichkey__scope">reader</span>');
    for (const k of ["d", "g", "f"]) expect(html).toContain(`data-kbc-whichkey-key="${k}"`);
    expect(html.match(/kbc-whichkey__group"/g)).toHaveLength(2);
    expect(html).toContain("kbc-whichkey__label is-group");
    expect(html.match(/is-group/g)).toHaveLength(1);
  });

  it("prints keycaps through renderKey but keeps the raw token in the data attribute", () => {
    const html = renderToString(
      <WhichKey
        view={{ prefix: [], rows: [{ keys: ["Space"], verb: "Leader" }] }}
        renderKey={(t) => (t === "Space" ? "SPC" : t)}
      />,
    );
    expect(html).toContain('data-kbc-whichkey-key="Space"');
    expect(html).toContain("<kbd>SPC</kbd>");
  });
});

const rows: KeyRow[] = [
  { keys: ["j"], verb: "Down", group: "Move", id: "move.down" },
  { keys: [], verb: "Ego graph", group: "Move", id: "graph.ego" },
  { keys: ["x"], verb: "Soon", group: "Edit", id: "edit.soon", planned: true },
];

describe("groupRows", () => {
  it("buckets by group in first-seen order and loses nothing", () => {
    const g = groupRows(rows);
    expect(g.map((x) => x.title)).toEqual(["Move", "Edit"]);
    expect(g.reduce((n, x) => n + x.rows.length, 0)).toBe(rows.length);
  });
});

describe("KeyboardHelp", () => {
  const base = { open: true, onClose: () => {}, rows };

  it("renders nothing when closed", () => {
    expect(renderToString(<KeyboardHelp {...base} open={false} />)).toBe("");
  });

  it("renders the sheet markup: scrim, dialog, rows with ids, nokey and planned marks", () => {
    const html = renderToString(<KeyboardHelp {...base} scopeName="Reader" footer="foot" />);
    expect(html).toContain("data-kbc-kbdhelp");
    expect(html).toContain('role="dialog"');
    expect(html).toContain('data-kbc-cmd="move.down"');
    expect(html).toContain("kbc-kbdhelp__nokey");
    expect(html).toContain("kbc-kbdhelp__row is-planned");
    expect(html).toContain("kbc-kbdhelp__planned");
    expect(html).toContain("kbc-kbdhelp__scopename");
    expect(html).toContain('class="kbc-kbdhelp__foot">foot');
    expect(html).not.toContain("data-kbc-preset");
    expect(html).not.toContain("Everywhere");
  });

  it("demotes global rows under an Everywhere divider and emphasises the primary groups", () => {
    const html = renderToString(
      <KeyboardHelp {...base} globalRows={[{ keys: ["?"], verb: "Help", group: "Global", id: "help" }]} />,
    );
    expect(html).toContain("kbc-kbdhelp__group--primary");
    expect(html).toContain("kbc-kbdhelp__break");
    expect(html).toContain("kbc-kbdhelp__cols--rest");
  });

  it("shows the profile switch only when profiles are given, with the current one selected", () => {
    const html = renderToString(
      <KeyboardHelp
        {...base}
        profile="plain"
        profiles={[
          { id: "vim", title: "Vim" },
          { id: "plain", title: "Plain" },
        ]}
      />,
    );
    expect(html).toContain("data-kbc-preset");
    expect(html).toMatch(/<option value="plain"[^>]*selected/);
  });

  it("uses the supplied close glyph and drops dialog semantics when not a sheet", () => {
    const html = renderToString(
      <KeyboardHelp {...base} asSheet={false} closeIcon={<i data-x="1" />} />,
    );
    expect(html).toContain('data-x="1"');
    expect(html).not.toContain('role="dialog"');
    expect(html).not.toContain("kbc-kbdhelp__scrim");
  });
});
