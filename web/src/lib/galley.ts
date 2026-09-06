// W2.14 — galley proof: pure hunk→section bucketing for the versions dock's
// "galley" diff-mode skin (VersionsPanel.tsx). The diff wire has NO section
// field (kb-server routes/versions.rs / kb-core vcs.rs) — text-mode
// `DiffLine.old_lineno`/`new_lineno` index rendered *prose blocks*
// (kb_core::parser::text_blocks — one line per block element, markup
// stripped), not source lines, so there is no honest line-space
// reconstruction available client-side. The only honest join (recon
// /tmp/w2-recon-errata-galley.md §2) is: fetch the served artifact HTML,
// pull its ordered `h1..h6[id]` heading list, and match each hunk's lines
// against those heading *titles* — monotonically forward, so a duplicate
// title, or a stray echo of an already-passed heading, never jumps a
// section bucket backward. Hunks before the first match land in an
// explicit "unplaced changes" group — never faked into a real section.
//
// Markdown artifacts: text-mode prose is derived from the SAME rendered
// HTML the heading list comes from (kb_core::versions::prose_at renders
// markdown → HTML → text_blocks for `.md`), so this title join holds for
// Markdown artifacts too. Raw mode's line-number-exact join does NOT (see
// the recon) — galley therefore stays text-mode-only; VersionsPanel always
// fetches the diff with `mode=text` on the wire for the galley view,
// regardless of the `raw` toggle's last selection.

import type { DiffHunk } from "../api/versions";

export type HeadingEntry = { id: string; text: string };

export type GalleySection = {
  /** Heading id, or `null` for the leading "unplaced changes" group. */
  id: string | null;
  /** Heading text, or `null` for the leading "unplaced changes" group. */
  title: string | null;
  hunks: DiffHunk[];
};

/** Collapse runs of whitespace to a single space and trim — mirrors
 * `text_blocks`'s inline-run whitespace-joining server-side, so a
 * multi-line heading ("Title\nHello world") compares equal to its
 * single-line diff-text echo ("Title Hello world"). */
export function collapseWs(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}

/** Ordered `h1`–`h6[id]` headings from a served artifact HTML document —
 * the client-side stand-in for a section field the diff wire doesn't have.
 * Uses `DOMParser`, so this is browser-only (no jsdom in the unit-test
 * harness — exercised via the real render path, not `*.test.ts`). */
export function extractHeadings(html: string): HeadingEntry[] {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const nodes = Array.from(
    doc.querySelectorAll("h1[id], h2[id], h3[id], h4[id], h5[id], h6[id]"),
  );
  return nodes.map((h) => ({
    id: h.id,
    text: collapseWs(h.textContent ?? ""),
  }));
}

/** Bucket text-mode hunks into ordered sections by matching each hunk's
 * lines against `headings`' titles, monotonically forward: once heading
 * index `i` is matched, only headings at index `> i` can match a later
 * hunk, so a hunk that merely echoes an already-passed title (or one
 * occurrence of a duplicate title) can never reopen or reorder an earlier
 * section. Hunks before the first match land in a leading
 * `{id: null, title: null}` "unplaced changes" bucket; empty buckets (no
 * hunks landed there) are dropped from the result. */
export function bucketHunksBySection(
  hunks: DiffHunk[],
  headings: HeadingEntry[],
): GalleySection[] {
  const titles = headings.map((h) => collapseWs(h.text));
  const sections: GalleySection[] = [{ id: null, title: null, hunks: [] }];
  let cursor = 0;
  let current = sections[0];

  for (const hunk of hunks) {
    const lineTexts = hunk.lines.map((l) => collapseWs(l.text));
    let matched = -1;
    for (let i = cursor; i < titles.length; i++) {
      if (titles[i] && lineTexts.includes(titles[i])) {
        matched = i;
        break;
      }
    }
    if (matched >= 0) {
      cursor = matched + 1;
      current = {
        id: headings[matched].id,
        title: headings[matched].text,
        hunks: [],
      };
      sections.push(current);
    }
    current.hunks.push(hunk);
  }

  return sections.filter((s) => s.hunks.length > 0);
}
