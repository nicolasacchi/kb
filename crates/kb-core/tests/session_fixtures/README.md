# Session-transcript golden fixtures

**Every fixture in this directory is SYNTHETIC.** Each one is hand-authored (or
generated from a hand-authored script): the record *shapes* — record kinds,
field names, block layouts, wrapper envelopes, `parentUuid` chains — copy real
Claude Code captures, and **none of the content does**. Every prompt, path,
title, commit subject, tool argument and tool output below was invented for the
fixture.

**Rule for this whole directory: never paste real transcript content into the
repo.** kb is public; a captured `~/.claude/projects/*/*.jsonl` is not. Read a
real capture to learn a shape, then re-type the shape with invented text — the
way every fixture here was authored. This rule is also quoted by
`../coderef_fixtures/README.md`.

Two sets live here, split by which engine they pin:

- **`synthetic-*.jsonl`** — the `session-view/1` engine (`sessions/view.rs`),
  consumed by `../session_closure.rs`, `sessions/view.rs`'s own test module and
  `session_render_tests.rs`.
- **the five digest-pipeline fixtures** (`tiny-noop`, `tiny-rate-limited`,
  `subagent-delegation`, `ask-user-question`, `tool-heavy-research`) — the
  digest pipeline (`parse_session_html_full` → `session_digest` /
  `session_digest_excerpt`), pinned by `../session_digest_snapshots.rs`, plus
  the terminal reader in `kb-cli`'s `commands/session_read.rs`.

## Synthetic shape fixtures (sessions-rethink W0)

| fixture | records | shape it models | closure |
|---|---|---|---|
| `synthetic-workflow-heavy.jsonl` | 40 | the workflow/subagent session: `Workflow`/`TaskCreate`/`TaskUpdate`/`TaskOutput` tool_use, `<task-notification>` user records, a sidechain lane, requestId-split assistant events (empty thinking + prose + tool_use under one `requestId`), `[mode]`/`permission-mode`/`ai-title`/`last-prompt`/`queue-operation` metadata, a hook attachment, a `file-history-snapshot`, a `git commit` Bash call | the invented closing summary, followed by a ghost empty-thinking fragment (the requestId-shatter tail the extractor must reach back through) |
| `synthetic-kb-commands.jsonl` | 24 | the kb-command session: `kb recall` / `kb search` / `kb remember` / `kb recollect` Bash calls with their outputs, `hook_additional_context` recall injections | the substantial closing wins over the later terse "Tests green." one-liner; the capture ends on a tool_result |
| `synthetic-husk.jsonl` | 6 | the trivial husk (P10): `[mode]` + caveat + `/clear` + a queued-command attachment, no assistant turn at all | `None` |

## The digest-pipeline set

Each of these five is parsed through the FULL digest pipeline the daemon runs —
`parse_session_html_full` (the kb-capture.sh envelope) → `session_digest` /
`session_digest_excerpt` — and the result is pinned with an insta snapshot in
`../snapshots/session_digest_snapshots__digest_*.snap`. The same five are
re-rendered through the `session-view/1` engine by `kb-cli`'s
`session_read.rs` tests (header, turn window, `--full`, `--turn`, `--grep`).

| fixture | records | what it exercises |
|---|---|---|
| `tiny-noop.jsonl` | 7 | the degenerate session: a bare `/clear`, no assistant turn, no `first_user_prompt`, no model, a `hook_cancelled` attachment, an empty `file-history-snapshot`. The digest degrades to `project: … branch: …` and an EMPTY excerpt |
| `tiny-rate-limited.jsonl` | 18 | a tiny session whose only assistant record is harness-synthesised: `isApiErrorMessage: true` + `model:"<synthetic>"` — it sets `model` but must NEVER become the closure (no `closed:` line). Eight attachment kinds incl. `skill_listing` and two `hook_additional_context` recall injections; no `ai-title` record |
| `subagent-delegation.jsonl` | 53 | ONE completed (synchronous) delegation via `tool_use name:"Agent"` — **not** `"Task"` (see the GC-B8 defects below) — whose `toolUseResult` carries `totalTokens`/`totalToolUseCount`/`toolStats.editFileCount`; a heredoc `git commit -m "$(cat <<'EOF' …)" && git push` whose SUBJECT must come from the heredoc body; one `is_error` tool_result; a populated `file-history-snapshot` (the authoritative edited set); a second `cwd` in the modal tally |
| `ask-user-question.jsonl` | 38 | THREE genuine `AskUserQuestion` steering decisions (one `tool_use`; the paired `toolUseResult` carries `questions[]` + `answers{}`, and the ORDER comes from `questions[]`), plus THREE completed subagent delegations summed together |
| `tool-heavy-research.jsonl` | 75 | the tool-heavy research turn: TWO **async** (`status:"async_launched"`) Agent launches that must land in `subagent_launched_unstatted` and never contribute a zero-valued `subagent_count`; a `WebFetch`; two `kb search` Bash calls; a `Skill` invocation; `ToolSearch`/`TaskCreate`/`TaskUpdate`/`Write`/`Edit`/`Read`; four `is_error` results; a first prompt longer than `PROMPT_PREVIEW_MAX_CHARS` (so the 200-char truncation is exercised); two human turns |

### Properties the consumers actually assert

The snapshots pin everything, but three test files assert *values* directly —
change a fixture and these break first:

- `sessions.rs::activity_subagent_stats_from_transcript_fixtures` — `subagent-delegation`
  = 1 completed delegation, 98,275 tokens, 40 tool calls, 0 files edited, 0
  unstatted · `tool-heavy-research` = 0 completed, 0 tokens, 2 unstatted ·
  `ask-user-question` = 3 completed, 42,968 + 52,473 + 65,620 tokens, 28 + 27 +
  35 tool calls, 0 unstatted.
- `session_digest_snapshots.rs` — every fixture is valid JSONL (one JSON value
  per non-empty line) and the pipeline is deterministic across a repeat parse.
- `kb-cli`'s `session_read.rs` — every fixture renders non-empty and
  deterministically; `ask-user-question` has at least one human turn with prose
  (the outline's first row backs the `--grep` test); `tool-heavy-research` has
  at least five turns (the tail-window test needs something to elide);
  `tiny-noop` renders no "not shown" note and answers a non-matching `--grep`
  with `no turn matches --grep`.

### Why they are synthetic (2026-09)

This set used to be five REAL Claude Code transcripts, truncated to a
representative line prefix and run through `session_scrub::scrub_transcript`
(`secrets` + `paths` layers). A path/secret scrub is not an entropy pass: the
operator's own prompts, project names, skill listings, injected memories and
hostnames all survived it. They were replaced wholesale with the synthetic set
described above — the same rule the `synthetic-*.jsonl` set has always followed.
Record counts, session ids and digest text therefore all changed, and the five
`session_digest_snapshots__digest_*.snap` snapshots were re-recorded against the
new fixtures. What did NOT change is the set of *properties* pinned above: each
replacement was authored to keep the shape, and the asserted subagent numbers
are byte-for-byte the ones the previous set carried.

Regenerate or extend by editing the JSONL directly (one JSON value per line;
`every_fixture_is_valid_jsonl` / `synthetic_fixtures_are_valid_jsonl` pin
validity), then re-record the snapshots with `INSTA_UPDATE=always cargo test -p
kb-core --test session_digest_snapshots`.

## Defects this fixture set surfaced — FIXED by GC-B8 (commit `cd2782de`)

These were found by reading real captures during the GC-C1 pass; GC-B8 fixed all
three and re-recorded the affected snapshots, so the snapshots pin the
**correct** behavior. The synthetic set keeps the shapes that expose them:

- **Entropy-layer JSON corruption** — `scrub_transcript` with `entropy: true`
  corrupted lines into invalid JSON. Root cause: the entropy regex's char class
  included `/`, so when a high-entropy run began immediately after a
  JSON-escaped `\/`, the replacement swallowed the `/` but left the preceding
  `\` dangling, producing an invalid `\[` escape. **Fixed**: `/` is excluded
  from the run char class (a `/` inside a real secret just splits one match into
  two, each still redacted once it clears the length floor).
- **The "subagent iceberg" (`session-memory-deep-review-2026-07.html`)** — real
  transcripts from this Claude Code build name the subagent-launching tool call
  `"Agent"`, never `"Task"`, so `sessions::classify_research`'s `"Task" => …`
  match arm never fired against real data and every subagent delegation was
  invisible to the digest (`research` came back `[]`/undercounted). **Fixed**:
  the match is version-tolerant (`"Task" | "Agent"`). `subagent-delegation.jsonl`
  and `tool-heavy-research.jsonl` both still say `"Agent"` — do not "modernise"
  them to `"Task"`, that is the regression they pin.
- **Commit-subject corruption (review's B5)** — a heredoc commit form
  (`git commit -m "$(cat <<'EOF' … )"`) had its `subject` recorded as the literal
  string `"$(cat <<'EOF'"` rather than the message. **Fixed**:
  `extract_commit_message` threads the full multi-line command through and parses
  the heredoc body's first non-empty line as the subject (covering `<<EOF` /
  `<<'EOF'` / `<<"EOF"`; the plain `-m "subject"` fast path unchanged).
  `subagent-delegation.jsonl` keeps the heredoc form for exactly this reason.
