import { describe, it, expect } from "vitest";
import { buildPastedFile } from "./pasteFile";

describe("buildPastedFile", () => {
  it("uses the trimmed title for the filename stem when given", () => {
    const f = buildPastedFile("body text", "md", "  My Title  ");
    expect(f.name).toBe("My Title.md");
  });

  it("falls back to the first non-empty line, stripping heading markers", () => {
    const f = buildPastedFile("\n\n## Some Heading\n\nbody\n", "md");
    expect(f.name).toBe("Some Heading.md");
  });

  it("falls back to pasted-text when there is no usable line", () => {
    expect(buildPastedFile("", "md").name).toBe("pasted-text.md");
    expect(buildPastedFile("   \n\n   ", "txt").name).toBe("pasted-text.txt");
    expect(buildPastedFile("#\n\nbody", "md").name).toBe("pasted-text.md");
  });

  it("picks the extension and MIME type per format", () => {
    const md = buildPastedFile("x", "md", "t");
    expect(md.name).toBe("t.md");
    expect(md.type).toBe("text/markdown");

    const txt = buildPastedFile("x", "txt", "t");
    expect(txt.name).toBe("t.txt");
    expect(txt.type).toBe("text/plain");

    const html = buildPastedFile("x", "html", "t");
    expect(html.name).toBe("t.html");
    expect(html.type).toBe("text/html");
  });

  it("collapses whitespace and caps the stem length", () => {
    const long = "word ".repeat(30); // way over 60 chars once collapsed
    const f = buildPastedFile(long, "md");
    expect(f.name.length).toBeLessThanOrEqual(63); // 60 + ".md"
  });

  it("strips path separators from the stem", () => {
    const f = buildPastedFile("../etc/passwd\nbody", "md");
    expect(f.name).not.toContain("/");
    expect(f.name).not.toContain("\\");
  });

  it("falls back to pasted-text when the stem is nothing but separators", () => {
    expect(buildPastedFile("///\nbody", "md").name).toBe("pasted-text.md");
    expect(buildPastedFile("body", "md", "\\\\/").name).toBe("pasted-text.md");
  });
});
