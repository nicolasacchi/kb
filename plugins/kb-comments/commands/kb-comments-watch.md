---
description: Watch a kb artifact (or folder) for NEW reviewer comments over SSE and triage each as it lands. Pass an artifact path/name or folder.
argument-hint: <artifact-path-or-folder>
---

Run the live comment-watch loop for `$ARGUMENTS` and triage each new
reviewer comment as it arrives. `kb comments watch` is an SSE-driven monitor
for new `you`-authored comments — it reconnects with `Last-Event-ID` and
exits after one batch with `--once`.

```bash
kb comments watch --path $ARGUMENTS --json --once --timeout 1200
```

For each comment surfaced:

1. Read its `source_relative` file.
2. Apply the requested edit (the watcher reindexes the file automatically).
3. `kb comments reply <comment_id> --path <source_relative> --body "…"`
   describing what changed, then `kb comments resolve` it.
4. If your edit moved the anchored text, `kb comments reanchor` to re-point it.

To keep watching, re-run this command (e.g. inside a `/loop`): each
invocation drains one batch, triages it, then waits for the next. Stay
hands-off for typo/phrasing fixes; for substantive "reconsider X" comments,
investigate and reply with your reasoning before resolving.

See the `kb-comments` plugin's `/kb-comments` command for the one-shot
(non-watching) triage of an artifact's existing open comments.
