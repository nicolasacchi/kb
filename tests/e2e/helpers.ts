// Shared helpers for e2e specs. The daemon is spawned by global-setup.ts
// at this fixed port. Workers run in separate processes from globalSetup
// so we can't read globalThis state — the port is hardcoded here and
// the global-setup.ts uses the same constant.
export const PORT = 4737;
export const BASE = `http://127.0.0.1:${PORT}`;

// Track U — artifact permalinks are path-based (`/a/<kb>/<source_relative>`).
// Build a RegExp that matches such a URL, escaping the path's regex-special
// chars (`.html`, etc.). Trailing group allows an optional `?p=` page query.
export function artifactUrlRe(kb: string, sourceRelative: string): RegExp {
  const esc = sourceRelative.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`/a/${kb}/${esc}(\\?|$)`);
}
