---
name: kb-weekly
description: Turn one project's ledger (a week — or any N-day window — of sessions/commits/decisions/research) into a short narrative "week in review" note. Use when the user asks "what did I ship on <project> this week", wants a weekly/periodic review, or asks to summarize recent project activity from kb. Reads GET /api/sessions/ledger (facts, daemon-computed, LLM-free); the narrative prose is written by YOU, the agent — never the daemon (#10/#26, kb's no-in-daemon-LLM rule). Saves the result as a kb note (kb notes new) in the sessions corpus the ledger's rows came from.
---

# /kb-weekly — the project ledger's narrative half

`GET /api/sessions/ledger` (moonshots M4) is a deterministic VIEW over
existing primitives — sessions grouped by UTC day, with their commits,
decisions, research topics, and honest active time. It emits **facts only**,
same as every other kb daemon surface (#10). **You** write the prose: a
short, honest "what happened on this project" narrative. Clean split — the
daemon never generates language, the skill never invents facts.

## Arguments

- `[--project <key>]` — a registered `[projects.*]` id, or a raw
  `project_key`. Absent = every project in the window (rare; usually you
  want one project — ask if it isn't obvious from context).
- `[--days N]` — trailing UTC window, default 7 (a week), max 31 (the
  ledger route's own ceiling). No arguments beyond `--project` → the
  default week.

## Step 1 — pull the facts

```bash
kb sessions ledger --project <key> --days <N> --json
```

This is the WHOLE evidence base — do not also read raw transcripts or
individual session shows; the ledger already joined sessions, commits,
decisions, and research for you (#28 fan-out, #11 newest-capture scoping,
all handled server-side).

**Why this skill does NOT use `kb context` (CT-D1), and must not.** The
CT-D1 acceptance criterion is that context ASSEMBLY gets one home — and this
skill has none to give up: it already makes exactly one evidence call, and
that call is on a different axis. `kb context` is **prospective and
topical** — "given this task text, what does kb know that bears on it" —
while the ledger is **retrospective and temporal** — "given this project and
this UTC window, what actually happened, day by day". A window is not a
query: the ledger's rows are selected by `started_at`, not by relevance, and
it must return the boring days too (a blocked day is a reportable outcome
here, whereas a pack would rank it away). Routing the ledger through
`kb context` would mean either inventing a query text the user never gave —
and then narrating from whatever it happened to match — or bolting a
time-window mode onto a relevance verb. Both trade an honest, complete window
for a ranked sample. One home for context assembly, one home for the ledger.

If the response's `days_out` has no day with any
`sessions`/`commits`/`decisions_count`, there is nothing to report — say so
plainly (**zero is a valid, reportable outcome**, same as `/kb-distill`'s
Step 4 gate) and stop; do not write a note that says nothing.

## Step 2 — read the shape

Per day (`days_out[]`, oldest first):

- `sessions[]` — `{sid, kb, display_name, outcome, active_secs}`. `outcome`
  is the session's own CLOSURE (the last real assistant text, ≤240 chars) —
  the single best signal for "what this session actually did." A session
  with no `outcome` produced no readable closure (a husk, or it ended
  mid-tool-call) — say that plainly rather than inventing one.
- `commits[]` — `{kind, sha, subject, resolved}`. `resolved: true` means the
  subject is the TRUE git-resolved subject; `resolved: false` means it's
  transcript-detected only (could be stale/wrong) — hedge accordingly
  ("likely shipped" vs "shipped").
- `decisions_count` — how many AskUserQuestion-style rulings landed that
  day. The ledger does NOT carry the decision text itself (a deliberate
  wire-size trim) — if the narrative needs the actual question/answer, pull
  it with `kb sessions show <sid> --json` for that ONE session, don't
  re-fetch the whole window.
- `research_topics[]` — up to 3 queries, ranked by frequency that day.

`totals` — window-wide sums (`sessions`, `commits`, `decisions`,
`active_secs`). Report totals as-is; never invent a percentage, a trend
line, or a comparison to a prior week the ledger didn't give you (the
ledger's own anti-nag fence: no streaks, no goals, no completion
percentages — carry that discipline into the prose).

## Step 3 — write the narrative (this is the LLM step)

A short, dense, honest paragraph or two per active day (skip empty days
entirely — don't pad). Lead with what SHIPPED (the closures + resolved
commits are the spine); fold in decisions where they explain a shipped
outcome; mention research only when it's substantive (a `research_topics`
entry with real repeated queries, not a one-off lookup). Write in past
tense, plain language, no marketing voice. This is a private working note,
not a highlight reel — a blocked or trivial day is reported as such, not
smoothed over.

Close with the window totals in one line (sessions / commits / decisions /
active time) — the honest-accounting numbers (#10), not a score.

## Step 4 — save as a note

Read `kb` off any `days_out[].sessions[]` entry in the Step 1 JSON — that's
the corpus the ledger's rows actually live in (usually one kb; if the
window somehow spans more than one, pick the one with the most sessions and
say so in the note). Then:

```bash
kb notes new --kb <that-kb> --folder "<project label>" \
  --title "Week in review — <project label> — <start date>–<end date>" \
  --tag weekly-review --tag "<project key>" \
  --body "<the narrative from Step 3>" --daemon <url-if-not-default>
```

`--body` takes the full Markdown narrative inline (or pipe it via `--stdin`
if it's long enough that shell-quoting gets unwieldy — same trick
`/kb-distill` doesn't need but `kb notes new --help` documents). This is a
**note**, not a `kb remember` memory — a weekly digest is a browsable
journal entry, not a durable salience-ranked fact competing with real
decisions/gotchas in the memory corpus (kb's non-goal fence: no
memory-benchmark arms race, no dashboards-for-dashboards — the note format
keeps this a calm read, not a scored artifact).

## Step 5 — report

State: the project + window covered, how many active days out of N had
anything to report, the note's kb + path (from `kb notes new`'s output),
and the totals line. If Step 1 found nothing, report that instead of Step
4/5 — no note gets written for an empty window.
