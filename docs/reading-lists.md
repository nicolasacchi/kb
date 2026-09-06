# Reading lists (RL-track, v0.18)

Multiple named, **ordered** reading lists per kb. An entry targets a whole
artifact **or a section of it**, carries an optional note, and shows a
**derived read state** — the daemon already watches what you read
(RP-track), so a list keeps itself up to date. Replaces the v0.13
bookmarks feature (clean cutover, V0016 dropped the old table).

Surfaces: the SPA (`/lists`, the ContextBar/TocSpy add buttons, the trail
queue bar) and the CLI (`kb list …`) — the CLI is how Claude Code
generates lists. The HTTP routes are listed in the README + the generated
[api-routes.md](api-routes.md).

## Concepts

- **List** — `(title, description?, pinned, archived)` per kb. Titles are
  unique per kb (case-insensitive) so the CLI can address lists by title.
  Ids look like `l_9f3a21c4d0aa`.
- **Entry** — an ordered pointer at `(artifact, anchor?)` + note. `anchor`
  reuses the kb-comments `Anchor` (`{kind:"section", id}` is the common
  case; chapter/selection also work). Ids look like `le_02bd11aa34f0`;
  `position` is a dense 0-based index — what `kb list show` numbers.
- **Read state** (`unread | in_progress | read`) is **derived**:
  - whole-artifact entry → the artifact's scroll completion / words-read
    fraction (≥95% ⇒ read);
  - section entry → that section's dwell classification from the
    reading-progress capture (read / skim ⇒ in-progress / unseen ⇒ unread);
  - a manual override (`read` / `unread`) beats the derivation; clear it
    to fall back. The SPA's read-dot shows a ring when an override is set.
  - kbs with `reading_progress = false` simply stay `unread` until
    overridden.
- **Time estimates** — `~Nm` per entry (artifact `word_count`, or a
  per-section estimate for anchored entries) at 220 wpm; lists show
  total + remaining.
- **Stale anchors** — every reindex re-resolves entry anchors
  (`ListAnchorHook`); a section that no longer exists flags the entry
  (`⚠ stale` in the SPA/CLI, `list.entry.anchor_stale` over SSE). Fix with
  `kb list reanchor <list> <entry> --section <new-id>` or the SPA's
  re-anchor PATCH.

## Section permalinks & trails

- Any section is addressable: `/a/<kb>/<rel>?sec=<heading-id>` scrolls the
  artifact iframe there with an arrival flash (and beats scroll-resume).
  Copy these from the TOC rail's hover copy button. The heading ids are
  the authoring contract's stable ids (or the runtime's `kb-h-*`
  fallbacks) — the same id space comments anchor to.
- Opening an entry from a list adds `?list=&entry=` and mounts the
  **queue bar**: `≡ List · entry 3/7 · ~31m left · [mark read] ← prev
  next → ✕`. next/prev walk entry targets — across artifacts or between
  sections of one artifact (no iframe remount) — skipping tombstones.
  `n`/`p` mirror the buttons. The URL is refresh-safe and shareable.

## CLI workflow

```bash
kb list create "Async Rust, properly" --description "read in order"
kb list add "Async Rust, properly" research/async/from-scratch.html --note "foundation"
kb list add "Async Rust, properly" pinning.html --section why-pin   # §section entry
kb list show "Async Rust, properly"      # read-dots, §chips, ~minutes, ⚠stale
kb list move "Async Rust, properly" 3 --to 1
kb list update "Async Rust, properly" 2 --read        # manual override
kb list update "Async Rust, properly" 2 --clear-read  # back to derived
kb list reanchor "Async Rust, properly" 2 --section setup
kb list delete "Async Rust, properly" --yes
```

`<list>` = `l_…` id or unique title; `<entry>` = `le_…` id or the 1-based
index `show` prints; `<target>` = 12-hex id, source-relative path, or a
unique filename (resolved via `/lookup`, ambiguity lists candidates).

## A session's story as a list (CT-E5)

`POST /api/sessions/threads/save` (P8) materializes a narrative thread — a
run of sessions in one folder — as an editable list. With
`"narrative": true` (CLI `kb sessions save-thread <folder> --narrative`, and
the SPA's **save as list** button) the list is the session's *story* rather
than one entry per transcript. Per session, in the caller's order, four
lanes:

| # | lane | source | entry note |
|---|------|--------|------------|
| 1 | capture | the session transcript artifact itself | `session capture` |
| 2 | touched | `session_files` — edits before reads, then basename | `touched · edit` / `touched · read` |
| 3 | produced | lance rows whose `kb_session` names the session | `memory produced` |
| 4 | recalled | the `memory_recalls` ledger, oldest hit first | `memory recalled` (`· used` when a later turn named it) |

Each lane read is the *existing* per-session read (the same ones behind
`/sessions/{sid}/files`, `/memories`, `/recalls`), so newest-capture scoping
(invariant #11) comes for free. An artifact appearing in two lanes is told
once, at its earliest lane. A list is single-corpus — the entry enrichment
resolves against one `KbContext` — so a lane item in another corpus, or one
whose artifact no longer resolves (a hard-deleted recalled memory), is
**counted in the description, never listed as a tombstone** (the same rule
`resolve_target` applies to a manual `list add`). Per-session entries are
capped at `narrative::ENTRIES_PER_SESSION_CAP`; a save expands at most
`SESSIONS_PER_SAVE_CAP` sessions and 400s past that rather than truncating
silently.

The **description** carries the ordering contract, each session's id +
capture date, and — only when the kb has an inert `[kb.*] code_url` — the
kb-code session-diff link:

```
Narrative order: capture → files touched → memories produced → memories recalled.
Session 6f0e…  · captured 2026-05-24
Session diff: https://kbc.example.com/session/6f0e…/diff
```

That URL is *rendered*, never fetched: kb has exactly one call direction
with kb-code and it points the other way (invariant #2). No `code_url`, or
an invalid one, simply omits the line — never a dead link. Ordering,
notes, the description and the URL validation are pure
(`kb_core::sessions::narrative`, golden-tested); the route only supplies
the storage reads.

Omitting `narrative` keeps the pre-CT-E5 flat shape (one entry per
transcript, no description).

## Import / export — the kb-list/1 document

`kb list export <list> [--format md|json]` and
`kb list import <file|-> [--into L] [--mode replace|append] [--dry-run]`
round-trip a portable document. **One heredoc materializes a whole
curated list** — the Claude Code flow:

```bash
kb list import - --kb platform <<'EOF'
# Async Rust, properly

> From zero to executor internals — read in order.

1. [ ] [Async from scratch](research/async/from-scratch.html)
   Why futures desugar the way they do.
2. [x] [Pinning, finally explained](research/async/pinning.html#why-pin)
3. [ ] [Executor internals](research/async/executor.html#scheduler)
   The scheduler section is the payload.
EOF
```

Markdown grammar (pinned by golden tests in kb-core AND kb-cli):

| element | meaning |
|---|---|
| `# Title` | list title (required) |
| following `> …` blockquote | description |
| `<!-- kb-list {json} -->` | provenance (`schema`, `kb`, `list_id`) — import targeting: `--into` > existing `list_id` > create-new named by the title |
| `N. [ ]` / `N. [x]` item | one entry, document order authoritative (`[x]` ⇒ read override) |
| `[text](path#section-id)` | target: source-relative path (or bare 12-hex id); `#fragment` ⇒ Section anchor |
| trailing `<!-- kb-entry {json} -->` | machine state (`id`, `read_override`, full `anchor`) — beats the visible markdown |
| ≥3-space-indented lines | the entry's note |
| anything else | ignored (forward-compatible) |

Import semantics: `replace` (default) wipes and reloads — the true
round-trip, preserving entry ids/`created_at` when re-importing into the
same list; `append` skips targets already present. Unresolvable paths are
skipped and reported (`{ref, reason}`), never fail the import. Entry ids
clashing with ANOTHER list are reminted (cross-kb/cross-list portability:
artifact ids are path hashes, so a doc exported from one kb resolves in
any kb sharing the source-relative path). The JSON form (`?format=json`)
is the same data as one object; the export body IS a valid import body.

## Implementation map

`crates/kb-core/src/lists.rs` (types, derivation, section words, codecs) ·
`crates/kb-core/src/sessions/narrative.rs` (CT-E5 lane ordering + description) ·
`migrations/V0015__lists.sql` (+ V0016 drops bookmarks) ·
`storage/sqlite.rs` `list_*` CRUD · `enrich.rs` `ListAnchorHook` ·
`crates/kb-server/src/routes/lists.rs` · SPA `web/src/{routes/{lists,
listDetail}.tsx, components/lists/*, hooks/useLists.ts, lib/listTrail.ts}`
· CLI `crates/kb-cli/src/commands/list.rs`. Architecture invariant: see
the index in the repo `CLAUDE.md` (derived read-state; anchors reuse
`review::Anchor` with row-persisted staleness; list mutations never bump
the gallery index generation; one `list.updated` per import).
