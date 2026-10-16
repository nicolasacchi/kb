// OL S1 — the "Search code for “q” in kb-code" row in Cmd+K.
//
// kb never queries kb-code from the browser (kb-code's CORS is pinned to its
// doc-lens read routes on purpose), so the palette offers ONE link into
// kb-code's own search page instead. `codeUrl` is the active kb's operator-
// entered `[kb.*] code_url`; only an http(s) URL yields a link, so a bad
// value (or a `javascript:` URL typed into kb.toml) renders nothing rather
// than a dead or hostile href.

import { validCodeUrl } from "./linkifyCitations";

export function codeSearchHref(
  codeUrl: string | null | undefined,
  query: string,
): string | null {
  const q = query.trim();
  if (!q) return null;
  const valid = validCodeUrl(codeUrl);
  if (!valid) return null;
  let u: URL;
  try {
    u = new URL(valid);
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  return `${valid.replace(/\/+$/, "")}/search?q=${encodeURIComponent(q)}`;
}
