# kb — Claude Code project guide

A Rust daemon that indexes, searches, and serves a personal collection
of LLM-generated HTML artifacts. Ships a CLI and a single-page web UI; runs
many daemons side-by-side (fleet monitoring = SPA Settings + `kb fleet
status` + `kb events --follow`); uses BM25 + vector embeddings (bge-small)
for hybrid search.

This file orients future Claude sessions — **keep it lean**. Read
[README.md](README.md) for the project front door,
[docs/http-api.md](docs/http-api.md) + [docs/cli.md](docs/cli.md) +
[docs/kb-code.md](docs/kb-code.md) for the user-facing surface (HTTP API, CLI
verbs, the sibling daemon) and
[docs/research/index.html](docs/research/index.html) for the design rationale. The
**full architecture invariants, the file-by-file code map, and the Rust/build
pitfalls** live in
[docs/architecture-invariants.md](docs/architecture-invariants.md) (indexed below
— read the matching entry before changing a load-bearing subsystem). kb-core-only
invariants (lance schema, storage actor, atlas determinism, embedding dim per-kb)
live in [crates/kb-core/CLAUDE.md](crates/kb-core/CLAUDE.md); kb-code-server-only
invariants (its own security posture, git-argv discipline, the cmd/1 and
kbc-theme/1 registries) live in
[crates/kb-code-server/CLAUDE.md](crates/kb-code-server/CLAUDE.md), and
web-code's own (the keyboard-dispatch contract above all) live in
[web-code/CLAUDE.md](web-code/CLAUDE.md). The rest of `docs/`
covers surface specifics — deployment ([self-host.md](docs/self-host.md),
[configuration.md](docs/configuration.md)), phase-by-phase shipping notes
([spike-findings.md](docs/spike-findings.md)), web UI, authoring, and the
comment workflow.

## Layout

```
crates/
  kb-core/         lib — storage, indexer, embed, watcher, ids, types,
                       parser, atlas, review, iframe, events, config, paths
  kb-server/       lib + bin — axum daemon (api/* + ServeDir + artifact subdomains)
  kb-cli/          bin (`kb`) — daemon, search, model, add, comments, atlas,
                       exclude, events, fleet, …; v0.38 adds the
                       connective-tissue verbs: `context` (the ONE budgeted
                       pack), `why-memory`, `memory {flag,expand,recalled-by}`,
                       `links {suggest,apply}`, `slo {status,snapshot,log}`,
                       `doctor --hooks`, `refs --by-target/--gallery`,
                       `versions --at`, `daycard --since`; v0.41 adds the
                       `slate` verb family (SL3) — the per-project
                       blackboard's protocol: `open` (the digest, printed
                       from the daemon's own `text`; the CLI NEVER renders
                       one), `show`/`delta`/`history`, the twelve kinds
                       (`now`/`warn`/`take`/`done`/`hand`/`ask`/`answer`/
                       `found`/`idea`/`tried`/`drop`/`mark`) plus the
                       `edit`/`pin`/`unpin` sugars, `promote --to
                       memory|note|plan`, `close`/`reopen`/`rotate`,
                       `watch` (SSE `?filter=slug:`), `stats`, `ls`,
                       `doctor`. Exit 3 = refused (`slate-taken` or
                       `slate-live-author`); the cursor
                       (`~/.cache/kb/slate-cursor-<sid>`) and topic marker
                       are CLIENT state written only by `open`/`delta`.
  kb-embedder/     bin — embed + rerank sidecar subprocess (`--model`/`--reranker`;
                       spawned by the daemon). The ONLY crate that links ONNX
                       Runtime (statically, via the kb-core `local-embedder`
                       feature). Excluded from the default `--workspace` build/CI so
                       kb/kb-server stay ORT-free, fast to link, and portable.
  kb-code-server/  lib + bin (`kb-code-server`) — SEPARATE sibling read-oriented
                       code-browsing daemon (design v4 "The read-first IDE" →
                       v2 "Operable Reader" → v3.0 "The Full Instrument",
                       research kb corpus). Live-mirrored index + tiered
                       tree-sitter extraction, Search Everywhere, provenance
                       (incremental blame + session↔commit join + why/story),
                       annotations, confirmed checkout, agent verbs. v3.0 adds:
                       bookmarks/TODO index/scopes (V0011-12), locals scope
                       graph + import graph + classified usages with
                       exact/likely/candidate trust classes (oracle-bar test:
                       a wrong exact is a release blocker; V0013/V0015), and
                       local review sessions (refs/kbc/review/<id>/ps<n>
                       patchsets + interdiff + blob-sha viewed state, V0014 —
                       mutations loopback-only). v4.0 "The Review Room"
                       (V0023): review-SCOPED comments pinned to patchset
                       blobs w/ lazy per-read carry-forward (exact→fuzzy→
                       snippet-guard ladder; an uncertain match is an honest
                       orphan, never a guessed line) via GET /reviews/{id}/
                       comments; review verdicts (loopback, verdict_ps
                       staleness); atomic annotation batches (one tx, one
                       SSE); suggestion storage + the APPLY route (loopback,
                       exact-match splice — drift = 409 w/ tree untouched;
                       the SECOND sanctioned working-tree mutation beside
                       checkout); branches with remote-tracking enumeration,
                       origin/HEAD default, ?sort=suggested (named terms).
                       v5.0 "The PR Room" (PRR, V0024–26): PR-BOUND reviews
                       (start-pr binds refs/kbc/pr/<n>, pr_meta snapshots,
                       pr-status drift probe, sweep refresh), agent-imported
                       FINDINGS (kbc-findings/1 — severity blocker|concern|ok,
                       f-* slugs, location ladder, origin import|manual w/
                       manual NEVER superseded by re-import, human
                       dispositions agree|dispute|waive|fix-later riding the
                       carry-forward ladder), agent report + verdict dialectic,
                       inbox/timeline/analytics/recurrence/impact reads
                       (deterministic named terms, never a quality verdict),
                       github-threads (diff_hunk→same ladder, live-composed
                       never persisted), export-github (kbc-github-export/1 —
                       orphans/multi/whole_file skipped honestly; kb-code
                       NEVER writes GitHub, the gh round is agent-layer via
                       /kb-review-work + publish recording), apply-batch
                       (verify-all-then-apply-all), hover + /framework/edges +
                       resolve-symbol (sym= addresses) + /diagnostics, scip
                       staleness + `scip run`, the Rails lens (rails-lens/1,
                       frameworks/rails — convention edges capped at
                       likely/candidate, structurally no exact), and the
                       lsp-live tier ([[intel.providers]] → kb-lip, exact
                       ONLY blob-verified, computed-fresh-NEVER-persisted).
                       v6.0 "One Inbox" (S2, tag v0.40): federated GET
                       /api/inbox (reviews + working-tree annotations + a
                       live kb desk/comments pull, surfaced-never-scored,
                       honest per-lane kb degrade); POST /api/code-actions
                       relays lip LSP quick fixes (conversion to a
                       suggestion rides the EXISTING annotations/batch op,
                       no new mutation route); [review] remote_mutations
                       (default OFF) graduates five review-mutation route
                       families off loopback-only for a bearer caller via
                       review_gate, working-tree line never moving; GET
                       /api/repos gains intel_providers (every matching lip
                       provider, not just the first).
                       Transcript-text + tree/ref-
                       mutating routes are loopback-only; the rest imports
                       kb-server's `auth_bearer`. Excluded from `ci-workspace`/
                       CI's `workspace` job (own `ci-code` recipe/CI job) —
                       build-time isolation, no ORT. **v7.0 "The
                       Continuum" added the security posture, git-argv
                       discipline, the cmd/1 + kbc-theme/1 registries and
                       workspaces v0 — all of it, with the invariants,
                       in [crates/kb-code-server/CLAUDE.md](crates/kb-code-server/CLAUDE.md);
                       read that before touching any of them.**
  kb-lip/          bin (`kb-lip`) — generic LSP→HTTP adapter (lip/1):
                       wraps ANY language server (config argv, stdio
                       JSON-RPC) behind identity/hover/definition/references/
                       symbols/diagnostics (push cache + LSP 3.17 pull);
                       the blob-hash guard hashes on-disk bytes before AND
                       after every LSP round trip and REFUSES on mismatch —
                       what lets kb-code mint lsp-live/exact; $/progress
                       tracking → refused:"indexing", never an empty guess.
                       v6.0 (S2-C): 6th endpoint POST /lip/code-actions —
                       the design-lip.md "no code actions" refusal
                       partially reversed (rename/formatting/
                       executeCommand still refused); same pre/post
                       blob guard, bracketing any codeAction/resolve
                       round trips. Reference provider configs +
                       Dockerfile.{ruby,rust,typescript,python,go} live
                       in providers/ (ruby-lsp default, solargraph alt;
                       rust-analyzer/typescript-language-server/pyright/
                       gopls added v6.0). Joins the kb-code CI carve-out
                       (ci-code lane).
  kb-code-cli/     bin (`kb-code`) — clap CLI, one verb per server surface
                       (search/blame/why/story/session-diff/map/pack/usages/
                       bookmarks/todos/review {start,snapshot,files,interdiff,
                       viewed,gc}/…, `hook install` for the why-hook plugin).
                       v4.0 adds branches/compare/merge-check/repo-state,
                       review {comments,verdict}, suggest {create,list,
                       apply,drop}, annotate batch, and `annotate watch` —
                       the /loop-ready SSE triage verb (seed-then-diff
                       seen-set; the connect-time event replay is idempotent
                       by construction; --ignore-author claude). v5.0 adds
                       the pr family {list,show,checks,comments,fetch},
                       review {start-pr,pr-status,report,artifact,findings
                       {import,list,add},disposition,inbox,timeline,sweep,
                       analytics,github-threads,export-github,publish},
                       suggest apply-batch, hover/framework/resolve-symbol/
                       diagnostics, scip run, and the watch loop's Finding
                       key (dispositions surface, own imports don't). v6.0
                       "One Inbox" adds `inbox` (--watch/--interval poll
                       loop over unified-inbox/1, own seed-then-diff
                       seen-set — not the SSE `annotate watch` machinery)
                       and `code-actions` (--suggest N converts an LSP
                       quick fix into an annotation+suggestion via the
                       existing annotations/batch op).
  kb-buildstamp/   lib — leaf git-stamp crate (PF-B1): the ONLY build.rs
                       watching .git/HEAD; a commit re-stamps ~20 lines
                       instead of kb-server + everything downstream. BIN-only
                       consumers (kb-cli); kb-server takes the stamp at
                       runtime via `set_build_stamp` (unset ⇒ 0.0.0-dev/
                       unknown, the SPA drift guard's no-op value).
web/               React 18 + Vite + TypeScript SPA
  src/             routes, components, hooks, api, styles
  dist/            vite output (gitignored; daemon serves via ServeDir)
web-code/          kb-code's own SPA (React 18 + Vite + TS, mirrors web/'s
                       tooling): omnibox + /search page, CM6 read-only reader
                       with blame gutter + why-panel + hover originating-change,
                       session-diff, annotations, checkout dialog; v3.0 adds
                       nav memory (Ctrl-o/i, g.), structure popup (gO), sticky
                       context, occurrence highlight, speed search, bookmarks
                       rail (gm/gM), ~todos, and the ~reviews cockpit
                       (patchset timeline, interdiff, viewed progress).
                       v4.0 "The Review Room": light/system THEME live
                       (dark→light→system Sun toggle; tokens carry z-ladder/
                       shadows/type-ramp; self-hosted fonts; 40+ icon set,
                       zero unicode-glyph buttons), TopBar IA (review chips
                       + Explore menu + mobile nav sheet), ~branches smart
                       landing (default hero, ranked reason-chip rows,
                       Start-review CTA, RefTypeahead), unified+SPLIT diff
                       renderers w/ server-span syntax highlight (per-line
                       integrity guard), full-page review diff
                       (~reviews/:id/diff[/*], j/k-keyboard, reading-order
                       sections), review comment THREADS on both gutters
                       (carry-forward lines, orphan sections, verdict bar,
                       ThreadsCard deep-links), CM6 suggestion editor w/
                       live preview + one-click loopback apply, and a
                       mobile review sheet + coarse-pointer comment pills.
                       v5.0 "The PR Room": ~reviews is the attention-ranked
                       INBOX landing; the Room cockpit's Report tab (verdict
                       dialectic + risk dial + finding cards w/ dispositions
                       + CI + GitHub-conversation cards, every count DERIVED,
                       drift captioned); diff findings as DiffThread variants
                       behind overlay lanes (findings/comments/diagnostics/
                       github), finding-mode + question-mode composers,
                       t/T/o/d/y keys, guided tour, dialectic ledger,
                       recurrence chips, X-ray caller chips; publish preview
                       composes real gh commands from export-github (the SPA
                       never calls GitHub); ask-the-agent card + awaiting
                       chips (pure voice derivation) + agent-reply toasts
                       (query-snapshot watermark diff); diagnostics gutter
                       (3rd lineGutter lane) + inspector card; peek trust
                       badges incl. lsp-live, ?sym= links, FrameworkCard.
                       v6.0 "One Inbox": `/~inbox` landing (reviews +
                       working-tree questions + kb desk/comments lanes,
                       Home/TopBar link) and QuickFixes (LSP quick-fix →
                       suggestion, shared by DiagnosticsCard + the
                       review-diff inspector). **v7.0 "The Continuum"
                       added the Desk, cmd/1, the Location Contract,
                       kbc-theme/1 and `~workspaces` — all of it, with the
                       invariants, in [web-code/CLAUDE.md](web-code/CLAUDE.md).
                       Its keyboard-dispatch section is load-bearing for
                       every future bare-key binding; read it first.**
                       `just ci-code-spa` (build + vitest);
                       web-code/e2e/ Playwright harness (`just ci-code-e2e`)
corpus/canon/      4 sample artifacts (frozen — copied from research)
tests/e2e/         Playwright suite (iframe + SPA + multi-daemon stress)
docs/              research/ (frozen design) + self-host, configuration, spike-findings, …
```

## Working with this repo

### Build + test commands

`justfile` is the canonical command list. The essentials:

```bash
just ci          # workspace fmt + clippy + tests
just ci-spa      # web/ npm ci + npm run build → web/dist/
just ci-e2e      # fast-profile build + SPA bundle + Playwright (chromium)
```

- **The full release link + lance/datafusion compile runs 20+ min cold** — on
  a shared or slow machine wrap long builds in `nice`/`ionice` (an untracked
  `CLAUDE.local.md`, when present, carries the host's exact throttle rule).
- **Daily iteration: `--profile fast`** (no LTO, `codegen-units=16`, ~3×
  faster): `cargo build --profile fast -p kb-cli`.
  Reserve `--release` for the binary the daemon runs (`~/.local/bin/kb` →
  `target/release/kb`) and tagged shipping builds. (Optional mold-linker tip:
  [docs/architecture-invariants.md](docs/architecture-invariants.md) → "Dev-environment tips".)

```bash
# Workspace sweep EXCLUDES kb-embedder so no ONNX Runtime is linked (fast). A
# bare `cargo test --workspace` unifies the `local-embedder` feature ON across
# the shared kb-core rlib and pulls ORT into everything — use --exclude:
cargo test --workspace --exclude kb-embedder --no-fail-fast
cargo clippy --workspace --exclude kb-embedder --all-targets -- -D warnings
# The ORT-linking surface (slow; static ONNX Runtime) — its own recipe:
just ci-embedder        # = cargo {clippy,test} -p kb-embedder + -p kb-core --features local-embedder
cargo fmt --all
cargo test -p kb-core <name>            # single-crate / single-test filter
```

**Platforms** (Linux · WSL2): the embedder bundles ONNX Runtime
statically (`ort-download-binaries`, fetched from CDN at build — for offline set
`ORT_STRATEGY=system` + `ORT_LIB_LOCATION=<dir>`). `KB_HOME` (or `KB_STATE_DIR`/`KB_CONFIG_DIR`/
`KB_CACHE_DIR`) overrides paths — tests + containers use it since
`directories` only honours `XDG_*` on Linux. `[indexer] watch_mode = "poll"` for
WSL `/mnt/*` / network mounts. Native Windows is out of scope (WSL2 is the path).

If a per-crate test count drops unexpectedly between commits, something regressed.

## Cadence

- **Phased commits on `main`, each green-CI.** Same shape every milestone
  (v0.1 → v0.14 and on). No long-lived branches.
- Each commit: `feat(crate): summary (PhaseID)` for features, else `docs:` /
  `chore:` / `feat(spa):`. PhaseIDs are per-track letters named in the plan file.
- Co-authored-by footer on every commit, naming the model that actually drove
  the session (e.g. `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`,
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`) —
  never attribute another model.
- Tag at milestone end with a multi-line annotated message summarising tracks
  + verification. Run the milestone's verification plan before tagging.

## Plan file workflow

Milestone plans live OUTSIDE the repo, in the operator's plan directory (an
untracked `CLAUDE.local.md`, when present, names the current file). They are
structured **Context → Status → Scope → Architecture → Phasing → Decisions →
Risks → Verification → Critical files** — read the current plan before starting
any phase.

When entering plan mode for a new milestone, ask the user about direction via
`AskUserQuestion` first; don't assume. Past milestones: the narrative in
[docs/spike-findings.md](docs/spike-findings.md) (plus
`git log --oneline --grep='^feat.*(v0'` — the pre-public history is kept in a
private archive, so on this repo it only covers commits since the public import).

## Architecture invariants

Load-bearing constraints — breaking them surfaces as subtle runtime failures, not
compile errors. **Full text + rationale for each is in
[docs/architecture-invariants.md](docs/architecture-invariants.md) (§N below); read
the matching entry before changing that subsystem.** kb-core-only invariants live
in [crates/kb-core/CLAUDE.md](crates/kb-core/CLAUDE.md); kb-code's own (server
+ SPA) live in
[crates/kb-code-server/CLAUDE.md](crates/kb-code-server/CLAUDE.md) and
[web-code/CLAUDE.md](web-code/CLAUDE.md).

 1. **Arrow version pinning** — lance + arrow MUST share one version (`=57.3.1`) or RecordBatch types won't unify.
 2. **Doc↔code bridge: kb extracts HINTS, kb-code mints CLASSES, nothing is cached (DCB v1)** — `kb_core::coderefs` is a pure, LLM-free, corpus-local extractor over `EnrichCtx::html` (Markdown + HTML uniformly; `TEXT_SKIP_TAGS` skips the kb-prompt template, #5) with a CLOSED grammar (whitelisted-extension paths ± `:line`/`:a-b`/`:a,b,c` · `Namespace::Class` with the `::` REQUIRED, never a bare CapWord · `Class#method` · `path#member` · gem/vendor ⇒ `external` · issues from `<a href>` only), golden-pinned, written to its OWN `code_refs`/`code_refs_docs` tables (never `edges` — that PK is artifact↔artifact and every consumer assumes `get_by_id` resolves the dst) and NEVER bumping the index generation (`UpsertChunks` precedent, #15). Every kb-side field is named `*_hint`: kb has no tree and no symbols and must be structurally unable to mint a trust class. Classification lives in kb-code's `codelens/1` (`path_state`/`line_state`/`symbol_state` — its OWN vocabulary, never `resolve.rs`'s `CLASS_*`), is computed per request and is NEVER persisted; W3 stores CLAIMS only, re-validated on read. ONE live call direction: kb-code→kb; kb knows kb-code only as an inert `[kb.*] code_url`. **kb-sibling/1** governs that pair: `sibling_protocol`/`sibling_major`/`schema_epoch`/`build_sha` ride `GET /api/identity` (`/healthz` stays pure liveness), `kb_core::sibling::refuse_if_volume_ahead` REFUSES BOOT when a sqlite volume's refinery epoch exceeds the binary's (`Db::open` per kb · kb-code's `Store::open`, before migrations run — the 13.5h kbc rollback outage), and `KbClient` handshakes once per process, failing CLOSED on a mismatch (`kb_sibling_mismatch`, distinct from unreachable) but grandfathering an ABSENT Hello (legacy peer, rolling deploys) and never caching an unreached probe. `code_refs` is registered in ALL THREE artifact-id lifecycle registries (`CASCADE_STEPS` + `SWEEP_TABLES` + the `cascade_relocate_doc` rekey tx) — none fires on an omission, and #27/F3 relocate never re-indexes, so a missing entry strands refs under a dead id forever.
 3. **`ConnectInfo` from request extensions** — read it from extensions, not as an extractor; a missing one fails CLOSED (not loopback).
 4. **Loopback bypass everywhere security applies** — trusted-hop gate first, then XFF walked right-to-left; `trusted_proxies` empty by default. **Fail-closed on a token-less public bind**: `serve_with_paths` refuses a non-loopback bind with no token (override `KB_ALLOW_NO_AUTH=1`); `auth_bearer` 401s a token-less non-loopback request rather than bypassing. — resolved ONCE per request inside `auth_bearer` (ladder: registry token [`Authorization` or `X-Kb-Token`] > trusted-hop identity header > legacy shared token→operator > loopback→operator), lowercase-folded via `kb_core::identity::normalize_username`, inserted as `Extension<Identity>`; **no route re-reads an identity header**. An untrusted peer's `Remote-User` is NEVER read (same fail-closed peer gate as XFF, #4) and the ADMISSION/401 surface is byte-identical to pre-v0.34 — a token registry is additive and must never *tighten* `KB_ALLOW_NO_AUTH` (the upstream-proxy-is-the-gate posture). The ONLY identity-gated verbs are comment/reply body-edit + delete (owner-only, 403 `urn:kb:errors:not-owner`, legacy no-`user` rows owned by the configured operator) — enforced on the direct routes AND the batch path; resolve/reply/attach/reanchor stay open. Usernames are plain lowercase strings — no users table (Authelia is the identity store); per-user rows key the resolved string; `''` = pre-v0.34 rows, rewritten to the operator by a marker-gated backfill at boot.
 5. **`<template id="kb-prompt">` convention** — per-artifact prompt bundle; outbound scrub may strip it; don't repurpose the id.
 6. **kb-comments/1 review files** (`.review/<id>.json`) — path-based id; every mutation under the per-kb `review_lock`; `reanchor` is the only anchor rewrite. *SLATE amendment (2026-09):* #6 now names the daemon-owned sidecar-ledger FAMILY, not one file shape — `.review/<id>.json` · `.attachments/` (#18) · `.proposals/<id>.json` · `<state>/slates/<slug>/ledger.jsonl` (kb-slate/1), schema-string discriminated, each under its OWN lock (per-kb for the first three, per-slate `KbHandles::slate_lock_for` for the ledger, daemon-wide because a slate keys on a project), mutated ONLY through routes and CLI, never hand-edited, never indexed as truth, registered in backup/reset. The slate ledger is append-only with tombstones; its `seq` is minted under the lock and IS the revision token; take-lease and age state are derived at read time from timestamps plus the #11 live registry, never written by a clock; `slate.updated` fires once per append, never per read (#24). Full text + the slate's own posture: docs/architecture-invariants.md §6.
 7. **`artifact_host_suffix` is runtime config** — threaded through parse/extract/index, never hardcoded; prod uses `.artifacts.<domain>`. *v2 (2026-08-21):* the host label may also be qualified `{kb_enc}--{id}` (`_`→`-`, rightmost `--` split) to disambiguate same-id artifacts across kbs (a shared-rel-path collision, e.g. two corpora's root `index.html`); bare labels keep the legacy alphabetical-first-wins walk; the SPA emits qualified only.
 8. **`history` table is append-only** — except `scroll_y`/`scroll_max`/`updated_at` upserts on the latest open row; `history.recorded` SSE fires only on INSERT.
 9. **`kb share` engine lives in kb-core** — takes an explicit `ShareCtx`; filesystem walk; export ALWAYS strips kb-prompt; tokens from daemon env. In-share cross-artifact links relativized → self-contained export (`--links` governs only out-of-share danglers); `--local`/`POST /share/export` returns a scrubbed+relativized `.zip` bundle. *S1 (2026-07-30):* selection = path walk OR an ORDERED list-derived file set (`stage_file_set`, same pipeline byte-for-byte) whose generated escaped `index.html` TOC is always the entry page — `POST …/lists/{id}/share/export` + `kb share --list <id-or-title> --local`; tombstoned entries skipped LOUDLY (`x-kb-share-skipped`), empty set 400s, `index.html` collision errors.
10. **Agent-memory recall = rank-position × salience × decay, optionally scaled by two v2 factors (MI-W2 amendment, 2026-08)** — deterministic, LLM-free; supersede/forget drop at recall time (forget is now itself a MI-W2.3 soft-forget tombstone by default — `kb-status: forgotten` + `kb-forgotten-at` spliced into the source via the existing generic meta_edit/markdown splices, reindexed, still on disk/searchable/census-visible — rather than the old traceless hard delete; `?purge=true` / `kb forget --purge` still hard-deletes for the rare case nothing may survive anywhere); write-only ingest. **MI-W5.R amendment (2026-08-08 operator ruling, on W5.1 bench evidence):** the single `[memory] scoring_v2` flag was SPLIT into `scoring_v2_relevance` (**default `true`** — the W5.1 live-corpus bench measured a real, positive effect: 25 queries, 14 better / 11 wash / 0 worse, 23/25 returning a different id set, +3% mean latency) and `scoring_v2_stability` (**default `false`** — the bench never exercised this factor at all, since the `memory_recalls` ledger it needs lives in the sessions corpus the probe didn't load; fixture-tested only, pending a bench that does). Both gate their factor independently inside `kb_core::memory::rerank_with_policy_scored`, which now takes two separate bools (`rerank_with_policy` delegates with BOTH hardcoded `false`, so that entry point's base formula stays byte-for-byte identical regardless of either default — not merely "times a neutral 1.0", the multiplication is skipped entirely); all four `(relevance, stability)` combinations are valid. The old `scoring_v2` key is kept as a DEPRECATED alias (`MemorySection.scoring_v2: Option<bool>`) that, if present, forces BOTH new flags to its value and logs a one-line boot warning — so an existing `kb.toml` never silently changes meaning. `scoring_v2_relevance` gates a per-corpus min-max normalized **relevance** factor over the search engine's own raw score (hybrid/BM25/vector), computed over the POST-FILTER survivor set (forgotten/tombstoned/floor-dropped siblings can never skew a survivor's normalization), degrading to a neutral `1.0` when a corpus can't support a meaningful spread (fewer than two scored hits, a tie, or no score at all on the empty-query/list_docs timeline path — never a divide-by-zero or an arbitrary tie-break). `scoring_v2_stability` gates an FSRS-inspired **stability** multiplier applied to the decay factor, derived in closed form from the memory's own W1 `memory_recalls` ledger (`recall_count`/`last_recalled_at`) plus salience/decay-bucket/created (Bjork desirable-difficulty weighted: a recall landing close to the decay/salience floor counts for more), monotonic non-decreasing in `recall_count` and capped so a memory can never become immortal — a finite multiplier can slow an exponential decay curve but never halt it in the limit. Each factor is SURFACED on the `Scored`/`RecallHit` decomposition iff its own flag is on (never silently absorbed into `score` alone), so `kb recall --explain` renders the full arithmetic of whichever factors are active. `recall --as-of` remains unbuilt (a separate, still-standing ruling): MI-W2.3 resolves ONE of its three original blockers (forget is no longer an undetectably-traceless hard delete), but the other two stand — pin state/salience/decay-policy are still read as CURRENT values only (no time-versioned ledger for any of them), so an as-of ranking would chimera historical content against present-day metadata. The MI-W2.4c EPOCH HONESTY marker (`KbPaths::tombstone_era_file`, daemon-boot-persisted, `GET /api/memory/tombstone-era`) backs `kb memory log`'s supersede-chain walk and `kb diff --between`'s date-resolved diff, both printing a caveat — in human AND `--json` output — when their window predates it. *CT amendment (v0.38):* the provenance/verification lane (U3 parse-back + `memories-from` reverse read + `expand`'s anchor re-resolve · `flagged` via an ordinary `[kb-flag]` comment, top-of-triage at `FLAGGED_URGENCY=2.0` · `used` explicit-reference-only, V0037 · `pos` the injected hit's RANK, MR1/V0041, NULL = unknown and never inferred from transcript position · `recalled-by` via the correlated `newest_capture_pred` fan-out) is SURFACED-NEVER-SCORED: wire/display structs only, computed post-rank over the returned page, structurally unreachable from `kb_core::memory`'s scoring types — each pinned by a byte-identical-decomposition test. *MS amendment (2026-08-30):* `kb recall`'s DEFAULT scope is now `auto` (CLI-side only — the route's own default stays `scope=all`), resolving to `scope=all` + `project=memory-<slug>` + `visible_to=<slug>,memory-<slug>` from the git MAIN-checkout root's basename (worktree-safe, `--git-common-dir`; falls back to fleet-wide outside a repo); `visible_to` is a NEW per-id link filter at `for_kb`'s same L7 stage but with INVERSE pass semantics (unlinked/`*`-global always visible; for_kb's unlinked = invisible answers "visible on kb X's page", visible_to's unlinked = visible answers "should leak into project X") — still SURFACED-NEVER-SCORED; `GET /api/context` threads the pair as `memory_project=`/`memory_visible_to=` into its previously-hardcoded-`scope=all` memories lane, absent ⇒ byte-identical.
11. **Sessions: the canonical session id is the transcript's own `sessionId`; transcripts are EPISODIC memory** — additive `kb_session` lance column + per-kb `sessions`/`session_files`/`session_decisions`/`session_commits`/`session_research` sqlite tables + cross-kb fan-out. Transcripts are excluded from default search + recall (R0); the indexed surface is a deterministic DIGEST, not the raw JSONL (R1); pulled on demand via `kb why`/`kb recollect`, never auto-injected; recollect's success/staleness are SURFACED signals, never score terms or deletions (R3). **Canonical id is recovered from the JSONL `sessionId` (ground truth), preferring it over `<meta name="kb-session">` then the filename — a capture hook once truncated the meta, breaking `claude -r` + the session↔memories link.** **Multi-capture: one long session is captured at every Stop, so a `session_id` accrues many `sessions` rows (different `artifact_id`s); every per-session READ — `*_for_session` (files/decisions/commits/research), the `sessions` LIST, and the funnel/rollup `COUNT(*)` — must scope to the NEWEST capture (the superset, as `sessions_get` does) or it double-counts — *PF-R1 (V0040):* materialized as `sessions.is_newest` (written transactionally by `recompute_is_newest` in upsert/delete/cascade; relocate rides; UNIQUE partial index = live no-double-flag assertion; `newest_capture_pred` now emits the flag lookup).** *Wave-0 amendment (2026-07-18):* the capture envelope is a split contract — byte-identical `<pre>` + additive post-`</pre>` JSON blocks (resolved commits incl. trailers; subagent sidecar digest) — with V0024–26 columns (sidecar-primary subagent aggregates, commit resolution, `via_subagent`); the `Kb-Session:` trailer + `GET /api/sessions/by-commit` make the session↔commit join exact. *W0.6 amendment (2026-07-21):* a third additive tail block `<section id="kb-session-sidecar-text">` inlines each sidecar's raw JSONL (2 MiB/agent · 8 MiB total RAW caps, deterministic truncate-then-escape, absent when no sidecars; searchable evidence, NOT a resumable transcript — round-trip still reads only the FIRST `<pre>`); **R1 is now a split surface: the digest still owns everything that RANKS (embed · SQ5 chunks · excerpt), while the `code` FTS column is a BOUNDED BM25-only full-text EVIDENCE lane (main transcript + sidecar text combined, capped at `sessions::SESSION_CODE_FIELD_CAP_BYTES` = 32 KiB/row post-parse — head-60%/tail-40%-truncated, `kb reindex` backfills)** — capped 2026-07-22 after the first ship left the main transcript uncapped and the corpus-wide `code` sum exceeded Arrow's ~2.147 GB `i32` ceiling, panicking lance's `interleave_bytes` (via `upsert_docs`'s `merge_insert`) on `kb reindex` and every subsequent `kb recollect` call; and the lance `kb_session` value is a HINT only — canonical session joins go artifact_id→sqlite (`SessionsGetByArtifactIds`; a trailing-dash meta once emptied recollect corpus-wide). *R0 single-kb opt-in default (2026-07-22):* `[kb.*] default_search_category` (`KbConfig`/`KbContext`, exposed read-only on `GET /api/kbs`'s `KbSummary`) lets a kb apply R0's `?category=` opt-in FOR you — a `scope=one` request against that kb with no `category` param defaults to the configured value (e.g. `"memory-session"` on the sessions corpus), so a user directly scoped there doesn't have to type/remember it; an explicit `category` param (any value) always passes through unchanged. `scope=all` is completely unaffected — `federated_search` builds its `Filters` from the request alone and never consults per-kb config, so R0 stays default-exclude for the federated path regardless of any kb's configured default. *Sessions-rethink amendment (2026-07-28→29, full text docs/architecture-invariants.md §11):* the transcript is now interpreted ONCE by the `session-view/1` engine (`sessions/view.rs`), walked by three presenters (HTML renderer, `kb sessions read`, `GET …/view`) that add no interpretation of their own; turns carry stable `t-<uuid12>` ids (uuid-derived — replaces reclaimed ordinals; old `#turn-N` links are an accepted break, memo D5) — a category-gated resolution rule (#6 amendment) now resolves memory-session comment anchors against that DECODED transcript, not raw bytes; V0029 froze nine `sessions` columns (`project_key`/`harness`/`last_assistant_text`/`active_secs`/`substance`/…) backfilled by the ONE spent reindex that also recomposed the digest excerpt as `title · first-prompt · closed:`; `substance` (trivial|routine|substantive) is the ONLY persisted triage value — no outcome enum, badges derive display-time; V0030 widens `session_research.kind` for the `grok_job` by-job join (`kb sessions by-job`); harness (`claude|codex|opencode|grok|kimi|omp`, a closed set of six — `kb_core::sessions::HARNESSES`) is captured via three sibling shell adapters; workflow journals + TaskOutput snapshots now ride the sidecar-text evidence block (reversing the earlier "deliberately excluded" text); *live-follow amendment (W7, 2026-07-29):* the LIVE lane (`GET /api/sessions/presence`, `GET …/{sid}/live`, `kb sessions read --live`/`--follow`) is derived-ephemeral over the transcript the capture pipeline already reads — no new tables, no new columns, nothing ever written; capture stays the record, and if the live view and a landed capture ever disagree, the capture wins by definition (the `sessions::view::view_append`/`view_full` equivalence golden is the contract that they can't structurally diverge in the first place). Both live routes are loopback-only HARD in v1 (stricter than every other sessions route, since a live transcript is unscrubbed mid-flight content) — see invariant #4's fail-closed ethos and docs/architecture-invariants.md §11 for the full posture. *CT amendment (v0.38):* the memory-recall ledger parse now PREFERS the `kb-recall/1` machine marker (`<!--kb-recall/1 kb=<kb> id=<hex12>[ pos=<n>]-->`, hook-emitted; free-text grammar = permanent fallback, both golden-pinned; *MR1/SL6 2026-09:* the body is an unordered `key=value` bag — `kb`+12-hex `id` required, `pos` 1–99 optional, **unknown pairs are ignored** so the grammar is forward-compatible, and the pre-MR1 `split_once(" id=")` would have rejected layout v2's own `pos=` marker outright) and returns `DerivedRecalls` with a `marker_parsed`/`fallback_parsed`/`failed` census — the hook warns on parse gaps (the string `kb doctor --hooks` greps); `cat`/`get` now produce `artifact_open` research rows; touches carry Exact/Fuzzy tier labels. *LSC amendment (2026-08-22, docs/research/kb-live-sessions-cockpit-2026-08.html):* the live-sessions COCKPIT is a two-axis derived-state layer over this same episodic material, not a new invariant — harness hooks (`plugins/kb-memory/hooks/kb-beat.sh`) PUSH one lifecycle EVENT per turn boundary (`start|prompt|tool|turn_end|blocked|unblocked|end`), never a state; `POST /api/sessions/beat` maps event→holder and the daemon ALONE derives state via `kb_core::sessions::live::derive_state` (holder × silence are independent axes — silence never flips holder, it only escalates `working`→`stalled?`→`presumed_ended` or `waiting`→`cold`). The registry (`kb-server::live_registry`) is IN-MEMORY ONLY — nothing persisted, LF-7 held — rebuilt on restart as a degraded Tier-0 view from capture rows (`confidence:"presumed"`, aged since last landed capture) fanned out via `buffered_join` (#28). `GET /api/sessions/live-status` rides `auth_bearer` with a forced-scrub floor (`cwd`→basename, `last_line` re-capped) — LF-5's own documented "token + forced scrub" graduation, taken for this STATE lane only; `/presence`/`/{id}/live` stay loopback-only HARD, unchanged, since those serve raw unscrubbed transcript bytes. `session.state` SSE fires only on a derived-state CHANGE, never per beat (#24).
12. **Meta edits rewrite the SOURCE** — scoped strictly to `kb-tags`/`kb-category`; the HTML splice skips `<template>`/`<script>`/`<style>`.
13. **Web config edits = persist-to-loaded-file + in-process restart** — `serve_loop` owns the restart with rollback; clean teardown; bind before spawn.
14. **Versions/Diff — three sources behind one façade** — git (path relative to the git root!) · index snapshots · working tree; `auto` is per-file. *CT-F6 (v0.38):* temporal resolution over that façade is ONE pure function and is PER-ARTIFACT — `resolve_as_of` (MI-W2.4b) makes the pick, `resolve_memento` only adds honest metadata, and every surface (`kb diff --between` · `GET …/versions?at=<unix>` · `kb versions --at` · the SPA's `?at=` reader) shares it. Always NEAREST-PRIOR and labelled as such (`exact` only on a same-second hit); an instant older than the oldest version is a MISS naming the floor, NEVER a silent degrade to the oldest; `?at=` is structurally additive (its own response struct, byte-identical when absent). Not a corpus timeline and not `recall --as-of` (#10, still rejected) — it touches no ranking. SPA mirror `web/src/lib/memento.ts` is golden-pinned in lock-step with kb-core's.
15. **Gallery row-set memo keys on the storage-actor index generation** — bump only on row-set/edge mutations; the `Mutex` is never held across `.await`.
16. **Notes are Markdown artifacts, not a new entity** — `is_note` = `kb-category: note` AND Markdown; toggle index = GFM task ordinal; excluded from gallery/atlas.
17. **Indexer ingest is a per-kb back-pressured mpsc** — NOT the shared bus; push via `IngestSink`; `blocking_send` only off the tokio worker.
18. **Comment attachments — manifest under `review_lock`** — stage → adopt → GC (ref-counted); magic-byte sniff gate; XSS-safe serve.
19. **Reading-progress follows the history lifecycle** — cumulative-idempotent (`max()` upsert); seed-on-open; no SSE, no `bump_generation`; touched ≠ read.
20. **Iframe back-button = `location.replace`, not push** — a bubble-phase interceptor yields one clean history entry per artifact. *Linkflow (2026-08):* a second, mutually exclusive branch relays artifact-shaped CROSS-origin links (`kb:link-open`, suffix derived from `location.host` per #7) + hover peek (`kb:link-hover`/`clear`) to the parent — still one router push per artifact; both nav branches snapshot `kb:scroll` first, `pagehide` flushes a final one, and the trampoline forwards `#fragment`→optional `sec`. Parent-side: ONE peek card, two triggers; per-tab sessionStorage reading-flow stack (`flowStack.ts` — chip/`u`/browser-Back return, seed slots between `?sec=` and server resume).
21. **Daemon binds an IPv6 loopback companion** — best-effort `[::1]:<port>` beside IPv4 loopback; loopback-only, never `[::]`.
22. **Markdown composer is CodeMirror 6 + hidden mirror `<textarea>`** — the mirror carries per-surface aria/value for Playwright; must stay layout-present.
23. **SPA server state = TanStack Query cache + SSE invalidation bridge** — `staleTime: Infinity`; the bridge owns invalidation (hooks carry no fetch/subscribe plumbing); three documented exceptions (sessions, open note, stale anchors); exceptions come in two kinds — three with their OWN SSE wiring (sessions, open note, stale anchors) and a no-SSE-tie set carrying finite staleTime + manual refresh (daycard, live tail, sessions presence, and DCB's cross-daemon doclens queries).
24. **One SSE connection per daemon per BROWSER** — a SharedWorker hosts the connection core (one unfiltered `/api/events` stream per daemon, fetch-parsed, no EventSource); tabs are MessagePort clients of the `sse` facade — never stream `/api/events` from tab code; worker is the sole cursor advancer; gap/bfcache-restore ⇒ resync; direct fallback + `?sse=direct` kill-switch are load-bearing.
25. **Reading lists: read state is DERIVED, anchors are `review::Anchor`** — override > section dwell > scroll completion, computed per response (never stored); anchor JSON canonical via `lists::anchor_to_json`; staleness row-persisted by the `ListAnchorHook`; list mutations never bump the index generation; import = one tx + one `list.updated`; the kb-list/1 MD grammar is golden-test-pinned in kb-core AND kb-cli. *v0.33:* `prune` (engine/route/CLI/SPA) drops read-time tombstoned entries in one tx, one `list.updated`.
26. **ONNX Runtime is isolated to kb-embedder** — `fastembed` is an optional kb-core dep behind `local-embedder`, enabled ONLY by kb-embedder (statically-bundled ORT, not `load-dynamic` — the old dlopen path deadlocked (macOS-era finding); the static bundle also keeps the daemon ORT-free and portable). The daemon drives BOTH embedding and reranking over IPC (`embed_ipc::RerankerClient`); never reintroduce an in-process `Reranker` in kb-server. Keep kb-embedder out of `--workspace` CI commands.
27. **Stored `Doc.path` is canonical; IDs are source-relative** — the indexer's `prepare_doc` stores `paths::canonical_abs(path)` and `reconcile` walks the canonical root, so symlinked source roots (WSL/bind mounts) don't desync the delete pass / `get_by_source_path`. Artifact IDs come from `doc_rel_path` (canonicalises both sides) and must NOT change. *F3 (2026-07-30):* the ONLY sanctioned path change is the relocate engine (`kb mv` / `POST …/docs/{id}/move` / `…/folders/rename`): durable `moves` intent log (V0032) → sidecar renames under `review_lock` → ONE actor tx migrating every id-keyed consumer (embedding preserved, no re-embed) + generation bump; watcher delete suppressed two-layer; startup replays incomplete moves; old ids/rels 301/resolve through the newest-wins moves chain. A raw fs move still destroys — never bypass the engine.
28. **Federated multi-kb read handlers fan out concurrently in submission order** — multi-kb on **one process** (not multi-daemon server proxy). Every scope=all handler over `state.kbs` (search·recall·sessions·lists·notes·anchors·/stats·/kbs) runs per-corpus work through `routes::buffered_join(futs, state.fanout_cap)` (`[server] fanout_cap`, default 8 = `routes::FANOUT_CAP`; `buffered`, NOT `buffer_unordered`), never a serial `for … in state.kbs` await loop. Submission (BTreeMap) order is load-bearing (per-hit kb attribution, RRF arm order, relevance output); no `std::sync::Mutex` guard crosses a fan-out await (#15); each future returns a partial the caller folds + drops-on-error (no `?`-propagate → one corpus never 500s the fleet). Callers pre-build the `Vec<CorpusFut>` (a `for<'b>` closure would quantify over `'static`). *v2 (2026-08-21):* ids collide across corpora (shared rel-path hash, #7 amendment), so federated merge attribution keys on `(kb, id)` (`fusion::rrf_fuse_keyed` + `hit_key`), replacing the old id-only `id_to_kb` first-wins map that could silently merge two corpora's distinct hits into one.
29. **Wikilinks/backlinks ride the existing edge graph; resolution is pure + corpus-local** — a note's `[[target|alias]]` is connective tissue, NOT a new table: it rides the `edges` table (`kind="link"`), so it flows into backlink/outlink counts, atlas link-lines, and the graph route. Parse + resolve live in `kb_core::links` (pure, LLM-free): comrak wikilink AST (skips code/fences like `notes::scan_tasks`) + a deterministic first-hit ladder (id → source-rel path → exact title ci → unique basename ci; ambiguous→dropped from the graph), corpus-local (intra-kb edges). Grammar golden-pinned in BOTH `kb_core::links` (Rust/comrak) AND `web/src/lib/wikilink.ts` (SPA hast pass) — keep in lock-step; the SPA renders from the server's resolution map (`NoteDetail.links`, keyed by `normalize_target`), never re-resolving. Render path UNCHANGED (`kb_options` doesn't enable the wikilink extension — served HTML keeps `[[…]]` literal; the SPA renders links). `EdgeRecordHook` parses from `raw_source` gated on a literal `[[`; reverse lookups via `backlinks_of(id)`. **Memories declined again (2026-08 close-out, Unit 4)** — not merely "memories are HTML": `parse_wikilinks` (comrak) finds ZERO links inside ANY `<p>…</p>`-wrapped content (CommonMark treats `<p>` as an HTML-block tag, so its content is never walked as inline markdown), and `crate::memory::text_to_body_html` wraps every memory body in `<p>` unconditionally — proven by `links::tests::wikilinks_are_invisible_inside_any_p_wrapped_html_only_bare_text_works`. Widening `EdgeRecordHook`'s `is_markdown` gate would silently do nothing; a real fix needs a memory-specific HTML→plain-text inverse of `text_to_body_html` (no general inverse exists for non-`text_to_body_html` bodies) feeding a THIRD parser whose edge cases would have to independently agree with the other two — a rework, not a safe widening. *CT-F3 amendment (v0.38):* **unlinked mentions** are a DERIVED queue, never a hook — `kb_core::mentions` + `GET /api/kb/{kb}/links/suggest` report docs whose PROSE names another artifact's exact title/unique basename with no `kind="link"` edge (computed per request, persisted nowhere, the dupes/triage posture), and the ONLY mutation is the explicit `POST …/links/apply` (`kb links apply`), which re-derives the mention before splicing. Not a second scanner: `links::prose_text` is the SAME comrak parse `parse_wikilinks` uses (code spans/fences, existing `[[…]]` and link labels skipped by construction) and `mentions::apply_wikilink` VERIFIES its splice by re-running both rather than translating a prose offset. Guard rails: self, already-edged pairs (directional), names under `MIN_MENTION_LEN` (12, surfaced), ambiguous names, `memory-session` transcripts (R0); the authored target is always verified to resolve back to the dst. The memory ruling is untouched — a memory (like any HTML artifact) can be a link TARGET but never a SOURCE, so its rows carry the honest `Applicability` note and `apply` 400s with the file untouched.
30. **Reader chrome = one home per action; ONE merged inspector rail (v0.21 → v0.23)** — ContextBar owns artifact verbs (anchor·add-to-list·copy-link·share·fullscreen icon→immersive binds `o`·**bare** `data-kb-act="bare"` binds `b`). The right dock is a SINGLE icon rail owned by `PreviewInspector` (the persistent shell): **6** inspector sub-tabs (`.kb-pinsp__icons`, `data-kb-itab`, `useInspectorTab` persists + defaults to last — a #23 carve-out) + comments + versions icons (`data-kb-act="dock-{comments,versions}"`), separated by a `.kb-pinsp__rail-gap` divider. **v0.22**: the standalone `about` sub-tab was dropped — the merged stack-everything tab (key still `"all"`) is now FIRST + relabeled **About**; ONE badge grammar (`itabBadge`→`.kb-pinsp__itab-badge`) on folder/links/sessions(memories)/comments/versions; the comments/versions panels' own ✕ are GONE (the rail icon toggles them). The old `[inspect|comments|versions]` text pills are GONE (`ReaderDock` is now just column chrome). `panelMode` ("inspect"|"comments"|"versions") picks the body: inspector sections (`show(tab)`; "all"=every section stacked) or the `panelSlot` (CommentsPanel/VersionsPanel, rendered as a `.kb-pinsp` child so it fills below the rail, no double-scroll). On desktop the rail is the always-docked column and e2e drives panels via `dock-comments` (1280×720); on mobile it is ONE bottom sheet — see v0.23 below. **v0.22 made metadata navigation**: a clickable-metadata "passport" (folder breadcrumb·category·mtime·tags all deep-link, id copies) + "Explore from here" facet chips (live counts via `useFacetCounts`) + a directed Links graph (Outlinks→/←Backlinks) + re-anchored related memories (`useRelatedMemories(kb, q)`) + three viewing modes (embedded·immersive `?view=`·**bare** = artifact origin + `rel=noreferrer`). Deep-link grammar: see #35. **v0.23 — mobile = ONE button, ONE sheet**: at ≤860px the ContextBar drops its standalone comments/versions `--mobile` toggles; the SOLE mobile entry is the surviving `data-kb-act="inspect"` button (badged with the open-comment count, `aria-controls="kb-reader-sheet"`), flipping `inspectorOpen` — re-scoped to "the mobile sheet is open in ANY panelMode". `.kb-pinsp` (full rail intact) is promoted to a fixed bottom sheet in EVERY panelMode, so inspect/comments/versions all switch from WITHIN it (mirrors desktop), never railless dead-ends. `panelMode` is orthogonal to `inspectorOpen` (the toggle handlers no longer `setInspectorOpen(false)`); `.detail--inspector-open` (mobile-CSS-only) rides all three `detail--with-*` modifiers in BOTH render paths (`detailClass` + the native-note className); a `.kb-pinsp-scrim` (`--z-scrim`) sits under the sheet (`--z-drawer`) and over the immersive FAB (`--z-float`), fixing the old sub-FAB z-order (was `--z-popover-hi`); dismiss = ✕ + scrim tap + Esc; `role=dialog`/`aria-modal` gated to mobile via the `asSheet` prop so desktop DOM + e2e are byte-identical. Deferred: multi-detent drag/snap + auto-open-to-comments. **v0.29 — TWO panes, still ONE rail**: the reader gained a two-pane artifact compare mode (`?pane2=`, `web/src/lib/paneUrl.ts`, golden-pinned, `parsePane2` TOTAL — malformed ⇒ null). Pane location is derived PURELY from the URL (no "is a split open" state to drift, #23); `pane2` is appended LAST in `artifactHref` so existing goldens are byte-unchanged. ONE `.kb-pinsp` rail for the whole reader, keyed to the FOCUSED pane; ContextBar IS per-pane (per-artifact chrome), the rail is NOT; sub-tab count stays **6**. Only an ARTIFACT may occupy a pane (four singletons block the general case: `useScrollRestoration` window-scroll assumption #31, `useRovingCursor`'s unscoped listener, `useDocumentTitle`/`lastGalleryUrl`, and the `React.lazy` split boundaries). **Per-pane visit refs live in `ArtifactPane` and origin checks MUST use exact `isOriginOfArtifact(origin,id,kb,suffix)`** (kb-qualified host, #7 v2) — suffix-only `isArtifactOrigin` passes for EVERY artifact iframe, so pane-2 beacons would POST against the wrong artifact (#8/#19). Mobile never renders pane 2. The `w`-prefix pane chords are DOC-ONLY in the registry (global scope would let `useRovingCursor`'s sibling listener also consume `h`/`l`; `Ctrl-w` is unavailable — HotkeyRoot returns early on any modifier). **Registers SUBSUME marks** (one tagged `Ref` store; two 26-slot stores would be two homes for one action) — browser-local, recorded CLI-parity exemption.
31. **Scroll restoration keys on the full URL; the in-app back must replay it (v0.20)** — `useScrollRestoration` persists the window offset (gallery+search window-scroll) in sessionStorage keyed on `pathname+search`, so the ContextBar "back to recent" MUST return to the originating gallery URL (`lastGalleryUrl`, matched on kb) or the key misses. Restore = bounded per-frame retry (waits for virtualizer height, self-terminating — never a standing render loop); the `dirty` guard stops a fast navigate-away clobbering the slot with 0; `history.scrollRestoration="manual"` in main.tsx. **W3.D/S2 amendment (sessions-rethink):** an optional `ephemeralParams: string[]` second argument strips named query params from the STORAGE KEY ONLY (`normalizeScrollKey`, pure, unit-pinned) — the URL is untouched, the key still derives purely from it. Motivating case: `/sessions?focus=<sid>` (the S2 mobile-sheet trigger) mutates the URL per row tap; without the carve-out every tap fragments the list's one scroll slot. `sessions.tsx` declares `["focus"]`; every pre-W3 call site (gallery/search) is unaffected (defaults to `[]`, a byte-identical no-op).
32. **Resilience: one confirm host, one toast surface, route ErrorBoundary (v0.20)** — `useConfirm()` (one promise-based `<ConfirmProvider>` host at app root) is the ONLY destructive prompt (never `window.confirm`); `ConfirmModal.expectedToken` optional (token path = Admin DRAIN/forget); e2e clicks `.confirm__go`. A user-action `.catch` must `toast.err` (never swallow). ErrorBoundary wraps lazy routes with `resetKey={pathname}` cleared in `componentDidUpdate` — NOT `key=` (a key remounts Detail every nav).
33. **kb-select: scope-aware pill + per-tab live URL + cold-seed lastKb (v0.20)** — active kb is a pure fn of the URL (`useExplicitKb`/`useActiveKb` off the shared `["kbs"]` query), per-tab live selection is the URL (never shared storage → two tabs never fight). Pill dims ("click to pin") on unscoped views, `.kb-ws-name` text byte-identical. `lastKb` (in `prefs.ts`, OUT of the patchSettings allow-list) is read ONCE on cold bare-`/` entry (ref-guarded, validated vs live kbs, skip when ==kbs[0]) + written on every active-kb change. `DaemonDownBanner` gates on a per-daemon connected signal (`daemons.some(d=>d.phase!=="disconnected")`), NOT the aggregate `idle` phase (which flashes on load).
34. **Permalink shells carry server-injected, escaped OpenGraph meta (v0.20)** — `serve_artifact_shell` (`spa.rs`) looks the doc up at request time (`get_by_source_path`) and splices HTML-escaped `<meta description>`+`og:*` before `</head>` on `/a/{kb}/{source_rel}`. Best-effort (any miss → plain shell); description prefers `kb-summary` else excerpt; only the parent `<head>` is touched (kb-prompt `<template>` #5 never in the shell); source-rel verbatim. XSS-critical splice/escape unit-pinned in `spa.rs`.
35. **Reader→gallery deep-links go through ONE builder; the gallery filter grammar is SPA↔wire↔server lock-step (v0.22)** — every clickable metadata atom (tag·category·folder-segment·mtime·Explore chip) builds its URL via `galleryUrl(kb, {tags,category,folder,from,to,sort,dir})` (`web/src/lib/galleryUrl.ts`, golden-pinned), never ad-hoc strings. The gallery reads `?category`/`?from`/`?to` (`gallery.tsx` + client filter), `fetchDocsPage` serialises them, and `kb_core::docs_query::matches` enforces them: `category` = exact positive `kb-category` gate, `from`/`to` = absolute **`mtime_unix`** window (NOT `since`'s indexed-or-mtime recency), both across every OR branch (route-injected, no DSL atom), unit-pinned. **W1.A** extends the same builder/wire grammar with `?read=` (csv over `never-opened|unread|in_progress|read`) — a route-side set-membership filter over the per-kb reading rollup in `routes/docs.rs::list` (NOT inside `docs_query::matches`, since the read-state signal isn't on `DocSummary`), applied after the docs_query pass and before sort+paginate. **W2.3a** adds `?ids=` (an explicit artifact-id set; a HARD filter, capped at 500 BOTH client- and server-side, 400 on overflow — never silent truncation). **v0.29**: Wave 3's three new selection surfaces all pivot through that SAME `ids=` atom rather than growing the grammar — the reflection-canvas brush resolves SERVER-side to per-lane id sets (the alternative, `comment_from`/`comment_to`/`session_from`/`session_to` on /docs AND /search, is 8 params × 2 surfaces × a 4-file lock-step), atlas lasso/map-home reuse it, and atlas search-DIMMING is deliberately NOT a filter at all (ephemeral component state — `ids=` removes rows, dimming must leave them in place; do not unify them). Over-cap degrades explicitly: creation-only brushes fall back to the `from`/`to` mtime window, mixed brushes disable the pivot and say why. Per-kb (every deep-link carries `?kb=`); facet refine-ability comes from `GET /api/kb/{kb}/facets`. **v0.33** adds `folder_exact=1` (exact folder, no descendants — `folderExact` on the builder, `docs_query::folder_matches_exact` server-side, client filter mirrored; absent = descendant-inclusive, byte-identical).

**Invariant budget — 35 is the cap.** This index (and
[docs/architecture-invariants.md](docs/architecture-invariants.md)) stay at 35
numbered slots. **Adding a new architecture invariant requires retiring or
merging an existing one** — a genuinely new load-bearing constraint means one of
the current 35 has become obsolete or subsumes/is subsumed by the newcomer; find
it, don't grow to 36. Retirement OPENS a slot the newcomer fills (retired slots
keep their number so cross-references never shift): **no slot is currently
open** — slot #2 was open after kb-tui's retirement (v0.24), was refilled by
v0.34 (kb-users), and, in the DCB milestone, was merged into #4 to make room
for the doc↔code bridge; the next invariant must retire or merge one. The cap
keeps the always-loaded "you'll break this" signal legible.

## Non-goals (recorded refusals)

kb's scope boundaries are **decisions, not omissions** — the canonical list, with
the *why* for each, is [README.md → Non-goals](README.md#non-goals). The ones an
agent working here must not accidentally violate: **no in-daemon LLM** (all
ranking/recall/digest is deterministic and reproducible; LLM steps live in the
agent layer, e.g. `/kb-reflect`) · **no CRDT/multiplayer** (comments + `.review`
are the collaboration surface) · **one trust tier — identity is attribution,
not authorization** (v0.34 re-ruling: named users exist and per-user
read-state/comment authorship/API attribution are first-class, but kb never
authenticates — no passwords/sessions/roles/read-only tokens/ACLs; every
identity is a full co-operator, memory + sessions stay shared;
`author: you|claude` is still the human↔agent role split, `user` is the
identity) · **no
in-daemon visibility/ACLs/public mode** (the corpus mount is the ACL; a live
public mirror = a dedicated daemon + edge allowlist, `kb share` for anything
crossing a trust boundary) · the **TUI is retired (v0.24)** (every tab's data
source had an SPA/CLI equivalent; fleet monitoring = SPA Settings + `kb fleet
status` + `kb events --follow` — don't rebuild a terminal dashboard) · **mobile
is a reader with a capture slot** (v0.25 share-sheet/SPA capture is a one-way
staging drop, not authoring), not an editor · the **desktop
app is archived** (`archive/kb-desktop` tag) · **no MCP server yet** (the CLI *is*
the protocol — deferred-with-trigger, a recorded ruling not an oversight) · **no
memory-benchmark arms race**. Reach for one of these before proposing a feature
that crosses it.

## Authoring artifacts (HTML destined for kb)

Canonical guide: [docs/authoring-artifacts.md](docs/authoring-artifacts.md). The
five hardest must-knows are detailed there: prompt inside `<template
id="kb-prompt">` (8 KB cap, stripped on non-loopback); real `<title>` +
`<h1>`/`<h2>` hierarchy; stable section ids (no UUIDs/timestamps); no `<base
href>` / `target="_top"` / `window.parent.*`; tags + category via `<meta>`.

## Handling SPA comments (kb-comments/1)

The full workflow — `kb find` → `kb comments list` → edit source → `reply` /
`resolve` / `reanchor`, plus the realtime `kb comments watch` loop and triage
rules — lives in [docs/comment-workflow.md](docs/comment-workflow.md). The CLI
is the only path; never edit `.review/<id>.json` directly (the daemon owns it,
ETag-protected, SPA panels listen over SSE).

## Where to find things · common pitfalls

The file-by-file code map (need → path) and the Rust/auth build pitfalls live
in [docs/architecture-invariants.md](docs/architecture-invariants.md) (sections
"Where to find things" and "Common pitfalls"). README owns the HTTP API canon.

## When to update these files

- **This file (`CLAUDE.md`)** — keep it lean: orientation, layout, build/test,
  cadence, plan workflow, the invariant **index**, pointers. A new invariant ⇒ add
  its one-liner to the index here *and* the full entry in
  `docs/architecture-invariants.md`. Also update on a crate-layout change or a
  cadence / commit-convention change.
- **[docs/architecture-invariants.md](docs/architecture-invariants.md)** — full
  invariant text, the code map, and the build pitfalls (add a pitfall once it has
  surfaced twice in a milestone).
- **[crates/kb-core/CLAUDE.md](crates/kb-core/CLAUDE.md)** — invariants that live
  entirely inside kb-core.
- **[crates/kb-code-server/CLAUDE.md](crates/kb-code-server/CLAUDE.md)** /
  **[web-code/CLAUDE.md](web-code/CLAUDE.md)** — invariants that live entirely
  inside kb-code's server or SPA respectively (its own 35-slot budget does not
  apply to these files).
- **`docs/*`** — surface specifics (authoring, comments, config, deploy, web).
  **[docs/http-api.md](docs/http-api.md)** — the HTTP API canon;
  **[docs/cli.md](docs/cli.md)** — the CLI verb list;
  **[docs/kb-code.md](docs/kb-code.md)** — the kb-code surface.
  **[README.md](README.md)** — the front door + the Non-goals canon.

Do NOT put per-feature implementation details here (code comments) or spike
findings ([docs/spike-findings.md](docs/spike-findings.md)).
