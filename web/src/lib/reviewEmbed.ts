import type { ReviewFile } from "../api/client";

// Portable round-trip — the SPA mirror of `kb_core::review::embed_into_html`
// / `extract_from_html` (crates/kb-core/src/review.rs). Keep the script id
// and the `</` escaping in lock-step with that file; the server's `import`
// endpoint and the Rust `extract` read the same shape.
export const KB_REVIEW_STATE_ID = "kb-review-state";

const SCHEMA = "kb-comments/1";

/// Inject (or replace) the review state as an inert
/// `<script type="application/json" id="kb-review-state">` block, returning a
/// standalone copy of `html`. Idempotent — a prior block is stripped first.
/// `</` is escaped to `<\/` so a comment body containing `</script>` can't
/// close the block early.
export function embedReviewIntoHtml(html: string, file: ReviewFile): string {
  const stripped = stripEmbeddedState(html);
  const safe = JSON.stringify(file).replace(/<\//g, "<\\/");
  const block = `<script type="application/json" id="${KB_REVIEW_STATE_ID}">${safe}</script>`;
  const lower = stripped.toLowerCase();
  const headIdx = lower.indexOf("</head>");
  if (headIdx !== -1) {
    return stripped.slice(0, headIdx) + block + "\n" + stripped.slice(headIdx);
  }
  const bodyIdx = lower.indexOf("</body>");
  if (bodyIdx !== -1) {
    return stripped.slice(0, bodyIdx) + block + "\n" + stripped.slice(bodyIdx);
  }
  return stripped + "\n" + block + "\n";
}

/// Parse an embedded `#kb-review-state` block back into a ReviewFile. Returns
/// `null` when no block is present; throws on a malformed block or an
/// unsupported schema. String-based (mirrors the Rust `extract_from_html`) so
/// it runs anywhere — it only ever parses our own self-emitted block, whose
/// JSON has `</` escaped, so the first `</script>` after the tag is the real
/// close.
export function extractReviewFromHtml(html: string): ReviewFile | null {
  const idPos = html.indexOf(`id="${KB_REVIEW_STATE_ID}"`);
  if (idPos === -1) return null;
  const open = html.indexOf(">", idPos);
  if (open === -1) return null;
  const close = html.indexOf("</script>", open);
  if (close === -1) return null;
  const raw = html.slice(open + 1, close).replace(/<\\\//g, "</").trim();
  if (!raw) return null;
  const file = JSON.parse(raw) as ReviewFile;
  if (file.schema !== SCHEMA) {
    throw new Error(`unsupported embedded review schema: ${String(file.schema)}`);
  }
  return file;
}

/// Remove a previously-injected block so a re-embed stays idempotent. Targets
/// only our own `<script…id="kb-review-state">…</script>` span.
function stripEmbeddedState(html: string): string {
  const needle = `id="${KB_REVIEW_STATE_ID}"`;
  const idPos = html.indexOf(needle);
  if (idPos === -1) return html;
  const start = html.lastIndexOf("<script", idPos);
  if (start === -1) return html;
  const endRel = html.indexOf("</script>", idPos);
  if (endRel === -1) return html;
  const end = endRel + "</script>".length;
  return (
    html.slice(0, start).replace(/[ \t]+$/, "") +
    html.slice(end).replace(/^\n/, "")
  );
}
