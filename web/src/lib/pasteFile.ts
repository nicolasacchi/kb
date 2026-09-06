// C1 (v0.26 capture ergonomics) — synthesizes a `File` from the
// CaptureSheet paste-text box so pasted content rides the SAME `files`
// multipart field as picked/dropped files (kb-core's `capture.rs` engine —
// `base_stem` docs), unlike the share-sheet `text` field (a distinct
// `capture_url_stub` snippet path). The server re-slugs the name via
// `capture_slug` + `unique_filename`, so the stem built here only needs to
// be reasonable, not safe — no full slugging, just a stripped-down path
// separator so a pasted path-looking line can't do anything odd.

export type PasteFormat = "md" | "txt" | "html";

const EXT: Record<PasteFormat, string> = {
  md: "md",
  txt: "txt",
  html: "html",
};

const MIME: Record<PasteFormat, string> = {
  md: "text/markdown",
  txt: "text/plain",
  html: "text/html",
};

function stripSeparators(s: string): string {
  return s.replace(/[/\\]+/g, "");
}

/// First non-empty line of `text`, leading `#`/`##`/… heading markers
/// stripped, internal whitespace collapsed, capped ~60 chars. `null` when
/// there is no usable line (blank text, or a heading-only line that
/// collapses to nothing) — the caller falls back to "pasted-text".
function firstLineStem(text: string): string | null {
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const collapsed = line
      .replace(/^#+\s*/, "")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 60)
      .trim();
    return collapsed || null;
  }
  return null;
}

export function buildPastedFile(text: string, format: PasteFormat, title?: string): File {
  const trimmedTitle = title?.trim();
  const stem = stripSeparators(
    trimmedTitle && trimmedTitle.length > 0 ? trimmedTitle : (firstLineStem(text) ?? "pasted-text"),
  );
  // A stem made ENTIRELY of separators strips to "" — without the fallback
  // the name would be a bare ".md", which the server rejects with a 415
  // (a leading-dot file has no extension to resolve a pipeline from).
  const name = `${stem || "pasted-text"}.${EXT[format]}`;
  return new File([text], name, { type: MIME[format] });
}
