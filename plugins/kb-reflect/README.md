# kb-reflect

The memory "dream": one slash command, no hooks, no skills.

- **`/kb-reflect [<session-id>]`** (`commands/kb-reflect.md`) — consolidates
  past Claude Code sessions into durable curated facts. It reads session
  digests via `kb sessions list`/`kb sessions show`, dedups each candidate
  fact against existing memory (`kb recall --scope all --no-floor`),
  proposes ADD / SUPERSEDE / MERGE / NOOP in a dry-run table, and — only
  after you approve — writes graded, provenance-linked facts with
  `kb remember`. **You** are the distiller (same model, same conversation);
  the kb daemon never runs an LLM of its own.
  - With no argument: sweeps recent sessions that produced no curated fact
    yet (`memory_count == 0`).
  - With a session id: force-reflects that one session, ignoring the
    `memory_count` gate.

`/kb-reflect` is the **corpus-wide, human-gated** half of memory
consolidation — it is the only place a MERGE (`kb forget --purge`, an
irreversible hard delete of the ids folded into a merged fact) is allowed,
which is why every write is gated behind an explicit dry-run approval. Its
per-session, autonomous-safe sibling is `/kb-distill` (the `kb-memory`
plugin), which writes ADD/SUPERSEDE only and never deletes.

## Requires

- A running kb daemon (`kb daemon`) with at least one memory corpus and a
  sessions corpus configured (see the `kb-memory` plugin's `/kb-setup`).
- `kb` on `PATH`.
- Claude Code — `/kb-reflect` is a Claude Code slash command; this plugin
  has no hooks and registers nothing for other harnesses. (Sessions
  captured from other harnesses — Codex, opencode, Grok Build, Kimi Code,
  omp, via the `kb-memory` plugin's capture adapters — still show up in
  `kb sessions list` and can be reflected on the same way.)

## Install

```
/plugin marketplace add nicolasacchi/kb
/plugin install kb-reflect@kb-plugins
```

Then run `/kb-reflect` (sweep mode) or `/kb-reflect <session-id>` (forced,
single-session) inside Claude Code.

## Configuration

None beyond the ordinary `kb` CLI daemon resolution — `KB_DAEMON_URL` to
point at a non-default daemon, and the daemon's own bearer token if it's
running non-loopback.
