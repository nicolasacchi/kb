---
name: kb-review-work
description: Work a kb-code PR review as the agent half of the loop — drain the human's questions/dispositions from a Review Room session, answer with code-verified evidence, land fixes/suggestions, and run the manual GitHub publish round from the export payload. Use when the operator says "work the review", "answer the review questions", after a `kb-code annotate watch` surface names review activity, or on a cadence over `kb-code review inbox`. The daemon stores; YOU author — every reply, verdict and gh call is agent-layer.
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

## The loop

1. **Pick the work.** Given a review id, use it. Otherwise:
   `kb-code review inbox --all-repos --json` — take the top row (the score is
   `unanswered×2 + unresolved`, already attention-ordered). On a cadence,
   first `kb-code review sweep --all-repos --json` (loopback) to refresh
   PR snapshots so staleness is real, then inbox.
2. **Situational awareness** (all `--json`): `review show ID` ·
   `review pr-status ID` (head drift? recommend `pr fetch` + `snapshot`
   before answering against stale code) · `review findings list ID`
   (dispositions + resolution confidence) · `review timeline ID` ·
   `review github-threads ID` (what was already said ON GitHub).
3. **Drain questions** — threads awaiting the agent:
   `kb-code review comments ID --json` (or live:
   `kb-code annotate watch --review ID --ignore-author claude`). For each
   open question whose last voice is the human: verify against the checkout
   (`kb-code hover/usages/framework/resolve-symbol/diagnostics …`), then
   `kb-code annotate reply <annotation-id> -m "<evidenced answer>"` and
   `annotate resolve` only when genuinely settled — an open question you
   answered but that awaits the human's read stays open.
4. **Honor dispositions.**
   - `agree` / `fix-later`→now: make the change — small mechanical fixes as
     suggestions on the finding's thread (`kb-code suggest <annotation-id>
     --from-file fix.txt`), then `suggest apply` (or `apply-batch` for the
     accepted set); larger fixes as ordinary edits in the checkout. Then
     `kb-code review snapshot ID` so carry-forward re-resolves everything.
   - `dispute`: re-verify from scratch. Refuted → reply conceding, recommend
     waive or drop it from the next import. Confirmed → reply with the
     evidence chain and leave the disposition to the human. Never flip a
     disposition yourself — it is the human's column.
   - `waive`: acknowledge in-thread only if there's something to add.
5. **Re-review when the PR moved** (`pr-status` says drift):
   `kb-code pr fetch N` → `review snapshot ID` → rerun the analysis →
   `review findings import ID --stdin` (mode `full` — dispositions and
   manual findings survive by construction; absent findings tombstone).
6. **Publish round (only when asked):**
   `kb-code review export-github ID --json` — heed `stale_export` (re-run
   step 5 first if set). Post via `gh` exactly what the payload contains
   (`gh api repos/{owner}/{repo}/pulls/{n}/comments -f body=… -f
   commit_id=… -f path=… -F line=… -f side=…` per comment;
   `gh pr review {n} --approve|--request-changes|--comment -b …` for the
   event). After each successful post: `kb-code review publish ID <slug>
   --url <html_url>` (and `--verdict` for the review event). Skipped
   orphans stay skipped unless the operator opts into file-level.
7. **Close out.** Summarize what changed (replies, fixes, snapshots,
   published) in your final message. If the review reached a natural end,
   `kb-code review distill ID --json` and — an explicit judgment call —
   keep what matters via `kb notes new` / `kb remember` citing the review
   id + head sha.

## Bounded honesty

Loopback-only verbs (`start-pr`, `snapshot`, `verdict`, `disposition`,
`findings import/add`, `suggest apply`, `publish`, `sweep`) need the daemon's
host. If a call 404s from a remote session, say so and stop that lane — never
route around the gate. If the repo isn't mounted in kb-code, findings orphan
honestly; the metadata lanes still work.
