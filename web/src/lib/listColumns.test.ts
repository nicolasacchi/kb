import { describe, expect, it } from "vitest";
import {
  LIST_COLUMNS,
  ariaSort,
  columnsStorageKey,
  defaultColumns,
  gridTemplate,
  nextSort,
  normalizeColumns,
  parseColumns,
  serializeColumns,
  toggleColumn,
} from "./listColumns";

const col = (id: string) => LIST_COLUMNS.find((c) => c.id === id)!;

describe("listColumns (OL L5)", () => {
  it("defaults to title, folder, words, read, age in model order", () => {
    expect(defaultColumns()).toEqual(["title", "folder", "words", "read", "age"]);
  });

  it("models all ten columns", () => {
    expect(LIST_COLUMNS.map((c) => c.id)).toEqual([
      "title", "folder", "category", "words", "links", "refs", "kept", "caps", "read", "age",
    ]);
  });

  it("toggle adds in model order and never hides title", () => {
    expect(toggleColumn(defaultColumns(), "category")).toEqual([
      "title", "folder", "category", "words", "read", "age",
    ]);
    expect(toggleColumn(defaultColumns(), "words")).not.toContain("words");
    expect(toggleColumn(defaultColumns(), "title")).toContain("title");
  });

  it("normalize drops unknown ids, duplicates and forces title", () => {
    expect(normalizeColumns(["age", "age", "nope", 3])).toEqual(["title", "age"]);
    expect(normalizeColumns("x")).toEqual(["title"]);
  });

  it("storage round-trips and parse is total", () => {
    const v = toggleColumn(defaultColumns(), "caps");
    expect(parseColumns(serializeColumns(v))).toEqual(v);
    expect(parseColumns(null)).toEqual(defaultColumns());
    expect(parseColumns("{not json")).toEqual(defaultColumns());
    expect(parseColumns('{"a":1}')).toEqual(defaultColumns());
    expect(columnsStorageKey("k")).toBe("kb:list-columns:k");
  });

  it("grid template follows the visible set plus the action track", () => {
    expect(gridTemplate(["title"])).toBe("minmax(0, 1fr) 28px");
    expect(gridTemplate(defaultColumns()).split(" 28px").length).toBe(2);
    expect(gridTemplate(defaultColumns())).toContain("64px");
  });

  it("header clicks map onto the existing sort/dir grammar", () => {
    expect(nextSort(col("words"), "recent", "desc")).toEqual({ sort: "words", dir: "desc" });
    expect(nextSort(col("title"), "recent", "desc")).toEqual({ sort: "title", dir: "asc" });
    expect(nextSort(col("title"), "title", "asc")).toEqual({ sort: "title", dir: "desc" });
    expect(nextSort(col("age"), "words", "desc")).toEqual({ sort: "recent", dir: "desc" });
    expect(nextSort(col("folder"), "recent", "desc")).toBeNull();
  });

  it("aria-sort reflects the active sort only", () => {
    expect(ariaSort(col("title"), "title", "asc")).toBe("ascending");
    expect(ariaSort(col("words"), "title", "asc")).toBe("none");
    expect(ariaSort(col("folder"), "title", "asc")).toBeUndefined();
  });
});
