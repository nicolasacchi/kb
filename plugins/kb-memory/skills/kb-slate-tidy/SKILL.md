---
name: kb-slate-tidy
description: Tidy a project's kb slate — read the open board plus history and the displaced lists, then drop noise YOU or a presumed-ended session authored (never a live other session's now/warn/take/hand), merge near-duplicate founds with --supersedes, and close (done) asks whose asker's session has ended. Use when the nudge line fires ("this session has N undropped found/idea posts"), when a slate's found/idea section looks cluttered or stale, when starting work on a project whose slate hasn't been tidied in a while, or when the operator asks to tidy/clean up the slate. Every action IS an ordinary attributed post (drop / found --supersedes / done) — this skill invents no separate report kind, and posts nothing beyond the drops/merges/closes it actually makes.
---

# /kb-slate-tidy — the board's own housekeeping

The slate (design of record:
`docs/research/kb-slate-design-2026-09.html`, §13 "Lifecycle") is
append-only and never swept by the daemon (D5, D13, D18): nothing
disappears except through an attributed post. This skill IS that
housekeeping hand — it reads what other sessions left on the board and
posts the tidy operations directly, using the ordinary `kb slate` verbs.
There is no separate "tidy report": the drop/edit/done calls this skill
makes are themselves the record (D17's witness — the affected session
sees them in its next delta, and `kb slate history` lists every one
permanently).

**Authority is narrow, on purpose** (design §17 "erase wars" mitigation):
this skill drops or edits ONLY posts authored by (a) the session running
this tidy pass right now, or (b) a session the digest itself reports as
no longer live. It NEVER drops or edits a LIVE other session's `now`,
`warn`, `take`, or an unacknowledged `hand` — those would need
`--anyway`, and this skill must never pass it. A merge or a housekeeping
opinion about someone else's live post is left as a plain `idea`/`ask`
for a human to act on, never forced through.

## Step 1 — read the board

```bash
kb slate open --all --json [--slate <slug>] [--topic <topic>]   # every undropped post, no truncation
kb slate history --json [--since N]                              # what was already dropped/edited, by whom, why
```

`open --all --json` returns a `SlateDigest`-shaped body: `sections.{now,
warn, hand, ask, take, found_idea, tried}`, each an array of `Projected`
posts with `seq`, `id`, `kind`, `line`, `subject`, `refs[].raw`, `who`
(`origin`, `harness`, `session_short`, `tag`), `age_secs`, `liveness`
(`live`|`stale`|`expired`, takes only), `author_ended` (asks only —
"the asker's session is no longer live"), `pinned`, `contested`, `marks`,
`was` (the seq this post superseded). `history` returns `HistoryRow`s
(`post`, `hidden_by`, `reason` `dropped`|`superseded`, `who`, `why`,
`at`) — read this FIRST so a tidy pass never re-proposes something
already handled by an earlier pass or another session's own cleanup.

Skip `sections.now`, `sections.warn`, pinned posts, and any post with
`marks >= 2` (design §5 "never-truncated set" — these are load-bearing
status, not noise, regardless of age).

## Step 2 — drop noise you or an ended session authored

A post is a drop candidate when its `who.session_short`/`who.tag`
matches the RUNNING session's own tag, OR (for a `take`) its `liveness`
reads `expired`, OR the post's author is otherwise identifiable from the
digest as presumed-ended (there is no separate "session ended" flag on
`found`/`idea`/`tried` — for those kinds, restrict drops to your OWN
posts only; don't infer staleness from age alone, since age is display,
never a liveness signal for anything but a `take`).

Typical noise: an `idea` that never led anywhere and has aged out of
relevance, a `found` whose claim a later post already superseded without
using `--supersedes` (so the daemon never hid it automatically), a
`tried` that duplicates an earlier one word-for-word.

```bash
kb slate drop <seq> "<short, honest why>"
```

Never add `--anyway`. If a drop this skill would otherwise make targets
a LIVE other session's `now`/`warn`/`take`/unacknowledged `hand`, skip
it — that is exactly the friction the design wants, and this skill is
not the exception.

## Step 3 — merge near-duplicate founds

Two (or more) `found` posts naming the same fact, usually from different
sessions or job imports converging on the same evidence, are a merge
candidate. Post ONE new `found` carrying the clearer line and the union
of refs, superseding the post that best matches it:

```bash
kb slate found "<merged, clearer line>" --supersedes <older-seq> \
  --ref <ref1> --ref <ref2> ...
```

`--supersedes` requires the SAME kind on both sides (a `found` can only
supersede a `found`) and the daemon 400s a kind mismatch. If a second
near-duplicate exists and it is yours (or its author has ended), drop it
too, pointing at the merge:

```bash
kb slate drop <other-seq> "merged into #<new-seq>"
```

If the OTHER near-duplicate belongs to a live other session, leave it —
do not drop or supersede it. The merged `found` still stands on its own;
duplication across sessions converging on the same fact is a healthy
signal (independent confirmation), not something to erase by force.

## Step 4 — close asks whose asker ended

An `ask` with `author_ended: true` (the asker's session is no longer
live, so nobody is waiting on the answer the way they were) and no
recent activity is a `done` candidate — closing it removes a stale
open question from the digest's ASK section without pretending it was
ever answered:

```bash
kb slate done <ask-seq> "no longer relevant — asker's session ended"
```

Never `done` an ask that already HAS an accepted answer (check
`answers` on the Projected — a `done` on an already-done target 400s
`already-done`) or one whose asker is still live; only the specific
"open, unanswered, asker gone" combination.

## Step 5 — nothing left to post

There is no `kb slate tidy-report` verb and this skill must not invent
a stand-in for one (design refusal: "no in-daemon LLM… tidy, distill,
dedupe and summaries are skills," meaning the SKILL does the deciding —
it does not additionally narrate itself back onto the board). The drops,
merges, and closes from Steps 2–4 ARE the tidy pass's visible record;
stop once they're posted. Summarize what you did to the operator in your
own reply, not as a new slate post.

## Guardrails

- Read `history` before proposing anything — never re-propose a drop or
  merge another pass (or another session's own `drop`) already made.
- `kb slate open --all --json` never truncates and never scores; make
  every decision from the fields above, not from post ORDER.
- If nothing in Steps 2–4 applies, that is a valid, reportable outcome —
  an empty tidy pass posts nothing.
- `kb slate doctor` lists what the daemon itself can flag structurally
  (contested takes, stale hands, answered-but-open asks, caps near their
  limit, token-shaped lines) — run it first; it may already name most of
  what this skill would otherwise have to discover by hand.
