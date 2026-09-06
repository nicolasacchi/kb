// Front-end derivations for fields the backend doesn't yet expose.
// These let cards render meaningfully on a kb that hasn't been
// re-indexed against the v0.6 parser yet. Once the backend ships
// the real fields (T1 / B1), `tagsFor` / `summaryFor` etc. prefer
// the server-side value and only fall back to derivation.

import type { DocSummary } from "../api/client";

// FNV-1a 32-bit. Stable, deterministic, fast. We use it to seed tag
// colors and as a placeholder ordering hint.
export function fnv1a(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0;
  }
  return h >>> 0;
}

// HSL color for a tag, derived from its name. Saturation and
// lightness are pinned so all tags share the same vibe (only hue
// varies). Works on dark and light themes.
export function tagColor(name: string): string {
  const hue = fnv1a(name) % 360;
  return `hsl(${hue}, 64%, 64%)`;
}

// Split a filesystem path into "directory tag" candidates. The
// parser may later overrule this with explicit `<meta name="kb-tags">`
// data, but until then this gives every artifact at least one or two
// signal tags drawn from its folder structure.
//
// Heuristic: take the last two non-empty directory segments before
// the file name, slugify them, dedupe. The closer-to-file segment is
// more specific, so it comes first: "incidents/checks/foo.html"
// becomes ["checks", "incidents"]. Generic top-level names
// ("artifacts", "kb", "html", "public") get filtered out — they
// carry no signal.
const GENERIC_DIRS = new Set([
  "artifacts",
  "kb",
  "html",
  "public",
  "docs",
  "src",
  "src-tauri",
  "node_modules",
  "dist",
  "build",
]);

export function pathToTags(path: string | null | undefined): string[] {
  if (!path) return [];
  const parts = path.split("/").filter(Boolean);
  // Drop the file name (last segment) — only directory parts become tags.
  if (parts.length <= 1) return [];
  const dirs = parts.slice(0, -1);
  const candidates: string[] = [];
  for (let i = dirs.length - 1; i >= 0 && candidates.length < 2; i--) {
    const slug = dirs[i].toLowerCase().replace(/[^a-z0-9-]+/g, "-");
    if (!slug || slug === "-" || GENERIC_DIRS.has(slug)) continue;
    if (candidates.includes(slug)) continue;
    candidates.push(slug);
  }
  // The closer-to-file segment is more specific — return more-specific
  // first so card tag-pills can accent it.
  return candidates;
}

// Slugify a free-text tag for optimistic display, mirroring the server's
// `parser::slugify_tag` (lowercase, runs of non-alphanumerics collapse to a
// single dash, leading/trailing dashes trimmed). The server re-slugs on
// write and the PATCH response is authoritative, so this only needs to be
// close enough to avoid a visible flash.
export function slugifyTag(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

// Convenience: server tags if present, otherwise path-derived.
export function tagsFor(doc: DocSummary): string[] {
  if (doc.tags && doc.tags.length > 0) return doc.tags;
  return pathToTags(doc.path);
}

// True for a hand-authored index/landing page (`index.html`). Excludes
// the auto-generated `kb index-page` output (`kb_category: "index-page"`),
// which the gallery treats as a generated artifact, not a real index.
export function isIndexPage(doc: DocSummary): boolean {
  if (doc.kb_category === "index-page") return false;
  return doc.path.split("/").pop()?.toLowerCase() === "index.html";
}

// Re-export: single home is `lib/time.ts` (QDUP-2). Call sites that
// previously passed `now` in unix-seconds must pass milliseconds instead
// (or omit — default is `Date.now()`).
export { relativeAge } from "./time";

// "New" = indexed within the last 24h. If indexed_at_unix is missing,
// fall back to mtime.
export function isNew(doc: DocSummary, now: number = Date.now() / 1000): boolean {
  const t = doc.indexed_at_unix ?? doc.mtime_unix;
  if (t == null) return false;
  return now - t < 86400;
}
