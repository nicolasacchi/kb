# Track-S deploy runbook

Example steps to turn on `/api/sessions/*` + the SPA route on an existing
reverse-proxied deployment (e.g. `kb.example.com`). Everything except the
production-config edits below ships already wired in the daemon; apply
these by hand and rebuild.

## 1. Add the bind mount to your deployment's compose file

In the `kb:` service's `volumes:` list, after the existing memory
mounts, add:

```yaml
      # v0.14 track-S — captured Claude Code transcripts (Stop-hook
      # `kb-capture.sh` writes session-<ts>-<sid>.html files). Writable
      # for the same reason as the memory mounts above.
      - ~/kb/sessions:/srv/sessions
```

The host dir `~/kb/sessions` must already exist (uid 1000, writable).

## 2. Add the kb section to your deployment's `kb.toml`

At the end of the file, after `[kb.memory-kb]`:

```toml
# v0.14 track-S — captured Claude Code transcripts. The Stop hook
# (`plugins/kb-memory/hooks/kb-capture.sh`) wraps each session's JSONL transcript as
# session-<ts>-<sid>.html and drops it into $KB_SESSIONS_DIR; the
# daemon's indexer treats `kb-category=memory-session` as a session
# enrichment trigger (V0008 sessions sqlite + /api/sessions/* +
# session.captured SSE).
[kb.sessions]
path = "/srv/sessions"
memory_scope = "global"
```

## 3. Rebuild + restart

Example redeploy flow (adapt to your own compose layout):

```bash
cd ~/your-deploy-dir && export KB_GIT_SHA=$(git -C ~/path/to/kb rev-parse --short=12 HEAD) && docker compose build kb && docker compose up -d kb
```

## 4. Export `KB_SESSIONS_DIR` in the local Claude env

So `plugins/kb-memory/hooks/kb-capture.sh` starts writing transcripts on Stop. Either
in `~/.zshrc` (persistent) or in the per-project `.claude/settings.json`:

```bash
export KB_SESSIONS_DIR=~/kb/sessions
```

## 5. Verify

```bash
curl -s https://kb.example.com/api/sessions | jq '.sessions | length'
```

Once a real Claude Code session ends, the row should surface at
`https://kb.example.com/sessions` and in `kb sessions list`.
