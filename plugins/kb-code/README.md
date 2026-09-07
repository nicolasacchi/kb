# kb-code

Claude Code integration for the **kb-code** daemon (`kb-code-server`) — the
sibling read-oriented code-browsing daemon (design v4, "the read-first IDE").
Three pieces:

- **`kb-code-why.sh`** (`hooks/`) — a `PreToolUse` hook that injects
  trailer/exact-confidence provenance ("which session wrote this, and why")
  right before Claude edits a file it can attribute. LLM-free, fail-open,
  never fuzzy.
- **`kb-code-annotations.sh`** (`hooks/`) — a `PreToolUse` + `SessionStart`
  hook (D4) that surfaces the operator's `flag-for-agent` review
  annotations into the same sessions, turning a reader-side "flag for
  agent" into a live human-to-agent code-review dialogue.
- **`kb-code hook install|uninstall|status`** (kb-code-cli) — prints the
  `settings.json` wiring for BOTH hooks and checks whether each is live;
  never edits your settings.json for you.

## The why-hook

| Hook | Event | What it does |
|---|---|---|
| `kb-code-why.sh` | `PreToolUse` (matcher `Edit\|Write`) | Resolves the file being edited to a configured kb-code repo (longest-prefix match against `GET /api/repos`, 5-min cached), queries `GET /api/why` (line-grade when an `Edit`'s `old_string` locates a line, else file-grade), and — **only** on `confidence: trailer` or `confidence: exact`, never `fuzzy`/`none` — injects a 3-5 line block: which session wrote it, when (from the covering blame region), its top recorded decision (line-grade only), and a link to the session in kb. |

Injected block shape:

```
kb-code provenance — src/lib.rs:
- Session: fixed the gizmo race (committed 2026-07-02)
- Decision: use a trailer instead of a squash-subject match
- kb session: http://127.0.0.1:4000/sessions?focus=<session_id>&kb=<kb>
```

(`&kb=<kb>` is appended only when the line-grade response resolved one —
see "Known limitation" below.)

**Verified injection mechanism** (see the header comment in
[`hooks/kb-code-why.sh`](hooks/kb-code-why.sh) for the full note): current
Claude Code hooks docs (`code.claude.com/docs/en/hooks`) confirm `PreToolUse`
supports `hookSpecificOutput.additionalContext`, matched on `tool_name` with
the target file at `tool_input.file_path` — the same
`hookSpecificOutput`/`additionalContext` mechanism kb-memory's
`UserPromptSubmit` `kb-recall.sh` already uses in production, just on a
different event. No fallback event was needed.

**Caps + dedupe**: at most 3 injections per Claude Code session; a given
`(repo, path)` pair is injected at most once per session — both tracked in
`~/.cache/kb-code/why-hook-seen-<session_id>` (one line per injection; its
line count IS the counter).

**Fails open everywhere**: missing `jq`/`curl`, kb-code daemon unreachable,
malformed JSON, no repo match, `fuzzy`/`none` confidence, cap hit, dedupe
hit — every one of these exits `0` with **no** output; the Edit/Write
proceeds completely unaffected. A 1.5s `curl --max-time` bounds each of the
(at most two) network calls per invocation.

### Env

| Var | Default | Purpose |
|---|---|---|
| `KB_CODE_WHY_HOOK=off` | unset | Kill switch — the hook exits immediately. |
| `KB_CODE_DAEMON_URL` | `http://127.0.0.1:4747` | The kb-code daemon (provenance API). |
| `KB_DAEMON_URL` | `http://127.0.0.1:4000` | The kb daemon — used only to build the "kb session" deep link (same env var kb-memory's `kb-recall.sh` already uses for the same daemon). |

### Known limitation

`GET /api/why`'s line-grade response carries which **kb corpus** a resolved
session lives in on `.attribution.kb` (see
`crates/kb-code-server/src/provenance/why.rs`'s `AttributionOut`) — best
effort, absent when neither the join ladder nor its loopback-only
enrichment follow-up resolved one. The **file-grade** response
(`.sessions[]`, no `line`) carries no such field at all. The "kb session"
link is scoped with `?kb=` when `.attribution.kb` is present, else falls
back to `?focus=` alone — not always corpus-pre-scoped.

## The annotations hook (D4)

| Hook | Event | What it does |
|---|---|---|
| `kb-code-annotations.sh` | `PreToolUse` (matcher `Edit\|Write`) | Resolves the edited file to its configured repo (SAME longest-prefix match + repos cache as `kb-code-why.sh` — one shared cache file, not a second one), queries `GET /api/annotations/open?repo=&intent=flag-for-agent`, client-filters to the annotations anchored on THIS file, and injects up to 2 per call with the exact commands to close the loop. |
| `kb-code-annotations.sh --session-start` | `SessionStart` (matcher `startup\|resume\|clear`) | If the session's cwd sits inside a configured repo with ANY open `flag-for-agent` annotations, emits ONE summary line (no per-annotation dump). |

PreToolUse injected block shape (one per annotation, blank-line separated —
at most 2 per call):

```
kb-code operator flag on src/lib.rs:42 (id ann_1a2b3c):
- "this loop is O(n^2), please fix before merging"
- after addressing it: kb-code annotate reply ann_1a2b3c -m "<what you did>" --repo myrepo --path src/lib.rs && kb-code annotate resolve ann_1a2b3c
- (thread has 2 replies — read them first: kb-code annotations src/lib.rs --repo myrepo)
```

SessionStart summary shape (always exactly one line):

```
kb-code: 3 operator flag(s) awaiting action in myrepo — list: kb-code annotations open --repo myrepo --intent flag-for-agent
```

**Caps + dedupe**: at most 2 annotations injected per PreToolUse call, at
most 6 DISTINCT annotation ids injected per Claude Code session — tracked in
`~/.cache/kb-code/annotations-hook-seen-<session_id>` (one line per injected
annotation id; its line count IS the counter). An annotation already shown
this session is never repeated, but a file with more open flags than fit in
one call discloses the rest on a later edit rather than hiding them forever.
`SessionStart`'s own summary line is never deduped (mirrors `kb-wake.sh`).

**Fails open everywhere**, same posture as `kb-code-why.sh`: missing
`jq`/`curl`, kb-code daemon unreachable, malformed JSON, no repo match, no
open `flag-for-agent` annotations, cap hit — every one of these exits `0`
with **no** output.

### Env

| Var | Default | Purpose |
|---|---|---|
| `KB_CODE_ANNOTATIONS_HOOK=off\|0\|false\|no` | unset | Kill switch — the hook exits immediately. |
| `KB_CODE_DAEMON_URL` | `http://127.0.0.1:4747` | The kb-code daemon — SAME var `kb-code-why.sh` uses, so both hooks share one `GET /api/repos` cache for a given daemon url. |

## Beyond the hook: `pack` for pre-load context

The hook only fires on Edit/Write — it never injects anything when an agent
is *pre-loading* files before a multi-file change. That's what `kb-code
pack` (`GET /api/pack`) is for: one budget-rationed call per file set
returning `{outline, provenance (top sessions by line coverage), story,
content}` — richer than `cat`-ing the same files at comparable token cost.
An agent planning edits across several files should call

```bash
kb-code pack src/a.rs src/b.rs --repo <name> --budget 8000 --json
```

up front, then rely on the hook's per-edit provenance nudges afterwards.
The two are complementary, not alternatives: `pack` = deliberate pre-read,
the hook = ambient attribution at write time.

## Install — pick one

### Mode 1: manual (settings.json)

```sh
mkdir -p ~/.claude/hooks
cp plugins/kb-code/hooks/kb-code-why.sh ~/.claude/hooks/
cp plugins/kb-code/hooks/kb-code-annotations.sh ~/.claude/hooks/
chmod +x ~/.claude/hooks/kb-code-why.sh ~/.claude/hooks/kb-code-annotations.sh
```

Then merge the `hooks` block from
[`hooks/settings.sample.json`](hooks/settings.sample.json) into
`~/.claude/settings.json` (global) or a project `.claude/settings.json`.

`kb-code hook install` prints exactly this snippet (and the plugin-manifest
path for Mode 2) — it never writes to your settings.json itself:

```sh
kb-code hook install
```

### Mode 2: Claude Code plugin

`kb-code` is one plugin in this repo's marketplace
([`.claude-plugin/marketplace.json`](../../.claude-plugin/marketplace.json)).
Its manifest is at
[`.claude-plugin/plugin.json`](.claude-plugin/plugin.json) and the hook
config at [`hooks/hooks.json`](hooks/hooks.json) (paths use
`${CLAUDE_PLUGIN_ROOT}`, which resolves to `plugins/kb-code/` once
installed).

```sh
claude --plugin-dir /path/to/kb/plugins/kb-code
```

…or add the marketplace and install:

```
/plugin marketplace add /path/to/kb
/plugin install kb-code@kb-plugins
```

### Status

```sh
kb-code hook status
```

Checks, for BOTH hooks, whether the script exists at the expected install
path(s), and whether a kb-code daemon is reachable at
`KB_CODE_DAEMON_URL`/the default —
does **not** parse `~/.claude/settings.json` for you (there's no single
canonical location once plugins are in play); it reports what it can verify
independently and tells you what to check by hand.

## `kb-code bench-search` — retrieval bench

`kb-code bench-search --queries <file.jsonl> [--repo NAME] [--daemon URL]
[--limit N] [--json]` runs each query in the file against the unified
`GET /api/search` box, and reports recall@1/@5 (per lane + overall — a hit
is `expect_path` appearing in a lane's top-k) and p50/p95 latency per lane.

Each line of the queries file is `{"query": "...", "expect_path": "...",
"expect_kind": "..."}` (`expect_kind` is an optional free-form label used
only for the human table's grouping, not scoring). A starter set for the kb
repo itself ships at
[`../../crates/kb-code-cli/bench/kb-repo-queries.jsonl`](../../crates/kb-code-cli/bench/kb-repo-queries.jsonl).

**Governance note (ADR-6)**: this bench's `--json` output is what governs
whether `[semantic] enabled` flips to `true` by default — see
`kb-code bench-search --help`. Flipping that default requires a recorded
bench run, not a vibe check.

## `kb-code scip ingest` — the SCIP precision tier

`GET /api/resolve`'s tags-tier/occurrence-tier ranking is a heuristic (no
type/scope resolution — see `resolve`'s own module doc). For a repo where a
real language server or compiler front-end can produce a
[SCIP](https://sourcegraph.com/blog/announcing-scip) index, ingesting it adds
an exact, ranked-first `"scip-exact"` tier on top:

```bash
# Rust — run at the repo root.
rust-analyzer scip .

# TypeScript / JavaScript — run at the repo root.
scip-typescript index
```

Both write `index.scip` by default. Then:

```bash
kb-code scip ingest index.scip --repo <NAME> [--daemon URL] [--batch-size N]
```

The CLI parses the index and POSTs already-mapped occurrence rows to the
daemon (`POST /api/scip/ingest`, LOOPBACK-ONLY — same gate as `checkout`/
`session-diff`), which resolves each document's path to its CURRENT blob and
replaces that blob's SCIP-sourced occurrence rows. A document whose file has
drifted since the index was generated (or that this daemon has never
indexed, or whose language it doesn't recognise) is skipped and reported
honestly, never ingested against out-of-date positions — the CLI prints the
skip counts and reasons. Re-run `scip ingest` any time the code changes
meaningfully; there's no watch mode.

## Prerequisites

- `kb-code` (kb-code-cli's binary) and a running `kb-code-server` daemon.
- `jq` and `curl` (used by both hooks).
- Optionally a running `kb` daemon (`KB_DAEMON_URL`) for the why-hook's "kb
  session" link and the join ladder's kb-side arms — the hook and daemon
  both degrade gracefully without one. `kb-code-annotations.sh` needs only
  the kb-code daemon.

## Skills

- **`/kb-review-work`** (`skills/kb-review-work/`) — the agent half of the
  Review Room loop (kb v0.39 "The PR Room"): drain a review's questions and
  dispositions with code-verified answers, land agreed fixes/suggestions,
  re-import on PR drift, and run the manual GitHub publish round from
  `review export-github`'s payload — recording every post via
  `review publish` so nothing double-posts. The daemon stores; the agent
  authors; `gh` runs only at the agent layer, only when asked.

- **`/kb-review-migrate`** (`skills/kb-review-migrate/`) — the one-shot
  migration from a LEGACY review HTML artifact to a `kbc-review/1` document
  plus a findings v2 sidecar (kb-code v7.3, design D9/D9-a). It runs
  `kb-code review import-legacy`, which reads ONLY the artifact's embedded
  `<script type="application/json">` machine block and never its prose, then
  lints the result via `review lint` and hands the operator the exact
  `review compose` line. It never composes, never invents a location or a
  slug, and reports every skipped finding by title — the artifact's prose is
  decoy by design, and a scraped finding would inherit a slug, a
  disposition and a GitHub thread it never earned.
