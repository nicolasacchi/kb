# kb-memory

Claude Code memory via the kb daemon. Three surfaces:

- **Hooks** (`hooks/`) — four deterministic, **LLM-free** hooks that wire kb in
  as Claude Code's memory: `UserPromptSubmit` recall, `SessionStart` protocol
  re-injection, verbatim `Stop`-hook transcript capture, and a `Stop`-hook
  nudge that suggests `/kb-distill` when a session committed code but kept no
  memory. The same corpus also serves other coding-agent harnesses — Codex,
  opencode, Grok Build, Kimi Code, and omp (Oh My Pi) — via per-harness
  capture/nudge adapters and, for omp, a TS extension (`kb-omp.ts`) plus
  `install-omp-hooks.sh`. Full detail, install modes, and the memory protocol
  are in [`hooks/README.md`](hooks/README.md).
- **`/kb-distill`** (`skills/kb-distill/SKILL.md`) — the episodic→semantic
  bridge: distills one captured session (or a `--since` window) into 1–3
  durable, provenance-stamped memories — what was decided, what shipped, what
  failed and why. Quality-gated (outcome + why + citation required), deduped
  against existing memory (`kb recall --no-floor`, `--supersedes` on overlap),
  idempotent (`--session-id` stamps lift `memory_count`, so re-runs skip),
  `--dry-run` supported. Write vocabulary is ADD/SUPERSEDE only — it never
  runs `kb forget`; corpus-wide merges stay with `/kb-reflect` (human-gated).
- **`/kb-verify`** (`skills/kb-verify/SKILL.md`, CT-C2) — does memory still
  match the code? Sweeps code-citing memories through kb-code's doclens
  (fresh resolution, never cached — invariant #2) and files dated
  `[kb-drift]` comments on citations that no longer resolve (one open
  comment per (memory, path), re-runs are no-ops), plus a born-stale check
  and an escalation path to `kb memory flag` when the FACT itself is wrong.
  Write vocabulary is COMMENT/FLAG only — never `forget`/`--supersedes`.
  `--dry-run` supported.
- **`/kb-weekly`** (`skills/kb-weekly/SKILL.md`) — one project's ledger (a
  week — or any N-day window — of sessions/commits/decisions/research)
  turned into a short narrative "week in review" note: facts from
  `GET /api/sessions/ledger` (daemon-computed, LLM-free), prose from the
  agent, saved back as a kb note in the sessions corpus.
- **`/kb-slate-tidy`** (`skills/kb-slate-tidy/SKILL.md`) — housekeeping for a
  project's `kb slate` board (see [`docs/slate.md`](../../docs/slate.md)): reads
  `kb slate open --all --json` plus `history`, drops noise the running session
  or a presumed-ended session authored (NEVER a live other session's
  `now`/`warn`/`take`/`hand`), merges near-duplicate `found`s with
  `--supersedes`, and closes (`done`) asks whose asker's session has ended.
  Every action is an ordinary attributed slate post — no separate "tidy
  report" kind exists.
- **`/kb-slate-distill`** (`skills/kb-slate-distill/SKILL.md`) — the slate's
  own episodic→semantic bridge: reads a closed or rotated slate's posts
  (current generation via the API, archived generations by reading
  `ledger.<gen>.jsonl` directly — the one sanctioned read-only exception,
  since no route serves them) and writes 1-5 `kb remember` memories
  (decisions plus the `tried` dead ends with their reasons), one narrative
  note, and a dated plan-file Status line. Same write discipline as
  `/kb-distill`: ADD/SUPERSEDE only, dedup via `kb recall`, provenance-linked.
- **`/kb-setup`** (`commands/kb-setup.md`) — a guided first-run bootstrap. It
  checks `kb` on PATH (offering [`scripts/install.sh`](../../scripts/install.sh)
  if missing), checks a daemon with `kb status`, and — after your approval —
  writes a minimal `~/.config/kb/kb.toml` with a **memory** corpus (global) and
  a **sessions** corpus, creates their dirs, starts the daemon, and verifies.
  Stepwise with user-visible checkpoints; it stops before writing or starting
  anything.

## Quick start

```
/plugin marketplace add nicolasacchi/kb
/plugin install kb-memory@kb-plugins
/kb-setup
```

`/kb-setup` gets you from nothing to a running daemon with memory + sessions
corpora. Then enable the hooks (they ship with this plugin) for automatic
recall/capture, paste [`hooks/CLAUDE.memory.md`](hooks/CLAUDE.memory.md) into
your project's `CLAUDE.md`, distill finished sessions into durable facts with
`/kb-distill` (this plugin — the Stop-hook nudge tells you when), and
consolidate corpus-wide later with `/kb-reflect` (the `kb-reflect` plugin).

## Prerequisites

- `kb` + `kb-embedder` on PATH — build from source, or
  `curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install.sh | sh`.
- `jq` (used by the hooks).

See [`hooks/README.md`](hooks/README.md) for daemon topology options
(one-per-project vs. one shared docker daemon), `KB_DAEMON_URL` /
`KB_SESSIONS_DIR`, and the manual (settings.json) install path.
