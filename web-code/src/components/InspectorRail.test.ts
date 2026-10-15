import { describe, expect, it } from "vitest";
import { railSections } from "./InspectorRail";
import { INSPECTOR_TABS } from "../hooks/useInspectorTab";

const none = { workspaceNotes: false, dossier: false, review: false };

describe("InspectorRail — five homes (OL-C4)", () => {
  it("the retired tabs' sections live in their new homes", () => {
    expect(railSections("understand", { ...none, dossier: true })).toEqual(["outline", "entity", "members"]);
    expect(railSections("notes", none)).toContain("trail");
    expect(railSections("facts", none)).toEqual(["facts", "comments"]);
  });

  it("members only appear with a dossier, workspace notes only with a workspace", () => {
    expect(railSections("understand", none)).not.toContain("members");
    expect(railSections("notes", none)).not.toContain("workspace-notes");
    expect(railSections("notes", { ...none, workspaceNotes: true })[0]).toBe("workspace-notes");
  });

  it("All still stacks every section of every home", () => {
    const all = railSections("all", { workspaceNotes: true, dossier: true, review: true });
    for (const tab of INSPECTOR_TABS) {
      if (tab === "all") continue;
      for (const id of railSections(tab, { workspaceNotes: true, dossier: true, review: true })) {
        expect(all, `${id} (from ${tab}) missing in All`).toContain(id);
      }
    }
  });
});
