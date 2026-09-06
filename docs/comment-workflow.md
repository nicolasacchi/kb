# Comment workflow — review artifacts with Claude in realtime

kb's review system (`kb-comments/1`) lets you attach comments to any
indexed artifact and have Claude Code handle them. This doc covers the
whole lifecycle: how comments are stored, how to drive them from the
CLI, and how to run a **live loop** where you write comments in the
browser and watch Claude reply and fix them without a refresh.

- For the artifact-authoring rules that keep anchors stable across
  regen, see [`authoring-artifacts.md`](authoring-artifacts.md).
- For the HTTP routes behind these verbs, see the README's HTTP API
  section.

## How comments are stored

Each artifact's comments live in one JSON file:

```
<state>/<kb>/.review/<artifact_id>.json     # schema: "kb-comments/1"
```

The daemon owns this file. Comment mutations go through its fine-grained
endpoints (the CLI + SPA both send small deltas — add / reply / resolve /
unresolve / edit / delete), each running load → mutate → save under a
per-kb `review_lock` (sharded by kb name); there's no client `If-Match` (a targeted
append/flip can't conflict the way a whole-document overwrite could).
`GET` still emits an `ETag` for cheap revalidation. SPA panels subscribe
to changes over SSE. **Never edit it by hand;** go through the CLI or the
SPA.

A comment carries:

| field | meaning |
|---|---|
| `id` | `c_<hex>`, unique within the artifact |
| `status` | `open` or `resolved` |
| `author` | the ROLE: `you` (a human, via the SPA annotator) or `claude` |
| `user` | v0.34, optional — WHO, server-stamped from the resolved identity (never client-supplied). Absent on pre-v0.34 comments, which belong to the configured `[identity].operator`. A `claude` comment carries the user whose agent wrote it. |
| `anchor` | where it points (see below) |
| `body` | the comment text |
| `replies[]` | threaded `{id, author, user?, body, createdAt}` replies |

**Role vs identity (v0.34).** `author` answers *human or agent*; `user`
answers *which teammate*. Filter either way: `kb comments list --author
claude` (role) or `--user jordan` (identity). Editing or deleting a comment
*body* is owner-only — the daemon 403s (`urn:kb:errors:not-owner`) when the
caller isn't the stamped user — while resolve, reply, attach, and reanchor
stay open to everyone, so the triage loop below works unchanged across a
team. See [self-host.md → Team identities](self-host.md#team-identities-v034).

**Anchors**, ranked most→least stable across artifact regeneration:

- `file` — the whole artifact (never goes stale).
- `chapter` — a heading-text path like `Tuning > Virtual nodes`
  (fuzzy-matched by heading text).
- `section` — an element `id` / `data-kb-id`.
- `selection` — a CSS path + character offset + snippet (most fragile;
  re-bound by fuzzy text match on reindex).

When a reindex breaks an anchor the daemon emits `comment.anchor_stale`;
when a later edit re-binds it, `comment.anchor_resolved`. The SPA's
stale-anchors dashboard surfaces these fleet-wide.

**Turn-anchored comments on captures (W6, moonshots M2).** A captured
session transcript (`kb-category: memory-session`) is reviewable exactly
like any other artifact, but its `section`/`selection` anchors resolve
differently under the hood: since the artifact's stored bytes are the raw,
escaped JSONL (not the rendered turn cards a reader sees), the daemon
resolves those two anchor kinds against the INTERPRETED transcript (stable
`t-<uuid12>` turn ids + decoded prose) rather than the raw file — so a
comment anchored to a specific turn stays Fresh across reindexes the same
way an ordinary artifact's anchors do. Nothing about the CLI/SPA workflow
above changes: the same `add`/`reply`/`resolve`/`reanchor` verbs, the same
`comment.anchor_stale`/`comment.anchor_resolved` events. Two ways to anchor
a comment to one turn: toggle the SPA's annotate mode (`?cm=on`) and click
anywhere inside the turn card (any element with an `id` resolves to a
`section` anchor); or, without leaving read mode, click the small "comment
on this turn" (❝) affordance next to a turn's timestamp — it posts the
identical `cm:compose` message the annotator's click path sends, just
without requiring annotate mode first.

## The CLI surface

Every `kb comments` verb identifies the artifact two ways: positional
`<kb> <id>`, or `--path <file>` (a source-relative path or unique
filename, resolved via `/api/kb/{kb}/lookup`). `--kb` is optional when
only one kb is configured.

```bash
# Find an artifact id from a path or filename
kb find atlas.html                 # prints the 12-hex id
kb find atlas.html --json          # full hit: id, path, source_relative, folder, title

# List comments (open by default; --all includes resolved)
kb comments list --path atlas.html --json

# Read the source the comment points at, apply the fix, then:
kb comments reply <comment_id> --path atlas.html --body "Fixed the typo in §2."
kb comments resolve --path atlas.html <comment_id>     # or --all

# Add a comment yourself (Claude-Code write path; --author claude by default)
kb comments add --path atlas.html --body "Consider a legend." --anchor section:legend

# Attach quick-response buttons (repeatable --choice-json). The SPA renders
# these as one-tap buttons on Claude-authored open comments/replies; a click
# posts the choice's `reply` as a "you" reply (and resolves if `resolve`).
kb comments reply <comment_id> --path atlas.html \
  --body "Want me to add a legend?" \
  --choice-json '{"label":"Yes","reply":"Yes, add a legend","resolve":true}' \
  --choice-json '{"label":"No","reply":"No, leave it"}'

# Export an artifact's open comments as a Claude prompt
kb comments export --path atlas.html

# Land several mutations in ONE atomic call (all-or-nothing; one SSE update)
kb comments apply --path atlas.html --ops-json '[
  {"op":"add_reply","comment_id":"c_aa05","author":"claude","body":"fixed in §2"},
  {"op":"resolve","comment_id":"c_aa05"}
]'

# Make the artifact portable: bake comments INTO a standalone HTML copy, and
# read them back later (ids/statuses/replies preserved). The sidecar stays
# the canonical store — these are an export + restore.
kb comments export --path atlas.html --embed -o atlas-review.html
kb comments import atlas-review.html --path atlas.html   # --force to overwrite
```

`apply` ops mirror the fine-grained verbs (`add_comment`, `add_reply`,
`edit_comment`, `edit_reply`, `set_anchor`, `resolve`, `unresolve`,
`resolve_all`, `unresolve_all`, `delete_comment`, `delete_reply`) and
reference existing ids only — a batch can't reply to a comment it also
creates (the id is server-minted). The whole batch runs under one
`review_lock` acquisition with a single `save_atomic` + one `comments.updated`
SSE, so concurrent SPA views refetch once, not per-op.

`--choice-json` is repeatable; each value is a JSON object with a string
`label` (the button text), a string `reply` (markdown posted as the user's
reply on click), and an optional bool `resolve` (also flip the comment to
resolved). It works on both `add` and `reply`; choices are intended for
`--author claude` (the SPA only renders choice buttons on Claude-authored
open items). The SPA also shows a fixed canned set (Agree / Disagree / Tell
me more) on any open thread whose last message was Claude's.

`kb comments reply` is the Claude half of the conversation: it appends a
`Reply{author: claude}` via the fine-grained `…/comments/{cid}/replies`
endpoint (same `review_lock` mutation path as `resolve`, no `If-Match`),
so the SPA renders it live. The `comment_id` is its sole positional; name
the artifact with `--path` (or `--artifact-id` + `--kb`).

> Don't `resolve` until the fix is actually applied — the human uses
> status to track what's left. Don't invent comment ids; use only ones
> from `kb comments list` (the review file is the source of truth).

## Review-pass verdicts (`kb comments verdict`)

Individual comments are `open`/`resolved` (above); a **verdict** is a
separate, artifact-level signal for the review *pass* as a whole — the
mechanism that lets a review actually **end**, not just accumulate
resolved comments. It's a three-state enum:

| state | meaning |
|---|---|
| `comment` | still under discussion — a working/neutral note, no pass/fail signal |
| `approve` | the review pass is settled: it passed |
| `request-changes` | the review pass is settled: it's blocked pending a fix |

```bash
# Set (or update) the verdict
kb comments verdict approve --path atlas.html --note "Looks good, ship it."
kb comments verdict request-changes --path atlas.html --note "Fix the legend contrast."

# Clear it back to no verdict
kb comments verdict --clear --path atlas.html
```

Same targeting convention as every other `kb comments` verb: `--path`
(or `--artifact-id` + `--kb`; `--kb` optional with one configured kb).
`<state>` (`comment` | `approve` | `request-changes`, `request_changes`
also accepted) is required unless `--clear` is passed — the two are
mutually exclusive. `--note` is an optional short free-text note attached
to the verdict.

**Where it lives:** the verdict is a field (`verdict: {state, at, by,
note}`, or absent) on the SAME `.review/<artifact_id>.json` sidecar every
comment lives in — every mutation runs under the per-kb `review_lock`
(invariant #6), the same as `resolve`/`reply`. It is **not** a new store
and it is **not** folded into the artifact's own `<meta>` tags: meta
edits stay scoped strictly to `kb-tags`/`kb-category` (invariant #12).
Read it back via `GET .../review/{id}`'s top-level `verdict` field; the
`comments.updated` SSE event also carries it (piggybacked onto the
existing event, not a new SSE type), so a badge or inbox can react to a
verdict change without a follow-up fetch.

**The display mirror:** setting `approve` or `request-changes` also
mirrors onto the artifact's own kb-tags as `status-approved` /
`status-changes-requested` — a best-effort **display shortcut only** (a
source-write hiccup never fails the verdict-of-record write, which has
already landed in the review JSON by that point). `comment` and
`--clear` remove any prior `status-*` tag instead of adding one. Treat
that tag as read-only evidence of the verdict, not a second place to set
it — the review-file `verdict` field is the value of record; the tag is
derived, never the other way around.

**No-op discipline:** setting the same state with the same note twice is
a no-op — no file rewrite, no `comments.updated` SSE, no tag-shortcut
write. That makes `kb comments verdict approve` safe to re-run from a
script without checking the current state first.

## Flagging a memory (`kb memory flag`)

An agent that discovers a **recalled memory is wrong** mid-session used to
have nothing between silence and a full `kb forget`/`remember --supersedes`.
`kb memory flag` fills that gap — it's not a new store, just an ordinary
`kb-comments/1` comment (author `claude`, body `"[kb-flag] <reason>"`) posted
through the same `add` route as `kb comments add`:

```bash
kb memory flag <id> --reason "supersedes cd0cbc3b — the port changed" [--kb NAME]
```

`--kb` is optional — omitted, every `memory_scope`-configured corpus on the
daemon is searched for the id (same footprint `kb recall`/`kb memory triage`
cover). It **refuses** (no write) when an OPEN `[kb-flag]` comment already
exists on that memory, so re-running it mid-session doesn't pile up
duplicate flags for the same complaint — resolve the existing one first.

A flag surfaces two ways without you having to go looking for it:

- `kb recall` marks the hit `flagged: true` (the `kb-recall` hook prefixes
  the line with `⚠ disputed:`; the field rides the wire for the SPA to
  render) — display only, **never** a scoring input (a flagged and an
  unflagged hit at the same rank tie exactly).
- `kb memory triage` promotes it to the TOP of the hygiene queue with
  `reason_kind: "flagged"` — an operator-action item outranks every
  heuristic reason (below-floor, dormant, duplicate, superseded).

**Resolving a flag** is the ordinary comment workflow, not a new verb —
pick whichever fits:

```bash
# The memory was simply wrong/stale: write a corrected one, point back at
# the flagged id, then close the thread.
kb remember "the port is 4001, not 4000" --supersedes <id>
kb comments resolve <comment_id> --artifact-id <id> --kb <kb>

# The memory was fine after all / the fix doesn't need a new memory: just
# reply with the reasoning and resolve.
kb comments reply <comment_id> --artifact-id <id> --kb <kb> \
  --body "confirmed still accurate as of 2026-08 — leaving as-is"
kb comments resolve <comment_id> --artifact-id <id> --kb <kb>
```

Either way the flag disappears from `recall`/`triage` the moment the
comment is resolved — both surfaces only ever look at OPEN `[kb-flag]`
comments.

## Attachments (files & images)

A comment or reply can carry file/image attachments, referenced inline in the
body with `![alt](attachment:<aid>)` (image) or `[label](attachment:<aid>)`
(file). The daemon owns the blobs; the CLI never touches `.attachments/` on
disk — every verb goes over HTTP.

```bash
# Stage a file WITHOUT adopting it — prints the inline ref token to splice
# into a --body (the purest "give me a ref" primitive):
kb comments upload --path report.html diagram.png
#   ✓ staged a_1a2b3c4d5e6f (diagram.png, 24.1 KB) in research/report.html
#     inline:  ![diagram.png](attachment:a_1a2b3c4d5e6f)

# …then reference it when you add a comment:
kb comments add --path report.html --author claude \
  --body "the bug is in this diagram: ![diagram.png](attachment:a_1a2b3c4d5e6f)"

# Or one-shot: --attach stages each file, adopts it, AND appends its inline
# ref to --body:
kb comments add --path report.html --body "see the screenshot" --attach shot.png

# Attach to an already-posted comment (or a reply, with --reply <rid>):
kb comments attach c_abc123 chart.png --path report.html
```

Allowed types: PNG, JPEG, GIF, WEBP, PDF, and UTF-8 text — the daemon sniffs
the bytes (the filename/MIME you send is advisory) and rejects anything else
with a 415. Images render inline in the SPA panel; other files show as a
download chip. An attachment that no comment/reply ends up referencing is GC'd
(a never-adopted `upload` is reaped after a 24 h grace).

`kb comments export --out-dir <dir>` writes a self-contained bundle
(`review.<ext>` + an `attachments/` folder, refs rewritten to relative paths).
`kb share --with-comments` additionally publishes the rendered comment threads
+ their attachments into the static site — note this exposes otherwise-private
review state.

## The live loop (`kb comments watch`)

`kb comments watch` is an SSE-driven monitor for **new `you`-authored
review activity** — both new top-level comments *and* your replies on
Claude's comments (including quick-response button taps) — scoped to one
artifact or a whole folder. That two-way surfacing is what lets a comment
be a live back-and-forth. The round-trip is fully wired through SSE, so
the result of Claude's work shows up in the same browser tab:

```
you comment in SPA ─▶ POST /review ─▶ comments.updated (SSE)
                                          │
                            kb comments watch surfaces it
                                          │
              Claude reads source, edits HTML, replies, resolves
                                          │
                       POST /review ─▶ comments.updated (SSE)
                                          │
              SPA useReview refetches ─▶ reply + resolved render live
```

```bash
kb comments watch --path <artifact-or-folder> --json --once --timeout 1200
```

| flag | effect |
|---|---|
| `--path` | a single artifact, **or** a folder (all descendants, resolved once at startup via `/docs?folder=`) |
| `--json` | one JSON object per surfaced item (vs a human block); carries a `kind` |
| `--once` | exit after the first surfaced item — one per loop turn |
| `--timeout S` | exit 0 after `S` seconds with nothing surfaced |
| `--backlog` | also emit items already present at startup, not just new ones |

**Why scoping is client-side:** the `/api/events` stream has no
per-kb/per-artifact filter (its `?filter=` only honours `run:`). `watch`
resolves the scope to a set of artifact ids once, subscribes to
`comments.updated`, and on each in-scope event diffs that artifact's
review file to surface only comments it hasn't seen.

**Why it's echo-safe:** the watch only surfaces `you`-authored items.
Claude's own `reply` is `author: claude` (skipped), and `resolve` adds no
new `you` item — so neither makes the loop react to itself. Comments key
on `(artifact, comment_id)` and replies on `(artifact, reply_id)`, and
the seen-set persists across reconnects, so nothing is surfaced twice.

Surfaced JSON lines carry a `kind`. A new comment:

```json
{"kb":"canon","artifact_id":"07a5501697a0","source_relative":"fullscreen-viz.html",
 "kind":"comment","comment_id":"c_aa05…","anchor":{"kind":"section","id":"legend"},
 "body":"Make the legend keyboard-accessible.","created_at":"2026-…Z"}
```

A reply on one of Claude's comments (e.g. an `Apply` button tap) adds
`reply_id` + `in_reply_to` so you have the context you're answering:

```json
{"kb":"canon","artifact_id":"07a5501697a0","source_relative":"fullscreen-viz.html",
 "kind":"reply","comment_id":"c_aa05…","reply_id":"r_9f…","anchor":{"kind":"file"},
 "body":"yes, apply it","created_at":"2026-…Z",
 "in_reply_to":{"author":"claude","body":"Apply the fix?"}}
```


### `kb desk wait` — the same loop, scoped to a handoff

`kb desk wait [--path handoff/<slug>.<ext>] [--once] [--timeout SECS]` is
the desk-scoped entry to **this same watch loop** (it delegates to
`kb comments watch`; backoff and echo-safety stay single-sourced). Default
`--path` is the `handoff/` folder. Typical agent pair:

```bash
kb desk offer draft.md --as ticket-123
kb desk wait --path handoff/ticket-123.md --once --timeout 900
```

## Driving it from Claude Code (`/loop`)

Run `kb comments watch … --once --timeout 1200` inside a Claude `/loop`.
Each turn blocks until you post a comment, then Claude **triages** it:

- **A single, clear, actionable fix** → read the source HTML at
  `source_relative`, apply the edit, `kb comments reply` describing what
  changed, then `kb comments resolve`. Hands-off; you watch it land in
  the browser.
- **Needs discussion / ambiguous / large** → optionally spawn
  Explore/Plan subagents to investigate, then `kb comments reply` with
  the question or proposed options and **leave it open** for you to
  weigh in. Resolve only once it's actually settled.

Notes and limits:

- Folder membership is fixed at watch startup — restart the watch to
  pick up artifacts added to the folder afterward.
- On reconnect, `watch` rescans the in-scope reviews (with
  `Last-Event-ID` resume) so a comment posted during a reconnect gap is
  still surfaced.
- Concurrent SPA + CLI edits serialise server-side under `review_lock`
  (no client-side conflict to retry); the SSE refetch keeps every open
  view current.

## Where to look in the code

| piece | path |
|---|---|
| Comment / Anchor / Reply / Verdict structs | `crates/kb-core/src/review.rs` |
| `reply` / `verdict` verbs | `crates/kb-cli/src/commands/comments.rs` |
| `[kb-flag]` tag grammar (`FLAG_COMMENT_PREFIX`/`is_flag_comment`/`flag_reason`) | `crates/kb-core/src/memory.rs` |
| `kb memory flag` verb | `crates/kb-cli/src/commands/memory.rs` (`flag`) |
| triage `flagged` reason + recall `flagged` field | `crates/kb-server/src/routes/memory.rs` (`triage`/`recall`), `crates/kb-core/src/triage.rs` |
| verdict routes + status-tag mirror | `crates/kb-server/src/routes/comments.rs` (`set_verdict`/`clear_verdict`/`apply_status_tag_shortcut`) |
| `watch` loop (SSE scope + diff) | `crates/kb-cli/src/commands/comments_watch.rs` |
| review GET/POST + `comments.updated` emit | `crates/kb-server/src/routes/review.rs` |
| SPA live refetch | `web/src/hooks/useReview.ts`, `web/src/components/CommentsPanel.tsx` |
