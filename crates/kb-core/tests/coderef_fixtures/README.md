# Code-ref golden fixtures (DCB W1.A)

**Every string in this directory is INVENTED.** kb is a public repo and the
corpus that motivated `kb_core::coderefs` is a client's work product, so these
fixtures copy the *shapes* of the real docs — element nesting, the mix of
citation forms per section, the exact families of near-miss token that the
grammar has to refuse — and none of the content. Same house rule as
`../session_fixtures/README.md` ("never paste real transcript content into the
repo. kb is public.") and the same authoring method as that directory's
`synthetic-*.jsonl` shape fixtures.

The real corpus is exercised OUTSIDE the repo, by the W1 gate run against the
operator's own daemon.

| fixture | shape it models |
|---|---|
| `workplan.html` | the multi-work-item engineering plan: `<h2>`/`<h3>` sections carrying GitHub issue links, per-item file lists, `<pre><code>` listings, a `<template id="kb-prompt">` + `<noscript>` + `<script>` + `<style>` decoy set, `data-kb-ref` declared refs (one well-formed, one typo'd), a `<meta name="kb-code-rev">`, and one long "conventions" list of tokens that LOOK like citations and must all be refused |
| `note.md` | the Markdown artifact: backtick code spans + a fenced block, proving one grammar covers HTML and rendered Markdown (`markdown::render_fragment` is applied first, exactly as the indexer does) |

Every planted decoy inside a skipped subtree (`template`/`noscript`/`script`/
`style`) contains the literal string `nope`, and no legitimate INFERRED ref in
either fixture does — that is what lets ONE positive assertion cover all four
skip tags at once (`skip_tag_subtrees_yield_zero_refs`, the invariant-#5 pin).
The one deliberate exception is `workplan.html`'s typo'd `data-kb-ref`
(`app/models/nope..rb`, the "declared but malformed must still be emitted"
case); the assertion therefore skips `declared` refs, which is sound because a
declared ref can only be minted from a `<code>` the walker actually reached.
