# kb-code-server — internals

Sibling of the workspace-root [`/CLAUDE.md`](../../CLAUDE.md) — kb-code is a
**separate daemon** (own binary, own auth posture, own migrations), not a
subsystem of `kb`/`kb-server`; root CLAUDE.md's crate-layout section and
[docs/kb-code.md](../../docs/kb-code.md) are the feature-level surface
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
   *V76-R4f amendment:* the same posture applies to the daemon's OUTBOUND
   half — a daemon never assumes a sibling; `[kb_daemon]`
   (`config::KbDaemonSection`) is DISABLED unless the operator configures
   `url`, so a throwaway install with a minimal `[[repos]]`-only toml
   never federates read-only lookups against whatever happens to answer on
   kb's well-known port.

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
   *V75-M3 amendment (D15, `history::radar`):* the N×M fan-out this
   invariant anticipated now exists — the conflict radar merges up to
   `radar::MAX_PAIRS` (40) candidate branches against one target — and the
   rules it must obey are the same three, made explicit. **ONE scratch ODB
   per PAIR, created and dropped inside the loop** (a 40-pair fan-out that
   held 40 directories open would trade the unbounded-growth bug for an
   unbounded-fd one); **the pair cap is HARD and the response says
   `computed` of `candidates`** rather than truncating to look complete;
   and a scratch-dir creation FAILURE is now its own error variant
   (`HistoryError::ScratchUnwritable` → `urn:kb:errors:scratch-unwritable`,
   `503`, naming the DIRECTORY), because the browsed repo needs no write
   access at all — that is exactly what the redirection above bought — so
   the only directory a merge-tree lane can be blocked on is the daemon's
   OWN state dir, and reporting that as "git failed" sends the operator
   looking in the wrong place. `merge_check` inherits the typed refusal by
   going through the same constructor. The radar also reads its result tree
   BACK out of the scratch ODB (`git show <tree>:<path>`) to COUNT conflict
   markers: that read must carry the same two env vars as the write, which
   is why both go through one `git_in_scratch` helper rather than two call
   sites that can drift, and it is budgeted separately
   (`radar::MAX_HUNK_PROBES`) with the field ABSENT past the budget, never
   a guessed zero.

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

11. **Two salts, two independently-gated families; a cache write purges
    every OTHER salt of the SAME language AND family for that blob; and a
    salt bump is a MEASURED decision** (V70-A3X, V72-B0, V72-H2b —
    `store.rs`/`lang.rs`/`ingest.rs`/`reextract.rs`). *V72-H2b amendment
    (2026-09-07, D7 + D16), stated first because everything below is now
    read per family:* `LangInfo` carries `symbol_salt`
    (`{id}@{grammar}+qN` — keys `symbols`, `occurrences`, `import_specs`,
    `call_sites`, `type_relations`) and `highlight_salt`
    (`{id}@{grammar}+hN+rolesM` — keys `highlights`), and `salt_for` is
    the ONE function that turns a `SaltFamily` into a string, so no call
    site picks the wrong one by habit. `M` is
    `highlight::ROLE_TABLE_VERSION`, pinned to every language's salt by
    test — widening `HighlightClass` is one edit that invalidates every
    painted row and cannot be forgotten for one language. The two sets are
    disjoint by construction, which is what keeps "is this row's salt in
    the CURRENT set" a per-FAMILY question with no join back through
    `files.lang`; `current_salt_cte` therefore TAKES a family, and passing
    the wrong one to a read hides every current row of the other family —
    passing it to the SWEEP deletes them. `store::SWEEP_TABLES` is the
    declaration that keeps the two apart (one pass per `(table, family)`,
    plus the `family` column predicate on the one table holding both), and
    `lib::salt_set_fingerprint` folds BOTH sets, so a highlight-only bump
    re-arms the V72-B0 marker rather than landing under a `done` one. No
    migration was needed for the split itself: two strings in the same
    `salt` TEXT column each family's table already had. **Two further
    rules.** (i) *The gate is a MARKER, never a row count.* `derived_status`
    (V0037 — and the NUMBER is a rule, see below; `(blob_hash, family,
    salt) -> rows`) is written inside the same
    transaction as the derivation it describes, and `Store::is_derived` —
    its EXISTENCE — is what `ingest::index_file`'s two gates ask.
    `has_symbols`'s `COUNT(*) > 0` cannot distinguish "not derived" from
    "derived, and zero rows was the honest answer", so every zero-symbol
    blob re-parsed on every visit forever (ERB, SCSS, a comment-only Rust
    file, a heading-less Markdown file — V72-H1 reported the ERB case and
    read it as being about ERB). Any FUTURE pass whose honest output can be
    empty owes itself a marker rather than a `has_*` count; `occurrences`,
    `import_specs`, `call_sites` and `type_relations` still carry the old
    shape and are a NAMED, unfixed instance of the same defect. (ii) *Price
    a bump before you ship it.* `kb-code reextract --bill` /
    `GET /api/reextract/bill` measure what a salt change costs on the
    configured mirror — exact census, timed sample, extrapolation labelled
    as one. D7 asks for the number per milestone. There is deliberately no
    verb that PERFORMS a re-extract: that would be a whole-corpus
    maintenance pass with a trigger, which is exactly what (a)/(b) below
    exist to keep out of this daemon. **(iii) A new migration takes the
    embedded set's CURRENT MAX + 1 — never a reserved slot, never a
    gap-fill.** The milestone ledger used to pre-assign numbers to
    in-flight units, on the belief (frozen into
    `V0034__review_docs_and_findings_v2.sql`'s own header, which must NOT
    be edited — changing an applied migration's bytes is this invariant's
    other trap) that "refinery applies by version, so a gap is inert". A
    gap IS inert; a gap-FILL is not. `refinery-core`'s
    `traits::get_unapplied_migrations` selects only `version > current`,
    and `abort_missing` — default `true`, which `Store::open` uses — makes
    an embedded migration BELOW the applied maximum a hard
    `MissingVersion`: a volume already migrated past a reserved slot
    REFUSES TO BOOT when the gap-fill merges. Not a skipped table, a dead
    daemon, and the same class of outage as the 13.5 h rollback. V72-H2b
    was holding such a slot (V0033) when this was found; it renumbered to
    V0037 and left 33 permanently empty.
    `store::tests::v72_b1::embedded_migration_versions_are_contiguous` now
    enforces it, with `PERMANENTLY_SKIPPED_VERSIONS` as the debt ledger
    for 33 — a list that may never grow.
    The original rule, unchanged in substance: `replace_symbols`,
    `replace_occurrences` and `put_highlights` all do this on WRITE; the
    matching READS (`symbols_for_repo`, the three occurrence
    `*_in_repo` queries) restrict to the blob's CURRENT salt, falling back
    to "everything" only when no row for that blob is current yet (which
    is what keeps ad hoc `"rust@1"`-style test fixtures byte-identical). A
    grammar/query bump (a `symbol_salt`/`highlight_salt` change) that
    writes without purging the old salt first leaves duplicate,
    contradictory rows for the same blob under two salts, permanently.
    V72-H2b: that purge is scoped by FAMILY as well as by language prefix
    — a symbol-salt bump that erased the highlight marker would silently
    re-paint a corpus it was never supposed to touch.
    `Store::sweep_stale_salt_derived` is a one-time boot sweep for rows a
    PRE-FIX binary already left behind — it is a remedy for old damage, not
    a substitute for purging on every write. *V72-B0 amendment (2026-09-06,
    after a production boot hang):* "one-time" and "boot" are now literally
    true, and the sweep NEVER runs on the boot path. It is driven from a
    background `spawn_blocking` task (`lib::spawn_stale_salt_sweep`) in
    bounded, resumable pages (`Store::sweep_stale_salt_page`, 128 distinct
    `files.blob_hash` per short transaction), gated by a completion marker
    (`<state>/kb-code/salt-sweep.marker`: the FNV-1a fingerprint of the
    sorted `lang::ALL_LANGS` salt set, plus `done` or `after=<blob_hash>`),
    and capped by a per-boot wall-clock budget that persists its cursor and
    resumes on the next boot. The original shape — one un-paged
    `blob_hash IN (SELECT blob_hash FROM files)` DELETE per table, in ONE
    transaction, called INLINE before `TcpListener::bind` — cost O(every
    live blob) random index seeks per table on every single boot; on the
    production store (~165k files, ~1.6M symbol rows, spinning disks at
    ~50 random reads/s) that is hours, and the daemon logged its migrations
    and then never bound its port. Two rules follow, and breaking either
    reintroduces an outage rather than a slowdown: **(a)** no whole-corpus
    maintenance pass may ever sit between `Store::open` and the bind, and
    **(b)** a background pass must stay PAGED — the store has ONE connection
    mutex, so an hours-long transaction on a background thread does not fix
    the outage, it only moves it from "never binds" to "binds and answers
    nothing". The marker is deliberately a sidecar FILE, not a table: a new
    migration would bump the refinery epoch and re-arm the kb-sibling/1
    volume-ahead guard (kb invariant 2) — the same rollback trap that cost
    13.5 h once already, and the reason the operator could not simply roll
    back to the previous image when this hang hit.

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

13. **The entity index stores CLAIMS; the trust class is computed per
    request from three inputs and never persisted** (V71-G0, `entities/1`,
    `src/entities/`, migration V0029). `entity_defs` rows carry the FQN the
    TREE proves (literal `class`/`module` nesting, reconstructed from range
    containment over the `symbols` rows — `extract.rs`'s `container` names
    only the NEAREST ancestor and cannot do this), the FQN the app's
    Zeitwerk configuration derives for the PATH, what that config read was
    worth at index time, and the blob it came from. `entities::class_for`
    is the ONE function that turns those into `exact`/`likely`/`candidate`,
    and `exact` is reachable from exactly one row of its table: a
    nesting-matched definition whose indexed blob is still the live one. A
    convention never mints one (`likely` at best, `candidate` when the
    config read degraded), and neither does a claim about bytes that have
    since changed — the aug-lane/1 freshness rule (design §P8) and root
    invariant #2's "kb-code mints classes, nothing is cached", applied to a
    lane whose whole job is naming things. `src/entities/zeitwerk.rs` reads
    `config/application.rb`, `config/initializers/{inflections,zeitwerk}.rb`
    and the `app/` listing AS TEXT — no Ruby is ever executed (invariant 10)
    — behind a fingerprinted per-`repo_root` cache, for exactly the reason
    invariant 12(b) records for the i18n locale index: a full reconcile
    dispatches it once per FILE. A custom `Zeitwerk::Inflector` degrades the
    state with a caption rather than guessing at inflections it cannot see.
    **`worktree` is in the row key** — an entity's FQN depends on the
    checkout, not just the blob, so `(repo_id, worktree, path, ordinal)` is
    structurally unable to alias two checkouts even before the deferred
    union-search-over-worktrees feature makes them share a repo id. The
    table is registered in this crate's ONE id-lifecycle registry
    (`Store::delete_file`'s transaction, beside `rails_edges` and for the
    same reason: its reads are path-keyed, not blob-keyed, so a deleted
    file would otherwise keep answering `?ent=` forever).

14. **`GET /api/seq` is a projection LAYER, and `reading_sets.kind` is
    where a tour and a trail live** (V71-G0, kbc-seq/1, `src/seq.rs`,
    design §P6). Four of the five projections are `reading_sets` rows and
    the fifth is a `canvas_sets` row; this module resolves over both and
    creates, moves and merges nothing — the physical unification is its own
    later unit, with the migration treatment. Widening
    `reading_sets::SET_KINDS` from two to four rather than adding two
    tables is invariant 9's ruling applied to the two remaining names.
    `seq::PROJECTIONS` and `SET_KINDS` are cross-checked BOTH ways by a
    unit test — a set kind with no projection would make a whole family of
    rows invisible to this layer, and a projection that is not a valid set
    kind would filter to nothing silently. `canvas_sets` deliberately has
    no `workspace_id` column: nothing would write it, so `?workspace=`
    excludes boards and SAYS SO in `notes` rather than shipping a column
    with no writer. *V74-L3b amendment:* the `tour` projection now resolves
    out of TWO tables as well — `reading_sets` (kind `tour`) and
    `canvas_boards` (kind `tour`, invariant 26) — exactly the shape the
    `board` projection took in V74-L1. `Store::seq_canvas_boards` takes the
    kind AND the projection name as parameters for that reason; nothing
    about the layer changes, and `source` still names the table each row
    physically lives in.

15. **A route this crate adds ships as a `RouteContract`, and two tests
    walk that declaration against its implementations** (V71-G0,
    `entities::V71_G0_ROUTES`). The v7.0 defect class was a silently dead
    surface — a registry row with no handler, a CLI verb that never sent
    the param its route required. A `RouteContract` names the path, the
    handler as it appears in `router.rs`, the params the route cannot
    answer without, and a function that deserializes the route's OWN params
    struct with one field removed. `entities`'s
    `every_declared_v71_g0_route_is_registered_and_requires_its_params`
    proves the registration and the requirement from the server side;
    kb-code-cli's `cli_requests_send_every_param_their_route_requires`
    proves a verb exists that sends them. Neither test can see a route
    that was never added to `V71_G0_ROUTES`, which is why that list lives
    beside the routes rather than in a test file — add to it in the same
    edit that adds the route.

16. **kbcq/1 is ONE grammar with ONE golden-pinned mirror, and
    `search::matcher` is THE matcher** (V71-D1, design D3). Three rules
    that are separate to state and easy to break independently.
    (a) **One grammar.** `search/grammar.rs` is the only parser; the SPA's
    `web-code/src/lib/kbcq.ts` is a MIRROR, and the two are pinned by ONE
    shared fixture — `crates/kb-code-server/grammar/kbcq.golden.json`,
    walked by `grammar.rs`'s `golden_corpus_matches_the_rust_parser` AND by
    `kbcq.golden.test.ts`, reading the same bytes (root CLAUDE.md #29/#35's
    lock-step discipline). `FILTER_SPECS` is the single declaration home for
    the key set: the parser reads it, `normalize` renders from it, the
    did-you-mean searches it, and
    `every_declared_filter_key_has_a_consumer`/`..._is_applied` walk it
    against `unified.rs` — declaring a key nothing reads FAILS, by name.
    That test is a whitespace-stripped source scan with a scan's limits (it
    proves the expression occurs, not that it is reached), the same trade
    `git_argv_lint` makes. Parsing is TOTAL: an unknown key, an empty value,
    a bad closed-vocabulary value or an unsupported negation is searched as
    an ordinary word and reported as a `Diagnostic` — never a 400, never a
    silent drop. `normalize` is a FIXED POINT, and `text_mode` is part of
    that contract: a LITERAL text query renders bare (`/…`), never delimited
    (`/…/`), because the delimited form is always regex and closing the
    slash would re-parse as an uncompilable pattern.
    (b) **One matcher.** Every name-shaped lane ranks through
    `search/matcher.rs` (nucleo). It returns a hard `MatchTier`
    (`exact` → `prefix` → `fuzzy`) that is an ordering key ABOVE the score,
    not a weight — the JetBrains Search-Everywhere failure mode ("an exact
    filename does not come first") designed out structurally, so no factor
    and no config can displace an exact name. It also returns the match
    positions as **UTF-16** `[start, end)` ranges, converted ONCE here
    because the only consumer is JavaScript; nucleo's own indices are CHAR
    positions and a client slicing on those highlights the wrong columns.
    `web-code/src/lib/speedSearch.ts` is DEPRECATED as a matcher (its
    remaining list-filter call sites are V71-F1/D2's to convert) — never add
    ranking logic there.
    (c) **One factor, one flag, one line in `explain`.** `[search]`
    (`config::SearchSection` → `search::Factors`) carries one boolean per
    ranking factor, kb's MI-W5.R precedent exactly: a factor whose flag is
    off is SKIPPED, not multiplied by a neutral 1.0, and appears on
    `explain:1`'s decomposition iff its own flag is on. `explain` must stay
    HONEST about what it cannot say: this box has no fusion — sections are
    fixed and never interleaved — so a rank is a position within ONE lane
    and `LaneExplain` says `fusion: "none"` rather than inventing a
    cross-lane additive score (the RRF caveat the search research records).
    (d) **The results page is a VIEW of that same response, and it says
    what its numbers are** (V71-D2, `search/results.rs`). `facets:1` and
    `group:` are kbcq/1 keys, not wire parameters, so the SPA's facet rail,
    a saved search and `kb-code search --facets --group dir` all express one
    fact one way. Two rules: every facet count's `basis` is the literal
    string `"page"` — counted over the hits the response actually returned,
    after each lane's own cap, never a corpus estimate (the corpus-wide
    alternative would be a second full scan per keystroke on a box whose
    text lane already cannot finish one); and every `FacetValue` carries the
    kbcq/1 CLAUSE that selects it, because a facet WRITES the query and is
    never hidden client state (kb root CLAUDE.md #35's `galleryUrl`
    discipline). A facet field whose selection kbcq/1 cannot express is NOT
    EMITTED, and `every_facet_field_writes_a_clause_that_reparses` walks
    `FACET_FIELDS` against `grammar::parse` and fails by name on a clause
    that does not come back as the filter it claimed. `group_hits`
    PARTITIONS a section — every row in exactly one group, rows keeping the
    lane's own order, groups in first-appearance order, nothing dropped and
    nothing re-ranked — addressed by POSITION (`indices`) because four of
    the six lanes have no stable hit id, with `hit_ids` riding alongside
    only where one exists rather than being fabricated. `UnifiedSearchResponse::stale`
    (`generation` + `as_of`) is ALWAYS present, and `generation` is the
    store's monotonic counter, NOT a commit distance — "behind by N
    commits" would need a git call per keystroke. The per-hit half is
    `FileHit::blob_sha` (the files lane's snapshot has the blob in hand);
    the other five lanes carry none and the wire does not pretend they do.

17. **`kbc-actions/1` is a server-rendered dispatch table whose ORDER is a
    golden and whose mutating rows are ABSENT, not disabled** (V71-E2, D5,
    `src/actions.rs`). Four rules that are separate to state and easy to
    break one at a time.
    (a) **Order is a `const` table, pinned per target kind.** JetBrains'
    `Alt+Enter` reorders under the user, which is exactly why nobody can
    muscle-memorise it; `golden_action_table_is_stable` renders
    `(group, id, version)` per kind and fails by name on any move. A
    browser-local "recent" section is allowed ABOVE the groups, visually
    separate, and is never merged into them.
    (b) **A caller the server did not clear does not see the Change group
    at all.** The verdict is `kb_server::middleware::is_loopback_origin`
    plus `[review] remote_mutations` — never a client guess — and the group
    VANISHES rather than rendering disabled (a disabled row is a map of the
    mutation surface). `ActionsOut::mutations.reason` states the caller
    property plainly, which is the same thing `GET /api/repos`'s `loopback`
    bool already reports and is not a confirmation that any particular
    mutating route exists. This route reads `ConnectInfo` directly — the
    same carve-out `routes::repos` and `search::unified` take, for the same
    reason.
    (c) **Nothing auto-navigates, because this route never resolves.** It
    must open inside a ~50 ms budget, so it deliberately does not run
    `resolve::resolve_position`; a row that cannot PROVE `exact` must not
    jump (D5 risk 5, the trust-class-laundering one). Every row therefore
    ships `auto_navigate: false` and the peek rung is ordered first for a
    symbol. The reader's own `gd`, which does resolve, keeps its
    single-exact-match rule unchanged.
    (d) **The target vocabulary is CLOSED at five names and "Ask here" is
    required on every one of them.** `every_target_kind_offers_ask_here`
    and `every_spec_builds_an_op_in_a_declared_group` are the two walks
    that keep the spec table and the op table from silently disagreeing —
    the v7.0 dead-surface defect, in this module's own shape. The route
    emits an ADDRESS, never an href: root CLAUDE.md #35's one-URL-builder
    rule means the SPA's `lib/codeUrl.ts` is the only place a kb-code URL
    is composed, and `lib/actionOps.ts`'s resolver is exhaustive over the
    `ActionOp` union so a new server variant fails the SPA BUILD.

17. **kbc-tree/1 computes the projection ONCE; kbc-scope/1 borrows
    kbcq/1's atoms rather than becoming a second query language**
    (V71-F1, `src/tree/`). Two rules, separate to state.
    (a) **One projection, two renderers.** `GET /api/tree/2` returns
    flattened rows; `kb-code tree` and `web-code`'s `FileTree` render
    them and re-derive nothing (the evidence report's own last risk:
    "two renderers of one projection will diverge"). The four projections
    live in `tree::project_{physical,role,namespace,change}` and share
    ONE flattener (`tree::flatten`), which is what keeps the two filter
    modes, the depth budget and the folder aggregate from drifting apart
    — a closed folder's counts are the same numbers it would show open,
    pinned by a test. `/api/tree` (the per-directory ODB listing) is
    FROZEN beside it, the `/usages/2` treatment. **`unplaced` and
    `truncated` are not optional fields**: `role` and `namespace` are
    INFERENCES, so every inferred grouping carries the class
    `roles::ROLE_TRUST`/`entities::class_for` minted for it (a path
    convention structurally cannot reach `exact`, invariant 13's posture
    applied to a grouping), and every file the projection could not place
    is listed with an EXACT `unplaced_total` beside the capped list. A
    projection that hides work is worse than no projection. The
    decoration budget is THREE lanes, and a dropped lane is named in
    `notes`; every lane is one whole-repo grouped read (`Store::
    open_annotation_counts_for_repo`, not the path-list `..._by_path`
    form a 6,553-file tree would turn into a 6,553-term `IN`), and a lane
    nobody asked for is never read at all (`tree::sources` gates on
    `Expr::atom_keys`). The three lanes with no builder in this milestone
    (churn/provenance/coverage — treemacs' deferred tier) are ABSENT from
    `LANES` rather than declared and empty.
    (b) **One grammar, borrowed.** `tree::scope` adds the BOOLEAN
    structure kbcq/1 has no notion of, and for the shared atom keys
    (`KBCQ_SHARED_KEYS` — `path:`/`ext:`/`lang:`) it hands the token to
    `search::grammar::parse` and reads the value back out of its typed
    `Filters`. The tree-only keys are declared in `SCOPE_ATOM_SPECS` and
    deliberately NOT added to `FILTER_SPECS`: a `role:` that parsed
    cleanly in the search box and then did nothing there would be the
    v7.0 dead-surface defect one layer up (typed there today it is an
    unknown key, and kbcq/1 warns by name). The vocabulary is walked from
    both ends — a source scan for the resolver expression AND a
    functional probe that runs every declared atom through the real
    parser and resolver and fails it for not DISCRIMINATING on a fixture
    — and `UNRESOLVED_ATOMS` is the E1-style ledger for the names the
    design uses that this milestone does not resolve, disjoint from the
    spec table by test. Resolution REFUSES rather than guessing: a scope
    with any diagnostic is not applied, and the caller renders the
    UNSCOPED tree with the reason captioned. `tree::V71_F1_ROUTES` joins
    invariant 15's `RouteContract` walk from both sides.

18. **`syntax/1` is the ONE file-type declaration, the tier is ONE
    short-circuit, and the Parity Grid is DERIVED** (V72-H1, D7,
    `src/syntax.rs`). Four rules, separate to state and easy to break
    independently.
    (a) **One declaration.** `syntax::REGISTRY` owns which file is which
    language — extensions, D7's filename-stem table (`Gemfile`,
    `Rakefile`, `Guardfile`, `Capfile`, `.rake`/`.jbuilder`/`.gemspec`/
    `.ru`) and the `#!` interpreter table — plus the grammar, the
    extraction tier and the injection-host flag. `lang::detect` is a
    FAÇADE over it and its contract is unchanged and load-bearing: `Some`
    means "a grammar exists and `salt` keys this file's derived rows", so
    a grammar-less row (`sql`, `dockerfile` — NAMED so the gap is visible
    on the two read surfaces) is invisible to every existing caller.
    Adding a language means adding a row, never a second `match` on an
    extension; `lang::ALL_LANGS`, `lang::for_id` and the registry are
    pinned to each other by test, as are the extension/filename/
    interpreter keys' uniqueness (two rows claiming `sh` would make
    detection order-dependent).
    (b) **One short-circuit.** `SyntaxRow::plan` is the whole HIGHLIGHT_ONLY
    mechanism: `ingest::index_file` consults it ONCE, at the top of the
    derivation, and writes spans with an explicitly EMPTY symbol set —
    never an aborted walk, and never a mid-pipeline `if lang_id == …`
    added somewhere else. The structural guarantee behind that is a test:
    no non-`Full` language may appear in ANY downstream pass's own gate
    (`lang::supports_token_level`, `imports::supports`, `locals::supports`,
    `hierarchy::supports_hierarchy`, `entities::indexes_lang`), so a
    highlight-only file can never reach a pass that needs the symbols the
    tier just skipped. `syntax::Tier` is a DIFFERENT axis from
    `ingest::TIER_*`/`files.lang` (`unknown`/`binary`/`too-large`/`lfs`,
    content skip markers): the tier is a property of the file TYPE,
    decided before a byte is read, and the `tier`/`tier_reason` fields on
    `GET /api/file` and per-file `GET /api/symbols` say only that — never
    that a particular blob has been derived. *V72-H2b amendment:* the
    plan is consulted once and feeds TWO independent gates, not one —
    `plan.highlight` is decided before either cache is asked, because
    "this type paints nothing" is a property of the type and stays the
    answer on the hundredth visit (`GET /api/file`'s `highlight_cache`
    reports `skipped_tier`, `hit` or `miss`, and the third is what a
    `highlight_salt` bump looks like from the outside). What a blob HAS
    been derived is invariant 11's `derived_status` marker, and the two
    axes must not be conflated on the wire either.
    (c) **The grid is derived, and golden-pinned.** Every Parity Grid cell
    is computed from the predicate that actually gates that lane
    (`lang::tags_query`, `extract::CST_OUTLINE_LANG_IDS`,
    `lang::supports_token_level`, `lang::locals_query`), never hand-typed
    — a hand-written matrix is exactly the thing that rots into a claim
    the daemon cannot back. Every non-`yes` cell carries a REASON,
    including every `no`: the grid's job is to be an honest map of where
    the instrument is weak, and an unexplained gap is not a map.
    `tests/fixtures/parity.golden.json` pins the whole wire, so a
    capability change is a deliberate golden update reviewable in the diff
    that causes it. HIGHLIGHT_ONLY currently has NO production row (the
    first arrive with H2a's SCSS/CSS/Markdown grammars) and a test records
    that emptiness with its reason rather than leaving it to be
    discovered — the `usages2::UNMINTED_KINDS` precedent.
    `syntax::V72_H1_ROUTES` joins invariant 15's `RouteContract` walk from
    both sides.
    (d) **The ENGINE is a separate axis from the tier, and a scanner row
    is unreachable from every tree-sitter-gated pass** (V72-H3, D7,
    `src/haml/`). `syntax::Engine` (`TreeSitter(crate)` | `Scanner(schema)`
    | `None`) says WHO parses; the tier says WHAT is derived. They are
    orthogonal on purpose: HAML is `Full`-tier with NO grammar (D7 — no
    viable one exists), which is a shape (a) and (b) alone could not
    express. Three consequences, each one a way to break this silently.
    First, `lang::detect`'s contract widened by exactly one engine: `Some`
    now means "SOMETHING parses this", so a caller that reads `Some` and
    then reaches for `lang::parse` gets `Unsupported` — every existing one
    already degrades on that (it is also what an unregistered id returns),
    but a NEW caller that `unwrap`s would be broken only for `.haml`.
    Second, a scanner row must not appear in ANY pass gated on a
    tree-sitter parse (`supports_token_level`, `imports::supports`,
    `locals::supports`, `supports_hierarchy`, `entities::indexes_lang`,
    `extract::CST_OUTLINE_LANG_IDS`, `tags_query`, `highlights_query`) —
    pinned by its own test, the sibling of (b)'s. Its rows come from the
    two arms `extract::extract_symbols` and `highlight::extract_highlights`
    grew instead, dispatched BEFORE `lang::parse` would fail. Third, the
    salt's version component is the SCANNER's own
    (`haml::SCANNER_VERSION`), so a change to `src/haml/` that would
    produce different rows for unchanged bytes owes a salt bump exactly as
    a grammar bump does — and `crate::lang::ALL_LANGS` must carry it or
    invariant 11's stale-salt sweep deletes every current HAML row.
    The Rails lens consumes HAML through the EXISTING extractors
    (`support::walk_haml_ruby_fragments` parses ONE synthesized Ruby
    program built from `haml::extract::ruby_program`'s fragment stream and
    hands the root to the same `scan` callback the ERB walk uses,
    re-anchoring lines afterwards through the program's line map) — so
    there is ONE minting path for a render/i18n edge, not two, and invariant
    12's trust posture applies unchanged. **The `haml` gem is an OFFLINE
    oracle only**: `tests/fixtures/haml/`'s expectations were generated
    once by hand and checked in, `ci-code` is pure Rust, and invariant 10
    is untouched — `tests/haml_corpus.rs` greps its own source for
    `Command::new` to keep that true.
    (e) **A host's guest regions are located in ONE place, and every
    re-anchoring goes through ONE offset map** (V72-H2a, D7,
    `src/injection.rs`). `injection::regions` is the only walk that finds
    embedded code; `OffsetMap` is the only thing that maps a guest
    position back. Two shapes, and the difference is load-bearing:
    `Shift` (the guest IS a contiguous slice — byte offsets AND rows map
    back) and `Lines` (the guest was REASSEMBLED — only rows map back,
    `host_byte` returns `None`, and a `None` line-map entry is a line this
    crate invented). A reassembled region is never painted, because
    approximating a byte offset for it would put spans on bytes its
    producer never looked at. Three hosts (`erb`→ruby, `haml`→ruby
    fragments + one program, `markdown`→whatever a fence's info string
    resolves to); HTML is NOT one, and the module doc says why (no
    `tree-sitter-html` in this build) rather than leaving a reader to
    infer that `<script>` injections work. Painting runs the GUEST's
    `highlight::extract_highlights_host_only`, which structurally cannot
    re-enter the layer — that is the entire recursion bound, and there is
    deliberately no depth counter to get wrong. Two properties must hold
    together or the layer is a lie: `SyntaxRow::injection_host` must equal
    `injection::is_host` (pinned), and Markdown's declared guest set must
    equal what `markdown::resolve_info_string` can actually return
    (pinned) — a wire that promises a fence language the painter skips is
    the v7.0 dead-surface defect in a new place. **The Rails lens's ERB
    and HAML output is byte-identical across this move** and its goldens
    are the proof: the walks moved, `walk_erb_ruby_fragments` /
    `walk_haml_ruby_fragments` kept their signatures, and nothing about
    edge kinds or trust classes changed.
    (f) **`outline/1` is a VIEW of the symbol rows, never a second
    extraction** (V72-H2a, D7, `src/outline.rs`). `GET /api/outline` reads
    exactly the store lookup `GET /api/symbols` does — same blob, same
    salt, same `extract::Symbol` — and `outline::nest` is the whole
    transformation: pure, total, deterministic. Deriving rows in the route
    instead would create a second answer that can disagree with the first,
    which is the thing one contract exists to prevent. Nesting is RANGE
    CONTAINMENT (invariant 13's own rule), never name matching: a
    `container == name` join cannot tell two identically-named symbols
    apart and reads a YAML row's dotted-path `container` as a name it is
    not. Row KINDS are the symbol kinds verbatim — this contract invents
    no vocabulary. Every response carries `honesty` (tier, engine,
    `derived_from`, and the REASON when there are no rows), so "no symbols
    by tier", "nothing indexed for this blob yet" and "no registry row at
    all" are three distinguishable answers rather than one empty list. The
    Parity Grid's `outline` cell is derived from this and is `yes` exactly
    when the row's tier derives symbols. The SPA's client-side outline
    derivations (`web-code/src/lib/outline.ts`, `StructurePopup.tsx`,
    `OutlineRail.tsx`, `lib/stickyContext.ts`) are NOT converted by
    V72-H2a and are named in the module doc; when they are, they render
    this response rather than re-deriving, for invariant 17(a)'s "one
    projection, two renderers" reason.

19. **The entity DOSSIER raises no trust class, and its budget is a ROW
    budget nothing may spend silently** (V72-G1.1, `entity/1`,
    `src/entities/dossier.rs` + `src/entities/ruby_body.rs`). Invariant
    13's posture, one layer up. Three rules. (a) **Every lane's class is
    borrowed, never minted.** The usages lane hands `usages2::usages2_at`
    — the SAME engine `/api/usages/2` calls, factored out for this — and
    copies each `UsageRow2` verbatim, so a group's rows carry the
    engine's own `trust` and the dossier is structurally unable to
    upgrade one; a member the TREE proves inherits its definition block's
    own `class_for` class, a member a LINE scan proves (`attr_*`, a
    constant assignment, `alias_method`) is capped at `likely`, and an
    INHERITED member is capped below `exact` because Ruby's method lookup
    can be shadowed by a mixin this index cannot order.
    `dossier::resolve_reference` is the ONE function turning a written
    superclass/mixin name into a class, and it reaches `exact` from
    exactly one shape — a root-anchored or empty-lexical-nesting
    reference whose every definition site is itself `exact` — because
    `module M; class A < B` is `M::B` when `M::B` exists and `::B`
    otherwise, a lookup Ruby performs at RUNTIME. (b) **Visibility is
    resolved or it is `unknown`.** `ruby_body::scan_visibility` tracks a
    keyword-block depth over a block's DIRECT body lines (a nested
    symbol's body is removed, its OPENER line is not — `private def
    total` and `attr_reader :a` both live on one) and refuses, naming the
    line, the moment a bare `private` appears at a depth it cannot
    account for (inside an `included do`, an `if`, behind a splat); every
    member from there on is `unknown`. A wrong `private` on a member
    table is the member-level shape of a wrong `exact`. (c) **The budget
    is rows, and the order is data.** `LANE_PRIORITY` is a `const` the
    response echoes in `honesty.budget.order`; drops are counted per lane
    in `honesty.budget.dropped` and a group emptied by the budget still
    reports its true `total`. `GET /api/entity` (`entities/1`) is FROZEN
    beside `/api/entity/dossier` — the index answers an ADDRESSING
    question and legitimately returns many entities, a dossier is about
    exactly one — and `dossier::V72_G1_ROUTES` joins invariant 15's
    `RouteContract` walk from both sides.
20. **`rails/1` is a per-request VIEW over three tables it does not own,
    and its trust ceiling is a TYPE** (V72-I1, `src/rails/`, design D7 +
    Track I). Three rules, separate to state.
    (a) **No table, no migration.** The eight nouns (`rails::NOUNS` —
    model/controller/action/route/job/mailer/view/concern) are a fold over
    `entity_defs` (invariant 13), `rails_edges` (invariant 12) and the
    mirror index, computed per request and persisted nowhere: `codelens/1`'s
    posture and root invariant #2's "kb-code mints classes, nothing is
    cached", applied to a lane whose whole job is naming things. A derived
    table would buy latency at the price of a fourth thing that can be
    stale in a system whose honesty story is "a stale fact must never read
    as a fresh one". The measured budget lives in `tests/rails_route.rs`.
    (b) **`rails::noun_trust` is the ONE minter, and it returns
    `frameworks::Trust`** — the Rails-lens enum with no `Exact` variant, so
    the oracle bar is enforced by the type rather than by remembering, the
    same way `rails_edges`'s own `CHECK (trust IN ('likely','candidate'))`
    enforces it in SQL. Two independent WITNESSES, a live blob and a
    Zeitwerk config that could be read buys `likely`; a drifted blob or a
    degraded config demotes. The witness list ships beside the class so the
    arithmetic is checkable, and a route row takes the EDGE's own class
    rather than re-deriving one upward. `rails::NOUNS` is also the `rails:`
    kbcq/1 atom's value vocabulary (invariant 16(a) reads it directly), so
    a ninth noun cannot exist on one surface and not the other.
    (c) **What it cannot say, it says.** Class ancestry is not indexed
    (there is no `entity_edges` table — invariant 13 records the deferral),
    so a "model" is a class under an `app/models` root corroborated by the
    lens's own edges, never a proven `ApplicationRecord` descendant, and the
    note saying so rides every response. Method visibility is not indexed
    either: the `action` noun resolves `public` with a LINE SCAN for a bare
    `private`/`protected` under a hard read budget
    (`rails::MAX_VISIBILITY_READS`) — a scan that misses `private def foo`
    and `private :foo` by construction and therefore may only ever DEMOTE a
    row, never promote one — and past the budget an action is honestly
    `unknown`, flagged, with the response `partial` and the budget named.
    The four read states (`ok`/`empty`+reason/`partial`+budget/`error`) are
    `rails::Honesty`, not a convention. `rails::filter` (the kbcq/1 atoms'
    resolver) is a SEPARATE, lighter read on purpose — a keystroke must not
    pay for the full join — but it reads the SAME `noun_for_path`, so the
    search box and `/api/rails/*` can never disagree about what a model is;
    it REFUSES (unapplied, with the reason captioned on the lane) rather
    than guess when the repo is not a Rails app or more than one repo is in
    scope, since the lanes' post-filter sees only a repo-relative path.
    (d) **Routes carry an ADDRESS, additively.** `route_action`'s
    `extra_json` gained `{"verb","path"}` from the same DSL walk that
    resolves the controller (`frameworks/rails/routes.rs`'s `RouteCtx`
    tracks the module and URL axes SEPARATELY — `namespace` moves both,
    `scope module:` only the first). This is additive content, NOT a
    `rails-lens/2` bump: no `kind` is added and no `kind`'s meaning
    changes. An edge written by an older binary therefore has no address
    until its source file is re-extracted, and every reader must render
    that as unknown — never as `/`.

21. **`aug-lane/1` has one enablement gate, one classing function, and
    two lane kinds because the daemon runs no tool** (V72-H4a, `src/lanes/`,
    migration V0030, design §P8/D7). Three rules, separate to state and
    easy to break one at a time.
    (a) **A lane is enabled ONLY by `[lanes]` in kb-code.toml.** Never by
    a route, never by a request, and never by a file inside a REPOSITORY —
    a committed `.kbc/lanes.toml` would be remote code execution by `git
    clone` (the `.vscode/tasks.json` trap the design's security posture
    names). `lanes::LANES` is a Rust table this binary ships and
    `config::LanesSection` is the only thing that turns a row of it on;
    the default is EMPTY, which is what keeps every other response
    byte-identical on a daemon that has never heard of lanes. The
    `sarif.*` row is a FAMILY TEMPLATE and is deliberately not
    addressable: a SARIF ingest into one shared bucket would make two
    scanners indistinguishable, so the operator declares one instance per
    tool (`sarif.brakeman`) and a typo is reported by name rather than
    silently becoming a lane that never runs.
    (b) **Two kinds, because of invariant 10.** The design sketches four
    source kinds (`git`/`file`/`http`/`exec`); this crate has two —
    `derived` (computed here, from `git` and the mirror, through
    `history::run_git_raw`) and `ingested` (POSTed by `kb-code lanes
    ingest`, which ran the tool on the operator's box, over the
    loopback-only mutation lane). The `exec` kind is not a server feature
    at all; it is the CLI, which is also why the three adapters live in
    THIS crate (one home for the fact shape, golden-pinned by `cargo test
    -p kb-code-server`) while only the parsed facts cross the wire. There
    is no configuration flag that could create a third path.
    (c) **The class is computed per request and never persisted, and
    `sha_source` is what makes `exact` reachable at all.** `lane_facts`
    has no class column (invariant 13's posture, and root invariant #2's
    "kb-code mints classes, nothing is cached", applied to somebody else's
    tool output). `lanes::classing::class_for` is the ONE function:
    `min(lane ceiling, per-fact cap, anchor state)`, pure, with the whole
    rules table in its own doc. `exact` requires BOTH that the fact's blob
    is the file's current one AND that `sha_source = "tool"` — a blob the
    DAEMON attributed at ingest because the tool named none is
    `mirror_at_ingest` and caps at `likely` forever, because reading it
    back as `exact` would be a wrong `exact`, which is a release blocker.
    Re-anchoring calls the ONE Ladder this crate already has
    (`annotations::anchor_for_line`/`resolve` +
    `review_comments::line_matches_snippet`) rather than growing a second,
    and the snippet it re-resolves is captured at ingest ONLY when the
    fact's blob is what is on disk at that moment — a fact about some
    other blob carries none and becomes an honest orphan instead of a
    manufactured match. The per-fact cap exists for exactly one shipped
    case and is not decoration: `git.behavior`'s `last_touch` reaches
    `exact` on a PRESENT `Kb-Session:` trailer (evidence) and caps itself
    at `likely` on an ABSENT one (absence of evidence is not evidence of a
    human). `lane_facts` is registered in `Store::delete_file`'s
    transaction beside `rails_edges` and `entity_defs`, for the identical
    reason: its reads are `(repo_id, path)`-keyed. An ingest REPLACES on
    that same key — the batch's paths PLUS `clear_paths` — which is
    invariant 12(a) restated and the only way "the offense was fixed" is
    expressible. Retention is a PAGED background sweep on V72-B0's shape
    (spawned before the bind, never awaited, one short transaction per
    page), and a DISABLED lane is never swept out from under a re-enable.
    `lanes::V72_H4A_ROUTES` joins invariant 15's `RouteContract` walk from
    both sides.
22. **`kbc-review/1` is a MARKDOWN document with a closed ref grammar; the
    slug is identity and is never reused; `compose` is the ONE authoring
    transaction** (V73-K1, `src/review_doc/`, migration V0034, design
    D9/D9-a). Four rules, separate to state and easy to break one at a time.
    (a) **The stored body is Markdown; HTML is only ever an EXPORT.** D9-a's
    recorded departure from the operator's original ask — HTML is a
    permanent XSS surface, cannot be interdiffed across re-reviews, and its
    references are dead text. `review_docs` revisions are APPEND-ONLY
    (`compose` never UPDATEs a row; highest revision wins on read), the
    MI-W2.3 soft-forget posture this crate already applies to
    `review_findings.superseded`, for the same reason: a re-compose that
    says something different must not destroy what a human formed a
    disposition against. `render` is the only HTML producer, every dynamic
    string goes through `render::esc`, every Markdown body goes through
    kb-core's UNTRUSTED-body renderer (`render.unsafe = false`), and an
    unknown `{{…}}` placeholder is left VERBATIM and reported rather than
    substituted — so an operator template's own CSS/JS braces survive and a
    typo is never a silent hole. **kb-code never generates a `<template
    id="kb-prompt">`**: that convention is kb's (root invariant #5).
    (b) **A bare `[[X]]` is a kb wikilink and can never be a kbc ref.**
    Root invariant #29 owns that syntax, which is why every kbc ref carries
    one of the seven CLOSED scheme prefixes (`refs::SCHEMES`). A `[[…]]`
    naming a known scheme that does not parse is `Malformed` with a reason,
    never silently degraded into a wikilink — that would make the failure
    invisible on both sides. ONE parser serves both surfaces (the prose scan
    and the typed front-matter fields), and the grammar is golden-pinned by
    `grammar/kbcrefs.golden.json`, the same one-fixture-two-parsers
    discipline invariant 16(a) states for kbcq/1. The fixture lives on the
    CRATE side because the Rust builder stage's Docker context is `COPY
    crates ./crates`.
    (c) **A ref that does not resolve is an ORPHAN, and `exact` is
    unreachable from a carry.** `cards::trust_for` is the ONE minter;
    `exact` comes from byte equality (the ref's `@sha` IS the patchset's
    blob) or from a `finding:` row in this daemon's own store, and nothing
    else. A CARRIED ref is capped at `likely` even when the ladder matched
    the snippet verbatim — an exact text match at a different line in a
    different blob is evidence, not proof, and invariants 13/20's oracle bar
    applies here too. The carry itself REUSES `annotations::resolve` plus
    `review_comments::line_matches_snippet` rather than adding a second
    matcher. `sym:`/`ent:` take only the EXACT rung of the existing
    addressing (an ambiguous name is an orphan naming the count) because a
    ref card, unlike a `?sym=` link, carries no fallback anchor beside it;
    `ent:` additionally inherits the entity index's own class as a CEILING
    (`cap_trust`), never a floor. Highlight spans are a pure STORE lookup
    (`GET /api/file`'s own rule) — an unindexed blob answers `null`.
    (d) **The slug is identity; the fingerprint is a change detector; the
    ledger makes "never reused" true.** `f-<n>` slugs are minted from
    `review_finding_slugs`, whose counter reads BOTH that ledger and the
    ordinals already on `review_findings`, so it cannot walk backwards for a
    pre-V0034 review or after a hand-deleted row. Document-path
    reconciliation matches by FINGERPRINT (`FindingIdentity::Fingerprint`,
    the SAME `reconcile_findings_import_on` core the v1 slug-keyed path uses
    — one implementation, two identity rules), so re-wording a finding keeps
    its slug and therefore a human's disposition; `severity` and `blocking`
    are deliberately NOT fingerprint inputs for that reason. `superseded_by`
    is written only when an incoming finding DECLARED `supersedes: [<slug>]`
    — never inferred, because guessing which new finding "is really" an old
    one is the wrong-`exact` class one layer up. V0024's origin rule is
    untouched: a `manual` finding is never adopted or superseded by a
    compose, and an explicit slug naming one is a whole-compose 400.
    `compose` is the ONE transaction (document revision + findings + report
    + verdict in one `BEGIN`/`COMMIT`, ONE `review.changed{reason:
    "compose"}`); `findings import` / `report --set` / `verdict` stay
    documented low-level twins, and D22's local-canonical ruling is
    unchanged — every authoring surface here is loopback-only and root
    invariant #4 is not amended. `review_doc::routes::V73_K1_ROUTES` joins
    invariant 15's `RouteContract` walk from both sides.

24. **`kbc-canvas/1`: a board node is a CLAIM re-resolved on every read, a
    board is COORDINATE-FREE, and the two mutation rules are enforced by
    the LINT rather than by the gate** (V74-L1, D10 + D21, `src/boards/`,
    migration V0036). Four rules, separate to state and easy to break one
    at a time.
    (a) **Nothing about resolution is stored.** `canvas_nodes` has no state
    column and never will: `pinned`/`carried`/`orphan`/`present`/`inert` is
    computed per request by `boards::resolve`, which is invariant 13's and
    21's posture (and root invariant #2's "kb-code mints classes, nothing is
    cached") applied to a lane whose whole job is pointing at other things.
    The `code` rungs are the crate's ONE ladder —
    `annotations::anchor_for_line` + `annotations::resolve`, guarded by
    `review_comments::line_matches_snippet` — not a fourth implementation.
    The only thing an apply DOES persist for resolution is
    `canvas_nodes.anchor_snippet`, captured under `lane_facts`' exact rule
    (invariant 21): the file must be readable AND the node's claimed blob
    must be what is on disk at that moment, because a snippet captured
    against other bytes manufactures a match later instead of admitting an
    orphan. **An orphan is SHOWN** — with its address and that snippet —
    never dropped, and `honesty` counts it. The two things a read
    deliberately does not probe say so in `note`: a `kbc-hunkid/1` id is a
    content address (so `present` means the review and patchset exist, not
    that the hunk does), and a `turn` is probed for EXISTENCE only, never a
    byte of transcript text — a board is bearer-readable and the
    transcripts lane is loopback-only.
    (b) **Boards are coordinate-free, and the refusal is the teaching
    surface.** D10 puts layout in TypeScript with one engine; the document
    therefore has no geometry except a top-level `pins` map. The
    `coordinates` lint rule runs on the RAW JSON *before* the typed parse —
    `BoardDoc` is `deny_unknown_fields`, so serde would otherwise refuse an
    `"x": 10` with a generic "unknown field" instead of the message that
    names `pins`. The server derives geometry in exactly ONE place,
    `boards::layout`, and only for the JSON Canvas export, whose spec
    requires it; that export says in its own payload that its coordinates
    were derived and that it is a snapshot. Do not grow `boards::layout`
    into a second layout engine — if the SPA's geometry ever has to survive
    a round trip, it sends PINS.
    (c) **`accepted` is not authorable, and idempotency is one hash.**
    `apply` may write only `pending` (the default, D21) or `draft`; the
    `status` lint rule refuses a document naming `accepted`/`archived`, so
    the rule holds for a LOOPBACK caller too rather than resting on the
    route gate, and `POST …/accept` is the only writer of that value. A
    CHANGED apply against an accepted board resets it and says
    `status_reset: true` — a human accepted a specific board, not a slug.
    Idempotency is `boards::content_hash` over a canonical rendering that
    deliberately EXCLUDES `status` and `repo`; a field-by-field diff would
    be a second answer to the same question. Nodes are keyed by the
    author's own id, which is what keeps a node's `thread_id` (an
    `annotations` parent — there is no second comments table) alive across
    a re-apply.
    (d) **The lint's two severities are the whole posture.** `refuse` is
    "I cannot read this" (a cap, an unknown vocabulary value, a dangling
    edge or step, a MALFORMED address); `warn` is "I read this and it
    points at something that is gone" (an unresolvable reference — the node
    becomes an honest orphan). Conflating them turns a board into either a
    liar or a brick wall. The report is a LIST, never a first error,
    because an agent retries the whole document. `kb-code canvas sweep
    --check` is the same resolution run as a CI gate: it NEVER mutates (a
    gate that repaired what it found could not fail) and exits 3 on drift.
    `boards::V74_L1_ROUTES` joins invariant 15's `RouteContract` walk from
    both sides; the four mutations are absent from it for the reason
    `lanes`' ingest route is absent from its own — a `RouteContract`
    describes a query-param surface, and a POST whose payload IS the
    contract has nothing for `params_accept_without` to say.
23. **`kbc-claim/1` is SURFACED-NEVER-SCORED, a transcript-derived join is
    LOOPBACK-ONLY, and a pseudo-file is a per-request VIEW with a real blob
    hash** (V73-K3, `src/claims.rs` + `src/review_pseudo.rs` +
    `src/review_turns.rs` + `src/review_hunks.rs`, migration V0035, design
    D9/D18/D25). Four rules, separate to state and easy to break one at a
    time.
    (a) **Claims are surfaced, never scored, and the pin is a source scan.**
    A claim is agent PROSE about code: it cannot be re-derived, so it is
    stored, and *because* it cannot be re-derived it must never be trusted
    the way a derivation is. It is rendered beside the fact it is about and
    is never a ranking term, a boost, a filter default or a trust class.
    `claims::tests::no_ranking_module_imports_the_claim_register` walks this
    crate's ranking sources (`search/matcher.rs`, `search/unified.rs`,
    `search/results.rs`, `review_inbox.rs`, `review_analytics.rs`,
    `unified_inbox.rs`, `resolve.rs`, `usages2.rs`) and fails BY FILE if one
    of them so much as names the module — a source scan with a scan's limits,
    the same trade `git_argv_lint` (invariant 3) and
    `every_declared_filter_key_has_a_consumer` (16(a)) make. `confidence` is
    the AUTHOR'S declaration, printed verbatim and multiplied into nothing.
    ONE table for all six of D18's renderings (explain cards, the
    alternatives ledger, decision threads, branch stories, trail notes,
    entity answers) — invariant 9's "a `reading_sets` row with `kind =
    'workspace'` IS a workspace" one layer up; six tables would be six
    cascade registrations and six chances to disagree with the Ladder.
    `claims` has no `trust` column (invariant 13's posture): what is STORED
    is the WITNESS, `blob_sha`, and `claims::ladder_state` turns it into
    `pinned`/`drifted`/`unanchored` per request. A drifted claim is shown
    with a caption naming BOTH blobs and is NEVER re-anchored — this module
    runs no ladder of its own, because a claim is about a whole subject
    rather than a line and guessing which lines it "really" meant is the
    wrong-`exact` class. **`claims` is deliberately NOT in
    `Store::delete_file`'s cascade**, unlike `rails_edges`/`entity_defs`/
    `lane_facts`: those are DERIVED rows whose path key would answer forever;
    a claim is AUTHORED content, like an `annotations` row, and deleting an
    agent's reasoning because the file it was about was deleted would destroy
    the record that explains why it was deleted.
    (b) **A join that reads transcript TEXT is loopback-only, and its
    non-exact tier says which witness is missing.** `review_turns` needs an
    `Edit`'s `old_string`/`new_string` — file content out of a raw
    transcript, D19's `raw-transcript` sensitivity class — so
    `GET /api/reviews/{id}/hunks/{hunk}/turns` rides the `transcripts_api`
    sub-router's gate, and the timeline's own `turns` lane re-checks
    loopback per request and reports `refused` with that reason rather than
    omitting itself. Those strings are read back from the JSONL on demand
    (`transcripts::search::read_turn_tool_input`) and NEVER persisted: the
    indexer's `TOOL_USE_TEXT_KEYS` allowlist keeps them out of the FTS
    column on purpose and that stays true. `exact` requires THREE
    independent witnesses — the bytes, the path, and a commit whose own diff
    reproduces the hunk's content address — and anything short of all three
    is `likely` NAMING the missing one; anything short of a byte match over
    `MIN_MATCH_BYTES` is not claimed at all (an empty list with a reason,
    never a fuzzy third tier). `CommitBasis` is the ceiling: a
    `path_in_range` basis caps every match at `likely` because you cannot be
    exact about which session made a change if you cannot say which commit
    made it.
    (c) **`kbc-hunkid/1` now has two implementations and therefore a
    golden.** The address was minted client-side by V73-K2a because the
    daemon stored it opaquely (`review_hunk_viewed`, V0031); the hunk↔turn
    join makes the daemon FIND the hunk an id names, so `review_hunks` is a
    second implementation of one grammar and
    `grammar/kbchunkid.golden.json` is the ONE fixture both read
    (`review_hunks.rs` and `web-code/src/lib/hunkId.golden.test.ts`) —
    invariant 16(a)'s kbcq/1 discipline and 22(b)'s kbc-refs/1 discipline,
    third instance. The fixture lives on the CRATE side because the Rust
    builder stage's Docker context is `COPY crates ./crates`. The hash is
    FNV-1a 64 over **UTF-16 code units**, not bytes, because the TS side
    hashes `charCodeAt`; the golden carries a non-BMP case so a
    "simplification" to bytes fails loudly rather than only for the diffs
    that contain one.
    (d) **A pseudo-file is a per-request VIEW with a REAL git blob hash, and
    it has no carry rung.** `review_pseudo` renders four names under the
    reserved `~review/` prefix from rows that already exist plus one
    `git log`; it stores nothing (`rails/1`'s invariant 20(a), applied to a
    review's prose). Its hash is literally `ingest::git_blob_hash` over the
    rendered bytes — the SAME function the mirror index uses — which is what
    lets `[[code:~review/pr-body.md:12@<sha>]]` be `pinned` by byte equality
    on K1's own ladder with no second notion of identity, and what lets a
    comment on a pseudo path resolve through
    `review_comments::resolve_for_ps_with_content` rather than a second
    matcher. There is deliberately NO `carried` state for a pseudo-file: it
    is regenerated whole on every read, so "the same line, moved" never
    happened, and re-anchoring prose into a regenerated document would be a
    guess with nothing behind it. All four names ALWAYS exist; two may be
    empty with a stated `reason`, which is a smaller surface than a set whose
    membership varies. The PR body is kept as ONE `pr_meta_json` snapshot,
    wholesale-replaced by `review sweep`, so there is **no revision chain**
    for it — a change is detectable (the hash moves) but the previous text is
    not recoverable, and no surface pretends otherwise.
    `review_timeline::V73_K3_ROUTES` joins invariant 15's `RouteContract`
    walk from both sides.

25. **`kbc-recipe/1`: the op set is CLOSED and is the guard, a repo file is
    a read-only input under trust-on-first-use, and an empty step names
    its reason from a closed vocabulary** (V74-L3a, D11 + D21,
    `src/recipe/`, migration V0038). Four rules, separate to state and
    easy to break one at a time.
    (a) **A recipe can never reference an exec lane, and the TYPE is why.**
    `recipe::ops::Op` is a Rust enum with fifteen variants and no `Exec`;
    an author cannot spell one, a `.kbc/recipes/*.toml` cannot deserialize
    into one, and `ops::tests::the_op_set_cannot_reach_an_exec_lane` walks
    the variant list AND every declared arg name so the property survives
    the set growing. This is invariant 10 ("the daemon never spawns a
    non-git process") and 21(b)'s two-lane-kind ruling, restated one layer
    up: the `facts` op reads `lane_facts` rows the operator's own CLI
    already ingested and cannot cause a tool to run, and it still refuses
    unless `[lanes]` enabled that lane — a recipe can never enable one.
    D21's other half holds here too: `?p.`/`?ctx.` values land in declared,
    typed params a closed op set consumes, so no query parameter names a
    route, a command or a tool. Every op declares the address kinds it
    ACCEPTS and the kind it PRODUCES, so the DAG is type-checked at LOAD
    and a mis-wired step fails by NAME rather than at run time with an
    empty table nobody can explain. Do not add an op whose output kind is
    not decidable from its args and its inputs — that is the seam a query
    language grows through.
    (b) **A repo-versioned recipe is read ONLY from the default ref,
    through the ODB, under TOFU.** Never the working tree (a recipe is
    something the team agreed to; an uncommitted edit has agreed to
    nothing) and never whatever branch is checked out. Trust keys on the
    git blob oid — a CONTENT address — so the same bytes at a different
    commit stay trusted and one changed byte does not; `changed` ships a
    unified DIFF, because "accept this again?" without showing what moved
    is a prompt nobody can answer. `recipe new --from-json -` writes a
    `recipes_server` row and NEVER into the tree, a repo file WINS a slug
    collision, and what it shadowed is REPORTED (`shadowed_by`) rather
    than dropped. `.kbc/` is inert data here in exactly the sense
    invariant 21(a) demands: it can propose a question, never enable a
    lane, name a tool, or change this daemon's configuration.
    (c) **Every step carries a census, and exactly ONE reason is clean.**
    `census::EmptyReason` is closed (eleven values) and only
    `filtered-out` means "nothing to worry about"; `lane-disabled`,
    `no-index`, `scope-excluded` and the rest are facts about the QUESTION.
    A free-text reason would be a reason nobody can test for and a UI
    cannot offer a remedy beside. `every_empty_reason_has_a_producer` is
    the source scan that stops a value being declared with nothing able to
    reach it (the dead-surface defect in this module's shape), and
    `exactly_one_empty_reason_is_clean` pins the posture itself. This is
    the fix for the `recipes/1` defect class D11's repair list names:
    three of the six shipped an honest-looking empty set over a missing
    input.
    (d) **Nothing is minted here, nothing is cached, and a run is
    deterministic.** An op COPIES the class its engine already computed
    (`usages2`'s own `trust`, `entities::class_for`, `rails::noun_trust`,
    `lanes::classing`) and this module has no code that raises one; an
    address whose engine reported nothing says `unknown`, which is also
    what a missing blob says — never a blank and never a zero (invariants
    13/20/21/22, root invariant #2). Results are recomputed per request
    and persisted ONLY when the operator asks for a materialised run over
    loopback, which records the mirror `generation` it was computed at so
    a replay captions itself stale instead of reading as live. Determinism
    is a property of the whole path (every op sorts on the address itself,
    identical step calls are memoised, only the budget consults a clock)
    and `two_runs_are_byte_identical` pins it. `recipes/1` stays FROZEN
    beside this — the `/api/usages` → `/api/usages/2` treatment — with its
    six bodies adopted as native adapters rather than re-expressed in the
    op set, because `new-public-api`'s language rules and
    `god-functions`' fan fold would each cost a recipe-shaped op variant.
    `recipe::routes::V74_L3A_ROUTES` joins invariant 15's `RouteContract`
    walk from both sides; the four mutations are absent from it for the
    reason `boards`' own four are.
26. **The Workspace re-key: every repo-keyed table declares WHICH identity
    owns its rows, the value comes from a TRIGGER, and the epoch it bumps
    is a one-way door with a backup and a rehearsal** (V75-M1, D13/D14,
    `src/workspace.rs` + `src/rekey/` + `src/backup.rs` + `src/frames.rs`,
    migration V0040). Five rules, separate to state and easy to break one
    at a time.
    (a) **Two nouns, and a third word that is not one of them.** A
    *Workspace* is one shared git OBJECT STORE (id = canonical
    `--git-common-dir` + root commit); a *Worktree* is one CHECKOUT (id =
    the admin-dir name, or the reserved `(main)`) whose **path is a mutable
    attribute** — `git worktree move` changes the path and never the
    identity, which is the whole reason the path is not the key. D26's
    kbc-seq/1 ALREADY owns the identifier `reading_sets.workspace_id`
    (V0029), where it means a reading set of kind `workspace` (the Desk);
    that is a different concept and `kb-code workspace` (singular) is its
    verb. The re-key therefore never adds a column called `workspace_id` to
    a table that could be read as a Desk — which is one of the two reasons
    the four authored, path-anchored projection tables take `worktree_id`,
    and why invariant 14's "canvas_sets deliberately has no `workspace_id`"
    is still literally true.
    (b) **`rekey::REPO_KEYED_TABLES` is the declaration, and the walk is
    the teeth.** `object` = a row that is a function of the object store (a
    blob, a commit, git history), so two checkouts derive the identical row
    → `workspace_id`. `worktree` = a row that is a property of a PATH ON
    DISK in one checkout (the mirror, its attention lane, working-tree
    annotations, review checkouts, the authored path-anchored projections),
    where sharing would be a WRONG ANSWER rather than a saving →
    `worktree_id`. `meta` = about the daemon, not about code (`repos`, the
    audit ledger — re-keying it would rewrite history — and the
    cross-daemon doc-lens pin). `store::tests::v75_m1::
    every_repo_keyed_table_is_classified` walks `sqlite_master` against the
    list from BOTH ends, so a table added later with a `repo_id`/`repo`
    column fails the build until it declares a class. The blob-keyed
    derived tables (`symbols`/`highlights`/`occurrences`/… ) are OUTSIDE it
    because ADR-2 keyed them by `(blob_hash, salt)` in V0001 and they have
    always been shared — absent by design, not forgotten, and a test says
    so.
    (c) **The value is written by a TRIGGER, next to the column.** V0040
    creates one `AFTER INSERT … WHEN NEW.<key> IS NULL` trigger per keyed
    table, reading the identity off the `repos` row the row already points
    at. That is the one-home rule applied to a denormalisation: a future
    `INSERT` cannot forget a column list it never touches, no `Store` write
    signature changed, and `every_keyed_table_has_a_trigger_that_writes_its_key`
    fails by name on a keyed table without one. `Store::set_repo_identity`
    is the ONLY writer of `repos.workspace_id`/`worktree_id`, and
    resolution runs in the background pass, never on the bind path — it
    shells `git rev-list --max-parents=0 HEAD`, which walks the whole
    reachable history, and it is paid ONCE per volume because the recorded
    root commit is reused on later boots. `src/workspace.rs` spawns no git
    process of its own (every read goes through `history::run_git_raw`),
    which is why its fixture-building tests live in `tests/`, outside
    invariant 3's scanned tree.
    (d) **The key was added; the rows were NOT collapsed, and that is a
    ruling.** No read widens from `repo_id = ?` to `workspace_id = ?`. A
    shared read over two repo ids returns each row TWICE; sharing needs the
    PRIMARY KEY to become `(workspace_id, …)`, which in SQLite is a table
    REBUILD, which is O(every row) INSIDE a migration — i.e. exactly the
    whole-corpus pass between `Store::open` and the bind that invariant
    11's V72-B0(a) forbids. And no surface today registers two worktrees of
    one workspace as two repos, so a widened read would be code no
    configuration can reach (the v7.0 dead-surface defect).
    `rekey::READS_NOT_WIDENED` is the ledger — cross-checked against the
    object class BOTH ways by test — and the collapse belongs with the
    worktree lifecycle verbs, under its own backup + epoch + rehearsal. The
    ONE read that does go through the new key is the per-workspace
    derived-row census on `GET /api/workspaces`.
    (e) **An epoch bump owes a backup and a rehearsal.** `backup::
    ensure_for_epoch_crossing` runs inside `Store::open`, after
    `refuse_if_volume_ahead` and before the refinery runner: the first boot
    that would carry a volume across `backup::REKEY_EPOCH` takes a `VACUUM
    INTO` snapshot named for the epoch it restores to, writes a
    `kbc-backup/1` receipt, and REFUSES the boot when it cannot — because
    `refuse_if_volume_ahead`'s only remedy is "restore the backup matching
    epoch V\<n\>", and the 13.5 h kbc outage was that sentence being true
    with no such backup. `REKEY_EPOCH` is a LITERAL, not
    `store::schema_epoch()`: the gate must keep firing for exactly this
    crossing after V0041 lands and must not re-fire for every routine
    additive migration. `KB_CODE_I_HAVE_A_BACKUP=1` is the one override and
    logs a warning naming itself. `kb-code backup` and `kb-code
    rehearse-migration` are LOCAL FILE verbs with no route and no daemon —
    a backup you can only take through a running daemon is the one you
    cannot take when the daemon refuses to boot. The rehearsal's row census
    is the contract it proves: **a re-key adds columns, never rows.**
    (f) **`kbc-frames/1` is D14's table, and this unit consumes none of
    it.** `frames::FRAMES` says per lane where its answer comes from off
    the working tree and what it may claim there; `off_head` is a CEILING,
    never a promise, and `ref_aware: false` means the lane IGNORES a ref,
    which a reader must say rather than silently substitute. Golden-pinned
    (`tests/fixtures/frames.golden.json`) and served verbatim, so every
    future off-HEAD banner DERIVES from these bytes instead of restating
    them — invariant 17(a)'s one-projection-two-renderers rule, stated
    before the second renderer exists.
    (g) **One worktree oracle; lifecycle is the loopback working-tree
    lane; removal only for daemon-created** (V76-R3b, `src/worktrees.rs`).
    `worktrees::classify(path)` is the only function that answers "what is
    this path?" (`kind: main|linked|bare|not-a-repo` plus workspace id,
    admin-dir name, common dir). `routes::repo_is_worktree` and
    `entities::worktree_key_for` call it; `GitRepo::is_worktree` stays the
    gix primitive (`git_dir != common_dir`) so a gix handle does not spawn
    git or walk history, and a fixture test pins that it agrees with
    `classify.kind == Linked`. `workspace::enumerate` takes linked ids
    from it — a second "is this a worktree?" helper is a regression. Create /
    lock / unlock / repair / prune / delete ride the SAME loopback-only
    sub-router `checkout.rs` already uses (the working-tree mutation lane);
    the daemon never provisions a worktree on behalf of an agent beyond
    these verbs. `DELETE` is refused unless `created_by_daemon` (preserved
    across `replace_worktrees`) and the caller has seen the loss preview
    (`preview_seen: true`, `confirm` equals the id). Readiness detects and
    never fixes; the lock-reason owner parse is `likely` at best
    (`"locked — owner unknown"` on failure) and does not take silence as
    an input. The unified inbox's `worktrees` lane is
    surfaced-never-scored and degrades honestly on an empty workspace
    table.

26. **`kbc-tour/1` is a BOARD (one step model), and `kbc-trail/1` is OFF
    by default with pause, purge and retention shipped in the same unit**
    (V74-L3b, `src/tours/` + `src/trails/`, migration V0039, design D12 +
    D17 + D10). Two families, four rules, and each is a way to break the
    posture quietly.
    (a) **A tour creates no table.** D10 rules that "a board's `steps` and
    a tour share the step model — do not build two", and the strongest
    reading of that is the one this unit takes: a tour is a
    `canvas_boards` row with `kind = 'tour'`, its steps ARE `canvas_nodes`,
    their order and cameras are `canvas_steps`, and consecutive steps are
    joined by GENERATED `then` edges. So there is one lint
    (`tours::lint::check` lowers to a `BoardDoc` and calls
    `boards::lint::check` — it ADDS four rules and re-implements none), one
    resolver (`boards::resolve::resolve_node`, over `CanvasNodeRow`), one
    snippet-capture rule (`boards::routes::build_nodes`, invariant 21's
    exact rule) and one walkthrough contract. `canvas_boards.kind` is
    Rust-validated (`tours::is_valid_board_kind`), never a SQL CHECK
    (invariant 9's house convention), and it is a REQUIRED parameter on
    every board/tour store read — `list_canvas_boards`,
    `get_canvas_board`, `set_canvas_board_status`, `delete_canvas_board`,
    `seq_canvas_boards` — because a read that forgot to say which family
    it wanted would silently return the other one. The `UNIQUE (repo_id,
    slug)` spans BOTH kinds, so an apply colliding across families raises
    `StoreError::SlugTakenByOtherKind` (a 409 naming the family) rather
    than an opaque constraint error. The `ref` string sugar is
    `review_doc::refs::parse_ref` (the ONE ref parser) lowered into
    `boards::RefFields`, and it covers `code:` with a line ONLY — every
    other scheme is refused NAMING the structured field that does the job,
    and `sym:`/`ent:` are refused specifically because resolving a symbol
    to a path and persisting it would be the cached-class invariant 24(a)
    and root invariant #2 forbid. Do not widen the sugar by resolving
    anything at apply time.
    (b) **Trails are OFF on first boot, and there are TWO switches.**
    `[trails] enabled` (default `false`) is the operator's master switch;
    the runtime mode (`off`/`recording`/`paused`, `trails_state`) is a
    separate persisted decision written ONLY by the loopback-only, audited
    `POST /api/trails/state`. A fresh volume reads `off` even with the
    config `true` — the absence of a decision is not a decision to record —
    and a stored mode a disabled daemon still holds reads back as `off`,
    because an indicator saying "recording" while nothing is recorded is
    the one lie this surface exists to prevent. Every write refusal names
    WHICH gate it hit (`trails-disabled`/`trails-off`/`trails-paused`);
    they are three different fixes. D17 permits this ledger only in the
    milestone that ships pause, purge AND retention together, so
    `trails::routes::{set_state,purge_trails}` and `trails::gc` are not
    follow-ups — they are the precondition, and none may be removed while
    the ingest route stands. `purge` is deliberately NOT gated on
    `enabled`: an operator who just turned the feature off must still be
    able to delete what it recorded.
    (c) **Nothing finer than a step can be STORED, and the finest thing
    ever RETURNED to an agent is a day.** `trail_steps` is the finest row
    in the schema and holds one derived, quantised `dwell_secs`; there is
    no client-supplied dwell field, and `trails::reject_sub_step_keys`
    refuses a payload naming a viewport/caret/scroll/per-line dwell BY
    NAME on the raw JSON before the typed parse (`boards::lint`'s
    `coordinates` precedent — the message teaches the rule). A sub-step
    span is REFUSED, never rounded. `GET /api/trails/aggregate` is the ONE
    agent-facing read: it groups on `trail_steps.day`, never selects
    `entered_at`, and `store::TrailAggregateRow` has no timestamp field to
    leak one into. The two HUMAN reads (`GET /api/trails`,
    `GET /api/trails/{id}`) are LOOPBACK-ONLY — stricter than any other
    read in this crate, for invariant 23(b)'s reasoning applied to
    attention data. Never add a per-line table, never widen the aggregate
    below the day, and never move the human reads onto `auth_bearer`.
    (d) **No trail number is a score, a gate or a trust class.**
    `trails::tests::no_ranking_module_imports_the_trail_ledger` walks this
    crate's ranking sources and fails BY FILE if one names
    `crate::trails` — `claims.rs`'s own pin (invariant 23(a)), which is
    root invariant #10's surfaced-never-scored law. A source scan has a
    scan's limits; it is the same trade `git_argv_lint` (invariant 3)
    makes. Dissent notes on an AUTHORED trail reuse the `annotations`
    store through a nullable `trail_id` (a reply inherits it via
    `routes::inherit_scope_field`, third instance) and SURVIVE a purge,
    because a note is the human's own authored words — invariant 23(a)'s
    ruling, applied to the one table a purge could plausibly have taken
    with it. `tours::V74_L3B_TOUR_ROUTES` and
    `trails::V74_L3B_TRAIL_ROUTES` join invariant 15's `RouteContract`
    walk from both sides.
27. **`branch-facts/1`: the base is CLASSED and never silently defaulted,
    `stale` is distribution-derived and never asserted over an open review,
    and a `Co-authored-by:` trailer is never agent provenance** (V75-M3,
    D15/D18, `src/history/facts.rs` + `src/branches.rs`). Four rules that
    are separate to state and easy to break one at a time.
    (a) **One pass, one cache key.** `facts::enumerate` is ONE `git
    for-each-ref` whose `--format` carries the ahead/behind
    (`%(ahead-behind:<sha>)`, `%(upstream:track)`) and the agent trailers
    (`%(trailers:key=Kb-Session…)`) that would otherwise be a subprocess
    per ref. `%(ahead-behind:)` is git ≥ 2.41 and `for-each-ref` fails the
    WHOLE invocation on an unknown atom, so there is exactly one documented
    retry without it, and `rules.ahead_behind_source` says which ran — a
    degrade that does not announce itself is indistinguishable from a repo
    with nothing to report. The derived base is cached in-process by
    `(repo, full_ref, tip_sha, default_sha)` (`facts::BaseCacheKey`);
    "invalidated when the ref moves" is obtained by the KEY, never by an
    invalidation pass, and `default_sha` is in it because the base moving
    changes the answer too. Nothing is persisted: a branch fact is
    derivable from the repo at any instant, so a table would be a second
    copy of git that can go stale in ways a key miss cannot.
    (b) **The base is a four-rung ladder and the CLASS always rides the
    wire** (`upstream` → `fork-point` → `merge-base` → `unknown`). Rung 1
    skips an upstream that is this branch's own remote MIRROR (`feature` ↔
    `origin/feature`) — that is a push target, not a base, and counting it
    as one reports "0 ahead" for every pushed branch. Rung 4 is a real
    answer: `base.ref` is `null` AND `ahead`/`behind` are ABSENT, never a
    measured-looking zero (`branches/1`'s own V70-A3X ruling, inherited).
    `POST /api/branches/review` REFUSES an `auto` base that came back
    `unknown` rather than reviewing against the default branch by
    substitution, and reports an explicit `--base` as `unknown` because the
    daemon did not detect it.
    (c) **`stale` and `active` PARTITION the set, the rule is on the wire,
    and an open review always wins.** The threshold is the 75th percentile
    of THIS repo's own last-activity ages — under
    `facts::MIN_REFS_FOR_DISTRIBUTION` refs there is no distribution and
    NOTHING is stale, with `rules.stale.degraded_reason` saying so. A
    branch with an open review is never stale however old it is; that is
    not a tie-break, it is the rule. `merged` is the same discipline in a
    different shape: the WITNESS is never omitted — `ancestry` is free (0
    ahead of the base IS the proof) and `patch-id` (`git cherry`, the only
    thing that sees a SQUASH merge) is a capped probe whose `probed` of
    `candidates` is reported, so an unprobed row is honestly unmerged
    rather than wrongly clean.
    (d) **D18's never-clause is structural.** `agent.class` is `exact` only
    for a machine trailer NAMING the run, `likely` only for an author email
    in `[branches] agent_emails`, and a `Co-authored-by:` trailer is not an
    input to `facts::agent_provenance` AT ALL — there is no field for it on
    `RawRef`. In an agent-assisted workflow that trailer is the shape a
    HUMAN-authored commit takes, so reading it as provenance would label
    the operator's own commits agent; `exact` is deliberately not
    configurable, so no deployment can widen its way back into that.
    Pinned by
    `facts::tests::a_co_authored_by_trailer_alone_is_never_agent` and by
    the fixture branch `human/assisted` in
    `tests/history/branch_facts_route.rs`.
    (e) **`branch:`/`touches:`/`by:`/`agent:` are kbcq/1 keys whose
    consumer is NOT `unified.rs`.** Invariant 16(a)'s dead-surface walk
    took a module MAP for exactly this: forcing four branch atoms through
    the unified search box so they had a consumer there would have created
    the dead surface that walk exists to catch, dressed as compliance. A
    `consumer_module` naming a file absent from the map still fails, by
    name. `touches:` is the one atom that costs a subprocess per branch, so
    it is capped (`facts::MAX_TOUCHES_SCAN`) and captions `scanned` of
    `candidates`; its pathspec goes after an explicit `--` and is asserted
    by `tests/security/git_argv_lint.rs` like every other one.

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
