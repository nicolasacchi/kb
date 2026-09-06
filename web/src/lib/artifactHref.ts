// Track U — build the path-based artifact permalink `/a/<kb>/<rel>`.
//
// The visible permalink mirrors the artifact's source-relative file path
// (e.g. `/a/platform/ideas/foo/bar.html`). The artifact id (a hash of the
// same path) stays internal — it's only used to build the iframe origin
// `<id>.artifacts.<suffix>`, which can't carry slashes.
//
// Each path segment is percent-encoded individually so the slashes remain
// literal separators that React Router's splat (`/a/:kb/*`) matches and
// the daemon's `{*path}` route resolves. A multi-page artifact's active
// sub-page rides as a `?p=` query param rather than extending the path,
// which would be ambiguous against the artifact path itself.
//
// RLs1 — the optional params grew into an options bag (the positional
// `page` string is still accepted):
//   page  → `?p=`     multi-page sub-page
//   sec   → `?sec=`   section deep link (heading id; scrolls + flashes)
//   list / entry → `?list=&entry=`  reading-list trail context (RLs5)
//   panel → `?panel=` Z4 inbox deep-link: which reader panel opens on mount
//                     (`comments`, or CT-F6's `versions`). detail.tsx
//                     consumes it once, then strips it from the URL —
//                     invariant #30's action-model still owns panel state
//                     thereafter. ONE param for "which panel opens": a
//                     second deep-link flag would be a second home for the
//                     same action.
//   turn  → `?turn=`   W3.E/S5: a session deep link — a numeric turn
//                     ORDINAL (resolved client-side via the outline) or the
//                     literal string `"end"` (targets the outcome anchor),
//                     OR (W7/LF-4) a `t-<uuid12>` render id used verbatim —
//                     see `ArtifactPane`'s turn-resolution effect.
//                     Serialised BEFORE `pane2` so `pane2` stays the LAST
//                     param on every URL (the #30 v0.29 byte-compat rule) —
//                     adding `turn` never moves it.
//   follow → `?follow=1` W7 (R15/LF-2): follow-mode entry (a list-row LIVE
//                     badge click, or the reader's own Follow chip
//                     surviving a same-session capture-landed reload).
//                     Reader VIEW STATE, like `?view=` — NOT part of the
//                     #35 gallery filter grammar. Joins the #31
//                     `ephemeralParams` set wherever a scroll-restoration
//                     key is derived. Serialised right after `turn`, still
//                     before `pane2` (byte-compat rule above).
//   at    → `?at=`    CT-F6 (RFC 7089 Memento): a unix-seconds instant to
//                     read this artifact AS OF. Reader VIEW STATE — it
//                     resolves the artifact's OWN version timeline to the
//                     nearest version at or before the instant and says so;
//                     it is NOT a gallery filter atom (#35) and touches no
//                     ranking. Serialised after `follow`, still before
//                     `pane2` (the byte-compat rule below), so every
//                     pre-CT-F6 golden string is unchanged when absent.
//   pane2 → `?pane2=` W3.P-b: the reader's SECOND pane (two-pane artifact
//                     compare). Carries the RAW value `lib/paneUrl.ts`'s
//                     `formatPane2` produces — build it there, never by
//                     hand. Appended LAST so every pre-W3 golden string is
//                     byte-unchanged when it is absent.
export type ArtifactHrefOpts = {
  page?: string;
  sec?: string;
  list?: string;
  entry?: string;
  panel?: "comments" | "versions";
  turn?: number | string;
  follow?: boolean;
  at?: number;
  pane2?: string;
};

export function artifactHref(
  kb: string,
  rel: string,
  pageOrOpts?: string | ArtifactHrefOpts,
): string {
  const opts: ArtifactHrefOpts =
    typeof pageOrOpts === "string" ? { page: pageOrOpts } : (pageOrOpts ?? {});
  const encRel = rel.split("/").map(encodeURIComponent).join("/");
  const base = `/a/${encodeURIComponent(kb)}/${encRel}`;
  // Hand-built query (not URLSearchParams, which encodes spaces as `+`)
  // so `?p=page%20two` stays byte-identical to the pre-RLs1 shape.
  const q: string[] = [];
  if (opts.page) q.push(`p=${encodeURIComponent(opts.page)}`);
  if (opts.sec) q.push(`sec=${encodeURIComponent(opts.sec)}`);
  if (opts.list) q.push(`list=${encodeURIComponent(opts.list)}`);
  if (opts.entry) q.push(`entry=${encodeURIComponent(opts.entry)}`);
  if (opts.panel) q.push(`panel=${encodeURIComponent(opts.panel)}`);
  // W3.E/S5 — BEFORE pane2 (see the opts doc comment above). `turn` is a
  // stable id/ordinal, never user text — no encodeURIComponent needed (a
  // `t-<uuid12>` render id is already URL-safe).
  if (opts.turn !== undefined) q.push(`turn=${opts.turn}`);
  // W7/LF-2 — right after turn, still before pane2 (byte-compat rule).
  if (opts.follow) q.push(`follow=1`);
  // CT-F6 — after follow, still before pane2. A unix integer, never user
  // text, so no encodeURIComponent (and `Math.trunc` so a float instant can
  // never reach the URL as `1.7e9`/`123.5`, which the daemon's `?at=`
  // integer grammar would reject).
  if (opts.at !== undefined) q.push(`at=${Math.trunc(opts.at)}`);
  // W3.P-b — LAST, always. `pane2`'s value is already the canonical raw
  // `formatPane2` string; the single encodeURIComponent here is what puts it
  // on the wire (paneUrl.ts's own escaping is field-internal).
  if (opts.pane2) q.push(`pane2=${encodeURIComponent(opts.pane2)}`);
  return q.length ? `${base}?${q.join("&")}` : base;
}
