---
name: kb-slate-distill
description: Distill a closed or rotated kb slate into durable memory — reads every post across the slate's current AND archived generations, writes 1-5 kb memories (decisions plus the tried dead ends with their reasons), one short narrative note, and dated plan-file Status lines. Use at milestone/tag time when a project's slate has been closed or rotated, when the operator asks to distill/archive a slate before it ages out of relevance, or when a plan file's Status still points at a slate that no longer has anything live left on it. The daemon never authors memory text (#10/#26, no in-daemon LLM) — this skill is where the slate's working state becomes curated fact.
---

# /kb-slate-distill \<slate-or-topic\> — the board becomes memory

A slate (design of record: `docs/research/kb-slate-design-2026-09.html`,
§13 "Lifecycle") is deliberately NOT memory — it is in-play working
state, capped and finite, meant to age out. This skill is the one
sanctioned crossing from the slate into memory: it reads a slate that is
done being worked (closed, or rotated into an archived generation) and
writes what is actually worth keeping, via the SAME `kb remember` path
`/kb-distill` uses, with the SAME write discipline.

**Write vocabulary — identical to `/kb-distill`'s contract:**

- **ADD · SUPERSEDE · NOOP.** Never `kb forget` a pre-existing memory;
  a many-into-one merge is reported and deferred to `/kb-reflect`.
- **Quality over quantity.** 1–5 memories per distill run; fewer is a
  valid, reportable outcome. No vague memory ("the team discussed
  X") — every memory names the decision or the dead end AND why.
- **Dedup and idempotent.** `kb recall --no-floor "<candidate text>"`
  before every write; `--supersedes` on a genuine overlap; every write
  carries `--link <slug>` (design §13 "Promotion") so the memory keeps
  a pointer back to the slate it came from.
- Slate reads are pull-only and never mutate the slate itself — this
  skill's ONLY slate-side write is the informational `promote`-style
  `done #n "promoted → mem <id>"` on each post it distilled from (see
  Step 4), matching what `kb slate promote` already does by hand.

## Step 1 — is this slate actually done?

```bash
kb slate open --all --json [--slate <slug>] [--topic <topic>]
```

A CLOSED slate's digest still reads normally (closing only refuses NEW
posts); a slate with an ACTIVE `now` and recent activity is not ready —
report that and stop rather than distilling a project still in flight.
`kb slate stats` is a fast sanity check (hands/takes/asks counts) before
committing to a full read.

## Step 2 — gather every post, current generation AND archives

**Current generation** (open or closed): the CLI/API surface is enough —
`kb slate open --all --json` for the undropped set, `kb slate history
--json` for everything dropped or superseded (a `tried` or a decision
`now` that got tidied away is still distillable; only a `done` closes
something without hiding it, so `history` alone is not the full
picture — cross-reference both).

**Archived generations** (after `kb slate rotate` or several rotations):
there is no route or CLI verb for a past generation — `history`/`--all`
are explicitly current-generation-only (the daemon's own doc comment:
*"the archives are for distill"*). Read the raw ledger files directly —
this is the ONE sanctioned exception to "never hand-read daemon storage"
(unlike `.review/<id>.json`, kb-slate ledgers are read-only from this
skill's side; only `kb slate` mutates them):

```
<state>/slates/<slug>/ledger.jsonl        # current generation
<state>/slates/<slug>/ledger.<gen>.jsonl  # archived, gen = 1, 2, 3, …
```

`<state>` is `$KB_STATE_DIR` if set, else `$KB_HOME/state` if `KB_HOME`
is set, else the platform default `~/.local/state/kb` (honouring
`$XDG_STATE_HOME`) — see `docs/configuration.md` "Path overrides". Each
line is one `kb-slate/1` `Post` JSON object (`seq`, `id`, `at`, `kind`,
`line`, `body`, `topic`, `subject`, `refs`, `re`, `supersedes`, `pin`,
`prov{…}`); a line whose top-level `schema` field disagrees with
`kb-slate/1` belongs to a different/future ledger format — skip that
generation and note it rather than guessing its shape. `meta.json`
beside the ledger names `generation` and `rotated_from`; walk backward
from `rotated_from` to cover every generation the topic actually lived
across, not just the newest archive.

Filter every generation's posts to the `--topic` you're distilling when
one was given (a daemon-wide slate can hold several milestones' topics
side by side — see design §4 "Topics"); an untopic'd project slate has
no filter to apply.

## Step 3 — extract what's worth keeping

From the full post set (current + archived, drops and supersedes
resolved so you're reading the LATEST version of each chain):

- **Decisions.** The `now` chain per topic (each `now` supersedes the
  last) is the topic's own status timeline — the LAST `now` before
  close/rotation is usually the headline outcome; earlier ones in the
  chain that mark a real pivot ("switched from X to Y because…") are
  decisions too, not noise. A `found` that stood unchallenged (never
  dropped, never contradicted by a later post) is a confirmed fact
  worth the same treatment as a decision.
- **Tried (dead ends with reasons).** Every `tried` post, verbatim
  reason (`--failed` text) intact — design calls this "the single most
  valuable category no memory system captures on its own." Don't
  compress away the WHY; a dead end without its reason is just noise
  with extra steps.
- **Not memory-worthy on their own:** `take`/`hand`/`mark`/`drop`/
  `answer`/standing `warn`s about host mechanics (build throttles,
  lock files) — these are working-state plumbing, not durable facts.
  An `ask`+accepted-`answer` pair IS worth a memory when the answer
  itself is a durable fact nobody should have to re-derive.

## Step 4 — write the memories, the note, and close the loop

```bash
kb recall --no-floor "<candidate memory text>"     # dedup check, every candidate
kb remember "<decision or dead-end, with why>" --link <slug> --session-id <sid>
kb slate done <source-post-seq> "promoted → mem <id>"    # per post you distilled from
```

1–5 `kb remember` calls (Step 1's quality gate — zero is valid if
nothing survives the filter). Each write's `--link <slug>` keeps the
provenance pointer the design promises ("the daemon never authors the
memory text" — you do, from evidence you can cite). After each memory
lands, `kb slate done <n> "promoted → mem <id>"` on the source post IF
it's still on the CURRENT generation (an archived-generation post has
no live `#n` to close against — skip the `done` call for those, the
memory's own provenance is the only record needed).

Then one narrative note:

```bash
kb notes new --title "<slate/topic> — closing narrative" --category note
```

A short prose summary (what the slate coordinated, what shipped, what
didn't and why) — this is the human-readable story the bare memory list
can't tell; keep it to a few paragraphs, cite the memory ids and
significant post `--ref`s rather than re-quoting them at length.

## Step 5 — the dated plan-file Status line

If a plan file's Status section still reads the SL5 pointer
(`Status: see \`kb slate open --topic <slug>\``), replace it with a
dated closing summary now that the slate no longer has anything live to
point at — one or two lines naming what shipped, citing the memory
id(s) and the narrative note, e.g.:

```
## Status
- 2026-09-12: **DISTILLED.** Slate topic `<slug>` closed/rotated;
  decisions + dead ends captured in mem <id1>, <id2>; narrative note
  <note-id>. See those for the full record — this plan file is closed.
```

Also append one dated entry to `docs/spike-findings.md` for the
milestone this slate coordinated, in that file's existing style (a
short "what shipped, what we learned" paragraph) — the plan-file
Status line is the pointer for THIS project's future readers; the
spike-findings entry is the durable, cross-project lesson.

## Guardrails

- Never run this against a slate still in active use — Step 1 is not
  optional.
- Never `kb forget` an existing memory here; a merge candidate is
  reported and deferred to `/kb-reflect`.
- An archived-generation ledger is READ-ONLY from this skill — never
  write to `ledger.<gen>.jsonl` directly; every mutation still goes
  through `kb slate` (the `done` calls in Step 4 target the CURRENT
  generation only, as above).
- If `<state>/slates/<slug>/` doesn't exist at all, the slate was never
  created (or was `purge`d) — report that and stop; there is nothing to
  distill.
