# kb-code-server — internals

Sibling of the workspace-root [`/CLAUDE.md`](../../CLAUDE.md) — kb-code is a
**separate daemon** (own binary, own auth posture, own migrations), not a
subsystem of `kb`/`kb-server`; root CLAUDE.md's crate-layout section and
[README.md](../../README.md)'s "kb-code" section are the feature-level surface
(HTTP routes, CLI verbs). This file holds the constraints that live entirely
inside this crate (and its `kb-code-cli`/`kb-lip` siblings) — the daemon's own
security posture, its git-argv discipline, its command/theme registries, and
its projection tables. Break one and the crate compiles but fails subtly at
runtime, exactly like the root file's own invariants list.
[`web-code/CLAUDE.md`](../../web-code/CLAUDE.md) is the other half of this
pair (D19 of the kb-code v7 design of record): the SPA's own invariants,
above all its keyboard-dispatch contract, which this crate's `commands/
registry.json` feeds.

## Orientation

The binary is `kb-code-server` (`src/main.rs`); `router.rs` builds the axum
app over several sub-routers merged into one `/api` nest (`api` —
`auth_bearer` reads/writes; `transcripts_api` and the working-tree mutation
routes — loopback-only; `review_remote` — the `[review] remote_mutations`
gate). `src/security/` (below) is layered on the MERGED nest, so every
sub-router inherits it with no per-route wiring. Config sections referenced
below live in `config.rs`: `[server] hostnames`/`git_fanout`, `[security]
secret_globs`/`strict_request_header`, `[doclens] origins`, `[review]
remote_mutations`. `commands/registry.json` and `themes/registry.json` are
this crate's two generated-registry sources (invariants 5–6); `migrations/`
is refinery-numbered and shares kb-sibling/1's epoch guard with every other
kb-code-server volume (`kb_core::sibling::refuse_if_volume_ahead` — a
migrated-ahead volume refuses to boot under an older binary rather than
silently regressing a safety feature, the 13.5h kbc rollback lesson root
invariant #2 records).

## Architecture invariants

1. **Security guards on `/api` are three crate-wide middlewares, chained
   outermost → innermost: `origin_host_guard` → `mutation_header_guard` →
   `audit_mutations`** (V70-A2, SEC-02/SEC-20; `src/security/`). Layered on
   the MERGED `/api` nest in `router.rs` (axum chains a `.layer()` call
   LAST = outermost), so `api`, `transcripts_api`, `review_remote` and both
   `doclens` sub-routers all inherit them with zero per-route wiring, and a
   route added to any of them inherits them too. The Origin+Host allowlist
   runs before EVERY other gate (`auth_bearer`, loopback-only,
   `review_gate`) — a rebound `Host` or a foreign `Origin` is refused
   regardless of what the route would otherwise permit; this is the fix for
   "loopback is not an authorization boundary, only 'same machine'" (any
   page in the operator's browser is a loopback peer too).
   `X-Kbc-Request: 1` is required on `POST`/`PUT`/`PATCH`/`DELETE` that
   carry an `Origin` header (a non-simple header, so a cross-origin page
   must win a CORS preflight this daemon answers only on the doc-lens
   sub-routers); `[security] strict_request_header = true` extends the
   requirement to Origin-less callers too. `audit_mutations` runs INSIDE
   both guards on purpose — a request the allowlist refused was never a
   mutation that happened, so it still lands in the ledger with its actual
   outcome (a 403), never silently dropped. Read `security::origin`'s
   module doc before touching admission on ANY route: it argues explicitly
   why there is no separate CSRF token (the header already IS the
   unguessable-to-a-cross-origin-page credential, and a token would add
   state without closing a gap the header leaves open).

2. **Read `security::full_path`, never `req.uri().path()`, inside any
   `/api`-nested guard.** `Router::nest` strips the matched prefix before
   the nested router's own middleware ever sees the request, so a
   path-keyed decision made on the stripped form is silently wrong: the
   mutation-header exemption list would never match `/api/doc-lens/pin`,
   and an audit-ledger row would record a route the caller cannot actually
   address. `full_path` prefers axum's own `OriginalUri` and falls back to
   re-prepending `/api` — caught once by `tests/security/audit_route.rs`'s
   route assertion, recorded here so the next path-keyed guard does not
   re-learn it.

3. **A validated ref/range is a TYPE, not a convention**
   (`git::revspec::{Revspec, RefRange}`, SEC-17). The two prior
   git-argv-injection fixes in this crate were both "a caller-supplied
   string reached git's option parser," patched by a free function
   (`reject_user_ref`) applied BY CONVENTION at each call site — which
   scales badly once v7 puts a user-supplied ref on every address, and
   which REJECTS `..`, so an implementer reaching for a range feature is
   pushed to bypass the validator rather than extend it. `Revspec::parse`
   and `RefRange::parse` are the ONLY constructors of their types (no
   escape hatch besides the crate-private `Revspec::trusted`, for refs this
   daemon minted itself, e.g. a resolved sha or a `refs/kbc/review/<id>/
   ps<n>`); a git helper that takes `&Revspec`/`&RefRange` cannot be called
   with an unvalidated `String` at all. `RefRange` validates its two
   ENDPOINTS separately (splitting on `...`/`..` FIRST, three-dot tried
   first since `a...b` also contains `a..`), so `..` stays legal as the
   separator and illegal inside a component — the exact bypass shape a
   naive "allow `..` in ranges" relaxation would open.
   `tests/security/git_argv_lint.rs` is a source-scan CI gate enforcing
   three things, and it is a scan with an allowlist, not a dataflow proof —
   read its own module doc before extending it: (a) the set of files
   allowed to spawn `git` (`Command::new("git")`) is PINNED
   (`GIT_SPAWNING_FILES`) — a new one fails the test with instructions, not
   a silent pass; (b) the `starts_with('-')` validation SHAPE may appear in
   exactly one file (`git/revspec.rs`), with one documented exception
   (`history::reject_dash_prefixed`, for a ref NAME read out of
   `for-each-ref` output, which a type cannot validate before it exists);
   (c) every caller-supplied PATHSPEC is preceded by `--` (with one
   documented non-case, `blame/timeline.rs`'s `-L a,b:path`, an option
   VALUE rather than a pathspec position). Migrate a new call site onto
   `Revspec`/`RefRange`; do not grow either allowlist to make a lint pass.

4. **Working-tree reads canonicalise BOTH sides and assert containment; the
   secret denylist is enforced at the FLOOR, unconditionally**
   (`security::paths::contained_abs_path`, `security::secrets`, SEC-13).
   `routes::safe_rel_path`'s lexical `..`/absolute-path rejection is
   necessary but not a containment proof: `std::fs` follows symlinks, so a
   repo-resident `docs/secrets -> /home/x/.ssh` turns a lexically-clean
   relative path into an escape. `contained_abs_path` canonicalises the
   DEEPEST EXISTING ancestor of a path and appends the literal remainder —
   so a file that legitimately does not exist (browsing a ref whose tree
   lacks it) still reaches the route's own honest 404 instead of being
   masked by a containment 403 — and refuses with
   `urn:kb:errors:path-outside-repo` on a genuine escape. It does not close
   the TOCTOU window (a symlink swapped in between the check and the read);
   that gap is a recorded scope decision (no portable `openat2
   (RESOLVE_BENEATH)` in this crate's dependency graph), not an oversight.
   `security::secrets::builtin_policy()` (`.env`, `*.key`, `*.pem`,
   `id_rsa*`, `config/master.key`, `config/credentials/*`, `*.sqlite3`, a
   handful more) is a `const`-derived, boot-free `static` enforceable from
   ANY of `read_repo_file`'s ~24 call sites across ten modules with no
   `AppState` to thread and none to forget; `[security] secret_globs` is
   ADDITIVE-ONLY (no repo/deployment config can re-open `.env`) and is
   checked only at the two surfaces that RETURN CONTENT and hold state
   (`GET /api/file`, `GET /api/pack`). Never make the built-in floor
   configurable or subtractable, and never enforce the operator's additions
   at a call site with no `AppState` — that reopens exactly the "the read
   route is directly addressable" hole SEC-13 names. The refusal names the
   matched PATTERN, never the bytes; `redaction_hint`'s content sniff is a
   HINT only (never redacts, never refuses) — a heuristic that silently
   mangled served source would be a correctness bug in a code reader.

5. **`merge-tree` writes into a per-request scratch ODB, never the browsed
   repo's own** (`history::scratch::ScratchOdb`, SEC-15). Until V70-A2,
   `git merge-tree --write-tree` grew the operator's repo by a tree and
   some blobs on every conflict check, unbounded and never GC'd.
   `GIT_OBJECT_DIRECTORY=<scratch>` redirects writes;
   `GIT_ALTERNATE_OBJECT_DIRECTORIES=<real objects>` (resolved via `git
   rev-parse --git-path objects`, so a LINKED WORKTREE — whose objects live
   in the common dir, not `<root>/.git/objects` — still resolves correctly)
   keeps reads working. A `Drop` guard removes the scratch dir on the
   ordinary and the error path; a boot-time `sweep_orphans` (unconditional,
   never age-gated — this process has created none yet at boot) catches
   whatever a `kill -9` left behind. Every git fan-out in this crate
   (merge-tree, ahead/behind, blame) runs under ONE daemon-wide
   `git_fanout` semaphore (`state::AppState::git_fanout`, default 4
   permits, `[server] git_fanout`) — this is a measured IO-bound RAID5
   host, and an unbounded N×M fan-out here is the same class of problem an
   unbounded reindex burst is on the kb side.

6. **`kbc-cmd/1`: `crates/kb-code-server/commands/registry.json` is the ONE
   declaration home for kb-code's whole keyboard/command surface — never a
   second copy of the truth.** Embedded server-side via `include_str!` and
   served ETag-validated at `GET /api/commands`; the SPA imports a
   GENERATED, checked-in `web-code/src/commands/registry.gen.ts`
   (`web-code/scripts/gen-commands.mjs`) rather than crossing the crate
   boundary at build time, because the Docker SPA build stage only has
   `web-code/` in its context. `web-code/src/commands/registry.gen.test.ts`
   regenerates the file in memory and asserts byte equality against the
   checked-in copy, so it cannot silently go stale. `kb-code commands
   doctor` is the CI gate — ALSO wired as a `#[test]` in kb-code-cli, so a
   bare `cargo test -p kb-code-cli` fails the build on registry drift, not
   just a manually-run verb — checking: every SHIPPED row's `cli` field
   names a real verb in THIS binary's clap tree or an honest
   `none:<reason>`; no row claims a reserved browser chord; a `scope:
   global` key may not be re-declared narrower (the internal-shadowing
   lint — three structural exemptions: `Escape`, a row carrying `vim_kind`,
   and a row that names a `browser_reserved` caveat); every `Escape` row
   carries a distinct `dismiss_order` and none of them navigates; and every
   conflict between two COACTIVE scopes at the same modal depth appears in
   BOTH entries' `ratified_conflicts`, or the build fails. A `dispatch:
   "central"` row is fired by the SPA's `CommandRoot`; a `dispatch:
   "surface"` row is owned by whichever route/component renders it (the CM6
   vim layer, for a row carrying `vim_kind`) — that field is a CONTRACT for
   readers and for `commands doctor`, not a second code path in this crate.
   See [web-code/CLAUDE.md](../../web-code/CLAUDE.md)'s keyboard section for
   the SPA-side half of this contract — it is the one that actually keeps
   breaking.

7. **`kbc-theme/1` follows the identical one-source-of-truth shape as
   cmd/1** — `crates/kb-code-server/themes/registry.json` (27 themes × 26
   OKLCH-shaped anchors, Rosé-Pine role vocabulary) served verbatim via
   `include_str!` at `GET /api/themes`, an ordinary `auth_bearer` read;
   `web-code/scripts/gen-themes.mjs` generates the checked-in
   `src/themes/registry.gen.ts` + `themes.gen.css`, pinned by
   `registry.gen.test.ts` the same byte-equality way commands are. Add or
   edit a theme by editing `registry.json` and regenerating — never by
   hand-editing the generated TS/CSS.

8. **`GET /api/schemas` is a curated STARTER set, never a corpus-wide
   schema dump, and its mirrors are hand-maintained** (`api_schemas.rs`,
   D20). Four names ship (`identity`, `healthz`, `scopes`, `repos-entry`),
   each a small struct DEFINED IN THIS MODULE that mirrors — by hand, not
   generated from — the shape of a real response type, with every field
   simplified to something `schemars` supports natively (`PathBuf`→
   `String`, etc.). None of these mirror structs is ever constructed (their
   only consumer is `schemars::schema_for!`), hence the module-scoped
   `#![allow(dead_code)]`; growing the registry is additive by construction
   (the `NAMES` list plus one `match` arm in `schema_json`/`example_json`),
   but there is NO drift test tying a mirror to the real struct it
   describes — a field renamed on `routes::RepoListEntry` without a
   matching edit to `RepoEntrySchema` goes unnoticed until someone trusts
   the wrong schema. Deriving `JsonSchema` directly on a real response
   struct was rejected here because it would cascade the derive across
   every field type it carries transitively (`ScipStatus`,
   `RepoIntelStatus`, `kb_core`'s own types, …) — a correctness bet this
   unit's one-shot-compile-check budget could not afford to get wrong.
   Expanding real-struct coverage (or adding a drift test) is future work,
   named here rather than silently assumed.

9. **A `reading_sets` row with `kind = 'workspace'` IS a workspace — not a
   new entity** (V0028, V70-A10, D26). Mirrors root invariant #16's posture
   (notes are Markdown artifacts, not a new table) and the design's own
   stated plan for `kbc-seq/1` (a v7.1 projection LAYER over the existing
   `reading_sets`/`bookmarks`/`canvas_sets` tables — this migration widens
   one of them rather than pre-empting that unification). `kind` is
   Rust-validated (`reading_sets::is_valid_set_kind`), never a SQL `CHECK`
   — this crate's house convention for a small stringly-typed vocabulary
   that must stay relaxable without a table rebuild (`annotations.rs`'s
   `anchor_kind`/`intent` do the same). `desk_json` is stored OPAQUE and
   VERBATIM — never parsed or interpreted server-side (the `<template
   id="kb-prompt">`/kb-share precedent: persist a client payload without
   becoming a second copy of the client's own schema), capped at
   `reading_sets::MAX_DESK_JSON_BYTES` (64 KiB) by the route.
   `annotations.set_id` is **TEXT**, matching `reading_sets.id`'s TEXT
   primary key (`set_` + 12 hex) — NOT the INTEGER shape `review_id` uses
   (a recorded deviation from the original unit brief, which specified
   `INTEGER` by analogy without checking `reading_sets`'s own PK type). A
   reply inherits its parent's `set_id` via the SAME `inherit_scope_field`
   ladder `review_id`/`ps_number`/`side` already use, which is what keeps
   the cascade-delete in `Store::delete_reading_set` a single `WHERE
   set_id = ?` rather than a parent/reply two-step. No SQL foreign key
   (same precedent as `review_id`/`parent_id`) — the cascade lives in Rust,
   inside the SAME transaction as the set and its spans.

10. **The daemon never spawns a non-git process** (P8 of the v7 design;
    standing since `scip run`'s own precedent). An exec-shaped
    augmentation — a future aug-lane, a rails-runner snapshot, `entity
    confirm` — runs from the CLI on the operator's own box and POSTs a
    claim over loopback; it is never invoked in-process by
    kb-code-server itself. This is a structural rule about WHO may spawn a
    child process from this crate, not a configuration default that could
    be flipped — see `security/mod.rs`'s module doc and the git-argv lint
    (invariant 3) for the adjacent discipline over the process this crate
    DOES spawn.

11. **A symbol/highlight-cache write must purge every OTHER salt of the
    SAME language for a blob before writing the fresh derivation**
    (V70-A3X's stale-salt fix, `store.rs`/`lang.rs`). `replace_symbols`,
    `replace_occurrences` and `put_highlights` all do this on WRITE; the
    matching READS (`symbols_for_repo`, the three occurrence
    `*_in_repo` queries) restrict to the blob's CURRENT salt, falling back
    to "everything" only when no row for that blob is current yet (which
    is what keeps ad hoc `"rust@1"`-style test fixtures byte-identical). A
    grammar/query bump (a `symbol_salt`/`highlight_salt` change) that
    writes without purging the old salt first leaves duplicate,
    contradictory rows for the same blob under two salts, permanently.
    `Store::sweep_stale_salt_derived` is a one-time boot sweep for rows a
    PRE-FIX binary already left behind — it is a remedy for old damage, not
    a substitute for purging on every write.

12. **The Rails lens's three read/write contracts, fixed together as one
    unit and easy to regress independently** (V70-A1, R1–R3,
    `frameworks/rails/`). (a) `replace_rails_edges`'s DELETE — the step
    before every re-derive, and `delete_file` — must be scoped to
    `(repo_id, src_path)`, matching every READ's key; scoping it to
    `(blob_hash, salt)` alone (the bug this unit fixed) leaves the
    PREVIOUS blob's rows live forever after an edit and skips them
    entirely on a file delete, so `GET /api/usages` and `GET
    /api/framework/edges` report phantom edges indefinitely. The INSERT
    stays `INSERT OR REPLACE` (the table's pre-existing `UNIQUE(blob_hash,
    salt, ordinal)` constraint is not path-scoped, so a plain `INSERT`
    would turn a rare same-content-different-path collision into a hard
    ingest error). (b) `frameworks::rails::i18n::locale_index_for` is a
    process-local cache KEYED BY `repo_root`, invalidated by a cheap
    fingerprint (path+size+mtime per locale file, never a content read) —
    a full reconcile must build the locale index ONCE, not once per
    dispatched file (measured at ~519 MB of re-read YAML per reconcile on
    the acme-shop fixture before this fix). Its own `cfg(test)` build
    counter is per-repo_root for the same reason `sweep_stale_salt_derived`
    testing needs isolation: a process-global counter false-fails under
    parallel test execution when an unrelated test's own Rails extraction
    runs concurrently. (c) the REVERSE (`dst_path`) usages lookup must NOT
    gate on `rails_lens_relevant_path` — that predicate is about the
    SOURCE side, and a destination-only file (a locale yml, a Stimulus
    controller) legitimately receives inbound edges with no source-side
    relevance of its own; `resolve.rs`'s own SRC-path-keyed tier query is a
    different, correct read and is unaffected by this rule.

## When to update this file

Add an invariant here when it lives entirely inside `kb-code-server` (or its
`kb-code-cli`/`kb-lip` siblings) and a contributor could break it without
touching kb proper or the SPA. Anything that is really about the SPA's own
keyboard/navigation/theme CONSUMPTION of what this crate serves belongs in
[web-code/CLAUDE.md](../../web-code/CLAUDE.md) instead — if you are unsure
which file owns a rule that spans both (a registry shape, a wire contract),
default to documenting the SERVER side of it here and the CONSUMPTION side
there, cross-linking rather than duplicating. Root
[`/CLAUDE.md`](../../CLAUDE.md) only needs a one-line pointer update here on a
crate-layout change; it does not enumerate this crate's own invariants (the
35-slot budget in root CLAUDE.md's invariant index is for kb-proper's
cross-cutting invariants and does not apply to this file).
