# Protocols: the versioned contract registry

kb has several plain-file and wire contracts that other programs read and write:
review sidecars, reading-list exports, slate ledgers, memory proposals, the
recall marker the hooks emit, the capture adapters' head record, the version
Hello two daemons handshake on, the bodies kb serves to kb-code, kb-code's own
review and inbox documents, and the portable session bundle. Each carries a
version string such as `kb-comments/2`. This page is the registry of those
contracts, the JSON Schemas that describe them, and the verb that checks a file
against them.

The schemas live in [`../schemas/`](../schemas/index.json), one JSON Schema
(draft 2020-12) per contract, named `<id with / as ->.schema.json`, with
`schemas/index.json` mapping each id to its schema file, kind and owner source
file.

## Checking a file: `kb validate`

```
kb validate <file>                   # schema auto-detected from the file
kb validate <file> --schema kb-comments/2
kb validate <file> --json            # {ok, schema, file, problems:[{line, pointer, message}]}
kb validate --list                   # the registry
kb validate -                        # read stdin
```

- Exit `0`: the file conforms. Exit `1`: it does not; every failing JSON pointer
  is listed (JSONL and text contracts also name the line). Exit `2`: usage - an
  unknown or undetectable schema, an unreadable file, or no file at all.
- It is a pure offline file check: no daemon, no storage, no network. The
  schemas are embedded in the binary, so an installed `kb` validates without a
  repository checkout.
- It never edits a file. The sidecars are daemon-owned (invariant 6): they change
  only through routes and CLI verbs, never by hand. `kb validate` is how you
  confirm a sidecar you suspect is damaged, or one you restored from backup, is
  still well-formed.
- Detection reads the JSON `schema` key; for a JSONL ledger the first line (or,
  for slate lines, which carry no `schema` key, their shape); a Markdown list's
  `<!-- kb-list {...} -->` header; a front-matter `schema:` line; or a
  `<!--kb-recall/1 ...-->` marker. `--schema` accepts a registered id, or a path
  ending in `.json` to check against your own JSON Schema (whole-file JSON only).
  Such a schema must be self-contained: a `$ref`/`$dynamicRef` that is not a
  same-document fragment (`#...`) is refused with exit 2, so a user schema can
  never make `kb validate` load another file or fetch a URL.

## The registry

| id | carrier | kind | owner (source of truth) |
|---|---|---|---|
| `kb-comments/1` | `.review/<artifact-id>.json` sidecar with no private note | json | `crates/kb-core/src/review.rs` |
| `kb-comments/2` | the same sidecar once it carries a private note | json | `crates/kb-core/src/review.rs` |
| `kb-list/1` | the `<!-- kb-list {...} -->` header of a Markdown reading list (a JSON list export validates too) | md-header | `crates/kb-core/src/lists.rs` |
| `kb-slate/1` | `slates/<slug>/ledger.jsonl`, one post per line | jsonl | `crates/kb-core/src/slate.rs` |
| `kb-proposal/1` | `.proposals/<id>.json` memory candidate | json | `crates/kb-server/src/routes/proposals.rs` |
| `kb-recall/1` | `<!--kb-recall/1 kb=<kb> id=<hex12>[ pos=<n>]-->` marker lines in a recall injection | text | `crates/kb-core/src/sessions/view.rs` |
| `kbc-findings/1` | findings import batch (`kb-code review findings import`) | json | `crates/kb-code-server/src/review_findings.rs` |
| `kbc-findings/2` | findings v2 sidecar (`review compose --findings`, `~review/findings.json`) | json | `crates/kb-code-server/src/review_doc/mod.rs` |
| `kbc-review-context/1` | `GET /api/reviews/{id}/context` body | json | `crates/kb-code-server/src/review_context.rs` |
| `kbc-github-export/1` | `GET /api/reviews/{id}/export/github` body | json | `crates/kb-code-server/src/review_github_export.rs` |
| `kbc-cmd/1` | kb-code's command registry | json | `crates/kb-code-server/commands/registry.json` |
| `kbc-theme/1` | kb-code's theme registry | json | `crates/kb-code-server/themes/registry.json` |
| `kb-sibling/1` | the version Hello fields of `GET /api/identity` on kb and on kb-code | json | `crates/kb-core/src/sibling.rs` |
| `coderef/1` | `GET /api/kb/{kb}/docs/{id}/code-refs` body, kb to kb-code | json | `crates/kb-server/src/routes/coderefs.rs` |
| `coderef-feed/1` | `GET /api/kb/{kb}/code-refs` cursor feed body, kb to kb-code | json | `crates/kb-server/src/routes/coderefs.rs` |
| `unified-inbox/1` | `GET /api/inbox` body, kb-code daemon to its CLI and SPA | json | `crates/kb-code-server/src/unified_inbox.rs` |
| `kbc-claim/1` | one agent-prose claim: the `POST /api/claims` body and the claim the routes answer with | json | `crates/kb-code-server/src/claims.rs` |
| `kb-capture-grok/1` | the `adapter-meta` first line of a captured grok transcript, capture hook to kb daemon | jsonl-head | `plugins/kb-memory/hooks/kb-capture-grok.sh` |
| `kb-session-bundle/1` | `manifest.json` inside a `<sid>.kbsession.zip` (extract it first, then `kb validate -`) | json | `crates/kb-core/src/session_bundle.rs` |

`kb-recall/1` is a text grammar, not JSON, so its registry entry carries a
`pattern` and `rules` instead of a schema file, and `kb validate` checks every
marker line in the file: `kb` required and non-empty, `id` required and exactly
12 lowercase hex characters, `pos` optional and an integer in 1..=99, unknown
`key=value` pairs and bare tokens ignored (the reader ignores them too, which is
what keeps the grammar forward-compatible). The `pos` range and the marker
framing are the reader's own constants (`kb_core::sessions::view`), imported by
the validator rather than copied.

`jsonl-head` means only the first non-empty line of the file is the contract
(a transcript whose head line is an adapter record); `kb validate` checks that
line and nothing else. `kbc-claim/1` is one claim; the `GET /api/claims` list
envelope (`{schema, total, returned, claims[]}`) is not a claim, so validate each
element of `claims`.

## Inventory of every versioned contract string

Every `"<name>/<n>"` string in `crates/` and `plugins/` falls in exactly one
class below. The classes are by who reads the document, not by how big it is.
A class is not a promise that the contract is stable: it says which gate, if
any, pins its shape.

1. **Registered** (schemas in `schemas/`, validated by `kb validate`, drift
   tested from real code): `coderef-feed/1`, `coderef/1`, `kb-capture-grok/1`, `kb-comments/1`, `kb-comments/2`, `kb-list/1`, `kb-proposal/1`, `kb-recall/1`, `kb-session-bundle/1`, `kb-sibling/1`, `kb-slate/1`, `kbc-claim/1`, `kbc-cmd/1`, `kbc-findings/1`, `kbc-findings/2`, `kbc-github-export/1`, `kbc-review-context/1`, `kbc-theme/1`, `unified-inbox/1`.
2. **SPA-consumed** (81). The reader is the web SPA (`web/` or
   `web-code/`). Where the Rust response type derives `ts_rs::TS` the
   wire-binding drift gate (kb's `types-check`, kb-code's `just gen-ts-code-check`;
   CI `drift` and `code-drift`) pins its field set; where the daemon builds the body with
   `json!` the SPA's hand-written types are the only mirror. These are NOT
   schema-registered: `aug-lane/1`, `bookmarks/1`, `branch-conflicts/1`, `branch-facts/1`, `branches/1`, `canvas/1`, `code-actions/1`, `codelens-pin/1`, `codelens-scorecard/1`, `codelens/1`, `comments/1`, `commit/1`, `compare/1`, `defs/1`, `diagnostics/1`, `diff/1`, `doc-refs/1`, `entities/1`, `entity/1`, `file-history/1`, `framework-edges/1`, `hierarchy/1`, `highlight-batch/1`, `highlight/1`, `hover/1`, `impact/1`, `join/1`, `kbc-actions/1`, `kbc-agent-authors/1`, `kbc-canvas/1`, `kbc-credentials/1`, `kbc-frames/1`, `kbc-github-threads/1`, `kbc-hunk-turns/1`, `kbc-hunkid/1`, `kbc-pseudo/1`, `kbc-question-state/1`, `kbc-recipe-run/1`, `kbc-recipe/1`, `kbc-refs/1`, `kbc-review-report/1`, `kbc-review-retrack/1`, `kbc-review-since/1`, `kbc-scope/1`, `kbc-store/1`, `kbc-tour/1`, `kbc-trail/1`, `kbc-tree/1`, `kbcq/1`, `lenses/1`, `map/1`, `merge-check/1`, `pr-checks/1`, `pr-comments/1`, `pr-detail/1`, `pr-reviews/1`, `prs/1`, `rails-lens/1`, `rails/1`, `range-diff/1`, `recipes/1`, `refs/1`, `repo-state/1`, `resolve/1`, `review-comments/1`, `review-findings/1`, `review-impact-file/1`, `review-inbox/1`, `review-map/1`, `review-reading-order/1`, `review-timeline/2`, `reviews/1`, `scrub/1`, `session-diff/1`, `session-replay/1`, `session-view/1`, `sets/1`, `stacks-layer-diff/1`, `stacks/1`, `syntax/1`, `usages/2`.
3. **Cross-boundary, not yet registered**. A different process or product reads
   it, so a registry entry would be right, and none exists yet. Each carries the
   reason:
   - `lip/1`: kb-lip to kb-code, two binaries. Seven endpoints
     (`identity`, `hover`, `definition`, `references`, `symbols`,
     `diagnostics`, `code-actions`) whose bodies are built with `json!` in
     `crates/kb-lip/src/http.rs`; an honest drift test needs one real round trip
     per endpoint against the fake LSP, which is its own lane. The blob-hash
     guard and the 1-based-line/byte-column codec are covered by kb-lip's own
     tests.
   - The other capture adapters (`kb-capture-codex/1`, `kb-capture-kimi/1`, `kb-capture-omp/1`, `kb-capture-opencode/1`): the same family as
     `kb-capture-grok/1`, each with its own key set in its own jq template.
     Register them with the same template-versus-schema parity test the grok
     one has.
   - `kbc-review/1`: the agent-authored review document (YAML
     front matter plus a JSON block, parsed by `review_doc`). `kb validate` has
     no front-matter-document kind yet.
   - kb-code daemon to its own CLI (31, same product, released
     together; the CLI is the protocol): `doclens-sync/1`, `fingerprint-verify/1`, `kbc-api-schemas/1`, `kbc-compose/1`, `kbc-legacy-import/1`, `kbc-prose-refs/1`, `kbc-review-base-explain/1`, `kbc-review-cat/1`, `kbc-review-diff/1`, `kbc-review-find/1`, `kbc-review-log/1`, `kbc-review-retrack-all/1`, `kbc-review-sync/1`, `kbc-seq/1`, `kbc-store-export-legacy/1`, `kbc-store-gc/1`, `kbc-store-legacy-refs/1`, `kbc-store-maintain/1`, `kbc-store-restore/1`, `kbc-store-sync/1`, `lane-ingest/1`, `outline/1`, `parity/1`, `reextract-bill/1`, `rehearsal/1`, `review-distill/1`, `review-findings-recurrence/1`, `review-lint/1`, `review-refs/1`, `review-render/1`, `usages/1`. They are response
     envelopes and are registered one at a time when something outside the
     product starts to read them; `unified-inbox/1` and `kbc-claim/1` are the
     pattern.
4. **Internal** (43): referenced from one crate only, so no other
   program reads them: `behavioral-fusion/1`, `behavioral/1`, `branch-favourites/1`, `branch-review/1`, `codelens-path/1`, `codelens-pins/1`, `coderef-lint/1`, `haml/1`, `kb-code-store/1`, `kb-slo/1`, `kbc-agent-queue/1`, `kbc-audit/1`, `kbc-backup/1`, `kbc-brief/1`, `kbc-cli-tools/1`, `kbc-compare-file/1`, `kbc-doctor/1`, `kbc-prose-refs-golden/1`, `kbc-refs-typeahead/1`, `kbc-restore-guard/1`, `kbc-review-job/1`, `kbc-review-snapshot/1`, `kbc-review-start/1`, `kbc-review-status/1`, `kbc-review-sync-open/1`, `kbc-review-verify/1`, `kbc-sibling/1`, `kbc-store-doctor/1`, `kbc-store-members/1`, `kbc-workspace/1`, `kbc-worktree-loss/1`, `kbc-worktree-prune/1`, `kbc-worktree-readiness/1`, `kbc-worktree/1`, `pack/1`, `resolve-symbol/1`, `review-analytics/1`, `review-impact/1`, `review-pr-status/1`, `review-risk/1`, `review-sweep/1`, `scopes/1`, `similar/1`.
5. **Negative fixtures, not contracts** (8): strings that exist
   only to be refused by a test (a future version, a wrong major): `kb-comments/3`, `kb-comments/99`, `kb-proposal/2`, `kb-sibling/2`, `kb-sibling/9`, `kb-slate/2`, `kbc-claim/2`, `lip/2`.
   (`omp/0001` and `other/1` also match the pattern but are a migration name and a
   rejected-schema test string, not contracts.)

## Where each gate lives

- Registered contracts that kb-core, kb-server or a plugin owns are drift tested
  by `crates/kb-cli/tests/schema_registry.rs`.
- Registered contracts that kb-code-server owns are drift tested by
  `crates/kb-code-server/tests/schema_drift.rs` and by the route tests that fetch
  the real body (`tests/review/*`, `tests/boot_e2e/boot.rs`), through the
  helpers in `crates/kb-code-server/tests/common/mod.rs`. Output types are
  serialized as the routes do and validated; input types are compared two ways:
  the keys serde requires must equal the schema's `required`, and every key
  serde accepts must be a declared property. Renaming a field, or adding a
  required one, in Rust without moving the schema fails a named test.
- The `kb-recall/1` id rule is one function, `kb_core::sessions::view::is_recall_marker_id`,
  called by the reader and by `kb validate`; a test in each crate fails if they
  ever disagree.

## Compatibility rules

- **`kb-comments/1` and `/2` are one document shape with two stamps.** The stamp
  is derived at save time: a sidecar with at least one private comment is written
  as `/2`, anything else as `/1`, byte-identical to what every earlier release
  wrote. A binary from before private notes accepts exactly `/1`, so it refuses a
  `/2` file instead of loading it, ignoring the `private` flag and re-saving the
  notes as public. The two schemas encode that: a `/1` file that contains a
  private comment is invalid, and a `/2` file must contain one.
- **Additive keys are tolerated.** Where the Rust type uses `serde(default)` the
  schema leaves the key optional and the object open; `tags`/`private` on a
  comment and unknown pairs in a recall marker are the standing examples. A new
  optional key is not a new version.
- **A new version is a new contract.** An incompatible change bumps the number,
  adds a schema file and an index entry, and keeps the old entry for as long as
  any binary can still write or read it.
- **Findings `/1` and `/2` are different documents.** `/1` is the import payload
  (every finding names its `f-` slug); `/2` is the sidecar (slug optional, plus
  `act`, `blocking`, `cites`, `supersedes`).

## What the schemas are, and are not

The schemas are hand-written structural lints derived from the serde structs. They
are not generated from Rust and they are not a second source of truth: when
`kb validate` and the daemon disagree, serde is right and the schema is the bug.
Section element shapes that are composed from other reads (the threads and
findings inside a review-context bundle) are deliberately left open.

Two tests keep the registry honest, both in `crates/kb-cli/tests/schema_registry.rs`:

- Sample documents are produced by the real code (a sidecar saved through
  kb-core, a list through `md::to_markdown`, a ledger line through `Post::mint`,
  a proposal through its struct, the identity and code-refs structs of
  kb-server, a session manifest through `BundleManifest`) and validated through
  the shipped binary, so a serde change that breaks a schema fails the build.
  kb-code's contracts are validated from real kb-code-server values by
  `crates/kb-code-server/tests/schema_drift.rs`, and everything also has
  hand-written fixtures under `schemas/fixtures/<id>/{valid,invalid}/`.
- Every `const ...SCHEMA...` string in any crate that belongs to a registered
  family must have a registry entry, so adding `kb-comments/3` without a schema
  fails.

## Adding or changing a contract

1. Edit or add `schemas/<id>.schema.json` (draft 2020-12, self-contained: no
   remote `$ref`) and its `schemas/index.json` entry.
2. Add the file to the embedded table in `crates/kb-cli/src/commands/validate.rs`.
3. Add fixtures under `schemas/fixtures/<id>/`: at least one valid, and invalid
   ones named `wrong-schema`, `missing-key` and `wrong-type`.
4. Run the registry tests; they name the missing piece.
