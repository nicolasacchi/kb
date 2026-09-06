// U4 (v0.25 quick capture) — SPA client for `routes/capture.rs`'s
// `POST /api/kb/{kb}/capture`. Multipart, so no Content-Type header (the
// browser sets the boundary) — but `X-Requested-By: kb-spa` IS required:
// the server's `from_default` (routes/capture.rs) derives the stamped
// `from:` provenance tag from it (stripping the `kb-` prefix, so a capture
// from here is tagged `from:spa`). Mirrors `api/notes.ts`'s problem+json-
// aware fetch style, not `api/client.ts`'s `uploadFiles` helper, which
// sends no such header (would fall back to the endpoint's own `"api"`
// default).

import { currentDaemonBase } from "./base";
import type { CaptureItem } from "./generated/CaptureItem";
import type { CaptureResponse } from "./generated/CaptureResponse";

export type { CaptureItem, CaptureResponse };

type Problem = { title: string; detail?: string };

async function problem(r: Response): Promise<Error> {
  const ct = r.headers.get("content-type") ?? "";
  if (ct.includes("application/problem+json")) {
    const p = (await r.json()) as Problem;
    return new Error(`${p.title}: ${p.detail ?? ""}`);
  }
  return new Error(`${r.status} ${r.statusText}`);
}

export type CaptureUploadOpts = {
  files: File[];
  /// Falls back to the filename server-side when omitted/blank.
  title?: string;
  /// One tag per element; joined with `,` for the wire (the server's
  /// `split_tags` re-trims + drops empties on its side too).
  tags?: string[];
  /// Opt-in HTML sanitize (ammonia) — omit to take the endpoint's own
  /// default (off on this route; Markdown ignores it either way, U1
  /// no-op). Sent explicitly as `"true"`/`"false"` when set.
  sanitize?: boolean;
};

/// `POST /api/kb/{kb}/capture` — stage 1+ files into the kb's `capture/`
/// folder. Returned ids are the indexer's own derivation (#27), valid
/// before the watcher has actually indexed the file (search/gallery only
/// see it once that debounce fires).
export async function captureUpload(
  kb: string,
  opts: CaptureUploadOpts,
): Promise<CaptureResponse> {
  const form = new FormData();
  for (const f of opts.files) form.append("files", f, f.name);
  if (opts.title) form.append("title", opts.title);
  if (opts.tags && opts.tags.length > 0) {
    form.append("tags", opts.tags.join(","));
  }
  if (opts.sanitize !== undefined) {
    form.append("sanitize", opts.sanitize ? "true" : "false");
  }
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/capture`,
    {
      method: "POST",
      headers: { "X-Requested-By": "kb-spa" },
      body: form,
    },
  );
  if (!r.ok) throw await problem(r);
  return (await r.json()) as CaptureResponse;
}
