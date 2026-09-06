# Driving kb from any agent

This file is the **agent-agnostic** entry point for working with the `kb`
daemon (Claude Code, Codex, Cursor, Aider, or any tool that can shell out).
The `kb` CLI is a plain binary that speaks HTTP to the daemon and reads its
bearer token from `~/.config/kb/token` automatically — there is no
Claude-specific coupling in the CLI itself. Claude Code users also get the
in-tree plugins (`plugins/kb-*`, slash commands + hooks) and the deeper
project guide in [`CLAUDE.md`](CLAUDE.md); everything below works the same
from any agent.

Run `kb tools` for the full, always-current verb manifest.

## Reviewing artifacts (the human↔agent comment loop)

A reviewer leaves margin comments on a rendered artifact in the kb web UI;
your job is to read them, edit the source, and reply. The CLI is the **only**
safe path — never edit `.review/<id>.json` directly (the daemon owns those
files; the SPA listens over SSE).

1. **Resolve the artifact** — `kb find <path-or-name> --json` prints the
   12-hex `id`, `path`, and `source_relative`.
2. **Read open comments** — `kb comments list --path <artifact> --json`.
   Focus on `you`-authored (reviewer) comments still `open`. For full
   context, `kb comments export --path <artifact>` prints the whole thread.
3. **Address each** — open the `source_relative` file and apply the edit the
   comment asks for. The file on disk is the source of truth; the watcher
   reindexes automatically.
4. **Reply, then resolve** — describe what changed, then close it:

   ```bash
   kb comments reply <comment_id> --path <artifact> --body "What changed."
   kb comments resolve --path <artifact> <comment_id>     # or --all when done
   ```

5. **If your edit moved an anchored element**, re-point it explicitly with
   `kb comments reanchor <comment_id> --anchor section:<new-id>` — the
   indexer's fuzzy resolver is *detection-only* and never rewrites anchors
   on its own.

**Triage rule of thumb:** a single clear, actionable fix (typo, phrasing) →
edit + reply + resolve hands-off. Anything ambiguous or large → reply with a
question or options and **leave it open** for the human. When unsure whether
a comment wants a change or a discussion, reply rather than silently
resolving.

### Landing several mutations at once (`apply`)

When one review turn produces multiple changes (reply to A, re-anchor B,
resolve C), send them as one **atomic** batch instead of N round-trips — all
ops land or none do, with a single live SSE update:

```bash
kb comments apply --path <artifact> --ops-json '[
  {"op":"add_reply","comment_id":"c_aaa","author":"claude","body":"fixed in §2"},
  {"op":"set_anchor","comment_id":"c_bbb","anchor":{"kind":"section","id":"overview"}},
  {"op":"resolve","comment_id":"c_ccc"}
]'
# or: --ops-file ops.json   (a bare array, or {"ops":[…]})
```

Ops mirror the fine-grained verbs: `add_comment`, `add_reply`,
`edit_comment`, `edit_reply`, `set_anchor`, `resolve`, `unresolve`,
`resolve_all`, `unresolve_all`, `delete_comment`, `delete_reply`. Ops
reference existing ids only (a batch can't reply to a comment it also
creates — the id is server-minted).

### Moving comments with the file (`export --embed` / `import`)

Comments live out-of-band in a daemon-owned sidecar, so a downloaded artifact
doesn't normally carry them. To hand a single self-contained file to someone
(or archive it) with its comments **inside** the HTML, and read them back
later:

```bash
kb comments export --path <artifact> --embed -o review-copy.html   # bake in
kb comments import review-copy.html --path <artifact>              # read back
```

`import` preserves comment ids, statuses, replies, and timestamps, and
refuses to overwrite existing non-empty comments unless you pass `--force`.

## Other common tasks

- **Search**: `kb search "<query>" [--kb NAME] [--json]` (hybrid by default).
- **Read an artifact**: `kb find <name>` → `kb cat <id>` / `kb get <id> --format md`.
- **Authoring** kb-indexed HTML: see
  [`docs/authoring-artifacts.md`](docs/authoring-artifacts.md) (real title,
  stable element ids for durable anchors, `<template id="kb-prompt">`,
  `kb-tags`/`kb-category` metas).
- **Full comment workflow + the realtime watch loop**:
  [`docs/comment-workflow.md`](docs/comment-workflow.md).

Verbs that talk to the daemon accept `--daemon URL` (default
`http://127.0.0.1:4000`); `--config PATH` is global.
