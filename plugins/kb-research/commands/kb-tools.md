---
description: Print the full kb CLI surface (every verb + synopsis + example) so you can drive the kb daemon without reading docs.
---

Run the kb CLI manifest, then use it to drive the user's request:

```bash
kb tools
```

`kb tools` walks the live clap command tree, so the manifest always matches
the installed binary. Read the output, then use the relevant `kb` verbs to
author, index, search, and explore artifacts — for example:

- `kb add <file> --kb <name>` — index a new artifact (or rely on the
  watcher, which reindexes on file change).
- `kb find <name>` / `kb search "<query>" --kb <name>` — locate artifacts.
- `kb related <id>` — neighbours in the embedding space.
- `kb comments list` — open review comments (see the `kb-comments` plugin).
- `kb why <file>` — why is a file the way it is? The past sessions that
  touched it + the prompt/decisions/commits that produced it (episodic
  memory — what actually happened, distinct from `kb recall`'s curated facts).
- `kb recollect "<task>"` — has something like this been done before? Semantic
  search over past sessions; each hit surfaces its recency/staleness, errors,
  and commits. Pull these BEFORE re-deriving or re-doing work.

If the user asked you to author a research artifact, pair this with the
`kb-artifact` skill (the authoring contract).

$ARGUMENTS
