# kb memory hooks

Deterministic, **LLM-free** hooks that wire the kb daemon in as an agent
harness's memory. Every hook just runs a `kb` verb (or, for the `Stop`
hooks, plain file operations). Most target Claude Code directly; the rest
are capture/nudge adapters for other coding-agent harnesses (Codex,
opencode, Grok Build, Kimi Code) and a full extension-based wiring for omp
(Oh My Pi).

| Hook | Event | What it does |
|---|---|---|
| `kb-recall.sh` | `UserPromptSubmit` | Runs `kb recall <prompt> --cwd <payload cwd>` and injects the top memories as turn context. Silent on no hits / daemon down. **Project-scoped recall**: passing `--cwd` (never `--scope all`) lets the CLI resolve its own default "auto" scope — global corpora plus the caller repo's own `memory-<slug>` corpus, derived from the payload's `cwd` rather than this hook's process cwd (some harnesses run hooks with an unrelated `$PWD`). Outside a repo, or against a daemon too old to know "auto", the CLI degrades to the old fleet-wide behavior on its own. **Also works unchanged as a codex `UserPromptSubmit` hook** (same `.prompt` stdin field, same `hookSpecificOutput.additionalContext` output). For **Kimi Code**, run it with `KB_HOOK_FMT=kimi` — plain-stdout output instead of the JSON envelope, and it reads Kimi's array-shaped `.prompt` (`[{"type":"text","text":…}]`) as well as Claude/codex's string form. **CT-A3**: alongside each hit's human-readable line, it appends a machine-readable `<!--kb-recall/1 kb=<kb-name> id=<hex12>[ pos=<n>]-->` marker (the `pos=` rank pair is MR1; see [`KB_RECALL_LAYOUT`](#kb_recall_layout--the-shape-of-the-injected-block-mr1)), folded into that hit's block — the `memory_recalls` ledger's capture-side parse (`kb_core::sessions::view::derive_memory_recalls`) prefers this marker and falls back to the free-text line only when it's absent (older captures, a hand-edited transcript). **CT-D1**: on the **first** `UserPromptSubmit` of a session id it ALSO makes one `kb context "<prompt>" --cwd … --session …` call and APPENDS its COUNTS line ("3 prior sessions · 2 open comments · 5 memories") plus "run `kb context`" beneath the ordinary recall block. The scent is **additive, never a replacement** (ruling 2026-08-22): recall has pushed memory titles on every turn since v0.9 and invariant #11's R0/R3 governs EPISODIC material — so transcripts stay counts-and-pointers-only ("pulled on demand, never auto-injected") while turn 1 keeps the memory titles it always had. Either half may be empty: an empty recall window with prior sessions/comments emits the scent alone, and both empty exits silently as before. First-turn proxy = an ATOMIC `set -o noclobber` create of `~/.cache/kb/context-scent-<sid>`, so it fires exactly once per session id. EVERY miss degrades to the plain recall block — no session id, an unwritable cache dir, a resumed session, a daemon too old to serve `/api/context`, or an empty corpus (`"no prior context"` is never injected). Turns 2..n never enter the branch and are byte-identical to pre-CT-D1. **MR1**: the SHAPE of the block is selected by `KB_RECALL_LAYOUT` (`v1` \| `v2`, the default \| `v2-last`) — see the section below. Tests: `tests/test-recall-scent.sh`, `tests/test-recall-layout.sh`, plus `tests/test-recall-{flagged,warns,drift,kimi}.sh`. |
| `kb-wake-kimi.sh` | `UserPromptSubmit` (Kimi Code) | The Kimi wake path. Kimi's `SessionStart` stdout never reaches the model (probe-verified: only `UserPromptSubmit` stdout is appended to context, as a `<hook_result>` user message), so the wake content rides the first prompt instead: once per session (marker `~/.cache/kb/waked-kimi-<sid>`) it emits the memory protocol (shared `memory-protocol.txt`), the recent-memories index (`kb recall '' --cwd <payload cwd>`, same project-scoped `--cwd`/no-`--scope all` as `kb-recall.sh`), and the distill-pending ledger surface-and-consume (same relay as `kb-wake.sh`, harness-labeled). **v0.45 N5**: appends, last, the first line of `kb chores --line` (same block as `kb-wake.sh`: at most once per UTC day via the CLI's own stamp, `KB_SKEW_SHOWN=0` because this script has no skew notice, 3s bound, nothing on any failure). Session marker files are left to `kb-recall.sh`, registered on the same event. Tests: `tests/test-wake-kimi.sh`, `tests/test-wake-kimi-chores.sh`. |
| `kb-wake.sh` | `SessionStart` | Re-injects the memory protocol + a compact index of recent memories (`kb recall '' --cwd <payload cwd>`, project-scoped like `kb-recall.sh` — no more `--scope all`). **MI-W0.3**: also surfaces (and consumes) the `~/.cache/kb/distill-pending` ledger — up to the 3 newest grok sessions queued by `kb-capture-grok.sh` that committed without a successful `kb remember`, dropping entries older than 14 days. See "Grok distill-pending relay" below. **v0.44 F10**: appends the first line of `kb chores --line` (at most once per UTC day, kept by the CLI; nothing when nothing is due, the daemon is down or the CLI predates the verb; 3s bound). Test: `tests/test-wake-chores.sh`. |
| `kb-capture.sh` | `Stop` | Captures the conversation transcript into the `[kb.sessions]` corpus (set `KB_SESSIONS_DIR`) via `kb sessions capture` (scrubbed). A failed capture is spooled privately and replayed, never embedded raw - see "A failed capture is spooled" below. |
| `kb-capture-codex.sh` | `Stop` (codex) | Capture adapter for OpenAI Codex CLI: translates the rollout JSONL named by `transcript_path` into a Claude-shaped session capture (commits, file edits from `patch_apply_end`, web searches, usage) so the digest, `kb why`, and `kb recollect` work across harnesses. Also runs as a CLI backfill: `kb-capture-codex.sh <rollout.jsonl>…`. Lossy by design (outputs capped, reasoning dropped); the raw rollout path rides an `adapter-meta` line. Same `KB_SESSIONS_DIR` gate + atomic overwrite-per-session contract as `kb-capture.sh`. Design + e2e evidence: [docs/research/kb-memory-for-foreign-harnesses-2026-07.html](../../../docs/research/kb-memory-for-foreign-harnesses-2026-07.html). |
| `kb-capture-opencode.sh` | `session.idle` (opencode plugin) | Capture adapter for opencode: takes a session id (or an `opencode export` JSON file), translates message/part rows into Claude-shaped JSONL, writes the same capture envelope. Wired by `~/.config/opencode/plugin/kb-memory.ts` (recall via `chat.message` + `experimental.chat.system.transform`, capture on `session.idle`, `shell.env` sets `NO_PROXY` so agent-initiated `kb` calls reach loopback under opencode's VPN proxy alias). CLI backfill: `kb-capture-opencode.sh <sessionID\|export.json>…`. |
| `kb-capture-grok.sh` | post-run (grokclaude) | Capture adapter for Grok Build sessions run via `grokclaude`: resolves a job's `meta.json.grok_session_id` to `~/.grok/sessions/<url-encoded-cwd>/<uuid>/`, translates `chat_history.jsonl` into Claude-shaped JSONL (reasoning folded as `thinking` blocks, tool calls name-mapped, timestamps joined from `events.jsonl` `loop_started`), and writes via the SAME preferred path as `kb-capture.sh` (`kb sessions capture`, bash-hand-rolled-HTML fallback). Skips fake runs (`GROKCLAUDE_FAKE`, or a `grok_session_id` absent/`fake-*`) and thin transcripts (<1 user or <1 assistant line). `--with-report` additionally renders `report.md` + `findings/*.json` as a linked `kb-category: reference` artifact (`grok-report-<job-ulid>.html`, cross-linked via `kb-session`). Modes: `<job-dir>` (the grokclaude post-run trigger's own call shape), `--session-dir <dir>` (direct), `--backfill [--root <blackboard-root>]`; `--dry-run` prints without writing. **MI-W0.3**: on a successful capture (either write path) also queues a distill-pending ledger line — see "Grok distill-pending relay" below. Fixture + golden mapping test: `crates/kb-cli/tests/adapter_grok.rs`. |
| `kb-capture-throttle.sh` | `PostToolUse`, `PreCompact` | LF-3a/D2 — a min-interval gate (`KB_CAPTURE_MIN_INTERVAL_SECS`, default 240s) in front of `kb-capture.sh`, so a long session gets a fresh capture every few minutes instead of only at Stop (makes the staleness badge + presence chip honest at minutes-granularity). **Default OFF** — opt in with `KB_CAPTURE_LIVE=1`; the hook entries are always registered, the script itself is the gate. `PreCompact` captures the pre-compaction tail, the one moment that content is otherwise lost. Test: `tests/test-capture-throttle.sh`. |
| `kb-distill-nudge.sh` | `Stop` | When the session ran `git commit` but `kb remember` never SUCCEEDED, shows a one-line suggestion to run `/kb-distill <session-id>` (the episodic→semantic distiller, a skill in this plugin). Two transcript greps — the parent transcript AND any Task-tool subagent sidecars under `<session-id>/subagents/*.jsonl` (**MI-W0.3**) — no daemon round-trip; once per session (marker in `~/.cache/kb/`); same `KB_SESSIONS_DIR` gate as capture. **MI-W0.3**: suppression is success-aware — it greps for `kb remember`'s own `remembered <12-hex-id>` stdout line, not just the command being run, so a FAILED remember (daemon down, 400, …) no longer silently suppresses the nudge. A `--json` remember success prints no such line, so a spurious nudge is possible — an accepted bias (losing a fact silently was worse). Test: `tests/test-distill-nudge.sh`. |
| `kb-distill-nudge-codex.sh` | `Stop` (codex) | **MI-W0.3** — the codex-side twin of `kb-distill-nudge.sh`: same git-commit-without-a-successful-remember check, but greps the CODEX rollout named by `.transcript_path` instead of a Claude transcript (tool-call lines matched on `"name":"(exec_command\|shell\|local_shell)"`, then the literal substring `git commit`; the `remembered <12-hex-id>` success marker is plain text either way, so it's the same regex). Session id read from the rollout's own `session_meta.payload.id`. Once per session (its own `distill-nudged-codex-*` marker namespace under `~/.cache/kb/`). Registered in `~/.codex/hooks.json`'s `Stop` chain after the capture entry (see "Codex CLI registration" below). Test: `tests/test-distill-nudge-codex.sh`. |
| `kb-capture-kimi.sh` | `Stop` / `SessionEnd` / `PreCompact` (Kimi Code) | Capture adapter for Kimi Code: Kimi's hook payload has no `transcript_path`, so the script derives the wire itself — `${KIMI_CODE_HOME:-~/.kimi-code}/sessions/wd_<basename(cwd)>_<sha256(cwd)[0:12]>/<session_id>/agents/main/wire.jsonl`. Translates the wire (`turn.prompt`, `content.part` text/think, `tool.call`/`tool.result` — Kimi tool names are already Claude-shaped; Write/Edit `.args.path` → `file_path`, usage `inputOther+inputCacheRead+inputCacheCreation`/`output` → `input_tokens`/`output_tokens`) into the same Claude-shaped JSONL and writes via the SAME preferred path (`kb sessions capture`, bash fallback with `kb-harness: kimi`). CLI backfill: `kb-capture-kimi.sh <wire.jsonl>…`. Test: `tests/test-capture-kimi.sh`. |
| `kb-distill-nudge-kimi.sh` | `Stop` (Kimi Code) | The Kimi-side twin of the distill nudge: greps the wire's `"name":"Bash"` `tool.call` lines for `git commit`, suppresses on a successful `remembered <12-hex-id>`, once per session (`distill-nudged-kimi-*` marker). On a hit it prints the one-line nudge (Stop stdout may be shown to the user) AND appends `kimi <sid> <epoch>` to the shared `~/.cache/kb/distill-pending` ledger, so the next interactive session's wake (any harness) surfaces it. Test: `tests/test-distill-nudge-kimi.sh`. |
| `kb-capture-omp.sh` | `session_stop` / `session.compacting` / `session_shutdown` (omp, via `kb-omp.ts`) | Capture adapter for Oh My Pi (omp): translates an omp v3 session JSONL (`~/.omp/agent/sessions/<encoded-cwd>/<ts>_<sid>.jsonl`) into the same Claude-shaped JSONL. omp sessions are an append-only TREE (`id`/`parentId`), so a single backward pass from the last entry captures the LEAF CHAIN only — abandoned branch experiments never pollute the activity — and everything at-or-before a trailing `reset_boundary` (`/clear`) is cut. Tool names are canonicalized (`bash`→`Bash`, …), Write/Edit `.arguments.path` → `file_path`, per-assistant `usage{input,output,cacheRead,cacheWrite}` → summed `input_tokens`/`output_tokens`; own injected `custom_message` entries are dropped so kb's context never echoes back into the corpus. Lenient pre-clean drops torn trailing lines (crash or active append) instead of aborting. Same preferred path + fallback + overwrite contract as the other adapters. **Lifecycle (v0.45 OC)**: one active conversion per session file across processes, a hard deadline, and every child reaped on cancel - see "The omp capture owns its lifecycle" below. **Segmented capture (v0.46 SEG-PR2, opt-in)**: a session whose leaf chain outgrows 16 MiB is landed as an ordered chain of ordinary sessions instead of one O(session) conversion - see "Segmented omp capture" below. CLI backfill: `kb-capture-omp.sh <session.jsonl>…`. Tests: `tests/test-capture-omp.sh`, `tests/test-capture-omp-lifecycle.sh`, `tests/test-capture-omp-segments.sh`, `tests/test-capture-omp-segments-scale.sh`, `tests/test-omp-capture-lifecycle.sh` (the `kb-omp.ts` spawn path, bun). |
| `kb-beat.sh` | `SessionStart`/`UserPromptSubmit`/`Stop`/`SessionEnd`/`Notification` (Claude Code); registered analogously for codex, Kimi Code, opencode, omp | **LSC-3** push collection for the live-sessions cockpit (design: [docs/research/kb-live-sessions-cockpit-2026-08.html](../../../docs/research/kb-live-sessions-cockpit-2026-08.html) §4/§5). One script for all harnesses: `kb-beat.sh <harness> <event>` reads the hook payload on stdin, maps it to the canonical `kb-live/1` beat (`v`, `session_id`, `harness`, `event` — one of `start\|prompt\|tool\|turn_end\|blocked\|unblocked\|end`, never a derived state — `at`, `host`, `pid`, `cwd`, `model`, `lease_secs`, optional `title`/`last_line`/`detail`), and POSTs it fire-and-forget (detached background subshell, `curl --max-time 2 --connect-timeout 1`) to `${KB_DAEMON_URL:-http://127.0.0.1:4000}${KB_BEAT_PATH:-/api/sessions/beat}`. Every failure path — no `curl`/`jq`, no daemon, the route not existing yet, malformed stdin — is silent and `exit 0`; this ships safely ahead of the server-side route. Kill switch `KB_BEAT=0`; opt out of `last_line` content with `KB_BEAT_CONTENT=0`; `KB_BEAT_DRYRUN=1` prints the JSON body to stdout instead of posting (used by `tests/test-beat.sh` and for manual sanity checks). Claude's Stop event carries `.last_assistant_message` verbatim (capped 240 chars) as `last_line`; codex resolves the canonical `session_id` from the rollout's own `session_meta.payload.id` (same ground-truth preference as every other adapter here), falling back to a bare `.session_id` if present. `KB_SESSIONS_DIR` gates it exactly like every capture hook (kb not configured for this project ⇒ no-op). Test: `tests/test-beat.sh`. |
| `kb-beat-throttle.sh` | `PostToolUse` (Claude Code) | A min-interval gate (`KB_BEAT_HEARTBEAT_MIN_INTERVAL_SECS`, default 180s) in front of `kb-beat.sh <harness> tool`, mirroring `kb-capture-throttle.sh`'s pattern but keyed on a per-session marker file's mtime (no artifact to stat here). Exists so a single long tool call (a 20-minute build) still refreshes the session's `lease_secs` mid-turn instead of only at the next `Stop` — design §11's named mitigation for "per-tool beats would be hundreds per session." Default **ON** (unlike the capture throttle — a beat is a tiny POST, not a multi-MB re-serialize); `KB_BEAT_HEARTBEAT=0` disables just the heartbeat, `KB_BEAT=0` disables every beat. |

Hooks never block: any internal failure exits 0 with nothing injected.

## kb slate

SL3 wires `kb slate` — the per-project SHARED WORKING STATE (who is on
what, open questions, hypotheses, dead ends; **not** memory) — into the
SAME two lanes every harness already shares for memory
(`kb-wake.sh`/`kb-wake-kimi.sh`, `kb-recall.sh`), so nothing new needs
registering anywhere `kb-memory` is already installed. Design:
[docs/research/kb-slate-design-2026-09.html](../../../docs/research/kb-slate-design-2026-09.html)
§12 (harness reach) + §16 (SL3).

- **The HYBRID block** (`kb-wake.sh` / `kb-wake-kimi.sh`, session start /
  Kimi's first prompt): `timeout 4 kb slate open --hybrid --budget 2000
  --session-id <sid> --cwd <payload cwd> --json`, appended (blank-line
  separated) after the memory index and the distill-pending block. Takes
  `.text` for the injected block and `.head_seq`, written to
  `~/.cache/kb/slate-cursor-<sid>` — this SEEDS the cursor the delta lane
  reads on every later prompt.
- **The DELTA block** (`kb-recall.sh`, every prompt, after the recall
  block and the CT-D1 turn-1 scent): cursor-gated — an ABSENT cursor file
  means the wake lane hasn't opened this session's slate yet (or never ran
  for this harness), so the delta stays silent rather than seeding its own
  cursor (the design's Cursor rule: "no read ever writes" except
  `open`/`delta` advancing it, and `open` owns the seed). When present:
  `timeout 2 kb slate delta --since <cursor> --session-id <sid> --cwd
  <payload cwd> --budget 1500 --json`; a non-empty `.text` is appended
  (blank-line separated) and the cursor advances to `.head_seq`. The
  `KB_HOOK_FMT=kimi` bare-stdout branch carries the same block — it rides
  the shared `block` variable, no separate wiring.
- **Session id ladder** (both blocks): the hook payload's own
  `session_id` → `KB_SESSION_ID` (env override) → the `current-session`
  marker `kb-wake.sh`/`kb-recall.sh` already write on every turn — so a
  resumed or foreign-harness session still resolves the SAME cursor file
  the wake lane seeded. `kb-wake-kimi.sh` skips the ladder (its
  once-per-session gate already requires a payload session id).
- **Markers**, both under `~/.cache/kb`, read/written only by these hooks
  and the CLI:
  - `slate-cursor-<sid>` — this session's slate read position (a bare
    `head_seq` integer), written by the HYBRID block and advanced by the
    DELTA block.
  - `slate-topic-<sid>` — written CLI-side by `kb slate open --topic <T>`
    ("declares the session topic", §9's CLI table) when a human or agent
    explicitly scopes a session to a topic, so a later `kb slate
    open`/`delta` call with no explicit `--topic` reuses it. Neither hook
    here writes this file — it exists so a manual `kb slate open --topic
    v7` mid-session keeps the hybrid/delta calls on that topic without
    re-passing the flag every time.
- **Fail-open, always.** `timeout 4` / `timeout 2` cap the added wall
  time; a missing `kb`, a non-zero exit, a malformed JSON body, no git
  repo at `--cwd`, or no daemon all degrade SILENTLY to the pre-SL3
  output — byte-identical, no error text, no partial block. Slug
  derivation is entirely server-side (`--cwd`) per the design's "shell
  slug drift" warning — these hooks never re-implement `kb_slugify` for
  the slate, so a linked worktree still resolves to the same project
  slate as its main checkout.
- **`KB_SESSION_ID`** — an optional env override (second rung of the
  ladder above), for invoking a hook outside its normal harness payload
  (e.g. a manual `KB_SESSION_ID=abc123 kb-wake.sh </dev/null` sanity
  check) where no payload session id exists to read.
- **Identity export (v0.44)** — the other direction: the hooks now
  *publish* the identity. `kb-wake.sh`, `kb-wake-kimi.sh` and `kb-recall.sh`
  export `KB_SESSION_ID` (+ `KB_HARNESS` when knowable) for the `kb` they
  spawn; on Claude's SessionStart they also append the same two lines to
  `$CLAUDE_ENV_FILE`, the one channel that reaches the agent's later Bash
  tool calls, so a shell `kb remember`/`kb slate …` is attributed to the
  session instead of the last-writer-wins marker. The omp extensions set
  `process.env` the same way (omp spawns its bash tool from it). Other
  harnesses have no such channel: their shell writes still fall back to
  the repo marker (flagged on stderr). `kb-wake.sh` names the harness only
  when knowable (`KB_HARNESS`, else `claude` iff `CLAUDE_PLUGIN_ROOT` is
  set), so codex/opencode shelling it are never relabelled `claude`.
  Deliberately NOT exporting: the `kb-capture-*.sh` adapters and
  `kb-beat.sh` call no identity-dependent verb (capture is `kb sessions
  capture`/scrub, beat is `curl` only), and `kb-slate-harvest.sh` posts
  with explicit `--harness`/session args per call.
- **How the session id reaches a shell `kb` write, per harness (v0.44 X6)** —
  the CLI resolves it by the ladder in `crates/kb-cli/src/session_identity.rs`
  (`--session-id`, `KB_SESSION_ID`, `CLAUDE_CODE_SESSION_ID`, `GROK_SESSION_ID`,
  the fresh repo marker `current-session-repo-<slug>`, the global
  `current-session` file - flagged on stderr - then none). What each harness
  puts on those rungs:

  | harness | channel that reaches the agent's shell | otherwise |
  | --- | --- | --- |
  | Claude Code | `$CLAUDE_ENV_FILE` (appended by `kb-wake.sh` on SessionStart) and Claude's own `CLAUDE_CODE_SESSION_ID` | repo marker |
  | omp | `kb-omp.ts` sets `process.env.KB_SESSION_ID`/`KB_HARNESS` on `session_start` (omp spawns its bash tool from it); covered by `tests/test-omp-slate.sh`, which CI runs under bun | repo marker |
  | Grok | `GROK_SESSION_ID` (ladder rung 4), when the harness exports it | repo marker |
  | Codex | none - no env-file or process.env channel | repo marker written by `kb-recall.sh` on every prompt |
  | Kimi Code | none | repo marker written by `kb-recall.sh`/`kb-wake-kimi.sh` |
  | opencode | none documented (the plugin sets `shell.env` for proxies only) | repo marker |

  A harness on the "repo marker" row is attributed to the most recent session
  that prompted in the same repo within the freshness window (40 minutes), so
  two concurrent sessions of ONE repo on such a harness can still be
  mis-attributed; the CLI prints the caveat. A script outside any harness can
  pass `--session-id` or export `KB_SESSION_ID`.
- **A failed capture is spooled, never embedded raw (v0.44 X6)** -
  `kb-capture.sh` has no bash envelope writer any more. When `kb sessions
  capture` fails (or `kb` is missing) the raw transcript is copied to a private
  spool outside every corpus - `$KB_CAPTURE_SPOOL`, else
  `$KB_CACHE_DIR/capture-spool`, else `${XDG_CACHE_HOME:-~/.cache}/kb/capture-spool`
  (dir 0700, files 0600, one item per session, never indexed). The next
  successful `kb-capture.sh` run, or `kb sessions capture --replay-spool`,
  pushes each item through the normal scrubbed capture path and deletes it;
  `kb doctor --hooks` (check `outbox`) counts what is waiting. Test:
  `tests/test-capture-spool.sh`.
- **The codex/opencode/kimi/omp/grok adapters spool too (v0.45 N4)** - none of
  them writes session HTML any more. Each translates its harness transcript to
  Claude-shaped JSONL (the first record is an `adapter-meta` line carrying
  `harness`, which is rung 1 of the enrich harness ladder, so the harness
  survives a Rust-written envelope - the old `kb-harness` meta tag was only
  rung 2) and hands it to `hook_adapter_land` in `kb-hook-lib.sh`: `kb sessions
  capture --stamp <session-start>` (secrets scrub, commit resolution, atomic
  write), then on success it drops that session's older spool item and replays
  any other pending items. On failure, or with no `kb`, the UNSCRUBBED
  translation is parked in the same private spool as above (the `.meta` records
  `session_id`, `stamp`, `harness`) and lands scrubbed on the next success or
  `--replay-spool`, the filename still carrying the session's true start time.
  A `kb` too old to know `--stamp` is retried once without it. omp's translated
  subagent sidecars ride the spool too (`<spool>/<session-id>/subagents/`, where
  the capture engine already looks; removed after a successful replay or when a
  live capture lands first). Test: `tests/test-capture-adapters-spool.sh`.
- **A replay never overwrites a fresher capture (omp capture round 5)** - the
  spool is shared by every session and every adapter, and `hook_adapter_land`
  replays ALL pending items while holding only the current session's lock, so
  a replay could read X's old M1 snapshot, lose a race with X landing M2, and
  then publish M1 over it (X's fingerprint already matched, so X stayed stale
  until it next changed). The fix is in the Rust writer, so it covers every
  adapter: `kb sessions capture --replay-spool` compares each item's snapshot
  time (its file mtime, i.e. when it was parked) with the mtime of the
  session's existing capture and DROPS the item (stderr line, `dropped_stale`
  in the summary, item and parked sidecars removed - not retried forever) when
  the capture was published after the snapshot. The check, the write and the
  rename run under a short per-session publish lock
  (`<capture-locks>/publish-<sid>.plock`, same directory as the adapter locks),
  and the existing capture is re-resolved under it, so a live capture landing
  between the check and the rename cannot be overwritten and no second
  filename is minted. Direct captures are never refused: they always carry the
  newest view of the transcript. Limits: the freshness key is file mtime (a
  clock set backwards or a filesystem with coarse mtime can misjudge items
  within the timestamp granularity), and the lock is advisory (non-unix builds
  run the check without it). Anything that bumps the existing capture's mtime
  without a new snapshot (a restore, `touch`, a tool rewriting it) makes a
  legitimately newer spooled snapshot look stale and be dropped. A replay also
  deletes an item only if its mtime is unchanged since it was read, so a newer
  snapshot re-parked meanwhile survives for the next replay. The rule lives only
  in the Rust writer: **deploy the kb binary before (or with) the plugin** - a
  plugin updated ahead of the binary has no replay protection (the replay-order
  test reproduces the overwrite against kb 0.45). Bash publishers that do not
  take the publish lock (the Claude `kb-capture.sh` does `mv -f`) are outside
  the rule; they do not use the shared omp spool. Tests: `replay_spool_never_overwrites_a_fresher_capture`
  (Rust) and `tests/test-capture-replay-order.sh` (real kb).
- **The omp capture owns its lifecycle (v0.45 OC)** - a growing omp session is
  captured from three triggers (turn end, compaction, shutdown) and from
  several omp processes at once, and a 400 MB session once left two orphaned
  `jq` conversions burning CPU for hours. `kb-capture-omp.sh` now:
  - **excludes** per session: an `flock` keyed on the hash of the session
    file's realpath (`$KB_CAPTURE_LOCKS`, else `<cache>/kb/capture-locks`)
    wraps the whole convert + land sequence (the Rust writer publishes through
    one fixed `<out>.tmp` name, so two concurrent captures of one session would
    interleave). A request that finds the lock held only bumps a request counter
    and returns; the owner re-converts the live file and runs another pass until
    no request arrived during the last one, so a newer request is coalesced,
    never dropped, and a stale conversion of THIS session can never overwrite
    a fresher one. (The lock is per session; the spool replay is shared - see
    "A replay never overwrites a fresher capture" below.)
    Different sessions never contend. CLI backfill (`kb-capture-omp.sh
    <file>...`) waits for the lock (`KB_CAPTURE_LOCK_WAIT_SECS`, default 60) and
    always converts.
  - **skips only unchanged input**: a pass is skipped when the fingerprint
    (parent inode:size:mtime-ns, a hash of the first 512 bytes = the in-place
    title slot, the sidecar-directory listing, the adapter version) equals the
    one recorded after the last SUCCESSFUL landing and the capture file still
    exists. It is recorded only when `hook_adapter_land` returned 0 - spooled
    (1) or lost (2) landings, timeouts and signals record nothing - and is read
    BEFORE the conversion, so an append during it forces another pass. A
    growing session is therefore never "already captured".
  - **owns its descendants**: in hook mode it re-execs itself under `setsid`
    (same PID, so a caller that signals the PID it spawned still reaches it),
    runs every long child (the pre-clean and translate `jq`, the edited-set
    pipe, the sidecar translations, the `kb` landing) in the background under
    `timeout` and awaits it with `wait` (a foreground child would defer the
    trap until it exited). `TERM`/`INT`/`HUP`/exit reap every process of ITS OWN
    SESSION (not its process group: `timeout(1)` moves its child into a group
    of its own), remove the scratch files and release the lock. A kill before
    landing leaves the previous published capture untouched. Nothing outside the
    script's own session is signalled.
  - **survives its own SIGKILL**: no trap runs on an untrappable kill, so a
    small watchdog in a session of its own reaps whatever still carries the
    run's `KB_CAPTURE_RUN` marker (or the owner's session) and removes the run's
    scratch directory. The watchdog INHERITS the per-session lock (the
    conversion children do not), so the lock outlives a killed owner until the
    orphans are dead: no new owner starts, and no orphaned `kb` can publish
    after a fresher conversion. A request arriving in that window sees the
    recorded owner (`<lock>.owner`, pid + start time) is gone and waits for the
    lock (`KB_CAPTURE_ORPHAN_WAIT_SECS`, default 8) instead of being dropped.
    Timing: the watchdog polls every 0.5 s, TERMs the owner's session at once,
    finds the other marker carriers with ONE `grep` over `/proc/*/environ`
    (milliseconds on a host with ~900 processes; it was two forks per process,
    about 2.5 s there), then polls (one `ps` per 0.1 s) up to 2 s before KILL.
    Measured on an idle host with a `kb` that ignores TERM (the worst case): the
    lock is free about 2.4 s after the owner SIGKILL; a cooperative orphan is
    gone sooner. This is NOT a hard bound: it grows with host load and process
    count, so `KB_CAPTURE_ORPHAN_WAIT_SECS` (default 8) is headroom (about 3x
    the measured worst case), not a guarantee. A request that waits the full
    time while the watchdog is still reaping is dropped, and the capture catches
    up at the next trigger (turn end). Limits, stated plainly: if the watchdog
    is SIGKILLed together with the owner (cgroup or OOM kill of both) the lock is
    released at once and the stale-publish race this closes can recur; the reap
    is by the run's `KB_CAPTURE_RUN` environment marker, so any process a
    capture spawns inherits it and is reaped too; the pid lists the watchdog
    and `kb-omp.ts`'s group KILL act on are only unsafe after a pid wrap (Linux
    never reuses a pid that is still a live process-group id); and the `INT`
    trap is inert if the caller started the script with SIGINT ignored
    (`kb-omp.ts` only sends TERM). The watchdog is per lock hold and is stopped
    before the lock is released. `KB_CAPTURE_NO_WATCHDOG=1` disables only the
    watchdog.
  - **a malformed record never loses the session**: the translator tolerates a
    non-string tool name, non-object `arguments`, a non-string `path` and
    non-object content entries (the record degrades; the rest of the transcript
    is kept; well-formed input is byte-identical). Should `jq` still exit with a
    data error, the records it already emitted (`jq -s` streams) are landed, the
    edited-set snapshot falls back to empty / is skipped, and only a deadline
    kill or TERM (rc 124/137/143) aborts the pass, so a session is never retried
    forever on a record it cannot parse.
  - **drops a poisoned subagent sidecar** instead of failing the pass: a
    sidecar whose translation errors is skipped (the main transcript and the
    healthy sidecars still land); only a deadline kill or a TERM aborts.
    **TERM bound (segmented tail conversion included):** every step whose
    cost grows with the input - the planner, the `session_exit` scan, the
    chain walk, the `kb ... --help` probes, `drop-part` - is a tracked
    background job whose answer is left in a file, never a `$(...)` (bash
    defers a trapped TERM until a command substitution returns), so a TERM is
    handled within ~6 s at those stages (pinned for a slow exit scan and a
    slow chain walk in `test-capture-omp-segments.sh`). A few constant-size
    `jq` reads (one plan-file field, one sidecar timestamp line) still run in
    `$(...)`; only an IO stall on those tiny reads could delay a TERM.
  - **bounds a pass** with `KB_CAPTURE_HARD_SECS` (default 120, the same value
    `kb-omp.ts` always passed as its timeout - this makes that timeout real):
    each child is capped at what is left of it. The `kb` landing gets its own
    fresh `KB_CAPTURE_BUDGET_SECS` after the conversion instead of what the
    conversion left over. A pass cannot finish a session whose TRANSLATION
    exceeds the 48 MiB capture cap (`kb sessions capture` refuses it and so
    does the spool); that outcome is reported on stderr and never recorded as
    captured. The cap itself is unchanged.
  - **translates in linear time**: the leaf-chain walk used to carry the growing
    chain array in a `reduce` state (O(chain^2): 38 s at 20k records, 3 min at
    40k); it is now a `foreach` that collects the chain, byte-identical output
    (pinned against the former program on every fixture, five edge cases and a
    linked synthetic chain).
  `kb-omp.ts` spawns the script OWNED (`detached`: its own session and process
  group, never omp's) and, on its timeout (`KB_CAPTURE_TIMEOUT_MS`, default
  120 s), sends SIGTERM to that pid first (so the trap runs) and SIGKILLs only
  the group it created after 3 s. An aborted `session_stop` signal (omp's 30 s
  handler budget) is NOT a cancellation (operator ruling): the handler just
  stops waiting, and the capture keeps running detached, still exclusive per
  session, still killed by the TS timeout while omp lives (if the host has
  exited that timer never fires and only the script's own deadlines and the
  watchdog bound it). The script's own bound is NOT `KB_CAPTURE_HARD_SECS`
  alone: that covers one conversion pass, and the `kb` landing that follows
  gets a fresh `KB_HOOK_BUDGET_SECS` (default 25: the landing call up to 20 s
  plus a spool-replay call up to 10 s). Measured with HARD=5 the last
  descendant was gone at about t+27 s, so the real bound is about
  `KB_CAPTURE_HARD_SECS` + 25 s per pass; coalesced extra passes (other
  processes bumping the same session) each get a fresh window. `run()`'s
  behaviour for every other hook and tool is unchanged. A shutdown-triggered
  capture is detached on purpose: omp gives `session_shutdown` 2 s and then
  exits, so the capture outlives it, bounded by that same per-pass figure.
  Tests: `tests/test-capture-omp-lifecycle.sh` (real subprocesses; run by
  `hook_shell.rs`), `tests/test-omp-capture-lifecycle.sh` (bun lane).
- **Segmented omp capture (v0.46 SEG-PR2, off by default)** - the single
  conversion above is `jq -s` over the WHOLE session file: CPU and RSS grow with
  the session, and a ~500 MB session never finished inside the 120 s hard
  deadline, so it was never captured at all. With segmentation on, a session
  whose planned leaf chain has more than one part is captured as an ORDERED
  CHAIN of ordinary capture sessions, each converted from only its own byte
  range, so a turn costs O(live tail), not O(session):
  - **Enable it** (either; checked at EVERY capture, so a RUNNING omp needs no
    restart - the next turn-end capture picks it up):
    `KB_CAPTURE_SEGMENTS=1` in the environment of the capture process, **or**
    the flag file `${XDG_CONFIG_HOME:-$HOME/.config}/kb/capture-segments`
    (`mkdir -p ~/.config/kb && touch ~/.config/kb/capture-segments`; its
    content is ignored). Remove the file to turn it off again;
    `KB_CAPTURE_SEGMENTS=0` forces it off even when the file exists. The env
    var only reaches omp processes that were started with it - the flag file is
    the path for sessions that are already running. It needs a `kb` that has
    `kb sessions segment-plan` (v0.46); the script probes it once per run and,
    when it is absent, prints ONE stderr warning and uses the unchanged
    single-capture path. Deleting superseded parts also needs `kb sessions
    drop-part` (v0.47): an older kb keeps them and says so, and because an
    outstanding drop keeps the input from being recorded as done, every
    trigger of that session re-runs the (cheap, already-landed) segmented pass
    and repeats the warning until the kb is upgraded.
  - **Ids**: part 1 keeps the bare session id (existing artifact ids never
    change); part k>=2 is `<id>-p<NN>` (`-p02`, `-p03`, ...). Every record of
    part k>=2 carries `sessionId: <id>-p<NN>`, and its first `adapter-meta`
    record adds `segmentOf`, `segmentIdx` and `rawSessionId` (part 1 carries no
    keys and is never rewritten to add them). Parts are DISJOINT slices of the
    leaf chain: event totals add up, `kb sessions recover --chain <id>`
    concatenates them.
  - **Below the threshold nothing changes**: a session whose plan has one part
    (the common case; the target is 16 MiB of raw chain bytes, `KB_CAPTURE_SEGMENT_BYTES`
    overrides it for tests) takes the single-capture path byte for byte (pinned
    against a golden made by the pre-segmentation script).
  - **One pass**, under the same per-session lock, coalescing, watchdog and
    hard deadline as above (the deadline bounds each PASS; a multi-pass catch-up is bounded only by the caller's timeout): the planner (`kb sessions segment-plan`, state next
    to the lock state in the private lock dir) cuts the chain at legal user
    boundaries; the LIVE TAIL lands first (the session is searchable at once);
    then up to 4 FROZEN parts, oldest first (`KB_CAPTURE_SEG_FREEZE_PER_PASS`),
    each from `--emit K` (only that part's lines), translated by the SAME
    TRANSLATE program - derived from it by three verified text substitutions
    (`$dmodel` = the planner's per-part value, `$sid` = the part id, `$segmeta`
    on the first meta; if TRANSLATE is ever edited so one no longer applies the
    script warns and falls back, it never drifts). More work than one pass may
    do re-runs passes (the coalescing loop) until caught up; nothing is
    recorded as done until every part is landed. A part is never started with 15 s or less of the pass deadline left (the pass
    then lands nothing and the next one continues; stderr says so). A catch-up that outlasts the
    caller's own timeout (`KB_CAPTURE_TIMEOUT_MS`, 120 s in `kb-omp.ts`) is cut
    by it like any capture and RESUMES at the next trigger: progress is
    persisted per landed part, never restarted.
  - **Landed means rc 0**: the script's own table (`<lock-base>.seg`: part,
    id, input key) is committed tmp+rename after each part that landed with rc
    0, so `kill -9` mid catch-up loses at most the part in flight. A WRITE-AHEAD
    table (`<lock-base>.seg.pend`) records every continuation part just before
    its landing is attempted and drops the row once the landed row exists: a
    part that was parked in the spool and published by ANOTHER session's replay,
    or that landed right before a `kill -9`, has no landed row but is still
    found by a later shrink/fork. Orphan drops are driven by these two tables
    only: if both are lost, or a `drop-part` fails, the part stays in the corpus
    until the next trigger (the session fails open with stale parts, never loses
    data). While any drop is outstanding the input is NOT recorded as done
    (`<lock-base>.done`), in the segmented pass and in the legacy `/clear` path
    alike, so the next UNCHANGED trigger passes the fingerprint shortcut, retries
    the drop and only then records. A part that
    converted but landed with rc 1 (spooled) or 2 (lost) keeps its converted
    files (`<lock-base>.parts/NN/`, private dir) and the retry lands them
    without converting again. A frozen part's key is (first id, last id, entry
    count, its own sidecars), so a title-only change re-lands only the tail, an
    append converts only the tail (plus the one part that just froze), and a
    late subagent re-opens only the part it belongs to.
  - **Sidecars** go to the last part that starts strictly before the sidecar's
    first timestamp (ties -> the earlier part); no readable timestamp -> the
    tail.
  - **Part 1 is re-landed smaller at its first freeze** (correctness over
    artifact stability): a session that was captured whole before segmentation
    keeps its bare-id artifact, which is then overwritten in place with just
    the first slice, and the rest arrives as `-pNN`.
  - **Rewind, fork, `/clear`, edits of earlier lines**: a frozen part is only
    valid while the chain behind it is. A branch/rewind behind a frozen
    boundary re-lands the changed parts in place (same ids) and drops the parts
    the chain no longer reaches through `kb sessions drop-part` (verifies the
    file really is that part, then removes it so the daemon's own delete
    cascade clears its rows - this script never removes a corpus file; the verb itself does unlink the verified file, since kb has no delete route for sessions); a
    `/clear` that leaves the session in one part re-lands the bare id in place
    and drops every `-pNN`. omp is NOT strictly append-only apart from the
    title slot: its session manager rewrites the whole file for
    `rewriteEntries()` (pruning old tool outputs, clearing large tool results
    and images during compaction, `discardEntryDurably` after a turn
    recovery), for a load-time migration or malformed-record sanitize, and when
    the session is moved to another cwd (`session-manager.ts`,
    `session-maintenance.ts`, `turn-recovery.ts` in `@oh-my-pi/pi-coding-agent`).
    The planner's checkpoint detects it (size, window hash, index checksum),
    rebuilds by streaming, and keys parts by entry ids: a prune that only blanks
    results keeps the same ids, so the already-landed parts are kept as they
    were captured (fuller than omp's pruned file) but byte offsets and
    therefore cut points shift, so the next plan can differ and re-land parts
    from the first changed cut on (bounded: 4 frozen parts per pass). A
    part that was only PARKED in the spool (never landed, so no table row) is
    purged too when the chain shrinks - its spool item and `.parts/NN` cache go
    at the start of the pass, so another session's spool replay can never
    publish it as a ghost.
  - **`[session-exit]`**: legacy emits ONE marker, for the last
    `session_exit` of the RESOLVED chain (the leaf chain after the last
    `/clear`). Only the live tail emits it: its own last exit, else the last
    exit of the earlier parts that is on the planner's chain (`--print-chain`;
    candidates are cached in `<lock-base>.exits` and re-checked against the
    chain every pass), so an exit on an abandoned branch, or one at/before a
    `/clear`, is never carried - exactly legacy. A frozen part never emits it,
    so an exit followed by a resume cannot leave a stale marker behind. If the
    planner cannot answer the chain query the tail is not converted (fail
    closed, retried next pass). A part whose own exit entry is later superseded is not
    re-landed, because it never carried one.
  - **A failed plan**: when `segment-plan` fails for a session larger than the
    target nothing is captured (fail closed - a corrupt `<lock-base>.plan` is
    safe to delete); only a session that fits one target falls back to the
    single-capture path. A landing that returns 0 but leaves no capture file is
    treated as a failure (not recorded, no re-landing loop); a pass that ran
    out of time with nothing landed does not re-run.
  - **Size safety**: a translated part over 40 MiB (`KB_CAPTURE_SEG_MAX_PART_BYTES`)
    is never landed oversized or truncated; the raw target is halved (the
    planner has ONE target, so it is per session, kept in `<lock-base>.target`)
    and the session is re-planned. The 48 MiB cap and `--allow-oversized` are
    untouched.
  - **Spool**: each unlanded part parks under its own key (`hook_sid_key` of the
    part id). The spool has a 48 MiB cap per item and no total cap; this script
    caps ONE session at 8 parked parts (`KB_CAPTURE_SEG_SPOOL_MAX`) - at the
    cap a part that is not already parked is not landed (stderr says so) and
    nothing advances, so the next pass regenerates from the source.
  - **Limits, stated plainly**: an older daemon shows the parts as unrelated
    sessions; a comment cannot span a cut; memory stamps and commit trailers
    carry the raw id and resolve to part 1 (read-time widening in the daemon);
    turning the flag off for a session that is already segmented makes the next
    capture re-land the WHOLE session under the bare id over part 1 and leave the
    `-pNN` parts in place - and since those parts still carry
    `segmentOf`/`segmentIdx`, chain-grouped readers (`recover --chain`, logical
    event sums) DOUBLE-COUNT their content until segmentation is re-enabled (the
    parts are then re-landed disjoint again; the same applies when `segment-plan`
    is unavailable after a segmented run); turning it back on re-lands part 1 small at the next
    capture that has something to do (the legacy path forgets part 1's table
    row; an unchanged session waits for its next change);
    the per-part default model is exact at part boundaries only (a
    `model_change` inside a part does not change records of that part that
    carry no model of their own - real omp assistant messages carry theirs).
    This makes the output differ from legacy BY DESIGN for records without a
    model of their own (legacy applies the chain-global LAST `model_change` to
    them, compaction and exit records included; a part uses the model in force
    at its start, which is the more accurate of the two); a subagent that keeps writing re-converts and re-lands
    its part on every pass.
  Tests: `tests/test-capture-omp-segments.sh` (run by `hook_shell.rs`, against
  the real planner and `drop-part`; with an older kb it uses
  `tests/fixtures/fake-segment-plan.py`), `tests/test-capture-omp-segments-scale.sh`.
- **One per-session key (v0.45 N4)** - every per-session file name (capture
  file, spool item, throttle lookup, markers) derives from `hook_sid_key`, and
  `kb sessions capture` (`sanitize_sid`) uses the identical algorithm: a plain
  id (ASCII alphanumerics and `-`, at most 80 bytes - every UUID) is its own
  key; any other id is `{prefix<=48}-{first 16 hex of sha256(raw id)}`, so
  `a_b` and `a-b` (or two ids sharing an 80-char prefix) can never share a file.
  The two sides are pinned by one golden table (`tests/test-sid-key.sh` and
  `sanitize_sid_matches_hook_spool_key_golden_table`). Captures written under
  the old lossy name are never renamed: the throttle and the capture engine
  still recognise one, but only when the id embedded in the file equals the raw
  id. The once-per-session markers (distill nudge, kimi wake, beat heartbeat)
  use the same key; the nudge/wake gates also honour a marker left under the old
  lossy name (`hook_marker_seen`), so an upgrade never double-fires. The omp
  sidecar file name (`agent-<name>.jsonl`) stays readable and gains a short hash
  ONLY when two different agent ids map to the same name. A hook run without
  the shared lib fails open rather than carrying a copy of a key algorithm.
- **Every `kb` call in the capture and harvest hooks is bounded** - the
  `kb-capture*.sh` adapters and `kb-slate-harvest.sh` source `kb-hook-lib.sh`
  and run each `kb` call under `run_to` inside `KB_CAPTURE_BUDGET_SECS`
  (default 25, below the 30s hooks.json timeout; `KB_HARVEST_BUDGET_SECS`,
  default 20, for the harvest). A hung daemon costs the capture (it is spooled
  or skipped fail-closed), never the session end.
- **Capture needs `kb` on the hook PATH** — `kb-capture-codex.sh` and
  `kb-capture-opencode.sh` fail closed without `kb sessions scrub`: with no
  `kb` the session is *skipped* (one stderr line, exit 0), not captured
  unscrubbed. They probe `$KB_BIN_DIR`, `~/.local/bin`, `~/.cargo/bin`,
  `/usr/local/bin` and `/opt/homebrew/bin` before giving up, so a minimal
  hook PATH is usually fine; if your harness strips even `$HOME`, export
  `KB_BIN_DIR`. The kimi/omp/grok adapters prefer `kb sessions capture` and,
  when that fails, scrub their bash-fallback output the same way (and write
  nothing if the scrubber is missing).

Per-harness recipes — the slate rides the same two lanes as memory, so
nothing new needs registering where `kb-memory` is already wired:

- **Codex** — `~/.codex/hooks.json`'s existing `kb-recall.sh` entry (see
  "Codex CLI registration" below) already carries the delta block on
  every prompt; the hybrid block rides the same first-prompt
  `kb-recall.sh`/`kb-wake.sh` call codex already makes. The two
  `kb-beat.sh codex …` entries are unrelated (liveness, not the slate) and
  need no change.
- **Kimi Code** — re-run `install-kimi-hooks.sh`: it already wires
  `kb-wake-kimi.sh` (hybrid, first prompt) and `KB_HOOK_FMT=kimi
  kb-recall.sh` (delta, every prompt) — no new `[[hooks]]` entry needed.
- **OpenCode** — the operator's `~/.config/opencode/plugin/kb-memory.ts`
  already shells `kb-wake.sh`/`kb-recall.sh` from its
  `experimental.chat.system.transform` handler the same way its beat
  addition shells `kb-beat.sh` (see "opencode registration" below); both
  scripts carry the slate blocks automatically, so the plugin needs no
  widening for this — only confirm `shell.env`/`KB_SESSIONS_DIR` still
  reach the child process, exactly as capture already requires.

## Codex CLI registration

`~/.codex/hooks.json` (codex ≥0.124; hooks need one-time trust via `/hooks`
in the codex TUI before they fire):

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [
          { "type": "command",
            "command": "/path/to/kb/plugins/kb-memory/hooks/kb-recall.sh",
            "timeout": 30 },
          { "type": "command",
            "command": "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-beat.sh codex prompt",
            "timeout": 5 } ] } ],
    "Stop": [
      { "hooks": [
          { "type": "command",
            "command": "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-capture-codex.sh",
            "timeout": 120 },
          { "type": "command",
            "command": "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-distill-nudge-codex.sh",
            "timeout": 10 },
          { "type": "command",
            "command": "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-beat.sh codex turn_end",
            "timeout": 5 } ] } ]
  }
}
```

`kb-distill-nudge-codex.sh` (**MI-W0.3**) is the codex-side twin of
`kb-distill-nudge.sh` — same JSON `systemMessage` shape, wired into the
SAME `Stop` group **after** the capture entry so it reads the rollout the
capture entry already wrote for this Stop.

The two `kb-beat.sh codex …` entries (**LSC-3**) are the push side of the
live-sessions cockpit — see `kb-beat.sh`'s own table row above. They're
additive: `KB_SESSIONS_DIR` needs to be set for them to do anything, and
every failure (no daemon, no route yet, a `session_id` codex's own hook
payload doesn't carry) degrades to a silent no-op. Codex has no verified
`SessionEnd`/`Notification`-equivalent hook yet, so `start`/`blocked`/`end`
stay unwired for codex in this phase — `turn_end` (`Stop`) is still the
strongest signal it offers, matching the "Finished" ambiguity §4 already
calls out for this harness.

Agent-initiated `kb` calls (search/why/recollect/remember) additionally need
loopback network inside codex's sandbox — `[sandbox_workspace_write]
network_access = true` in `~/.codex/config.toml`. The hooks themselves run
outside the sandbox and need nothing.

## Kimi Code registration

Kimi Code hooks are `[[hooks]]` entries in
`${KIMI_CODE_HOME:-~/.kimi-code}/config.toml` (fields: `event`, `matcher`,
`command`, `timeout`; stdin payload carries `hook_event_name`, `session_id`,
`cwd`, `client_type`; exit 0 / fail-open, same contract as Claude's).
Install (idempotent, marker-delimited block, backs up the config,
`--uninstall` to remove):

```sh
plugins/kb-memory/hooks/install-kimi-hooks.sh [--sessions-dir DIR]
```

which writes the equivalent of:

```toml
[[hooks]]
event = "UserPromptSubmit"
command = "/path/to/kb/plugins/kb-memory/hooks/kb-wake-kimi.sh"
timeout = 15

[[hooks]]
event = "UserPromptSubmit"
command = "env KB_HOOK_FMT=kimi /path/to/kb/plugins/kb-memory/hooks/kb-recall.sh"
timeout = 15

[[hooks]]
event = "UserPromptSubmit"
command = "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-beat.sh kimi prompt"
timeout = 5

[[hooks]]
event = "Stop"
command = "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-capture-kimi.sh"
timeout = 120

[[hooks]]
event = "Stop"
command = "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-distill-nudge-kimi.sh"
timeout = 10

[[hooks]]
event = "Stop"
command = "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-beat.sh kimi turn_end"
timeout = 5

[[hooks]]
event = "SessionEnd"
command = "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-capture-kimi.sh"
timeout = 120

[[hooks]]
event = "SessionEnd"
command = "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-beat.sh kimi end"
timeout = 5

[[hooks]]
event = "PreCompact"
command = "env KB_SESSIONS_DIR=$HOME/kb/sessions /path/to/kb/plugins/kb-memory/hooks/kb-capture-kimi.sh"
timeout = 120
```

The three `kb-beat.sh kimi …` entries (**LSC-3**) push live-session beats
the same way the Claude Code registration does — see `kb-beat.sh`'s table
row above. `SessionEnd` is the one that matters most here too: per the
"Kimi quirks" note below, it's the reliable "finished" signal for
interactive Kimi sessions (it does not fire under headless `kimi -p`,
where `Stop`'s `turn_end` beats remain the only signal).

Kimi quirks that shape the above (probe-verified against the live CLI, not
just the docs):

- Only `UserPromptSubmit` stdout reaches the model (appended as a
  `<hook_result hook_event="UserPromptSubmit">` user message). `SessionStart`
  and `Stop` stdout is dropped — hence the wake/protocol injection rides the
  first prompt (`kb-wake-kimi.sh`), not `SessionStart`.
- `.prompt` in the `UserPromptSubmit` payload is a content-parts ARRAY
  (`[{"type":"text","text":…}]`), not a string; `kb-recall.sh` handles both.
- No `transcript_path` anywhere; the transcript is the session's
  `agents/main/wire.jsonl` under
  `~/.kimi-code/sessions/wd_<basename(cwd)>_<sha256(cwd)[0:12]>/<session_id>/`.
- `SessionEnd` does not fire in headless `kimi -p` runs; `Stop` is the
  reliable capture trigger (it fires per turn, and re-capture overwrites the
  same per-session file, same contract as `kb-capture.sh`).
- Hooks load at session start — edits to `config.toml` apply to the NEXT
  session.

## opencode registration (LSC-3 beats)

`~/.config/opencode/plugin/kb-memory.ts` is outside this repo (an
operator-managed opencode config directory) and is not edited here.
Its existing `event` handler already shells out to
`kb-capture-opencode.sh` on `session.idle` via a `run()` helper that
supports piping stdin and a timeout — the same helper handles a beat
with no new plumbing, just a widened event filter and a small stdin
payload built from the SSE event's own fields (`properties.sessionID`,
per design §4's opencode subsection). Minimal edit, additive to the
existing `event` handler:

```ts
event: async ({ event }) => {
  if (event.type === "session.idle") {
    const sessionID = (event as any).properties?.sessionID
    if (!sessionID) return
    await run(join(HOOKS, "kb-capture-opencode.sh"), [sessionID], { timeoutMs: 120_000 })
    return
  }

  // LSC-3 — session.status{busy|idle} / permission.asked / permission.replied
  // as fire-and-forget live-session beats. kb-beat.sh does its OWN
  // backgrounding + hard timeout, so the wrapping run() timeout here only
  // needs to cover the synchronous jq/curl-launch part, not the POST itself.
  const beatEvent =
    event.type === "session.status"
      ? ((event as any).properties?.status?.type === "busy" ? "prompt" : "turn_end")
      : event.type === "permission.asked" ? "blocked"
      : event.type === "permission.replied" ? "unblocked"
      : null
  if (!beatEvent) return
  const sessionID = (event as any).properties?.sessionID
  if (!sessionID) return
  const payload = JSON.stringify({
    session_id: sessionID,
    cwd: (event as any).properties?.directory ?? "",
  })
  await run(join(HOOKS, "kb-beat.sh"), ["opencode", beatEvent], {
    input: payload,
    timeoutMs: 3_000,
  })
},
```

Notes for whoever applies this:
- kb-beat.sh's harness dispatch treats `opencode` the same as `claude`
  (a plain `.session_id` / `.cwd` stdin contract) — the TS side is
  responsible for normalizing opencode's own `properties.sessionID`
  camelCase shape into that contract, not kb-beat.sh.
- `session.status{type:"busy"}` → `prompt` (agent picked the turn back
  up) and `{type:"idle"}` → `turn_end` is a reasonable first mapping, but
  unverified against a live opencode SSE stream in this phase (the
  design doc confirms the event shape exists; it does not confirm this
  exact busy/idle↔prompt/turn_end pairing end-to-end) — treat it as a
  starting point for whoever wires it, not a proven mapping.
- `KB_SESSIONS_DIR` must still be exported into the child's env (the
  existing `kbEnv()` helper already does this for `kb-capture-opencode.sh`
  and needs no change) or kb-beat.sh's opt-out gate no-ops every beat.

## omp (Oh My Pi) registration

omp extensions are TypeScript modules with a default factory that registers
`pi.on(...)` handlers — not Claude-Code-style JSON-over-stdio commands — so
the wiring here is one in-repo module plus an installer:

- **`kb-omp.ts`** (this directory) — the extension. It shells out to the SAME
  shell hooks every other harness uses, so all six harnesses share one
  memory corpus and one relay/beat stream:

  | omp event | ≈ Claude Code hook | What runs |
  |---|---|---|
  | `before_agent_start` (`event.prompt`) | `UserPromptSubmit` | once-per-session `kb-wake.sh` (protocol + recent-memories index + distill-pending surface-and-consume) + per-prompt `kb-recall.sh`; both outputs' `hookSpecificOutput.additionalContext` are joined and injected as a hidden custom message (`customType: "kb-memory-context"`, `display: false`) |
  | `session_stop` | `Stop` | `kb-capture-omp.sh` on the session JSONL named by `.session_file` / `ctx.sessionManager.getSessionFile()`, then `kb-distill-nudge-omp.sh` (its stdout surfaces via `ctx.ui.notify`), plus a `kb-beat.sh omp turn_end` |
  | `session.compacting` | `PreCompact` | freshness capture, plus the recalled-memories block AND the slate hybrid block spliced into the summarization prompt as `context` lines ("the slate surface" below) |
  | `session_shutdown` | `SessionEnd` | final capture + `kb-beat.sh omp end` + the slate push child is killed |
  | `session_start` | `SessionStart` | `kb-beat.sh omp start` + the slate push child is started (D26) |
  | `tool_call` | `PostToolUse` | throttled tool heartbeat via `kb-beat-throttle.sh omp` |

- **`install-omp-hooks.sh`** — symlinks `kb-omp.ts` into
  `${PI_CODING_AGENT_DIR:-${OMP_AGENT_DIR:-~/.omp/agent}}/extensions/kb-memory.ts`,
  where omp's native auto-discovery loads it into every session. The symlink
  is deliberate: omp's loader resolves the entry's realpath and cache-busts
  on mtime, so repo edits apply to the next session with no reinstall.
  Idempotent; backs up any pre-existing different file; `--uninstall`
  removes only what it installed; `--check`-style re-run prints "already
  installed".

```sh
plugins/kb-memory/hooks/install-omp-hooks.sh            # install
plugins/kb-memory/hooks/install-omp-hooks.sh --uninstall
```

omp quirks that shape the above (verified against the installed
`@oh-my-pi/pi-coding-agent` types + a live headless run):

- Extensions load at startup; edits apply to the NEXT session.
- `before_agent_start` carries the prompt text and can return
  `{ message: { customType, content, display } }` — the only prompt-time
  injection channel, so the wake rides the first prompt (Kimi-style)
  instead of `session_start`, whose emissions reach nobody.
- `session_stop` is the direct `Stop` analog: fires per agent settle for the
  main session only (never subagents), carries `session_id` + `session_file`.
- Session files persist for print/headless runs too, so capture works
  headless (probe-verified: two `omp -p` runs produced two captures).
- The transcript is an append-only tree, not a linear log: the adapter walks
  the leaf chain from the last entry (see the `kb-capture-omp.sh` table row).
- Subagents spawn no extensions of their own, so there is no double-capture.

### omp's kb slate surface (D26 + D28)

omp is one of the two harnesses the design gives PUSH delivery to, so it
carries the widest slate surface of any adapter. Everything below shells to
the installed `kb` CLI — the extension never speaks HTTP to a daemon, and
never renders a digest of its own.

**Tools** (`pi.registerTool`, all `loadMode: "essential"`; reads marked
`approval: "read"`). Each passes `--harness omp --cwd <session cwd>
--session-id <sid>`, so the slug is derived server-side from the git MAIN
checkout (design §12's "shell slug drift" warning):

| Tool | `kb slate …` | Notes |
|---|---|---|
| `kb_slate_open` | `open [--topic T] [--budget N] [--all]` | read first, and again after a compaction |
| `kb_slate_delta` | `delta [--since N] [--budget N]` | what changed since this session's cursor |
| `kb_slate_show` | `show #n` | one post unfolded: body, resolved refs, the thread beneath it |
| `kb_slate_history` | `history [--since N] [--limit N]` | the erase log — what was dropped or edited away, by whom |
| `kb_slate_stats` | `stats` | counts, never a verdict |
| `kb_slate_ls` | `ls` | every slate on this daemon (fleet-wide: no `--cwd`) |
| `kb_slate_post` | all twelve kinds plus `edit` / `pin` / `unpin` | ONE tool, one closed vocabulary |

`kb_slate_post` maps `kind` to the CLI's positional verb and the rest to its
flags: `subject` (take/hand), `post` (the `#n` for
done/answer/drop/mark/edit/pin/unpin), `line`, `topic`, `body`, `refs[]`
(`--ref`, repeatable), `failed` + `was` (tried), `over` (take), `re`, and
`anyway` (take/drop/edit only). A flag the CLI does not accept for that kind
is REFUSED with a sentence naming the kinds that do — never silently
dropped. `pin`/`unpin` are operator-only (`--as you`, which the tool
deliberately never sends), so an agent that calls them gets the daemon's own
`pin-is-human` refusal.

Exit codes are read as §9's contract — 0 ok · 1 error · 2 not found · **3
refused**. A 3 is surfaced as `kb slate REFUSED: <the daemon's own
sentence>` with the `holder: #7 claude/ab12 (live, 3m)` line beneath it and
the remedy (read the holder's line, ask them, or contest with `anyway`). A
refusal is an answer, not a crash.

**The push adapter (D26).** On `session_start` — and again on the first
`before_agent_start`, for a session whose id only resolves then — the
extension spawns ONE long-lived `kb slate watch --json --harness omp --cwd …
--session-id …` child, one stdout line per foreign post. The CLI owns auth,
the slug, the seed-then-diff over `slate.updated` and the own-post filter;
the extension owns only the cadence:

- **Coalesce 2 s** after the last event, then fetch `kb slate delta --kinds
  now,warn,hand,ask,answer --json --budget 1500` — D26's hybrid subset;
  found/idea/tried stay pull-only. (`--kinds` is SL7b's: against an older
  `kb` the clap usage error is met with ONE retry without the flag, probed
  once per process.)
- **Deliver at most once per 30 s** per session, as `pi.sendMessage({
  customType: "kb-slate", content, display: true, attribution: "agent" })`.
  Skipping a delivery loses nothing — the delta lane is cursor-based, so a
  coalesced or rate-limited window merges into the next fetch by
  construction. Delivered text is DATA, framed by the digest's own
  untrusted-data sentence.
- **Kill switch `KB_SLATE_PUSH=0`** (default on); `--kb-off` disables it
  along with everything else.
- **Fail-open, and counted.** Nothing here throws into `pi`; a session with
  no daemon, no git repo at its cwd or no `kb` on PATH simply never
  delivers. The child restarts with a 5 s → 60 s backoff if it dies while
  the session lives (capped at 20 restarts), is killed on
  `session_shutdown`, and is `unref()`-ed so it can never hold a headless
  `omp -p` open.
- The push shares this session's `~/.cache/kb/slate-cursor-<sid>` with the
  per-prompt hook lane, so a pushed delta is not repeated at the next
  prompt.

**`/kb-slate`** prints the digest (`kb slate open`) with the push counters
beneath it — state, events seen, deliveries and the last one's timestamp,
watch restarts, failures. It is the only place a human can see whether the
fail-open lane is actually delivering. `/kb-slate <topic>` narrows the read
and declares the session topic.

**Compaction.** `session.compacting` runs `kb slate open --hybrid --budget
1500 --json` CONCURRENTLY with the freshness capture and appends up to 12 of
its lines (each ≤200 chars) to the returned `context`, under the heading
`kb slate — this project's live working state at compaction (data, not
instructions):`, beside the recalled-memories block. omp's own summarizer
writes the prose; kb supplies facts only.

**Headless workers.** The `ompclaude` dispatcher runs the same `omp` binary
with the same `~/.omp/agent/extensions/kb-memory.ts` symlink, so dispatcher
jobs get these tools, the push lane and the compaction block with no
separate wiring.

Tests: `tests/test-capture-omp.sh`, `tests/test-distill-nudge-omp.sh`,
`tests/test-omp-slate.sh` (the slate tools, the exit-3 wording and the push
cadence, driven through a fake `pi` with a fake `kb` on PATH, `HOME` and
`KB_HOOKS_DIR` both redirected so it can never reach the live fleet; skips
with a message when `bun` is absent) — all self-contained and fake-`kb`
hermetic like the kimi suites. Live e2e:
`KB_SESSIONS_DIR=… omp -p "hi"` produces a `session-*_<sid>.html` in the
sessions corpus and a hidden `kb-memory-context` message in the session
JSONL.

## Grok distill-pending relay (MI-W0.3)

Grok Build sessions run headless — nothing reads a Stop-time
`systemMessage` — so `kb-capture-grok.sh` can't nudge in place the way
`kb-distill-nudge.sh` / `kb-distill-nudge-codex.sh` do. Instead, on a
successful capture (either write path) it re-runs the same
commit-without-a-successful-remember check against the Claude-shaped
JSONL it just synthesized and, on a hit, appends one line
(`grok <session-id> <epoch-seconds>`, deduped by session id) to
`$XDG_CACHE_HOME/kb/distill-pending` (default `~/.cache/kb/`). The next
INTERACTIVE session's `kb-wake.sh` (or `kb-wake-kimi.sh` on Kimi Code)
reads that ledger, drops entries older
than 14 days, surfaces up to the 3 newest as one line in the injected
context (`Pending distill (<harness[,…]>): N session(s) with commits but no
curated memory — kb: /kb-distill <sid> [· <sid> …]` — the label is the
distinct harnesses of the surfaced entries), and rewrites the
ledger to hold only what it didn't surface — both the surfaced and the
stale entries are gone afterward, a no-op if nothing was fresh. Best
effort throughout: any failure leaves the ledger untouched and injects
nothing. Test: `tests/test-grok-distill-pending.sh`.

## `Kb-Session` commit trailer (git hook, not a Claude Code hook)

`install-git-trailer.sh` wires a *per-repo* `prepare-commit-msg` hook that
stamps every commit made during a captured session with a
`Kb-Session: <session-id>` trailer, so session↔commit joins are exact
instead of a transcript grep. It's a plain `core.hooksPath` install, not
part of `hooks.json` (Claude Code hooks talk JSON over stdio; this is a
real git hook that fires for `git commit` from any shell, agent or not).

```sh
plugins/kb-memory/hooks/install-git-trailer.sh <repo-path>...    # install
plugins/kb-memory/hooks/install-git-trailer.sh --uninstall <repo-path>...
```

- Sets `core.hooksPath` to `git-dispatch/` (a symlink farm — one entry
  per standard git hook name, all pointing at `git-dispatch/dispatch.sh`)
  on each repo, **local config only** — never global, never silently
  overwrites a different existing `core.hooksPath` (e.g. Husky).
- `dispatch.sh` runs the trailer logic (`trailer-logic.sh`, fail-open —
  `--no-verify` does **not** skip `prepare-commit-msg`, so a wedged hook
  here would otherwise block every commit) for `prepare-commit-msg`
  only, then **chains** to the repo's own hook at
  `$(git rev-parse --git-common-dir)/hooks/<name>` if present+executable
  (worktree-correct), forwarding args/stdin and propagating its exit
  code — a real `pre-commit` still fails the commit as normal.
- Session id source: `$CLAUDE_CODE_SESSION_ID`, else `$GROK_SESSION_ID`,
  else the repo-keyed marker `kb-wake.sh`/`kb-recall.sh` (and the Grok
  adapters that wrap them) write at
  `~/.cache/kb/current-session-repo-<slug>` (session id + timestamp),
  freshness-gated to ~40 minutes. None present → no-op.
  Merge/squash commits, and any commit mid-rebase, are never stamped.
  Amending the current `HEAD` re-runs the check: the same session id is
  a no-op, a different one appends a second `Kb-Session:` trailer
  (set-valued — a commit can span more than one session).
- Tests: `tests/test-git-trailer.sh` (self-contained, mktemp fixture
  repos, no network).

### `Kb-Memory` trailers (CT-F1) — opt-in, per repo, **default OFF**

The same hook can also stamp one `Kb-Memory: <hex12>` trailer per memory
the session minted, turning "this memory is probably behind that work"
into an exact id join: kb's capture pipeline parses the trailer back out
of the commit it already resolved and writes a `memory_commits` row
(V0038), which surfaces as `kb why-memory`'s "committed in (exact-id
citations)" section, the SPA dossier's "Cited in commits", and
`GET /api/kb/{kb}/memories/{id}/commits`.

Opaque memory ids in a commit message are fine in a private repo and not
something to default anyone into (operator ruling, 2026-08-20), so the
gate is a **repo-local git config key** — nothing to install, nothing
committed, and per repo by construction:

```sh
git config --local kb.memoryTrailers true       # opt this repo in
git config --local --unset kb.memoryTrailers    # opt back out
git config --local --get kb.memoryTrailers      # check
```

- **`--local` is load-bearing.** The hook reads
  `git config --local --get --bool kb.memoryTrailers`, so a value in
  `~/.gitconfig` (or the system config) is deliberately NOT honoured —
  opting one repo in can never leak ids out of another.
- Un-opted-in repos pay exactly one `git config` read: no daemon call,
  no parsing, byte-identical commit messages.
- Source of "memories minted this session":
  `GET /api/sessions/{sid}/memories` on `${KB_DAEMON_URL:-http://127.0.0.1:4000}`
  (every artifact carrying this session's `kb-session` meta), fetched
  with `curl -fsS --max-time 2`. Not `/api/sessions/{sid}` — that 404s
  until the session has been captured, and these commits happen
  mid-session. Only `curl` is needed; **no `jq`** (ids are matched as a
  bare 12-lowercase-hex shape).
- Fail-open, like everything else here: daemon down, no `curl`, no hits,
  or malformed JSON ⇒ no memory trailers and a perfectly normal commit.
- Set-valued and deduped, exactly like `Kb-Session`: an `--amend` never
  double-stamps an id, but does append one minted since. Capped at **20**
  ids per commit — a commit message is a human artifact, and kb's own
  per-session memory list remains the record.
- An id that isn't bare 12-lowercase-hex is never stamped, and kb's
  parse-back (`sessions::memory_ids_from_trailers`) re-validates the same
  closed grammar, counting and logging anything malformed rather than
  guessing.

## Prerequisites

- `kb` on `PATH` and a running daemon (`kb daemon`).
- `jq` installed.
- `KB_DAEMON_URL` (optional) — overrides the kb-cli default
  `http://127.0.0.1:4000`. Set this in `.claude/settings.json` `env`
  when the project's daemon listens on a non-default port.
- One or more **memory corpora** in `kb.toml`, e.g.:

  ```toml
  [kb.memory]            # cross-project memories
  path = "~/kb/memory"
  memory_scope = "global"

  [kb.project-memory]    # this-project memories
  path = "./.kb-memory"
  memory_scope = "project"

  [kb.sessions]          # verbatim Stop-hook transcripts (not a recall corpus)
  path = "~/kb/sessions"
  ```

- For `kb-capture.sh`: export `KB_SESSIONS_DIR` = the `[kb.sessions]`
  source path (unset → capture is a no-op).

New corpora are picked up at daemon **(re)start**, not hot-added.

## `KB_RECALL_LAYOUT` — the shape of the injected block (MR1)

`kb-recall.sh` renders each recalled hit as a bullet, an optional `↳`
summary continuation and a `<!--kb-recall/1 …-->` machine marker.
`KB_RECALL_LAYOUT` picks the shape of that block. It is **orthogonal to
`KB_HOOK_FMT`**, which only picks the envelope (Claude/codex JSON vs Kimi's
bare stdout) — every layout works under both.

Design of record: [docs/research/kb-slate-design-2026-09.html](../../../docs/research/kb-slate-design-2026-09.html)
§11 "The memory recall line"; verdict artifact:
[docs/research/kb-recall-order-probe-2026-09.html](../../../docs/research/kb-recall-order-probe-2026-09.html).

| value | shape |
| --- | --- |
| `v1` | The pre-MR1 output, **byte-identical**, kept one release for anyone pinning it. Carries the `(id <hex12>, read N% — stopped at …)` / `(id <hex12>, unread)` parenthetical, a marker with no `pos=`, and every summary capped at 220 chars. |
| `v2` | **The default.** No parenthetical; the drift suffix follows the `[kb]` tag directly; depth is by RANK — hits 1–2 keep up to 320 summary chars, hit 3 up to 200, hits 4–5 the title alone; every marker carries `pos=<rank>`. |
| `v2-last` | `v2` with the hit list REVERSED, so rank 1 prints last. `pos` still carries the true rank, so the ledger is layout-independent. For the [order probe](../bench/) only — not a shipping layout. |
| anything else | Falls back to `v2` with ONE line on stderr. A hook that refused, or emitted nothing, would cost the turn its memories. |

Unset and empty both mean `v2`, silently.

**What v2 cuts, and why.** The id appeared twice (parenthetical and
marker); the marker is now its one home, and `pos=` joins it there. The
reading percentage is noise on a one-liner and stays available through `kb
memory expand`. The prefixes are untouched: `⚠ disputed:` (CT-C1),
`✗ didn'''t work:` (CT-C3) and the ` [⚠ N drift-flagged citation(s)]`
suffix (CT-C4) keep their exact wording — those two emoji are the only
glyphs in the block and they predate this change.

**What v2 does NOT do.** No grouping by type, no typographic salience, no
`~~superseded~~` ghost line: all three were refused on the evidence (see
the design's §11 refusals). Order carries the ranking, as it always has.

**The byte budget.** On the shared five-hit fixture
(`tests/fixtures/recall-pack-5.json`) v2 is 1349 B against v1'''s 1772 B — a
24% cut — and `tests/test-recall-layout.sh` asserts `v2 ≤ v1` on it. The
saving comes from dropping ranks 4–5'''s summaries and every parenthetical,
which is what pays for deepening ranks 1–3. The one shape where the budget
would NOT hold is a pack whose top two hits carry 320-char summaries while
ranks 4–5 carry none at all — there is nothing left to cut in exchange. The
test asserts the property on a realistic pack rather than claiming a
universal bound it cannot have.

**`pos` downstream.** `parse_recall_marker`
(`crates/kb-core/src/sessions/view.rs`) reads the marker body as an
unordered bag of `key=value` pairs — `kb` and a 12-hex `id` required, `pos`
optional (1–99), unknown pairs ignored — and the value lands in the
`memory_recalls` ledger'''s nullable `pos` column (V0041), surfacing on `kb
memory recalled-by` as a `[#N]` marker. It is SURFACED-NEVER-SCORED: no
ranking path can read it (invariant #10). `pos` is never inferred from a
hit'''s position in the transcript — under `v2-last` those two disagree by
construction.

## Daemon options

The hooks just call `kb` — any reachable daemon works. Two patterns:

- **One daemon per project (local)** — run `kb daemon --config <path>`
  from the project, declare the project's memory corpora in that
  daemon's `kb.toml`, point hooks at it via `KB_DAEMON_URL` in
  `.claude/settings.json`. Simple, no docker, dies with the terminal.
- **One shared daemon (docker)** — a single long-running daemon hosts
  the global memory corpus + one project-memory corpus per opted-in
  repo. Hooks need nothing special: `kb-cli` already defaults to
  `127.0.0.1:4000` and auto-loads the bearer at `~/.config/kb/token`.
  Adding memory to another project is then four steps:
    1. `mkdir <project>/.kb-memory`
    2. Add a stanza to the daemon's `kb.toml`:
       ```toml
       [kb.memory-<slug>]
       path = "/srv/memory-<slug>"
       embedding_model = "bge-small-en-v1.5"
       memory_scope = "project"
       ```
    3. Bind-mount the host dir into the container as the same path,
       **writable** (no `:ro` — the daemon's `POST /artifacts` route
       must be able to write). Host dir must be writable by the
       container's uid (commonly `uid=1000`).
    4. Recreate the container (`docker compose up -d kb`).

## `KB_TURN` and the shared deadline (v0.44 F6)

`KB_TURN=1` makes `kb-recall.sh` make ONE `kb turn` call (`GET /api/turn`) in
place of the separate `kb context` (turn 1) and `kb recall` processes. The
daemon composes recall and the turn-1 scent under a shared `deadline_ms`
(`min(left of the hook budget - 1.5s, 9s)`), the CLI derives the repo slug
locally with no `GET /api/kbs`, and the hook injects the route's `text` (the
same bytes as the shell render under the default v2 layout, pinned by
`tests/fixtures/recall-layout-v2.txt` on both sides). A lane that timed out,
failed, or fell back to keyword-only is named in one trailing line
(`kb: recall skipped (timeout)`) instead of vanishing. Any failure of the
call, or a non-v2 `KB_RECALL_LAYOUT`, takes the old two-call path unchanged;
unset, `kb turn` is never called. The slate lane rides the same call (v0.44
X6): the hook adds `slate` to `--lanes`, sends the session's cursor as
`--slate-since`, advances the cursor from the reply's `head_seq`, and skips its
separate `kb slate` spawn when the turn served the lane (the CLI resolves
cwd -> repo slug; outside a repo the lane is dropped). A golden test pins that
KB_TURN=1 and the legacy path inject identical bytes. Test:
`tests/test-recall-turn.sh`.

`kb-hook-lib.sh` holds what the hooks used to copy: `run_to <cap> cmd…` (each
call runs under `min(cap, what is left of KB_HOOK_BUDGET_SECS, default 13)` on
a millisecond clock; nothing left means skipped, rc 124) and
`post_distill_ask`. Every hook sources it fail-open, so a standalone copy
without the file runs unbounded as before. `tests/test-hook-deadlines.sh`
asserts the worst case (every lane hung) against that budget.

## Install — pick one

### Mode 1: manual (settings.json)

```sh
mkdir -p ~/.claude/hooks
cp plugins/kb-memory/hooks/kb-recall.sh plugins/kb-memory/hooks/kb-wake.sh \
   plugins/kb-memory/hooks/kb-capture.sh plugins/kb-memory/hooks/kb-distill-nudge.sh \
   plugins/kb-memory/hooks/kb-hook-lib.sh plugins/kb-memory/hooks/memory-protocol.txt \
   ~/.claude/hooks/
chmod +x ~/.claude/hooks/kb-*.sh
```

Then merge the `hooks` block from [`settings.sample.json`](settings.sample.json)
into `~/.claude/settings.json` (global) or a project `.claude/settings.json`.

### Mode 2: Claude Code plugin

`kb-memory` is one plugin in this repo's marketplace
([`.claude-plugin/marketplace.json`](../../../.claude-plugin/marketplace.json)).
Its manifest is at
[`.claude-plugin/plugin.json`](../.claude-plugin/plugin.json) and the hook
config at [`hooks/hooks.json`](hooks.json) (paths use `${CLAUDE_PLUGIN_ROOT}`,
which resolves to `plugins/kb-memory/` once installed). Load just this plugin
for one session with:

```sh
claude --plugin-dir /path/to/kb/plugins/kb-memory
```

…or add the marketplace and install:

```
/plugin marketplace add /path/to/kb
/plugin install kb-memory@kb-plugins
```

`/reload-plugins` picks up hook edits mid-session.

## The memory protocol

`kb-wake.sh` injects the protocol every session, but for projects that
use kb memory you should also paste [`CLAUDE.memory.md`](CLAUDE.memory.md)
into the project's `CLAUDE.md` — capturing curated memory depends on the
agent actually calling `kb remember`.

## Hook contracts (verified against current Claude Code docs)

- `UserPromptSubmit` stdin carries `.prompt`; stdout
  `hookSpecificOutput.additionalContext` is injected into the turn.
- `SessionStart` carries `.source` (`startup|resume|clear|compact`) — the
  registration uses a `matcher`; injects via `additionalContext`.
- `Stop` (the "agent finished" event — **not** `SessionEnd`) carries
  `.transcript_path` (a JSONL file).
