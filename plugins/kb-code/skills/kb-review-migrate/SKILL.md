---
name: kb-review-migrate
description: Migrate ONE legacy review HTML artifact into a kbc-review/1 document plus a findings v2 sidecar, using only the artifact's embedded machine JSON block — never its prose. Use when the operator asks to migrate/import an old PR-review artifact into kb-code, or points at a `PR-*.html` review file and wants it in the Review Room. This skill prepares and lints; the operator runs `compose`.
---

# /kb-review-migrate — one legacy artifact, one document

A legacy review artifact is an HTML page: prose, headings, styled finding
cards, and — crucially — an embedded `<script type="application/json">`
block carrying the machine record the page was rendered from. This skill
turns ONE such artifact into the two inputs `kb-code review compose` takes:
a `kbc-review/1` Markdown document and a findings v2 sidecar.

**Contract — read these before you start:**

- **Only the JSON block. Never the prose.** This is design ruling D9-a and
  it is not negotiable. A finding recovered from a `<section class="finding">`
  is a GUESS, and a guessed finding goes on to inherit a slug, a human's
  disposition and a place in a GitHub thread. `kb-code review import-legacy`
  enforces this structurally — it does not parse the DOM at all. **You must
  not "help" by reading the page and adding what the block missed.** If the
  block is thin, the migration is thin, and that is the honest outcome.
- **No machine block ⇒ no migration.** `import-legacy` exits 3 naming the
  block ids it looked for. Report that to the operator and stop. Do not
  hand-write a document from the prose and present it as a migration.
- **You never run `compose`.** This skill prepares, lints and hands over the
  exact command. Composing is a loopback WRITE against a review the operator
  owns, and the last look at a machine translation belongs to a human.
- **One artifact per run.** A batch loop belongs to the operator's shell,
  not to a skill that must inspect each result.

## The loop

### 1. Establish the target

You need a review id to compose into, and the artifact path.

```bash
kb-code review list --repo <REPO> --json          # is there already a review?
kb-code review show <ID> --json                   # confirm base/head + patchsets
```

If no review exists for the PR the artifact describes, tell the operator —
creating one (`review start-pr`) changes git refs and is theirs to run.

### 2. Extract and map

```bash
kb-code review import-legacy <artifact.html> \
    --review <ID> \
    --out-doc /tmp/<slug>-review.md \
    --out-findings /tmp/<slug>-findings.json \
    --json
```

Read the JSON. Four fields decide whether this migration is honest:

| field | what to do with it |
|---|---|
| `block_id`, `block_bytes` | Confirm a real block was read. |
| `mapping[]` | Every stated substitution (`severity "spicy" → "concern"`). Each one is a place the legacy vocabulary did not fit; check none is nonsense. |
| `skipped[]` | Findings NOT migrated, with reasons — almost always "no location". **Report every one to the operator by title.** These are the migration's real losses. |
| `notes[]` | Degrades the importer stated (no summary in the block, prose not scraped, …). |

If `mapping[]` looks wrong for this artifact family, re-run with `--strict`:
it refuses instead of substituting, which tells you exactly which value the
importer cannot express.

### 3. Lint before you hand over

```bash
kb-code review lint <ID> --doc /tmp/<slug>-review.md \
    --findings /tmp/<slug>-findings.json --json
```

This is a `compose --dry-run` under the hood, so a document that lints clean
composes cleanly. Exit 3 means the lint reported ERRORS — read `rows[]`:

- `ref_orphan` / `ref_wrong_patchset` — a migrated ref points at code this
  patchset does not have. Usually the artifact reviewed an older head.
  **Do not delete the ref to make the lint pass.** Say so; the operator may
  want to `review snapshot` first.
- `finding_no_location` — should not appear (the importer skips those), and
  if it does, something is wrong with the mapping, not with the document.
- `duplicate_fingerprint` — the legacy block carried two findings the
  reconciler cannot tell apart. Report both titles; the operator decides.

Fix only what is genuinely a translation defect (a mangled YAML scalar, a
path with a stale prefix). Never fix a lint by weakening a claim.

### 4. Hand over — do not compose

Print, verbatim, the `next` line `import-legacy` gave you, plus your report:

```
Migrated <artifact> → <doc> + <findings>
  block:    <block_id> (<n> bytes)
  findings: <n> migrated, <n> skipped
  mapped:   severity "critical" → "blocker" (2 rows)
  skipped:  "A finding with no location at all" — no location path
  lint:     0 errors, 2 warnings (ref_malformed ×2 — see rows)

Run when you're happy with it:
  kb-code review compose <ID> --doc <doc> --findings <findings> --tier standard --dry-run
  kb-code review compose <ID> --doc <doc> --findings <findings> --tier standard
```

## The mapping table

What `kbc-legacy-import/1` does, field by field. Keys are tried in the order
listed; the first non-empty one wins.

| kbc-review/1 target | legacy keys tried | rule |
|---|---|---|
| `summary_md` | `summary_md`, `summary`, `overview`, `abstract` | Verbatim. Absent ⇒ a stated placeholder that SAYS it is one, plus a `notes[]` entry. |
| `risk.level` | `risk` (object `{level, why}` or string), else `verdict`/`recommendation`/`conclusion` | `low`/`medium`/`high` (`moderate`→medium, `critical`→high). From a verdict: `request-changes`→high, `comment`→medium, `approve`→low. An unmappable value yields NO risk block rather than a guessed one. |
| `blocks.context` | — | Written by the importer: names the block it read, the counts, and that the prose was not scraped. |
| finding list | `findings`, `issues`, `items`, `comments` | First array wins. |
| `finding.title` | `title`, `summary`, `headline`, `name` | Missing ⇒ SKIPPED. |
| `finding.location.path` | `path`, `file`, `filename`, `location` | Missing ⇒ SKIPPED (never invented). |
| `finding.location.lines` | `line` (scalar) or `lines` (array) | Present ⇒ `kind: lines`; absent ⇒ `kind: whole_file`. |
| `finding.severity` | `severity`, `level`, `impact` | `blocker`/`concern`/`ok` pass through. Declared aliases: `critical`/`high`/`blocking`/`must-fix`→blocker · `medium`/`warning`/`warn`/`minor`/`low`→concern · `nit`/`nitpick`/`info`/`note`/`praise`/`pass`→ok. Anything else → `concern` **with a stated `mapping[]` row** (or a refusal under `--strict`). |
| `finding.act` | — | Derived by ONE rule: `ok`→`note`, everything else→`issue`. The legacy shape has no act axis. |
| `finding.blocking` | — | `severity == "blocker"`. Findings v2 keeps these separate on purpose; the legacy shape conflated them, so this is the honest reconstruction. |
| `finding.category` | `category`, `kind`, `type` | Must be one of correctness/security/performance/design/tests/docs/style/other; anything else → `other` with a `mapping[]` row. |
| `finding.rationale` | `rationale`, `detail`, `details`, `description`, `body` | Verbatim, with `(migrated from legacy finding \`<id>\`)` appended when the block carried an id. |
| `finding.recommendation` | `recommendation`, `fix`, `suggestion`, `remedy` | Verbatim. |
| `finding.slug` | — | **Never carried across.** A legacy id is not a kbc slug: slugs are minted from this review's own monotonic ledger and are never reused (D9). The legacy id survives in the rationale so a row is still traceable. |
| `finding.cites` / `supersedes` / `evidence` | — | Always empty. A cite is a kbc ref, and the legacy block has none. |

Accepted machine-block ids, in probe order: `kb-review-data`, `review-data`,
`pr-review-data`, `kbc-review`, `review-json`. A `<script
type="application/json">` with any other id is ignored — that is deliberate,
so an analytics blob on the same page can never be mistaken for the record.

## What this skill will not do

- Read the artifact's prose (D9-a).
- Invent a location, a slug, a severity or a summary.
- Run `compose`, `snapshot`, `verdict` or any GitHub call.
- Migrate more than one artifact per run.
- "Improve" a migrated finding by re-reviewing the code. If the operator
  wants a fresh review, that is `/kb-review-work` or a new review, not a
  migration wearing one's clothes.
