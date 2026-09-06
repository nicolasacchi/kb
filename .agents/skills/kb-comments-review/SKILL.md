---
name: kb-comments-review
description: Triage open review comments on a kb artifact — list them, edit the source to address each, then reply + resolve. Agent-agnostic; drives the `kb` CLI.
---

# kb comment review (agent-agnostic)

A portable mirror of the Claude Code `/kb-comments` command for any
skill-capable agent (Codex, Cursor, …). The full root guide is
[`AGENTS.md`](../../../AGENTS.md); the deep workflow doc is
[`docs/comment-workflow.md`](../../../docs/comment-workflow.md).

Handle the open review comments on the artifact the user names (a filename,
source-relative path, or folder). The `kb` CLI is the **only** safe path —
never edit `.review/<id>.json` directly; the daemon owns those files
(ETag-protected, SPA panels listen over SSE). The CLI reads its bearer token
from `~/.config/kb/token` and defaults to `--daemon http://127.0.0.1:4000`.

## Loop

1. **Resolve** — `kb find <artifact> --json` → the 12-hex `id`, `path`,
   `source_relative`.
2. **Read** — `kb comments list --path <artifact> --json`; focus on
   `you`-authored comments still `open`. `kb comments export --path
   <artifact>` prints the whole thread for context.
3. **Address each** — edit the `source_relative` file on disk (the source of
   truth; the watcher reindexes automatically).
4. **Reply + resolve**:

   ```bash
   kb comments reply <comment_id> --path <artifact> --body "What changed."
   kb comments resolve --path <artifact> <comment_id>     # or --all
   ```

5. **Re-anchor if needed** — if your edit moved the anchored element:
   `kb comments reanchor <comment_id> --anchor section:<new-id>`. The
   indexer's fuzzy resolver is detection-only and never rewrites anchors
   itself.

## Batch a turn atomically

Send multiple mutations as one all-or-nothing batch (one round-trip, one live
SSE update):

```bash
kb comments apply --path <artifact> --ops-json '[
  {"op":"add_reply","comment_id":"c_aaa","author":"claude","body":"done"},
  {"op":"resolve","comment_id":"c_aaa"}
]'
```

Ops: `add_comment`, `add_reply`, `edit_comment`, `edit_reply`, `set_anchor`,
`resolve`, `unresolve`, `resolve_all`, `unresolve_all`, `delete_comment`,
`delete_reply`. They reference existing ids only.

## Triage

A single clear actionable fix → edit + reply + resolve hands-off. Anything
ambiguous or large → reply with a question or options and leave it open for
the human. When unsure, reply rather than silently resolving.
