// OL-C7 — the first lines of a file for Home's Continue hero. Pure so a
// vitest can pin the cap and the binary refusal without rendering.

import type { FileResponse } from "../api/types";

export const HERO_SNIPPET_LINES = 8;
/// A hard byte ceiling on what is handed to the highlighter: one minified
/// line must not turn the landing page into a paint job.
export const HERO_SNIPPET_MAX_CHARS = 1200;

/// The leading lines of `file` as plain text, or `null` when the file is not
/// text (base64) or is empty — the hero then shows its location only.
export function heroSnippet(
  file: Pick<FileResponse, "encoding" | "content">,
  maxLines: number = HERO_SNIPPET_LINES,
): string | null {
  if (file.encoding !== "utf8" || file.content.length === 0) return null;
  const lines = file.content.split("\n").slice(0, maxLines);
  const text = lines.join("\n").slice(0, HERO_SNIPPET_MAX_CHARS).trimEnd();
  return text.length > 0 ? text : null;
}
