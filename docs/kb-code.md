# kb-code — the sibling code-reading daemon

`kb-code` is a separate daemon built in this workspace
(`crates/kb-code-server` + `crates/kb-code-cli` + the `web-code/` SPA) and
shipped from the same tag: its own binaries, its own port (4747), its own
CI recipes, never a `kb` subcommand. It reads a git checkout the way kb
reads a corpus — provenance, review, search, annotations — and it never
writes code (Claude Code stays the write path).

Install it from the `kb-code-<ver>-<triple>.tar.gz` release asset or the
`ghcr.io/nicolasacchi/kb-code` image; configure it with its own
`kb-code.toml` ([configuration.md](configuration.md)). The optional
`kb-lip` LSP adapter lives in `crates/kb-lip` with reference provider
configs under [`providers/`](../providers/README.md).

Related: [CLI reference](cli.md) · [HTTP API](http-api.md) ·
[project overview](../README.md).

It has its own CI recipes (`just ci-code` / `ci-code-spa` / `ci-code-e2e`,
kept out of the `just ci` aggregate) and its own dependency surface (gix,
tree-sitter + grammars, grep-searcher/grep-regex, nucleo). The base surface:
a live-mirrored index with tiered language extraction; Search Everywhere —
files/symbols/text/semantic/transcripts lanes behind one box; provenance —
streamed incremental blame, the session↔commit join ladder, and
`why`/`story`/`provenance-report`; a reader SPA with blame gutter,
session-diff, annotations and confirmed checkout; and the agent verbs
`map`/`pack`/`defs`/`xrefs`/`similar`/`impact` plus the why-hook plugin.
Auth splits two ways: ordinary routes ride kb-server's imported
`auth_bearer`; anything carrying transcript-derived text or mutating the
working tree (`search/transcripts`, `session-diff`, `checkout`) is
loopback-only. The full design ("The read-first IDE — session-aware code
reading for humans and agents") and its implementation plan live as
artifacts in the `research` kb corpus (outside this repo) rather than under
`docs/research/` here.

**`kb-code review distill <ID> [--json]`** (CT-E7, `GET
/api/reviews/{id}/distill`, `review-distill/1`) composes one completed
review's full local record — meta, every patchset, files touched at the
latest patchset, the verdict (with `verdict_ps` + staleness), every comment
thread (open and resolved, carry-forward re-resolved against the latest
patchset), and every stored suggestion (with its applied-audit trail) —
into a single deterministic JSON document. It is a pure read: a review is
local-only state that evaporates at GC leaving just commits, and
re-distilling the same review at the same patchset state yields a
byte-identical document. kb-code never pushes this into kb itself — that
hand-off is an AGENT-layer judgment call: an agent that decides a review is
worth keeping runs `kb-code review distill <id> --json` and authors the kb
artifact itself (`kb notes new` / `kb remember`), citing the review's id and
head sha in the note/memory body. There is no auto-push and no new
daemon-to-daemon write path — the doc↔code bridge's ONE call direction
(kb-code→kb) stays exactly as narrow as before.

## The PR Room

PRR ("The PR Room," kb v0.39 track T2) binds kb-code's local review sessions
(V3.R1) to real GitHub pull requests and gives them a durable findings
ledger, calibration analytics, and a computed GitHub-shaped export — without
kb-code ever calling GitHub's write API itself. Every GitHub mutation (a
review comment, a PR review, a check) is posted by an AGENT's own `gh` call,
never by this daemon; kb-code only computes payloads and, after the fact,
records what was published (`review publish`). The full loop — drain
dispositions, answer with nav-verb evidence, apply fixes, run the publish
round — is `plugins/kb-code/skills/kb-review-work` (`/kb-review-work`).

**PR-bound reviews.** `kb-code review start-pr --repo R --pr N [--base]
[--title] [--session]` (`POST /api/reviews/pr`, LOOPBACK-ONLY) fetches
`refs/pull/N/head` into `refs/kbc/pr/N` (400 on failure — this is
load-bearing), creates the review, captures ps1, and best-effort-enriches
with GitHub PR metadata; the review exists either way, even when the GitHub
enrichment itself fails. `kb-code review pr-status ID` (`GET
/api/reviews/{id}/pr-status`) answers two halves: LOCAL (snapshot vs. local
patchset tip) always answers; LIVE (a fresh GitHub fetch + `commits_behind`)
degrades to `unavailable_reason` on any GitHub-side failure. `kb-code review
sweep {--repo R | --all-repos} [--include-closed]` (`POST
/api/reviews/sweep`, LOOPBACK-ONLY) walks every PR-bound review (default
`state=open`) and reconciles each against live GitHub — the cron/agent
entry point for "every PR the LLM touched."

**Findings ledger (`kbc-findings/1`).** `kb-code review findings import ID
{--from-file FILE|--stdin} [--mode full|additive]` (`POST
/api/reviews/{id}/findings/import`, LOOPBACK-ONLY) batch-imports a
generator agent's findings, origin-gated so a batch slug collision can never
silently overwrite a human-authored ("manual") finding. `kb-code review
findings list ID [--ps N|latest] [--disposition D] [--all]` (`GET
/api/reviews/{id}/findings`, bearer) reads them back — each finding carries
a `severity` (`blocker`|`concern`|`ok`), a free-text `category`, and a
position resolved via the SAME `resolve_for_ps_with_content` ladder
`/comments`/`/distill` use, so a finding can never disagree with what a
human sees in the browser. `kb-code review findings add ID --severity S
--category C --path P {--line N|--lines A-B|--whole-file} -m TITLE
--rationale R` (`POST /api/reviews/{id}/findings`, LOOPBACK-ONLY, addendum
§E) lets a human author one finding directly. `kb-code review disposition ID
SLUG {agree|dispute|waive|fix-later|clear} [-m NOTE]` (`PUT`/`DELETE
/api/reviews/{id}/findings/{slug}/disposition`, LOOPBACK-ONLY) records the
human's verdict on each finding. `GET /api/reviews/{id}/findings/recurrence`
(bearer, route-only — no dedicated CLI verb yet) surfaces which of a
review's own findings recur across the repo's other reviews, off the same
`recurrence_pairs` query `review analytics` uses.

**Report + artifact.** `kb-code review report ID` (`GET
/api/reviews/{id}/report`, bearer) reads the agent-authored review report;
`kb-code review report ID --set --from-file FILE` (`PUT
/api/reviews/{id}/report`, LOOPBACK-ONLY) wholesale-replaces it
(`generated_at` server-stamped). `kb-code review artifact ID` (`GET
/api/reviews/{id}/artifact`, bearer) is the ONE new kb-code→kb call this
unit adds (`join::kb_client::KbClient::doc_meta`) — a live, UNPERSISTED
verification of the review's kb artifact hint (set via kb's own `PATCH
/api/reviews/{id}` step, not this CLI); invariant #2's kb-code→kb-only call
direction stays unchanged.

**Inbox, timeline, analytics, impact.** `kb-code review inbox {--repo
R|--all-repos} [--state open|closed|all] [--limit N]` (`GET
/api/reviews/inbox`, bearer) is a cross-repo attention queue, `score =
unanswered_questions*2 + unresolved_findings` — deterministic and
decomposed into named terms, never a daemon-authored quality verdict.
`kb-code review timeline ID` (`GET /api/reviews/{id}/timeline`, bearer) is a
pure composition of existing rows (`review_created`/`pr_bound`/`patchset`/
`findings_import`/`finding_added`/`disposition`/`verdict`/
`finding_published`/`verdict_published`/`comment`) — nothing new is stored.
`kb-code review analytics [--repo R] [--from UNIX] [--to UNIX]` (`GET
/api/reviews/analytics`, bearer) is the disposition calibration instrument —
a severity×disposition matrix, acceptance rates, weekly buckets, latency,
and recurrence, all over non-superseded findings only (`superseded_count` is
reported separately, never silently dropped). `GET
/api/reviews/{id}/impact?path=` (bearer, route-only — the SPA's "Reviewer
X-ray" chips, no CLI verb) reports, per changed callable symbol in a file,
how many of its callers are also in this review's own change set vs.
elsewhere, capped at 20 symbols.

**Export + publish (`kbc-github-export/1`).** `kb-code review export-github
ID [--finding SLUG]... [--include-waived] [--include-orphaned-as-general]`
(`GET /api/reviews/{id}/export/github`, bearer) is pure computation — zero
GitHub calls — that hands back a ready-to-`gh` payload (position-mapped via
the same resolve ladder every other review surface uses); it warns loudly
when `stale_export` is true, recommending a fresh `pr-status` + `snapshot`
before publishing. `kb-code review publish ID SLUG --url URL [--comment-id
ID]` / `kb-code review publish ID --verdict --url URL [--review-id ID]`
(`POST /api/reviews/{id}/findings/{slug}/published` / `.../verdict/
published`, LOOPBACK-ONLY) is advisory-only recording, called AFTER the
agent's own `gh` call succeeds — kb-code never touches GitHub's write API
itself. `kb-code review github-threads ID` (`GET
/api/reviews/{id}/github-threads`, bearer) reads the PR's own GitHub review
conversation back, position-mapped onto the review's latest patchset via
the same carry-forward ladder — GitHub stays the source of truth, nothing
persisted. These join the pre-existing read-only GitHub overlay (`GET
/api/prs`, `GET /api/prs/{n}`, `GET /api/prs/{n}/checks`, `GET
/api/prs/{n}/comments`, `GET /api/prs/{n}/reviews`, `POST /api/prs/fetch`)
— every live GitHub read degrades to `unavailable_reason` rather than
erroring.

**Batch apply.** `kb-code suggest apply-batch ID... [--resolve]` (`POST
/api/annotations/apply-batch`, LOOPBACK-ONLY, PRR-R10) applies many stored
suggestions in one atomic, multi-file batch — two-phase (every id verified
first: existence, not-applied, anchor re-resolve, byte-exact original,
same-file overlap) so any single failure 409s the whole batch with nothing
written, mirroring the single `suggest apply --resolve` contract.

**Nav verbs: hover, framework, resolve-symbol, diagnostics.** `kb-code hover
PATH:LINE:COL --repo NAME` (`GET /api/hover`) composes the resolve ladder's
top candidate into one tooltip-shaped view (a symbol half + defsite +, when
applicable, a framework half). `kb-code framework PATH --repo NAME [--kind
K]` (`GET /api/framework/edges`) is the direct `rails_edges` read — every
edge where `PATH` is the src or dst, direction-labeled, optionally filtered
by a closed rails-lens/1 `kind`. `kb-code resolve-symbol SYM --repo NAME`
(`GET /api/resolve-symbol`) resolves the opaque
`<namespace>:<container>:<name>[:<kind>]` deep-link grammar (e.g.
`rust:kb_core::config:ServerSection`, `rails:route:users#create`) — a miss
is a `{"found": false}` 200, never a 404. `kb-code diagnostics PATH --repo
NAME` (`GET /api/diagnostics`) reads live diagnostics from a configured
lip/1 provider — see the lip lane below.

**SCIP: `kb-code scip run` + staleness.** `kb-code scip run {--repo NAME |
--all} [--dry-run] [--timeout-secs N]` reads each target repo's `[[scip.
repos]]` argv off `GET /api/repos`, spawns it (argv array, never a shell
string, `current_dir` = the repo's working tree), and on a zero exit chains
straight into `kb-code scip ingest` against `<repo_path>/<output>` — one
command replaces "remember the indexer invocation, then remember to run
ingest afterward." `POST /api/scip/ingest` (LOOPBACK-ONLY) compares each
document's CLI-computed `blob_hash` against what the daemon currently has
indexed for that path and SKIPS (never mis-ingests against drifted content)
a doc that's stale, untracked, or in an unrecognised language; a successful
call also stamps a `scip_runs` row at the repo's current git HEAD, which
`GET /api/repos`'s `ScipStatus` reads to answer "is this repo's SCIP index
fresh against its current HEAD."

**The lip provider lane — live LSP overlay (`crates/kb-lip`, `lip/1`).**
`kb-lip` is a NEW crate/binary: a generic LSP→HTTP adapter that spawns one
language server as a stdio child process and speaks a small closed HTTP
surface in front of it (`/lip/identity`, `/lip/definition`, `/lip/hover`,
`/lip/references`, `/lip/diagnostics`, `/lip/code-actions`). kb-code-server
is the CLIENT (`crate::lip`, configured via `[[intel.providers]]` — an
ALLOWLIST of `(repo, lang)` pairs; an empty `repos` opts in zero repos,
never "every repo," mirroring `[semantic]`'s own polarity); the handshake
(`GET {url}/lip/identity`) is lazy on first use and cached for the
process's lifetime, so a provider that's down at boot never delays or
fails kb-code's own start. Every `POST /lip/*` call carries a `blob_sha`
the adapter hashes the file against BEFORE AND AFTER talking to the LSP —
a mismatch answers `{"refused": "blob_mismatch"}` (HTTP 200, never a
best-effort answer against possibly-stale bytes) instead of risking a
wrong "exact." A passing blob guard is what lets a lip/1 answer earn
`precision: "lsp-live"`, `trust: "exact"` — a NEW top tier in the
resolve/hover/usages ladder, consulted first for any configured `(repo,
lang)` pair; on refusal/timeout/absence the existing ladder (SCIP → other
tiers) runs unchanged underneath. Answers are computed fresh per request
and NEVER persisted — no new table, no cache; the deterministic SCIP tier
stays the reproducible one. **v0.40 supersede:** design-lip.md's original
"no code actions" refusal is PARTIALLY REVERSED (operator-ratified
2026-08-28, design-s2.md § S2-C) — the sixth endpoint, `POST
/lip/code-actions`, surfaces LSP quick fixes behind the same
`codeActionProvider` capability gate every other verb uses; the rest of
the refusal STANDS (no rename, no formatting, no
`workspace/executeCommand` — an action whose edit can't be materialized as
`TextEdit`s is dropped and counted, never executed). See
[`providers/README.md`](../providers/README.md) for running a provider
end-to-end (reference configs for ruby/solargraph plus, since v0.40,
rust-analyzer, typescript-language-server, pyright, and gopls; the
`[[intel.providers]]` wiring steps; and live-smoke evidence against a
430-gem Rails app).

**`usages/2` — the classified-usages wire (v7.1, D4).** `GET
/api/usages/2?repo=&path=&line=&col=[&ref=][&limit=]` (`kb-code usages
PATH:LINE:COL --repo R --v2`) is an ADDITIVE second projection of the SAME
ladder `GET /api/usages` has always run — one classifier, two wires;
`usages/1` is frozen and byte-identical. Each row adds: a `kind` from a
CLOSED 34-name vocabulary (`def`·`call`·`read`·`write`·`mutate`·`import`·
`include`/`extend`/`prepend`·`inherit`·`alias`·`instantiate`·`rescue` ·
the Rails-lens names `route`/`view_render`/`i18n_key`/`association`/
`callback`/`job_enqueue`/`helper` · `unclassified`, which is a first-class
honest outcome and never a coerced `call`); a SCIP-style orthogonal `roles`
bitset (SCIP's own bit VALUES, plus a kb-code `vendor` bit) with
`role_names` decoding it — the `test`/`vendor`/`generated` bits are PATH
heuristics read from `[scopes]` (same globs `/api/impact/analysis` uses),
never a claim about content and never an input to a trust class; per-row `precision` (so an `exact` from scip,
from the locals graph, from the Ruby STRICT rule and from lsp-live are
distinguishable rather than one undifferentiated block); the `enclosing`
symbol (Kythe's `childof` — blame the caller, not the file); and
`blob_sha`, pinning the row to bytes. Caps are IN BAND: `totals` carries
the true count per class, `capped[]` names any group whose page hid rows
(`{group, returned, total, reason}`), and `kind_totals` is computed over
the true totals, not the returned page — there is no way for this wire to
truncate silently. **Ruby's one door to `exact`** is D4's STRICT rule
(`intel::ruby_strict`, `precision: "ruby-locals-strict"`): a same-file
local binding, minted `exact` only when the binding is unambiguous in its
own lexical scope AND the name is not a method on the enclosing hierarchy
(`include`/`prepend`/`extend` walked) AND the enclosing method contains no
`eval`/`instance_eval`/`binding`/`send`/`define_method`/`method_missing`;
otherwise the rows stay `likely` and `ruby_strict.verdict` NAMES the clause
that refused. It is computed per request from the file's own bytes and
never persisted (invariant #2), so it costs no salt bump and no re-extract;
the oracle set behind it is built from real client code (kept outside this
repository) and is a release gate — a wrong `exact` fails the build. Cross-file Ruby `exact`
still comes only from the lsp-live lane above. Filters/grouping/cursor are
deliberately NOT on this wire yet (the Usages dock owns them, client-side,
over the returned page — see the dock's own paragraph below).

**The Usages dock and the `gr` repoint (v7.1, V71-E2).** `gr`, the reader's
gutter "N usages" lens chip and the peek card's `u` all now call
`/api/usages/2` and land in the bottom DRAWER, which is where
`web-code/src/desk/placement.ts` has said usages belong since v7.0. The
dock is census → chips → grouped tree → CM6 preview: the **census strip**
renders the SERVER's own `totals`/`kind_totals` verbatim (never a count
derived from the page it happens to hold), and every row hidden by a chip
or by a server-side cap produces its own on-screen REASON line — the rule
is that a count never changes without a visible reason. Chips are `kind`,
`trust`, `exclude tests|vendor|generated` (the role bits, i.e. path
heuristics) and a `scope` path prefix; the grouping axes are
`dir · file · kind · module · enclosing · trust`. The grep lane is not
deleted, it is DEMOTED to an explicit "**mentions**" chip that fetches
`/api/xrefs` only when switched on and reports its count in its own field —
the recon found kb-code already shipping three disagreeing "usages"
numbers, and this is the fix. `]u`/`[u` walk the active set from anywhere
the reader is mounted; a click opens through the Ramp, so a new-tab open is
TRAIL-LINKED back to where the reader came from (`via: usage_of`). The peek
card's `u` is no longer gated on `class === "exact"` — that gate meant Ruby
and Go, where the static ladder cannot reach `exact` at all, never saw the
affordance.

**`kbc-actions/1` — actions on a target (v7.1, D5).** `GET
/api/actions?repo=&path=&line=&col=[&ref=][&end_line=][&end_col=][&text=]
[&target=]` (`kb-code act --list TARGET --repo R [--json]`) returns an
ordered, typed action list for a resolved target, computed per request and
persisted nowhere. `targets[]` is a CLOSED five-name vocabulary
(`symbol · range · text · path · enclosing`) rendered by the SPA as a
SEGMENTED CONTROL, never a hidden cycle; the resolution ladder puts `range`
first for a multi-line selection and `symbol` first for a caret, and a
path-shaped selection becomes a `path` target only after being VERIFIED
against the repo index. Each action carries a stable `id` plus a `version`
(golden-pinned per target kind, because a menu that reorders under the user
can never be muscle-memorised), a one-line `doc` rendered identically in the
menu and in the CLI JSON, an `enabled`/`disabled_reason` pair, a closed
`op` the client executes, the `kb-code` command line that does the same
thing, and — for a row backed by a daemon read — the exact `request`.
**Mutating rows are ABSENT, not disabled, for a caller the server did not
clear** (loopback, or `[review] remote_mutations`), and `mutations.reason`
says so rather than leaving a silent gap. **"Ask here" is a required row on
every target kind**, test-enforced. Nothing auto-navigates: this route
deliberately does not run the resolve ladder (it must open inside a ~50 ms
budget), so it cannot prove `exact`, and D5's rule is that candidate/likely
never auto-navigate. The SPA opens ONE menu component from four doors —
right-click inside the code surface, `.`, `Shift+F10` and the ContextMenu
key — plus a long-press on mobile that opens the single bottom sheet;
Shift+right-click passes through to the browser and the menu's last row says
so. The drag-select pill shows the top three rows of the SAME list, derived.
`kb-code act <ID> --at TARGET` is a RESOLVER, not an executor: it performs a
row's backing READ when there is one and otherwise prints the op plus the
command line that performs it; an ORDINAL is refused by name, and a mutating
row refuses without `--confirm`.

**The Rails lens (`rails-lens/1`).** `crate::frameworks::rails`
deterministically, LLM-freely extracts nineteen closed edge kinds from
Rails convention — routes→`controller#action`, `render`/`turbo_stream` call
sites→partials/views, ViewComponent + Stimulus bindings, model
associations/scopes/callbacks/validations/delegates/concern includes,
ActiveJob/ActionMailer call sites, spec↔subject resolution, i18n keys→
locale files, controller↔helper convention, and `devise_for`→override
controllers — see the full `kind`/`src`/`dst_kind` table in
`crates/kb-code-server/src/frameworks/mod.rs`. Every edge is capped at
`Trust::Likely` (never `Exact` — these are convention matches, not
scope/type proofs; the storage column's own `CHECK` constraint has no third
value to accidentally emit), and a genuinely uncertain match is DROPPED
entirely rather than downgraded. `[rails_lens]` in `kb-code.toml`
(`repos`/`disabled_repos`) overrides the auto-detected default per repo —
`disabled_repos` always wins; see
[`configuration.md`](configuration.md#kb-codetoml-kb-code-daemon-config).

## One Inbox

kb-code v6.0 "One Inbox" (S2, ships as repo tag v0.40) ships a federated
attention queue, a mobile-friendly review-mutation gate, LSP-backed quick
fixes, and a four-language provider fleet — the operator-ratified scope of
`/tmp/design-s2.md`. Kept out (recorded refusal): anything GitHub
push-based (webhooks/App) — polling + `review sweep` stay the freshness
mechanism.

**One inbox.** `kb-code inbox [--daemon URL] [--json] [--watch] [--interval
SECS=30]` (`GET /api/inbox`, `unified-inbox/1`, ordinary `auth_bearer`
read) federates THREE lanes, never a merged cross-lane score
(surfaced-never-scored — the lanes have incommensurable units, each keeps
its own source ordering): reviews awaiting you (verbatim `review-inbox/1`
rows across every repo, the SAME composition/sort `GET /reviews/inbox`
uses, capped 50), open working-tree questions (`intent` in
`question`/`flag-for-agent`, `review_id IS NULL` so a review-scoped
question is never double-counted against the reviews lane's own
`unanswered_questions` term, capped 50), and kb's own desk + open-comments
(two concurrent federated `KbClient` pulls, each capped 50 with its own
`truncated` flag). The kb lane degrades HONESTLY and never 500s: any
kb-side failure collapses the WHOLE kb lane to `{available:false, reason}`
(closed vocabulary `disabled`|`unreachable`|`sibling_mismatch`) while the
other two lanes render regardless. `--watch` is a plain HTTP poll loop with
its OWN seed-then-diff seen-set keyed per lane (review rows on
`(review_id, updated_at, score)`, annotation rows on `(id, updated_at)`,
kb rows on `(kb, id, updated_at)`) — deliberately NOT the SSE-driven
`annotate watch` machinery, whose `SeenKey`/scope model is single-daemon
by design. SPA: `/~inbox` (kb items aren't repo-scoped, so it's a SIBLING
of Home, not nested under `/r/:repo/~…`), badge = reviews.length +
annotations.length (+ kb attention when available).

**LSP quick fixes.** `kb-code code-actions PATH:LINE[:COL] [--end
LINE[:COL]] --repo NAME [--kinds a,b] [--suggest N] [--json]` (`POST
/api/code-actions`, `code-actions/1`, ordinary `auth_bearer` read) lists
LSP code actions for a caller-chosen RANGE from a configured lip/1
provider, same honest-degrade posture as `/api/diagnostics` —
`available:false` + a closed `reason` (`unknown_language`|
`no_provider_configured`|`file_unreadable`|`provider_unavailable`|
`blob_stale`|`capability_absent`, the last NEW here for a provider that
predates lip's code-actions capability). NOTHING is persisted — computed
fresh per request, the same law every lip overlay follows. `--suggest N`
converts the Nth listed action into one annotation+suggestion record PER
(file, edit) via the EXISTING `POST /api/annotations/batch` op — there is
no new mutation route, and applying a suggestion remains the existing
loopback-only apply/apply-batch path. SPA: `QuickFixes.tsx`, shared
between `DiagnosticsCard` and the review-diff diagnostics inspector card.

**Mobile mutations (`[review] remote_mutations`).** Five previously
loopback-only review-mutation route families — finding disposition
`PUT`/`DELETE`, verdict `PUT`/`DELETE`, finding/verdict
publish-recording `POST`, and manual finding create `POST` — move onto a
NEW gated sub-router (`review_remote`) admitting a non-loopback bearer
caller when `[review] remote_mutations = true` (`kb-code.toml`, default
`false`); OFF is BYTE-IDENTICAL to the pre-v0.40 loopback-only `404`
(never a `401`/`403` that would confirm the route's existence to a probing
caller). Every OTHER review mutation (create/snapshot/patch/delete/
viewed/gc, `/reviews/pr`, `/reviews/sweep`, `/reviews/{id}/report` PUT,
`/findings/import`) and the entire working-tree mutation lane (`checkout`,
suggestion apply/apply-batch, `scip/ingest`, `prs/fetch`) stay
loopback-only HARD regardless of the flag — pinned by a one-test-per-route
"never moves" suite. `GET /api/identity` gains additive `remote_mutations:
bool` for capability discovery (never required reading — every route
enforces the gate itself); the SPA renders a small "Remote review
mutations: on/off" chip on Home when present.

**Provider fleet + multi-provider status.** Four new reference lip/1
provider configs join `ruby-lsp.toml`/`solargraph.toml`:
`rust-analyzer.toml` (port 4845), `typescript-language-server.toml`
(4847), `pyright.toml` (4849), `gopls.toml` (4851) — each with a matching
reference `Dockerfile.*`, never CI-built, same posture as
`Dockerfile.ruby`. `GET /api/repos`'s `intel` field stays single-valued
(first config-order match, back-compat); a new `intel_providers:
Vec<RepoIntelStatus>` field carries EVERY matching provider in config
order, for a mixed-language repo (kb itself: Rust + TypeScript) that
legitimately has more than one. See
[`providers/README.md`](../providers/README.md) for install/enable steps and
live-smoke evidence per language.

## The Continuum — ground truth + the Desk

kb-code v7.0 (tag `kb-code-v7.0`, design of record
`docs/research/kb-code-v7-continuum-2026-09.html`) is milestone one of
"The Continuum" program: ground-truth repair, a local-daemon security
hardening pass, the Desk shell, a command registry, and the review-unblock
slice. Full invariants — including the security guards, the git-argv
discipline, and the SPA's keyboard-dispatch contract — live in the new
crate guides: [`crates/kb-code-server/CLAUDE.md`](../crates/kb-code-server/CLAUDE.md)
and [`web-code/CLAUDE.md`](../web-code/CLAUDE.md).

**Audit ledger (`kbc-audit/1`).** `kb-code audit [--since WHEN] [--limit N]
[--json]` (`GET /api/audit`, ordinary `auth_bearer` read, hard-capped at 500
rows) reads back the append-only `mutations` ledger V70-A2 added: one row
per mutating `/api` request — route, method, the admission rung it was
admitted on (`loopback`/`bearer`/`review_gate`), repo/target, a
per-request id, and the outcome (the response status, so a refused or
failed mutation is in the ledger beside the 200s) — written on the blocking
pool AFTER the response so a full ledger disk never turns a successful
mutation into a 500. `--since` accepts an ISO instant/date or a duration
(`90m`/`24h`/`7d`); default 24h.

**Self-description.** `kb-code schema list [--json]` / `kb-code schema show
NAME [--json-schema] [--example]` (`GET /api/schemas` / `GET
/api/schemas/{name}`, D20) serve a CURATED starter set of four
schemars-generated JSON Schemas — `identity`, `healthz`, `scopes`,
`repos-entry` — each a small struct hand-mirrored (not derived) from an
existing response type, not a corpus-wide dump of every wire shape this
daemon serves (`kb_code_server::api_schemas`'s module doc names the exact
scope and the no-drift-test caveat). `kb-code tools [--json]` is the
sibling manifest surface: a clap-tree walk, `--json` adding a
`likely_mutating` field from a checked-in verb-name-fragment heuristic
(`mutation_confidence: "heuristic"` — explicitly not a real per-route
`read_only`/`mutation_class` audit against `router.rs`).

**Nine verb-less routes get CLI verbs** (recon `cli-agent-surface.md` open
question 7 — every route below pre-existed; only the CLI twin is new):
`kb-code diff --repo R --path P --from REF [--to REF]` (`GET /api/diff`),
`kb-code commit SHA --repo R` (`GET /api/commit`), `kb-code file-history
PATH --repo R [--limit N] [--before UNIX]` (`GET /api/file-history`),
`kb-code range-diff --repo R --old RANGE --new RANGE` (`GET
/api/range-diff`), `kb-code scopes` (`GET /api/scopes`), `kb-code doc-refs
--repo R --path P` (`GET /api/doc-refs`), `kb-code review impact ID` (`GET
/api/reviews/{id}/impact`), `kb-code review findings recurrence ID` (`GET
/api/reviews/{id}/findings/recurrence`), and `kb-code pr reviews N --repo
R` (`GET /api/prs/{n}/reviews`). **Known gap, found while documenting this
unit:** the shipped `review impact` verb never forwards the route's
REQUIRED `path` query param (`ReviewImpactParams.path` is a bare `String`,
not `Option<String>`) — `kb-code review impact ID` sends no query string at
all, so the route 400s on every invocation; there is no test exercising
this verb in either the A8 or the H1 gate. Use `GET
/api/reviews/{id}/impact?path=<file>` directly (or the SPA's Reviewer
X-ray chips, described above) until a `--path` flag is added.

**CLI hygiene (D20), the rest.** `kb-code doctor [--agent] [--json]`
reports daemon reachability, sibling-protocol/schema-epoch skew against
this binary's own `kb_core::sibling` constants, token-resolution SOURCE
(never the value), and whether cwd falls inside a configured repo —
diagnostic only, it never refuses a mutating verb on skew. `kb-code token
path` prints the token FILE path only; this CLI's own bearer resolves from
`KB_CODE_TOKEN` or `token_file` (default `<config>/kb-code-token`,
override `KB_CODE_TOKEN_FILE`) — there is no `--token` flag (argv leaks to
`ps`/history/transcripts). `GET /api/repos` gains `writable`/`is_worktree`
per entry and a top-level `loopback` bool. Every NEW `--json` verb this
unit adds uses one envelope shape (`{schema, ok, data, warnings, degraded,
empty_reason}` / `{ok:false, error:{code, message, hint}}`) and one
documented exit-code table (2 usage, 3 conflict, 4 refused/loopback, 5
unreachable) — retrofitting the ~150 pre-existing verbs onto it is
explicitly out of scope for this unit.

**Local-daemon hardening (V70-A2) adds no new route** — six guards over the
daemon AS SHIPPED (Origin/Host allowlist, the `X-Kbc-Request: 1` mutation
header, path containment for working-tree reads, a server-enforced secret
denylist, CSP on the SPA response, the `mutations` audit ledger above) plus
two structural fixes (`Revspec`/`RefRange` validated-ref newtypes, a
per-request scratch object directory for `merge-tree`). Full detail —
including why there is deliberately no separate CSRF token — is in
[crates/kb-code-server/CLAUDE.md](../crates/kb-code-server/CLAUDE.md)'s
invariants 1–5.

**Workspaces v0 (D26, V70-A10) — no new entity.** A workspace is a
`reading_sets` row with `kind: "workspace"` (V0028), riding the SAME wire
surface `kb-code set` already uses. `GET /api/sets?repo=[&kind=workspace][&group=ref]`
lists them (`kind` omitted keeps the pre-existing plain-set listing
byte-identical; `group=ref` groups workspaces by their optional `ref`
label). `POST /api/sets` accepts `kind: "workspace"` plus three additive
fields: `desk_json` (an opaque, ≤64 KiB `DeskState` snapshot — see
web-code/CLAUDE.md's Desk section — stored verbatim, never parsed
server-side), `ref` (a shape-validated, never git-resolved label), and
`description_md` (≤64 KiB Markdown, separate from the pre-existing
one-line `description`). `PATCH`/`DELETE /api/sets/{id}` are unchanged.
`POST`/`GET /api/annotations` gain `set_id` (TEXT, matching
`reading_sets.id`'s own key shape) for a workspace's notes — general
path-less notes AND code-anchored comments alike
(`annotations::ANCHOR_KIND_SET`); a reply inherits its parent's `set_id`.
CLI: `kb-code workspace list|show|save|open [--print-url]|note add|export
--md`; SPA: `~workspaces` (a branch-style landing) plus a Notes tab panel
in the reader rail.

**Entity index (`entities/1`, V71-G0) — `GET /api/entity?repo=&ent=`.**
Every definition site of one Ruby class or module (its "reopenings",
which in a Rails app are scattered across many autoload roots), each with
a trust class COMPUTED PER REQUEST and never stored: `exact` only for a
definition addressed by the name the tree literally nests and whose
indexed blob is still the live one; `likely` for a name the app's
Zeitwerk configuration derives from the PATH (a convention is not a
proof); `candidate` when that configuration could not be read
(`zeitwerk.state: "degraded"`, always captioned in `notes`) or the blob
has drifted. `ent` is a constant path (`Order`, `Reseller::Order`); a
bare last segment resolves across the repo and, when more than one
constant answers to it, EVERY one is listed with `ambiguous: true` —
nothing is merged on a bare name. A member address (`Foo#bar`, `Foo.bar`)
is refused by name, not 404'd: the member table is a later unit. Rows are
keyed `(repo, worktree, path, ordinal)` so a second checkout can never
alias the first. FROZEN as of V72-G1.1 — see the dossier below, which is
a sibling path rather than a widening of this one. CLI: `kb-code entity
<NAME> --repo R [--worktree W] --sites [--json]`.

**Entity dossier (`entity/1`, V72-G1.1) — `GET /api/entity/dossier?repo=&ent=`
`[&worktree=][&inherited=1][&budget=N][&usages_per_kind=N]`.** Everything
about ONE entity, computed per request and persisted nowhere: `entity`
(fqn, `kind` = `class|module|constant|unknown`, namespace, every file that
reopens it), `definitions` (each reopening as a live block with its
`blob_sha`, its `reopening_index` and the literal opener chain — `module
Shop; class Order` vs `class Shop::Order`, with `opener_form`
`top-level|nested|compact|mixed`; the block at the path the app's Zeitwerk
configuration expects keeps its keyword, every other is a `reopen`),
`members` (the merged table across every reopening — name, kind
`instance_method|singleton_method|attr_reader|attr_writer|attr_accessor|constant|alias`,
`visibility` with an honest `unknown` when a `private` keyword's scope
cannot be resolved, `defining_type`, `inherited`, path/line/blob_sha, `via`
= `tree|macro|assignment`, sorted by visibility then name), `hierarchy`
(the superclass chain, `include`/`prepend`/`extend` mixins, known
subclasses and implementors, each with `resolved:
exact|likely|candidate|unresolved`), `usages` (the `usages/2` ENGINE's own
rows, regrouped by kind — every group carries the TRUE `total`, an explicit
`truncated`, and a `trust_census` whose `census_basis` says it counts the
RETURNED rows), `unknown_members` (the metaprogramming holes:
`define_method`, `method_missing`, `delegate`, dynamic `attr_*`,
`class_eval`/`instance_eval`/`module_eval`, `send`/`public_send`, dynamic
`alias_method`), `namespace_tree` (direct children with definition and
descendant counts), and `honesty` (`state: ok|partial|empty`, a `reason` on
the latter two, and a `budget` report — the budget is a ROW budget, spent
definitions-first and usages-last, and every dropped row is counted by
lane). A trust class is never raised here: a line scan caps at `likely`, an
inherited member caps below `exact`, and a usage row's class is the
engine's own, verbatim. An unknown constant is a typed 404
(`entity-unknown`); an entity still in the index whose files carry no live
bytes is `empty` with a reason; an ambiguous bare name refuses to pick and
lists every `candidate`. CLI: `kb-code entity <NAME> --repo R
[--inherited] [--budget N] [--usages-per-kind N] [--worktree W] [--json]`
(and `--sites` for the frozen index above). **SPA (V72-G1.2):** the reader
shell reads this wire as its `dossier` CENTER MODE at `?ent=<fqn>` — the
same `/r/{repo}/{path}` route, with the dock, rail, drawer and both stripes
unchanged (the landmark golden asserts an identical region set for reader
and dossier). The center is the snippet-list dossier in D6's order
(definitions as live, syntax-highlighted blocks · the merged member table
with sort + inherited toggle · hierarchy · grouped usages · the
metaprogramming holes · the namespace tree); the file tree narrows to
`?scope=ns:<fqn>` (kbc-scope/1's existing `ns` atom, with a "scoped to
<fqn>" chip that clears it); and the inspector rail gains a Dossier tab
holding the member table as a jump list. Keys: `Space e d` opens the
dossier for the class/module under the cursor, `i` toggles inherited
(re-fetching with `inherited=1`), `M` cycles the member sort, `] s`/`[ s`
step the sections, and `Space R d` selects the rail tab — the four
in-dossier rows are gated `center == dossier`, so they are inert
elsewhere. Every count on screen is the wire's own (`total` beside
`truncated`, never `rows.length`), "show more" re-asks with a higher
`usages_per_kind` rather than revealing rows the browser never had, and
`partial`/`empty` render as visible captions naming the budget and each
lane it dropped.

**kbc-seq/1 (V71-G0) — `GET /api/seq?repo=[&projection=][&workspace=]`.**
One READ layer over the sequence projections that already exist: `set`,
`workspace`, `tour` and `trail` are `reading_sets` rows (the `kind`
vocabulary, widened from two to four), `board` is a `canvas_sets` row.
Plurals and `canvas` are accepted as documented aliases; an unknown
projection is a 400 naming the vocabulary, never a silent empty list.
Each row reports the table it still lives in (`source`), and `size` is
`null` — not `0` — for a board, whose payload this daemon never parses.
A layer, not a physical merge: nothing is created, moved or unified, and
every projection is still written through its own family's routes.
`PATCH /api/sets/{id}` gains `workspace_id` (D26's "convert to and from
it with one key"), validated to be an existing workspace in the same
repo; an empty string UNBINDS. Boards carry no workspace binding yet, so
`?workspace=` excludes them and says so in `notes`. CLI: `kb-code seq
list --repo R [--projection P] [--workspace ID] [--json]`.

**kbc-tree/1 (V71-F1) — `GET /api/tree/2?repo=[&view=][&root=][&depth=]
[&expand=][&scope=][&filter=][&mode=][&decorate=][&base=][&review=]
[&limit=]`.** The PROJECTED, decorated tree, computed ONCE server-side so
`kb-code tree` and the SPA's left dock render the same rows (`/api/tree`,
the per-directory ODB listing, is untouched and frozen — the same
one-ladder-two-wires shape `/api/usages/2` has). Four projections:
`physical` (the mirror index), `role` (Rails role buckets — a path
CONVENTION, every bucket capped at `likely`, structurally never `exact`),
`namespace` (the V71-G0 entity index, a constant expanding into the files
that define it, each group taking the WEAKEST of its definition sites'
classes) and `change` (what differs from a base ref, grouped by git
status). Two things are load-bearing and always in band: **`unplaced`** —
every file the projection could not place, listed (capped) with an exact
`unplaced_total` beside it, because a projection that hides work is worse
than no projection — and **`truncated`**, which names the cap, the
returned count and the true total. `filter=` ranks through the daemon's
ONE matcher (nucleo; match positions returned as UTF-16 ranges) in VS
Code's two modes: `filter` prunes non-matches but keeps a match's
ancestry, `highlight` keeps every row and badges the ancestors with a
match count. `decorate=` takes up to THREE lanes from `git,review,
findings,annot,todo,bookmark` — a fourth is dropped and named in `notes`
— and a folder row carries the fold of its subtree (worst finding, summed
annotations, changed-descendant count). CLI: `kb-code tree --repo R
--daemon URL [--view V] [--scope EXPR] [--filter TEXT] [--mode
filter|highlight] [--decorate a,b,c] [--base REF] [--review ID] [--depth
N] [--limit N] [--format tree|paths|json]`; any of those flags routes the
verb to this wire, none of them leaves it byte-identical to the legacy
listing.

**kbc-scope/1 (V71-F1) — the path-set algebra, resolved through the same
route.** A boolean expression (`&&`, `||`, `!`, parentheses, juxtaposition
= AND, `$name` references to `[scopes]` in `kb-code.toml`) over atoms.
The atoms kbcq/1 already owns — `path:` (JetBrains' `//*` recursive and
`/*` this-level wildcards), `ext:`, `lang:` — are parsed by kbcq/1's own
parser and read back out of its typed filters, never re-parsed, so they
cannot mean one thing in the search box and another in the tree. The
tree-only atoms are `role:`, `ns:`, `pack:` (Packwerk `package.yml`,
deepest wins), `owner:` (CODEOWNERS, GitHub's last-match-wins),
`set:`, `annot:open`, `todo:any`, `bookmark:any`; `review:`, `finding:`,
`session:`, `since:`, `churn:`, `diag:` and `sym:` are NAMED as
unresolved, each with its reason, rather than silently accepted. Parsing
is total, but resolution REFUSES rather than guessing: any diagnostic
(unknown atom, unbalanced parens, unknown `$name`) means the scope is not
applied, `scope_applied: false` says so, and the UNSCOPED tree is
returned with the reason in `notes` — never a silently different set and
never an empty tree. CLI: `kb-code scope list|show <NAME|EXPR>
[--paths]|from-paths <PATH>… |import <packwerk|codeowners>`. `from-paths`
is JetBrains' scope-from-selection: a PURE proposal from the selection
alone, each candidate resolved against the real repo so the "+N files you
did not select" delta is shown before anything is kept. Nothing is
persisted by any of these verbs — saved scopes in sqlite are a later
unit; `[scopes]` in `kb-code.toml` is the one source this milestone
reads, tagged `source: config`.

**Track R — review unblock (D22 local-canonical): IN FLIGHT, not yet
landed as of this writing (V70-A9D).** This section is a placeholder,
deliberately left unfilled rather than guessed: Track R registers a real
Rails repo on the loopback daemon with a writable checkout, repoints that
repo's own PR-review command at it, and imports one of its open PRs end to
end as the first real exercise of the LLM-authored review path. Update this paragraph (route/CLI surface, any
new `review compose` verb, the artifact-linking convention) once R lands
and its commit is on `main`.

## Understanding — kbcq/1, one matcher

kb-code v7.1 grows the Search-Everywhere box's grammar and gives every
name-shaped lane ONE matcher. Design of record: D3 of
`docs/research/kb-code-v7-continuum-2026-09.html`.

**kbcq/1, the one query grammar** (`GET /api/search?q=`). The existing lane
prefixes (`@sym` `#file` `/re/i` `?nl` `~session` `~~transcript`) and the
fixed, never-interleaved section order are unchanged. New: `"quoted
phrases"` (a quoted token is a search TERM, never a filter — quoting is how
you search for the literal text `lang:rust`), negation on the keys that
support it (`-path:` `-lang:` `-ext:` `-kind:`), `a|b` alternation on the
multi-valued keys, and four new keys — `ext:` (path suffix, so `.erb`/`.yml`
work where `lang:` cannot), `kind:` (`Symbol::kind`, symbols lane), `sort:`
(`relevance|path` — re-orders the returned PAGE, never re-selects it) and
`explain:1`. The parser NEVER fails: an unknown key, an empty value, a bad
value for a closed vocabulary or an unsupported negation is searched as an
ordinary word and reported in a `diagnostics[]` entry naming the token (with
a did-you-mean within two edits: `laang:rust` → `lang:rust`). The response
also carries `normalized` — the canonical re-rendering of the query that
actually ran, a fixed point under re-parsing, and what `kb-code search`
prints as `(ran: …)`. The grammar has a TS mirror
(`web-code/src/lib/kbcq.ts`) and the two are pinned in lock-step by ONE
shared fixture, `crates/kb-code-server/grammar/kbcq.golden.json`, walked by
both sides' golden tests.

**One matcher, with match indices and a hard exact tier**
(`crates/kb-code-server/src/search/matcher.rs`). The files and symbols lanes
no longer drive nucleo themselves; both go through one module that also
returns (a) the UTF-16 `[start, end)` offsets of the matched characters —
`FileHit.ranges` / `SymbolHit.ranges`, which the SPA highlights with, closing
the "two matchers, only one of them highlighted" gap — and (b) a structural
`tier` (`exact` → `prefix` → `fuzzy`). The tier is an ordering key ABOVE the
score: an exact filename or symbol name can never be displaced by a longer
fuzzy match that scored higher, whatever the ranking factors say. Every hit
also carries a stable `hit_id` (`h-` + 12 hex over lane/repo/path/anchor) an
agent can name in a later turn.

**Ranking factors, each behind its own flag** (`[search]` in
`kb-code.toml`: `frecency` = on, `demote_generated` = off, `lexical_rarity`
= on — see [configuration.md](configuration.md)). Following kb's
own MI-W5.R precedent, a factor whose flag is off is SKIPPED, not multiplied
in as a neutral 1.0, and is surfaced on the decomposition iff its own flag is
on. `explain:1` returns, per hit, `{lane, rank, tier, base, factors[],
final_score}` and, per section, `{rank_basis: "per-lane", fusion: "none",
factors_on[]}` — because this box does not fuse: sections are fixed and never
interleaved, so a rank is a position within one lane and there is no
cross-lane additive score to report. `lexical_rarity` is the text lane's
first ranking of any kind (it replaces `ORDER BY path`): dual identifier
tokenisation over the text each hit ACTUALLY matched, weighted by
within-result-set rarity.

**`kb-code search` for agents.** `--explain` (sugar for appending
`explain:1` — the grammar is the protocol), `--count-only` (per-lane counts,
the ~50-token probe before spending a budget on bodies) and `--budget N`
(≈4 bytes/token; drops WHOLE hits from the end of the lane order and reports
`truncated: {by: "budget", omitted: N}` — never a clipped snippet, never a
silent cut). `kb-code bench-search` can now score the sessions/transcripts
lanes at all (its match key was `path` alone, so two of six lanes always read
0%); the recorded client query set (drawn from the operator's own
`/pr-review` Pass-2 moves) and its runs are kept outside this repository;
`crates/kb-code-cli/bench/kb-repo-queries.jsonl` is the in-repo set.

**The results page** (`/search`) — a facet rail, grouping, a live preview,
refine-within-results, history + saved searches and a result-set stack, all
over the SAME `GET /api/search` the Omnibox calls. Two more kbcq/1 keys
carry it, so the page and `kb-code search --json` return the same thing:

- **`facets:1`** (`kb-code search --facets`) adds a `facets` census to the
  response — counts by lane, repo, language, extension, directory and symbol
  kind, each value carrying the kbcq/1 CLAUSE that selects it. Clicking a
  facet in the rail APPENDS that clause to the query string, so a filter you
  cannot see in the box is not in force (kb's own `galleryUrl` discipline,
  root CLAUDE.md #35). The counts' `basis` is the literal string `"page"`:
  they count the hits this response returned, after every lane's own cap —
  never a corpus estimate — and the payload says so in words.
- **`group:file|kind|lane|dir|none`** (`kb-code search --group KEY`) adds a
  `groups` partition to each section: `{key, label, count, indices[],
  hit_ids[]}`, addressed by POSITION because four of the six lanes carry no
  stable hit id. Grouping never re-ranks and never drops a hit; a lane with
  no answer for the key gets ONE honestly-labelled group. `group:none` is a
  distinct fact from an absent `group:` and survives a `normalize` round
  trip, so a saved "ungrouped" search stays ungrouped.

The response also always carries **`stale: {generation, as_of}`** — the
daemon's monotonic index counter (not a commit distance: "behind by N
commits" would mean a git call per keystroke) — and the FILES lane's hits
carry **`blob_sha`**, the blob they were indexed from. The other five lanes
do not, and the wire does not pretend otherwise. A lane that could not run
still reports it as that section's `unavailable_reason`.

Refine-within-results, search history and saved searches are BROWSER-LOCAL
(`Alt-r` / `Alt-h` / `Alt-s`): refinement is orderless, literal, smartcase
narrowing of the page already returned — it never re-queries and never
re-ranks, and the page reads "N of M shown". There is no `saved_searches`
table and no `kb-code search saved` verb; CLI parity for a saved search is
the `kb-code search '<kbcq>'` line the page copies with `Alt-y`. The page's
keys are ordinary `kbc-cmd/1` rows (`scope: "search"`, `dispatch:
"surface"`) resolved through the same dispatcher every other surface uses.

## Languages and lanes (v7.2)

**`syntax/1` (V72-H1, D7) — `GET /api/syntax` and `GET /api/parity`.** ONE
registry (`crates/kb-code-server/src/syntax.rs`) says, per file TYPE, which
ENGINE parses it, which EXTRACTION TIER the ingest pipeline runs, whether
it is an injection host, and the extensions, exact filenames and `#!`
interpreters that address it. `lang::detect` is a thin façade over that
table: `Some` means "something in this build PARSES this file, and `salt`
keys its derived rows". The engine is `tree_sitter` (the grammar crate),
`scanner` (V72-H3's first-party HAML scanner) or nothing at all — the wire
carries `grammar` and `scanner` as two nullable fields derived from it, so
"no grammar" and "not parsed" are distinguishable.

The tier is `full` (highlight spans + symbols), `highlight_only` (spans;
symbol extraction skipped by ONE short-circuit at the top of the pipeline,
never an aborted walk) or `none` (neither — a type with no grammar linked,
or a parse-only grammar like ERB). It rides `GET /api/file` and the
per-file `GET /api/symbols` as `tier` + `tier_reason`, both additive, so an
empty symbol list can be read as "no symbols by tier" instead of a bug. It
is a property of the file TYPE, decided before a byte is read — a different
axis from `files.lang`'s content skip markers (`unknown`/`binary`/
`too-large`/`lfs`).

The stem table D7 asks for ships here: `Gemfile`, `Rakefile`, `Guardfile`,
`Capfile` and the `.rake`/`.jbuilder`/`.gemspec`/`.ru` extensions are Ruby
(they were `unknown` before, i.e. Ruby source the instrument silently
ignored); `Gemfile.lock` deliberately stays plain (a resolver artefact in
its own format, not Ruby); the `#!` sniff widens past bash-family to
`ruby` and `python`; `Dockerfile` and `.sql` are NAMED rows with no
grammar, so the gap is visible instead of invisible.

`GET /api/parity` is the **Parity Grid**: rows = every registry language,
columns = `highlight, symbols, outline, usages, hover, lens`, each cell
`yes`/`no`/`partial` and DERIVED from the predicate that actually gates
that lane — never hand-typed, so it cannot claim a capability the daemon
does not have. Every non-`yes` cell carries a reason. The grid is pinned by
a checked-in golden (`crates/kb-code-server/tests/fixtures/
parity.golden.json`), so a capability change is a deliberate golden update,
reviewable in the diff that causes it. CLI: `kb-code syntax [--json]`,
`kb-code parity [--json]` — daemon reads, because the honest answer is what
the DAEMON's build can do.

Not in this unit, by design: new grammars (SCSS/CSS/Markdown), the
injection-aware pipeline, the universal `outline/1` contract, and the
`symbol_salt`/`highlight_salt` split. `highlight_only` therefore ships as a
mechanism with no production row yet, recorded by a test that fails when
the first one lands.

**`haml/1` (V72-H3, D7) — the first-party HAML scanner.** HAML is the one
file type kb-code parses with code it owns rather than a tree-sitter
grammar: no viable grammar exists (the best available is a 13-star
repository, and nvim-treesitter registers no `haml` entry at all) while a
Rails monolith's views are roughly half HAML. `crates/kb-code-server/src/
haml/` is an indentation-aware scanner — `lexer` (physical lines, byte
ranges, sigils), `parser` (the tree), `extract` (spans, outline, Ruby
fragments), `projection` (the corpus's comparison shape) — registered as a
`full`-tier `syntax/1` row whose engine is `scanner: "haml/1"` and whose
`grammar` is `null`.

It reproduces HAML's own tree, including the two rules a naive indentation
walk gets wrong: a **mid-block keyword** (`- else`, `- when`, `- rescue`)
is a CHILD of the block it continues, and **continuations are resolved from
the source** — an attribute list runs until its brackets balance (a scan
that respects string literals AND nested `#{}`), a `|` block until a line
does not end in `|`, a trailing comma pulls in one more line — each capped,
each cap a diagnostic. Malformed input is CAPTIONED, never fatal: tabs in
the indentation, a dedent landing between two open levels, an unbalanced
`{` or `#{`, invalid UTF-8 all produce a `DiagnosticKind` over whatever
structure was recoverable, where HAML's own parser raises and produces
nothing. Diagnostics live on the returned value and are persisted nowhere —
kb-code has no diagnostics table and this unit adds none.

What it feeds:

- **highlight spans** — the template's own tokens (tag names, shorthands,
  attribute names and literal values, script sigils, filter names,
  comments, interpolation delimiters) plus every Ruby fragment painted by
  the EXISTING Ruby highlighter, its spans shifted into HAML coordinates.
  Sorted and non-overlapping, as the per-line integrity guard requires.
- **the outline** — one `symbols` row per element and per filter, named the
  way the source reads (`%section#hero.big` → `section#hero.big`),
  `container` naming the nearest enclosing element, line range covering the
  subtree. Script lines are NOT outline rows: they are Ruby, and this lane
  mints no symbol for Ruby it did not scope-resolve. These kinds join
  YAML/TOML/JSON's `key` in `extract::OUTLINE_ONLY_KINDS`, so they never
  pollute a repo map.
- **the Rails lens, through the EXISTING extractors.**
  `haml::extract::ruby_program` concatenates every Ruby fragment (script
  lines, `#{…}` interpolations, `{…}` attribute hashes, `[…]` object
  references, a `:ruby` filter body) into ONE parseable Ruby program with
  `end`s derived from the indentation tree, plus a LINE MAP back to HAML
  lines. `frameworks::rails::support::walk_haml_ruby_fragments` parses it
  once and hands the root to the SAME `scan` callback the ERB walk uses,
  re-anchoring lines afterwards — so `views`, `i18n`, `view_component` and
  `jobs_mailers` resolve HAML call sites with byte-identical logic and mint
  `render_partial`, `render_view`, `turbo_stream_target`, `i18n_key`,
  `view_component_render`, `job_enqueue` and `mailer_deliver` at the same
  trust classes. **Capped at `likely`/`candidate` structurally** — the
  lens's `Trust` has no `Exact` variant and the `rails_edges` DDL is
  `CHECK (trust IN ('likely','candidate'))`. Unlike ERB's per-tag re-parse,
  the HAML walk reconstructs control flow across constructs, so a `render`
  written inside a `- if` resolves. `stimulus` is deliberately absent from
  the HAML dispatch and the code says why: it regex-scans the ERB CST's raw
  HTML `content` nodes, and HAML has no HTML text to scan.

Correctness is pinned by a **divergence corpus**: 41 synthetic templates
under `crates/kb-code-server/tests/fixtures/haml/`, each with the REAL
`haml` gem's parse projected into one shared shape. The expectations were
generated ONCE, offline, on a developer box (`generate_expected.rb`, haml
7.5.1); **the gem is never invoked by CI and never by the daemon** — a test
greps the corpus suite's own source for a process-spawn call so that cannot
quietly stop being true, and `CORPUS.md` records the version, the three
normalisations and the differences deliberately NOT normalised away. Beside
it: ~500 byte-level mutations asserting no panic and no out-of-bounds span,
offset-map round trips, a program-parses-as-Ruby sweep, and an ERB↔HAML
edge-parity pair.

Parity Grid cells for `haml`: `highlight` yes; `symbols` **partial**
(template outline rows — the embedded Ruby fragments get no symbols of
their own); `outline` **partial** (rendered from the symbols table; the
universal `outline/1` contract is not built yet — the same reason every
other row carries); `usages` **partial** (convention edges only,
`likely|candidate`, no occurrences index so the ladder has no exact tier);
`hover` **partial** (word-scan resolve over template outline rows); `lens`
**no** (the CODE lens is callable/type declarations with occurrence-backed
usage counts, which HAML has neither of — the Rails lens is a different
lane, and claiming `partial` here because render/i18n edges exist would be
exactly the over-claim the derived grid exists to prevent).

Not in the HAML unit, by design: the universal `outline/1` contract and the
injection-aware pipeline generalisation (H2a — but the HAML→Ruby fragment
mapping is written so H2a can lift it), the SPA's consumption of HAML in
the Rails lens (I2), and any Herb/ERB change (gated off by D7).

## Rails — `rails/1`, the entity index (v7.2)

kb-code v7.2 turns the `rails-lens/1` convention edges into the NOUNS a
Rails developer names. Design of record: D7 + Track I of
`docs/research/kb-code-v7-continuum-2026-09.html`.

**`rails/1` (`GET /api/rails/*`) — the Rails entity index.** A derived VIEW
that joins three things this daemon already stores: the entity index
(`entities/1`, every Ruby `class`/`module` definition site), the Rails lens
(`rails-lens/1`, the convention edges extracted at ingest) and the mirror
index (`files`/`symbols`, for the templates that are not constants and the
methods that are not entities). Eight nouns come out — `model`,
`controller`, `action`, `route`, `job`, `mailer`, `view`, `concern`.

Computed **per request and persisted nowhere**: there is no `rails_entity`
table and no migration. The join is a fold over reads this daemon already
serves, in the posture root invariant #2 states for the whole doc↔code
bridge and that `codelens/1` and `entities/1` already hold — a derived table
would buy latency at the price of a fourth thing that can be stale.

Every row is an **address** (path, line, `blob_sha`, plus an FQN or a route
triple) carrying its trust class and the **witnesses** that produced it —
which convention, which edge, which definition — so a reader can check the
arithmetic rather than trust the badge. **No row is ever `exact`**: the
class is minted by one function whose return type is the Rails lens's own
two-variant `Trust`, so a directory name and an English pluralisation
structurally cannot reach the oracle bar. Two independent witnesses, a live
blob and a Zeitwerk config that could be read buys `likely`; a drifted blob
or a degraded config demotes to `candidate`.

What it cannot say, it says: class ancestry is not indexed (there is no
entity-edge table yet), so a "model" is a class under an `app/models` root
corroborated by the lens's own `association`/`validation`/`scope`/`callback`
edges — not a proven `ApplicationRecord` descendant. Method visibility is
not indexed either, so the `action` noun resolves `public` with a cheap line
scan of the controller source under a hard read budget; past the budget an
action reads `visibility: "unknown"`, is flagged, and the response is
`partial` with the budget named.

Routes now carry their **address**. `rails-lens/1`'s `route_action` edges
gained `extra_json = {"verb": "GET", "path": "/orders/:id"}`, reconstructed
by the same DSL walk that resolves the controller (two independent axes:
`namespace` moves both the module and the URL, `scope module:` only the
first, `scope path:` only the second; `member` contributes `:id`, a nested
resource the parent's `:<singular>_id`, and a leading `/` escapes the
enclosing scope). This is additive content, **not** a `rails-lens/2` bump —
no `kind` is added and no `kind`'s meaning changes — so an edge written by
an older binary simply has no address until its file is re-extracted, and
reads as unknown rather than `/`.

Routes:

- `GET /api/rails/home?repo=` — the **passport**: framework detection, the
  Rails version resolved from `Gemfile.lock` (else `Gemfile`, else honestly
  absent), TRUE totals per noun, lens freshness (edge count, source-file
  count, orphaned source paths, grammar version, index generation), the
  Zeitwerk read state, and `honesty`.
- `GET /api/rails/{models|controllers|actions|routes|jobs|mailers|views|concerns}?repo=[&q=][&limit=][&offset=]`
  — one noun's rows. `q=` is a case-insensitive substring over name and
  path, applied BEFORE `total` is counted, so `total` stays the true one;
  `truncated` names the gap.
- `GET /api/rails/orphans?repo=` — the **orphan report v1**: six lanes
  (routes with no reachable action · public actions with no route · views no
  render edge reaches · models referenced only from their own file · jobs
  never enqueued · locale keys never referenced). Every lane states its own
  witness and **why it might be wrong**, and the report carries a caption
  saying the whole thing is derived from likely/candidate convention edges —
  a triage queue to read, never a verdict to act on unread.

Every response reports one of four read states (`ok` / `empty` with a reason
/ `partial` with the budget that bit / `error`). All ten routes are ordinary
`auth_bearer` browsing reads, declared as `RouteContract`s in
`kb_code_server::rails::routes::V72_I1_ROUTES` and walked from both the
server and the CLI side by the same dead-surface tests V71-G0 added.

**kbcq/1 Rails facet atoms.** The search grammar gains `model:`,
`controller:`, `action:`, `route:` (matching either the verb+path address or
the `controller#action` target), `job:` and the generic `rails:<noun>` —
whose value vocabulary is `crate::rails::NOUNS` itself, so a ninth noun
cannot exist on one surface and not the other. Each resolves ONCE per
request to the set of files that noun lives in; the files, symbols and text
lanes keep only hits inside it, and the narrowed section carries a
`caption` naming the noun and the file count. Atoms are ORed with each
other and ANDed with every other filter. They need a SINGLE repo in scope —
with more than one the atom is not applied and the caption says why, rather
than matching a same-named path in the wrong repo. Same lock-step
discipline as every other key: the TS mirror (`web-code/src/lib/kbcq.ts`)
and the one shared fixture `crates/kb-code-server/grammar/kbcq.golden.json`.

CLI: `kb-code rails {home,models,controllers,actions,routes,jobs,mailers,views,concerns,orphans}
--repo R [--q TEXT] [--limit N] [--offset N] [--json]`. The `--json` form is
the standard envelope with `schema: "rails/1"`; the text form is compact
enough for an agent, and every number it prints is the daemon's own.
