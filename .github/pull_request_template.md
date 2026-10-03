<!--
Short on purpose. Dependabot and other bot PRs are exempt.
Every sentence that says fixed / verified / pinned / fails closed / never must
point at something that would FAIL if the sentence were false.
`plugins/kb-research/skills/kb-claims` lists the normative claims your diff adds.
-->

## What

<!-- One paragraph: the change and why. -->

## Claims → Evidence

<!-- One row per claim. Evidence = a test name, a canary (ci/canaries/NN-*.sh),
or a CI run/job. "Reviewed by eye" is not evidence. If nothing fails when the
claim is false, say so here and soften the claim. -->

| Claim | Evidence (test / canary / CI run) | Fails if the claim is false? |
|---|---|---|
|  |  |  |

## Twins checked

<!-- The other entry points of every operation you changed (PATCH vs batch vs
import; kb-server vs kb-code; CLI vs scheduler; daemon vs SPA). Name what you
looked at and what you did, or why a twin is left. -->

## Not verified

<!-- Explicit list, so absence of evidence is stated. For example: "no local
build — verified on CI only"; a path no test reaches; a platform not run. -->

## Security / lifecycle files

<!-- Only if the diff touches middleware.rs, router.rs, security/**, review_gate.rs,
github.rs, review_store/{cred,registry}.rs, a CASCADE_STEPS/SWEEP_TABLES entry, or
comment-privacy code: tick once the refute-first pass is done — you tried to make the
ORIGINAL failure happen through a twin entry point and recorded the attempt above. -->

- [ ] refute-first pass done (or: not applicable)
