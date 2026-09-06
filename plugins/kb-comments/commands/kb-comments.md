---
description: Triage open SPA comments on a kb artifact — list them, edit the source to address each, then reply + resolve. Pass an artifact path/name (or a folder).
argument-hint: <artifact-path-or-name>
---

Handle the open review comments on the artifact identified by `$ARGUMENTS`
(a filename, source-relative path, or folder). The `kb` CLI is the **only**
safe path — never edit `.review/<id>.json` directly; the daemon owns those
files (ETag-protected, and SPA panels listen over SSE).

Workflow:

1. **Resolve the artifact.** `kb find $ARGUMENTS --json` prints the 12-hex
   id, `path`, and `source_relative`.
2. **Read the open comments.** `kb comments list --path $ARGUMENTS --json`.
   Focus on `you`-authored (reviewer) comments that are still open.
3. **For each comment:** open the `source_relative` file, apply the edit the
   comment asks for (the file on disk is the source of truth — the watcher
   reindexes it automatically).
4. **Reply, then resolve:**

   ```bash
   kb comments reply <comment_id> --path $ARGUMENTS --body "What changed, briefly."
   kb comments resolve --path $ARGUMENTS <comment_id>     # or --all when done
   ```

5. If a comment's anchor went stale because your edit moved the text, re-point
   it explicitly with `kb comments reanchor` rather than guessing — the
   indexer's fuzzy resolver is detection-only.

Use `kb comments export --path $ARGUMENTS` to get the whole thread as one
block when you want full context before editing.

Triage rules of thumb: a typo / phrasing fix → edit + reply + resolve
hands-off; a substantive "reconsider X" → investigate first, reply with your
reasoning, and only resolve once the user agrees. When unsure whether a
comment wants a change or a discussion, reply rather than silently resolving.
