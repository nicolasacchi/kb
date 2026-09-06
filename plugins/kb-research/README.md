# kb-research

Author kb-indexed HTML research artifacts and keep their status claims true,
in one flow. One command, two skills, no hooks:

- **`/kb-tools`** (`commands/kb-tools.md`) — runs `kb tools`, which walks the
  installed binary's live clap command tree and prints the full `kb` CLI
  manifest (every verb + synopsis + example), then uses the relevant verbs
  to drive the user's request. Handy any time you're unsure of the exact
  flag syntax, since it always matches the installed `kb`.
- **`kb-artifact`** (`skills/kb-artifact/SKILL.md`) — the kb authoring
  contract: write a standalone deliverable (research report, design review,
  RFC, options write-up, deep dive) as one self-contained HTML file that
  satisfies kb's five must-knows (the `<template id="kb-prompt">`
  convention, a real heading hierarchy, stable section ids, no
  `<base href>`/`target="_top"`/`window.parent.*`, `kb-tags`/`kb-category`
  meta) plus an accessibility floor, then drop it into a kb corpus's source
  directory — the daemon's watcher indexes it automatically.
- **`kb-audit`** (`skills/kb-audit/SKILL.md`) — periodic truth-maintenance
  for a corpus of research artifacts: extracts each doc's status claims
  (`kb-tags status:*`, per-finding markers, prose claims citing a commit or
  file), verifies them against what the repo actually contains (`git log`,
  `rg`), and reports the drift. Dry-run by default; `--fix` flips stale
  markers in the source HTML with a dated note, never silently.

## Requires

- A running kb daemon (`kb daemon`) with at least one corpus configured, for
  authoring + indexing.
- `kb` on `PATH`.
- `git` and `rg` (ripgrep) on `PATH` — `kb-audit` shells out to both to
  verify a doc's claims against the repository; nothing else in this plugin
  needs them.
- Claude Code, or any harness that loads Claude Code-style skills/commands.
  Nothing here is hook-based, so there's no per-harness adapter to install.

## Install

```
/plugin marketplace add nicolasacchi/kb
/plugin install kb-research@kb-plugins
```

Then run `/kb-tools` to see the live CLI surface, or let Claude Code invoke
the `kb-artifact`/`kb-audit` skills automatically when a task matches
(authoring a research deliverable, or auditing a corpus's status claims).

## Configuration

None beyond the ordinary `kb` CLI daemon resolution — `KB_DAEMON_URL` to
point at a non-default daemon, and the daemon's own bearer token if it's
running non-loopback. `kb-audit` defaults to auditing `docs/research/`
relative to the repo you run it in; pass a different corpus directory as its
argument.
