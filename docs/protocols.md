# Protocols: the versioned contract registry

kb has several plain-file and wire contracts that other programs read and write:
review sidecars, reading-list exports, slate ledgers, memory proposals, the
recall marker the hooks emit, and kb-code's review documents. Each carries a
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

## The registry (v1)

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

`kb-recall/1` is a text grammar, not JSON, so its registry entry carries a
`pattern` and `rules` instead of a schema file, and `kb validate` checks every
marker line in the file: `kb` required and non-empty, `id` required and exactly
12 lowercase hex characters, `pos` optional and an integer in 1..=99, unknown
`key=value` pairs and bare tokens ignored (the reader ignores them too, which is
what keeps the grammar forward-compatible). The `pos` range and the marker
framing are the reader's own constants (`kb_core::sessions::view`), imported by
the validator rather than copied.

Other versioned strings exist (`kb-sibling/1`, `lip/1`, `unified-inbox/1`, the
`kbc-store` family, `kbc-tour/1`, the many read-route envelopes). They are API
response shapes with no standalone file carrier and are not yet registered.

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
  a proposal through its struct) and validated through the shipped binary, so a
  serde change that breaks a schema fails the build. kb-code's contracts are
  pinned by their committed goldens and registries plus hand-written fixtures
  under `schemas/fixtures/<id>/{valid,invalid}/`.
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
