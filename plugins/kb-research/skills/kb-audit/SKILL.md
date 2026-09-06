---
name: kb-audit
description: Periodic truth-maintenance for kb research artifacts — extract each doc's status claims (kb-tags status:*, per-finding markers, shipped/open/planned prose with cited commits/files), verify them against repo truth (git log --grep/-S, rg code presence), and emit a drift report. Use when a corpus of research/review docs has aged past the code it describes and their status tags may be stale, when the user asks to audit/reconcile research docs against the repo, or before a milestone to catch docs that still say "open" for shipped work. --fix flips stale markers in the SOURCE html with a dated note; default is a dry-run report.
---

# /kb-audit — reconcile research artifacts against repo truth

Research artifacts drift: a review filed 30 findings `status:open`, ten shipped,
and the doc still says open. This skill walks a corpus of HTML artifacts, pulls
each **status claim**, checks it against what the **repo actually contains**, and
reports the drift. With `--fix` it flips the stale markers *in the source HTML*
with a dated note — never silently, never guessing.

**Posture — all judgment happens here, in the agent layer.** Extraction is
mechanical (grep/parse), but deciding whether a claim is *satisfied* is a
reasoning act: you read the claim, you read the git log and the code, you rule.
The kb daemon stays deterministic and LLM-free — this skill **never** adds
daemon-side scoring, a "staleness score" column, or any hosted judgment (the
no-in-daemon-LLM non-goal, invariant #26 / #10). The `kb` CLI and `git`/`rg`
are the only tools; the ruling is yours.

**Contract:**

- **Dry-run by default.** No argument and no `--fix` ⇒ report only, zero writes.
- **Evidence or no verdict.** Every DRIFTED / CONFIRMED row cites concrete repo
  evidence: a commit sha, a file path, an `rg` hit with line, or an explicit
  "no match for `<pattern>`". A claim you cannot check is `UNVERIFIABLE`, not
  drifted — say why.
- **`--fix` edits the SOURCE, minimally.** Flip (or, if none exists, **add**)
  the doc-level status token, and mark each resolved finding with a pinned
  inline `<span>` + a dated note — see "Marker vocabulary" in Step 5. **No HTML
  comments**: an earlier revision of this skill required a
  `<!-- kb-audit … -->` comment; that's dropped — comments aren't indexed by
  the daemon and drift invisibly, so the dated marker text itself is the only
  note. **Never** touch `<template id="kb-prompt">` (#5), `<script>`, or
  `<style>`; **never** rewrite a heading `id=`/`data-kb-id=` (anchors + comments
  key on them, #6). Prose is left verbatim except the token/marker being added.
- **The doc is a claim, the repo is truth.** When they disagree, the repo wins —
  but a claim can be *ahead* of `main` (shipped on a branch/worktree, deployed
  but uncommitted). Record that as `AHEAD`, don't "correct" it to open.

## Arguments

- `[<corpus-dir>]` — directory of HTML artifacts to audit. Default
  `docs/research/` (relative to the repo root you're in). May also be a kb
  corpus source dir.
- `--since <7d | YYYY-MM-DD>` — only audit artifacts whose `mtime` (or `git log`
  last-touch) is on/after the cutoff. Skips docs already reconciled recently.
- `--docs <glob|name,name>` — restrict to named artifacts (comma list of
  basenames, or a glob like `kb-*review*`). Overrides `--since` selection.
- `--fix` — after the report, apply the stale-marker flips to the source HTML.
  Requires the report to have run in the same invocation (never blind-fix).
- No arguments → `docs/research/` dry-run, every artifact.

## Step 1 — select the artifacts

```bash
ls <corpus-dir>/*.html                 # default docs/research/
git -C <repo-root> log -1 --format=%cI -- <file>   # last-touch for --since
```

Apply `--since` (drop older) then `--docs` (restrict to named/glob). Print the
selected set as a numbered list before proceeding — the operator should see the
scope. A corpus with a house `_template/` dir or non-artifact HTML: skip files
with no `<meta name="kb-tags">` (they're not kb artifacts).

**Pre-broken targets.** A doc that fails an unrelated structural validator (a
pre-existing missing heading `id=`, a broken link, anything the corpus's own
validator flags that has nothing to do with status claims) is still in scope —
**proceed with the audit anyway**. Do not fix the unrelated error and do not
let it block the status pass; Step 5 covers what to do with it (flag, don't
fix).

## Step 2 — extract status claims (three lenses, mechanical)

For each artifact, pull every claim into a working list. Three lenses:

1. **Doc-level status tag** — the `status:*` token in the `kb-tags` meta and any
   mirrored `<span class="tag">status:*</span>`:
   ```bash
   grep -oE 'status:[a-z]+' <file> | sort -u
   ```
   Known tokens: `open` · `applied` · `shipped` · `partial` · `planned` ·
   `deferred` · `reference` · `research` · `wip` · `done`. The doc-level tag is
   the coarse claim ("this whole doc is unactioned").
2. **Per-finding / per-item markers** — inline status on a finding or work-item
   (e.g. `<span class="badge">status:open</span>`, a `✅`/`✓`/`SHIPPED`/`DONE`
   marker, a `data-status=` attr). These are the fine-grained claims that drift
   independently of the doc tag.
3. **Prose claims with citations** — sentences asserting an outcome that name a
   commit sha, file path, or verb: "shipped in `db7ce381`", "landed on main",
   "live on kb.example.com", "status:open — not yet actioned",
   "`crates/kb-core/src/foo.rs` now does X". Capture the claim **and** its cited
   evidence (sha / path / symbol) — that citation is what Step 3 checks.

**Missing status token.** If lens 1 finds no `status:*` token at all (some
docs, especially ones authored before this convention existed, carry none),
that is **not** `NO-CLAIMS` on its own — lenses 2 and 3 still apply. If the
audit resolves a checkable claim, Step 5 **adds** a token rather than flipping
one (the pilot's `evolving-the-kb-memory-system.html` had no token and gained
`status:partial`). Only report `NO-CLAIMS` when *none* of the three lenses
find anything to check.

Record each as `{artifact, lens, claim-text, claimed-status, cited-evidence}`.
A doc with no extractable claim is reported `NO-CLAIMS` and skipped.

## Step 3 — verify each claim against repo truth

For every claim, run the check its citation implies. The repo is truth; you are
ruling whether the doc's claim still holds.

- **Cited a commit sha** — does it exist and say what the doc says?
  ```bash
  git -C <repo-root> show -s --format='%h %cI %s' <sha>
  ```
  Missing sha (rebased away / never landed) ⇒ the "shipped" claim is suspect.
- **Cited a feature / verb / symbol** — is it in the tree now?
  ```bash
  git -C <repo-root> log --oneline --grep='<phrase>' -i | head
  git -C <repo-root> log -S '<code-token>' --oneline | head      # pickaxe: added/removed
  rg -n '<symbol-or-string>' <repo-root>/crates <repo-root>/web  # present in code today
  ```
  A `status:open` finding whose fix symbol is now in the tree (an `rg` hit at the
  cited path) is **DRIFTED** → should be `shipped`. A `shipped` claim with no
  `rg` hit and no commit is **DRIFTED** → suspect / reverted.

  **Phase-id collision caution.** Short phase letters (`R1`, `Z1`, `F2`...) get
  reused across unrelated milestone tracks in this repo — `git log --grep`
  keyed on a bare phase id can silently match the wrong track. The pilot hit
  this for real: `kb-next-prompts-playbook-2026-07.html`'s own `R1`/`R4` phase
  ids collided with the unrelated v0.24 "Track R" phase ids. Always confirm a
  match via the commit **body** (does it describe the same finding?) and the
  **files touched** — never flip on the phase-id string alone.

  **Renamed-feature caution.** A design can ship under a different name than
  the doc used at authoring time — the `kb-reflect` design shipped as
  `/kb-distill`. Search by the described *capability*, not the literal verb:
  `rg` for the behavior, and check `plugins/*/skills/` and `plugins/*/commands/`
  as well as CLI verbs under `crates/kb-cli` — a shipped feature is as likely to
  be an agent-layer skill as a daemon/CLI change.
- **Cited a deploy / live URL** — you generally can't verify prod from here; mark
  `UNVERIFIABLE (deploy)` unless the user says otherwise. Never assume live.
- **No citation at all** — `UNVERIFIABLE`; note that the claim carries no anchor.

Assign one verdict per claim:

| verdict | meaning |
|---|---|
| `CONFIRMED` | doc's claim matches the repo (open is still open, shipped is in the tree) |
| `DRIFTED` | repo contradicts the claim (open→already shipped, or shipped→absent) |
| `RULED` | claim was open, but the repo shows the question was formally decided — a non-goals/README ruling, an explicit accept/reject — rather than code landing. A decision recorded is a resolution too; don't downgrade it to `UNVERIFIABLE` for lack of an `rg` hit — the citation is the ruling artifact itself |
| `AHEAD` | claim is real but ahead of `main` (branch/worktree/deploy, uncommitted) — name the branch |
| `UNVERIFIABLE` | no checkable citation, or a deploy/runtime claim |

## Step 4 — the drift report (always)

Print one table, most-actionable first (`DRIFTED` at top), grouped by artifact:

| artifact | lens | claim (short) | doc says | repo says | evidence | verdict |
|---|---|---|---|---|---|---|

`evidence` is the concrete anchor: `commit db7ce381` · `rg hit crates/…/foo.rs:812`
· `no match for 'RerankerClient'` · `mtime 2026-07-06`. Prefer a sha or a
`path:line` — the operator must be able to re-run your check. Follow the table
with a one-line-per-artifact rollup (`N claims: x confirmed, y drifted, z ruled,
a ahead, w unverifiable`) and the count of source edits `--fix` *would* make.

If checking a claim surfaces a pre-existing, unrelated validation error (Step
1's "pre-broken targets"), list it under its own **"pre-existing (out of
scope)"** line per artifact — visible in the report, never folded into the
drift counts above, and never auto-fixed.

**Dry-run stops here.** Without `--fix`, this report is the whole deliverable.

## Step 5 — apply fixes (only with `--fix`)

### Marker vocabulary

Every verdict resolves to exactly one of five outcomes. **Two CSS classes
carry all of them — pin these, don't invent new ones:** `shipped-note` and
`partial-note` (both first used in the pilot, commit `3ba9035d`, following the
GC-A9 convention in `docs/authoring-artifacts.md`). Both render as a small
inline `<span>` placed right after the finding's heading/prose — **never**
inside a heading `id=`-bearing tag:

| outcome | when | in-file marker | doc-level `kb-tags` |
|---|---|---|---|
| **shipped** | `DRIFTED`, code confirmed on `main` | `<span class="shipped-note">shipped <hash(es)> <date></span>` | flip to `status:shipped` only once *every* finding in the doc is resolved |
| **partial** | some, not all, findings resolved | `shipped-note` on each resolved finding; `partial-note` on one that's partly done (e.g. infra shipped, rollout still pending) | flip (or **add**, per Step 2) `status:partial` + one dated reconciliation line (in `kb-summary` or a lede `<p>`) summarizing what shipped |
| **ruled** | `RULED` — a decision was recorded, not code | `<span class="shipped-note">ruled <date> — <where the ruling lives, e.g. "README Non-goals", plus a commit hash if one exists></span>` | counts toward `status:partial`/`status:shipped` exactly like a code-shipped finding — a ruling is a resolution, not an open item |
| **ahead** | `AHEAD` — real, but on an unmerged branch/worktree | `<span class="partial-note">ahead — branch NAME, not yet on main</span>` (substitute the real branch name) — always name it | not flipped on an `AHEAD` claim alone |
| **open (confirmed)** | audit re-ran, nothing new landed | none on unchanged findings (stay silent) — add ONE dated confirmed-open note at the **doc level only** ("confirmed open 2026-07-10 — no evidence of drift") so a null result is still visible | leave `status:open` as-is |

For each claim where the correct new status is unambiguous, edit the
**source HTML**:

1. **Missing token → add, don't hunt for one to flip.** Per Step 2: if the doc
   has no `status:*` token and the verdicts justify one, add it fresh. Report
   it as `added` in Step 6, not `flipped`.
2. **Apply the token + the marker from the table above.** Exact string
   replacement on the token/marker only — surrounding prose, other tags, and
   whitespace stay byte-identical.
3. **No `<!-- kb-audit … -->` HTML comments.** An earlier revision of this
   skill required one; that's dropped now — comments aren't indexed by the
   daemon and invite silent drift (this contradicted the GC-A9 template, which
   used inline spans + prose only, per `73c598f7`). The marker text itself
   *is* the dated note — nothing else to append. Absolute date always (never
   "today").
4. **Never touch** `<template id="kb-prompt">`, `<script>`, `<style>`, or any
   heading `id=`/`data-kb-id=` (#5/#6). If a status token lives *inside* a
   prompt template, leave it and report it as fix-skipped.
5. **Ambiguous, `AHEAD`, or `UNVERIFIABLE`** claims are **not** auto-fixed (an
   `ahead` marker, if added, is descriptive — it never flips the doc-level
   tag) — list them under "needs human ruling" with what you'd change and why
   you held off.
6. **Pre-existing, unrelated breakage stays unfixed.** A validation error that
   predates this audit and has nothing to do with the status claim you're
   verifying (Step 1) is **not yours to fix here** — leave it, and flag it (a)
   in the Step 4 drift report's "pre-existing (out of scope)" line and (b) in
   the `--fix` commit body. Don't silently paper over it, and don't let it
   grow the audit into an unplanned cleanup.

Do the flips with a precise string edit (a targeted `sed`/edit on the exact
token line, or the file-edit tool), then re-grep to confirm the token changed and
nothing else did:

```bash
grep -n 'status:' <file>          # the flipped/added token(s) only
git -C <repo-root> diff --stat -- <file>   # bounded diff, no heading-id churn
```

The daemon's watcher reindexes the edited source automatically; no manual
`kb reindex` needed unless the corpus dir isn't watched.

## Step 6 — verify + report

After `--fix`, confirm the edits landed and the artifacts still parse:

```bash
git -C <repo-root> diff --stat -- <corpus-dir>       # only status tokens + notes changed
kb find <basename>                                    # still indexed (id unchanged)
```

Report: claims audited, confirmed / drifted / ruled / ahead / unverifiable
counts, source edits applied (flagging which were `added` vs `flipped` tokens,
per Step 5), fixes skipped (with the reason — prompt-template, ambiguous,
needs-ruling), and any pre-existing out-of-scope issues flagged but left
unfixed. State explicitly that a re-run now reports the flipped/added claims
as `CONFIRMED`.

## Completed pilot (2026-07-10) — and how to run the next audit

The pilot ran on **2026-07-10** (commit `3ba9035d`), auditing
`fresh-eyes-next-iteration-2026-07.html`, `kb-next-prompts-playbook-2026-07.html`,
and `evolving-the-kb-memory-system.html` — the three docs known-drifted at
authoring time. (`kb-code-craft-review-2026-07.html` was **not** part of the
automated pilot: it was the hand-flipped GC-A9 **template**, commit `73c598f7`,
that established the marker convention this skill now follows — treat it as a
worked reference, not a pilot target.)

Outcome: all three landed on `status:partial` (two flipped from `status:open`;
`evolving-the-kb-memory-system.html` had **no status token at all** and got one
**added**, per the missing-status-token handling above). The pilot is what
surfaced every lesson folded into this revision: the phase-id collision
(playbook `R1`/`R4` vs the unrelated v0.24 "Track R"), the renamed-feature miss
(`kb-reflect` design → shipped as `/kb-distill`), the `RULED` outcome (F6
second-operator ruling, F2 MCP-deferral re-ruling), an `AHEAD` case (F5,
`feat/resurface-thin-slice`, not on `main`), and a pre-existing structural
error in `evolving-the-kb-memory-system.html` (22 `<h2>`/`<h3>` without `id=`)
that was flagged, not fixed. Full per-doc reasoning is in the commit body of
`3ba9035d`.

**Running the next audit:** there's no longer a fixed known-drift target list —
that was this pilot's starting condition, and it's now resolved. Pick the
corpus the normal way (Step 1): `--since <date-of-last-audit>` (use
`3ba9035d`'s date, 2026-07-10, as the floor) over `docs/research/`, or `--docs`
for a specific artifact a reviewer flagged as possibly stale. Apply Steps 2–6
and the Step 5 marker vocabulary as documented above — there's no separate
pilot procedure anymore; this **is** the procedure. Always dry-run first,
review the drift table with the operator, then `--fix` only the unambiguous
rows; the output is itself worth a short memory (`/kb-distill`).
