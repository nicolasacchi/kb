// Download helpers. The daemon serves both endpoints same-origin with a
// `Content-Disposition: attachment` header, so a temporary `<a download>`
// click is enough — no fetch/blob round-trip (generalises the inline
// `downloadText` pattern in CommentsPanel.tsx).

import { currentDaemonBase } from "../api/base";

/// Trigger a browser download of a same-origin URL whose response carries
/// `Content-Disposition: attachment`. Creates a transient anchor, clicks
/// it, and removes it. Used by the gallery card/row buttons, which can't
/// nest a real `<a download>` inside the card's outer `<Link>`.
export function triggerDownload(url: string): void {
  const a = document.createElement("a");
  a.href = url;
  // Hint a download; the server's Content-Disposition supplies the name.
  a.download = "";
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
}

/// URL for the raw source of a single artifact, as an attachment.
export function artifactDownloadUrl(kb: string, id: string): string {
  return `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/artifact/${encodeURIComponent(
    id,
  )}?download=1`;
}

/// URL for a `.zip` of every artifact in `folder` (descendant-inclusive).
/// An empty folder downloads the whole kb.
export function folderDownloadUrl(kb: string, folder: string): string {
  const q = folder ? `?folder=${encodeURIComponent(folder)}` : "";
  return `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/download${q}`;
}

/// URL for a session's portable `.kbsession.zip` (manifest + transcript) for
/// cross-machine `claude -r` resume via `kb sessions pull` / `kb sessions
/// rehydrate`. The daemon forces the redaction floor on a non-loopback bind.
export function sessionExportUrl(sessionId: string): string {
  return `${currentDaemonBase()}/api/sessions/${encodeURIComponent(sessionId)}/export`;
}

/// Save an in-memory Blob to disk under `filename` (transient object URL +
/// synthetic `<a download>`). Used for POST-fetched blobs (e.g. the share
/// bundle) that a plain `<a href>` can't reach.
export function saveBlob(filename: string, blob: Blob): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}
