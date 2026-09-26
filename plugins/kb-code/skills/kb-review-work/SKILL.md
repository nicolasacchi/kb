---
name: kb-review-work
description: Work a kb-code PR review as the agent half of the loop — sync the PR into the review store, read the patchset (not the clone) with `diff`/`log`/`cat`, drain the human's questions/dispositions, answer with code-verified evidence, author + gate with `compose --slugify` + `verify`, and run the manual GitHub publish round from the export payload. Use when the operator says "work the review", "answer the review questions", "sync the PR" or "re-sync it", after a `kb-code annotate watch` surface names review activity, or on a cadence over `kb-code review inbox`. `review status` is the drift question; `review sync` is the one call that re-syncs. The daemon stores; YOU author — every reply, verdict and gh call is agent-layer.
---

# /kb-review-work — drain the Room's queue

A PR review in kb-code is a durable dialogue: the reviewer agent imported
findings (`kbc-findings/1`), the human read them in the Review Room and left
dispositions, questions, and their own findings. This skill is the AGENT half:
read what awaits, answer with evidence, change code where agreed, and (only
when asked) publish to GitHub via `gh` from the export payload. kb-code never
calls GitHub's write API — you do, deliberately, at the end.

**Contract:**
- Answer with EVIDENCE, not opinion — every reply that touches code cites
  what you verified via the nav verbs (`hover`, `usages`, `framework`,
  `diagnostics`, `callers`), same discipline as the review that started this.
- A dispute you cannot refute with evidence is a finding you retract (reply
  conceding + recommend `waive`/re-import without it) — never argue past
  the code.
- Working-tree mutations only through the sanctioned paths: your own edits in
  the checkout, `kb-code suggest apply`/`apply-batch`. Never force anything
  on a dirty tree.
- GitHub publishing happens ONLY when the operator asked for the publish
  round, ONLY from `export-github`'s payload (its line/side/commit_id are
  ladder-verified), and every posted item is recorded back via
  `kb-code review publish` so nothing double-posts.

**Addressing.** The resolve-grammar verbs — `find`, `diff`, `log`, `cat`,
`verify`, `sync`, `status`, `retrack` — take `<id>`, `pr:<N>` or `<id>/ps<n>`,
inferring the repo from the store when `pr:<N>` is unique (`--repo` to
disambiguate). The older verbs (`show`, `comments`, `findings list`,
`timeline`, `github-threads`, `export-github`, `publish`, `distill`) take a
numeric `<id>`; get it once from `find`/`status` and reuse it.

## The loop

1. **Pick the work.** Don't demand the review id up front.
   `kb-code review find --pr N --json` returns every review bound to PR N
   (across every configured repo unless `--repo` narrows it). Then
   `kb-code review status pr:N --json` to decide whether anything actually
   moved before you spend anything. For the whole queue:
   `kb-code review inbox --all-repos --json` — take the top row (already
   attention-ordered).
2. **Situational awareness** (all `--json`): `review status <id>` is THE drift
   answer — `head_moved` against the LATEST patchset tip, base state, the
   file-count drift vs. the forge, verdict staleness, open findings. Also
   `review show <id>` · `review findings list <id>` (dispositions + resolution
   confidence) · `review timeline <id>` · `review github-threads <id>` (what
   was already said ON GitHub). To look at a change set use
   `review diff <ref> --stat` / `--path P` / `--budget N` — computed in the
   store against the patchset's own base, and it works with no `refs/kbc/*` in
   the clone. There is no `git -C` path to the PR any more; the store owns
   those refs.
3. **Read the patchset, not the clone.** `review log <ref>` for the patchset's
   commits, and `review cat <ref> <path> --side old|new` for one file at its
   base (`old`) or tip (`new`, the default). Secret-denylisted paths are
   refused here, as everywhere.
4. **Drain questions** — threads awaiting the agent:
   `kb-code review comments <id> --json` (or live:
   `kb-code annotate watch --review <id> --ignore-author claude`). For each
   open question whose last voice is the human: verify against the checkout
   (`kb-code hover/usages/framework/resolve-symbol/diagnostics …`), then
   `kb-code annotate reply <annotation-id> -m "<evidenced answer>"` and
   `annotate resolve` only when genuinely settled — an open question you
   answered but that awaits the human's read stays open.
5. **Honor dispositions.**
   - `agree` / `fix-later`→now: make the change — small mechanical fixes as
     suggestions on the finding's thread (`kb-code suggest <annotation-id>
     --from-file fix.txt`), then `suggest apply` (or `apply-batch` for the
     accepted set); larger fixes as ordinary edits in the checkout.
   - `dispute`: re-verify from scratch. Refuted → reply conceding, recommend
     waive or drop it from the next import. Confirmed → reply with the
     evidence chain and leave the disposition to the human. Never flip a
     disposition yourself — it is the human's column.
   - `waive`: acknowledge in-thread only if there's something to add.
6. **Re-review when the PR moved** (`review status` says `head_moved`): ONE
   call replaces the old `pr fetch` + `snapshot` pair —
   `kb-code review sync --repo <REPO> --pr N --json`. It creates the review
   if missing, fetches base + head into the store, and snapshots only when
   `(tip, merge-base)` changed. Branch on `reason` — one of `created`,
   `head-moved`, `base-moved`, `retargeted`, `unchanged`, `merged-final` —
   together with `minted`. Use `--wait[=SECS]`: bare `--wait` = 600 s,
   `--wait=0` returns the daemon job id at once (re-running the same sync
   attaches to the running job). A partly-failed run exits 7 (partial). For a
   pinned or retargeted base, move it with `kb-code review retrack <id> --base
   main`; `--base` on `sync` itself applies ONLY when sync CREATES the review,
   never to an existing one. (A closed review whose PR reopened is never
   reopened unattended — sync reports `review-closed-pr-open`; re-run with
   `--reopen`.) Then rerun the analysis and `review findings import <id>
   --stdin` (mode `full` — dispositions and manual findings survive by
   construction; absent findings tombstone).
7. **Author and gate.** `kb-code review compose <id> --doc review.md
   --findings findings.json --slugify` writes the document and the findings in
   one transaction. `--slugify` gives every finding without a valid slug the
   ASCII `f-<kebab>` slug derived from its title (uniquified within the batch,
   deterministic) instead of letting the WHOLE batch 400 on `invalid_slug`, and
   it is UTF-8-safe — every non-ASCII character (a title with `à`) is treated
   as a separator and never sliced, so it cannot panic. Author-written VALID
   slugs are never touched. Then the MANDATORY post-compose gate:
   `kb-code review verify <id> --json` — it checks the document is present and
   lints clean, counts findings (≥ `--min-findings` if given), that every
   finding anchor resolves, and that the verdict sits on the latest patchset;
   it EXITS 3 on failure. A failed verify is not a formality to note and move
   past — it is the reason the review is not done.
8. **Publish round (only when asked):**
   `kb-code review export-github <id> --json` — heed `stale_export` (run
   `review sync` first if it is set). Post via `gh` exactly what the payload
   contains (`gh api repos/{owner}/{repo}/pulls/{n}/comments -f body=… -f
   commit_id=… -f path=… -F line=… -f side=…` per comment;
   `gh pr review {n} --approve|--request-changes|--comment -b …` for the
   event). After each successful post: `kb-code review publish <id> <slug>
   --url <html_url>` (and `--verdict` for the review event). Skipped
   orphans stay skipped unless the operator opts into file-level.
9. **Close out.** Summarize what changed (replies, fixes, syncs, published) in
   your final message. If the review reached a natural end,
   `kb-code review distill <id> --json` and — an explicit judgment call —
   keep what matters via `kb notes new` / `kb remember` citing the review
   id + head sha.

## The morning loop

`kb-code review sync --repo <REPO> --open [--merged-since DATE] --json`
replaces a hand-orchestrated sweep. It runs the single sync for every open PR
(and, with `--merged-since`, the PRs merged since that date) SEQUENTIALLY, each
under the per-repo sync lock, so a caller no longer needs a "the lead owns
every fetch" rule. The per-shape `--wait` default is 600 s for one PR and 3600 s
for `--open`. When some PRs succeeded and some failed, the run is PARTIAL: it
exits 7 with `degraded: true`, so a partial morning is visible rather than
silent.

## Bounded honesty

Loopback-only verbs (`start-pr`, `snapshot`, `verdict`, `disposition`,
`findings import/add`, `suggest apply`, `publish`, `sweep`, and now `sync`,
`status --fetch`, `retrack`, plus `store sync`, `store gc`, `store set-base-url`,
`store credentials --test`, `store legacy-refs`, `store export-legacy`,
`store maintain`) need the daemon's host. If a call 404s from a remote
session, say so and stop that lane — never route around the gate. If the repo
isn't mounted in kb-code, findings orphan honestly; the metadata lanes still
work.

**Exit codes** (the shipped table in `crates/kb-code-cli/src/envelope.rs`; the
design's §13 table — `3 = not found`, `4 = conflict` — was NOT adopted, and
`review_agent.rs` says so in a test comment):

| code | meaning |
|---|---|
| 0 | ok |
| 1 | generic failure |
| 2 | usage (bad flag / bad `<id>`·`pr:<N>` address) |
| 3 | conflict — HTTP 409 **and** 503, plus a failed `verify` and a store still seeding |
| 4 | refused (401/403 only) |
| 5 | unreachable (daemon down) |
| 6 | upstream (a forge fetch/API call failed) |
| 7 | partial (`degraded: true`) |
| 8 | not found |

Branch on the difference between 3 and 8 rather than guessing. A 404 is
structurally ambiguous — loopback-only routes deliberately 404 a non-loopback
caller — so 8 is used only where the body is an unambiguous not-found.
