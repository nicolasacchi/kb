---
description: The memory "dream" — distil past Claude Code sessions into durable curated facts. Reads session digests, dedups against existing memories, proposes ADD/SUPERSEDE/MERGE, and after you approve a dry-run writes graded, provenance-linked facts via `kb remember`. Dry-run first; pass a session id to force re-reflection.
argument-hint: [<session-id>]
---

You are running the kb memory **dream**: consolidating the raw episodic record of
past sessions into a few durable, curated facts worth recalling in future work.
**You** are the distiller — the same model, the same tokens, no hosted model on
the daemon. The `kb` CLI is the only path. Three hard rules:

- **Dry-run first, human-gated.** Propose everything in a table and STOP for
  approval before any write. Never write, supersede, or delete unprompted.
- **Append-only + reversible by default.** `--supersedes` drops a memory at recall
  but keeps the file; only a MERGE uses `kb forget --purge`, which **deletes** —
  and only with explicit approval. (Plain `kb forget`, without `--purge`, only
  soft-forgets — tombstones the memory in place, still on disk and
  census-visible — so a MERGE must pass `--purge` to actually remove the
  superseded ids.)
- **Recall is untouched.** You only WRITE (via `kb remember`); the per-turn ranking
  stays deterministic and LLM-free (#10). Facts land in the MEMORY corpus, never
  the sessions corpus (#11).

> **Division of labour:** `/kb-distill` (kb-memory plugin) is the per-session,
> autonomous-safe sibling — it writes ADD/SUPERSEDE only and never `kb forget`,
> so it can run right after a session (the Stop-hook nudge suggests it). This
> command is the corpus-wide **dream**: the only place a MERGE (hard delete)
> is allowed, which is why it is human-gated.

## Modes

- **`/kb-reflect`** (no argument) — sweep recent sessions that have produced **no**
  curated fact yet (`memory_count == 0`) and reflect over them.
- **`/kb-reflect <session-id>`** — **force** re-reflection of one session, ignoring
  the `memory_count` gate (use when a session already has a fact but more is worth
  keeping). `$ARGUMENTS` carries the id.

> `memory_count` means "has ≥1 curated fact stamped to it", not "fully distilled":
> one earlier `kb remember` shadows a session from the sweep. That is intended —
> the forced mode is the escape hatch.

## Step 1 — gather candidate sessions

```bash
kb sessions list --limit 40 --json
```

Parse the JSON. In **sweep** mode keep sessions where `memory_count == 0`. In
**forced** mode keep only `$ARGUMENTS`. Optionally narrow to the project you care
about by the `folder` (basename) or `cwd` field. Each row carries `session_id`,
`artifact_id`, `kb` (the sessions-corpus name), `first_user_prompt`, `folder`,
`git_branch`, `memory_count`.

## Step 2 — get the CANONICAL session id (do not skip)

The `session_id` from `kb sessions list` can be a legacy capture-hook artifact —
the full UUID with a **trailing `-`** (37 chars), or otherwise not a clean
36-char UUID (`8-4-4-4-12` hex). A bad id (a) breaks the memory↔session link and
`claude -r`, and (b) breaks idempotency, because `memory_count` is matched against
the **stored** id while a memory carries the **clean** id.

Check each candidate's **stored** id against a clean 36-char UUID
(`8-4-4-4-12` hex). A stored id is *not* clean if it has a trailing `-` (full UUID
+ dash) or is shorter (the old `cut -c1-24` truncation). **Do not just strip the
dash and use it** — `kb sessions show` and `memory_count` both key on the *stored*
id, so a stamped-clean / stored-dashed mismatch would break idempotency and the
session→memory link. Instead, if **any** candidate id is not clean, refresh the
stored ids from the transcript ground truth, then re-list:

```bash
kb reindex --kb <the "kb" field from the list JSON, usually "sessions">
# wait ~3-5s for the watcher, then:
kb sessions list --limit 40 --json
```

After the reindex the stored id IS the clean canonical UUID (recovered from the
JSONL `sessionId`), so use the stored id **verbatim** from then on — for
`kb sessions show`, for `--session-id`, and for the `memory_count` gate, all three
now agree. (If a daemon can't be reindexed, you may still `show` using the raw
stored id, but then omit `--session-id` and record provenance via the body
permalink in Step 6 rather than stamp a non-canonical id.)

## Step 3 — read each candidate session

```bash
kb sessions show <session-id> --json
```

Bounded, never the raw transcript. Use: `session.first_user_prompt`,
`decisions` (steering moments), `commits` (what shipped), `files` / `touches`
(what was worked on), `git_branch`, `session.title`. If you need more signal,
`kb get <artifact_id> --kb <sessions-kb> --format md` prints the one-pager
(NOT `kb cat` — it resolves against local lance state only, so it can't see a
shared/docker daemon's artifacts, and it dumps raw source bytes anyway; the
raw transcript via `--format html` is multi-MB, grep it bounded).

## Step 4 — distil candidate facts (your judgement)

From the prompt + decisions + commits + files, extract **0..N durable facts** —
things genuinely worth recalling in a *future* session. Be stingy; most sessions
yield zero or one.

- **Durable:** a user preference / identity / correction; a project decision,
  architecture choice, or convention; an operational fact (a command, a path, a
  gotcha, a deploy step).
- **NOT durable (skip):** anything already in the repo / README / CLAUDE.md / git
  history; transient task or debugging state; a restatement of the code; anything
  that will be stale next week.

Write each as one self-contained sentence (the body) plus a short title (≤ 60
chars). Then **dedup the candidate set against itself** — two sessions often yield
the same fact; recall (next step) cannot see a fact you wrote seconds ago, so merge
duplicates now.

## Step 5 — reconcile against existing memory (the dedup oracle)

For **each** candidate:

```bash
kb recall "<the candidate fact>" --scope all --no-floor --limit 8 --json
```

`--scope all` is load-bearing here, not a leftover: `kb recall`'s bare
default is now `auto` (project-narrowed to the caller's own repo), which
would hide cross-project duplicates from this dedup oracle. Keep it explicit.

`--no-floor` is essential — it surfaces low-salience and decayed neighbours the
normal floor hides (exactly the facts you'd otherwise re-create). Classify:

- **NOOP** — a near-identical fact already exists → skip it.
- **SUPERSEDE** — an existing fact is now outdated by this one → note its `id`.
- **MERGE** — several existing facts (and/or candidates) say one thing → pick the
  merged wording and the `id`s to remove.
- **ADD** — genuinely new.

When uncertain, prefer **NOOP / SUPERSEDE over ADD** — never write a near-duplicate.

## Step 6 — grade salience + choose scope (the rubric)

| Tier | Salience | Scope |
|---|---|---|
| user identity / preference / correction | **0.8–0.9** | `--global` (recallable everywhere; omit `--link`) |
| project decision / architecture / milestone | **0.6–0.7** | project memory: `--link <projectkb>` (visible only to that project) |
| operational fact (command / path / gotcha) | **0.5–0.6** | project memory (or `--global` if it's cross-project) |

Find the project's memory corpus name from the daemon (the kb whose memory scope is
`project`); on most setups it is `memory-<project>`. `--global` is the `kb remember`
default; `--link <kb>` scopes visibility to that kb (and is **not** the corpus
selector — the target corpus is `--scope`/`--kb`).

**Distribution check:** print a histogram of the proposed saliences. If **>60 %**
land in one bucket, RE-GRADE — a flat distribution just recreates the 0.5
monoculture this is meant to fix.

## Step 7 — DRY-RUN table (always, before any write)

Print one row per candidate:

| fact (title) | class | salience | scope | neighbours compared | source session | ids to supersede/**forget** |

…followed by the salience histogram and a clear note of **exactly which ids will be
hard-deleted** by any MERGE. Then **STOP** and ask the operator to approve: all, a
named subset, or none. Do not proceed without an explicit go.

## Step 8 — write the approved facts

- **ADD:**
  ```bash
  kb remember "<fact>" --title "<short label>" --salience <g> \
    --tags <a,b> [--global | --link <projectkb>] [--session-id <clean-uuid>]
  ```
- **SUPERSEDE (1:1):** the same, plus `--supersedes <old_id>` (drops the old at
  recall; the file remains — reversible).
- **MERGE (N:1):** write the merged fact (ADD form), then for **each** source id:
  ```bash
  kb forget <id> --purge [--kb <kb>]
  ```
  ⚠️ `kb forget --purge` **deletes the file — irreversible** (plain `kb
  forget`, without `--purge`, only soft-forgets: the memory is tombstoned but
  stays on disk and searchable). Only run `--purge` on the ids you listed and
  the operator approved.
- **Provenance:** pass `--session-id <clean-uuid>` when you have one (it links the
  fact to its origin session and lifts that session's `memory_count`, so the next
  sweep skips it). `--session-id` takes one id, so when several sessions
  contributed, also name them in the body text (e.g. "(sessions <id1>, <id2>)").

## Step 9 — verify + report

Spot-check one written fact surfaces:

```bash
kb recall "<a fact you just wrote>" --scope all --json
```

(again, `--scope all` explicitly — the bare default now narrows to this
project and could miss a fact that's about to land in a different corpus.)

Then report: facts added, superseded, merged/deleted, and sessions skipped. A
second `/kb-reflect` sweep should now skip the sessions you just reflected (their
`memory_count` rose).

## Guarantees (why this is safe)

- **#10** — recall stays deterministic + LLM-free; you only ever WRITE.
- **#11** — sessions stay pull-only; you READ digests on demand, write into the
  MEMORY corpus, never auto-inject.
- **#26** — no model on the daemon; the distiller is this agent.
- **Reversible by default** — `--supersedes` is a recall-time drop; a plain
  `kb forget` is a reversible soft-forget; only an approved MERGE's
  `kb forget --purge` hard-deletes.
