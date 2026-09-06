# kb-comments

Triage SPA review comments (kb-comments/1) on kb artifacts from Claude Code.
Two slash commands, no hooks, no skills:

- **`/kb-comments <artifact>`** (`commands/kb-comments.md`) — the one-shot
  triage loop for a single artifact: list its open comments (`kb comments
  list`), edit the source file to address each, then reply and resolve
  (`kb comments reply` / `kb comments resolve`), reanchoring
  (`kb comments reanchor`) if an edit moved the anchored text.
- **`/kb-comments-watch <artifact-or-folder>`** (`commands/kb-comments-watch.md`)
  — the live loop: drains new comments over SSE (`kb comments watch --once`)
  and triages each as it lands. Re-run it (e.g. inside a `/loop`) to keep
  watching.

The `kb` CLI is the **only** safe path to a comment thread — never edit
`.review/<id>.json` directly. The daemon owns those files (ETag-protected)
and the SPA's comment panels listen for changes over SSE; a hand edit races
both. The full workflow, triage rules, and the realtime watch loop's
mechanics are documented in
[`docs/comment-workflow.md`](https://github.com/nicolasacchi/kb/blob/main/docs/comment-workflow.md)
in the main repo.

## Requires

- A running kb daemon (`kb daemon`) with the artifact's kb configured.
- `kb` on `PATH`.
- Claude Code — both commands are Claude Code slash commands (`$ARGUMENTS`
  is the Claude Code argument-substitution convention); this plugin has no
  hooks and registers nothing for other harnesses.

## Install

```
/plugin marketplace add nicolasacchi/kb
/plugin install kb-comments@kb-plugins
```

Then use `/kb-comments <artifact-path-or-name>` for a one-shot pass, or
`/kb-comments-watch <artifact-or-folder>` to keep watching for new comments.

## Configuration

None beyond the ordinary `kb` CLI daemon resolution — `KB_DAEMON_URL` to
point at a non-default daemon, and the daemon's own bearer token if it's
running non-loopback. See the main repo's
[configuration docs](https://github.com/nicolasacchi/kb/blob/main/docs/configuration.md).
