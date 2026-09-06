# kb architecture invariants & code map

The deep "developing" reference for the kb workspace. The root
[`../CLAUDE.md`](../CLAUDE.md) carries a one-line **index** of every invariant
below (so the "you'll break this" signal stays in the always-loaded guide); this
file holds the full text plus the file-by-file code map and the Rust/build
pitfalls. Read the matching entry here before changing a load-bearing subsystem.

Invariants that live **entirely inside `kb-core`** (lance schema, storage actor,
atlas determinism, embedding dim per-kb, enrichment-hook registry) are NOT
repeated here — they live in
[`../crates/kb-core/CLAUDE.md`](../crates/kb-core/CLAUDE.md). Surface specifics
(authoring, comments, config, deploy, web) live in the rest of `docs/`; the
HTTP API canon lives in [`../README.md`](../README.md).

## Architecture invariants

These constraints are load-bearing — breaking them surfaces as subtle runtime
failures, not compile errors.

> **Invariant budget (2026-07-02): 35 is the cap.** This list is capped at 35
> numbered slots. Adding a new invariant requires **retiring or merging an
> existing one** — a genuinely new load-bearing constraint means one of the
> current 35 has become obsolete (or subsumes / is subsumed by the newcomer).
> Retirement OPENS a slot the newcomer fills; retired slots keep their number so
> cross-references (`#N`) never shift. **Slot #2 was filled by kb-users/1
> (v0.34)** — it had been open since kb-tui's retirement (v0.24) — and, in the
> DCB milestone (2026-08), #2's identity-is-attribution text was merged into #4
> (`auth_bearer` is the ONE admission + attribution gate) to make room for the
> doc↔code bridge, which now fills slot #2. No slot is currently open, so the
> next invariant must retire or merge one. Keep this file and the
> [`../CLAUDE.md`](../CLAUDE.md) index at the same 35 slots so the
> always-loaded "you'll break this" signal stays legible.

### 1. Arrow version pinning

lance + arrow MUST share one version (`=57.3.1`). `cargo metadata` showing two
arrow versions = the dep graph won't unify RecordBatch types. See
`Cargo.toml [workspace.dependencies]`.

### 2. The doc↔code bridge: kb extracts hints, kb-code mints classes, nothing is cached (DCB v1)

kb and kb-code both know about a doc's references to source code, and the split
between them is structural, not stylistic.

**kb's half is pure, corpus-local extraction.** `kb_core::coderefs` reads ONE
artifact's parsed body (`EnrichCtx::html` — so a Markdown note and an HTML
report go through the same grammar, and the `<template id="kb-prompt">` /
`<script>` / `<style>` / `<noscript>` subtrees are skipped exactly as
`parser.rs`'s `TEXT_SKIP_TAGS` skips them, invariant #5) and emits a closed set
of refs against a **closed grammar**: whitelisted-extension paths (± `:line`,
`:a-b`, `:a,b,c`), `Namespace::Class` (the `::` is REQUIRED — a bare CapWord is
never a symbol), `Class#method`, `path#member`, gem/vendor paths as `external`,
and GitHub issues from `<a href>` only. No LLM, no I/O, no clock, no network —
deterministic given the bytes, golden-pinned in `crates/kb-core/tests/coderef_goldens.rs`.
Refs ride their own `code_refs` / `code_refs_docs` tables, NOT `edges`: the
edges PK is artifact↔artifact and `backlinks_of` / the graph BFS / the atlas
link layer / the orphan-delete cascade all assume `get_by_id` resolves the
destination, which a repo path never will.

**Every kb-side field is a HINT and is named like one** — `path_hint`,
`line_start`, `symbol_container`, `context_tokens`. kb has no working tree and
no symbol index, so it is *structurally* unable to say whether a path exists,
whether a basename is ambiguous, whether a cited line still holds, or how
precisely a symbol resolved. It must never learn: adding a resolved column here
would create a second, stale answer to a question kb-code answers correctly.

**kb-code's half is classification, computed per request and NEVER persisted.**
`codelens/1` owns the compound honesty vocabulary (`path_state`, `line_state`,
`symbol_state`) and its own trust words — it never borrows `resolve.rs`'s
`CLASS_*`, because those mean SCIP-grade *symbol* precision and a doc-derived
name match earns strictly less. No cached verdict may outlive the tree it was
computed against; the W3 reverse index stores CLAIMS only (doc X cites path Y)
plus the resolving sha, and re-validates on read. A claim exists only under a
live pin (W3.A.R): unpin and boot-prune both drop the doc's `doc_refs` rows
outright, so a `repos.id` reused for a different tree can never resurrect a
stale claim under it.

**One live call direction: kb-code → kb.** kb knows kb-code solely as an inert
config URL (`[kb.*] code_url`); it never opens an HTTP client to it. The
browser calls kb-code directly, gated by kb-code's own doc-lens-scoped CORS.

**`kb-sibling/1` — how the two binaries recognise each other.** Two
independently deployed daemons need a contract, and both halves of this one are
HARD (rust-analyzer's advisory-only handshake is the counter-example: advisory
IS the documented failure mode). *The Hello*: both daemons carry
`sibling_protocol` (`"kb-sibling/1"`, compared EXACTLY — a different string is a
different contract, not a newer one), `sibling_major` (`1`), `schema_epoch`
(this binary's highest EMBEDDED migration version) and `build_sha` additively on
`GET /api/identity`; `/healthz` stays PURE liveness on both and learns none of
it, so an orchestrator's restart loop can never be driven by a contract
mismatch. *The boot guard*: `kb_core::sibling::refuse_if_volume_ahead` compares
each sqlite volume's own epoch (`MAX(version)` in refinery's
`refinery_schema_history`) against the binary's BEFORE migrations run — kb from
`Db::open`, per kb as it opens; kb-code from `Store::open` — and REFUSES TO
BOOT when the volume is ahead, naming both epochs, the path and the
remediation. Refinery only ever migrates forward, so without it an old binary on
a forward-migrated volume boots green and fails per-request on columns it
doesn't know (the 13.5 h kbc outage, 2026-08: a deploy rollback). *The client
handshake*: `KbClient` probes kb's identity once per process (lazy, cached — the
answer is a property of the two BINARIES, so only a restart refreshes it) and
fails CLOSED on a mismatch with its own `kb_sibling_mismatch` reason, distinct
from "unreachable" because the peer is up and answering. Two outcomes
deliberately are NOT refusals: an ABSENT Hello is a legacy peer (grandfather
rule — a rolling deploy will reach the not-yet-upgraded kb, and refusing there
turns an ordering detail into an outage), and an unreachable/non-2xx probe
reached no conclusion at all, so nothing is cached and one blip can never latch
federation off.

**`code_refs` is registered in ALL THREE artifact-id lifecycle registries** —
`CASCADE_STEPS`, `SWEEP_TABLES`, and the literal per-table list inside
`cascade_relocate_doc`. None of them fires on an OMISSION (the goldens only
fire when you ADD an entry), and relocate deliberately preserves the embedding
without re-indexing (#27/F3), so a table missing from the rekey tx would leave
refs stranded under a dead id with nothing to regenerate them —
`kb refs <new-id>` empty forever, rendered as doc-rot.

**So is `memory_commits` (V0038, CT-F1)** — the same three registries, keyed on
its `artifact_id` column (the CAPTURE that recorded the claim), for the same
reason: nothing fires on an omission and relocate never re-indexes, so a missed
registration strands the memory→commit join under a dead id forever. Its
`memory_id` column is deliberately absent from all three — it names a memory
that usually lives in a DIFFERENT kb, so this kb's `keep` set says nothing
about it (sweeping on it would wipe the table on the first reconcile) and a
same-kb rekey would be a guess, since ids collide across corpora (#7 v2/#28
v2). Same accepted limitation as `memory_recalls.memory_id`.

**`slo_snapshots` (V0039, CT-F5) is the counter-example, and the reason to
state the rule as "artifact-id-keyed"** — it is deliberately absent from all
three registries, and that absence is the CORRECT registration, not the
oversight it superficially resembles. The table carries no `artifact_id` and no
artifact-derived key: a row is a WHOLE-CORPUS reading at a wall-clock instant
(the kb is implied by which per-kb sqlite file it lives in). Deleting, moving
or reindexing a document must NOT rewrite or reclaim a past reading — that
would falsify the history the append-only log exists to keep. CT-F5's other
write, the CT-A3 recall parse census, went the opposite way for the same
reason: it IS per-capture, so it rode into `sessions` (three nullable columns)
rather than into a sibling table that would owe its own three registrations —
`sessions` already has them.

Pins: `kb_core::coderefs` grammar goldens · `code_refs_rekey_on_relocate_cascade`
· `memory_commits_rekey_on_relocate_cascade` ·
`cascade_delete_removes_memory_commits` · `sweep_removes_orphan_memory_commits`
· `cascade_cleanup_tables_are_pinned` · `record_code_refs_never_bumps_generation`
· kb-sibling/1: `kb_core::sibling` unit tests ·
`open_refuses_a_volume_whose_schema_epoch_is_ahead_of_this_binary` (kb-core,
kb-code-server) · `per_kb_index_db_refuses_to_open_when_the_volume_epoch_is_ahead`
(kb-server) · kb-code's `sibling_handshake_*` (ok / mismatch / absent-fields /
unreachable)
· kb-code's `doclens_never_emits_a_resolve_trust_class` (reconciled: R7 — this
spec's earlier draft named it `doclens_never_emits_class_exact`; `12-w1c` §15
is the owning spec for that test and R7 rules its name; this reference is
updated to match so `check-invariants.sh`'s grep target and the shipped test
agree, m24).

### 3. `ConnectInfo<SocketAddr>` read from request extensions

Not as an extractor (`req.extensions().get::<ConnectInfo<SocketAddr>>()`). A
missing `ConnectInfo` fails CLOSED — `request_is_loopback` returns `false` (→
enforce auth/rate-limit); treating it as loopback would fail-OPEN the security
layer if the router were ever served bare. Every `serve_*` entrypoint wires
`into_make_service_with_connect_info`, so it's never `None` at runtime.
Middleware tests pass an explicit `ConnectInfo` (the `req_with` helper).

### 4. `auth_bearer` is the ONE admission + attribution gate

`auth_bearer`, `rate_limit`, `outbound_scrub` skip only when the *genuine* client
is loopback.

- The immediate TCP peer must be a trusted hop first — `127.0.0.1` / `::1` or a
  `[server] trusted_proxies` IP — before `X-Forwarded-For` is consulted at all.
- XFF is walked **right-to-left** (`middleware::xff_real_client`), skipping
  trusted hops, so a forged leftmost `X-Forwarded-For: 127.0.0.1` can't fake a
  loopback origin (a real proxy appends the true client IP on the right).
- `trusted_proxies` is empty by default. A same-host proxy works (it connects
  from loopback, always a trusted hop); a dockerised proxy reaching the host over
  the bridge needs its IP listed or the bypass never engages.
- **Fail-closed on a token-less public bind.** When NO token is configured the
  bypass is the *only* thing standing between the network and `DELETE /api/kb`,
  so `serve_with_paths` refuses to start on a non-loopback `local_addr` (incl.
  `0.0.0.0`) unless `KB_ALLOW_NO_AUTH=1` is set (the "an upstream proxy is the
  gate" opt-out). `auth_bearer` enforces the same rule at request time:
  no-token + non-loopback genuine client → `401`, not a silent bypass. The
  decision lives in two pure helpers next to the rule —
  `middleware::refuse_public_bind_without_auth` (startup) and
  `middleware::allow_no_auth` (the env knob) — so both sites agree. Loopback
  binds and any configured-token deployment are unaffected.

#### Identity resolution (v0.34, kb-users/1)

*(Slot history: #2 held kb-tui's "dirty snapshot excludes animation counters"
until v0.24 removed that crate with its only test pins; the slot stayed open
until v0.34 filled it. Do not renumber.)*

kb resolves **who** a request is, never **whether** it may enter. Authentication
stays at the edge (Authelia forward-auth) or in possession of a bearer token;
kb owns no passwords, sessions, roles, or ACLs. Design:
[research/kb-users-identity-design-2026-08.html](research/kb-users-identity-design-2026-08.html).

**One resolution point.** `auth_bearer` (`kb-server/src/middleware.rs`) decides
admission, then computes an `Identity { user, source }` and inserts it into
request extensions. Handlers take `Extension<Identity>`; **no route may read an
identity header itself** — one home for the rule, so the trusted-hop gate can
never be bypassed by a handler that "just needs the username".

**The ladder** (first match wins; TOKEN BEATS HEADER — explicit beats ambient):

1. **Registry token** — a `<config>/tokens` entry matched, presented either as
   the `Authorization` bearer or as `X-Kb-Token`. The second carrier exists
   because the production edge (traefik `kb-inject-bearer`) OVERWRITES
   `Authorization` with the shared daemon token, so an edge-lane agent has
   nowhere else to put its own credential. Entries are `<user>:sha256:<hex>`
   (preferred) or `<user>:<plaintext>`; parsing is
   `kb_core::identity::parse_tokens_file` — the CLI (`kb token issue|revoke`)
   uses the SAME parser so its notion of a live line can't drift from the
   daemon's.
2. **Trusted identity header** — the configured header (default `Remote-User`)
   is read ONLY when the immediate peer passes the same trusted-hop gate as XFF
   (`peer.is_loopback() || trusted_proxies.contains(peer)`, #4). An untrusted
   peer's header is never read — fail-closed. Belt-and-braces at the edge:
   traefik's forwardAuth DELETES the client's value of every
   `authResponseHeaders`-listed header before copying Authelia's, so the
   configured header MUST be listed there (`docs/self-host.md`).
3. **Legacy shared token** → the configured `[identity].operator`.
4. **Loopback with no credentials** → operator.

Every resolved username passes `normalize_username` (trim + lowercase-fold +
validate `^[a-z0-9._@-]{1,64}$`); config `operator`/`users[].name` must already
be lowercase (hard config issue otherwise). History is append-only, so a case
fork would be a permanent ledger split.

**Attribution never gates.** The admission/401 surface is byte-identical to
pre-v0.34 for every case that existed before, and identity resolution runs on
EVERY admitted request — including loopback ones, which must still inspect the
token/header or a loopback agent with its own token would silently attribute as
the operator. A non-empty token registry counts as auth for the fail-closed
public-bind guard but must NEVER tighten `KB_ALLOW_NO_AUTH=1`: adding a registry
to an Authelia-gated deploy must not 401 browser users who carry only
`Remote-User` (unit-pinned).

**The one identity-gated pair.** Comment/reply **body edit + delete** are
owner-only — 403 `urn:kb:errors:not-owner`, with a row whose `user` is absent
(pre-v0.34 file) owned by the configured operator. Enforced on the direct
routes AND inside the batch `apply` path (gating each owner-gated op before
`apply_ops`, preserving its all-or-nothing semantics); the SPA mirrors it via
the pure `canEditComment(comment, me, operator)`, which takes the operator name
from `/api/identity` rather than hardcoding a default. Resolve, reply, attach,
and reanchor stay open to every user — that is the collaboration surface, and
the agent triage loop resolves the human's comments by design.

**Storage keys the username string.** No users table, no numeric ids: Authelia
is the identity store and kb is an attribution ledger across ~20 per-kb sqlite
DBs. V0034 adds `history.user` (default `''`), rebuilds
`idx_history_artifact_open` per-user, and adds `list_entry_user_state`
(per-user list read overrides; `list_entries.read_override` is FROZEN — never
written again, and the reading rollup's override overlay reads the new table).
`''` rows are rewritten to the operator by `identity_backfill`, gated by an
`identity_backfill_done` marker so an override the user has since CLEARED is
never resurrected on the next boot. Accepted tradeoff: a rename shows old rows
under the old name until a future `kb users rename` backfill; never data loss.

**Out of scope, recorded:** kb-code-server (it reuses `auth_bearer`, so an
`Identity` rides along, but no kbc route consumes it), and everything listed as
shared-forever in the non-goal — memory, sessions, pins, saved queries, purge,
config, `DELETE /api/kb`. Every identity is a full co-operator.

### 5. `<template id="kb-prompt">` convention

The per-artifact prompt bundle. `kb_core::parser::extract` reads it; the outbound
scrub CAN strip it on non-loopback (opt-in per kb via
`[outbound] strip_kb_prompt = true`; default `false`). Don't repurpose the id.

### 6. Daemon-owned sidecar ledgers — kb-comments/1 review files at `<state>/<kb>/.review/<id>.json`, and the kb-slate/1 ledger at `<state>/slates/<slug>/ledger.jsonl`

- **id is the artifact's path-based id** (`ArtifactId::from_path` — SHA-256
  prefix of the source-relative path), NOT the content hash: editing HTML keeps
  the id and the review stays attached; only renaming/moving changes it.
- Written via `review::save_atomic` (tmpfile + rename + parent fsync). ETag =
  `sha256(mtime_ns ‖ len ‖ body)` (`review::etag_for`).
- **All comment mutations go through fine-grained `routes::comments` endpoints**
  (add / reply / resolve / unresolve / edit / reanchor / delete + resolve-all),
  each load → typed-mutation → save under the **per-kb `review_lock`**
  (`KbHandles::review_lock_for(&kb)`). Sharded per kb (not daemon-wide, not
  per-(kb,id)): different kbs run concurrently; same-kb mutations — and the
  stale-anchor sidecar prune in `set_anchor`/`delete_comment`, run INSIDE the
  guard — stay serialised so they can't lost-update the sidecar or race a
  load→remove→save. (The indexer is a separate unsynchronized sidecar writer that
  self-heals via its orphaned-key prune.) No client `If-Match`; `GET` emits the
  `ETag` for cheap revalidation.
- **`reanchor`** (`PATCH …/comments/{cid}/anchor` →
  `ReviewFile::set_comment_anchor`) is the ONLY mutation that rewrites a comment's
  `anchor`; the indexer's fuzzy resolver stays detection-only.
- **W6 amendment (2026-07-29) — a second RESOLUTION rule, not a second
  rewrite path.** `reanchor` is still the only mutation; but for a
  `memory-session` capture, "detection" no longer means "parse the raw
  on-disk bytes" — `sessions::view::resolve_capture_anchor` resolves
  `Section`/`Selection` anchors against the DECODED, INTERPRETED transcript
  (the `session-view/1` engine's `SessionView`: turn ids + prose text) since
  the raw bytes contain neither a rendered id nor readable prose (see
  invariant #11's W6 amendment for the full mechanism). `File`/`Chapter`
  anchors on a capture still resolve via the ordinary byte-level path.
- **SLATE amendment (2026-09) — #6 names the daemon-owned sidecar-ledger
  family, not one file shape.** Design:
  docs/research/kb-slate-design-2026-09.html. An amendment to #6, not a new
  slot; the budget is capped at 35 and full. The family is
  `.review/<id>.json` (kb-comments/1), `.attachments/` (#18),
  `.proposals/<id>.json` (kb-proposal/1) and
  `<state>/slates/<slug>/ledger.jsonl` (kb-slate/1): schema-string
  discriminated, each under its OWN lock (per-kb for the first three,
  per-slate `KbHandles::slate_lock_for` for the ledger, which is
  daemon-wide rather than per-kb because a slate keys on a project),
  mutated ONLY through routes and CLI, never hand-edited, never indexed as
  truth, registered in backup/reset. `.review` keeps its reanchor rule. The
  slate ledger is append-only with tombstones; its `seq` is minted under
  the lock and is the revision token; take-lease and age state are derived
  at read time from timestamps plus the #11 live registry, never written by
  a clock (the desk refusal), and `slate.updated` fires once per append,
  never per read (#24).

### 7. `artifact_host_suffix` is runtime config, not a const

`[server] artifact_host_suffix` (default `.artifacts.localhost`), threaded
explicitly through `parse_artifact_id(host, suffix)`,
`parser::extract_links(html, suffix)`, `indexer::run(..., suffix)` from
`KbHandles.origin` / `OriginConfig`; same for `parent_origin`. Never hardcode a
suffix — prod switches it to `.artifacts.<domain>`. Tests use
`kb_core::iframe::DEFAULT_HOST_SUFFIX`.

**Host-grammar v2 amendment (CT, 2026-08-21) — a `{kb_enc}--{id}` QUALIFIED
label disambiguates artifacts that collide on id across kbs.**
`ArtifactId::from_path` hashes only the source-relative path (invariant
#27), so two kbs whose source trees both carry the same rel path — a
shared root `index.html` template is the common case — hash to the SAME
12-hex id. Pre-v2, the bare-id walk over `state.kbs` (`BTreeMap`,
alphabetical) always picked the alphabetically-first owner for a
colliding id: on a 7-corpus deployment where every corpus ships its own
root `index.html`, the alphabetically-first kb (nicknamed "hermes" in
that fleet) silently won EVERY collision, serving its own root page to
every visitor of every other colliding kb (the "hermes-wins" bug). The
fix is a second, opt-in label shape both server and client parse
identically (both golden-pinned — `iframe.rs`'s `host_id_*` tests and
`artifactLinks.test.ts`), added to `kb_core::iframe`:

- **`ArtifactHostId::Qualified { kb_enc, id }`** — `{kb_enc}--{id}`, a
  SINGLE DNS label (never a dot inside — the existing `*.artifacts.
  <domain>` wildcard cert covers it with no routing change). `kb_enc` =
  the owning kb's name with every `_` folded to `-`
  (`encode_kb_name`/`encodeKbForHost`, deliberately lossy — `foo_bar` and
  `foo-bar` encode to the same label, which is fine because resolution
  re-derives `kb_enc` from each CANDIDATE kb's own name rather than
  inverting the encoding); `id` is exactly 12 lowercase hex. The parser
  (`parse_artifact_host_id`) splits at the RIGHTMOST `--` so a `kb_enc`
  that itself contains `--` (e.g. `foo__bar` encodes to `foo--bar`) still
  recovers the id deterministically — it always occupies the last 12-hex
  segment.
- **`ArtifactHostId::Bare(label)`** — everything else (today's 12-hex id
  or a legacy file-stem): byte-identical to pre-v2 behaviour, including
  the alphabetically-first walk (`resolve_bare`, ex-`resolve_artifact_kb`).

**Server resolution** (`routes/artifact.rs::resolve_artifact_kb`): on
`Qualified`, walk `state.kbs` (`BTreeMap` order) for the FIRST kb whose
`encode_kb_name` matches `kb_enc` AND whose storage resolves `id` — both
conditions on the SAME kb; a name match alone never short-circuits the
walk. A total miss (no kb encodes to `kb_enc`, or none of the name-matched
kbs own the id) falls back to treating the FULL reconstructed label
(`{kb_enc}--{id}`) as a Bare id through the legacy walk — honest: this
will normally 404, but it preserves the pathological legacy case where a
filename-stem id happens to contain `--`. `routes/dispatch.rs`'s
subdomain-vs-SPA-shell fork and `kb_core::share::classify_cross_link` both
accept either shape (share drops `kb_enc` on a match — a `ShareCtx` is
already single-kb scoped, so only the id matters there).

**Deploy coupling**: the server accepts BOTH grammars; the SPA
(`artifactOrigin`/`isOriginOfArtifact` in `web/src/lib/artifactHost.ts`,
`kb` now a REQUIRED argument, not optional) emits ONLY qualified origins.
Because two same-id artifacts from different kbs are otherwise
indistinguishable by bare origin — exactly the case `isOriginOfArtifact`
exists to guard, the cross-origin `postMessage` trust boundary (#4/#20/
this invariant) — the daemon should ship WITH OR BEFORE an SPA build that
emits qualified links; they already ship in one image, so this is a
sequencing note, not a code coupling. The practical effect: colliding ids
across kbs now resolve to DISTINCT browser origins
(`{kb_enc_a}--{id}.artifacts.<suffix>` vs
`{kb_enc_b}--{id}.artifacts.<suffix>`), so `postMessage` targeting and the
`kb:link-open`/`kb:link-hover` relay (#20) can never cross-wire two
different kbs' same-id artifacts.

### 8. `history` table is append-only EXCEPT for `scroll_y`/`scroll_max`/`updated_at` on the latest open row per artifact

New visits INSERT; bumps within the 30-min gap UPDATE `updated_at` on the same
row (idempotent for tab reloads); scroll updates UPDATE the latest open row's
scroll fields. The `history.recorded` SSE fires ONLY on INSERTs (not bumps or
scroll) so SPA subscribers don't refetch on every reload — the `is_new_visit`
flag in `OpenResult` gates the emit; don't push that decision into the route
handler. `history_list` sorts `(started_at DESC, id DESC)` for deterministic
same-second order (tests rely on it).

### 9. `kb share` engine lives in kb-core + takes explicit inputs

- The static-export engine (`kb_core::share`) runs INSIDE the daemon (CLI and SPA
  both POST `/api/kb/{kb}/share`), so it can't reach kb-server state:
  `run_share`/`stage_files` take a `ShareCtx { handle, source_path, suffix,
  live_origin, outbound }` the route assembles from `KbContext`.
- **Filesystem walk from `source_path`** (folder subtree, or single file + its
  relative-asset closure via `share::assets`), NOT index-driven — lance holds no
  CSS/JS/image assets, so an index-only copy ships unstyled.
- **In-share cross-artifact links are relativized on export** — `stage_files`
  rewrites every `/a/<kb>/<path>` permalink and `<id>.artifacts.<suffix>`
  subdomain URL whose target is ALSO in the share to a relative deploy path
  (`share::relativize_links`; ids/paths derived from `doc_rel_path` exactly as
  the indexer does, so NO storage round-trip and it resolves even before
  indexing), so a shared folder is self-contained. `--links warn|absolute`
  governs ONLY out-of-share danglers; the relativize pass runs before
  `prefix_permalinks` so `absolute` only ever bounces danglers.
- **`--local` / `POST /api/kb/{kb}/share/export`** returns the staged bundle
  (scrubbed + relativized) as an in-memory `.zip` — no host backend, no registry
  row, no tokens. The CLI writes the zip (`PATH.zip`) or extracts it into a
  directory; the bytes land on the CLI host, not the daemon (right for a remote
  daemon). Entry page + danglers ride out on `X-Kb-Share-Entry` /
  `X-Kb-Share-Danglers` response headers.
- **Export scrub ALWAYS strips `<template id="kb-prompt">`**
  (`scrub::scrub_export`), regardless of `[outbound]` — a static share is more
  exposed than the daemon.
- **CF/GitHub API tokens come from the daemon's environment** (`[share.*]
  *_token_env`, default `KB_CF_API_TOKEN` / `KB_GH_TOKEN`), never the request/CLI
  (`config::read_secret_env`; kb has no `pass://`).
- **Pages asset hash is `blake3(base64(bytes) ‖ ext_without_dot).hex()[..32]`** —
  follow the code, not Doc 24's prose (which omits the base64 step).
- `ShareBackend` is an enum (Cloudflare / GitHub + `#[cfg(test)] Fake`), NOT a
  `dyn` trait. Live deploy/gate/revoke wire shapes are pinned by an `#[ignore]`
  spike with real creds, not CI.

*Share-a-list amendment (S1, 2026-07-30):* the selection model is now **path
walk OR an explicit ORDERED file set** — `share::stage_file_set(ctx, opts,
paths, index)` takes caller-ordered source-relative paths (a reading list's
enriched entries) and reuses the byte-same pipeline via the shared
`stage_from_file_list` (per-artifact asset closure, scrub — kb-prompt still
ALWAYS stripped — relativization maps, dangler policy, deploy-path collision
stays an explicit error). Order is expressed ONLY by a generated
`index.html` TOC (list title/description/notes, `Section` anchors as
`#fragment`s; `Chapter`/`Selection` link plainly), which is always the
`entry_path`; an artifact claiming `index.html` errors; the TOC is generated
HTML (fully escaped, no scripts, no kb-prompt) and never enters scrub. The
daemon surface is `POST /api/kb/{kb}/lists/{id}/share/export` (tombstoned/
unresolvable entries SKIPPED and reported via `x-kb-share-skipped` entry
ids — never silently; empty resolvable set → 400, never an empty zip); CLI
`kb share --list <id-or-title> --local <path>` (publish flags documented-
ignored, like `--local`); SPA "Share (.zip)" on the list page. Tokens/
scrub/dangler semantics are UNCHANGED — the file-set mode adds selection +
the index page, nothing else.

### 10. Agent-memory recall ranks on rank-position × salience × decay

kb doubles as Claude Code's memory. A memory is an ordinary HTML artifact in a
corpus whose `kb.toml` sets `memory_scope = "global"|"project"`, carrying
`kb-salience` (f32 0..1), `kb-decay` (`slow|fast`), `kb-supersedes` (artifact
id). kb stays **LLM-free** — the agent calls `kb remember`; recall is
deterministic. Lance projects no per-hit `_score`, so `memory::rerank` uses each
hit's position in its corpus list (`1/(60+rank)`) × salience × `exp(−k·age_days)`.
**Decay basis (RA3): the write-time `<meta name="kb-created">`** — stamped by the
ingest route, parsed into the existing `created_unix` column (the `kb-created`
meta wins over filesystem btime in the indexer; non-memory artifacts keep btime),
read by recall as `created_unix.or(mtime_unix)`. Because the timestamp lives in
the source it survives reindex, so a rewrite no longer resets a memory's age (the
old `mtime_unix` basis did); pre-RA3 memories fall back to `mtime_unix` until
rewritten. The recall route's inline decay floor reads `kb_core::memory::
DEFAULT_SALIENCE` (RA1), and `kb recall --no-floor` (`?no_floor=true`, RA-recall)
bypasses that floor for dedup callers without touching `rerank`. **Supersede/forget drop happens
at recall time** inside the pure `rerank`, fed by a corpus-wide `kb_supersedes`
scan — so an old memory drops even when its superseder isn't in the hit window.
**Ingest is write-only** (route writes; watcher indexes once — `Created` bypasses
the dedup gate) with a collision-resistant slug (`<slug>-<unix>[-n].html`).
Memory corpora are discovered at daemon (re)start, not hot-added.

**MI-W2 amendment (2026-08) — `scoring_v2`: two additional deterministic
factors, gated off by default; `forget` is now itself a soft-forget
tombstone.** `kb forget` no longer hard-deletes by default: MI-W2.3 makes
forgetting a SOFT tombstone — `kb-status: forgotten` + `kb-forgotten-at`
spliced into the artifact's own source via the existing generic
`meta_edit`/`markdown` splices (the same byte-preserving primitives #12
governs; the memory-specific caller decides which two metas to write, the
generic splice stays general-purpose), then reindexed. A forgotten memory
now stays on disk, still searchable, still census-visible — rather than the
old traceless hard delete. `?purge=true` / `kb forget --purge` still
hard-deletes for the rare case nothing may survive anywhere.

**MI-W5.R amendment (2026-08-08 operator ruling, on W5.1 bench evidence) —
`scoring_v2` split into two independent flags.** The single `[memory]
scoring_v2` flag gated two factors the W5.1 live-corpus bench measured
UNEQUALLY: **relevance** (25 queries, 14 better / 11 wash / 0 worse, 23/25
returning a different id set, +3% mean latency) is strongly evidenced;
**stability** (the FSRS term) was NOT exercised at all — every hit had
`recall_count == 0` because the `memory_recalls` ledger lives in the
sessions corpus, which the probe never loaded, so it remained
fixture-tested only. Shipping one flag that gates both would enable the
unmeasured factor the moment an operator flips the measured one on. `[memory]
scoring_v2_relevance` (`kb.toml`, daemon-wide, **default `true`** — measured,
wins) and `[memory] scoring_v2_stability` (**default `false`** — unmeasured,
pending a bench that loads the sessions corpus) now gate their factors
independently inside `kb_core::memory::rerank_with_policy_scored`, which
takes both as separate bools; `rerank_with_policy` delegates with BOTH
hardcoded `false`, so that entry point's base formula stays byte-for-byte
identical to pre-W2 kb regardless of either flag's default. All four
`(relevance, stability)` combinations are valid and unit-tested. The old
`scoring_v2` key is accepted as a DEPRECATED alias (`Option<bool>` on
`MemorySection`) — if present, it overrides BOTH new flags to its own value
(reproducing the pre-split "one flag gates both" behavior bit-for-bit) and
logs a one-line `tracing::warn!` at parse time, so an existing `kb.toml`
doesn't silently change meaning now that the two replacement flags have
different defaults.

`scoring_v2_relevance = true` (or the deprecated alias) gates a per-corpus
min-max normalized **relevance** factor over the search engine's own raw
score (hybrid `_relevance_score`, BM25 `_score`, or vector `1/(1+distance)`
— whichever the route's query arm produced), computed over the POST-FILTER
survivor set: forgotten/tombstoned/floor-dropped siblings can never skew a
survivor's normalization. This is an MI-W2.R review fix — the first ship
computed the min/max over the raw, unfiltered hit list, so a memory about to
be dropped by the tombstone/`forgotten`/salience-floor chain still
contributed its raw engine score to its corpus's spread, skewing every
survivor's factor. The underlying search queries never excluded
forgotten/tombstoned docs to begin with, and since soft-forget (MI-W2.3) now
KEEPS those rows in the index rather than hard-deleting them, a forgotten
sibling sitting in the hit list is the NORMAL case, not a rare edge — so the
bug was live on every recall against a corpus with any forgotten/tombstoned
memories, not a theoretical concern. Degrades to a neutral `1.0` when a
corpus can't support a meaningful spread (fewer than two scored hits, a tie,
or no score at all on the empty-query/list_docs timeline path) — never a
divide-by-zero or an arbitrary tie-break. This factor is now ON by default.

`scoring_v2_stability = true` gates an FSRS-inspired **stability**
multiplier applied to the decay factor, derived in closed form from the
memory's own W1 `memory_recalls` ledger (`recall_count`/`last_recalled_at`)
plus salience/decay-bucket/created (Bjork desirable-difficulty weighted: a
recall landing close to the decay/salience floor counts for more).
Monotonic non-decreasing in `recall_count`, and capped so a memory can never
become immortal — a finite multiplier can slow an exponential decay curve
but never halt it in the limit. This factor stays OFF by default (unmeasured
on the live corpus).

Both factors, when their own flag is on, are SURFACED on the
`Scored`/`RecallHit` decomposition independently (never silently absorbed
into `score` alone) — `relevance_factor` is `Some` iff `scoring_v2_relevance`
is on, `stability` is `Some` iff `scoring_v2_stability` is on — so `kb recall
--explain` renders the full arithmetic of whichever factors are active.

`recall --as-of` remains unbuilt — a separate, still-standing ruling, and
MI-W2.3 resolves only ONE of its three original blockers. Forget is no
longer an undetectably-traceless hard delete, but the other two blockers
stand: pin state, salience, and decay-policy are still read as CURRENT
values only (no time-versioned ledger for any of them), so an as-of ranking
would chimera historical content against present-day metadata.

The MI-W2.4c EPOCH HONESTY marker (`KbPaths::tombstone_era_file`,
daemon-boot-persisted, `GET /api/memory/tombstone-era`) backs `kb memory
log`'s supersede-chain walk and `kb diff --between`'s date-resolved diff,
both printing a caveat — in human AND `--json` output — when their window
predates it.

**CT amendment (v0.38, 2026-08-21) — the provenance/verification lane is
SURFACED-NEVER-SCORED, and U3 provenance is now READ BACK.** The
connective-tissue milestone added a family of display-only signals around
recall; every one of them obeys the same law the `Scored` decomposition
established: it may ride the WIRE structs (`RecallResult`,
`MemoryCensusRow`, `MemoryRecalledByRow`, `SessionRecallHit` — all
kb-server display types), but it must be structurally unable to reach
`kb_core::memory`'s scoring types (`RecallHit`/`Scored`) or
`rerank_with_policy*`'s arithmetic. Each is computed strictly POST-rank
over only the returned page, and each has a unit test pinning that a hit
with and without the signal at identical score/rank stays byte-identical:

- **U3 parse-back (CT-A1)**: the five write-time provenance metas
  (`kb-author`/`kb-source-kb`/`kb-source-artifact`/`kb-source-anchor` +
  `kb-session`) are parsed back into four nullable lance columns
  (`ensure_u3_provenance_columns`, lazy `add_columns`, backfilled by one
  reindex) and surfaced on census/recall; the REVERSE read
  (`list_docs_with_kb_source_artifact`, filtered on the (kb,id) PAIR) powers
  `GET …/docs/{id}/memories-from` — "which memories were lifted from this
  artifact". `kb memory expand` (CT-B4) walks the other direction: the
  stored `kb-source-anchor` re-resolves through kb-comments/1's EXISTING
  `fuzzy_resolve_anchor` ladder (never a second heuristic) against the
  origin's CURRENT source; an unresolvable anchor renders an honest line,
  never a guessed passage.
- **`flagged` (CT-C1)**: an OPEN `[kb-flag] <reason>` comment (an ordinary
  kb-comments/1 comment, invariant #6 — no new storage) marks the hit
  `⚠ disputed` on recall and jumps it to the TOP of the triage queue
  (`FLAGGED_URGENCY = 2.0`, deliberately above the 0..1 heuristic scale).
  Resolving the comment clears both surfaces; both read only OPEN flags.
- **`used` (CT-C5, V0037)**: a second deterministic pass in
  `derive_memory_recalls` marks a ledger row used iff a turn STRICTLY AFTER
  the injection explicitly names the memory (12-hex id, or its title at
  ≥ 12 chars — `MIN_TITLE_LEN_FOR_REFERENCE_MATCH`) in authored text only
  (Prose/Thinking/Decision — never tool output, so a grep echo can't credit
  it). EXPLICIT-REFERENCE-ONLY by design: acting on a fact without naming
  it reads as unused; every rendering labels it a lower bound. The
  `scoring_v2_stability` fetch reads `recall_count`/`last_recalled_at` from
  the same table and structurally cannot see `used`.
- **`pos` (MR1, V0041 — SL6, 2026-09)**: the RANK a hit held in the pack
  that injected it (1 = top), an additive nullable `memory_recalls` column
  written from the `kb-recall/1` marker's own `pos=` pair and from nothing
  else. It joins this lane under the same law and for a sharper reason than
  the others: a ledger column recording where the ranker put a memory, if
  it were ever readable by `rerank_with_policy*`, would close a feedback
  loop in which today's ranking is evidence for tomorrow's. It rides
  `MemoryRecalledByRow`/`SessionRecallHit` and `kb memory recalled-by`'s
  `[#N]` marker for DISPLAY only. NULL is a first-class value — a pre-MR1
  capture, a fallback-only parse, or a `pos` outside `1..=99` all read as
  "rank unknown", and nothing infers one from the hit's position in the
  transcript (layout `v2-last` prints the pack reversed, so position is not
  rank and no capture records which layout produced it).
- **`recalled-by` (CT-B2)**: the memory-side reverse of the ledger —
  `GET …/memories/{id}/recalled-by` fans out per invariant #28 (the ledger
  lives with the RECALLING session's kb, not the memory's), each per-kb
  read scoped through the CORRELATED `newest_capture_pred` (#11's
  multi-capture collapse; the correlation target must be the qualified
  `mr.session_id` — an unqualified name silently self-correlates against
  the subquery's own range var). Rows carry `used` since CT-C5.
- **`warns` (CT-C3)**: a memory written with `kb remember --failed` carries
  `<meta name="kb-outcome" content="failed">` (the durable declaration —
  absent means ok, there is never an "outcome: ok" noise meta) PAIRED with
  the `outcome:failed` tag (the INDEXED carrier, slugified to
  `outcome-failed`; gallery-filterable via the existing `?tags=` grammar
  for free). `warns` needs no per-hit lookup at all — `tags_csv` already
  rides both recall projections, so the set is collected during the
  existing fan-out and only APPLIED post-rank.
- **`code_hints` / `drift_open` (CT-C4)**: the memory→code scent, computed
  with NO kb-code call anywhere on the path (the red-team killed a blocking
  cross-daemon call inside the `UserPromptSubmit` hot path). `code_hints`
  is ≤5 distinct path-shaped hints from the kb-LOCAL `code_refs` table
  (issue/external kinds excluded) with `code_hints_total` keeping
  truncation explicit; `drift_open` counts OPEN `[kb-drift]` comments filed
  by the `/kb-verify` sweep (CT-C2) — read in the SAME single
  `.review/<id>.json` load per hit as `flagged` (`fetch_review_marks`, one
  read, both marks). Drift is therefore at most one sweep stale by design:
  sweep-then-flag, never live verification.
- **`?sort=unverified` (CT-E4)**: the agent-hot-human-cold drift meter on
  `GET /api/memory/census` — bucket `(recall_count > 0 AND never opened by
  this user)` first, then `recall_count` DESC, `id` ASC ties. Never-opened
  reads the SAME `reading_latest_for_ids` source the SPA's `read_pct`
  keys off, so the server bucket and the SPA badge agree row-for-row; the
  extra full-id-set fan-out runs ONLY on this branch (the default path's
  IO and ordering stay byte-identical).

`kb why-memory` (CT-B1) is pure CLIENT composition of three existing
endpoints — zero new server surface — and its footer says exactly what its
labels mean ("commit still in history", never "fact still true"). The SPA's
Memory Dossier (CT-B6, `ProvenanceThread`) is the integrator over all of the
above — origin class · currency · attention — and its code-citation
freshness lane (shared with the reader's own staleness line, CT-E3) keeps
four DISTINCT honest states: verified counts, "no checkout pinned", a
server-authored refusal rendered verbatim, and "kb-code unreachable"
degrading to the kb-side-only wording. An unreachable sibling never renders
as "fresh".

**MS amendment (2026-08-30) — recall's DEFAULT scope narrows to the caller's
project; `visible_to` is a new per-id link FILTER, not a score term.** Prior
to MS, `kb recall`'s default was `--scope all`: every turn's recall fanned
out across the whole memory fleet regardless of which project the agent was
actually working in. MS makes the CLI default `--scope auto`, which resolves
to `scope=all` PLUS `project=memory-<slug>` and
`visible_to=<slug>,memory-<slug>`, where `slug` is the basename of the git
MAIN-checkout root — derived via `--git-common-dir` so a linked worktree
resolves to the SAME slug as its main tree (the same worktree-safety
`kb remember`'s existing slug ladder already had, and which MS extends the
ladder to reuse rather than re-deriving). Outside a repo, `auto` degrades to
today's fleet-wide behaviour (plain `scope=all`, no `visible_to`) — a
deliberate compatibility floor, not a narrowing bug. A new `--cwd` flag lets
a caller (the `kb-recall.sh` hook, whose process cwd is not reliably the
project directory) supply the directory the slug is derived from explicitly.
Explicit `--scope all|global|project` is completely unchanged and always
wins over `auto`'s derivation.

Two things are load-bearing about where this narrowing lives. First, **the
narrowing is CLI-side, not route-side**: `GET /api/memory/recall`'s own
default stays `scope=all` with no `visible_to` — every existing direct
caller of the route (the SPA, `kb-code`, any script hitting the HTTP surface
without going through the new CLI default) is byte-identical to pre-MS.
`auto` is a CLI-computed set of explicit query params, not a new server-side
mode. Second, **`visible_to` is a FILTER, applied at the same L7 stage
`for_kb` already occupies, and is structurally unable to touch score** — it
sits on the exact same footing as the forget/supersede drops (#10 body) and
the CT provenance signals above: SURFACED-NEVER-SCORED, computed strictly
post-rank-eligibility, never folded into `rerank`'s arithmetic.

`visible_to`'s pass/fail rule is the deliberate INVERSE of `for_kb`'s, and
the two must not be conflated. `for_kb=X` is a strict allowlist: a memory
with NO link rows at all is treated as INVISIBLE (the existing L7 comment's
"race window" reasoning) — it answers "what is visible to kb X's own
`/memory` page", where an unlinked memory hasn't opted into being shown
anywhere in particular. `visible_to=X,Y` is an opt-out: an UNLINKED memory,
or one linked `*` (global), always PASSES; only a memory linked to a
specific, non-matching kb set is dropped. It answers a different question —
"what should leak into project X's session" — where the default posture for
an ordinary, unlinked memory is that it's everybody's context until someone
scopes it away. Same link data, same L7 stage, opposite default because the
two callers are asking opposite questions; implementing `visible_to` as
`for_kb`'s allowlist inverted-and-negated would silently hide the entire
existing unlinked-memory corpus from every project-scoped recall the day it
shipped.

`GET /api/context` (invariant CT-D1 above) threads the same pair through as
`memory_project=` and `memory_visible_to=`, into the memories lane that was
previously hardcoded `scope=all` — both params absent is byte-identical to
pre-MS context packs; nothing else in the context pack's composition
changes. As with every prior corpus-shaped addition here, a NEW memory
corpus (a new project's `memory-<slug>`) is config-only: a `kb.toml` stanza
+ bind mount + daemon restart, discovered at boot per the `10` body above —
`auto`'s slug derivation never creates a corpus, it only ever narrows or
misses one that a human already declared.

### 11. Sessions: `<meta name="kb-session">` is the canonical session id

A "session" is one memory-session artifact (`kb-category=memory-session`) in a
`memory_scope` corpus — typically the verbatim Stop-hook transcript from
`plugins/kb-memory/hooks/kb-capture.sh`. **Canonical-id derivation order
(`parse_session_html_full`): the transcript's own JSONL `sessionId` (ground
truth) → `<meta name="kb-session">` → the `session-<ts>-<sid>.html` filename
regex → stem.** The JSONL wins because a capture-hook bug (`cut -c1-24`) once
truncated the meta + filename to 24 chars (half a UUID), which broke `claude -r`
and desynced the transcript id from the full id `kb remember` stamps — so the
parser recovers the authoritative id from the transcript itself (existing
sessions repair on reindex). `kb remember` stamps the full id on every memory in
a session (read from `~/.cache/kb/current-session`, written un-truncated by
`kb-wake.sh`). One **additive lance column `kb_session`
(Utf8 nullable)**, on every bm25/vector/hybrid + `list_docs` projection so a
memory hit carries its origin session id. The **per-kb `sessions` sqlite
enrichment table** holds the parse-expensive metadata (`session_id`,
`started_at`, `ended_at`, `message_count`, `first_user_prompt`,
`source_relative`), populated by the indexer after each memory-session upsert.
**Cross-kb fan-out at the HTTP layer** iterates `state.kbs` in BTreeMap order
(like `/api/anchors`, `/api/memory/recall`); `memory_count` is a cheap lance
`count_rows` scan excluding the transcript. **Touches scan**
(`extract_touched_ids` — substring scan matching 12-hex ids + source-relative
paths) is cached in the daemon-wide `TouchesCache` LRU keyed `(kb, artifact_id,
mtime_unix)` so the SPA atlas overlay can poll every render without rescanning.

**Multi-capture: read the NEWEST capture, never the union.** A long session is
captured at *every* Stop, so one `session_id` accrues many `sessions` rows (one
per capture, each a distinct `artifact_id`; on a busy day a single session can
have 20+). The newest capture is a SUPERSET — each capture re-parses the full
transcript-so-far. The write side is per-capture (`*_replace` keys on
`artifact_id_session`); every READ that aggregates by `session_id` MUST collapse
to the newest capture or it double-counts. Concretely:
`sessions_get`/`sessions_get_many` take the newest row
(`ORDER BY started_at DESC, artifact_id ASC LIMIT 1`); `sessions_list` and all
four `*_for_session` detail queries (files/decisions/commits/research) filter
`artifact_id_session = (that newest-capture subquery)`; the funnel + research
rollup scope their `COUNT(*)` the same way (a bare join inflated the per-query /
per-stage event totals N×, while `COUNT(DISTINCT session_id)` was always fine).
A read joined on the bare `session_id` is the bug — it re-lists every file /
decision / search once per capture (the "repeated edited/created (N)" inflation).

*PF-R1 amendment (V0040, 2026-09-03):* the newest-capture pick is now
MATERIALIZED as `sessions.is_newest` — same contract, precomputed at write
time instead of re-derived per read. `newest_capture_pred` still exists and is
still the one sanctioned cross-table answer, but it now emits a flag lookup
(`s2.is_newest = 1`) instead of re-sorting the capture group; self-referential
sites read the column bare. The tie-break is unchanged and lives in exactly
two places (`recompute_is_newest` + the V0040 backfill):
`ORDER BY started_at DESC, artifact_id ASC LIMIT 1`. Maintenance is
TRANSACTIONAL, in the same tx as any mutation that can change which capture
is newest — `sessions_upsert` (including the prior-group re-derive when a
reindex repairs a truncated session_id), `sessions_delete`, and the
`sessions` step of `cascade_delete_doc` (delete of the newest promotes the
next); `cascade_relocate_doc` needs nothing (it rekeys `artifact_id` only —
test-pinned). Never maintain the flag from a background job, and never write
`is_newest` directly outside `recompute_is_newest`: the UNIQUE partial index
(`sessions(session_id) WHERE is_newest = 1`) is a live assertion that
double-flagging fails the offending write instead of silently reintroducing
the double-count this whole section exists to prevent. This also structurally
retires the MI-W1.A self-correlation bug class — a bare flag has no
correlation target to get wrong.

**Episodic-memory retrieval (R0–R5).** Sessions are an *episodic* memory the
agent PULLS on demand — distinct from the curated *semantic* memory (`kb
remember`/`recall`), which it pushes every turn. Load-bearing constraints:

- **Transcripts never appear in default search or recall (R0).** A
  `memory-session` row is internal episodic data: `routes/search.rs`
  `Filters::keep` drops it (unless `?category=memory-session` opts in) and runs
  unconditionally on BOTH search paths (the federated path no longer gates
  `keep` on `has_filter`); the recall fold (`routes/memory.rs`) skips it before
  scoring, on the query AND empty-query (`list_docs`) paths. Done in the route
  layer (proven post-filter), not lance — `memory_scope` is untouched and
  `SessionCaptureHook` is category-driven, independent of it. Shared const
  `kb_core::sessions::MEMORY_SESSION_CATEGORY`.
- **The RANKING surface is a deterministic DIGEST; the EVIDENCE lane is
  BOUNDED full text (R1, split 2026-07-21, capped 2026-07-22).** For a
  memory-session row, the indexer's `prepare_doc` replaces `fields.body` +
  `fields.body_text_excerpt` with
  `sessions::session_digest(&parse_session_html_full(...))` — title + first
  prompt + decisions + research queries + commit subjects + touched basenames +
  cwd/branch — so everything that EMBEDS, chunks (SQ5), or excerpts stays
  digest-led (retrieval on meaning, not transcript scaffolding). But
  `fields.code` (the `pre code, pre` parser field — an FTS column that appears
  in NO read projection) is NO LONGER cleared: it carries the verbatim
  main-transcript text plus the sidecar-text block, so BM25 — `/api/search`
  keyword mode, the hybrid BM25 arm, and recollect's candidate pool — finds
  any exact token that ever appeared anywhere in a session. The vector arms
  never see it (a BM25-only lane, operator-ratified 2026-07-21:
  exact-identifier recall was the pain; digest-led semantics is the point).
  **`code` is a BOUNDED evidence lane, not unbounded**: the first ship left
  the main-transcript portion completely uncapped (only the sidecar-text
  block had its own per-agent budgets), and on the live 916-row sessions
  corpus the corpus-wide sum of `code` bytes exceeded Arrow's `i32::MAX`
  (~2.147 GB) ceiling, panicking `lance`'s `interleave_bytes`
  (`arrow-select`'s `interleave.rs`, hit via `upsert_docs`'s `merge_insert`
  path — a small table fits inside one physical batch, so the whole corpus's
  `code` bytes land in one `interleave` call) on `kb reindex` and then on
  every subsequent `kb recollect` call. Fixed 2026-07-22:
  `sessions::truncate_code_field` caps the WHOLE `code` field (main
  transcript + sidecar-text combined, one guard) to
  `sessions::SESSION_CODE_FIELD_CAP_BYTES` (32 KiB/row) post-parse, in
  `prepare_doc`'s memory-session branch — head-60%/tail-40%-truncated around
  a marker line reporting the dropped byte count, UTF-8 char-boundary-safe.
  No entity-escaping step (unlike the sidecar-text scheme): at this point
  `code` is already HTML-UNESCAPED plain parser output (`.text()` over
  html5ever's decoded DOM) and is never re-serialized back into HTML, so
  there's no entity-boundary concern, only a byte-boundary one. Sizing: at
  32 KiB/row the corpus-wide ceiling is only reached at ~65,536 (2^16) rows —
  today's corpus is ~1.4% of that; even at 32,768 rows (2^15, well past "low
  tens of thousands") the worst-case sum is exactly 1 GiB, half the ceiling.
  The raw transcript on disk is untouched (#27) — `code` is an
  index-time-only derived field, never written back to the artifact. Backfill
  = `kb reindex --kb <sessions-kb>`.
- **Retrieval is pull-only, two verbs.** `kb why <file>` (`GET /api/why`) is a
  structured JOIN over `session_files`/`session_decisions`/`session_commits`
  (basename-matched, exact-vs-fuzzy by path alignment, batched session lookup,
  capped) — zero new index. `kb recollect <q>` (`GET /api/sessions/recollect`)
  is federated hybrid/BM25 over the R1 digests, re-ranked by
  `sessions::recollect_score` = `rel × recency_decay` (gentle 365-day
  half-life). **Success/error are SURFACED fields + near-tie-breakers, NEVER
  score terms** — a stronger digest match always wins (golden-pinned).
  **Staleness is a query-time SURFACED signal (age + `stale` flag), never used
  to drop a hit** (#27: episodes are immutable evidence, discounted not
  deleted). Both advertised once at SessionStart (`kb-wake.sh`), never
  per-turn-injected.
- **Insights are more child tables, deterministically parsed (R4/R5).**
  `session_research` (V0020) records kb/web searches, subagents, skills, plan
  spans — extracted from the SAME `tool_use` loop, "detected not ground truth"
  (#10), golden-pinned per block shape, cascade-deleted in `sessions_delete`.
  The session↔comments link (`GET /api/sessions/{sid}/comments`) is a
  computed-on-read JOIN (no edge table): in-corpus `target_artifact_id` →
  `review::load` (never parsing `.review/*` by hand, #6), fanned out (#28).

**Wave-0 amendment (kb-code program, 2026-07-18) — the capture envelope is a
split contract, and the session↔commit join is exact going forward.**
`kb sessions capture` (Rust, `crates/kb-cli/src/commands/sessions_capture.rs`;
`kb-capture.sh` shells out to it, bash heredoc kept as permanent fallback)
writes the same `session-<ts>-<sid>.html`: the `<pre>` transcript block stays
**byte-identical** to the historical heredoc (`recover_jsonl_from_capture`
reads only `<pre>` — round-trip safety is the invariant), while new blocks are
**additive after `</pre>` and absent when empty**: `<script type=
"application/json" id="kb-session-commits">` (per detected sha, one capture-time
`git show -s` at the record cwd resolves `sha_full`, true subject, author,
parent count, and existing trailers — best-effort, `resolved=0` on
missing repo/unresolvable sha; old captures keep transcript-detected values,
`resolved=0`) and `<script … id="kb-session-subagents">` (the
`<sid>/subagents/agent-*.jsonl` sidecar walk, non-recursive so workflow
journals are excluded; 256 KB cap, truncation-flagged). Enrichment prefers the
blocks and falls back to transcript-only parsing. Three additive migrations —
**V0024** (`sessions.subagent_count/tokens/tool_calls/files_edited/
launched_unstatted`: sidecar-primary when the digest block exists — async
agents counted — parent `toolUseResult` sync-only fallback; launched-unstatted
kept separate so zeros never masquerade as "no subagents ran"), **V0025**
(`session_commits.sha_full/repo_root/resolved/author/parents/trailers`),
**V0026** (`session_files.via_subagent`, main-thread rows win on
`(path, action)` collision). The **`Kb-Session:` trailer**
(`plugins/kb-memory/hooks/git-dispatch/`, per-repo `core.hooksPath`
dispatchers, fail-open, source-arg gated, `$CLAUDE_CODE_SESSION_ID` primary /
repo-keyed marker fallback) makes future joins exact; the reverse lookup is
**`GET /api/sessions/by-commit?sha=`** (≥7 hex chars, prefix on `sha` and
`sha_full`, newest-capture-scoped, #28 fan-out) plus the flat bulk feed
**`GET /api/sessions/commit-map`** for external join precomputation (kb-code).

**W0.6 amendment (2026-07-21) — sidecar conversation text rides the
envelope.** A third additive tail block, `<section
id="kb-session-sidecar-text" hidden>` (one `<details
data-kb-sidecar-agent="<agent_id>"><summary>…</summary><pre>…</pre></details>`
per agent, sorted by agent_id), inlines each sidecar's raw JSONL:
truncate-then-escape (same 3-entity scheme as the main `<pre>`, so entities
never split; over-budget agents keep head 60% / tail 40% of their budget
around one fixed marker line), deterministic budgets
(`SIDECAR_TEXT_AGENT_CAP_BYTES` = 2 MiB raw, `SIDECAR_TEXT_TOTAL_CAP_BYTES`
= 8 MiB raw charged by actual use — rendered size may exceed by the
entity-expansion factor), absent when the session has no sidecars, and
byte-identical replacement (`replace_sidecar_text_block`) so
`--refresh-subagents`' NoChange no-op holds across the whole block pair. It
is a SEARCHABLE-EVIDENCE block, not a resumable transcript: round-trip
(`recover_jsonl_from_capture`, export/rehydrate) still reads only the FIRST
`<pre>`, and `subagents/workflows/**` stays excluded from the sidecar walk.
Written by `kb sessions capture`, the plain `kb import claude-history`
backfill, and `--refresh-subagents`, all from one shared sidecar walk.

**W0.6 fix amendment (2026-07-22) — the `code` FTS column is capped, not
unbounded.** The same commit that stopped clearing `fields.code` for
memory-session docs (above) left the MAIN transcript portion completely
uncapped — only the sidecar-text block above had its own per-agent budgets.
`code` is an Arrow `Utf8` column (`i32` offsets); `lance`'s
`interleave_bytes` (hit via `upsert_docs`'s `merge_insert` when re-upserting
matched rows — `dataset::write::merge_insert`'s `interleave_batches`)
accumulates a RUNNING TOTAL of bytes across every row folded into one merged
batch and panics past `i32::MAX` (~2.147 GB) — a corpus-wide cumulative
ceiling, not a per-row one. The live 916-row sessions corpus's uncapped
`code` column exceeded it, panicking on `kb reindex --kb sessions` (which
produced the oversized column) and then on every subsequent `kb recollect`
call (`ensure_fts_index` re-scans on its route). Fixed by
`sessions::truncate_code_field`, capping the WHOLE `code` field (main
transcript + sidecar-text combined — one guard is simplest and is what
actually bounds the corpus-wide sum, regardless of the row's internal mix)
to `sessions::SESSION_CODE_FIELD_CAP_BYTES` = 32 KiB/row, applied post-parse
in `indexer::prepare_doc`'s memory-session branch: head-60%/tail-40%
truncation around a marker line (mirrors the sidecar-text scheme's shape),
but with NO escape step — `code` at this point is parser output
(`.text()`-collected, already HTML-unescaped by html5ever) that's never
re-serialized into HTML, so only UTF-8 char-boundary-safety is needed, not
entity-boundary-safety. Sizing keeps `rows × cap` safely under the ceiling
even at "one giant interleaved batch" worst case: the ceiling is reached
only past ~65,536 (2^16) rows; at 32,768 rows (2^15, well past "low tens of
thousands") the worst case is exactly 1 GiB, half the ceiling. Lance-batching
finding (investigated, not patched): `lance`'s scan/index batching
(`LANCE_DEFAULT_BATCH_SIZE`, default 8192 rows) is ROW-COUNT-based, not
byte-size-based, and is a process-wide env var — no batch-size knob is
exposed on the `FtsIndexBuilder`/`merge_insert` call sites kb-core uses, so
it's not a targeted, safely-scoped lever from here. The per-row byte cap is
the correct fix regardless: it bounds the corpus-wide sum even if lance ever
batches the *entire* corpus into one `interleave` call (the observed
failure mode, since 916 rows already fit under lance's own 8192-row
default).

**Canonical-join corollary (2026-07-21).** The trailing-dash incarnation of
the capture-hook meta bug (jq's newline → `tr` → `<sid>-` on 804/804
artifacts) proved the lance `kb_session` column can lie corpus-wide while
sqlite stays clean — recollect returned `{"sessions":[]}` for every query
because its lance→sqlite join keyed on the meta value. The rule going
forward: **lance `kb_session` is a display/filter HINT only; any join that
needs a session's identity resolves artifact_id→sqlite**
(`StorageMsg::SessionsGetByArtifactIds`, SC4 read lane — the `sessions`
table's PK is `artifact_id` and its `session_id` is JSONL-recovered).

**R0 single-kb opt-in default (2026-07-22).** R0 (above) is correct-by-
default for `scope=all` — federated search must not get polluted with
transcript noise across every corpus on the daemon — but is bad UX once a
user has directly scoped to the sessions kb itself: they'd have to know to
type `?category=memory-session` on every query. `KbConfig`'s per-kb
`KbSection` gained a field sibling to `memory_scope`,
`default_search_category: Option<String>` (`crates/kb-core/src/config.rs`,
`#[serde(default)]`, TOML `[kb.sessions] default_search_category =
"memory-session"`), threaded to the runtime `KbContext` exactly like
`memory_scope` is (`crates/kb-server/src/state.rs` / `lib.rs`) and exposed
read-only on `GET /api/kbs`'s `KbSummary.default_search_category`
(`routes/kbs.rs`) so the SPA can offer a "make searchable by default"
affordance and explain why a kb's transcripts already show up unprompted.
`routes/search.rs`'s single-kb `get` handler (the `scope=one` path, resolved
AFTER the `scope=="all"` branch has already returned to `federated_search`)
mutates the freshly-built `Filters` — `if params.category.is_none() { if let
Some(default_cat) = &ctx.default_search_category { filters.category =
Some(default_cat.clone()) } }` — so an absent `?category=` behaves exactly
as if the caller had typed the configured value (it also satisfies the R0
gate in `Filters::keep`, since `self.category` now equals
`MEMORY_SESSION_CATEGORY`). An EXPLICIT `category` param — any value,
including literally `memory-session` — always reaches `Filters::from_params`
unchanged and is never overwritten; the default fires ONLY on true absence.
**`scope=all` is completely unaffected**: `federated_search` builds its one
shared `Filters` from `Filters::from_params(params)` alone and never reads
`ctx.default_search_category` for any corpus in the fan-out, so R0 stays
default-exclude there regardless of whether a participating kb has the
field configured (`default_search_category_does_not_leak_into_scope_all`
pins this: a kb WITH the default set still has its memory-session rows
excluded from a `scope=all` query with no explicit `category`).

**W1 amendment (sessions-rethink, 2026-07-28) — ONE interpretation engine,
three presenters.** `crates/kb-core/src/sessions/view.rs` (grammar
`session-view/1`, const `VIEW_GRAMMAR`) is now the ONLY place a captured
transcript's JSONL gets INTERPRETED — requestId merge, `tool_use`↔
`tool_result` pairing, task-lifecycle threading, sidechain/side-lane
grouping, command/kb-command cards, wrapper-envelope classification
(`classify_user_text`, R14). Three presenters walk its `SessionView` IR and
add no interpretation of their own: `session_render.rs` (the HTML reader,
request-time; its own module doc states "the renderer no longer parses the
transcript itself"), `kb sessions read`
(`crates/kb-cli/src/commands/session_read.rs`), and
`GET /api/sessions/{sid}/view` (`routes/sessions.rs`). `TailBlocks`
(commits/subagents, already extracted from the envelope's tail blocks) is a
caller-supplied input, never re-parsed by the engine. `ViewCarry` +
`view_append`/`view_finish`/`view_bootstrap` are the SAME engine's
incremental constructor — `session_view(jsonl, …)` is defined as
`view_finish(view_append(ViewCarry::default(), jsonl))`, golden-pinned as an
equivalence (`view_full ≡ fold(view_append)` across chunk sizes) — built for
W7's live-tail panel (memo R15/LF-4), unconsumed by any shipped presenter
yet. Extending interpretation (a new card shape, a new wrapper kind) is a
`view.rs`-only change; a presenter that starts re-deriving interpretation
locally reintroduces the exact drift this wave closed.

**W1/M2 amendment — stable `t-<uuid12>` turn identity (moonshots M2, memo
R2).** A turn's id is `"t-"` + the first 12 hex chars (dashes stripped) of
its SEED record's own JSONL `uuid` (`turn_id_from_uuid`; a
content-independent hash fallback when no record in a merged group carries
one). This REPLACES the pre-W1 renderer's ordinal `id="turn-N"` scheme,
which reclaimed numbers whenever a card was skipped — so a milestone that
changes card-emission rules (this one does) would otherwise silently
renumber every existing permalink, jump-rail anchor, and comment anchor.
The break is deliberate and accepted (memo D5): a bookmarked pre-W1
`#turn-N` link no longer resolves to any element; `data-turn="<ordinal>"`
carries the old numeric position as DATA ONLY, never a live anchor. Every
nav surface targets `#t-<uuid12>` going forward: the jump rail, plan bands,
the outcome footer, `?turn=<N|end>` deep links (resolved via the outline,
which carries both `n` and `id`), M7's find-in-transcript hash target, and
turn-anchored comments (next amendment).

**W6 amendment (moonshots M2, memo R2) — memory-session comment-anchor
resolution, 2026-07-29.** #6's rule that `reanchor` is the only anchor
REWRITE is untouched; this is a RESOLUTION rule only. The indexer's
stale-anchor pass (`indexer::finish_indexed_doc`) normally re-resolves an
open comment's `review::Anchor` against the artifact's raw on-disk bytes
(`review::fuzzy_resolve_anchor_with`) — correct for an ordinary artifact,
but a `memory-session` capture's raw bytes are
`<h1>…</h1><pre>{escaped JSONL}</pre>` plus additive tail blocks: NEITHER a
`t-<uuid12>` id NOR the `#ses-outcome` footer NOR any rendered prose exists
anywhere in those bytes — the renderer mints all three at REQUEST time. A
`Section{id:"t-<uuid12>"}` or `Selection{snippet:"…rendered prose…"}`
anchor on a capture would therefore flap Stale on every reindex regardless
of whether its target was still there (the verified gap). The fix,
`sessions::view::resolve_capture_anchor`, is CATEGORY-GATED on
`kb-category==memory-session`: `Section` resolves against the live
`SessionView`'s turn ids plus the `SES_OUTCOME_ANCHOR` id; `Selection`
resolves against the DECODED transcript's prose/text content
(`Item::Prose`/`Thinking`/`Decision`/… — never the raw escaped characters),
reusing the generic resolver's Jaro-Winkler scoring primitives
(`review::{jaro_winkler, fuzzy_threshold, context_chars}`, widened to
`pub(crate)` for this reuse). `File`/`Chapter` anchors on a capture fall
through unchanged to the generic byte-level resolver (out of this fix's
scope — a capture's raw HTML has exactly one heading and no other
structure, so `Chapter` resolution there was already effectively dead;
`File` is `Exact("file")` everywhere regardless). This makes turn-anchored
review practical: the reader's existing `?cm=on` annotator already anchors
Section-scope on any `[id]` ancestor (a turn's `<article id="t-…">`
qualifies for free), and a `data-kb-turn-comment` "comment on this turn"
affordance (`session_render_runtime.js`, wired onto a per-turn permalink
`<a class="ses-turn__permalink" href="#t-…">` the CSS had carried since W1
but nothing linked until now) posts the SAME `cm:compose` message the
annotator's own click-to-compose path sends — no new anchor kind, no new
wire.

**W2 amendment (sessions-rethink, 2026-07-29) — V0029, the frozen
sessions-columns manifest, the digest closure, and the one-reindex
budget.** Migration `V0029__session_project_and_outcome.sql` adds NINE
columns to `sessions` in one additive migration (memo R5's frozen bundle;
REJECTED alternatives recorded in the migration itself: a persisted
`trivial` bool, subsumed by `substance`, and a persisted
`outcome`/`outcome_reason`, rejected as in-daemon judgment — see the
substance ruling below): `project_key`/`repo_root` (P1's derived-key
ladder, rungs 2+3 shipped — rung 1, a forward-capture tail block, is
deferred), `harness TEXT NOT NULL DEFAULT 'claude'` (see the harness-lane
amendment below), `cc_version`, `last_assistant_text` (the closure
quintuple: the last real non-wrapper non-synthetic assistant prose,
head-capped `LAST_ASSISTANT_TEXT_MAX_CHARS` = 700 chars — the wire preview
`outcome`, ≤`OUTCOME_WIRE_MAX_CHARS` = 240 chars, is derived server-side
from this column, never persisted separately), `all_cwds`, `commit_count`,
`user_turns` (the wrapper-skipping "real prompts" count), `active_secs`
(R6/D4: sum of per-event-delta gaps each clamped to
`ACTIVE_DELTA_CLAMP_SECS` = 300s — the SAME fn both the reader header and
the persisted column call, so they can never disagree), and `substance`
(below). Backfill is `kb reindex` — no bespoke backfill path; the SAME
`SessionCaptureHook` that populates a fresh capture repopulates every
pre-existing row. Riding the SAME reindex, memo D1-B's digest change:
`session_digest_excerpt` (the search/recollect CARD text) is RECOMPOSED as
`title · first-prompt · closed: <text>` (previously a blind 400-char prefix
of the full digest body, whose line order puts decisions/research/
files/project context BEFORE the closure — a card almost never reached
it), and the full `session_digest` body gains its own capped `closed:`
line (`DIGEST_CLOSED_LINE_MAX_CHARS` = 200, shorter than both the
persisted column and the wire preview — one line among several here, not
the headline). **The R3 one-reindex budget is SPENT**: V0029's backfill and
the digest re-embed rode the SAME `kb reindex --kb <sessions-kb>` (run
once, live, 2026-07-29) — any FURTHER change to `session_digest`/
`session_digest_excerpt` output costs a second reindex and must justify
it explicitly. (W6's SGR-remnant strip and the anchor-resolution fix in
this same wave do NOT touch the digest — the unchanged
`session_digest_snapshots` golden suite is the proof.)

**Substance, not judgment (memo R4).** `substance` is the ONLY persisted
triage value — `'trivial'|'routine'|'substantive'`, with `NULL` meaning
un-backfilled (every reader MUST treat `NULL` as `'substantive'`:
un-backfilled history is never hidden behind a husk filter). There is
deliberately NO persisted outcome/shipped/blocked/decided enum: display-time
badges (commit count, error count, decision count — all already on the row)
are DERIVED at read time, never stored, keeping outcome CLASSIFICATION out
of the daemon (the no-in-daemon-LLM non-goal; `/kb-distill` — and now
`/kb-weekly`'s narrative step — is where judgment/prose happens, at the
agent layer, never in-daemon).

**W4 amendment — the by-job join (memo R8/ADD-2).**
`V0030__session_research_grok_job.sql` widens `session_research.kind`'s
CHECK constraint (a table rebuild — SQLite can't ALTER a CHECK in place) to
admit `'grok_job'`, whose `query` is a grokclaude job ulid. TWO independent
writers emit the SAME `(kind='grok_job', query=<ulid>)` row shape, joined
at READ time rather than via a new table: the DRIVER side
(`extract_grok_job`, a parse-time regex over Bash `tool_use`/`tool_result`
pairs matching `grokclaude {research,session,build,panel,fleet}` plus a
recovered ulid) runs on every Claude Code session from day one; the CHILD
side (the grok capture itself, W5) emits its own row when THAT capture IS
the invoked job. `GET /api/sessions/by-job/{ulid}` (`kb sessions by-job`)
fans out `session_research_by_job` per kb (#28), resolves each hit's owning
session, and tags it `JobLinkRole::Driver`/`Child` — one ulid, two
capture-time-independent writers, one federated join. `grok_job` rows are
excluded from `session_digest`'s `researched:` line (a bare ulid is opaque,
not topical ranking signal) — a documented digest EXCLUSION, not a change,
so it did not consume the V0029 reindex budget above.

**W5 amendment — the harness lane (memo R5/R8, moonshots M5 Phase 1),
2026-07-29.** `sessions.harness` (V0029, `NOT NULL DEFAULT 'claude'`)
records which of the CLOSED set `kb_core::sessions::HARNESSES =
["claude","codex","opencode","grok","kimi"]` produced a capture — extraction
ladder: an adapter-meta JSONL first line (`harness:"codex"|"opencode"|
"grok"|"kimi"`) → `<meta name="kb-harness">` → `HARNESS_DEFAULT` (`'claude'`: a
transcript with no adapter fingerprint came from Claude Code itself;
un-backfilled history reads as honestly Claude, never a `NULL` hole).
Four sibling capture adapters translate a foreign harness's own transcript
format into the SAME Claude-Code-shaped JSONL-in-`<pre>` envelope every
other amendment here already governs
(`plugins/kb-memory/hooks/kb-capture-{codex,opencode,grok,kimi}.sh`) — `grok` is
a DIALECT label, not a driver name (a grok capture launched via the
grokclaude skill or any other route is still `harness:"grok"`; the
launching tool may additionally be recorded in `adapter-meta`).
*2026-08-12:* `kimi` joined the set — Kimi Code's hook payload has no
`transcript_path`, so `kb-capture-kimi.sh` derives the session's
`agents/main/wire.jsonl` from `cwd`+`session_id`
(`wd_<basename>_<sha256(cwd)[0:12]>`), and its wake/recall hooks emit plain
stdout (only `UserPromptSubmit` stdout reaches the model there).
`kb-capture-grok.sh` is the newest and most elaborate: PREFERS the same
Rust shared writer (`kb sessions capture --transcript … --session-id …`)
every adapter prefers (atomic writes, one-file-per-sid reuse, git-commit
resolution, `kb-decay: fast`), falls back to a hand-rolled bash/jq
translation when `kb` is unavailable, supports `--backfill` (walk a
grokclaude blackboard root, unconditionally skipping `GROKCLAUDE_FAKE`
jobs) and `--with-report` (D5: render `report.md` + `findings/*.json` as a
linked kb artifact IN THE SESSIONS CORPUS, beside the transcript it
summarizes — not a separate corpus), and layers the existing
`session_scrub` three-layer scrub (a Grok chat may inline read file
contents the same way a Claude transcript's tool results do).

**W5 amendment — workflow journals + task-output snapshots are now
CAPTURED, reversing the earlier exclusion (memo R10/ADD-3), 2026-07-29.**
Earlier text in this file (and `sessions.rs`'s `collect_subagent_digests`
doc comment) described `subagents/workflows/**` as "DELIBERATELY excluded
from the sidecar walk" — true ONLY for the STRUCTURED subagents digest
block (`extract_subagents_block`, which stays scoped to direct
`agent-*.jsonl` sidecars; a workflow-launched subagent has no such sibling
file, so that walk still can't see it). As of W5, a SEPARATE walk —
`collect_extra_sidecar_sources` — extends the EVIDENCE-only sidecar-text
tail block (`<section id="kb-session-sidecar-text">`, the SAME caps as
every other sidecar: 2 MiB/agent · 8 MiB total raw · head/tail truncate)
with two more sources: `subagents/workflows/<wf_id>/journal.jsonl` (a
workflow-launched subagent's ONLY durable evidence — verified live, it has
no sibling `agent-*.jsonl`) and any TaskOutput truncation-marker path still
on disk at capture time. Rationale: `/tmp` is ephemeral — capture time is
the ONLY time this data exists; without it, a workflow-heavy session's
actual work renders as shadows. Neither source feeds the STRUCTURED digest
(`subagent_count`/`subagent_tokens`/… stay `agent-*.jsonl`-only) —
evidence-only, matching the sidecar-text block's existing "raw evidence,
not the digest" contract. A real bug this same wave fixed alongside the
reversal: workflow-only sessions (no direct `agent-*.jsonl` at all) were
previously DROPPED from subagent aggregation entirely by an
`agents.is_empty()` early return — the journal walk now gives them a
non-empty sidecar set, so they're no longer invisible.

**Live-lane corollary (SHIPPED W7, 2026-07-29, memo R15/LF-7 — NO new
invariant slot).** The CAPTURE remains the record. Everything the live
lane shows — `GET /api/sessions/presence` (LF-1's Tier-1 stat-only readdir
probe over opt-in `[sessions] live_transcripts_dir`, `KbConfig::sessions`),
`GET /api/sessions/{sid}/live?from=&raw=` (LF-3b's incremental delta route,
`kb_core::sessions::tail::{TailReader, resolve_live_transcript}` +
`sessions::view::{view_bootstrap, view_append}`), and `kb sessions read
--live`/`--follow` (LF-6, direct-disk, zero daemon, the SAME
`resolve_live_transcript` resolver) — is DERIVED, EPHEMERAL, and NEVER
STORED: no new table, no new column, no lance write, no review file. The
server-side `LiveTailCache` (`crate::live_tail_cache`, an `(sid,
inode)`-keyed LRU of in-memory `ViewCarry` state) is a pure perf
optimization, never load-bearing — a cache miss always falls back to a
stateless 2 MiB-window bootstrap that is independently correct. No
daemon-side capture trigger exists (a recorded refusal; capture stays
hook/CLI-driven, never daemon-initiated) — the CLI hint for a crashed/
no-Stop session is `kb sessions capture --transcript <path>`, manual by
design. **Security posture is STRICTER than every other sessions route**:
both `/presence` and `/live` refuse non-loopback with 403 even given a
valid bearer token (`middleware::is_loopback_origin` checked inside each
handler, invariant #3/#4's fail-closed ethos extended — unlike `/raw`/
`/export`/`/view`, which force a redaction floor instead of refusing, a
live transcript is unscrubbed mid-flight content with no floor that makes
it safe to serve remotely); both set `Cache-Control: no-store`. D-LF1
(mount `~/.claude/projects` into the prod container?) resolved NO for v1 —
Tier-1 is host/dev-daemon-only; the remote/phone lane is Tier-0
(`sessionPresence.ts`'s capture-derived "active" status, needing zero
config) plus D2's throttled mid-run capture, both already live since W5.
The live IR can never structurally contradict the captured IR: `view_append`
and `session_view` are two constructors over the SAME grammar
(`sessions/view.rs`'s `view_full(doc) ≡ fold(view_append)` equivalence
golden), so a live turn's `t-<uuid12>` id is byte-identical to what the
eventual capture renders — the SPA follow-mode handoff
(`ArtifactPane.tsx`'s `session.captured` listener) reloads onto that exact
fragment with no jump.

**CT amendment (v0.38, 2026-08-21) — the memory-recall ledger parse has a
machine grammar; the derive pass census-counts its own gaps.** The
`memory_recalls` ledger derivation (`derive_memory_recalls`) no longer
depends solely on re-parsing kb-recall.sh's human-readable line: the hook
now appends a `kb-recall/1` machine marker
(`<!--kb-recall/1 kb=<kb-name> id=<hex12>[ pos=<n>]-->`) after each hit,
folded into the same `items[]` entry by `ingest_attachment`, and
`parse_recall_marker` PREFERS it — the free-text grammar
(`parse_recall_item_parts`, which also recovers the TITLE for CT-C5's
used-matching, stripping CT-C1's `⚠ disputed:` prefix) is the permanent
fallback for older captures. Both grammars are golden-pinned. *MR1
amendment (SL6, 2026-09):* the marker BODY is an unordered, whitespace-
separated bag of `key=value` pairs — `kb` and a 12-hex `id` are required,
`pos` (1–99, the hit's rank) is optional, and **unknown pairs are ignored,
so the grammar is forward-compatible**; a hook may add a pair without a
kb-core release landing first. The widening is not cosmetic: the pre-MR1
`split_once(" id=")` would have REJECTED layout v2's own `pos=` marker and
silently zeroed the ledger the day v2 shipped, with only
`DerivedRecalls.failed` as the tripwire. A malformed `id` is still the one
fatal defect (it is the row's identity); a malformed `pos` degrades to
`None` and never costs the row. The return type is `DerivedRecalls` — rows PLUS
a three-way parse census (`marker_parsed`/`fallback_parsed`/`failed`), so
the enrichment hook can `tracing::warn!` on "recalls happened but the parse
dropped them" (the exact string `kb doctor --hooks` greps for) instead of a
silent ledger gap. Additionally: `kb_cli_query` (the session research-row
extractor) now returns a `(kind, query)` pair with `ARTIFACT_OPEN_VERBS`
(`cat`/`get`) producing `artifact_open` rows (CT-A5 — these produced NO row
before, not a mis-kinded one); `extract_touched_ids` returns per-id
Exact/Fuzzy confidence tiers that ride the touches wire as labels (CT-A6 —
an Exact tier is never downgraded by a later Fuzzy hit on the same id); the
subagent aggregates + `via_subagent` chips render in the sessions SPA
(CT-A7; `all_cwds` is decided-dropped — it never reaches the wire).

**LSC amendment (2026-08-22) — the live-sessions cockpit: two independent
axes, push events never states, and a second, deliberately looser posture
for the derived-state lane.** Design:
[`docs/research/kb-live-sessions-cockpit-2026-08.html`](research/kb-live-sessions-cockpit-2026-08.html).
The design's own verdict (§10 "What this refuses to do") is explicit that
this is an amendment to invariant #11, not a new slot — the invariant
budget is capped at 35 and full (see the root `CLAUDE.md`'s "Invariant
budget" note), and the W7 live-follow round above set the precedent of
amending #11 rather than adding #36. Four phases landed it: LSC-1
(`kb-core/src/sessions/live.rs` — the pure two-axis model + the Claude Code
transcript adapter, `kb sessions status --local`), LSC-3
(`plugins/kb-memory/hooks/kb-beat.sh` — the push side), LSC-2
(`kb-server/src/live_registry.rs` + `routes/sessions.rs`'s `beat`/
`live_status` — the daemon side), LSC-5 (`kb-core/src/sessions/
live_adapters/{codex,grok,kimi,opencode}.rs` — the four sibling adapters +
`scan_all`, plus the abandon horizon below).

- **The model is two INDEPENDENT axes, not three buckets** (design §3):
  *who holds the ball* (`Holder::Agent | Human | Ended`, from turn-boundary
  evidence) and *how long since it moved* (silence, in seconds). Silence
  NEVER flips the holder axis — a 20-minute `cargo build` appends nothing
  to a transcript, but the agent still owns the turn; treating silence as
  "waiting on you" is the exact failure a retired internal cockpit prototype
  shipped (mined as prior art, design §2). Silence only ESCALATES within
  an axis: `Holder::Agent` walks `working` → `stalled` (45 min,
  `STALL_AFTER_SECS`) → `presumed_ended` (8h, `ABANDON_AFTER_SECS`,
  LSC-5 amendment below); `Holder::Human` walks `waiting` → `cold` (8h,
  `COLD_AFTER_SECS`). `Holder::Ended` is always `finished`, regardless of
  silence. All three thresholds live in `kb_core::sessions::constants` and
  are threaded through as a `LivePolicy` struct (never read as ambient
  constants inside `derive_state`, so the function stays independently
  testable). `derive_state` is the ONE place any of this is decided — both
  `kb sessions status --local`'s adapters and the daemon's
  `live_registry`/`routes::sessions::beat`/`live_status` call it, never
  reimplement it.
- **A beat is a PUSH EVENT, never a state** (design §5, `kb-beat.sh`'s own
  "THE DISCIPLINE" header comment). The wire vocabulary is closed:
  `start|prompt|tool|turn_end|blocked|unblocked|end`. `POST
  /api/sessions/beat` (`routes::sessions::map_event`) does the ONLY
  interpretation the daemon performs on intake — event → `(Holder, blocked:
  bool)` — and then defers entirely to `derive_state`; blurring that split
  (a hook asserting "I am waiting") defeats the whole design, so
  `kb-beat.sh` is structurally unable to emit one. `blocked` rides as a
  flag on `Holder::Agent` (not a new `LiveState` variant, per the
  design's own instruction) so a presenter can render a distinct
  "waiting on a permission prompt" sub-label without growing the state
  enum.
- **The abandon horizon (LSC-5) — silence has a horizon even for the
  "silence is not evidence" rule.** The first five-harness run pinned five
  grok sessions whose last record was `turn_started` six days earlier to
  the top of IN PROGRESS — no tool call runs for six days, so "the agent
  still owns the turn" had stopped being an honest reading. Past
  `ABANDON_AFTER_SECS` (8h, same horizon as `COLD_AFTER_SECS` — "past a
  working day, an unclosed turn is a corpse, not a build"), an
  agent-held session reports `presumed_ended` rather than `stalled`. This
  is DISTINCT from `finished`: `finished` requires an observed `end` event
  (`Holder::Ended`, an explicit SessionEnd/equivalent signal); `presumed_ended`
  is an INFERENCE from a missing close, and the two states must never
  render as the same claim to an operator — one is a fact, the other a
  guess about an abandoned process.
- **Storage is NOTHING — the registry is in-memory only, LF-7 held.**
  `kb-server::live_registry::LiveRegistry` is a process-local `HashMap`
  behind a `std::sync::Mutex` (no `.await` inside the critical section,
  invariant #15); a daemon restart empties it outright, by design — no
  sqlite table, no migration, no lance write, keeping this clear of the
  in-flight v0.38 milestone's schema-epoch claims. Bounded two ways so a
  fleet running for months never leaks: `REGISTRY_CAP` (4096 sessions,
  LRU-by-touch eviction) and `REGISTRY_TTL_SECS` (7 days, swept on every
  write). The mitigation for the restart-loses-state cost is `GET
  /api/sessions/live-status`'s Tier-0 layer: rows rebuilt from landed,
  durable captures that have no registry entry, newest-capture-scoped
  (#11's multi-capture rule) and fanned out over `state.kbs` via
  `buffered_join` (#28) — always at `source:"capture"`,
  `confidence:"presumed"`, aged since the capture's own `ended_at`/
  `started_at`. A registry entry for a `session_id` always WINS over a
  Tier-0 row for the same id (never merged/enriched) — a freshly-restarted
  daemon is honest-but-coarse rather than empty, and the moment a beat
  lands for that session the row sharpens back to `source:"hook"`,
  `confidence:"observed"`. This is the SAME tie-break invariant #11's W7
  amendment already established for the live transcript lane ("if the
  live view and a landed capture ever disagree, the capture wins by
  definition") — nothing here overrides it, since the registry only ever
  fills the gap where no capture exists yet.
- **The posture graduation is deliberate and narrow, not a loosening of
  #4/#11's live-lane fail-closed ethos.** `POST /api/sessions/beat` and
  `GET /api/sessions/live-status` ride ordinary `auth_bearer` — they are
  the ONE part of the sessions live-lane family that is NOT
  loopback-only, unlike `/presence` and `/{session_id}/live` (both
  untouched, still loopback-only HARD per the W7 amendment above). This
  is exactly the graduation the W7 text already wrote down as available:
  "if D-LF1 ever mounts live transcripts into prod, `/live` graduates to
  the export-route posture (token + forced scrub)." The live-sessions
  cockpit takes that documented path for the STATE lane only — "session
  X, harness claude, project kb, human has held the ball for 49 minutes"
  is metadata of the same kind kb already serves under `auth_bearer`
  elsewhere (e.g. `last_assistant_text`), not raw unscrubbed transcript
  bytes. The forced-scrub floor is real, not aspirational:
  `routes::sessions::redact_for_non_loopback` collapses `cwd` to its
  basename and re-caps `last_line` (`OUTCOME_WIRE_MAX_CHARS`) for any
  non-loopback caller, applied AFTER ordering/capping so it never skips a
  row. `ConnectInfo` is read as an axum extractor on `live_status` (the
  house pattern for a route HANDLER; invariant #3's "read from
  extensions" rule targets generic Request-taking middleware, not this).
- **`session.state` fires ONLY on a derived-state CHANGE, per invariant
  #24.** `routes::sessions::beat` compares `RecordOutcome::previous_state`
  (the state `derive_state` would have reported for this session
  immediately before this beat, against the SAME `now_unix`) to the
  freshly-derived state and emits iff they differ — never once per beat,
  or a busy fleet would flood every browser. Registered in
  `routes/schema.rs`'s `V0_0_1_TYPES` + `per_type` in the same commit, as
  the drift test requires; payload is `{session_id, harness, state,
  holder, project, since_unix}` — deliberately NOT `title`/`last_line`
  (content leaks in only through the request/response bodies above, never
  the SSE broadcast).
- **Push wiring is uneven across harnesses by design, and `--local` and
  daemon mode see different subsets as a result.** LSC-3 wires
  `kb-beat.sh` into Claude Code's full lifecycle (`SessionStart`/
  `UserPromptSubmit`/`Stop`/`SessionEnd`/`Notification` + a throttled
  `PostToolUse` heartbeat), codex's `UserPromptSubmit`/`Stop` only (no
  verified `SessionEnd`/`Notification`-equivalent hook exists yet), and
  Kimi Code's `UserPromptSubmit`/`Stop`/`SessionEnd`. opencode's beat
  wiring is a documented TS snippet for the operator-managed
  `~/.config/opencode/plugin/kb-memory.ts` (outside this repo, not
  auto-applied). Grok has NO push wiring in this phase — `kb-beat.sh`'s
  own harness dispatch calls it out as "best-effort — no live-verified
  push-hook payload shape exists yet; dormant". A harness with no beats
  wired still surfaces in the daemon-backed default `kb sessions status`
  as a Tier-0 `presumed` row once it has landed at least one capture (all
  five harnesses capture normally) — just coarser and only as fresh as
  its last capture, never at `confidence:"observed"`. `kb sessions status
  --local` (LSC-5's `live_adapters::scan_all`) is unaffected by any of
  this: it reads each harness's own on-disk event file directly
  (`~/.claude/projects`, `~/.codex/sessions`, `~/.grok/sessions`,
  `~/.kimi-code`, `~/.local/share/opencode/opencode.db`), independent of
  whether a push hook is wired for it.

### 12. Meta edits rewrite the SOURCE

The `PATCH /api/kb/{kb}/artifacts/{id}/meta` route edits tags/category by
rewriting the source file — HTML via `kb_core::meta_edit::set_meta_content`
(byte-preserving `<meta>` splice), Markdown via `markdown::set_frontmatter_field`
— then the watcher re-indexes (write-only path, new content hash dodges the dedup
gate). Lance stays the single source of truth, so the edit shows in every read
path for free. Two load-bearing constraints: (a) the HTML rewriter **skips
`<template>`/`<script>`/`<style>`/comment regions** and never scans inside
another tag's attribute values — a naive "first `<meta name=kb-tags>`" match
would corrupt `<template id="kb-prompt">` (#5); dedicated
`kb_prompt_template_untouched` test; (b) **strictly scoped to `kb-tags` +
`kb-category`** — never the memory metas
(`kb-salience`/`kb-decay`/`kb-global`/`kb-linked-kbs`; #10). Clearing all tags
reverts to path-derived; the response echoes the effective set.

### 13. Web config edits = persist-to-loaded-file + in-process restart

`PUT /api/config` rewrites the SAME file the daemon loaded (explicit `--config`
or the home-local default, threaded into `KbHandles.config_path`) and trips an
in-process restart so every field applies. Four load-bearing shapes:

- **`serve_loop` owns the restart, not `exec`.** `serve_with_paths` is
  single-shot, returning `ServeOutcome::{Restart, Shutdown}` after teardown; the
  loop re-reads + rebuilds on `Restart`, and on a failed boot **rolls back to the
  last-known-good config** (kept in-memory) so a bad edit can't strand the
  daemon. Pid file + state dir stay valid (no re-fork); `paths` are pinned, so
  `PUT` rejects a changed `[daemon] name` and **test-binds a changed `[server]
  addr`** before persisting (400 before disk).
- **Clean teardown is mandatory for reload, unlike OS-signal exit.** The metrics
  ticker + reconcile loop are infinite `interval` loops and the indexer holds an
  `EventBus` sender clone (a reference cycle) — an in-process reload would leak
  them + spawn duplicate kb-embedder subprocesses. So every spawned task watches
  the `shutdown` watch (`tokio::select!`); `bring_up_kb` returns its
  `JoinHandle`s; `serve_with_paths` joins them (+ aborts startup-compact) before
  dropping `KbHandles`. `shutdown_signal` completes on the watch too so a
  `PUT`/`/api/shutdown` drains promptly.
- **Bind the listener BEFORE spawning per-kb tasks.** A failed rebind must return
  `Err` with nothing spawned (else partial tasks leak, since teardown only runs
  after the server future). Don't reorder.
- **`save_preserving` is a minimal diff, not a whole-file re-render.** It splices
  only changed fields via `toml_edit`, so comments + key order survive and
  omitted defaults aren't materialised. `KbConfig::save` (whole re-render) stays
  for `kb add`. Secrets are never in config — the `[share]` editor edits env-var
  *names*.

### 14. Versions/Diff — three sources behind one façade; the git path base is load-bearing

`[kb.*] versions` (`auto`|`git`|`index`|`both`|`off`, default `auto`) selects
where `kb_core::versions::VersionCtx` reads revisions:

- **git** — `vcs::find_git_root` walked up from the corpus dir (**may be an
  ancestor** — a corpus is often a subdir of a larger repo), then `vcs::git_log
  --follow` / `vcs::git_show`. Git ops take the path **relative to the git root**
  (`abs.strip_prefix(git_root)`), NOT the corpus-relative `doc_rel_path` — mixing
  the two bases is the most likely bug. Shell-out to `git` (no `git2`/`gix`);
  absent/non-repo/untracked → empty history, **never a 500** (deployed container
  `.dockerignore`s `.git`).
- **index** — `artifact_snapshots`, appended by the `snapshot-capture`
  `EnrichmentHook` ONLY when `versions_mode.uses_index()` and the artifact isn't a
  `memory-session` (storing 25 copies of a transcript is the growth risk), pruned
  to `DEFAULT_SNAPSHOT_KEEP` (25). Stores raw source; prose re-extracted at diff
  time.
- **working tree** — the live on-disk file, always the newest timeline entry.

`auto` is **per-file**: git when the file has commits, else snapshots. The text
diff reduces EVERY source through `parser::text_blocks` (block prose, markdown
rendered first) so cross-source diffs are apples-to-apples (a markup-only change →
empty prose diff); `mode=raw` diffs source bytes. `git_root` + `versions_mode`
resolved once at bring-up, memoised on `KbContext`.

**Temporal resolution over the façade is ONE pure function, and it is
PER-ARTIFACT (CT-F6, v0.38).** `kb_core::versions::resolve_as_of` (MI-W2.4b) is
the only place a date/instant becomes a version ref; `resolve_memento` wraps it
without re-deciding anything, adding only honest metadata (`exact`,
`oldest_ts_unix`, `miss_note`) — so `kb diff --between`, `GET
…/versions?at=<unix>`, `kb versions --at`, and the SPA's `?at=` reader can never
disagree about which version a moment names. Three rules hold on every surface:

1. **The answer is the NEAREST version at or before the instant, and says so.**
   `relation` is always `"nearest-prior"`; `exact` is true only on a
   same-second hit. An approximate answer is never rendered as an exact one.
2. **An instant older than the oldest known version is a MISS** that names the
   floor (`found: false` + `note`) — never a silent degrade to the oldest
   version, which would claim the artifact stood that way before it existed.
3. **It is per-artifact resolution, not a timeline and not a ranking.** It
   resolves one artifact's own revision list. It is NOT the rejected `recall
   --as-of` (invariant #10: pin state / salience / decay policy are read as
   CURRENT values only, so an as-of RANKING would chimera history against
   present-day metadata) and it touches no scoring path at all.

`?at=` is additive on `GET …/versions`: without it the route returns the
`VersionsResponse` it always did, from the same code path (`VersionsAtResponse`
is a separate struct rather than an `Option` field, so additivity is structural,
not a `skip_serializing_if` convention) — pinned by
`at_body_is_the_plain_body_plus_a_trailing_memento_key`. The SPA resolves the
memento CLIENT-side from the timeline already in the `["versions", kb, id]`
query cache (`web/src/lib/memento.ts`, goldens mirrored case-for-case against
`kb_core::versions`'s) rather than issuing a second fetch — keep the two in
lock-step, the same way #29's wikilink grammar is kept.

### 15. Gallery row-set memo keys on a storage-actor index generation

`GET /docs` shares ONE `list_docs(u32::MAX)` scan + `edge_counts()` GROUP BY per
`(kb, generation)` via a single-slot `KbContext.gallery_cache`
(`Arc<Mutex<Option<GalleryCache>>>`). The generation is an `Arc<AtomicU64>`
shared between `StorageActor` and every `StorageHandle`; the actor bumps it
(`bump_generation`) inside `handle()` on **exactly** the mutations that change
the non-atlas `list_docs` projection or the sqlite `edges` table — `UpsertDoc`,
`DeleteDoc`, `DeleteByPath`, `RecordEdges`, `DropKbData` (the last bumps the
instant lance is emptied, *before* the sqlite purge, so a torn drop can't strand
the cache). It must NOT bump on `UpdateAtlas` / `ClearEmbeddings` / `CompactAll`.
**Add a new lance-docs or edges mutation arm ⇒ decide its bump and update
`index_generation_bumps_only_on_row_set_and_edge_mutations`** (the test pinning
the set). `index_generation()` is a free atomic load; bump is `Release`, read
`Acquire` (read-your-writes). The cache holds `Arc<Vec<DocRow>>` +
`Arc<edge_counts>`; the handler filters/sorts over **borrowed refs** and clones
only the ≤limit page. The `std::sync::Mutex` guard is **never held across
`.await`**. A hit requires `stored.generation == index_generation()` (strict
`==`) so a mid-scan bump only forces a future miss, never a stale serve.
Atlas-projection requests bypass the cache.

### 16. Notes are Markdown artifacts, not a new entity

A "note" (free-standing note / todo-list attached to a kb or folder) is an
ordinary **Markdown** artifact carrying `kb-category: note`, which buys search,
comments, versions, and folder-grouping for free; the only new capability is
**editing a note's body** (`routes/notes.rs`). **Note identity is
`kb_core::notes::is_note(path, kb_category)` = `kb-category: note` AND Markdown —
the markdown gate is load-bearing**, because `note` also predates this feature as
a *content* category on HTML write-ups; a category-only test would hijack those
out of the gallery into a broken native render. EVERY "is this a note?" site
agrees on `is_note`: `lance::list_notes`, the route `note_row` guard, the
gallery/folders exclusion, and the SPA `detail.tsx` `isNote`. Load-bearing
shapes:

- **Scope = file location** (`paths::doc_folder`; `""` = kb root). The canonical
  *notepad* is a deterministic `_notepad.md` per scope (idempotent create — never
  clobbered); ad-hoc notes are `note-<slug>-<unix>[-n].md`. Status rides
  `kb-status`, tags ride `kb-tags`. Pure path/compose/checklist logic lives in
  `kb_core::notes` (LLM-free).
- **The toggle index IS the document-order ordinal of GFM task items.**
  `notes::scan_tasks` parses with comrak's AST (the SAME parse
  `markdown::render_fragment` uses) and flips the symbol byte at each
  `NodeValue::TaskItem`'s `symbol_sourcepos`. So `POST …/toggle {index}`, the
  `task_done`/`task_total` lance columns, and the SPA's nth rendered checkbox
  address the same task for EVERY GFM form (bullet, ordered, blockquoted, nested)
  — don't regress to a line scanner (only handled bullets, silently diverged).
  `parser::tests::task_counts_match_source_scan` pins comrak-count == scanner.
- **Whole-file rewrite is safe** (vs #12's byte-preserving rule): notes are
  system-authored Markdown with no `<template id="kb-prompt">`. A note write emits
  a `watch.modify` nudge so the indexer picks it up even in a not-yet-watched
  subfolder; the content-hash dedup gate makes a duplicate event a no-op.
- **Notes are excluded from the gallery grid + atlas + `/folders` counts**
  (`DocsQuery::exclude_notes`, a markdown-aware `is_note` gate — NOT a flat
  `exclude_categories = ["note"]`, which would also hide HTML merely tagged
  `note`) but **stay searchable** (search bypasses `docs_query::matches`) and
  resolvable by id/path. The SPA renders a note NATIVELY in Detail (interactive
  checkboxes + inline editor, same-origin JSON); `rawView` falls back to the
  served-HTML iframe.

### 17. Indexer ingest is a per-kb back-pressured mpsc, NOT the shared bus

The indexer (`run_with_ingest`) drains a dedicated `tokio::mpsc<WatchWork>` per
kb — it NEVER reads `watch.*` off the `EventBus`. Every producer (live watcher,
reconciler walk + delete pass, operator reindex, quarantine/note nudges) pushes
through `kb_core::indexer::IngestSink`, which (a) `send`/`blocking_send`s onto the
bounded channel — **back-pressure**: a burst larger than the queue BLOCKS the
producer instead of dropping — and (b) **mirrors** the event to the bus as a
`watch.*` frame so the SPA event log / `kb events --follow` / webhook
subscribers are unchanged. Consequences:

- **The indexer can't lag on unrelated bus traffic** — it reads only its channel.
  `recv()` returning `None` = all senders dropped = shutdown.
- **`blocking_send` is illegal on a tokio worker.** The watcher's initial walk +
  the `walk_send_work` reconcile/reindex walks run on a std::thread /
  `spawn_blocking` for this reason. Don't call `IngestSink::blocking_send` from
  async code; use `send().await`.
- Add a new producer ⇒ push through the kb's `IngestSink` (`KbContext.ingest`),
  never a bare `bus.emit("watch.*")` (the indexer won't see it). `run`
  (broadcast-receiver signature) is a back-compat TEST shim bridging
  `broadcast::Receiver` into the mpsc; production calls `run_with_ingest`
  directly. The mtime dedup lives in the shared `walk_core` so both walk flavours
  skip unchanged files identically.

**GC-B7 — batched storage commits.** The channel contract above is unchanged,
but the consumer now batches: after the blocking `rx.recv().await`, the indexer
opportunistically drains (`try_recv`, never blocking — an empty channel just
ends the batch, so a live-watcher trickle still indexes one file at a time with
no added latency) further same-kind (Created/Modified) items into a batch
capped at `INGEST_BATCH_MAX_DOCS = 32` total, prepares each (`prepare_doc`:
read → parse → embed),
and commits the group through ONE `upsert_docs` `merge_insert` per flush
(`flush_prepared_batch`) — a flush fires early when the accumulated
body+html+raw bytes would exceed `INGEST_BATCH_MAX_BYTES` (8 MB). A failed
batch commit falls back to per-doc `upsert_doc` retries, so one bad doc (e.g.
a wrong-dim embedding) can't sink the rest of the batch. A `Deleted` item ends
the drain and runs AFTER the batch it was drained alongside — recv FIFO order
is preserved, so a create-then-delete-same-path pair still deletes last.

### 18. Comment attachments — manifest under `review_lock`, sniff-gated upload, XSS-safe serve

A file/image attached to a comment/reply. Blob bytes live at
`<state>/<kb>/.attachments/<artifact_id>/<aid>`; a sibling per-artifact
`_manifest.json` holds metadata + an `adopted` flag; the review JSON carries a
denormalized `attachments: Vec<Attachment>` on `Comment`/`Reply`.

- **The manifest is guarded by the SAME per-kb `review_lock`** (extends #6):
  every stage / adopt / detach / GC runs load→mutate→save under that guard (the
  stage endpoint takes the lock too).
- **Lifecycle = stage → adopt → GC.** The SPA stages an upload *before* the
  comment exists (to drop an inline `![](attachment:<aid>)` ref into the draft),
  then `addComment`/`addReply` adopt the staged ids via `attachment_ids`; a
  one-shot `POST …/comments/{cid}/attachments` (+ reply / + detach `DELETE`)
  uploads+adopts to an existing target.
- **GC is reference-counted** (`attachments::gc_plan`): a *referenced* blob is
  ALWAYS kept (robust to the save-review-then-save-manifest crash window); an
  `adopted` blob with no live reference is reaped immediately; a never-adopted
  *staged* blob only after a 24 h grace.
- **Upload gate = the daemon's magic-byte sniff** (`attachments::sniff_allowed`,
  NEVER the client's content-type) against a fixed allowlist
  (png/jpeg/gif/webp/pdf/utf-8 text). **Serve XSS guard**
  (`routes::attachments::serve`): `Content-Type` = sniffed type,
  `X-Content-Type-Options: nosniff` ALWAYS, `Content-Disposition: inline` ONLY for
  raster images — every other type is force-downloaded so an HTML/SVG/JS payload
  can't execute. Adding an inline-served type ⇒ re-audit the rule + its
  `attachment_text_is_served_as_forced_download` test.
- **Static-share comment publishing is opt-in** (`kb share --with-comments` →
  `ShareOpts.include_comments`): renders each body with
  `markdown::render_comment_fragment` (`render.unsafe = false` — raw HTML
  stripped), rewrites `attachment:<aid>` refs to relative paths, copies blobs. It
  PUBLISHES private review state — SPA/CLI warn first.

### 19. Reading-progress follows the history lifecycle + is cumulative-idempotent

`reading_sections` is a child of the history `open` row via `visit_id … ON DELETE
CASCADE` — it shares history's append-except-upsert lifecycle. Deletion truth
(since the R2 systematic cascade, 2026-07): a TRUE artifact unlink runs
`cascade_delete_doc` in `CascadeMode::Full`, which DOES delete the artifact's
`history` + `reading_sections` rows (they are the two `keep_user_data_prunes:
false` steps in `CASCADE_STEPS`); only an EXCLUSION-shaped delete — file still
on disk but unmapped, `send_unmapped_delete` → `CascadeMode::KeepUserData` —
preserves them (alongside the `.review` sidecar) so re-widening the extension
map restores progress. `artifact_id` remains a denormalized query key, not a
lifecycle key. Rows also vanish on visit purge (`history_purge` /
`purge_kb_data` delete them explicitly, before the parent). The iframe runtime
(`runtime_js`) holds CUMULATIVE per-visit dwell / enters / active_ms and the
server UPSERTs with `max()`, so a resent beacon or remount-restore can't
double-count or regress — kept correct across the 30-min resume by
**seed-on-open** (`POST …/history/open` echoes prior reading state in
`OpenResponse.reading`; the runtime restores once, guarded by `rSeeded`). The
reading endpoint emits **NO SSE** (mirrors scroll); the `Reading*` storage arms
do **NOT** `bump_generation` (sqlite side-channel, like all `History*` arms —
#15). `section_id` is the runtime's **live DOM heading id** (`kb-h-<slug>` from
`buildToc`) on BOTH capture and SPA-heatmap sides — join by that id; never
re-derive server-side. Recall enrichment (`read_pct` / `last_read_at` /
`stopped_at`) + session `/readings` are **best-effort + uncached**. Capture is
gated per-kb by `[kb.*] reading_progress` (default on) — disabled ⇒ `post_reading`
no-ops 204. **touched ≠ read**: `/touches` is what the agent referenced in a
transcript; a reading is what the human scrolled. The classifier separates read /
skim / unseen by dwell vs the section's expected reading time (words ÷ ~200 wpm),
NOT raw scroll.

### 20. Iframe back-button = `location.replace`, not push

A bubble-phase click interceptor in `runtime_js` converts eligible same-origin
`<a>` clicks inside an artifact iframe to a same-frame `location.replace` (the
sandbox lacks `allow-top-navigation`). The trampoline → `open-artifact` → parent
push (`detail.tsx`) then yields exactly one clean history entry per artifact (the
iframe is re-keyed per id) — a real PUSH would pollute the joint session history
so Back walks the iframe's internal stack. Hash links scroll natively; modified /
`_blank` / cross-origin clicks untouched. Bubble phase (not capture) so an
artifact's own handler can preventDefault first.

*Linkflow amendment (2026-08-07):* the interceptor gained a SECOND, mutually
exclusive branch — an artifact-shaped **cross-origin** link (a sibling
`<id2>.artifacts.<suffix>` subdomain, or an `/a/<kb>/<rel>` SPA permalink on the
own origin / the configured parent origin) is `preventDefault()`ed and relayed to
the parent as `kb:link-open {href}` instead of being left to navigate the iframe
natively (which silently desynced every piece of SPA chrome from the visible
content). The parent resolves the href purely (`web/src/lib/artifactLinks.ts`,
golden-pinned; id-only forms go through the `api/artifactLookup.ts` kb ladder)
and performs the SAME single router push — one history entry per artifact holds
on both branches, and no click can fire both (the same-origin relative path
still takes replace+trampoline, pinned by `spa-backbutton.spec.ts`). The
subdomain test derives the host remainder from `location.host` at runtime — #7,
never a hardcoded suffix. Both navigating branches post a synchronous `kb:scroll`
snapshot BEFORE handing the page over, and `pagehide` flushes one final snapshot
(mirroring the reading tracker's), so the recorded offset is the true
leave-point, not the 500ms-debounced one. The trampoline now recovers the
client-side-only `#fragment` from `location.hash` and forwards it as the
OPTIONAL `sec` field on `open-artifact` (absent ⇒ payload byte-identical), which
the parent decodes onto the pre-existing `?sec=` deep link. The runtime also
relays hover — `kb:link-hover {href, rect, text}` / `kb:link-clear` — for the
parent-side peek card; all three new messages are consumed under the pane's
exact `isOriginOfArtifact` gate (#8/#19). The reading-flow return (the ContextBar
chip / `u` / browser Back) rides a per-tab sessionStorage stack
(`web/src/lib/flowStack.ts`, `kb:flow:v1` — no URL params, #35) whose popped
scroll offset enters the pane's kb-probe seed ladder BETWEEN the `?sec=`/`?turn=`
deep links and the server's resume value.

### 21. Daemon binds an IPv6 loopback companion alongside IPv4 loopback

`serve_with_paths` binds `[server] addr` (default `127.0.0.1:4000`) and, when
that primary bind is IPv4 loopback, ALSO a best-effort `[::1]:<port>` companion
serving the SAME router (fresh `shutdown_signal` each, joined via `try_join!` so
both drain on the shared shutdown watch). Why: where `/etc/hosts` / gai.conf
resolve `localhost` (and `*.artifacts.localhost` iframes) to `::1` first, an
IPv4-only bind leaves the browser hitting a dead `::1:<port>` and falling back to
a STALE cached page ("the app won't update"), though `curl` works via
happy-eyeballs. **Loopback-only** — NOT a `[::]` dual-stack bind, which would
expose private kb data to the LAN — and **best-effort**: an IPv6-disabled host
(or TIME_WAIT clash) keeps just the IPv4 listener (the `bind_with_retry` Err is
logged, not fatal). Bound up-front before any task spawn (shares #13's
bind-before-spawn guarantee); skipped when the primary bind is already IPv6 or
non-loopback.

### 22. The markdown composer is CodeMirror 6 with a hidden mirror `<textarea>` seam (ME-track)

Every comment/reply/note authoring surface shares
`web/src/components/MarkdownEditor.tsx`, which wraps a CM6 editor
(`web/src/editor/*` — syntax highlight + inline live-preview decorations + slash
menu + ⌘-shortcuts) and renders an **always-mounted, visually-hidden `<textarea>`
mirror** (`CodeMirrorInput.tsx`). CM6 is a `contenteditable` with no `.value`,
but the e2e suite drives composers via `getByRole("textbox",{name})` + `.fill()`
+ `.toHaveValue()` and the `.cp__*-input` classes — so the mirror carries the
**per-surface `ariaLabel` + `textareaClassName` + the controlled
`value`/`onChange`**, while CM6's content gets a GENERIC `aria-label` ("Markdown
editor") so a name query resolves to exactly one element. Both directions flow
through the single `value` state (human edits CM6 → updateListener → onChange →
value → mirror; Playwright fills the mirror → onChange → value → CM6
external-sync). Load-bearing consequences: (a) the mirror must stay
**layout-present** (1px/opacity-0/`tabindex=-1`, NOT `display:none`) so Playwright
sees it + `getByRole` finds the named ones — `styles/editor.css` is imported LAST
so `.cme__mirror` overrides the legacy `.cp__*-input` sizing; (b) keep the
`.cp__editor` wrapper + the `Write`/`Preview` tab roles + the
`.cp__editor-preview` pane (the third tab is `Split`); (c) the controlled bridge
(`useCodeMirror.ts`) creates the view ONCE per `configKey` and pushes external
value changes **only when `value !== doc`** (else cursor-jump on every
keystroke); (d) CM6 must never reach the `annotate` rollup input (10 KiB CI
guard) — don't import `editor/*` from `annotate.ts`; (e) `attachment:` paste/drop
still routes through the existing container `dndProps` → `insertAtCursor`
(unchanged); the toolbar 📎 + slash `/attach` open the same `ComposerAttachBar`
picker via its `open()` handle.

### 23. SPA server state lives in the TanStack Query cache; SSE invalidates it (TQ-track)

Fetch-shaped server state in the SPA rides ONE TanStack Query cache
(`web/src/api/queryClient.ts`), keyed by the documented contract — `["docs",
kb, …]`, `["doc", kb, relPath]`, `["review", kb, artifactId]`, `["notes"]`,
`["lists"]`, `["list", kb, listId]`, `["anchors"]`, `["memories", …]`, `["sessions"]`,
`["readingProgress", kb]`. The cache philosophy is **SSE-driven**: `staleTime:
Infinity`, no focus/reconnect refetching, retry once on 5xx only — data is
fresh until an event says otherwise. One **bridge**
(`startSseInvalidationBridge`) maps daemon events onto prefix invalidations
(`artifact.*` is burst-gated 1500 ms per kb; a `gap` frame invalidates the
world), so hooks carry NO fetch/abort/subscribe plumbing of their own.
Optimistic mutations patch the cache via `setQueryData` and
invalidate-to-resync on failure (useReview), or roll back from a
`getQueryData` snapshot (useAnchors).

Deliberate exceptions fall into TWO structurally distinct categories, and the
distinction is the point — conflating them is what let the count drift.

**(a) Own bespoke SSE wiring.** These hooks ARE tied to the event stream; they
just subscribe themselves instead of going through the bridge, because their
invalidation key is finer-grained than the bridge's per-kb gates can express:
`useSessions` (per-session rows arrive mid-capture), the open `useNote`
(keystroke-level echo suppression), and `useStaleAnchors` (per-artifact anchor
drift). A new hook joins this category only with a concrete reason the bridge's
gate cannot serve it — the bridge remains the default.

**(b) No SSE tie at all.** These have no event to listen to, so they carry a
FINITE `staleTime` and an explicit manual-refresh affordance instead of
`staleTime: Infinity` + bridge invalidation: `["daycard", kb]` (a clock-derived
window, already annotated as such in `queryClient.ts`), `useLiveTail` and
`sessions/presence` (derived-ephemeral live lanes, invariant #11's live-follow
amendment — the capture is the record, the live view is a poll), and — DCB v1 —
the doclens queries `["doclens", kb, docId, repo]` / `["doclensScorecard", kb,
docId]`. doclens is the clearest case in the category: kb-code is a SEPARATE
DAEMON on a different origin with no connection to kb's `sse` facade, so there
is no "own wiring" to speak of, only its absence. It carries `staleTime` 30s
(it reads a live git worktree, which changes with no notification anyone could
forward), `retry: false`, and a manual `↻` beside the rendered `resolved_unix`
so the reader can always see how old the resolution is and re-run it.

Neither category is a licence to skip the bridge: a same-origin, same-daemon
query whose invalidation the bridge CAN express belongs on the bridge with
`staleTime: Infinity`, full stop.

Don't add per-hook
subscriptions for fetch-shaped state (the bridge owns invalidation; adding
one re-introduces the double-refetch class), don't lower `staleTime` (it
reintroduces polling the event system exists to avoid), and don't return
`Map`s from `queryFn` (structural sharing can't compare them — return rows
and derive in a module-level memoized `select`, see `useReadingProgress`).
Beacon/lifecycle flows (scroll + reading POSTs, the visit lifecycle, the SSE
plumbing itself) are NOT query-shaped and stay custom by design.

### 24. One SSE connection per daemon per BROWSER — a SharedWorker owns the streams (SW-track)

The daemon is HTTP/1.1-only, and Firefox caps persistent connections at 6
per host:port; per-tab SSE streams exhausted that pool (parked sockets →
new tabs hung loading forever). So the SPA holds **one unfiltered
`/api/events` stream per daemon for the whole browser**: a SharedWorker
(`web/src/workers/sse.worker.ts`) hosts the context-agnostic connection
core (`web/src/sse/core.ts` — no DOM, no localStorage, injected
`CursorStore`), and tabs are thin MessagePort clients
(`web/src/sse/transport.ts`, protocol in `web/src/sse/protocol.ts`).
Frames are parsed by a hand-rolled fetch/ReadableStream SSE decoder
(`web/src/sse/stream.ts`), NOT EventSource — the daemon names every frame
with `event:`, so EventSource delivers only pre-registered types
(hand-maintained type lists; types subscribed after connect silently
dropped until reconnect). The stream carries NO `?types=` filter —
`metrics.tick` rides along (1 Hz is noise; the old on-demand second
stream is gone) and new daemon event types reach subscribers with zero
client changes. **Never open EventSource or stream `/api/events` from tab
code** — go through the `sse` facade (`subscribe` / `subscribeEvent` /
`subscribeAll`); the facade keeps per-tab registries + aggregation, so
the invalidation bridge (invariant #23) is transport-blind. Rules that
keep it correct: the worker is the sole cursor ADVANCER (in-memory; tabs
mirror `kb:lid:<url>` to localStorage, throttled, and seed a fresh worker
via `hello.cursors` — re-introducing per-tab cursor writes re-introduces
the multi-tab skip race); a port joins the worker's broadcast set only
after a valid `hello`, so the `snapshot` reply is always the port's FIRST
message (late-joining tabs get correct accumulated status); `gap` →
worker clears its cursor + broadcasts `resync` and every tab invalidates
wholesale; bfcache restore (`pageshow.persisted`) re-joins with a fresh
port and fires the same resync path (the tab missed every port message
while frozen — its `staleTime: Infinity` cache would otherwise serve
stale data indefinitely). The **direct fallback** (same core, hosted
in-tab, localStorage cursors) is load-bearing, not vestigial: it is the
compat story (no-SharedWorker browsers), the failure containment (no
`snapshot` within the handshake timeout → degrade), and the test escape
hatch (Playwright cannot intercept SharedWorker-initiated requests) —
force it with `?sse=direct` or `localStorage["kb:sse:transport"] =
"direct"` (also the dev workaround: HMR never reaches a running
SharedWorker). The worker URL stays a HASHED asset: a deploy briefly runs
two worker generations (bounded 2× connections; the build-sha drift
banner nudges stale tabs), whereas a stable name would be cached
`max-age=3600` and serve a stale worker under a new shell. The daemon
gauges live consumers as `sse_subscribers` in `metrics.tick` (dedicated
counter + Drop-guard in `routes/events.rs` — NOT `receiver_count()`,
which counts internal bus subscribers); e2e:
`tests/e2e/sse-shared.spec.ts`.

### 25. Reading lists — derived read-state, review::Anchor reuse, no generation bumps (RL-track, v0.18)

Reading lists (the v0.13 bookmarks replacement; `lists` + `list_entries`,
V0015, V0016 dropped the old table) are per-kb sqlite side-channel state
with four load-bearing rules. **(a) Read state is DERIVED, never
stored** beyond the manual `read_override`: every list response builds
one `(DocSummary, reading::ReadingSummary)` per distinct artifact and
runs `kb_core::lists::derive_read_state` per entry — override wins, then
section dwell (`Section{id}` joins `summary.sections` on the live DOM
heading id, the invariant-#19 id space), then whole-artifact scroll/word
completion (≥ `FULLY_READ_PCT`). Storing a computed state would rot the
moment a visit lands. **(b) Entry anchors ARE `review::Anchor`** —
serialized canonically via `lists::anchor_to_json` (the dedupe index
compares anchor TEXT byte-wise; never hand-build the JSON) and
re-resolved by the indexer's `ListAnchorHook` (`enrich.rs`, registered
last) through `review::fuzzy_resolve_anchor`. Staleness is persisted ON
THE ROW (`anchor_stale` — restart-safe by construction, unlike the
comments tracker's in-process map); transitions emit
`list.entry.anchor_stale` / `_resolved` only AFTER the
`ListEntriesSyncResolution` write lands, and that machine write never
touches `updated_at`. **(c) No list mutation bumps the index
generation** (invariant #15's pin test asserts it) — lists never change
the lance row-set; a gallery list-membership badge must therefore use
the lifted-hook decoration pattern, never a generation bump. **(d)
Ordering is a dense 0-based renumber inside one sqlite transaction** —
race-free because all writes serialise through the storage actor; a
bulk import is ONE transaction and ONE `list.updated` emit (no per-entry
SSE storm), preserving entry ids/`created_at` on same-list round-trips
and reminting ids that live in another list (table-wide PK). The
portable kb-list/1 Markdown grammar is pinned by golden tests in BOTH
kb-core (`lists::tests::golden_markdown_export_is_stable`) and kb-cli —
divergence fails tests, not users. Surfaces: `routes/lists.rs`,
`commands/list.rs`, SPA `["lists"]` / `["list", kb, id]` keys +
`?sec=`/`?list=&entry=` deep links (docs/reading-lists.md).
*v0.33 X3 — prune:* tombstones stay READ-TIME derived (an entry whose
artifact_id no longer resolves — relocate rekeys entries, so a moved doc
is never a tombstone); `lists::prune_list` deletes every tombstoned
entry in ONE tx (dense renumber preserved), `POST …/lists/{id}/prune` /
`kb list prune` / the SPA "clean up" button all go through it, emitting
one `list.updated` only when removed > 0 — rules (b)/(c)/(d) unchanged.

### 26. ONNX Runtime is isolated to `kb-embedder` (portability)

`fastembed` (and the ONNX Runtime it bundles) is an **optional** kb-core
dependency behind the `local-embedder` feature. Only `crates/kb-embedder` enables
it (`kb-core = { features = ["local-embedder"] }`); kb-server and kb-cli
depend on kb-core WITHOUT the feature and link **zero** onnxruntime. So the heavy
native dep lives in exactly one binary — the niced sidecar the daemon spawns. The
daemon drives **both** embedding and reranking over the stdin/stdout NDJSON IPC
(`embed_ipc`): `IpcBackend` (embed) + `RerankerClient` (rerank, a
`kb-embedder --reranker` subprocess). `embed::Reranker`/`Embedder::new`/`with_info`
and the `fastembed::*` enum maps (`fastembed_model`/`fastembed_reranker`) are all
`#[cfg(feature = "local-embedder")]`; the model/reranker registry tables
(`ModelInfo`/`RerankerInfo`) are fastembed-free so the daemon reads `dim` etc.
without ORT. Load-bearing consequences: (a) ORT is **statically bundled**
(`ort-download-binaries-rustls-tls`, NOT `ort-load-dynamic`) — the old dlopen
path deadlocked (macOS-era finding); the static bundle also keeps the daemon
ORT-free and portable; (b)
`cargo {test,clippy} --workspace` must pass `--exclude kb-embedder`, else Cargo
unifies `local-embedder` ON across the shared kb-core rlib and pulls ORT into
every binary (the `justfile` `ci-workspace` does this; `ci-embedder` covers the
ORT surface); (c) every match on `EmbedderBackend` needs the `Local` arm gated, or
it's non-exhaustive with the feature off; (d) `kb model download` shells out to
`kb-embedder --download-only` (kb-cli links no ORT); (e) ORT binaries are fetched
from a CDN at build — for offline builds set BOTH `ORT_STRATEGY=system` and
`ORT_LIB_LOCATION=<dir>` to link a local ONNX Runtime instead.

### 27. Stored `Doc.path` is canonical; artifact IDs are source-relative

The indexer's `prepare_doc` (formerly `index_file` — GC-B7 split it into
`prepare_doc`/`finish_indexed_doc`, batch-committed via
`flush_prepared_batch`→`upsert_docs`; behaviour unchanged) stores
`paths::canonical_abs(path)` (canonicalise-or-fallback) and
`reconcile` walks the **canonical** source root, so a symlinked source root —
WSL/bind mounts — doesn't desync the canonical
prefix check in reconcile's delete pass or the exact-match in
`get_by_source_path`. The daemon also canonicalises `source_path` at bring-up, so
the watcher agrees. **Artifact IDs do NOT depend on this**: they derive from the
source-relative path via `paths::doc_rel_path`, which canonicalises BOTH sides
before stripping — changing the stored absolute representation must never change an
id (ids key review files, history, reading-progress). Existing indexes self-heal on
the first reconcile after upgrade (canonical rows replace raw ones); non-symlinked
roots are a no-op (raw == canonical). The daemon already canonicalised
`source_path` at bring-up before this change, so the standard daemon flow stored
canonical paths regardless — this hardens the **core** layer (tests, containers,
any caller that passes a raw symlinked root), not a live production data-loss.
Regression: `reconcile_emits_delete_through_a_symlinked_source_root`.

**Known limitations (symlinks below the root).** The fix covers a symlinked
*root*; two narrower shapes are out of scope: (1) an **intermediate** path
component that is a symlink can make `doc_rel_path` derive a different rel-path —
hence a different artifact id — between an index run (canonicalise succeeds) and a
delete pass for an already-gone file (canonicalise fails → raw fallback), so the
`artifact.removed` id and its id-keyed cleanups may target the wrong id; (2) a
source **file** that is itself a symlink to a target *outside* the root is stored
under its canonical out-of-root path, which then fails reconcile's
`starts_with(canonical_root)` check (never reconciled away). Both are mitigated in
practice — `walkdir` does not follow symlinks, so such files enter only via live
watcher events, never the walk — but symlinked source *files* are formally
unsupported; keep corpora on real paths.

*Relocate amendment (F3, 2026-07-30):* ids stay path-derived — and the ONLY
sanctioned way to change an artifact's path is the **relocate engine**
(`kb_core::relocate`, `kb mv`, `POST …/docs/{id}/move`, `…/folders/rename`).
A raw filesystem move is still a watcher-detected delete+create that
cascades user data away; relocate instead writes a durable `moves` intent
row FIRST (V0032, `completed_at` stamped inside the same sqlite tx as the
rekey), renames source + `.review/<id>.json` (internal id rewritten) +
`.attachments/<id>/` under the per-kb `review_lock` (#6), then ONE actor
write-lane message clones the lance row under the new id PRESERVING the
embedding (chunks re-keyed, no re-embed), UPDATEs every id-keyed sqlite
consumer in one transaction (edges both ends, sessions + children incl.
`session_files.target_artifact_id`, history, reading_sections,
`list_entries` — positions/notes untouched, #25 — memory tables, snapshots,
atlas points, `excluded_files.path`) and bumps the generation once (#15).
The watcher race is guarded two-layer (in-memory `PendingMoves`, 10 s TTL,
registered BEFORE the rename + durable moves-table suppression consulted by
`process_delete`); `bring_up_kb` replays incomplete moves idempotently and
ABANDONS never-renamed intents so they can't suppress a real delete
forever. Old identities keep resolving through the newest-wins `moves`
chain: SPA shell `/a/{kb}/{old_rel}` → 301 (query preserved), `/lookup`
returns the live doc as Exact, `/docs/{old_id}` → 301. Known accepted
costs: atlas historical coords strand until the next rebuild. (FU1 wired
the shared indexer DedupCache into relocate — rekey on move, no re-embed.)

### 28. Federated read handlers fan out concurrently in submission order

**Multi-kb, one process** — not multi-daemon. "Fleet" here means every
corpus in `state.kbs` on *this* daemon. Remote daemons (`daemons.toml`,
`kb fleet status`, SPA daemon switch) fan out on the **client** side;
the server never proxies scope=all to peer hosts.

Every "scope=all" / multi-kb read handler — `search` (browse + hybrid), `memory`
recall, `sessions` (list/get/memories/touches/readings + the find-first lookups),
`lists`, `notes`, `anchors` (corkboard + stale), `/inbox`, `/queries/zero-hit`
(GC-B3), `/stats`, `/kbs` — runs its
per-corpus work through `routes::buffered_join(futs, state.fanout_cap)`, NOT a
serial `for (name, ctx) in state.kbs` await loop. Latency was linear in corpus
count; the operator runs ~18 corpora. *PF-R1:* the cap is the operator-
configurable `[server] fanout_cap` (default 8 = the old hardcoded value;
`routes::FANOUT_CAP` remains the one true default constant backing
`ServerSection::default_fanout_cap()` — never let them drift), resolved once
at boot/restart onto `KbHandles::fanout_cap`. It changes CONCURRENCY only —
submission order, `buffered` (never `buffer_unordered`), and the fold are
untouched by any value, and `buffered_join` clamps 0 to 1.

Three things are load-bearing:

1. **Submission order, not completion order.** `buffered_join` is built on
   `futures::StreamExt::buffered` (ordered), never `buffer_unordered`. The merge
   is order-sensitive: per-hit kb attribution (see the 2026-08-21 `(kb, id)`
   amendment below — superseding the old `id_to_kb` first-wins map), the RRF
   arm order, and relevance-mode output all depend on iterating corpora in
   `BTreeMap` order (the
   `state.kbs` map is a `BTreeMap`, invariant: deterministic iteration). Callers
   build one boxed future per corpus, bake the kb name into each future's output,
   `buffered_join`, then fold in the returned (submission) order and apply the
   existing explicit sort. A completion-order collect would flip these under load
   — the golden gates are `federated_search_scope_all_is_deterministic_and_attributes_kbs`
   and the `memory_recall_*` / `l7_recall_*` suite.
2. **No `std::sync::Mutex` guard across a fan-out await (invariant 15).** Inside
   each future, take any guard (embedder, `last_reconcile`), extract the owned
   value, drop the guard, THEN await. `Embedder::model_name()` returns `&'static
   str`, so the embedder guard drops at the `let model` before the storage await.
3. **Per-corpus skip-on-error isolation.** Each future returns a partial
   (`Vec`/`Option`/`Result`); the caller folds and DROPS failures. A future must
   never `?`-propagate a per-corpus error — one corrupt corpus must not 500 the
   whole fleet view (the pre-refactor `continue`/`unwrap_or` behaviour).

`buffered_join` is `pub(crate)` in `routes/mod.rs` with `CorpusFut<'a, T>` (the
boxed-future alias) and a submission-order / cap-concurrency / fold-drop unit
test. The closure can't be higher-ranked over the item borrow (a `for<'b>` bound
quantifies over `'static` and the futures borrow non-`'static` handler locals), so
callers pre-build the `Vec<CorpusFut>` rather than passing a closure to a generic
`fanout(items, cap, f)`. Don't reintroduce a serial federated loop, and don't
swap `buffered` for `buffer_unordered`.

**CT amendment (2026-08-21) — federated merge attribution now keys on
`(kb, id)`, not id alone; submission-order determinism is retained.**
Bullet 1's original "`id_to_kb` first-wins" claim assumed `DocSummary::id`
(a source-relative-path hash, invariant #27) is globally unique across
corpora — disproven by the same collision the ARTIFACT HOST GRAMMAR v2
amendment (invariant #7) documents: two kbs sharing a rel path hash to
the same id. The old `id_to_kb.entry(id).or_insert(kb)` first-wins map
attributed EVERY hit sharing that id to whichever kb folded first, and
`rrf_fuse`'s id-only dedup then merged both corpora's score contributions
into a single row under that one kb — silently losing the other corpus's
hit rather than surfacing it. `federated_search` (`routes/search.rs`) now
tags every arm with its owning kb name AT FOLD TIME (`Vec<(String,
DocSummary)>` throughout the browse, hybrid, and rollup/rank-map paths —
no separate `id_to_kb` lookup map at all), and
`kb_core::fusion::rrf_fuse_keyed` (the keyed counterpart of `rrf_fuse`,
same `Σ 1/(k+rank)` + stable-sort contract) accumulates on `(key,
doc.id)` instead of `doc.id` alone: two arms sharing a key still merge
their contributions exactly like `rrf_fuse` (a corpus's own bm25 + vector
arms, pre-fused into one arm per corpus before reaching here), but two
arms with DIFFERENT keys never merge even on a colliding id — each keeps
its own row and its own score. Every per-hit merge map downstream of the
fan-out — the reading rollup, `bm25_rank`/`vec_rank`, the snippet-body
map, and `cmp_search`'s sort/tie-break key — is keyed the same way via
`hit_key(kb, id) = "{kb}:{id}"` (`:` is safe: `KbName::new` forbids it in
a kb name).

Submission order itself — the OTHER half of bullet 1 — is UNCHANGED:
`rrf_fuse_keyed` keeps `rrf_fuse`'s exact tie-break (score desc, then
first-appearance-across-arms asc), arms are still submitted in
`state.kbs` `BTreeMap` order, and `buffered` (never `buffer_unordered`)
still drives the fan-out. Only the per-hit IDENTITY key changed, not the
ordering discipline. Golden coverage:
`federated_search_scope_all_is_deterministic_and_attributes_kbs`
(`crates/kb-server/tests/end_to_end.rs`) plus `fusion.rs`'s
`keyed_fuse_keeps_same_id_distinct_across_different_keys`,
`keyed_fuse_still_merges_same_key_arms`, and
`keyed_fuse_matches_plain_fuse_when_every_arm_shares_one_key` tests, and
`search.rs`'s `cmp_search_uses_explicit_keys_not_the_docs_shared_id` test.

### 29. Wikilinks/backlinks ride the existing edge graph; resolution is pure + corpus-local

A note's `[[target]]` / `[[target|alias]]` reference is the connective tissue
that links notes to any artifact, note, or session. It is NOT a new entity or a
new table — it rides the v0.3 `edges` table (`kind="link"`), so wikilinks flow
into the backlink/outlink counts, the atlas link-layer, and the graph route the
same way HTML `<a href>` links always have. Load-bearing shapes:

- **Parsing + resolution live in `kb_core::links`, pure + LLM-free.**
  `parse_wikilinks` uses comrak's wikilink AST (`wikilinks_title_after_pipe`),
  so code spans/fences are skipped exactly like `notes::scan_tasks`. `resolve`
  is a pure function over a supplied `&[DocLite]` (the caller does the I/O) with
  a deterministic first-hit ladder: **id → source-relative path → exact title
  (ci) → unique basename (ci)**. Tiers 3–4 may be `Ambiguous` (the graph drops
  those — a shared title isn't an unambiguous edge). Resolution is
  **corpus-local** (each kb's edges live in its own sqlite); never resolve a
  wikilink cross-kb.
- **The grammar is golden-pinned in BOTH languages and must stay in lock-step**
  — `kb_core::links` (Rust, comrak) and `web/src/lib/wikilink.ts` (the SPA hast
  pass), the same dual-render contract callouts use. The SPA does NOT re-resolve:
  the server hands it a resolution map (`NoteDetail.links`, keyed by
  `normalize_target(target)`) and the SPA looks targets up by the same
  normalization, so a rendered link and the recorded edge always agree.
- **The render path is unchanged.** `markdown::kb_options` does NOT enable the
  wikilink extension — resolving a target to a permalink needs storage, which the
  pure render path lacks — so served HTML leaves `[[…]]` literal and the SPA
  renders links from the map. A dangling/ambiguous link is a muted "pending"
  affordance, never an error.
- **The edge hook parses from `raw_source`, gated on a literal `[[`.** The
  `EdgeRecordHook` (invariant kb-core #5) resolves a Markdown note's `[[…]]`
  against `list_docs(u32::MAX)` and records `kind="link"` edges deduped with the
  HTML-link pass; `EnrichCtx.source_root` maps candidate abs paths to rel. Plain
  notes (no `[[`) pay nothing. Reverse lookups go through `backlinks_of(id)`.
- **Resolution is index-time; danglers heal on the SOURCE's next index, not the
  target's.** A `[[B]]` written before B exists records no edge; when B later
  appears, the edge materialises only when A is re-indexed (touch/edit A). The
  SPA's inline render of A self-heals immediately (outgoing links are resolved
  live per request), but B's backlink panel won't list A until then — an
  accepted limitation (no retroactive reverse re-resolution scan). The SPA
  backlink panel listens to `artifact.indexed` (not just `note.*`) so it
  refreshes the moment any reindex commits an inbound edge.
- **Memory-to-memory wikilinks: declined again (2026-08 close-out, Unit 4)**,
  re-investigated on the hypothesis that widening `EdgeRecordHook`'s
  `is_markdown(ctx.path)` gate to admit memory HTML might be a safe small
  change. It is not, and the reason is sharper than "memories are HTML
  artifacts": `parse_wikilinks` (comrak's CommonMark block parser) finds
  **zero** wikilinks inside ANY `<p>...</p>`-wrapped content — CommonMark
  treats `<p>` (like `<html>`/`<body>`) as an HTML-block-starting tag, so its
  content is captured verbatim as an opaque `NodeValue::HtmlBlock` and never
  walked as inline markdown text, where the wikilink extension's inline scan
  actually runs. Only genuinely tag-free plain text triggers it. Proven by
  `links::tests::wikilinks_are_invisible_inside_any_p_wrapped_html_only_bare_
  text_works`, which round-trips a full memory HTML document, a bare `<p>`
  fragment, and multiple blank-line-separated `<p>` blocks (all `[]`) against
  tag-free plain text (finds both links). `crate::memory::text_to_body_html`
  wraps EVERY memory body in `<p>…</p>` unconditionally — this isn't an edge
  case the gate could special-case around, it's the universal shape of a
  memory's stored body. Widening the gate alone would ship a feature that
  silently does nothing. A real fix would need a memory-specific HTML→plain-
  text inverse of `text_to_body_html` run before the parse — which has no
  general inverse for a memory body that ISN'T `text_to_body_html`'s own
  output (`kb remember --source fetched-web`, SPA-authored bodies, anything
  with real markup) and would stand up a THIRD parsing/sanitization pipeline
  (alongside the Rust/comrak ↔ SPA/hast pair this invariant already keeps in
  lock-step) whose entity-unescaping and tag-stripping edge cases would have
  to independently agree with what the corpus actually renders. That is a
  rework of the note-only assumption, not a safe widening — still declined
  until there's an actual design for that inverse-extraction step.
- **CT-F3 (v0.38) — unlinked mentions are a DERIVED queue, never a hook.**
  "The graph you wrote is half the graph you meant": `kb_core::mentions`
  finds docs whose PROSE names another artifact's exact title or unique
  basename with no `kind="link"` edge to show for it, and
  `GET /api/kb/{kb}/links/suggest` reports them **computed per request,
  persisted nowhere** (the memory dupes/triage posture; the same
  never-cache-a-claim rule as invariant #2's trust classes). Nothing runs on
  the ingest path and nothing is ever auto-linked: the only mutation is the
  explicit `POST …/links/apply` (`kb links apply`), which RE-DERIVES the
  mention against the current corpus before splicing. It is not a second
  scanner — `links::prose_text` is the SAME comrak parse `parse_wikilinks`
  uses (so code spans/fences, existing `[[…]]`, and Markdown link labels are
  skipped by construction), and `mentions::apply_wikilink` verifies its
  splice by re-running `parse_wikilinks` + `prose_text` on the candidate
  rather than translating a prose offset back to the source. Guard rails:
  self-mentions, already-edged pairs (directionally — a `dst → src`
  backlink does not suppress `src → dst`), names below
  `mentions::MIN_MENTION_LEN` (12 chars, the CT-C5 floor precedent, surfaced
  on the wire), ambiguous names (dropped exactly as the resolver's ladder
  drops them), and `memory-session` transcripts (R0, invariant #11) are all
  excluded. The authored target is always VERIFIED to resolve back to the
  destination through `ResolveIndex`, so applying can only ever write an
  edge the hook will agree with. **The memory ruling above is untouched**: a
  memory (like any HTML artifact) can be a link TARGET but never a link
  SOURCE, so its rows are reported with the honest
  `mentions::Applicability` note and `apply` refuses them with a 400, file
  untouched.

### 30. Reader chrome — one home per action; ONE merged inspector rail (v0.20 → v0.23)

Every reader-chrome feature has exactly ONE home, by altitude:

- **ContextBar** (`web/src/components/chrome/ContextBar.tsx`) owns the
  per-artifact verbs: anchor · add-to-list · copy-link · share · **fullscreen**
  (icon-only `data-kb-act="fullscreen"` → `setImmersive(true)`, binds `o`,
  replaced the old open-in-tab `<a>`) · **bare** (v0.22, `data-kb-act="bare"`,
  binds `b`). Copy-link shows a 1.5 s inline "copied ✓" swap (`is-ok`/`is-fail`);
  the failure path surfaces via the F1 toast.
- **The right dock is a SINGLE icon rail** (`.kb-pinsp__icons`), owned by
  `PreviewInspector` — now the persistent shell of the column. **v0.21 merged
  the two old tab rows into one**: the `[inspect|comments|versions]` text pills
  are GONE (`ReaderDock` is now just the `.kb-dock` column chrome). The rail is
  one `role=tablist`: **6** inspector sub-tabs (`data-kb-itab`,
  **About**/Meta/Links/Folder/Sessions) + comments + versions icons
  (`data-kb-act="dock-{comments,versions}"`), separated by a
  `.kb-pinsp__rail-gap` divider. **v0.22 reshaped the rail**: the standalone
  `about` sub-tab was dropped and the merged stack-everything tab (internal key
  still `"all"`, so `show()` + persisted localStorage are unchanged) is now
  FIRST and relabeled **About**. Badges follow ONE grammar (`itabBadge(t)` →
  `.kb-pinsp__itab-badge` count when >0, else the meta error `.kb-pinsp__itab-dot`):
  folder (sibling count) · links (out+back) · sessions (related-memories count)
  · comments (`comments-badge`, open count) · versions (`versions-badge`, index-
  snapshot count). The comments/versions PANELS no longer carry their own ✕
  (`cp__close`/`vp__close` removed) — the rail icon toggles them closed.
  - **`panelMode`** ("inspect"|"comments"|"versions", from detail.tsx's
    `showPanel`/`showVersions`) selects what the body shows: the inspector
    sections (`show(tab)` = `tab===t || tab==="all"`; the `"all"` default stacks
    every section — the single-scroll fallback the e2e relies on) OR the
    `panelSlot` (the CommentsPanel/VersionsPanel detail.tsx passes in). The slot
    renders as a direct `.kb-pinsp` child (CSS `.kb-pinsp > .comments-panel`,
    `flex:1`) BELOW the rail, so it fills the body without a second scrollbar.
  - All inspector hooks stay unconditional at the top (Rules of Hooks); only the
    section-body JSX is wrapped behind the `panelMode === "inspect"` guard.
    `useInspectorTab` (cloned from `useInspectorCollapsed`) persists the chosen
    sub-tab in localStorage and **defaults to the last-used tab** — a documented
    #23 carve-out. Clicking an inspector icon also calls `onSwitchToInspect`
    (clears comments/versions); clicking comments/versions toggles their state.
  - **e2e drives panels through `dock-comments`** at 1280×720 (never a mobile
    ContextBar toggle). On desktop the rail is the always-docked column;
    **v0.23** collapsed the mobile surface to ONE `kb-ctxbar__act--mobile` button
    (the surviving `data-kb-act="inspect"`) that raises the SAME merged rail as a
    single bottom sheet — see the v0.23 subsection below.
  - The inspector column is `360px` (was 320 — matched to the comments/versions
    width so the persistent rail doesn't reflow when you switch body).
- **v0.22 — metadata is navigation; three explicit viewing modes.** The reader
  became a faceted-discovery surface (see also the gallery deep-link grammar in
  #35):
  - **Clickable-metadata passport** (the About tab's Identifier block): folder
    renders as a per-segment breadcrumb of gallery `<Link>`s (shared
    `FolderCrumbLinks`, root → "(root)"), category + mtime + each tag deep-link
    via `galleryUrl()`, the id copies to clipboard, and a `created` row was
    added. `TagPill` gained an optional `to` (renders a `<Link>` when set).
  - **"Explore from here"** chip strip (top of About): every facet (folder ·
    category · each tag) pivots into the gallery, badged with the corpus-wide
    count from `useFacetCounts(kb)` — ONE cached fetch each of /tags + /facets +
    /folders, intersected client-side (never per-facet count queries).
  - **Links tab is a directed graph**: Outlinks → (`src===self`) + ← Backlinks
    (`dst===self`), both from the corpus-local edge graph (#29). The dead
    `doc.backlinks/outlinks` Metrics rows (null on the single-doc GET) are gone.
  - **Related memories are re-anchored** to the artifact (recall `q` = title +
    summary), shared by the Sessions-tab panel + the rail count badge via the
    `useRelatedMemories(kb, query?)` TanStack hook (under the `['memories']`
    SSE-invalidated prefix). `useVersions`/`useFacetCounts` are likewise TanStack
    queries refreshed by the `docsGate` (now also invalidates
    `['versions'|'facets'|'folders']`).
  - **Three viewing modes** — embedded (default) · immersive (`o`, now URL-
    derived `?view=immersive` so it is SHAREABLE; the iframe key is id-based so
    `?view` never reloads it) · **bare** (`b` / the ContextBar bare button):
    opens the artifact's own `<id>.artifacts.<suffix>` origin in a new tab with
    `rel="noopener noreferrer"` — `noreferrer` makes the daemon's referrer-gated
    bounce (`routes/artifact.rs`) skip so it renders chrome-free. The bare URL
    MUST be the artifact origin, NOT `/a/{kb}/{rel}` (SPA shell, #34) nor
    `/api/kb/{kb}/artifact/{id}` (`CSP: sandbox` neuters scripts). Zero server
    change.
- **v0.23 — mobile is ONE button, ONE sheet.** At ≤860px the ContextBar drops
  its standalone `comments`/`versions` `--mobile` toggles; the SOLE mobile entry
  is the surviving `data-kb-act="inspect"` button — kept by name (no e2e/rename
  ripple), badged with the open-comment count (`.kb-ctxbar__cnt`) and carrying
  `aria-expanded` / `aria-controls="kb-reader-sheet"` / `aria-haspopup="dialog"`.
  It flips `inspectorOpen`, **re-scoped** from "inspector-mode sheet gate" to
  "the mobile sheet is open in ANY panelMode".
  - **The sheet IS `.kb-pinsp`** promoted to `position:fixed; inset:auto 0 0 0`
    in EVERY panelMode, so the FULL rail (`.kb-pinsp__icons` with `data-kb-itab`
    + `dock-comments`/`dock-versions`) rides inside it and comments/versions
    render as the `panelSlot` child below the rail via the existing
    un-media-scoped `.kb-pinsp > .comments-panel / .versions-panel {flex:1 1 auto;
    min-height:0}` (no new fill CSS, no double scroll). The old divergent
    mobile.css block — the `.detail--with-panel/-versions` rail-dissolvers
    (`.kb-pinsp__icons{display:none}` + `.kb-pinsp{display:contents}`) and the
    `.comments-panel/.versions-panel{position:fixed}` railless sheets — is
    DELETED; it was the source of the railless dead-end.
  - **`panelMode` is orthogonal to `inspectorOpen`:** `handleTogglePanel` /
    `handleToggleVersions` / `handleToggleAnnotate` no longer call
    `setInspectorOpen(false)`, so switching bodies keeps the sheet open; the
    AnnotatorBridge `onComposeAnchor` / `onFocusComment` callbacks call
    `setInspectorOpen(true)` so an in-artifact tap surfaces the sheet on comments.
  - **Slide-in** mirrors `.kb-drawer`: mounted-and-translated
    (`translateY(100%)`→`translateY(0)` + `visibility` on the
    `.detail--inspector-open` gate, `.24s var(--ease)`, `prefers-reduced-motion`
    honored). The gate class MUST be applied in BOTH render paths (`detailClass`
    AND the native-note inline className) on all three `detail--with-*` branches —
    a missed edit silently leaves notes without the sheet.
  - **Z-order fix:** a tap-to-dismiss `.kb-pinsp-scrim` (mirrors
    `.kb-drawer-scrim`) sits at `--z-scrim` UNDER the sheet at `--z-drawer` and
    OVER the immersive FAB at `--z-float` — the pre-v0.23 sheet sat at
    `--z-popover-hi` (30), BELOW the FAB (35). The FAB is also hidden while
    `.detail--inspector-open`.
  - **Dismiss** = the labelled ✕ in `.kb-pinsp__sheet-head` (now shown in every
    mode, with a decorative `.kb-pinsp__grab` pill) + scrim tap + Esc (guarded
    against typing, disjoint from the immersive Esc effect). `role=dialog` /
    `aria-modal` / `id="kb-reader-sheet"` are gated to mobile via the new
    `asSheet` prop so the desktop docked `<aside>` semantics + e2e are untouched.
  - **Deferred:** multi-detent drag/snap gestures and any auto-open-to-comments
    one-shot. Desktop (>860px) is byte-for-byte unchanged
    (`.detail--inspector-open` has no desktop rule).

**v0.29 amendment — TWO panes, still ONE rail.** The reader gained a two-pane
artifact compare mode, and "one home per action" survives it by making the rail
focus-keyed rather than pane-local:

  - **ONE `.kb-pinsp` rail for the whole reader**, fed by the FOCUSED pane's
    kb/doc/id. ContextBar IS duplicated per pane (it is per-*artifact* chrome —
    anchor, add-to-list, copy-link, share, bare, split); the rail is NOT. The
    sub-tab count stays **6**; a second rail or a 7th sub-tab breaks this
    invariant.
  - **Pane location is derived PURELY from `?pane2=`** (`web/src/lib/paneUrl.ts`,
    golden-pinned, `parsePane2` is TOTAL — malformed ⇒ `null`, never partial).
    There is deliberately no "is a split open" React state to drift out of sync
    with the URL; that is what keeps back/forward and permalink-sharing honest
    (#23). `pane2` is appended LAST in `artifactHref` so every pre-existing
    golden string is byte-unchanged when it is absent.
  - **Only an ARTIFACT may occupy a pane.** No search/gallery/list pane: four
    singletons block the general case (`useScrollRestoration` keys on
    `pathname+search` and assumes WINDOW scroll (#31); `useRovingCursor` owns one
    unscoped window keydown listener; `useDocumentTitle`/`lastGalleryUrl` are app
    singletons; every route sits behind a `React.lazy` boundary that exists to
    keep CodeMirror/atlas out of first paint).
  - **Per-pane visit lifecycle is load-bearing for #8/#19.** `isArtifactOrigin`
    is SUFFIX-only, so every artifact iframe passes it — with two panes, pane 2's
    `kb:scroll`/`kb:reading` beacons would be accepted by pane 1's handler and
    POSTed against the WRONG artifact. Use
    `isOriginOfArtifact(origin, id, kb, suffix)` (exact origin equality; the
    kb-qualified id is in the hostname — host-grammar v2, #7) and keep the
    visit refs inside `ArtifactPane`.
  - **Mobile never renders pane 2.** Two cross-origin iframes each wanting full
    viewport height do not degrade by stacking; ≤860px is simply "single pane,
    ignore `?pane2`", leaving the v0.23 one-button-one-sheet contract intact.
  - **Pane chords are doc-only in the registry.** The `w`-prefix family
    (`w v`/`w q`/`w h`/`w l`/`w o`) is registered for the cheat sheet but
    EXECUTED by an independent handler in `detail.tsx`: at `scope: "global"`,
    `useRovingCursor`'s sibling window listener would also see the `h`/`l` and
    move the gallery cursor (`preventDefault()` does not stop a sibling
    listener). `Ctrl-w` is unavailable regardless — `HotkeyRoot` returns early on
    any modifier and Cmd/Ctrl-W is the browser's close-tab.
  - **Registers SUBSUME marks**, they do not sit beside them: shipping both would
    make `m <letter>` and `" <letter>` two homes for "remember this place" —
    exactly the failure this invariant exists to prevent. One tagged `Ref` union
    store (`web/src/lib/registers.ts`), marks are the artifact-position kind, old
    entries migrate forward on read. Browser-local, never crosses the network ⇒
    **recorded CLI-parity exemption**.
  - *Linkflow amendment (2026-08-07):* the reading-flow return chip
    (`data-kb-act="flow-back"` + its stack popover, `u` keybind) is ContextBar
    chrome on the PRIMARY pane only — NOT a 7th inspector sub-tab (the rail
    count stays 6) and never on the compare pane. ONE `PeekCard` component
    serves BOTH peek triggers (the pre-existing Alt-hover on SPA-native `/a/`
    links via `HotkeyRoot`, and the in-artifact hover relay via
    `ArtifactPane`) — two triggers, one home. The flow stack is per-tab
    sessionStorage (like the scroll-restoration tier, not the registers tier)
    and browser-local ⇒ same **recorded CLI-parity exemption** as registers.

### 31. Scroll restoration keys on the full URL; the in-app back must replay it (X1)

`useScrollRestoration(key)` (`web/src/hooks/useScrollRestoration.ts`) persists
the **window** scroll offset (gallery + search both window-scroll —
`useWindowVirtualizer` / a plain list, no inner overflow container) in
sessionStorage keyed on `location.pathname + location.search`. Load-bearing:

- **The key is the FULL filtered URL** (every view/filter/query keeps its own
  slot), so the in-app "back to recent" button MUST return to the *originating*
  gallery URL — not a bare `/?kb=` — or the key misses and the scroll is
  silently lost. The gallery records its URL+kb in
  `web/src/lib/lastGalleryUrl.ts`; the ContextBar back button replays it when
  it's for this artifact's kb (else the bare grid).
- **Restore is a bounded per-frame retry** (≤`MAX_RESTORE_FRAMES`) that waits
  until the virtualizer has laid out enough height to reach the target, then
  `scrollTo` once — self-terminating, never a standing loop. The
  passive `scroll` listener wakes only while scrolling.
- **The `dirty` guard is load-bearing:** persist only after a real (or restored)
  scroll, so a fast navigate-away before the restore lands never clobbers the
  saved slot with 0.
- `history.scrollRestoration = "manual"` (set in `main.tsx`) makes the hook the
  single source of truth so the native restore doesn't race its rAF.

**W3.D/S2 amendment (sessions-rethink, 2026-07-29) — the `ephemeralParams`
carve-out.** `useScrollRestoration(key, ephemeralParams?)` gained an optional
second argument: a list of query-param NAMES stripped from the STORAGE key
only (`normalizeScrollKey`, pure + unit-pinned in
`useScrollRestoration.test.ts`) — the URL itself is untouched, and the stored
offset still restores to whatever the live URL shows. This is NOT a general
"ignore some params" escape hatch: the key still derives PURELY from the URL
(no new component state to drift), it just canonicalises params that toggle
**ephemeral UI on top of a stable scrollable view** rather than genuinely
re-scoping the content. The motivating case: `/sessions?focus=<sid>` (S2's
mobile-sheet trigger — sheet visibility IS `?focus=` presence) mutates the URL
on every row tap; without the carve-out, each tap fragments the list's ONE
scroll position into a new per-`focus` sessionStorage slot, and a back-nav to
bare `/sessions` misses the saved offset entirely. `sessions.tsx` declares
`useScrollRestoration(pathname+search, ["focus"])`. Every pre-W3 call site
(gallery, search) is unaffected — the parameter defaults to `[]`, which
`normalizeScrollKey` short-circuits to a byte-identical return, so this is a
strict superset of the pre-amendment contract. A future ephemeral param
(e.g. a sheet-open flag with no URL correlate) would extend the SAME
declared list, never grow a second mechanism.

### 32. Resilience: one promise-based confirm host, one toast surface, route ErrorBoundary (v0.20)

- **`useConfirm()` is the only destructive-prompt path**
  (`web/src/components/ConfirmProvider.tsx`): a single `<ConfirmModal>` host
  mounted at the app root (never unmounts), driven by a promise resolver in a
  ref, so a call site stays a one-line `if (!(await confirm(...))) return;`.
  `ConfirmModal.expectedToken` is optional — omit for a plain yes/no, keep it
  for the type-the-token path (Admin DRAIN/forget). NEVER reintroduce
  `window.confirm` (comment-thread delete shipped unguarded once). The dialog is
  `dialog.confirm`; e2e clicks `.confirm__go` instead of accepting a native
  dialog.
- **One toast surface** (`web/src/lib/toast.ts` + `<Toasts>`): a user-action
  `.catch` must `toast.err`, never swallow silently (a stale-ETag 409 that
  looked successful was the bug). Background prefetches stay silent.
- **Route ErrorBoundary** (`web/src/components/ErrorBoundary.tsx`) wraps the lazy
  routes with **`resetKey={loc.pathname}` cleared in `componentDidUpdate` — NOT
  `key={pathname}`** (a `key` remounts Detail on every nav, resetting panel
  state + breaking 2 e2e).

### 33. kb-select: scope-aware pill + per-tab live URL + cold-seed lastKb (K1)

The active kb is a pure function of the tab's URL (`useExplicitKb`/`useActiveKb`
in `useActiveKb.ts`, both off the shared `["kbs"]` query), so two tabs on
different corpora are already independent — **per-tab live selection is the URL
and is never written to shared storage** (that would make them fight).
Load-bearing:

- The Header pill is honest: scoped (explicit path / `?kb=`) shows the kb;
  unscoped section views show the `kbs[0]` fallback **dimmed + "click to pin"**
  (`kb-ws--unscoped`), with the `.kb-ws-name` text byte-identical (e2e asserts
  it by value).
- **`lastKb` (in the `prefs.ts` blob) is read ONCE on a cold bare-"/" entry**
  (ref-guarded, validated against the live kb list — ghost-kb guard #13, skipped
  when it equals `kbs[0]`) and `navigate(replace)`d to; written on every
  active-kb change. It is deliberately **OUT of the `patchSettings` allow-list**
  (browser-local nav preference, not daemon UI state) — no new query/SSE (#23),
  no BroadcastChannel (#24). `search.tsx` uses the shared `useKbs()` (not a
  private `fetchKbs`) so its `kbs[0]` default matches the pill's.
- **Connection-loss surfacing (E1)** — `DaemonDownBanner` (also subscribe-only on
  `useDaemonStatus`, never a new connection #24) shows a slim banner when the SSE
  link drops. Gate it on a **per-daemon** connected signal
  (`daemons.some(d => d.phase !== "disconnected")`), NOT the aggregate `phase`:
  aggregate `idle` means BOTH "connected & quiet" and "never connected yet", so
  gating on it flashes the banner on first load.

### 34. Permalink shells carry server-injected, escaped OpenGraph meta (OG1)

A `/a/{kb}/{source_rel}` permalink is a byte-for-byte static SPA shell, so client
`useDocumentTitle` is invisible to crawlers/unfurlers. `serve_artifact_shell`
(`crates/kb-server/src/routes/spa.rs`) looks the doc up at request time
(`get_by_source_path`) and splices **HTML-attribute-escaped** `<meta
name=description>` + `og:title`/`og:description`/`og:type` immediately before
`</head>`. Load-bearing: **best-effort** (no dist / unknown kb / not-found →
plain shell, never a failure); description prefers the authored `kb-summary`
else the body excerpt; the splice only touches the parent `<head>` (the
kb-prompt `<template>` #5 is never in the shell); source-rel is used verbatim
(URL-safe paths — an escaped path just misses the lookup). The XSS-critical
splice/escape is pure-function unit-pinned in `spa.rs` tests.

### 35. Reader→gallery deep-links go through ONE builder; the gallery filter grammar is SPA↔wire↔server lock-step (v0.22)

Every clickable metadata atom in the reader (tag pill · category · folder
breadcrumb segment · mtime pivot · "Explore from here" chip) builds its URL
through the single `galleryUrl(kb, {tags, category, folder, from, to, sort, dir})`
helper (`web/src/lib/galleryUrl.ts`, golden-test-pinned) — never ad-hoc string
concatenation — so the gallery's URL grammar has one source of truth and can't
drift. The gallery (route `/`) reads each param and the filter is enforced at
THREE layers that must stay in lock-step (like the `?q=` DSL, #14-adjacent):
the SPA reads `?category`/`?from`/`?to` (`gallery.tsx`) + mirrors them in the
defense-in-depth client filter; `fetchDocsPage` serialises them
(`api/client.ts` `DocsQueryParams`); and the server applies them in
`kb_core::docs_query::matches` — `category` is an exact positive `kb-category`
gate and `from`/`to` are an absolute **`mtime_unix`** window (NOT the
`indexed_at`-or-`mtime` recency `since` uses), both applied ACROSS every OR
branch (route-injected, no DSL atom), unit-pinned in `docs_query.rs`. These
filters are PER-KB (every reader deep-link carries `?kb=`); a fleet variant
would be NEW work under #28. The category facet that makes a deep-link
refinable comes from `GET /api/kb/{kb}/facets` (the gallery Sidebar's Category
control + the reader's Explore chips both read it). **W1.A** extends the same
builder/wire grammar with a fourth flat param, `?read=` (csv over
`never-opened|unread|in_progress|read`) — unlike `category`/`from`/`to` it is
enforced as a route-side set-membership filter in `routes/docs.rs::list` over
the per-kb reading rollup rather than inside `docs_query::matches` (the
read-state signal isn't on `DocSummary`), applied after the `docs_query` pass
and before sort + paginate, and joined onto the visible PAGE (never the
corpus, never the `GalleryCache` memo — invariant #15) via
`reading_rollup_for_ids` on the `default` projection only.

**W2.3a — the `ids=` atom.** `galleryUrl(kb, {ids})` serialises an explicit
artifact-id set, enforced by `docs_query::matches` across every OR branch like
the others. It is a HARD filter (non-members disappear), capped at
`MAX_IDS_FILTER = 500` **both** client-side (`AtlasView`) and server-side
(`routes/docs.rs`, which 400s on overflow rather than silently truncating).

**v0.33 Y1/X1 — the `folder_exact=1` flag.** `galleryUrl(kb, {folder,
folderExact})` serialises `folder_exact=1` only when the flag is on AND
`folder` is set (existing goldens stay byte-identical); `fetchDocsPage`
mirrors it; the server dispatches `docs_query::folder_matches_exact`
(pure equality) instead of the descendant-inclusive prefix match, gated by
`DocsQuery.folder_exact` across every OR branch. The gallery's
defense-in-depth client filter applies the SAME exact-vs-prefix split — a
fourth lock-step point unit-pinned on both sides. Absent flag =
descendant-inclusive, byte-identical to pre-v0.33.

**v0.29 amendment — the pivot atom is the ONLY new grammar, deliberately.**
Wave 3 added three surfaces that each wanted their own time/selection filter,
and all three were routed through what already exists rather than growing the
lock-step:

  - **The reflection canvas brush** (four synchronized UTC-day tracks) resolves
    SERVER-side to per-lane artifact-id sets and pivots through the existing
    `?ids=` atom. The obvious-looking design —
    `comment_from`/`comment_to`/`session_from`/`session_to` on `/docs` **and**
    `/search` — is **8 params × 2 surfaces**, each one a four-file lock-step with
    goldens on each. The gallery grammar therefore gains **ZERO** atoms here.
    Degradation is explicit, never silent: a creation-only brush over the 500-id
    cap falls back to `galleryUrl(kb, {from, to})` (the mtime window already
    exists); a mixed brush disables the pivot and says why, driven by the
    server's per-lane `truncated` flags.
  - **Atlas lasso/selection and map-home** pivot through the same `?ids=` atom.
  - **Atlas search-dimming is NOT a filter at all** — the dim set is ephemeral
    component state, never a URL atom, because dimming and filtering are
    different operations: `ids=` removes rows, dimming must leave them in place.
    Do not "unify" them.

Corollary for future work: a new *view* over existing rows (a brush, a lasso, a
selection) is a client concern that ends in `ids=`; only a genuinely new
*predicate over the corpus* earns an atom, and earning one means touching
`galleryUrl.ts` + `fetchDocsPage` + `docs_query::matches` + `routes/docs.rs`
together, with goldens.

## Where to find things

| Need | Where |
|---|---|
| HTTP route handler | `crates/kb-server/src/routes/` |
| Middleware (auth, rate, scrub, origin) | `crates/kb-server/src/middleware.rs` |
| Router stack order | `crates/kb-server/src/router.rs` |
| Daemon state (KbHandles, KbContext) | `crates/kb-server/src/state.rs` |
| Storage actor + handle | `crates/kb-core/src/storage/actor.rs` |
| Gallery row-set memo + index-generation counter | `crates/kb-core/src/storage/actor.rs` (`index_generation`/`bump_generation`) + `crates/kb-server/src/state.rs` (`GalleryCache` + `KbContext.gallery_cache`) + `crates/kb-server/src/routes/docs.rs` (`gallery_snapshot`); `kb_core::docs_query::{matches,cmp_rows}` (see invariant #15) |
| `/facets` aggregate memo (SC3) | `crates/kb-server/src/state.rs` (`FacetsCache` + `KbContext.facets_cache`) + `crates/kb-server/src/routes/facets.rs` (`facets_snapshot`, rebuilds via `routes::docs::gallery_snapshot` so both memos share one `list_docs` scan) — same generation-keyed discipline as the gallery memo (invariant #15) |
| Lance schema + queries | `crates/kb-core/src/storage/lance.rs` |
| sqlite migrations + edges | `crates/kb-core/src/storage/sqlite.rs` + `crates/kb-core/migrations/` |
| HTML parser + extract_links | `crates/kb-core/src/parser.rs` |
| Atlas (UMAP + PCA + k-means) | `crates/kb-core/src/atlas.rs` |
| kb-comments/1 + fuzzy resolver + export | `crates/kb-core/src/review.rs` |
| Fine-grained comment HTTP handlers (add/reply/resolve/edit/delete + resolve-all, under `review_lock`) | `crates/kb-server/src/routes/comments.rs` |
| Review GET + export route | `crates/kb-server/src/routes/review.rs` (`get`, `post_export`) |
| Comment attachments — sniff/sanitize/manifest/GC/ref-rewrite (kb-core) | `crates/kb-core/src/attachments.rs` (`sniff_allowed`, `sanitize_filename`, `is_inline_image`, `Manifest`/`gc_plan`, `rewrite_attachment_refs`) + `Attachment` + mutations on `crates/kb-core/src/review.rs` + `paths.rs` (`kb_attachment_{dir,blob,manifest}`) + `[server.attachments]` in `config.rs` + `markdown::render_comment_fragment` (see invariant #18) |
| Attachment HTTP routes (stage/serve/adopt/detach) | `crates/kb-server/src/routes/attachments.rs` (`stage`/`serve`/`upload_to_comment`/`upload_to_reply`/`detach_*`); `attachment_ids` adoption + delete-GC in `routes/comments.rs`; `attachment_routes` sub-router (review limiter + the sole `DefaultBodyLimit`) in `router.rs` |
| Attachment CLI verbs | `crates/kb-cli/src/commands/comments.rs` (`upload`/`attach` + `--attach` on `add`/`reply` + `export --out-dir` bundle) |
| Attachment SPA (inline embeds + strip + composer upload + lightbox) | `web/src/components/{CommentBody,AttachmentStrip,ComposerAttachBar,AttachmentLightbox}.tsx` + `web/src/hooks/useComposerAttachments.ts` + `web/src/lib/attachmentUrl.ts` + `web/src/styles/attachments.css`; wired in `CommentsPanel`/`CommentModal`/`useReview`/`api/client.ts` |
| Static-share comment publishing (opt-in) | `crates/kb-core/src/share/mod.rs` (`inject_comments`/`render_comments_section`; `ShareOpts.include_comments`/`review_dir`/`attachments_root`) + `routes/share.rs` + `kb share --with-comments` + SPA `ShareModal` |
| Indexer pipeline + anchor lifecycle | `crates/kb-core/src/indexer.rs` |
| Post-upsert enrichment hooks (session → memory-recall-ledger → memory-link → edge → code-refs → snapshot → list-anchor) | `crates/kb-core/src/enrich.rs` (`EnrichmentHook` + `EnrichCtx` + `default_hooks`) — ordered, best-effort; invoked by `indexer.rs::finish_indexed_doc` after the batched `upsert_docs` commit (GC-B7). See kb-core CLAUDE.md invariant #5 |
| iframe injection (probe + annotator) | `crates/kb-core/src/iframe.rs` |
| Config schema (kb.toml) | `crates/kb-core/src/config.rs` |
| Fleet status sweep (daemons.toml fan-out) | `crates/kb-cli/src/commands/fleet.rs` (`kb fleet status`; identity/stats/open-errors per daemon) |
| Operator SSE tail (`kb events --follow`) | `crates/kb-cli/src/commands/events.rs` (wraps the `kb push` tail loop: Last-Event-ID reconnect + backoff) + `crates/kb-cli/src/sse.rs` |
| Request counter middleware | `crates/kb-server/src/middleware.rs::count_requests` + `state::RequestMetrics` |
| Metrics ticker (1Hz `metrics.tick` SSE) | `crates/kb-server/src/lib.rs::spawn_metrics_ticker` |
| Event→webhook bridge | `crates/kb-server/src/lib.rs::spawn_webhook_bridge` (daemon-wide `[webhooks]` subscriber → POST; shutdown-aware + joined in `teardown_tasks`) + `kb_core::config::WebhooksSection`. Extension model in `docs/extending.md` |
| Storage actor queue depth | `crates/kb-core/src/storage/actor.rs::StorageHandle::{queue_depth, queue_capacity}` + `pub const CHANNEL_CAPACITY` |
| CLI clap surface | `crates/kb-cli/src/main.rs` |
| CLI subcommand impls | `crates/kb-cli/src/commands/<verb>.rs` |
| SSE byte-stream parser | `crates/kb-cli/src/sse.rs` |
| Comments reply verb | `crates/kb-cli/src/commands/comments.rs` (`reply`) |
| Live comment watch loop (SSE scope + diff) | `crates/kb-cli/src/commands/comments_watch.rs` |
| SPA SSE facade (registries + aggregation) | `web/src/api/sse.ts` |
| SSE connection core (worker- or tab-hosted) | `web/src/sse/core.ts` + parser `web/src/sse/stream.ts` |
| SSE SharedWorker + tab transport + wire protocol | `web/src/workers/sse.worker.ts` + `web/src/sse/transport.ts` + `web/src/sse/protocol.ts` (see invariant #24) |
| SPA review hook | `web/src/hooks/useReview.ts` |
| Markdown composer (CodeMirror 6 — comments/replies/notes) | `web/src/components/MarkdownEditor.tsx` (orchestrator: Write/Preview/Split + toolbar) + `web/src/components/{CodeMirrorInput,EditorToolbar}.tsx` + `web/src/editor/{useCodeMirror,extensions,theme,commands,slash,livePreview}.ts` + `web/src/styles/editor.css` (container queries + `.cme__mirror` seam); call-sites `CommentsPanel.tsx`/`CommentModal.tsx`/`NoteEditor.tsx`; `ComposerAttachBar.tsx` exposes `open()` for the toolbar/slash 📎; e2e `tests/e2e/spa-editor.spec.ts` + unit `web/src/editor/commands.test.ts` (see invariant #22) |
| SPA atlas view | `web/src/components/AtlasView.tsx` |
| Annotator (in-iframe) | `web/src/scripts/annotate.ts` |
| History storage methods | `crates/kb-core/src/storage/sqlite.rs` (`history_*` impl block) |
| History HTTP routes (open/scroll/search/list) | `crates/kb-server/src/routes/history.rs` |
| In-iframe scroll runtime | `crates/kb-server/src/routes/artifact.rs` (`runtime_js`) |
| SPA history client + timeline view | `web/src/api/history.ts` + `web/src/components/HistoryTimeline.tsx` |
| Path-permalink resolver (by source-relative path) | `crates/kb-server/src/routes/docs.rs` (`get_by_path`, route `docs/by-path/{*path}`) |
| SPA permalink helper (`/a/<kb>/<path>` + `?p=`) | `web/src/lib/artifactHref.ts` |
| SPA path→id resolution + iframe origin | `web/src/routes/detail.tsx` (`fetchDocByPath`, `artifactOrigin`) |
| Share engine (resolve→stage→scrub→links) + backend | `crates/kb-core/src/share/{mod,host,assets}.rs` |
| Cloudflare client (Pages Direct Upload + Access) | `crates/kb-core/src/share/cloudflare.rs` (`asset_hash`, `gate_to_include_rules`) |
| GitHub Pages client (public lane) | `crates/kb-core/src/share/github.rs` |
| Share config (`[share.*]`) + env-secret read | `crates/kb-core/src/config.rs` (`ShareSection`, `read_secret_env`) |
| Share registry (`shares_*` + V0004) | `crates/kb-core/src/storage/sqlite.rs` + `crates/kb-core/migrations/V0004__shares.sql` |
| Share HTTP routes | `crates/kb-server/src/routes/share.rs` |
| Share CLI verb | `crates/kb-cli/src/commands/share.rs` (`Cmd::Share` in `main.rs`) |
| SPA Share button + modal | `web/src/components/chrome/ContextBar.tsx` + `web/src/components/ShareModal.tsx` + `web/src/api/client.ts` (`createShare`/`listShares`/`revokeShare`) |
| Agent-memory rerank + `render_artifact` | `crates/kb-core/src/memory.rs` (pure `rerank` on rank-position×salience×decay; shared HTML builder) |
| Recall fan-out route (`GET /api/memory/recall`) | `crates/kb-server/src/routes/memory.rs` (scope resolution, embed-once-per-model, tombstone scan) |
| Context pack (`GET /api/context`, CT-D1) | `crates/kb-server/src/routes/context.rs` (the ONE context assembler — composes `memory::recall_compose` + `sessions::recollect_compose` + `inbox::collect_open` + `code_refs_of`; pure `fit_lane`/`lane_budget`/`scent_line` budget core) + `crates/kb-cli/src/commands/context.rs` (`Cmd::Context`, pure `render_human`) + `plugins/kb-memory/hooks/kb-recall.sh` (the turn-1 SCENT branch + `tests/test-recall-scent.sh`) + `plugins/kb-memory/skills/kb-distill/SKILL.md` (Step 5 arm 1 / Step 8 visibility). **Both engines were split out of their axum handlers for this** (`recall`/`recollect` are now thin `Ok → Json` wrappers) — extend those `*_compose` fns, never fork a second copy of the fan-out |
| Memory ingest + forget routes | `crates/kb-server/src/routes/artifacts.rs` (`POST` write-only + collision-safe slug; `DELETE`) |
| Supersede tombstone scan | `crates/kb-core/src/storage/lance.rs` (`list_supersede_targets`) + `actor.rs` (`ListSupersedeTargets`) |
| Memory CLI verbs | `crates/kb-cli/src/commands/memory.rs` (`Cmd::{Remember,Recall,Forget}` in `main.rs`) |
| SPA `/memory` view | `web/src/routes/memory.tsx` + `hooks/useMemories.ts` + `components/MemoryActions.tsx` |
| Claude Code plugin marketplace | `.claude-plugin/marketplace.json` (lists `kb-memory` + `kb-research` + `kb-comments`) — each self-contained under `plugins/<name>/`. The repo is a *marketplace*, not a single root plugin. |
| Claude Code memory hooks (LLM-free) + plugin | `plugins/kb-memory/` — `hooks/` (`kb-recall`/`kb-wake`/`kb-capture`.sh + `hooks.json` + `CLAUDE.memory.md`) + `.claude-plugin/plugin.json` |
| Claude Code research/comments plugins | `plugins/kb-research/` (`kb-artifact` authoring skill + `/kb-tools` command) + `plugins/kb-comments/` (`/kb-comments` + `/kb-comments-watch` commands). Packaging only — every command drives the `kb` CLI; see `docs/extending.md`. |
| Redesign chrome (Header / QueryRibbon / Sidebar / StatusBar / ContextBar) | `web/src/components/chrome/{Header,QueryRibbon,Sidebar,StatusBar,ContextBar,HotkeyRoot}.tsx` + `web/src/styles/{tokens,chrome}.css` |
| Design tokens + Inter Tight / JetBrains Mono / Source Serif 4 | `web/src/styles/tokens.css` |
| Atlas inspector + regions strip + canvas refinements | `web/src/components/{AtlasInspector,AtlasView}.tsx` |
| PreviewInspector right rail + TOC mini-spy | `web/src/components/{PreviewInspector,TocSpy}.tsx` + runtime `kb:toc`/`kb:section` emit in `crates/kb-server/src/routes/artifact.rs::runtime_js` |
| Anchor corkboard | `crates/kb-core/src/corkboard.rs` + `crates/kb-core/migrations/V0005__corkboard.sql` + `crates/kb-server/src/routes/anchors.rs` + `web/src/{routes/anchors.tsx,hooks/useAnchors.ts}` |
| Reading lists | `crates/kb-core/src/lists.rs` + `crates/kb-core/migrations/V0015__lists.sql` (V0016 drops the old bookmarks table) + `storage/sqlite.rs` (`list_*`) + `crates/kb-server/src/routes/lists.rs` + `crates/kb-core/src/enrich.rs` (`ListAnchorHook`) (replaces v0.13 bookmarks; orthogonal to corkboard and to artifact-authored `<meta name="kb-status">`) |
| Sessions | `crates/kb-core/src/sessions.rs` + `crates/kb-core/migrations/V0008__sessions.sql` + `storage/sqlite.rs` (`sessions_*`, compound keyset cursor `(before, before_id)`) + `kb_session` lance column on every projection + `crates/kb-server/src/{routes/sessions.rs,touches_cache.rs}` + `crates/kb-cli/src/commands/sessions.rs` + SPA `web/src/{routes/sessions.tsx,hooks/useSessions.ts,api/sessions.ts,lib/sessionColor.ts,styles/sessions.css}` (semantics: invariant #11) |
| Live-follow (W7, R15) — tailer + resolver | `crates/kb-core/src/sessions/tail.rs` (`TailReader::read_delta`, `resolve_live_transcript`, `read_bootstrap_window`) + `SessionsSection` (`crates/kb-core/src/config.rs`, `[sessions]`) + `sessions::view::{view_bootstrap,view_append,ViewCarry::stats}` |
| Live-follow — routes + server-side cache | `crates/kb-server/src/routes/sessions.rs` (`presence`, `live`, `build_live_delta`, `locate_capture_cwd_and_ended`) + `crates/kb-server/src/live_tail_cache.rs` (`(sid,inode)`-keyed `ViewCarry` LRU, never load-bearing) + `router.rs` (`/sessions/presence` static, `/sessions/{sid}/live`) (semantics: invariant #11's live-lane corollary) |
| Live-follow — CLI | `crates/kb-cli/src/commands/session_read.rs` (`run_live`, `follow_raw_loop`, `follow_interpreted_loop`, `live_wire_json`, `live_not_found_hint`) — `kb sessions read --live [--follow]`, direct-disk, zero daemon |
| Live-follow — SPA | `web/src/hooks/useLiveTail.ts` (ref-accumulated poll loop, NOT a TanStack entry) + `web/src/components/reader/LiveTailPanel.tsx` (the ticker) + `web/src/lib/sessionPresence.ts` (Tier-0/Tier-1 derivation) + `web/src/hooks/useSessions.ts::useSessionPresence` (30s poll, `["sessions","presence"]`) + follow-mode/handoff wiring in `web/src/components/reader/ArtifactPane.tsx` + the Follow chip in `web/src/components/reader/SessionContextCard.tsx` |
| Notes / todo-lists | `crates/kb-core/src/notes.rs` (pure: paths/compose/split/toggle/append/counts + `NoteSummary`) + `parser.rs` (`task_done`/`task_total` counts) + additive `task_*` lance columns (`storage/{schema,lance}.rs` lance-schema-v17 migration + `list_notes` + `StorageMsg::ListNotes`) + `crates/kb-server/src/routes/notes.rs` (CRUD + `/toggle` + `/tasks` + `note.*` SSE) + `docs_query.rs::DocsQuery.exclude_categories` + `crates/kb-cli/src/commands/notes.rs` + SPA `web/src/{routes/notes.tsx,hooks/useNotes.ts,api/notes.ts,components/{NoteMarkdown,NoteEditor,NotesPanel}.tsx,styles/notes.css}` + native render in `routes/detail.tsx` + gallery rail in `routes/gallery.tsx`; e2e `tests/e2e/spa-notes.spec.ts` (semantics: invariant #16) |
| Reading-progress | `crates/kb-core/src/reading.rs` (pure classify / summarize) + `crates/kb-core/migrations/V0014__reading_sections.sql` + `storage/sqlite.rs` (`reading_*`, `history_opens_in_window`, purge wiring) + `storage/actor.rs` (`Reading*` msgs — no `bump_generation`) + `[kb.*] reading_progress` (`config.rs` → `KbContext.reading_progress`) + capture **and** the back-button interceptor in `crates/kb-server/src/routes/artifact.rs::runtime_js` + `routes/history.rs` (`post_reading`, `get_reading`, seed-on-open `OpenResponse.reading`) + recall enrichment `routes/memory.rs` + session readings `routes/sessions.rs::readings` + `plugins/kb-memory/hooks/kb-recall.sh` + `crates/kb-cli/src/commands/reading.rs` + SPA `web/src/{api/reading.ts,hooks/useReading.ts,components/{TocSpy,PreviewInspector}.tsx,routes/{detail,sessions}.tsx}`; e2e `spa-backbutton.spec.ts` + `spa-reading-progress.spec.ts` (semantics: invariants #19, #20) |
| Query DSL parser + ribbon edit | `crates/kb-core/src/query.rs` (Pratt-style; Expr/Atom/Key; `parse`/`to_docs_query`/`from_docs_query`/`to_url_params`); SPA `web/src/components/chrome/QueryRibbon.tsx`; docs route `?q=` in `crates/kb-server/src/routes/docs.rs` (response carries `ms` + `query_warnings`) |
| Saved queries (localStorage only) | `web/src/hooks/useSavedQueries.ts` + popover in Header's `SavedQueriesButton`. Daemon-side sync (a migration + per-kb sqlite table) is still open. |
| Memory pin + DecayPolicy | `crates/kb-core/src/memory.rs` (`DecayPolicy::{Strict,Balanced,Loose}::drop_threshold` + `rerank_with_policy` + `RecallHit.pinned`) + `crates/kb-core/migrations/V0006__pinned_memories.sql` + `crates/kb-server/src/routes/memory.rs` pin/unpin POST/DELETE + `/api/memory/policy` GET/PUT; SPA table in `web/src/routes/memory.tsx` |
| Memory decay-policy persistence | `crates/kb-server/src/state.rs::{load_memory_policy, save_memory_policy}` → `<state>/memory-policy.json` (path helper `KbPaths::memory_policy_file` in `kb_core::paths`) |
| Cross-cutting hotkeys + KeyHelp overlay | `web/src/components/chrome/HotkeyRoot.tsx` (g-chord 800ms; `j/k/g a/g g/g m/g h/g s/?/Esc`; KeyHelp modal inlined) |
| Cmdk command rows | `web/src/components/Cmdk.tsx` (`COMMANDS` array; substring-match against label/hint; commands first then search hits) |
| StatusBar live request rate | `web/src/components/chrome/StatusBar.tsx` subscribes to `metrics.tick` SSE; pulse flips `is-active` purple with `kb-pulse` keyframe animation |
| Meta editors (edit tags/category in source) | `crates/kb-core/src/meta_edit.rs` (HTML `<meta>` splice) + `crates/kb-core/src/markdown.rs::set_frontmatter_field` + `parser::slugify_tag`; route `crates/kb-server/src/routes/artifacts.rs::patch_meta` (`PATCH …/artifacts/{id}/meta`); SPA editor in `web/src/components/PreviewInspector.tsx` + `web/src/api/client.ts` (`updateArtifactMeta`) + refetch-on-`artifact.indexed` in `web/src/routes/detail.tsx` (semantics: invariant #12) |
| Inspector backlinks list | `web/src/components/PreviewInspector.tsx` (`buildBacklinks` from incoming `link` edges) |
| Config validate + comment-preserving save | `crates/kb-core/src/config.rs` (`validate`→`ValidationIssue`, `save_preserving`→`merge_changes`/`item_value_differs`, `write_atomic`; `toml_edit` dep) (semantics: invariant #13) |
| Daemon restart loop + teardown | `crates/kb-server/src/lib.rs` (`ServeOutcome`, `serve_loop`, single-shot `serve_with_paths`, `bind_with_retry`, `teardown_tasks`, watch-aware `shutdown_signal`); `crates/kb-server/src/state.rs` (`KbHandles.{config,config_path,restart_requested,terminate}` + `with_config`); callers `crates/kb-cli/src/commands/daemon.rs` + `crates/kb-server/src/main.rs` |
| Config GET/PUT route | `crates/kb-server/src/routes/config.rs` (GET running config + env presence + model registry; PUT validate→test-bind addr→`save_preserving`→`daemon.restarting` SSE→restart) + `router.rs` `/config` |
| SPA config editor | `web/src/routes/settings.tsx` (Config tab) + `web/src/components/settings/DaemonConfig.tsx` (save→restart→reconnect via `/api/identity` `started_at` poll; addr-change `ConfirmModal` + new-origin link) + `web/src/components/settings/config/{fields,sections}.tsx` + `web/src/api/config.ts` (`mergeConfig` deep-merge; `ConfigValidationError`) + `web/src/api/sse.ts` (`daemon.*`) |
| Versions/Diff — git bridge + diff engine | `crates/kb-core/src/vcs.rs` (`find_git_root`, `git_log`/`git_show` shell-out, `VersionsMode`, `diff_lines` via `similar`) + `parser::text_blocks` (block-structured prose for diffing) (semantics: invariant #14) |
| Versions/Diff — snapshot store + unified façade | `crates/kb-core/migrations/V0013__artifact_snapshots.sql` + `storage/sqlite.rs` (`snapshot_*`) + `storage/actor.rs` + `enrich.rs` (`SnapshotCaptureHook` + `DEFAULT_SNAPSHOT_KEEP`) + `crates/kb-core/src/versions.rs` (`Version`/`VersionCtx::{list,raw_at,prose_at}`) |
| Versions/Diff — HTTP route + CLI + SPA | `crates/kb-server/src/routes/versions.rs` (`/artifacts/{id}/versions` + `/diff`) + `KbContext.{git_root,versions_mode}` (state.rs, memoised at `bring_up_kb`) + `crates/kb-cli/src/commands/versions.rs` (`kb versions`/`kb diff`) + `web/src/{api/versions.ts,hooks/useVersions.ts,components/VersionsPanel.tsx,styles/versions.css}` + ContextBar `data-kb-act="versions"` |
| Reader chrome action-model (v0.20) | `web/src/components/chrome/ContextBar.tsx` (artifact verbs + fullscreen) + `web/src/routes/detail.tsx` (ReaderDock `dock-*` pills, `o`/immersive) + `web/src/components/PreviewInspector.tsx` (`.kb-pinsp__icons` rail) + `web/src/hooks/useInspectorTab.ts` (semantics: invariant #30) |
| Resilience: confirm/toast/error-boundary (v0.20) | `web/src/components/ConfirmProvider.tsx` (`useConfirm`) + `web/src/components/settings/ConfirmModal.tsx` (optional `expectedToken`) + `web/src/lib/toast.ts` + `web/src/components/{Toasts,ErrorBoundary}.tsx` (semantics: invariant #32) |
| Scroll restoration (v0.20) | `web/src/hooks/useScrollRestoration.ts` + `web/src/lib/lastGalleryUrl.ts` (gallery records, ContextBar back replays) + `history.scrollRestoration="manual"` in `web/src/main.tsx` (semantics: invariant #31) |
| kb-select cold-seed + daemon-down (v0.20) | `web/src/api/prefs.ts` (`lastKb`, `loadLastKb`/`saveLastKb`) + `web/src/app.tsx` (cold-seed) + `web/src/hooks/useActiveKb.ts` (`useExplicitKb`/`useActiveKb`) + `web/src/components/chrome/DaemonDownBanner.tsx` (semantics: invariant #33) |
| Lazy CodeMirror at composer mount (v0.20) | `web/src/components/LazyMarkdownEditor.tsx` (React.lazy wrapper, forwardRef-preserving) — imported by `CommentsPanel`/`CommentModal`/`NoteEditor`; `editor.css` ships in the `MarkdownEditor` lazy chunk (was eager in `main.tsx`) |
| Permalink OpenGraph meta (v0.20) | `crates/kb-server/src/routes/spa.rs` (`serve_artifact_shell`/`artifact_meta_tags`/`inject_head_meta`/`escape_html_attr`) (semantics: invariant #34) |
| Quick capture — engine (staged uploads, provenance stamping, opt-in sanitize) | `crates/kb-core/src/capture.rs` (filename policy + collision loop, md front-matter / html head-splice stamping, `ammonia` sanitize profile, url/text stub builder, id derivation shared with the indexer — #27) + `[server.capture]`/`capture_dir` in `crates/kb-core/src/config.rs` |
| Quick capture — HTTP routes | `crates/kb-server/src/routes/capture.rs` (`POST /api/kb/{kb}/capture` + share-target `POST /capture`, shared `do_capture` core) + `router.rs` (`capture_routes` body-limit sub-router; `route_layer`'d auth on the outside-`/api` share-target route — #4) |
| Quick capture — CLI verb | `crates/kb-cli/src/commands/capture.rs` (`kb capture [FILES.. \| -] --kb --title --tags --sanitize --url --text --name`) |
| Quick capture — SPA sheet + share-target landing | `web/src/{api/capture.ts,components/CaptureSheet.tsx}` (picker/drag-drop/paste, kb picker, sanitize toggle) + `web/src/lib/capturedParam.ts` (`?captured=` parser, consumed by `app.tsx`'s one-shot toast) + Cmdk/drawer entries dispatching `kb:capture.open` + `web/public/manifest.webmanifest`'s `share_target` |
| Slate engine (kb-slate/1: types, validation, `project`/`project_append`/`project_finish`, liveness, displaced/nudge) | `crates/kb-core/src/slate.rs` + `crates/kb-core/src/paths.rs` (`slate_dir`/`slate_ledger_file`/`slate_meta_file`/`slate_archive_file`) + `crates/kb-core/tests/slate_fixtures/*.jsonl` (see invariant #6's SLATE amendment) |
| Slate HTTP routes + per-slate lock + `slate.updated` SSE | `crates/kb-server/src/routes/slates.rs` + `crates/kb-server/src/slate_registry.rs` (`SlateRegistry`, the `LiveRegistry` precedent) + `KbHandles::slate_lock_for` in `state.rs` |
| Slate CLI verbs (`kb slate open/take/found/drop/edit/mark/pin/promote/…`) | `crates/kb-cli/src/commands/slate.rs` (see its module doc — the CLI never renders a digest, exit codes 1/2/3, the client-side cursor/topic markers) + `Cmd::Slate`/`SlateAction` in `main.rs` |
| Slate hook wiring (session-start hybrid block, per-prompt delta, protocol paragraph) | `plugins/kb-memory/hooks/{kb-wake.sh,kb-wake-kimi.sh,kb-recall.sh,kb-omp.ts,memory-protocol.txt,CLAUDE.memory.md}` + `plugins/kb-memory/hooks/tests/test-*-slate.sh` |
| Slate tidy/distill skills | `plugins/kb-memory/skills/{kb-slate-tidy,kb-slate-distill}/SKILL.md` |
| Slate operator doc | `docs/slate.md` |
| Slate dispatcher bridge (digest-in / take-on-spawn / harvest-out, all five job-exit paths) | `~/project/grokclaude` (`src/engine.rs`: `with_kb_slate_digest`, `post_kb_slate_take`, `trigger_kb_slate`/`trigger_kb_slate_abandoned`; `src/lib.rs`'s `reap`) + `plugins/kb-memory/hooks/kb-slate-harvest.sh` (kb-side adapter) + `plugins/kb-memory/hooks/tests/test-slate-harvest.sh` |

## Common pitfalls

- **Never call kb-code's `Store` inline from async context** (2026-08-31 prod
  incident, three consecutive v0.40 deploy rollbacks): an inline `Store` call
  blocks a tokio async-worker thread for its whole mutex-wait + query; under
  a sink reconcile burst enough concurrent store-touching probes saturate ALL
  workers and the runtime can't poll ANYTHING — `/healthz` included — while
  the process sits at near-zero CPU (worker starvation, not deadlock). Every
  async-context call goes through `store::StoreBlocking::run_blocking` (the
  blocking-pool hop); and the conn mutex is `parking_lot::Mutex`, because
  std's unfair handoff let the sink's per-file lock loop starve a parked
  reader for an entire burst (observed: `/api/identity` >90 s while single
  sink messages were ~11 s). Full narrative: `store.rs` module doc; the
  regression pin is `kb-code-server/tests/starvation.rs`. Trigger to
  remember: `~/project/kb` is bind-mounted into prod kb-code, so an agent
  session editing this repo IS a live reconcile burst on kbc.example.com.
- **Raw-string `#` collisions in HTML test fixtures**: `r#"..."#` ends at the
  first `"#` — embedded `<a href="#section">` aborts the string. Use `r##"..."##`
  for HTML fixtures with `#` in attributes.
- **clippy `items_after_test_module`**: code after `#[cfg(test)] mod tests` trips
  this. New `pub fn`s belong before the test module.
- **`format!` arg-passing to `impl Into<String>`**: passing `&format!(...)` trips
  `clippy::needless_borrows_for_generic_args`. Drop the `&`.
- **Doc-comments on function args**: not allowed. Move to the function-level doc
  with a "param notes" prose section.
- **`cargo fmt` rewrites widely**: run before each commit. A cleanup commit at
  each milestone end captures fmt-only changes.
- **Concurrent cargo jobs corrupt the incremental target dir**: running
  `cargo test` + `cargo clippy` (or two `cargo build`s) simultaneously can
  produce phantom `E0063 "missing field …"` errors on a struct you can see is
  fully populated. Run Rust checks SERIALLY; a clean re-run passes. (Also: a
  `cmd | tail` pipe reports `tail`'s exit, not cargo's — and zsh populates
  `$pipestatus`, not `$PIPESTATUS`.)
- **Gating a "connection lost" UI on the aggregate SSE phase flashes on load**:
  `AggregatedStatus.phase === "idle"` means BOTH "connected & quiet" AND "never
  connected yet" (empty snapshots), and `connectOne` emits a `connected:false`
  snapshot synchronously on start. Gate a daemon-down banner on a per-daemon
  signal (`daemons.some(d => d.phase !== "disconnected")`), not the aggregate
  phase (see `DaemonDownBanner`, invariant #33).
- **Test fixture corpus has no kb-prompt template**: scrub-strip tests must drop a
  fixture HTML file with `<template id="kb-prompt">` themselves (see
  `boot_with_outbound_and_promptful_artifact`).
- **Auth tests need `X-Forwarded-For` to bypass loopback**: 127.0.0.1 + no XFF =
  loopback bypass. To exercise the auth path, send `X-Forwarded-For: 8.8.8.8`.
- **Middleware on the `/api` router sees NEST-STRIPPED paths**: `count_requests`
  (and any `.layer` on `api` inside `.nest("/api", api)`) receives
  `req.uri().path()` WITHOUT the `/api` prefix — e.g. `/kb/canon/docs`, not
  `/api/kb/canon/docs`. Path classifiers must normalise both forms
  (`path.trim_start_matches('/')` then strip an optional `api/`), as
  `classify_route` + `kb_from_path` now do. The TM-track surfaced a latent bug
  here: the original `classify_route` only stripped a literal `/api/`, so every
  per-route request counter except the `Other` catch-all silently read zero. Unit
  tests pass full `/api/...` paths and won't catch this — assert the stripped form
  too.
- **`KB_TEST_INDEX_TIMEOUT_SECS` — the "seed a corpus, then wait for the
  watcher/indexer" test deadline (MI test-hardening, 2026-08)**: `kb-server/
  tests/common/mod.rs` and `kb-cli/tests/common/mod.rs` (`poll_until`/
  `poll_until_sync`, one copy per crate — integration-test binaries can't
  share code across crates) replace every hardcoded 10s poll deadline (and
  every bare fixed-`sleep`-then-single-shot-query) with one idiom: poll with
  geometric backoff (50ms → capped 500ms) against a deadline that scales
  from this env var (default 30s), panicking with a message naming WHAT was
  awaited and for how long. Five test families false-red under host I/O
  contention with the old hardcoded-10s/bare-sleep shape before this fix:
  `atlas_backfill.rs`/`atlas_points_memo.rs` (a DIFFERENT root cause — see
  below), `notes_cli_links_and_backlinks`/`notes_cli_round_trip_against_
  daemon`, `cat_read.rs`'s `cat_offline_flag_*`/`cat_via_daemon_*`/
  `cat_record_flag_*`, and `api_artifact_route_scrubs_on_non_loopback`. Bump
  the env var on a slower/more contended box rather than hand-editing a call
  site or a hardcoded constant. The atlas test files' failure mode is
  SEPARATE and not fixed by this knob: `atlas_backfill.rs`/`atlas_points_
  memo.rs` are this crate's heaviest lance/datafusion consumers (full corpus
  scan + PCA per call) and have produced lance/datafusion memory-pool
  exhaustion when run concurrently — `common::atlas_lance_lock()` (a
  per-binary `tokio::sync::Mutex`, held for the whole test body) serializes
  them against each other instead; raising a timeout does nothing for that
  failure mode. **Same root cause, kb-core's own unit tests (2026-08
  follow-up):** seven `kb_core::atlas::tests` functions (in-crate, not an
  integration-test binary, so they can't share `common::atlas_lance_lock` —
  see `atlas.rs`'s `lance_heavy_test_lock`) hit the identical lance/
  datafusion `FairSpillPool` exhaustion; same fix shape (a plain
  `tokio::sync::Mutex`). The module comment there spells out why the
  shortfall is byte-CONSTANT (a fixed `sort_spill_reservation_bytes`-derived
  per-partition reservation, not data-volume-dependent) and why the fix
  isn't instead `LANCE_BYPASS_SPILLING=1` (kb-cli's `main()` already sets
  this for production, but setting it from `cargo test` code would call the
  now-`unsafe` `std::env::set_var` from a binary that runs many tests
  concurrently on separate OS threads — unlike `main()`, which is guaranteed
  single-threaded at that point). Honesty note: the race did NOT reproduce
  locally across 8 attempts (`--test-threads=1`, default ×6, `--test-
  threads=32`, and the full 1659-test suite) — it's a genuine low-probability
  scheduling race, plausibly more likely under CI's cgroup-throttled
  containers than an idle dev box, and the fix (removing the concurrent-
  access precondition) doesn't depend on reproducing it. **Also 2026-08:**
  `notes_cli_links_and_backlinks` resurfaced with a THIRD, different failure after the
  `poll_until` fix above — not a poll timeout but `kb notes new`'s own
  outbound HTTP request timing out, because the CLI's per-call timeouts are
  literals hardcoded at each `client_with_timeout_and_bearer` call site
  (5-600s) with no env override. Fixed the same way: a narrowly-scoped
  test-only override, `KB_TEST_HTTP_TIMEOUT_SECS`, read inside
  `client_with_timeout_and_bearer` (`kb-cli/src/http.rs`) and passed to the
  spawned `kb` subprocess via `Command::env(...)` from `kb-cli/tests/common/
  mod.rs::http_timeout_secs()` — a per-child-process env var, so (unlike the
  atlas case above) there's no multi-threaded `set_var` hazard on either
  side. Production's default timeouts are untouched since the var is never
  set outside a test harness.
- **The reconcile backstop is blind to a SAME-SECOND rewrite (2026-08,
  `memory_*` census family)**: `memory_salience_patch_round_trips_and_
  survives_reindex` and `memory_forget_soft_then_purge_round_trip` false-red
  on the CI runners — three runs, ending in a 120s "timed out … waiting for:
  census to reflect the patched salience" with the widened knob already
  applied, i.e. 120s of an IDLE pipeline (the daemon answered every census
  poll throughout, so storage + runtime were healthy — nothing was ever
  EMITTED for the changed file). Mechanism, and the reason a *faster* disk
  makes it worse: both of those tests have the daemon rewrite a seeded source
  in place (`kb_core::memory::set_salience` / the soft-forget tombstone, via
  `fsx::write_atomic`) and then wait for the reindex. If the seed and that
  rewrite land in the SAME wall-clock second — the seed→boot→patch path is
  sub-second on NVMe, >1s on this HDD box, which is exactly why it passes
  locally — then the reconcile pass's G5 producer-side dedup skips the file
  FOREVER: `kb_core::indexer::walk_core` compares `disk_mtime ==
  Some(stored)` with `.as_secs() as i64` on BOTH sides, so a same-second
  rewrite is indistinguishable from an untouched file and every later pass
  re-stats the same second and skips again. `[indexer] watch_mode = "poll"`
  is NOT an escape hatch here: `notify`'s `PollWatcher` truncates mtime the
  same way (`system_time_to_seconds`, poll.rs) and only consults a content
  hash under `compare_contents`, which the daemon doesn't enable — so the
  live watcher (native fs events) is the ONLY thing that can heal such a
  rewrite, and if that one event is missed the test waits out its whole
  deadline. What the runner misses is narrower than "inotify is broken
  there": `l6_link_mutation_emits_memory_linked_sse` is green in the same CI
  job and needs a live event for a post-boot file CREATE (`POST …/artifacts`
  → plain `std::fs::write`, artifacts.rs), whereas both failing tests mutate
  through `fsx::write_atomic`, i.e. a RENAME over an already-watched,
  already-indexed path (`MOVED_FROM`+`MOVED_TO` folded through
  notify-debouncer-full's file-id cache). Test-side fix: `boot_memory_corpora` backdates
  every seeded file's mtime (`seed_memory_file`, 10s) so any post-boot
  rewrite is strictly newer in whole seconds, and sets `reconcile_secs = 5`
  so the pass heals it well inside `index_wait_deadline()` — the family no
  longer depends on live fs events on any runner. **Open, product-side:**
  the same second-granularity gap means a real user's rewrite-within-a-second
  of an indexed file is invisible to the "self-healing" reconciler if the
  watcher also misses it. The precedented fix is git's *racily-clean* rule —
  never dedup a file whose mtime is in the CURRENT second (`disk_mtime ==
  stored && disk_mtime < now_unix()`), costing at most one extra
  content-hash-deduped emission per file touched in the second a pass runs.
  Not shipped here (it touches the v0.24 SC1 storm fix's hot path and wants
  its own change + bench).

## Dev-environment tips

- **Mold linker** (optional, machine-local — put in `~/.cargo/config.toml`,
  NOT the repo, since a repo-level config would apply to CI too and a runner
  without mold installed would break):
  ```toml
  [target.x86_64-unknown-linux-gnu]
  rustflags = ["-C", "link-arg=-fuse-ld=mold"]
  ```
  No `clang` needed if gcc ≥12.1 (supports `-fuse-ld=mold` natively — check
  `gcc --version`). Cuts the kb-cli link ~60s → ~5s. **sccache was tried and
  REMOVED (2026-07-30)**: three build-blocking wedges in one day
  (permission-denied `.d` writes) for a measured 10–20% benefit on this
  disk-bound box — don't re-add it without new evidence; the wrapper line
  in older copies of this tip is the stale part.
- **Dev debuginfo is `line-tables-only`** (`[profile.dev]` in Cargo.toml,
  PF-B1): full debuginfo grew `target/debug` to 224 GB on the disk-bound dev
  box and once SIGBUS'd the CI linker. Backtraces keep file:line; only
  debugger variable-inspection fidelity degrades. If you flip it back
  locally, do NOT reintroduce the old `CARGO_PROFILE_DEV_DEBUG` env in CI —
  one source of truth. A one-time `cargo clean --profile dev` (or deleting
  `target/debug`) reclaims the historical bloat after pulling PF-B1.
- **Per-commit rebuilds are broken by `kb-buildstamp`** (PF-B1): the ONLY
  build.rs watching `.git/HEAD` lives there. Keep it that way — a lib that
  links kb-buildstamp (or any new build.rs with a `.git` rerun trigger)
  silently re-cascades the every-commit rebuild of kb-server + kb-cli +
  kb-code-server + kb-code-cli that PF-B1 removed. Bins only.
- **cargo-nextest** (`cargo install cargo-nextest --locked` — don't pull it
  from a distro package manager if that would drag in a second, conflicting
  Rust toolchain package; check whether the active `cargo`/`rustc` are
  rustup-managed first) reports one line per failing test instead of cargo
  test's opaque *"error: N targets failed"*, and gives `--retries` for known
  flaky tests (e.g. lance-backed tests under concurrent load).
- A Bash/shell timeout kill on `cargo build` terminates the top-level cargo
  process but does not reliably kill rustc children it already spawned — they
  can keep compiling into the target dir after the parent is reported dead.
  Harmless if left alone, but don't start a *new* concurrent build against the
  same target dir until `ps`/`fuser` confirms they've exited (same corruption
  class as running two cargo invocations against one target dir at once).
