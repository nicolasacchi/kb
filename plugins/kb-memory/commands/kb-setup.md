---
description: Guided first-run bootstrap for kb as Claude Code's memory — checks the kb binary and daemon, writes a minimal kb.toml with a memory + a sessions corpus, starts the daemon, and verifies. Stops for your approval before writing files or starting anything.
---

You are running **`/kb-setup`**: a guided, stepwise bootstrap that gets a fresh
machine from "no kb" to "a running daemon with a memory corpus and a sessions
corpus". Run the steps **in order**, show the user each check's result, and
**STOP for explicit approval before any step that writes a file, creates a
directory, or starts a process** (Steps 4 and 5). Never guess past a failed
check — surface it and wait.

The `kb` CLI is the only path. Everything here is idempotent-friendly: re-running
after a partial setup should detect what already exists and skip it.

## Step 0 — orient

Note the current OS and the user's home directory (you'll write absolute paths —
kb's config loader does **not** expand a leading `~` in a corpus `path`, so use
the real home path, e.g. `/home/alice/kb/memory`, never a literal `~/kb/memory`).

## Step 1 — is `kb` on PATH?

```bash
command -v kb && kb --version
```

- **Found** → report the version, continue to Step 2.
- **Not found** → offer the installer and **STOP** until it's installed:

  ```bash
  curl -fsSL https://raw.githubusercontent.com/nicolasacchi/kb/main/scripts/install.sh | sh
  ```

  (Installs `kb` + `kb-embedder` into `~/.local/bin`; honors `$PREFIX`.) If the
  user prefers building from source, point them at the repo README's *Install*
  section instead. After they install, re-run Step 1. If `~/.local/bin` isn't on
  their PATH, tell them to add it before continuing.

## Step 2 — is a daemon already reachable?

```bash
kb status
```

`kb status` hits the daemon at `http://127.0.0.1:4000` (override with
`--daemon <url>` or `KB_DAEMON_URL`). Interpret:

- **Reachable** (prints an observability snapshot) → a daemon is already up.
  Skip Steps 3–5. Go to Step 6 and report what already exists. If its config has
  **no** memory/sessions corpus, offer to add the two stanzas from Step 3 to the
  daemon's existing `kb.toml` (show the diff, get approval, then
  `kb daemon stop` / restart — new corpora are picked up only at (re)start).
- **Unreachable** ("no kb daemon reachable…") → no daemon yet. Continue to Step 3.

## Step 3 — plan the config (show it, don't write yet)

The daemon loads `~/.config/kb/kb.toml` when started with no `--config`. Plan:

- **Config file:** `<home>/.config/kb/kb.toml`
- **Corpus dirs:** `<home>/kb/memory` and `<home>/kb/sessions`

Present this exact `kb.toml` to the user, with `<home>` replaced by the real
absolute home path (and honor `$KB_HOME` if set — then everything lives under
`$KB_HOME` instead):

```toml
# kb.toml — written by /kb-setup. Loaded by `kb daemon` from ~/.config/kb/kb.toml.
# Reference: https://github.com/nicolasacchi/kb/blob/main/docs/configuration.md

[server]
addr = "127.0.0.1:4000"   # loopback-only — no token needed for local use

# Cross-project curated memory (kb remember / kb recall).
[kb.memory]
path = "<home>/kb/memory"
embedding_model = "bge-small-en-v1.5"
memory_scope = "global"

# Verbatim Claude Code session transcripts — episodic memory
# (kb why / kb recollect; captured by the kb-memory Stop hook).
[kb.sessions]
path = "<home>/kb/sessions"
embedding_model = "bge-small-en-v1.5"
```

Tell the user the first index will download the ~130 MB `bge-small-en-v1.5`
model once (to `<cache>/kb/models/`). If `~/.config/kb/kb.toml` **already
exists**, do not clobber it — show its contents and ask whether to merge just
the two `[kb.*]` stanzas into it instead. **STOP and get approval** before Step 4.

## Step 4 — create dirs + write the config (after approval)

Only after the user approves:

```bash
mkdir -p "$HOME/.config/kb" "$HOME/kb/memory" "$HOME/kb/sessions"
```

Write the planned `kb.toml` to `$HOME/.config/kb/kb.toml` (with the absolute
paths substituted). Confirm the file landed (`cat` it back).

## Step 5 — start the daemon (after approval)

`kb daemon` runs in the **foreground** and stays up. Start it **detached / in
the background** so this command can keep going, then poll for readiness:

```bash
kb daemon        # launch in the background (do NOT block on it)
# then, after a moment:
kb status
```

If `kb status` still reports unreachable after a few seconds, surface the
daemon's startup output (a bad `addr`, an unwritable corpus dir, or a dim
mismatch will be named there) and stop for the user to resolve it.

## Step 6 — verify

```bash
kb status              # daemon up, both kbs listed
kb model list          # bge-small-en-v1.5 resolves / downloads
```

Optionally prove the write path end-to-end (only if the user wants it):

```bash
kb remember "kb-setup verified on $(date -u +%Y-%m-%d)" --title "setup smoke" --global
kb recall "kb-setup verified" --scope all --json  # explicit: deterministic
                                                    # from any cwd, not
                                                    # narrowed to whatever
                                                    # project auto (the
                                                    # bare default) infers
```

## Step 7 — report what exists now + next steps

Tell the user plainly:

- **Running:** a kb daemon on `127.0.0.1:4000` with `memory` (global) and
  `sessions` corpora.
- **To capture sessions:** the `kb-memory` plugin's Stop hook only writes
  transcripts when `KB_SESSIONS_DIR` points at the sessions corpus. Add to
  `~/.claude/settings.json` (or the project's `.claude/settings.json`):

  ```json
  { "env": { "KB_SESSIONS_DIR": "<home>/kb/sessions" } }
  ```

- **To turn on automatic recall + capture:** enable the `kb-memory` plugin
  (`/plugin install kb-memory@kb-plugins`) — its `UserPromptSubmit`,
  `SessionStart`, and `Stop` hooks wire recall/protocol/capture. See the
  plugin's README.
- **Curated memory:** paste `CLAUDE.memory.md` (in the kb-memory plugin) into
  the project's `CLAUDE.md` so the agent calls `kb remember`; consolidate later
  with **`/kb-reflect`**.

$ARGUMENTS
