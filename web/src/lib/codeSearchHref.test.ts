import { describe, it, expect } from "vitest";
import { codeSearchHref } from "./codeSearchHref";

describe("codeSearchHref", () => {
  it("links into kb-code's search page with the query encoded", () => {
    expect(codeSearchHref("https://kbc.example.com", "theme family")).toBe(
      "https://kbc.example.com/search?q=theme%20family",
    );
  });

  it("trims trailing slashes on the base and whitespace on the query", () => {
    expect(codeSearchHref("http://127.0.0.1:4747//", "  resolve  ")).toBe(
      "http://127.0.0.1:4747/search?q=resolve",
    );
  });

  it("encodes characters that would break out of the query string", () => {
    expect(codeSearchHref("https://kbc.example.com", "a&b=c#d")).toBe(
      "https://kbc.example.com/search?q=a%26b%3Dc%23d",
    );
  });

  it("is null without a code_url (the kb has no linked kb-code)", () => {
    expect(codeSearchHref(null, "x")).toBeNull();
    expect(codeSearchHref(undefined, "x")).toBeNull();
    expect(codeSearchHref("", "x")).toBeNull();
  });

  it("is null for a code_url that is not an absolute http(s) URL", () => {
    expect(codeSearchHref("kbc.example.com", "x")).toBeNull();
    expect(codeSearchHref("javascript:alert(1)", "x")).toBeNull();
    expect(codeSearchHref("ftp://kbc.example.com", "x")).toBeNull();
  });

  it("is null for an empty or blank query", () => {
    expect(codeSearchHref("https://kbc.example.com", "")).toBeNull();
    expect(codeSearchHref("https://kbc.example.com", "   ")).toBeNull();
  });
});
