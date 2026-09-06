---
name: kb-verify
description: Sweep code-citing kb memories against the live checkout via kb-code's doclens and file dated [kb-drift] comments on citations that no longer resolve — plus a born-stale check and an escalation path to kb memory flag when the FACT itself is wrong. Use on a cadence (weekly, or after a large refactor lands), when the user asks whether memory still matches the code, or when a recall hit cites code you know just moved. Dedups to one open comment per (memory, path). Supports --dry-run.
---

# /kb-verify — does memory still match the code?

Memories cite code (`path:line`, `Class#method` — the DCB hint grammar,
invariant #2). Code moves; memories don't. This skill is the **agent-layer
sweep** (sanctioned LLM tier — it may call BOTH CLIs, `kb` and `kb-code`) that
closes the gap: resolve every code-citing memory's refs against a real
checkout, and turn each hard miss into a **dated `[kb-drift]` comment** on the
memory — landing in the one inbox that already has a triage habit
(kb-comments/1, invariant #6), and feeding resurface's comment lane for free.

**Contract — verification writes COMMENTS, never memory mutations:**

- **Write vocabulary = COMMENT · REPLY · FLAG-escalate. NEVER** `kb forget`,
  never `--supersedes`, never a meta edit — consolidation belongs to
  `/kb-reflect` and the operator. You are a smoke detector, not a demolition
  crew.
- **`[kb-drift]` ≠ `[kb-flag]`.** A drift comment says *the citation rotted*
  (the fact may still be true). Only when verification shows the **fact
  itself** is now wrong (the cited code's current behavior contradicts the
  memory's claim) do you escalate: `kb memory flag <id> --reason "…"` — which
  refuses if an open flag already exists, so it self-dedups.
- **Dedup: at most ONE open `[kb-drift]` comment per (memory, cited path).**
  Re-runs must be no-ops on already-reported drift.
- **Honest states, never guesses.** kb-code unreachable → report "not
  verifiable this run" and stop; NEVER file drift you didn't observe.
  `ambiguous` resolution → report-only, never a comment (a maybe is not a
  finding).

## Arguments

- `--kb <NAME>` — sweep one memory corpus (default: every kb whose docs carry
  code refs — discover, don't assume).
- `--limit <N>` — cap the number of memories verified this run (default 25,
  newest-recalled first — verify what agents are actually being told).
- `--dry-run` — full pipeline, print the findings table, write **nothing**.

## Step 1 — build the sweep set

Memories that cite code, via the kb-local `code_refs` tables (never a kb-code
call for discovery — the refs live kb-side):

```bash
kb refs --json                    # corpus walk: docs with code-ref hints
# or one corpus:  kb refs --kb <NAME> --json
```

Keep only docs from **memory corpora** (`memory_scope` set — `kb fleet status`
/ `GET /api/kbs` shows which) that have ≥ 1 path-shaped ref. Order by
`recall_count` descending when available (`GET /api/memory/census`): a memory
recalled 14× with a rotten citation is the highest-value finding.

## Step 2 — resolve each memory's refs against a checkout

```bash
kb-code doclens show --kb <kb> --doc <id> --json   # --repo <name> unless pinned
```

Per ref, doclens answers `path_state`/`line_state`/`symbol_state`
(`present|ambiguous|absent|external`) — computed fresh, never cached
(invariant #2: kb-code mints classes per request; you store CLAIMS as
comments, which readers re-validate). Classify:

- **present** everywhere → healthy; nothing to write.
- **absent** path OR absent line-range → **drift finding** (Step 4).
- **ambiguous** → note in the report; no comment.
- **external** / no repo configured / daemon down → "not verifiable"; no comment.

Optional era split (CT-F2): if the doc declares a rev, add `?at=declared` via
the HTTP route to distinguish **"wrong when written"** from **"rotted
since"** — say which in the comment body when you know.

## Step 3 — born-stale check

For each **present** ref, blame the cited range (`--lines` is `START:END`,
1-based inclusive):

```bash
kb-code blame <path> --repo <name> --lines <a>:<b> --json
```

If the newest commit touching those lines is **> 90 days older than the
memory's own `kb-created`**, the citation was already old news when the memory
was written ("the fact entered the corpus already unmoored"). That is a
**report-line**, not a comment — unless it co-occurs with drift, in which case
fold it into the drift comment's body.

## Step 4 — file drift comments (the write step)

Dedup first — skip any (memory, path) that already has an OPEN drift comment
(`list` shows open-only by default; filter by the memory's source-relative
path):

```bash
kb comments list --kb <kb> --path <source-rel> --json
# skip when any open body starts with "[kb-drift] <path>"
```

Then file, one comment per drifted path, dated, stating what was checked
(`--author claude` and `--anchor file` are the defaults):

```bash
kb comments add --kb <kb> --artifact-id <id> \
  --body "[kb-drift] src/storage/actor.rs:1502-1516 — absent in kb@<short-sha> (swept 2026-08-21); was cited for the read-lane classification. Fact unverified, citation dead."
```

The `[kb-drift] <path>` prefix is the machine-greppable half (CT-C4's recall
`code_hints` renders open drift-flags from exactly this grammar); everything
after the `—` is for the human. If a prior drift comment exists but is
RESOLVED and the ref drifted **again**, file a fresh comment (history stays
append-only per thread).

## Step 5 — escalate proven wrongness

Only with evidence in hand (you read the current code and it contradicts the
memory's claim):

```bash
kb memory flag <id> --reason "cited fn now does X, memory claims Y — see [kb-drift] comment"
```

## Step 6 — report

End with a table: memory id · title · refs checked · drifted (commented) ·
ambiguous · born-stale · flagged · skipped-dedup. State the checkout + sha
verified against and which corpora were NOT verifiable (kb-code down, no repo
pin) — absence of findings there is absence of verification, not health.
Zero findings is a valid, reportable outcome.

## Idempotency

Same corpus + same checkout twice → zero new writes (Step 4's dedup gate).
`--dry-run` must show exactly what a wet run would write.
