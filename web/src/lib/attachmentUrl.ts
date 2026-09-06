// Y-track — shared helpers for comment attachment URLs + inline-ref tokens.
//
// The inline-ref scheme is a custom `attachment:<aid>` URL placed in a
// comment body's markdown (`![alt](attachment:aid)` for images,
// `[label](attachment:aid)` for files). CommentBody resolves it to the
// daemon serve URL via a narrow `urlTransform` whitelist + img/a overrides.

import { currentDaemonBase } from "../api/base";

/// The custom URL scheme an inline attachment ref uses in a comment body.
export const ATTACHMENT_SCHEME = "attachment:";

/// Absolute serve URL for an attachment blob (built against the current
/// daemon base so it works against a remote daemon too).
export function attachmentServeUrl(kb: string, id: string, aid: string): string {
  return `${currentDaemonBase()}/api/kb/${encodeURIComponent(
    kb,
  )}/review/${encodeURIComponent(id)}/attachments/${encodeURIComponent(aid)}`;
}

/// `true` for a raster image content-type (rendered inline as an `<img>`;
/// everything else is a download chip). Mirrors the server's
/// `attachments::is_inline_image` intent on the client side.
export function isImageType(contentType: string): boolean {
  return contentType.startsWith("image/");
}

/// The markdown token that embeds an attachment inline in a comment body —
/// image syntax for rasters (→ `<img>`), link syntax otherwise (→ `<a>`).
export function attachmentToken(
  filename: string,
  aid: string,
  contentType: string,
): string {
  const label = (filename || "attachment").replace(/[\[\]]/g, ""); // keep markdown link text intact
  return isImageType(contentType)
    ? `![${label}](${ATTACHMENT_SCHEME}${aid})`
    : `[${label}](${ATTACHMENT_SCHEME}${aid})`;
}

/// Human-readable byte size for an attachment chip.
export function humanSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
