---
name: kb-claims
description: Scan a PR diff (or branch) for the NORMATIVE claims it adds in comments, docs and test names (NEVER / ALWAYS / REFUSES / fails closed / pinned by / structurally / byte-identical / lossless / cannot race ...), resolve each to the test, canary or CI run that pins it, and print a claim table (pinned / pinned-by-canary / unpinned). Use before opening or merging a PR that adds invariants-style prose, when reviewing a "fix" whose commit message asserts a property, or when the PR template's Claims -> Evidence section needs filling honestly. Agent layer only; the refute-first pass for security/lifecycle files is a named step.
---

# /kb-claims — every normative sentence needs a pin

This repo's habit of dense, confident comments (NEVER, REFUSES, fails closed,
unit-pinned) is what makes it reviewable — and what lets a WRONG claim survive:
the adversarial review's process note says it plainly, *a comment asserting a
property is not evidence of it*. This skill turns that sentence into a table
you can read before merge.

**Posture — judgement stays in the agent layer.** The scan is `grep` over the
diff (`claims.sh`, deterministic, no LLM); deciding whether a named test
actually fails when the claim is false is YOUR reasoning step. Nothing here
posts to GitHub, and the kb daemon gains no scoring (no-in-daemon-LLM, #10/#26;
kb-code never writes GitHub).

## Step 1 — list the claims the diff adds

```bash
plugins/kb-research/skills/kb-claims/claims.sh            # origin/main..HEAD
plugins/kb-research/skills/kb-claims/claims.sh origin/main my-branch
```

Each row is `pinned` or `UNPINNED`, with `file:line`, the token that triggered it,
and the added line. `pinned` only means a resolvable name sits within three lines
(`pinned by \`x\``, an `invariant:N` marker, a `file.rs::test` citation, or a
backticked snake_case `fn` that exists) — it does NOT mean the pin is any good.

## Step 2 — resolve every row to evidence

For each claim, classify it into exactly one:

1. **pinned** — name the test, then READ it and answer: *if the claim were false,
   would this test fail?* A test that seeds the wrong input, uses a different id
   (the M7 case), or cannot carry the input (M11) is not a pin. Prefer to prove
   it: break the property in a scratch edit and watch the test fail (CI-only
   hosts: add a canary under `ci/canaries/` instead — see below).
2. **pinned-by-canary** — a `ci/canaries/NN-*.sh` breaks exactly this property
   and names the tests that must fail (`scripts/ci/run-canaries.sh`). Cite the
   canary file.
3. **unpinned** — nothing fails if the sentence is false. Either add the
   test/canary, or DELETE or soften the sentence. Never leave a confident
   comment that nothing checks.

Also run the mechanical gates that already resolve names:
`scripts/check-pinned-by.sh` (every `pinned by \`name\`` is a real fn) and
`scripts/check-doc-anchors.sh` (every `\`Symbol\` (\`path:N\`)` lands on its
symbol).

## Step 3 — the table

Print (and paste into the PR template's *Claims → Evidence* section):

| Claim (file:line) | Class | Evidence (test / canary / CI run) | Fails if claim is false? |
|---|---|---|---|

## Step 4 — refute-first, for security and lifecycle files

If the diff touches the security/lifecycle set — `crates/kb-server/src/middleware.rs`,
`router.rs`, `crates/kb-code-server/src/security/**`, `review_gate.rs`,
`github.rs`, `review_store/cred.rs`, `review_store/registry.rs`, anything that
registers an `artifact_id`-keyed table in `CASCADE_STEPS`/`SWEEP_TABLES`, comment
privacy (`ReviewFile::visible`) — run the adversarial pass BEFORE merge, as a
named step rather than an occasional event: for every "fixed" claim, try to make
the original failure still happen through a TWIN entry point (PATCH vs batch vs
import; kb-server vs kb-code; CLI vs scheduler). Two of the review's own "fixes"
were incomplete in exactly that way. Record what you tried in *Twins checked*.

## Bots

Dependabot PRs are exempt from the template and from this skill.
