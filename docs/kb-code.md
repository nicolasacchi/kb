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
**v7.3 widens it to `review-timeline/2`** (the PR body, working-tree
comments, compose revisions, the agent report, claims, GitHub comments and
the hunk↔turn join, plus filters and paging) — additively, every v1 payload
key unchanged; see [The stream](#the-stream--timeline-v2-pseudo-files-claims-hunkturn-v73-track-k).
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

## Review diff v2 (v7.3, Track K)

The full-page review reader (`/r/{repo}/~reviews/{id}/diff[/<path>]`) is
where a patchset is actually read. v7.3's diff v2 (design §D9) turns it
from a scrolling list of unified diffs into a navigable instrument. Every
control writes the URL and reads nothing else, so a reload — or a link you
paste to someone else — reproduces the view exactly.

**URL grammar** (all additive; each omitted at its default):

| param | values | meaning |
| --- | --- | --- |
| `ps` | `N` · `A..B` | one patchset, or the INTERDIFF between two. Absent = latest. |
| `ctx` | `10` · `full` | the context dial. Absent = git's own `-U3`. |
| `noise` | `collapsed` | collapse noise-labelled hunks. Absent = `shown`. |
| `map` | `0` | hide the file-map column. Absent = shown. |
| `file` | a path | the map cursor / all-files scroll hint. |
| `hunk` | a hunk id | the hunk cursor, content-addressed (below). |

**The file map column.** The review's files as a left column, grouped into
chapters. The chapters are DERIVED from `GET /reviews/{id}/reading-order`'s
own per-stop `reason` strings by grouping CONSECUTIVE stops that share one
— the wire carries no authored chapters (those are a Track-K SHOULD), and
the column says "chapters derived from the reading order's own reasons" on
screen rather than implying otherwise. Grouping is consecutive, never a
global re-bucket, because the reading order is a topological walk and
re-bucketing would reorder the one thing it asserts. A file the reading
order does not mention lands in a named trailing group rather than being
dropped. Each row carries per-file chips (viewed · open comments ·
findings · unpublished drafts · noise class), every count taken verbatim
from the wire. The column shares the `]f`/`[f` cursor and is hidden on
mobile, where the existing "Files" drawer stays the single entry point.

**Per-hunk state, content-addressed (`kbc-hunkid/1`).** Each hunk shows a
strip: its `@@` header, `+N −M`, a threads chip, a drafts chip, its noise
chips, a fold and a viewed checkbox. Viewed is SERVER state — new table
`review_hunk_viewed` (migration V0031) behind
`PUT /api/reviews/{id}/hunk-viewed` `{hunk_id, path}` and
`DELETE /api/reviews/{id}/hunk-viewed/{hunk_id}`, both **loopback-only**,
the same unconditional gate the per-file `PUT .../viewed` rides (this is
the review-mutation family `[review] remote_mutations` explicitly never
reaches). The marks ride back on `GET /reviews/{id}/files` as an ADDITIVE
`hunks_viewed: [{hunk_id, path}]` array — absent from an older daemon,
which every reader treats as "no marks", never as an error.

`hunk_id` is minted by the SPA (`web-code/src/lib/diffHunks.ts`) as a
64-bit FNV-1a over the file path plus the hunk's own `+`/`-` lines,
deliberately EXCLUDING line numbers and context rows — so a rebase that
renumbers the file around a change does not rename it, which is the
"content-addressed per-hunk reviewed state that survives rebases" §D9
asks for. The daemon stores the id opaquely and does not parse diffs; the
addressing scheme can therefore version (a `kbc-hunkid/2`) with no
migration, and an id minted under an older scheme simply stops matching —
a viewed hunk reads UNVIEWED, never the reverse.

**The patchset switcher.** A base→head pair in the diff header. Head picks
`GET /reviews/{id}/files?ps=N`; choosing a base patchset switches to
`GET /reviews/{id}/interdiff?from=&to=` and writes `?ps=A..B`. The
interdiff wire's file rows carry no viewed or annotation fields, so in
range mode those columns are ABSENT under a caption saying why — never
rendered as zero. Before this unit the full-page diff read `?ps=` and
nothing ever wrote it, so it was permanently pinned to `latest`.

**Expand / collapse and the context dial.** `z c`/`z o`/`z a` fold one
hunk; the dial (`?ctx=3|10|full`) applies to every hunk on the page, and
"↑ 10"/"↓ 10" widen one hunk further. Widened rows are REAL file content:
`GET /api/diff` is hardcoded to `-U3` (no context param, and this unit did
not add one), so the extra rows are spliced from
`GET /api/file?repo=&path=&ref=<patchset tip>`. If that file is
unavailable the hunk stays at its wire width under a caption — kb-code
never invents a line of code it did not read.

**Noise classification — a label, never a filter.** Five classes, each
with its rule stated verbatim in the chip's tooltip:

| class | rule |
| --- | --- |
| `generated` | the path matches a generated-output pattern (lockfile, `*.gen.*`/`*.generated.*`, schema dump, minified bundle, test snapshot) |
| `rename-only` | the file's status is a rename and the patch adds and removes zero lines |
| `whitespace-only` | removing every space and tab makes the hunk's added text identical to its removed text |
| `moved` | this hunk's removed block is byte-identical to an added block in another file already loaded on this page |
| `large` | the hunk changes more than 120 lines (a file is large past 800 changed lines) |

Classification is pure, client-side and golden-pinned
(`web-code/src/lib/diffNoise.ts`). Nothing is ever removed from the page:
`?noise=collapsed` COLLAPSES a labelled hunk, which stays counted in the
header census and is one click from open. Two deliberate limits are stated
rather than hidden — hand-authored `migrations/` are not `generated` (only
schema DUMPS like `db/schema.rb` are; a migration is the change, not a
rendering of it), and `moved` only searches the file diffs the page has
loaded, so a label always names its counterpart file while its absence
claims nothing.

**Drafts and atomic publish.** Comments and questions composed in the diff
are DRAFTS: badged as such, listed in a tray, kept in the browser's
`sessionStorage` per `(repo, review)` — they survive a reload and are
discarded with the tab, which the tray says out loud. There is no
`review_drafts` table and no `kb-code review draft` verb; an unpublished
draft is not yet a fact about the review. `Publish` sends the whole tray as
ONE `POST /api/annotations/batch` — one store transaction, at most one
`annotation.changed` SSE — and the tray is cleared only after the server
accepts, so a refused batch leaves nothing half-written. `Discard` goes
through the app's shared confirm host.

FINDINGS are deliberately **not** drafted, and the reason is a gate:
`/api/annotations/batch` is an ordinary bearer route, while creating a
finding (`POST /reviews/{id}/findings`) rides
`review_gate::review_mutations_gate` (`[review] remote_mutations`, default
OFF). Adding an `add_finding` op to the batch would graduate finding
creation off that gate as a side effect of a UI change. The composer's
finding tab therefore still posts immediately through its own route;
folding findings into the atomic publish needs a gated batch op and is
recorded here as open work.

**Keys** (all `scope: diff`): `Space h` hunk viewed · `z c`/`z o`/`z a`
fold/unfold/toggle · `Space c` context dial · `Space n` noise collapse ·
`Space m` map column · `] p`/`[ p` patchset step · `Space w` drafts tray ·
`Space W` publish · `Space X` discard. `Space h` rather than bare `v`
because `v` is the reader's visual mode and the two scopes are coactive —
the same ruling that moved the per-file `diff.toggle-viewed` to `Space v`.

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

**kbc-canvas/1 (V74-L1) — boards of reference nodes, on the Ladder.**
Design of record: D10 (boards, the canvas rebuilt) + D21 (an agent-proposed
board is PENDING until a human accepts it), Track L. A **board** is an
ordered set of REFERENCES into surfaces this daemon already serves — never
a copy of them — plus authored edges between those references, and every
reference is re-resolved through the carry-forward **Ladder** on every read.

Three properties hold the whole thing up.

*Nothing about resolution is stored.* There is no state column: `pinned` /
`carried` / `orphan` is computed per request, exactly as `lanes::classing`
and `entities::class_for` compute theirs, and for the reason root invariant
#2 states for the whole doc↔code bridge — a stale fact must never read as a
fresh one. *An orphan is SHOWN, never dropped:* it keeps its title, its
note, its last-known address and the one line of source it was anchored to,
and says the code is gone. *Boards are coordinate-free:* the document has no
`x`/`y` anywhere, layout is the SPA's (TypeScript, one engine), and the only
authored geometry is a `pins` map — an explicit override for a card a human
placed by hand.

Node kinds are CLOSED at ten. `code` (path + optional symbol + a primary
range and an optional context range + blob + guard hash), `note`
(Markdown), `query` (a kbcq/1 query plus the count its author saw), `hunk`
(review + patchset + a `kbc-hunkid/1` address), `finding` (review +
`f-*` slug), `annotation`, `turn` (session + `t-<uuid12>`), `bookmark`,
`group` and `link`. Edge kinds are CLOSED at eight — `calls`, `renders`,
`reads`, `writes`, `then`, `implements`, `contradicts`, `question` — and an
edge's provenance is `authored` or `derived`: a DERIVED edge must name the
trust class it inherited, an AUTHORED edge may not carry one at all (a
human's arrow is not a claim the index can back). Node threads REUSE the
annotations store — a node's `thread_id` points at an annotation parent, and
there is no second comments table.

Four states answer a node, and what each promises is different: `pinned`
(the claimed bytes are still at the claimed place — the blob is unchanged,
or the guard hash over the range still matches, or the anchored line
re-resolved verbatim where it was), `carried` (the bytes moved; the range
comes back shifted, with `shifted_by`), `orphan` (no confident match, or the
addressed thing is gone), `present` (an id-shaped reference whose target
exists) and `inert` (a note, a group, a link — there is nothing to resolve).
Every state carries a REASON from a closed vocabulary. The `code` rungs are
`annotations::anchor_for_line` + `annotations::resolve` guarded by
`review_comments::line_matches_snippet` — the same three calls review
comments, findings and `aug-lane/1` make, not a fourth implementation.

What a board read deliberately does NOT probe, it says: a `kbc-hunkid/1` id
is a content address the SPA mints from diff text, so `present` for a hunk
node means the review and the patchset exist and the daemon did not
recompute the diff; a session turn is probed for EXISTENCE only and never a
byte of transcript text (a board is bearer-readable, the transcripts lane is
loopback-only); and a `query` card's count is not re-run on an ordinary read
at all — `?live=1` opts in, `canvas sweep` always does, and the count that
comes back is a PAGE count with `basis: "page"`, the same honesty the facet
census already ships.

**The apply lint.** `kb-code canvas apply -f board.json`
(`POST /api/boards/apply`, loopback-only) is an idempotent upsert BY SLUG:
the same document twice is a no-op with `unchanged: true` and no revision
bump, because the whole document has one canonical content hash rather than
a field-by-field diff that could disagree with itself. A node that survives
a re-apply keeps its id, which is what makes a node thread durable across an
edit. Before anything is written the document is linted, and the report is a
LIST rather than a first error — an agent retrying an apply needs every
problem in one round trip:

| rule | severity | what it catches |
|---|---|---|
| `coordinates` | refuse | `x`/`y`/`width`/`height`/`position`/… ANYWHERE outside the `pins` map. The message names `pins` as the one sanctioned way to fix a position |
| `node-cap` / `edge-cap` | refuse | more than 200 nodes / 600 edges, with the count |
| `steps-required` | refuse | more than 40 nodes and no `steps` reading order |
| `components` | warn, then refuse | disconnected components, listed by member; above six an apply refuses unless `--allow-disconnected`. Group membership counts as connectivity |
| `node-kind` / `edge-kind` / `edge-provenance` / `status` | refuse | a value outside a closed vocabulary, naming the whole vocabulary |
| `node-ref` | refuse | a required reference field missing, another kind's field present, or a MALFORMED address (a `..` path, a 0-based range, a context range that does not contain its primary, a non-`f-*` finding slug, a non-http `link`) |
| `edge-endpoint` / `step-node` / `pin-node` / `group-member` | refuse | a reference to a node that is not in the document, a self-edge, a duplicated step |
| `node-id` / `slug` / `title` / `body-cap` | refuse | a malformed or duplicated id, a reserved slug (`apply`, `sweep`), an over-long title, a body over 8 KiB |

A reference that does not RESOLVE is a warning, not a refusal: the node
becomes an honest orphan and the response says so. That split — "I cannot
read this" versus "I read this and it points at something that is gone" —
is the whole posture in one line.

**Status.** `pending` → `accepted` is a human's move (D21). `apply` may only
ever write `pending` (the default) or `draft`; `accepted` and `archived` are
reachable ONLY through `POST /api/boards/{slug}/accept` / `archive`, both
loopback-only. A document naming `accepted` is refused by the lint, so the
rule holds for a loopback caller too rather than resting on the gate. A
CHANGED apply against an accepted board resets it to the status the document
asked for and says `status_reset: true` — a human accepted a specific board,
not a slug.

**Exports.** `GET /api/boards/{slug}/export?format=md|jsoncanvas|kb-html`
(and `kb-code canvas export`). All three are pure functions of an
already-resolved board — they run no query and resolve no anchor — and all
three carry the same sentence: *an exported board is a SNAPSHOT; the live
board re-resolves on every read and this file does not*. **Markdown** is one
`##` per group, nodes in step order, each as its address, state, note and
snippet. **JSON Canvas** is the Obsidian 1.0 format, and it is lossy in a
stated way: the spec requires `x`/`y`/`width`/`height` on every node, so the
server derives a deterministic layered layout FOR THIS FORMAT ONLY (pins are
honoured exactly; everything else is laid out by longest-path depth over the
edge DAG, rows in step order, on `egoGraph.ts`'s own geometry constants) —
the same algorithm FAMILY as the SPA's engine, deliberately not a port, and
the file says which. Everything the spec cannot express (a range, a trust
class, a resolution state) rides `kbc_`-prefixed extension fields the spec's
own extensibility rule says other apps ignore. **kb-html** is the
high-fidelity one: a single self-contained artifact with a real `<title>` +
`<h1>`/`<h2>` hierarchy, stable section ids, `kb-category`/`kb-tags`/
`kb-summary` metas, addresses as reader deep links, every byte escaped, no
scripts and no event handlers. The `<template id="kb-prompt">` wrapper is
NOT generated here — the kb side owns it.

**`kb-code canvas sweep --check [--slug S]`**
(`GET /api/boards/sweep?repo=`) re-resolves every node of every (or one)
board and reports drift: orphans, carried nodes, query deltas, and stale
pins (a fixed position still held for a card that no longer points
anywhere). It NEVER mutates — a gate that repaired what it found could not
fail — and with `--check` it exits **3** on any drift, the documented
"well-formed response reporting a conflict with current state" rung of the
CLI's exit table. That is what makes a walkthrough board CI-gateable.

Storage is four new tables (`migrations/V0036__canvas.sql`), NOT a
`canvas_sets` row: that table's payload is opaque by contract and a board
must be re-resolvable, which requires structured rows. `canvas_sets` is
FROZEN beside it (the `/api/usages` → `/api/usages/2` treatment) and the
routes live under `/api/boards` because `GET /api/canvas/{id}` (an i64) and
`GET /api/canvas/{slug}` are the same axum pattern. kbc-seq/1's `board`
projection now resolves out of BOTH tables and stays a layer; a kbc-canvas/1
board reports a TRUE node count where a `canvas_sets` row still honestly
reports `null`.

Reads (`/api/boards`, `/api/boards/{slug}`, `/api/boards/{slug}/export`,
`/api/boards/sweep`) are ordinary `auth_bearer` and are declared as
`RouteContract`s in `kb_code_server::boards::V74_L1_ROUTES`, walked from
both the server and the CLI side by the dead-surface tests V71-G0 added.
Mutations (`apply`, `accept`, `archive`, `DELETE`) ride the same
loopback-only sub-router the review mutations do, so
`security::audit_mutations` records each attempt with its outcome. D10
sketches a later graduation onto a named-family
`[review] remote_mutations = ["review", "canvas"]` allowlist; that is its
own unit and nothing here weakens root invariant #4.

CLI: `kb-code canvas {boards,show,apply,accept,archive,rm,export,sweep}`.
`canvas list` keeps its pre-existing meaning — the v3.4-C1 canvas SETS — so
no script breaks; `canvas boards` lists kbc-canvas/1 boards.

**The SPA (V74-L2).** `/r/{repo}/~boards` is the list (status filter, per-board
counts, a "check drift" panel over `GET /api/boards/sweep`) and
`/r/{repo}/~boards/{slug}` renders one board. `~canvas` — V3.4-C2's
working-set canvas — stays mounted and FROZEN beside it, linked once from a
"Legacy" section at the bottom of the list with the difference stated: a
`canvas_sets` payload is opaque by contract and cannot be re-resolved, which
is the whole point of a board. Nothing was migrated and neither page reads the
other's data.

Layout is `web-code/src/lib/boardLayout.ts`, which is an ADAPTER over
`egoGraph.ts`'s `layoutLayeredDag` — the one engine (D10) — plus two things
the engine has no notion of: card-sized pitch, and pins. A pinned card keeps
its authored position exactly and is not flowed, mirroring the server's
export-only `boards::layout::place`. A `code` node renders through the SAME
`LiveRefCard` the review document uses (server spans, state badge, fold), so
there is one live code card in the SPA rather than two that could disagree
about what `carried` looks like; every other kind gets an address card that
shows the daemon's own `address`, `state` and `reason`. `?ctx=1` fetches the
± context expansion (never synthesised), `?live=1` re-runs the query cards
under a caption, and `?step=` is the walkthrough's position — Location
Contract params, appended last, each omitted at its default and each with a
total parser (`lib/boardsUrl.ts`).

Keys: nineteen `scope: "board"` rows gated `when: board == boards` (the
`board` context key gains a fifth value), split into a reading half
(`!walkthrough`: `j`/`k`/`Enter`, `z c`/`z o`/`z a`/`z M`/`z R`, `+`/`-`,
`Space b {t,p,u,A,s}`) and a walking half (`walkthrough`: `n`/`k`/`p`) that
`commands doctor` can prove disjoint; plus `Space g w` (`nav.boards`) and
`Space b a` (`boards.add`). `Escape` leaves the walkthrough through the
existing `dismiss.mode` rung — no new Escape row.

Add-to-board is an actions/1 row, `collect.board`, on all five target kinds.
It is a `mut_spec`, so a caller the server has not cleared for mutations never
sees it (rule 2: ABSENT, not disabled) — board mutations are loopback-only.
The SPA composes the WHOLE next document from the board already on the wire
plus one node (`web-code/src/lib/boardDoc.ts`; there is no partial-patch
route), checks itself against the lint's own `coordinates` rule before
sending, and renders a refusal's findings with their rule ids verbatim.
Node threads reuse the annotations store: the first comment creates an
ordinary annotation at the node's anchor and the board records its id through
`apply` — if that second write is refused the comment survives and the panel
says only the LINK could not be recorded. A node with no path and no line
(a `note`, a `turn`) is told it cannot carry a thread rather than being given
an invented anchor.

**kbc-tour/1 (V74-L3b) — a tour IS a board whose nodes are its steps.**
Design of record: D12 (tours and trails) + D10 ("a board's `steps` and a
tour share the step model — do not build two"), Track L. A **tour** is an
ordered walk through references this daemon already serves, each step
carrying prose, a blob and a camera, every reference re-resolved through the
Ladder on every read — which is, word for word, a board.

So kbc-tour/1 creates no table. A tour is a `canvas_boards` row with
`kind = 'tour'` (migration V0039): its steps are `canvas_nodes`, their order
and cameras are `canvas_steps`, and consecutive steps are joined by
generated `then` edges. That is invariant 9's ruling ("a `reading_sets` row
with `kind = 'workspace'` IS a workspace — not a new entity") applied one
table over, and it is what "do not build two" asks for at the strongest
reading: ONE lint (`boards::lint::check`, which the tour lint extends rather
than duplicates), ONE resolver (`boards::resolve::resolve_node`), one
walkthrough contract, one `?step=` grammar, one snippet-capture rule. A tour
and a board share `canvas_boards`' `UNIQUE (repo_id, slug)`, so they share
one slug space in a repo — an apply that would collide across the two
families gets a `409` NAMING the family that holds the slug, never a raw
constraint error. Neither ever appears in the other's list, and each 404s on
the other's route.

A tour adds exactly three things on top of a board, and each is additive.
**`camera`** — per-step `{fold, context}` hints on `canvas_steps.camera_json`,
with NO coordinates: `boards::lint`'s `coordinates` rule runs over a tour
document unchanged, so geometry cannot be smuggled in through a camera any
more than through a node. **The `ref` sugar** — a step may name its target
with a kbc-review/1 ref string instead of the structured reference fields,
parsed by `review_doc::refs::parse_ref` (the ONE ref parser) and lowered to
the same `RefFields` a board node carries. It covers `code:` refs *that cite
a line or a range*, and every other scheme is refused with the structured
field that does the job named in the refusal: a whole-file `code:` gives the
Ladder nothing to carry; `sym:`/`ent:` would require resolving a symbol to a
path and PERSISTING that derivation, which is the cached-class this crate
refuses; `hunk:`/`finding:` carry no review id; `gh:`/`kb:` are inert links
that belong in the step's prose. **The linear structure** — steps ARE the
nodes, and the `then` chain is generated, so a tour lints as one connected
component on its merits rather than by passing `--allow-disconnected` (which
would also suppress a real problem on a board).

`kb-code tour pack <slug> --budget N` (`GET /api/tours/{slug}/pack`) is the
budgeted context pack. Steps are emitted in walk order until the next one
would cross the budget; every step that did not fit is COUNTED and NAMED, and
the drop is a SUFFIX — a pack with a hole in the middle would read as a
shorter tour rather than a truncated one. A step is emitted whole or not at
all (half a snippet reads as complete), and an ORPHAN step is emitted with
its prose and its last-known address and no snippet, because there is
nothing honest to show. The rendering is the same function the `md` export
uses, so a pack is literally a prefix of the export.

Exports are `md` and `codetour`, both SNAPSHOTS that say so with the
revision and the resolution counts they were taken at. **CodeTour** (the VS
Code `.tour` format) is additionally LOSSY and lists every loss in its own
`kbc_lossy` array inside the file rather than in a doc nobody re-reads: the
blob per step (CodeTour anchors on a line or a text pattern and has no
notion of the bytes an author read), the resolution state, every camera, and
every step that does not address a file and a line — those are omitted with
a count rather than approximated.

Reads (`/api/tours`, `/api/tours/{slug}`, `…/pack`, `…/export`) are ordinary
`auth_bearer` and are declared as `RouteContract`s in
`kb_code_server::tours::V74_L3B_TOUR_ROUTES`. `POST /api/tours/apply` and
`DELETE /api/tours/{slug}` ride the same loopback-only sub-router the board
mutations do; `accepted`/`archived` stay reachable only through the BOARD
transition routes, so D21's pending-until-a-human-accepts rule holds
unchanged. CLI: `kb-code tour {list,show,apply,pack,export,rm}`.

**kbc-trail/1 (V74-L3b) — the navigation record, OFF by default.**
Design of record: D12 + **D17** ("Attention, not comprehension") + the
Security posture's §Privacy line. A **trail** is an ordered list of PLACES
the operator went, one dwell number each, recorded only after an explicit
opt-in, shown to the operator in full, shown to an agent only in AGGREGATE,
purgeable wholesale, and aged out by a retention window.

D17 does not say "server-side trails, with purge to follow": it says
server-side "only in the milestone that ships pause, purge and retention
together". A ledger of where a person looked with no way to stop it and no
way to delete it is a different product from the one this design describes,
so the three controls are not features layered on a store — they are the
precondition for the store existing at all, and they ship in the same file
set as the ingest route.

**Two switches, and they mean different things.** `[trails] enabled` in
`kb-code.toml` (**default `false`**) is the operator's master switch: with
it off nothing is recorded, no mode transition is possible, and the
retention sweep starts no task. The runtime mode — `off` | `recording` |
`paused` — is a separate, persisted decision in `trails_state`, changed only
by the loopback-only, audited `POST /api/trails/state`. A fresh volume reads
`off` even with the config `true`, because the absence of a decision is not a
decision to record. A write refusal always names WHICH gate it hit
(`trails-disabled`, `trails-off`, `trails-paused`): "nothing is being
recorded" has three different fixes and a generic 403 names none of them.

Four properties are structural, not promises:

| property | how it is enforced |
|---|---|
| nothing finer than a STEP can be stored | `trail_steps` is the finest row in the schema and holds ONE dwell number. `trails::reject_sub_step_keys` refuses a payload naming a viewport, a caret, a scroll offset or a per-line dwell — BY NAME, on the raw JSON, before the typed parse, so the refusal teaches the rule (the `coordinates` precedent). A sub-step span is REFUSED, never rounded |
| dwell is DERIVED and QUANTISED | the client sends `entered_at`/`left_at`; the server computes the difference, floors it to `[trails] step_granularity_secs` and clamps it. There is no client-supplied dwell field, so "never finer than dwell-per-step" is a property of the wire |
| the agent-facing read is aggregate, and its only time key is the DAY | `GET /api/trails/aggregate` groups by `(path, symbol)` and reads `trail_steps.day`, never `entered_at`; its row struct has no timestamp field to leak one into, and its `?since=`/`?until=` window is a `YYYY-MM-DD` grammar for the same reason |
| no trail number is a score, a gate or a ranking term | a source scan (`trails::tests::no_ranking_module_imports_the_trail_ledger`) fails BY FILE if any ranking module in this crate so much as names `crate::trails` — `kbc-claim/1`'s own pin, which is root invariant #10's surfaced-never-scored law |

Typed hops are CLOSED at eleven (design §P7 verbatim): `search`,
`definition_of`, `usage_of`, `caller_of`, `blame`, `why`, `story`, `review`,
`framework`, `manual`, `agent_suggested`. A RECORDED trail is minted per repo
per UTC day (or by an explicit fork); an AUTHORED one is laid down whole by
an agent for the human to walk. `POST /api/trails/{id}/fork` carries a COPY
of the steps from its branch point, so a fork reads as "I was here, and then
I went another way" rather than as an empty trail with a pointer; parent and
fork may each be purged without the other.

A step's state is computed on every read and stored nowhere, and it is
deliberately the SHALLOW end of the Ladder: `pinned` (the blob is still the
blob), `carried` (the file is there under different bytes), `orphan` (the
path is gone), `inert` (there was no path). A trail step is a place someone
went, not an anchored claim about specific bytes, so re-anchoring its line
into a changed blob would invent precision the record never had.

**The two HUMAN reads are loopback-only** — `GET /api/trails` and
`GET /api/trails/{id}` return the operator's own movement record, and the
design's "read-tracking never leaves the operator's box" makes that a
stricter class than an ordinary bearer read (invariant 23(b)'s reasoning,
applied to attention data). `GET /api/trails/state` is `auth_bearer` because
D17's indicator is mandatory and an indicator that cannot render is not an
indicator; it reports `mutable: false` for a caller that could not change the
mode anyway, so the SPA hides the control rather than offering a button that
403s. `GET /api/trails/aggregate` is `auth_bearer` because it IS the
agent-facing surface.

`POST /api/trails/purge` (loopback-only, audited) is WHOLESALE by default —
the point of a purge is that it leaves nothing behind — with `?before=` for a
window and `id` for one trail. It is deliberately NOT gated on `[trails]
enabled`: an operator who has just turned the feature off must still be able
to delete what it recorded while it was on. It removes trails and their
steps; an `annotations` row carrying a `trail_id` SURVIVES, because a dissent
note on an authored trail is the human's own words and invariant 23(a) rules
that authored content is not derived data. Retention is
`[trails] retention_days` (default 30, `0` = the operator's explicit keep
everything, surfaced in `/state`) swept by a paged background task on V72-B0's
shape: spawned and never awaited, `TRAIL_GC_PAGE` trails per short
transaction, a yield between pages, and no task at all when nothing can
expire.

Dissent notes REUSE the annotations store rather than growing a second
comments table (D10's node-thread ruling, one layer over): `annotations`
gains a nullable `trail_id`, a reply inherits it through the SAME
`inherit_scope_field` ladder `review_id`/`set_id` already use, and
`kb-code trail notes <id>` drains them. CLI: `kb-code trail
{state,list,show,aggregate,purge,notes,fork,author}`; `purge` requires
`--yes`.

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

**comments/1 (V72-J1, D8) — `GET /api/comments?repo=[&path=][&kind=]
[&keyword=][&state=][&limit=][&offset=]`, `GET /api/comments/file?repo=
&path=`, `GET /api/comments/summary?repo=`, `GET
/api/comments/keywords`.** ONE scanner over every comment in a repo,
classified into eight kinds — `doc` (a block immediately above a
definition, with the definition it documents), `annotation` (a keyword
hit), `directive` (a tool pragma: `rubocop:disable/enable/todo`,
`frozen_string_literal`, `typed:`, RBS `#:`, `eslint-disable`, `@ts-
expect-error`, `noqa`, `type: ignore`, `go:build`, `shellcheck`, …, with
the tool named and, for a SUPPRESSION only, whether it carried a
reason), `section` (a banner or fold marker), `licence` (a head-of-file
SPDX/copyright block), `generated` (`# == Schema Information` and other
"do not edit" markers — **never** `doc`, whatever it sits above),
`commented_code` (the run re-parses cleanly in the file's own grammar)
and `prose`. Adjacent same-kind lines form one block with a range;
`annotation` and `directive` are the two exceptions, one block per line,
because each carries its own identity. The keyword grammar is
CONFIGURABLE (`[comments] keywords`, an override REPLACES the default
set) and defaults to RuboCop's six — `TODO FIXME OPTIMIZE HACK REVIEW
NOTE` — plus `XXX`/`BUG`; a smart_todo parenthetical `TODO(on:
date('2027-09-01'), to: 'someone')` is parsed into typed fields (an OPEN
`key: value` bag, so `by:` and any future key round-trip) while staying
verbatim in the annotation's own text. **`state` is computed per request
and persisted nowhere**: a `doc` block is `fresh`, `drifted` (with
`age_days`, `code_commit` and `doc_commit` — the arithmetic, never a
verdict about whether the comment is wrong; that judgement is an
agent-layer step) or `unknown` WITH ITS REASON (`uncommitted`,
`blame-budget`, `blame-unavailable`, `no-documented-symbol`); an
annotation with a past `on: date(…)` is `aged`; a suppression with no
`--`-separated reason is `unreasoned`; everything else is `none`. Every
bound is in band — `scan` (rows examined, the true `rows_matching_
filters` count, the cap, whether it was hit) and `blame` (files blamed,
files wanted, the `MAX_BLAMED_FILES` budget, `exhausted`) — and a row the
blame budget could not reach is reported `unknown`, never quietly
`fresh`. `summary` counts kinds and keywords EXACTLY (whole-repo `GROUP
BY`s) and covers only the two blame-free state lanes, naming the three it
excludes and why rather than sampling them. **`GET /api/todos` is now a
filtered VIEW over this index** (`kind=annotation` and a keyword in the
legacy five-marker family); `todo_items` and the old marker scanner are
gone, so there is exactly one scanner over these lines. Its response
struct, ordering, limit/truncation and scope semantics are byte-identical;
its ROW SET changes in exactly two enumerated ways — outline-tier
languages (YAML/TOML) are now scanned, and a two-marker line reports the
LEFTMOST keyword rather than the first entry of a hardcoded array. Source
is never mutated: a change to comment text rides the EXISTING suggestion +
apply path. CLI: `kb-code comments list|show <PATH:LINE>|file|summary|
audit|keywords --repo R [--path P] [--kind K] [--keyword K] [--state S]
[--limit N] [--offset N] [--json]`, where `audit` is the actionable slice
(drifted docs · aged annotations · unreasoned suppressions) in a compact
form an agent can act on.

**comments/1 in the SPA (V72-J2, D8).** A per-file comment gutter (kbc-
theme/1 Lane Budget's fourth `editor/lineGutter.ts` slot, alongside blame/
annotations/diagnostics) with three modes (`all`/`quiet`/`doc-only`,
`Space C c`); a `~comments` dashboard (`Space g m`) defaulting to the
actionable slice as three server-paged lanes (mirroring `comments audit`'s
own three requests) with kind/keyword/path facets and a "show everything"
toggle; the existing identifier hover tooltip gains an additive freshness
caption + YARD-vs-signature disagreement chip when a `doc` row documents
the hovered symbol; and the claim → annotation bridge
(`comments.track-as-annotation`, `Space C t`) mints an ordinary annotation
via the existing `POST /api/annotations` path with the ONE new
`intent: "claim"` value (`annotations::INTENT_CLAIM` — a route-boundary
string, no migration), joined back to its source comment by live line
(four derived states: open/tracked/resolved/gone). `GET /api/todos`'s
`~todos` page is UNCHANGED (own URL, own specs) and links forward to
`~comments` as the richer surface rather than being redirected — see
`web-code/CLAUDE.md`'s "Comments/1 in the SPA" section for the full
surface-by-surface rationale (gutter-slot decision, mode semantics, the
bridge's no-tracker-semantics rule, registry rows).

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

Not in that unit, by design: new grammars (SCSS/CSS/Markdown), the
injection-aware pipeline, the universal `outline/1` contract, and the
`symbol_salt`/`highlight_salt` split. `highlight_only` therefore shipped as
a mechanism with no production row (SCSS became the first, in V72-H2a); the
salt split is V72-H2b, below.

**The salt split, the highlight cache gate and the eighteen roles
(V72-H2b, D7 + D16).** One salt used to key every derived row per file, so
a highlight-query or role-table change re-extracted SYMBOLS too, and a
`tags.scm` fix re-painted the corpus. `crates/kb-code-server/src/lang.rs`'s
`LangInfo` now carries two, and each derived family is invalidated on its
own:

| salt | format | keys |
|------|--------|------|
| `symbol_salt` | `{id}@{grammar}+qN` | `symbols`, `occurrences`, `import_specs`, `call_sites`, `type_relations` |
| `highlight_salt` | `{id}@{grammar}+hN+rolesM` | `highlights` |

`M` is `highlight::ROLE_TABLE_VERSION`, and a test pins every language's
salt to it — widening the role vocabulary is one edit that invalidates
every painted row and cannot be forgotten for one language. The two sets
are disjoint by construction (`+q` vs `+h`), which is what lets the store
keep asking "is this row's salt in the CURRENT set" per FAMILY with no join
back through `files.lang`. The V70-A3X stale-salt sweep runs one pass per
`(table, family)` pair (`store::SWEEP_TABLES`), so a family is stale only
when ITS OWN salt moved; the V72-B0 completion marker folds BOTH sets into
its fingerprint, so a highlight-only bump re-arms it. **No migration was
needed for the split** — two salt strings live in the same `salt` TEXT
column their family's table already had.

The **highlight cache gate** is the second half. Highlights used to be
written only inside the symbols cache-MISS branch, so the two could not be
invalidated separately even in principle. They are now two independent
gates in `ingest::index_file`, and both ask a new question: `derived_status`
(migration V0037 — a new migration takes the embedded set's current max +
1, never a reserved slot: refinery's `abort_missing` makes a gap-FILL a
hard boot refusal on any volume already migrated past it, which
`embedded_migration_versions_are_contiguous` now prevents) records
`(blob_hash, family, salt) → rows`, and the gate
is the row's EXISTENCE. The old gate was `COUNT(*) > 0` over the derived
rows themselves, which cannot tell "not derived" from "derived, and zero
rows was the honest answer" — so every zero-symbol file re-parsed on
**every visit, forever**: ERB templates (tier `none`, the case V72-H1
reported), SCSS files (tier `highlight_only`), comment-only Rust files,
heading-less Markdown. `GET /api/file` gains an additive
`highlight_cache: "hit" | "miss" | "skipped_tier"` reporting what the gate
would say for that blob, and the boot walk logs `highlight_hits` /
`highlight_misses` / `highlight_skipped` beside its existing counters.

The **role table** widens from fifteen classes to **eighteen** (D16), which
is what the split makes affordable. The three additions were picked by
counting the capture names the grammars in this build actually emit, and
the counts are asserted, not narrated:

| role | capture names | grammars |
|------|---------------|----------|
| `constant-builtin` | `constant.builtin`, and YAML/TOML's top-level `boolean` | 9 |
| `punctuation-special` | `punctuation.special` | 7 |
| `string-special` | `string.special{,.regex,.symbol,.key}` | 7 |

`map_class`'s lookup is now two levels deep — the first two dotted scope
words, then the top-level word — so every unlisted sub-scope keeps exactly
the class it always had (`comment.documentation` is still `comment`,
`string.escape` still `string`). Measured and NOT adopted, recorded by a
test with their evidence: `constructor` (6 grammars), `variable.parameter`
(5), `type.builtin` (3), `namespace` (0 — no grammar in this build emits it
at all; CSS's `@namespace` is an at-rule KEYWORD in the query's pattern
text, not a capture name), Markdown's `text.*` family (1 grammar, 4
captures). The wire is kebab-case (`rename_all` moved from
`snake_case`, byte-identical for all fifteen legacy single-word names), and
the SPA mirrors the list in three places kept in lock-step by test:
`api/types.ts`'s `HighlightClass`, `themes/derive.ts`'s `SYNTAX_ROLES`, and
`styles/tokens.css`'s `--syn-*`. Registry themes give each new role its own
hue; the built-in palette carries seven chrome hues rather than eleven, so
there each new role defaults to its parent's token and the widening renders
byte-identically until a theme separates it.

**The re-extract bill — `GET /api/reextract/bill?repo=[&sample=]`,
`kb-code reextract --bill [--repo R] [--sample N] [--json]`
(`reextract-bill/1`).** D7 asks for the cost of a salt bump to be MEASURED
and recorded per milestone, so the bill has three parts and labels each
with how it was obtained. (1) The **census** is exact: files and bytes per
`files.lang`, and rows per derived table over the blobs this repo reaches
— paged and wall-clock-budgeted for the V72-B0 reason (the un-paged
`blob_hash IN (SELECT …)` shape is O(every live blob) random seeks per
table with the store's single connection mutex held), and it says
`census_complete: false` rather than presenting a partial count as a total.
(2) The **timed sample** is measured: up to 200 files per language
(deterministically the first in `path` order, so two runs compare), read
from the working tree and pushed through the REAL
`extract::extract_symbols` / `highlight::extract_highlights` with the
results discarded — the bill writes nothing, because a bill that mutated
the cache it prices would be measuring its own second run. (3) The
**projection** is extrapolated and says so on every row: the sample's
milliseconds scaled by `total bytes / sampled bytes`, with the scale factor
printed. There is deliberately no verb that PERFORMS a re-extract: bumping
a salt is an edit to `lang.rs` plus a deploy, and the mirror re-derives
itself through the ordinary two gates on the next visit to each file.

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

**The `~rails` surface (V72-I2).** `/r/:repo/~rails` is the dashboard over
that wire — a repo-scoped sentinel page beside `~todos`/`~workspaces`, not a
new shell. It shows the passport (detection, version and its source, the
TRUE per-noun totals, lens freshness, the Zeitwerk read state, the read
state), then one section per noun with a card per row: the model's table
name and its association/validation/scope/callback counts, a route's
verb+path and the action it reaches (flagged when it reaches none), an
action's visibility, a job's enqueue-site count, a view's inbound render
count, a concern's includers. Each card is ONE address the reader opens, a
set of facet chips that pre-fill the search box with the matching `kbcq/1`
atom (`model:`, `controller:`, `action:`, `route:`, `job:`, `rails:<noun>`),
its witness count, and a trust LINE STYLE — dashed for `likely`, dotted for
`candidate`, never solid, because `rails/1` has no exact tier. Paging and
the `q=` filter are the SERVER's, so the "showing 1–25 of 312" caption is
always true. The orphan report renders every lane with its own `why`
verbatim and the report's caption above them, and a row that appears in a
lane gets a badge on its card naming the lane.

Two reader surfaces come with it. A model file carrying an annotaterb
`# == Schema Information` banner gets a **Schema card** in the inspector
rail — the column table, parsed in the browser out of the text `GET
/api/file` already returned, captioned as a client read rather than a daemon
fact (`comments/1`'s `GET /api/comments/file` landed on main mid-unit and is
the named follow-up source for the block's RANGE) — and the banner itself is FOLDED in the buffer behind a one-line
placeholder (a browser-local opt-out pref, `Space z`, or the card's own
button). Hovering (`K`) a line that produced rails-lens edges adds a **Rails
atom table** to the hover card: the association/render/i18n/enqueue targets
on that line, each with its own trust and address, plus the `kbc-actions/1`
rows for the target, rendered whole in the daemon's order. Nothing on either
surface auto-navigates. A route helper (`orders_path`) is shown as a SEARCH
over the route index rather than a resolved target — `rails-lens/1` mints no
route-helper edge, and the card says so instead of inventing one.

**HAML in the lens, verified (V72-I2).** The `.haml` dispatch was already
correct at V72-I1 — `rails::extract` has its own HAML arm,
`rails_lens_relevant_path` lists both `.haml` predicates, `lang::detect`
resolves `.haml` to the first-party scanner, and `find_view_files` matches a
template by its STEM rather than its extension. What was missing was a test
that any of it survives the real pipeline: every fixture running through
`ingest::index_file` → `replace_rails_edges` → `GET /api/rails/*` was 100%
ERB, and the only HAML lens assertions call `frameworks::extract_edges`
directly, bypassing `is_rails`, the store and the daemon. The `acme-app`
fixture now ships a `.haml` view (a partial render, a lazy `t(".key")`, a
route helper) and `tests/rails_route.rs` asserts through the SHIPPED path
that the partial rendered from HAML is not in the `view_never_rendered`
orphan lane and that a locale key referenced only from HAML is not reported
unused — with the genuinely-unused key as the negative control, so an
emptied lane cannot pass either.

CLI: `kb-code rails {home,models,controllers,actions,routes,jobs,mailers,views,concerns,orphans}
--repo R [--q TEXT] [--limit N] [--offset N] [--json]`. The `--json` form is
the standard envelope with `schema: "rails/1"`; the text form is compact
enough for an agent, and every number it prints is the daemon's own.
**`aug-lane/1` (V72-H4a, D7 / §P8) — augmentation lanes.** A **lane** is a
source of facts about code that kb-code did not derive from its own
tree-sitter pipeline: the repository's git history, a coverage run, a
linter, a SARIF-emitting scanner. One registry
(`crates/kb-code-server/src/lanes/mod.rs`), one fact store (`lane_facts` +
`lane_runs`, migration V0030), one classing function, one CLI.

Three rules hold the whole thing up.

*A lane is enabled ONLY by config.* `[lanes] enabled = ["git.behavior", …]`
in `kb-code.toml` — never by a route, never by a request, and never by a
file inside a repository (a committed `.kbc/lanes.toml` would be remote
code execution by `git clone`). The default is empty: a daemon that says
nothing about lanes has every lane off and every other response
byte-identical. `[lanes.retention_days]` overrides a lane's registry
default.

*The daemon runs no tool.* The registry has two kinds. A `derived` lane is
computed by the daemon from git and the mirror alone; an `ingested` lane's
facts arrive as a `lane-ingest/1` POST from `kb-code lanes ingest`, which
ran the tool on the operator's own box, over the **loopback-only**
mutation lane (beside `checkout` and apply-suggestion, and in the V0027
mutations ledger). There is no third path and no flag that creates one.

*The trust class is computed per request and never persisted.*
`lane_facts` has no class column. `lanes::classing::class_for` is the one
function that turns a stored claim into a class, from `min(lane ceiling,
per-fact cap, anchor state)`:

| condition | class | `reason` |
|---|---|---|
| the path is not readable in the repo | `orphan` | `path-gone` |
| blob == current, `sha_source = tool` | `exact` | `blob-current` |
| blob == current, `sha_source = mirror_at_ingest` | `likely` | `blob-current-sha-attributed` |
| blob moved, file-level fact | `likely` | `file-level-blob-moved` |
| blob moved, content unreadable | `orphan` | `content-unreadable` |
| blob moved, no stored snippet | `orphan` | `no-snippet` |
| blob moved, snippet found verbatim | `likely` | `reanchored-exact` |
| blob moved, only a fuzzy match | `candidate` | `reanchored-fuzzy` |
| blob moved, nothing matched | `orphan` | `no-anchor` |

Re-anchoring reuses the ONE carry-forward Ladder this daemon already has
(`annotations::anchor_for_line` + `annotations::resolve` +
`review_comments::line_matches_snippet` — the same three calls review
comments and findings make), and the snippet it re-resolves is captured at
ingest only when the fact's blob is what is on disk at that moment: this
daemon cannot read bytes it does not have, so a fact about some other blob
carries no snippet and becomes an honest orphan rather than a manufactured
match. `sha_source` is the honesty bit that makes `exact` reachable at
all — a blob the daemon *attributed* at ingest is not a blob the tool
*named*, and reading it back as `exact` would be a wrong `exact`.

**Routes.** `GET /api/lanes[?repo=]` (registry + enablement + counts +
last ingest, and any `[lanes] enabled` id that matches no row, named);
`GET /api/lanes/facts?repo=&path=[&lane=][&at_blob=]` (facts with the
class computed NOW, the re-anchored line, the age and the run provenance;
enabled lanes with nothing to say are listed in `absent` with the command
that would produce facts); `GET /api/lanes/summary?repo=` (per
lane/kind/severity counts over a stated bound — counts of stored claims,
carrying no class, because a class is per path per request);
`POST /api/lanes/{lane}/ingest?repo=` (loopback-only). Ingest refuses
rather than truncates (413 with the counts), refuses a path outside the
repository **by row** while the rest of the batch lands, and REPLACES the
lane's facts for every path the batch names plus every path in
`clear_paths` — which is how "the offense was fixed" is expressible at
all.

**The four lanes.**

- **`git.behavior`** (derived) — `churn` (non-merge commits and distinct
  authors in the last 90 days), `co_change` (files sharing ≥2 of those
  commits, top 10 with the true total beside them) and `last_touch`.
  Computed on demand through `history::run_git_raw` and memoised on
  `(repo, HEAD, path)`; nothing is stored. Agent authorship comes from the
  commit's official `Kb-Session:` trailer block: present is proof, so the
  fact can reach `exact`; **absent is not proof of a human**, so that fact
  caps itself at `likely`.
- **`coverage.simplecov`** (ingested) — `kb-code lanes ingest
  coverage.simplecov --repo R --file coverage/.resultset.json
  [--strip-prefix /app]`. Both resultset shapes; several suites merge by
  summing hits. One `coverage` fact per relevant line (`{hits}`) and one
  `coverage_summary` per file (`{covered, total, pct}`, `pct` null rather
  than 0.0 when a file has no relevant line).
- **`rubocop`** (ingested) — `--file rubocop.json` (`rubocop --format
  json`) → `{cop, message, correctable, corrected, severity_raw}` with the
  severity normalised into `error|warning|info|hint` and RuboCop's own word
  kept. Also `--from-lip --paths a.rb,b.rb`, which pulls the daemon's
  kb-lip diagnostics; those facts carry a **tool-named** blob, because
  lip's own pre/post blob guard proved it — and the CLI brackets the whole
  round with its own before/after read of the same blob, since lip's guard
  covers the LSP call and not the gap between two of the CLI's HTTP
  requests. A file that moved mid-round is skipped with a stated reason.
- **`sarif.*`** (ingested) — ONE generic SARIF 2.1.0 adapter and a lane
  per tool: `--lane sarif.brakeman`, declared in `[lanes] enabled`. The
  `sarif.*` row is a template and is not itself addressable. Results with
  no location, regions with no `startLine`, and artifacts outside the root
  are named skips, never guesses.

**CLI:** `kb-code lanes list|facts|summary|ingest`, each with `--json`.
`ingest` parses on the operator's box and posts in `--batch-size` chunks;
`--dry-run` prints the run summary without posting.

Retention is a paged background sweep (`lanes::gc`) on the V72-B0 shape:
spawned before the bind and never awaited, one short transaction per page,
recomputed cutoff per page so an interrupted pass simply resumes. A
disabled lane is never swept out from under a re-enable.

Not in this unit, by design: the SPA's Facts gutter, rail and hover
(H4b); any lane beyond the four; the retrofit of kb-lip, rails-lens and
the DCB doc-lens as lanes; and LLM-produced facts, which this daemon has
no place for at all.

**V72-H2a (D7) — three grammars, one injection layer, `outline/1`.**

*New languages.* `css`, `scss` and `markdown` join the `syntax/1` registry,
all three CST-WALK outlines rather than `tags.scm` query languages (the
model `yaml`/`toml`/`json` already used — none of the three grammars ships
a tags query, and none of the three languages has a definition vocabulary
one could target). `css` and `markdown` are tier `full`; **`scss` is tier
`highlight_only`**, the first production row for the mechanism V72-H1
shipped, and the reason is a grammar fact rather than a scope decision —
see "The HIGHLIGHT_ONLY ledger" below. `.md`/`.markdown` were `unknown`
before, i.e. prose the instrument silently ignored.

- **CSS/SCSS** (`crates/kb-code-server/src/css.rs`) mint a documented
  seven-kind vocabulary: `rule` (a selector list), `placeholder` (an SCSS
  `%name` whose whole selector list is one), `at_rule` (`@media`,
  `@supports`, `@use`, `@import`, `@forward`, `@charset`, `@namespace`,
  `@at-root`, and any other at-rule — named by its header text),
  `keyframes`, `mixin`, `function` and `variable` (an SCSS `$name:` or a
  CSS custom property `--name:`). An ordinary declaration is deliberately
  NOT a row — a real stylesheet has thousands and an outline that lists
  every one is a re-print of the file. SCSS control flow (`@if`/`@each`/
  `@for`/`@while`/`@include` with a block) is not a row either, but the
  walk descends THROUGH it, so a rule written inside an `@each` still
  appears. `detail` carries the full header for rows whose name is a
  fragment of it (`@mixin button($size: md)` for the mixin `button`).
- **Markdown** (`src/markdown.rs`) mints one `heading` row per heading
  SECTION — the row's line range covers the whole section, not the heading
  line, which is what lets `outline/1`'s containment nesting reproduce the
  document's own structure; `detail` is the level, `h1`..`h6`. Fenced code
  blocks are injection HOSTS, not rows. **Links are deliberately not
  minted as candidate refs**: kb already extracts exactly that class of
  doc→code hint corpus-side (`kb_core::coderefs`, root invariant #2), and
  a second kb-code-local extractor over the same prose would be a
  competing lane with its own drift.
- **YAML gains D7's reuse facts.** An anchor (`&name`), an alias (`*name`)
  and a merge key (`<<: *name`) ride the key row's `signature`, which is
  already on `GET /api/file`, `GET /api/symbols` and the `outline/1` row's
  `detail`. No new rows and no new kinds — an anchor is a property OF a
  key. They are surfaced, never RESOLVED: `*defaults` naming `&defaults`
  would be a `usages` claim, and YAML has no occurrences index, so the
  Parity Grid says `no` there and this lane must not contradict it. (The
  schema-hover half of D7's YAML item is kb-lip's, not this unit's.)

Grammar crates, all MIT: `tree-sitter-css 0.25.0`, `tree-sitter-scss
1.0.0`, `tree-sitter-md 0.5.3` (its BLOCK grammar — the crate also ships an
inline grammar, which kb-code does not register, so inline emphasis/link
highlighting is not claimed). Each is pinned EXACTLY; `tree-sitter-scss` is
the one old-style binding in the set (it depends on `tree-sitter` itself
rather than the ABI-stable `tree-sitter-language`), and the root
`Cargo.toml` records why that is safe and how it would fail loudly.

*The injection layer* (`src/injection.rs`) is the ONE place a host
language's embedded guest code is located, replacing the two unrelated
mechanisms that existed before (ERB's per-directive CST walk, HAML's
whole-template synthesized Ruby program). A `Region` carries its guest
language, the host byte range it came from, the guest source, and an
`OffsetMap` — `Shift` for a contiguous slice (both byte offsets and rows
map back) or `Lines` for a REASSEMBLED program (only rows map back, and a
`None` entry is a line kb-code invented). Three hosts: `erb` → ruby,
`haml` → ruby (fragments plus the one program), `markdown` → whatever a
fence's info string resolves to. **HTML is not a host** — kb-code links no
`tree-sitter-html` (the Stimulus scan regex-scans ERB's raw `content`
nodes instead), so `<script>`/`<style>` injections do not exist here and
the docs say so rather than implying otherwise. Fence languages resolve
against the registry itself (a row's `lang` first, then its EXTENSIONS),
so ```` ```rb ````, ```` ```yml ```` and ```` ```ts ```` work with no alias
table; a fence naming something nothing parses, and a fence whose body is
not a contiguous slice (inside a block quote or a list item, where `> `
markers interleave), yield no region and are left unpainted rather than
mis-offset. Painting runs the guest's HOST-ONLY highlighter, so it is
bounded at exactly one level by construction — no depth counter to get
wrong. `GET /api/syntax` gains a DERIVED `injections: [lang_id…]` per row,
and a test pins the registry's `injection_host` flag against the layer's
own `is_host`. **ERB and HAML rails-lens output is byte-identical** (their
goldens are the proof): the walks moved, the edge minting did not.

*`outline/1`* — `GET /api/outline?repo=&path=[&ref=]`, `kb-code outline
<PATH> --repo R [--ref REF] [--json]`. One per-file outline contract for
every registered file type: `{schema, repo, path, ref, lang, tier, rows,
total, truncated, honesty}`, each row `{kind, name, range{line_start,
line_end, col_start, col_end}, depth, detail?, children[]}`. Row kinds are
the SYMBOL kinds, verbatim — this contract invents no vocabulary: Rust/
Python/Ruby/JS/TS/Go/Bash items from `tags.scm`, `key` for YAML/TOML/JSON,
`rule`/`at_rule`/`mixin`/… for CSS/SCSS, `heading` for Markdown,
`element`/`filter` for HAML.

It is a **VIEW of the symbol rows, never a second extraction**: the rows
come from exactly the store lookup `GET /api/symbols` does — same blob,
same salt — so the two can never disagree. Nesting is RANGE CONTAINMENT
(the rule `entities/1` already uses), not name matching, so a file with two
identically-named symbols nests correctly. `honesty` names the tier, the
engine (`tree-sitter:<crate>` / `scanner:haml/1` / `none`), what the rows
were derived from, and — when there are none — WHY, so the four shapes a
caller can hit are all distinguishable: rows; a `full`-tier file nothing
has indexed yet; a `highlight_only`/`none` tier (a design outcome); an
unregistered file type. Nothing is persisted and nothing is cached.

The SPA's three existing consumers (the `gO` structure popup, the outline
rail, the sticky-context header) still derive their own tree client-side
from `GET /api/file`'s flat `symbols` array; this unit ships no SPA and
does not rewrite them. `src/outline.rs`'s module doc names all four files
so the conversion lands on THIS response rather than a second derivation.

*Parity Grid changes.* The `outline` column flips off its shared "the
universal `outline/1` contract is not built yet" `partial` — it is now
`yes` for every row whose tier derives symbols and an explained `no` for
every row that derives none. The `symbols` and `hover` reasons are now
derived from the outline's SHAPE (`extract::OutlineShape`), so a stylesheet
and a prose document no longer borrow YAML's key-path wording. The
`usages` cell's Rails-lens branch now gates on
`frameworks::rails::LENS_LANG_IDS` instead of `injection_host && tier ==
full`: that proxy was exactly true while HAML was the only `full`
injection host and would have started claiming convention edges for
Markdown. Both goldens ship: `tests/fixtures/parity.golden.json` and (new)
`tests/fixtures/syntax.golden.json` — the registry's own wire, pinned for
the same reason the grid is, so adding a language, an extension, a salt or
an injection guest is a reviewable golden diff.

*The HIGHLIGHT_ONLY ledger.* V72-H1 shipped the tier as a mechanism with
no production row and recorded that emptiness with a test. V72-H2a gives
it exactly one row, `scss`, and it is a GRAMMAR judgement: `crate::css`
mints a real stylesheet outline for SCSS and its own tests prove it, but
`tree-sitter-scss` 1.0.0 — its upstream's only release — cannot parse
`@extend`, and the resulting `ERROR` swallows the REST of the enclosing
block. A `full` tier would therefore ship outlines that are silently short
and look complete, where highlighting over an error tree degrades VISIBLY
(uncoloured text). `css` (the official grammar) and `markdown` are `full`.
The test that pins the gap fails the day a grammar that parses `@extend`
lands, at which point flipping the tier back is a one-line change in
`syntax::REGISTRY`.
## Review — `kbc-review/1`, the review document (v7.3)

kb-code v7.3 makes a review a **document** rather than a pile of API calls.
Design of record: D9 + D9-a + Track K of
`docs/research/kb-code-v7-continuum-2026-09.html`.

### The document

A `kbc-review/1` document is **Markdown with YAML front matter**, stored per
review and patchset in `review_docs` (migration V0034) as an **append-only
chain of revisions** — `compose` never updates a row, it appends the next
revision and the highest wins on read. The body is Markdown and never HTML.
D9-a records why: HTML is a permanent XSS surface, cannot be interdiffed
across re-reviews, and its references are dead text. The operator's HTML
artifact is met one level up, as a rendered **export** (below).

Front matter — every key optional except `summary_md`, every unknown key
refused **by name**:

| key | shape |
|---|---|
| `schema` | `kbc-review/1` |
| `summary_md` | **required** — the prose summary; becomes `report.summary` |
| `risk` | `{level: low\|medium\|high, why: <one line>}` |
| `reading_order` | `[{chapter, stops: [<ref> \| {ref, why}]}]` |
| `blocks` | named sections: `context`, `approach`, `alternatives_considered`, `tests`, `rollout`, `open_questions` |
| `findings` | the v2 finding list (or supply it as a sidecar) |
| `flows` | `[{name, steps: [<ref>]}]` — named paths through the diff |
| `questions` | `[{to: to_author\|to_reviewer\|to_agent, ask, ref?}]` |
| `author` | `{kind: agent\|human, model?, session_id?, considered: [...], not_considered: [...]}` |

The front-matter parser is a **closed YAML subset** written for this purpose
(`review_doc::frontmatter`): block mappings and sequences, plain/quoted
scalars, `\|`/`>` block scalars, one-line flow sequences, comments. Anchors,
aliases, tags, merge keys, flow mappings, nested flow collections, tabs in
indentation and duplicate keys are **refused by name** with the line number.
Refusing is the point — a general YAML parser's job is to accept; this one's
job is to guarantee that what the document says is what the daemon read.

### Tiers, and what an omission means

`--tier minimal|standard|full` prices the authoring and states what the
document **promises**:

| tier | requires |
|---|---|
| `minimal` | `summary_md`, and a `findings` list (an EMPTY list is a valid, explicit "this review found nothing" — different from not having looked) |
| `standard` | + `risk` |
| `full` | + `author`, + at least one named `blocks` section |

`reading_order` is never a tier failure because it has a deterministic
substitute: when a document declares none, the response carries a **derived**
order from the existing review map (dependencies first, tests last —
literally `review_map::compute_reading_order`, the same function
`GET /api/reviews/{id}/reading-order` serves), captioned `derived`.
`flows` and `questions` have no substitute and are therefore reported as
omissions at every tier rather than as failures at `full`.

Every optional block a document does **not** carry is listed in `omitted[]`
on every read and in the rendered export. An absence is stated, never
discovered.

### Refs — the grammar

A ref is a `[[…]]` span whose body starts with one of nine closed scheme
prefixes (V73-K5 added `ci` and `question` to K1's original seven).
**A bare `[[X]]` is a kb wikilink and is never a kbc ref** (kb root
invariant #29 owns that syntax); a `[[…]]` that names a known scheme but does
not parse is reported as `ref_malformed`, never silently degraded into a
wikilink.

| scheme | form | example |
|---|---|---|
| `code` | `code:<path>[:<line>[-<end>]][@<blob-sha>]` | `[[code:app/models/order.rb:120-134@a1b2c3d]]` |
| `sym` | `sym:<Qualified>[#<member>]` (`::` is never a field boundary) | `[[sym:Namespace::Class#method]]` |
| `ent` | `ent:<Fqn>` | `[[ent:Shop::Order]]` |
| `finding` | `finding:<slug>` | `[[finding:f-7]]` |
| `gh` | `gh:<comment\|review\|issue\|pr>/<id>` | `[[gh:comment/12345]]` |
| `kb` | `kb:<kb>/<id>` | `[[kb:research/9f8b7182d433]]` |
| `hunk` | `hunk:<path>@<ps>#<n>` (`ps` may be `3` or `ps3`) | `[[hunk:app/models/order.rb@2#3]]` |
| `ci` | `ci:<check-name>` | `[[ci:build (ubuntu-latest)]]` |
| `question` | `question:<n>` (1-based, this document's own `questions[]`) | `[[question:2]]` |

Refs are read from BOTH surfaces through one parser: the prose scan (which
skips fenced code blocks, inline code spans and the front-matter region) and
the typed front-matter fields (`reading_order` stops, `flows` steps, a
question's `ref`, a finding's `cites`). The grammar is golden-pinned by
`crates/kb-code-server/grammar/kbcrefs.golden.json`, one fixture read by the
Rust parser and (from v7.3's SPA unit) by its TS mirror.

### Refs — the live cards

`GET /api/reviews/{id}/doc?resolve=true` turns every ref into a **card**,
computed per request and persisted nowhere:

| state | meaning | trust |
|---|---|---|
| `pinned` | the bytes the author cited are the bytes shown | `exact` when the `@sha` IS the patchset's blob; `likely` when the ref pinned no blob |
| `carried` | the bytes moved and the ladder re-anchored them, with a caption saying how | `likely` — never `exact` |
| `orphan` | no honest match; **no position is reported** | none |
| `inert` | `gh:`/`kb:`/`ci:` — kb-code makes no LIVE claim (it never calls GitHub, does not own the kb corpus, and reads `ci:` from a snapshot rather than re-fetching Checks) | none |

A `[[question:<n>]]` ref is `pinned`/`exact` when this document has an `n`th
question and an ordinary `orphan` (naming the count) otherwise — the one
scheme that resolves against the document's OWN structure rather than the
repository or the store.

A `code:` ref is carried by the **same** ladder review comments use
(`annotations::resolve` plus the snippet guard) — not a second matcher. A
`carried` ref is capped at `likely` even when the snippet matched verbatim:
an exact text match at a different line in a different blob is strong
evidence, not proof, and a wrong `exact` is this crate's release blocker.
`sym:`/`ent:` resolve through the existing symbol/entity addressing and take
only the EXACT rung — an ambiguous name is an orphan naming the count, never
a guess, because a ref card carries no fallback anchor the way a `?sym=`
link does. `ent:` additionally inherits the entity index's own class as a
CEILING. Each card carries a snippet (capped at 40 lines) and server
highlight spans, or `highlights: null` when the target blob is not one this
daemon has indexed — a pure store lookup, never derived in a handler.

### Findings v2

Additive on `review_findings` (V0034), beside the unchanged `severity`
(`blocker|concern|ok`) and the unchanged origin rule:

- `act` — `issue|question|suggestion|nitpick|praise|note|todo|chore`
- `category` — `correctness|security|performance|design|tests|docs|style|other`
- `blocking` — the reviewer's own call, deliberately **not** derived from
  `severity` ("a blocker that is not blocking this PR" is a real thing to say)
- `cites` — SECONDARY refs; the PRIMARY location stays the annotation anchor
  that gives the finding its ladder, its thread and its GitHub export
- `fingerprint` — a stable hash of `act + category + normalised title +
  primary path`; NULL on every pre-V0034 row and never backfilled
- `superseded_by` — the slug that replaced a tombstoned finding, written
  ONLY when an incoming finding declared `supersedes: [<slug>]`; never inferred

**The slug rule.** `f-<n>` slugs are minted once per review from a monotonic
ledger (`review_finding_slugs`) and are **never reused**, not even after the
finding they named is tombstoned. Reconciliation on the document path
matches by FINGERPRINT and keeps the slug, so re-wording a finding does not
orphan a human's disposition; a finding that vanishes from a re-compose is
tombstoned, never renumbered; an explicit author-supplied slug is honoured
(and recorded as taken); a `manual` finding is never adopted or superseded
by a compose, and an explicit slug naming one is a whole-compose 400.

### `compose` — the one authoring transaction

`POST /api/reviews/{id}/compose` — **loopback-only** (D22 local-canonical;
no review-authoring surface graduates off loopback). One call:

1. validates the front matter against the tier,
2. lints (below), resolving every ref,
3. reconciles the findings by fingerprint,
4. appends the document revision,
5. sets the report (`summary_md` becomes `report.summary`, normalised
   through the same `normalize_report_shape` `PUT /report` uses),
6. optionally sets the review-level verdict,

— all in ONE sqlite transaction, then emits exactly ONE
`review.changed{reason:"compose"}` and returns the resolved read. Any lint
ERROR is a 400 carrying the whole lint (every problem at once) with nothing
written. `dry_run: true` stops after the lint and writes nothing.

It composes no `risk_score`: `risk` is a level plus a sentence, and coercing
that into a number would be a precision the document never claimed.

```
kb-code review compose <id> --doc review.md [--findings findings.json] \
    [--tier standard] [--mode full|additive] [--ps N] \
    [--verdict approve|request-changes|comment] [-m NOTE] [--dry-run] [--json]
```

`findings import`, `report --set` and `verdict` remain the documented
**low-level twins** — the same writes, one at a time, each with its own
commit. The V0 `compose` form (`--from-file`/`--stdin`, a `kbc-compose/1`
JSON body with `summary` + a `kbc-findings/1` block) is unchanged; `doc_md`
is what selects the document path.

### `lint` — the pre-flight

`GET /api/reviews/{id}/doc/lint` lints the STORED document;
`kb-code review lint <id> --doc review.md` lints a CANDIDATE via a `compose`
dry run — the same code path, so a document that lints clean and then fails
to compose would be one bug, not two surfaces disagreeing. Both write
nothing.

| rule | severity | when |
|---|---|---|
| `doc_parse` | error | the front matter or the model does not parse |
| `size_cap` | error | over `MAX_DOC_BYTES` (256 KiB), `MAX_REFS` (2 000) or `MAX_FINDINGS` (500) — a REFUSAL naming the numbers, never a truncation |
| `tier_unmet` | error | a tier requirement is missing, naming the field and the tier |
| `ref_orphan` | error | a ref resolved to nothing, with nearest candidates |
| `ref_wrong_patchset` | error | the ref names a patchset this review does not have (or not the one being read) |
| `finding_no_location` | error | a finding with no usable primary location |
| `finding_vocabulary` | error | act / severity / category / slug outside its closed set |
| `duplicate_fingerprint` | error | two findings reconciliation could not tell apart |
| `duplicate_slug` | error | two findings claiming one slug |
| `ref_malformed` | warn | `[[code:]]` — a kbc scheme that does not parse |
| `bare_wikilink` | info | `[[Order]]` is a kb link, not a kbc ref |
| `stale_sha` | info | the ref resolved by carrying forward, not by a blob match |
| `bare_symbol_mention` | info | `Shop::Order` in prose with no `sym:`/`ent:` prefix (the `::` is required — a bare CapWord is never reported) |
| `question_without_ref` | info | a question with no location |

`kb-code review lint` exits **3** when the lint reports any error — the same
`EXIT_CONFLICT` slot an HTTP 409 uses, and for the same reason: the request
is well-formed and the state refuses it.

### `render --template` — the HTML export

The stored Markdown plus its resolved cards, poured into an operator HTML
template. This is the ONE place kb-code produces HTML from a review, and it
is an export: nothing rendered is ever stored.

```
kb-code review render <id> [--template house.html] [--out review.html] [--ps N] [--json]
```

With `--template` the operator's own file is POSTed to the loopback-only
twin so the daemon stays the only renderer; without it,
`GET /api/reviews/{id}/doc/render?template=<name>` renders through a
REGISTERED name — `default` (built-in) plus every key of
`[review] doc_templates` in `kb-code.toml`. The route takes a NAME, never a
path, so it is structurally unable to be talked into reading an arbitrary
file.

Placeholder grammar — `{{name}}` for a name in the closed set below; **every
other `{{…}}` is left byte-for-byte alone** and reported in
`unknown_placeholders`, so a template's own CSS/JS braces are never mangled
and a typo is never a silent hole:

`{{title}}` · `{{meta}}` (repo, review, patchset, revision, tier, render
time) · `{{summary}}` · `{{risk}}` · `{{reading_order}}` · `{{blocks}}` ·
`{{findings}}` · `{{cards}}` · `{{flows}}` · `{{questions}}` · `{{author}}` ·
`{{omitted}}` · `{{body}}`

Every dynamic string is HTML-escaped, and every Markdown body goes through
kb-core's existing UNTRUSTED-body renderer (`render.unsafe = false`), so raw
HTML inside a review document is escaped rather than passed through. Nothing
this renderer emits can execute; the only script in the output is script the
operator put in their own template. **kb-code never generates a
`<template id="kb-prompt">`** — that convention is kb's (root invariant #5),
and the kb-side authoring step owns writing one when the export is dropped
into a corpus. `kb-code review set-artifact` records the link back.

### Routes

| route | posture |
|---|---|
| `GET /api/reviews/{id}/doc?ps=&resolve=true` | bearer |
| `GET /api/reviews/{id}/doc/lint?ps=` | bearer |
| `GET /api/reviews/{id}/doc/render?ps=&template=<name>` | bearer |
| `POST /api/reviews/{id}/doc/render` | loopback-only (the operator's template bytes; writes nothing) |
| `POST /api/reviews/{id}/compose` | loopback-only |

The three reads are declared as `RouteContract`s in
`kb_code_server::review_doc::routes::V73_K1_ROUTES` and walked by the same
dead-surface test V71-G0 added.

CLI: `kb-code review {doc,lint,render,compose}`.

### The Document tab — the SPA half

The Review Room's cockpit gains a sixth tab, `?tab=doc`, rendering
`GET …/doc?resolve=true`: the front matter as a header (tier badge, risk
chip, revision *n* of *m*, the author block with `considered` /
`not_considered`, `omitted[]` as a captioned degrade list), the reading order
(the author's chapters and stops, or the daemon's own captioned **derived**),
the named `blocks` in their DECLARED order (never the wire's alphabetical
one), `flows` and `questions` as ref lists, the findings with their v2 axes,
and the Markdown body — with **every `[[…]]` replaced inline by a live
card**. A card shows the server's snippet with the server's own highlight
spans (never a client-side highlighter), its state as a WORD (`pinned` /
`carried` / `no honest match` / `inert`), its trust through the shared
line-style badge, the daemon's caption verbatim, and an address that links
into the reader (or the review diff, or the finding permalink). An orphan is
a full, visible card that says it has no honest match and carries no link;
an inert `gh:`/`kb:` link carries none either, because kb-code names no host
it could resolve. `?cards=folded` folds every card to its address line — a
folded card still says what it points at. The side panel lists every ref as a
jump list. `GET …/doc/lint` renders as a small read-only panel on the tab's
header, with each row's "did you mean" candidates; composing stays
loopback-only, so the tab shows the exact `kb-code review compose` line an
agent would run, to copy.

The refs grammar has a TS mirror, `web-code/src/lib/kbcRefs.ts`, which walks
the SAME `crates/kb-code-server/grammar/kbcrefs.golden.json` fixture the Rust
parser does — one fixture, two parsers, neither generated from the other
(kbc-review/1 joining the discipline kbcq/1 already carries). Keys:
`6` opens the tab, `Space r` folds/unfolds every card, `] r`/`[ r` step the
focused card, `Space o` opens its target, `Space y` copies the compose line.
V73-K2b also registered the five pre-existing `review.tab.*` rows (`1`…`5`),
which had shipped in v7.0 and dispatched nowhere.

The findings wire (`GET /api/reviews/{id}/findings` and its single-finding
siblings) carries the five v2 fields from v7.3's SPA unit onward: V0034
landed them as columns and the document read surfaced them, but the wire
every finding CARD reads did not, so the two axes D9 added were storable and
unreadable. They are additive — `"issue"` / `false` / `null` on every
pre-V0034 row, which is what those rows always meant.

## The stream — timeline v2, pseudo-files, claims, hunk↔turn (v7.3, Track K)

Design of record: D9 + D9-a + D18 + D25 of
`docs/research/kb-code-v7-continuum-2026-09.html`. Four surfaces that all
answer one question — *what happened to this review, and who did it* — and
one CLI verb that migrates the old artifacts into it.

### `review-timeline/2` — one ordered stream

`GET /api/reviews/{id}/timeline` (bearer) was a lifecycle changelog; it is
now the whole conversation. The widening is **additive**: every
`review-timeline/1` event keeps its `at` field and its own payload keys
byte-for-byte (pinned by a test), the `events` array is still there, and
what is new is a typed envelope, five more lanes, and narrowing.

Every event carries:

```json
{ "at": 1757000000, "ts": 1757000000, "kind": "comment", "lane": "comments",
  "author": { "kind": "human", "name": "you" },
  "ref": "code:app/models/order.rb", "body_md": "why cents?",
  "drift": null, "…kind-specific keys": "…" }
```

`ts` is `at` under the name the rest of v7 uses; both are emitted so a v1
reader keeps working. `author.kind` is `human | agent | system`, derived
from the author NAME through kb's own closed harness vocabulary
(`kb_core::sessions::HARNESSES` + `agent`) — **a name convention, not
authentication**; kb-code authenticates nobody and root CLAUDE.md's
"identity is attribution, not authorization" ruling is unchanged. `ref` is a
`kbc-review/1` ref (K1's grammar) when the event has a location, and is
absent — never fabricated — when it does not. `drift` appears only when the
daemon can name BOTH sides of what moved.

| lane | kinds | source |
|---|---|---|
| `lifecycle` | `review_created` `pr_bound` `patchset` | the review, its PR binding, its patchsets |
| `pr_body` | `pr_body` | the `pr_meta_json` snapshot |
| `findings` | `findings_import` `finding_added` `disposition` `finding_published` | `review_findings` |
| `verdict` | `verdict` `verdict_published` | `reviews` |
| `comments` | `comment` | review-scoped `annotations` |
| `wt_comments` | `wt_comment` | working-tree `annotations` on this review's own files |
| `document` | `doc_revision` | `review_docs` — K1's append-only compose chain |
| `report` | `report` | `reviews.report_json` |
| `claims` | `claim` | `claims` (V0035, below) |
| `github` | `github_comment` | LIVE, via the same `list_pull_comments` call `/github-threads` makes |
| `turns` | `turn` | the hunk↔turn join — `?hunk=` only, LOOPBACK only |

**Every lane reports its own state** in `sources[]` (`ok` · `skipped` · 
`refused` · `degraded`), with a reason on anything that is not `ok`. A lane
that failed is never silently empty — the v6.0 One-Inbox per-lane
precedent, so a GitHub outage cannot make a timeline quietly lie about what
was said.

Two lanes are conditional, each for a stated reason. **`github`** is a LIVE
network call the other ten are not: it is on for a PR-bound review, off
otherwise, `?github=0` turns it off, and a failure degrades the lane rather
than the request. Thread NESTING stays on `/github-threads` — a timeline is
chronological by definition, so re-parenting replies here would be a second,
disagreeing answer. **`turns`** needs `?hunk=<kbc-hunkid/1>` **and**
loopback, because the join reads raw transcript content (D19's
`raw-transcript` sensitivity class); off loopback the lane is `refused` with
that reason, never absent.

Narrowing: `?kind=` (CSV over the closed vocabulary — an unknown name is a
400 NAMING it, never an empty page) · `?author=` (`human|agent|system` or a
literal name) · `?since=`/`?until=` (an inverted window is a 400) ·
`?limit=`/`?offset=`. `total` is the count AFTER filtering and BEFORE
paging, so a page can say what it is a page of. There is deliberately no
`include_superseded`: a timeline is a HISTORY, and a superseded finding's
own import and disposition still happened.

```
kb-code review timeline ID [--kind K,K] [--author A] [--since T] [--until T] \
    [--limit N] [--offset N] [--github true|false] [--hunk ID] [--ps N] [--json]
```

### Pseudo-files — `kbc-pseudo/1`

Four things a reviewer must read are not files in the tree, so until now
they could be displayed but never *addressed*: you could not cite line 12 of
a PR body. `GET /api/reviews/{id}/pseudo[/{name}]` (bearer) gives each one a
name under the reserved `~review/` prefix and a **real git blob hash** —
literally `ingest::git_blob_hash` over the rendered bytes, the same function
the mirror index uses.

| name | rendered from |
|---|---|
| `~review/pr-body.md` | the `pr_meta_json` snapshot's `body`, VERBATIM (no header, so a line number means the line the author wrote) |
| `~review/review.md` | the current `kbc-review/1` document revision, front matter and body |
| `~review/findings.json` | findings v2 in exactly the shape `compose --findings` accepts (superseded excluded, counted) |
| `~review/commits.md` | `git log <base>..<tip>` with each commit's trailers, via git's own `%(trailers:only,unfold)` |

Because the hash is a real blob hash, `[[code:~review/pr-body.md:12@<sha>]]`
resolves through the SAME card ladder a tracked path takes: `pinned`/`exact`
on byte equality, `pinned`/`likely` when the ref pinned no blob, `orphan`
otherwise. There is deliberately **no carry-forward rung** for a
pseudo-file: it is regenerated whole on every read, so "the same line,
moved" is not a thing that happened, and re-anchoring prose into a
regenerated document would be a guess with nothing behind it. A comment
anchored to a pseudo path resolves through
`review_pseudo::resolve_on_pseudo`, which is `review_comments::
resolve_for_ps_with_content` given the pseudo bytes — not a second matcher.

Nothing is stored. A consequence worth stating rather than discovering: the
PR body snapshot is wholesale-replaced by `review sweep`, so this daemon has
**no revision chain** for it. A change is DETECTABLE (the `blob_sha` moves,
and refs pinned to the old one stop reading `pinned`) but the previous text
is not recoverable, and no surface pretends otherwise. All four names always
exist; two of them may be empty, `present: false`, with a `reason`.

The derived reading order gains **chapter zero**, "The review itself",
listing all four before the diff — a reviewer who reads the code before the
description is reading it without the question it was meant to answer.

```
kb-code review pseudo ID [NAME] [--ps N] [--json]
```

### `kbc-claim/1` — the agent prose register

An agent reading code produces two kinds of output. One is a FACT this
daemon can re-derive, and the crate's discipline is that such a fact carries
a class minted per request and is never cached. The other is PROSE — "this
is the retry path", "we chose the queue over a cron because…", "the
alternative I rejected was…" — which cannot be re-derived, so it must be
stored, and *because* it cannot be re-derived it must never be trusted the
way a derivation is.

ONE table (`claims`, migration V0035), and that is the point. D18 names six
renderings — commit-time explain cards, the rejected-alternatives ledger,
decision threads, branch stories, trail notes, entity answers — and rules
that each is a KIND plus a rendering, never a table.

```
{ id, repo, subject_kind, subject, subject_path?, review_id?,
  kind, body_md, confidence?, evidence[], session_id?, model?, blob_sha?,
  state, caption, current_blob?, created_at }
```

- `subject_kind` — `path | sym | ent | commit | hunk | review | branch`
- `kind` — `explain | alternative | decision | story | note | answer`
- `evidence[]` — `kbc-review/1` refs, validated on write and stored as
  written (a resolved position is a per-request derivation; persisting one
  would make a stale answer indistinguishable from a fresh one)
- `confidence` — the AGENT'S OWN declaration, 0..=1, surfaced verbatim.
  Nothing multiplies it into anything.

**Three rules.** *(a) Surfaced, never scored.* A claim is rendered beside
the fact it is about and is never a ranking term, a boost, a filter default
or a trust class. The pin is structural: a source scan over this crate's
ranking modules fails by file if any of them so much as names `claims`.
*(b) The class is computed per request and is not a column.* There is no
`trust` column, exactly as `lane_facts` and `entity_defs` have none. What is
stored is the WITNESS — `blob_sha`, the bytes the author was looking at —
and the read turns it into `pinned` / `drifted` / `unanchored` by comparing
with the file's live blob. A drifted claim is shown with a caption naming
BOTH blobs, never hidden and never re-anchored. `unanchored` is the honest
answer, not a failure: a decision about a branch has no blob. *(c) Writes
are loopback-only and audited* (D22; root invariant #4 unamended).

`claims` is deliberately **not** in `Store::delete_file`'s cascade, and that
is a ruling. `rails_edges`/`entity_defs`/`lane_facts` are DERIVED rows whose
path key would make a deleted file answer forever; a claim is AUTHORED
content, like an `annotations` row. Deleting an agent's reasoning because
the file it was about was deleted would destroy the record that explains
why. A claim about a vanished path reads back `unanchored`, with the reason.

| route | posture |
|---|---|
| `GET /api/claims?repo=&subject=&subject_kind=&path=&review=&kind=&limit=&offset=` | bearer |
| `GET /api/claims/{id}` | bearer |
| `POST /api/claims` | loopback-only |

```
kb-code claim add --repo R --subject ADDR --subject-kind K --kind K --body - \
    [--confidence F] [--evidence REF]... [--session-id S] [--model M] \
    [--blob SHA] [--review N] [--json]
kb-code claim list --repo R [--subject|--path|--review|--kind|--limit|--offset] [--json]
kb-code claim show ID [--json]
```

### hunk↔turn — two tiers, and a third answer

`GET /api/reviews/{id}/hunks/{hunk}/turns` — **LOOPBACK-ONLY**. "Which agent
turn wrote this hunk?" is a question the session↔commit join cannot answer:
that join is per COMMIT, and a commit is many hunks by many turns.

The hunk is addressed by its `kbc-hunkid/1` content address — the same one
the SPA's per-hunk viewed state uses, now with a Rust implementation
(`review_hunks`) pinned to the TypeScript one by a shared golden
(`crates/kb-code-server/grammar/kbchunkid.golden.json`, read by both
`review_hunks.rs` and `web-code/src/lib/hunkId.golden.test.ts`). The hash is
FNV-1a 64 over **UTF-16 code units**, which is load-bearing rather than
incidental: the TS side hashes `charCodeAt`, so a byte-based "simplification"
would disagree only for diffs containing a non-ASCII character. The golden
carries one.

| tier | requires |
|---|---|
| `exact` | a captured `Edit`/`MultiEdit`/`Write`/`NotebookEdit` whose `file_path` resolves to the hunk's path, whose `old_string`/`new_string` appear byte-for-byte in the hunk's removed/added text, **and** whose session's commit join reaches a commit whose OWN diff reproduces this hunk id. Three independent witnesses: the bytes, the path, the commit. |
| `likely` | the bytes match, but the path moved, or no commit join reaches the session, or the carrying commit could not be named exactly. Evidence, not proof. |
| *(not claimed)* | anything else — an EMPTY list with a `reason`. There is no fuzzy third tier: a wrong `exact` is this crate's release blocker, and a "possible" tier is where both a wrong `exact` and an uncertain match would hide. |

`commit_basis` says how well the carrying commit could be named:
`hunk_exact` (a commit's own diff reproduces this content address — proof,
not proximity), `path_in_range` (no single commit did, so the set is every
commit in range touching this path — too wide for `exact`, so every match is
capped at `likely`), or `none`. The byte match is CONTAINMENT, because
git's `-U3` window routinely groups several edits into one hunk, and it is
floored at `MIN_MATCH_BYTES` (24) — a one-line `old_string` of `end` is
contained in half the hunks in a Ruby repository, so a match under the floor
is not claimed at any tier.

Nothing is persisted: the diff is re-derived, the ids re-minted, the tool
inputs re-read from the JSONL, the tiers re-computed. `old_string`/
`new_string` are file content out of a raw transcript, which is why the
route is loopback-only; the bearer-visible half (session id, the
`t-<uuid12>` turn id minted through kb-core's own derivation, tool, path,
tier) is what the timeline's `turn` events carry, and the surrounding
assistant text never leaves loopback. If the kb sibling is unreachable the
commit join degrades honestly (`kb_lane: "degraded"`) and every tier caps at
`likely`; it never fails the request.

```
kb-code review turns ID --hunk HUNKID [--ps N] [--json]
```

### The SPA surface (`V73-K2c`)

The Room's own half of all four surfaces above — timeline v2's lane bar +
filters + paging, the claim register (Document tab, beside the Report tab's
findings, and the reader inspector rail's Claims card), the on-demand
hunk↔turn "turns" chip, and the pseudo-files chapter zero + read-only
buffer view — lives in `web-code/`, documented in
[web-code/CLAUDE.md](../web-code/CLAUDE.md)'s own "Timeline v2, the claim
register, hunk↔turn chips and pseudo-files" section. That section also
names a genuine server-side wire bug this unit found and fixed: a duplicate
`"author"` JSON key on `comment`/`wt_comment` timeline events that silently
clobbered the v2 envelope for those two of seventeen kinds.

### `import-legacy` — migrating the old artifacts

`kb-code review import-legacy <artifact.html>` (`kbc-legacy-import/1`) reads
**only** the artifact's embedded `<script type="application/json">` machine
block and maps it onto a `kbc-review/1` document plus a findings v2 sidecar.
D9-a is a hard rule and the implementation makes it structural: the importer
never parses the DOM. A finding recovered from a `<section class="finding">`
is a GUESS, and a guessed finding goes on to inherit a slug, a human's
disposition and a place in a GitHub thread.

It is a LOCAL verb — no daemon, no route. The artifact is a file on the
operator's box and the mapping is pure; uploading it to have it transformed
and handed back would add a mutation-shaped route that mutates nothing.

Accepted block ids, in probe order: `kb-review-data`, `review-data`,
`pr-review-data`, `kbc-review`, `review-json`. Any other id is ignored, so
an analytics blob on the same page can never be mistaken for the record. An
artifact with no block **exits 3** naming the ids it looked for.

The mapping is tolerant in ONE direction: key ALIASES are accepted; VALUES
outside a closed kbc vocabulary are never silently coerced — each is mapped
through a substitution that appears in `mapping[]`, or refused under
`--strict`. A finding with no usable path is SKIPPED with a reason rather
than given one. A legacy id is **never** adopted as a slug (slugs are minted
from the review's own monotonic ledger and never reused); it survives in the
rationale so the row stays traceable. The full field-by-field table lives in
the skill's own doc.

```
kb-code review import-legacy ARTIFACT.html [--strict] [--tier T] \
    [--out-doc F] [--out-findings F] [--review N] [--json]
```

The agent-layer wrapper is the **`/kb-review-migrate`** skill
(`plugins/kb-code/skills/kb-review-migrate/`): it runs `import-legacy`,
reports every substitution and every skipped finding by title, runs
`review lint`, and hands the operator the exact `review compose` line. It
never composes — the last look at a machine translation belongs to a human.

### Legacy review artifacts → kbc-review/1

A vocabulary-coverage pass (V73-K4) checked every element a typical
pre-K1 review artifact carries — an operator- or agent-rendered HTML page
with an embedded machine-readable findings block — against what
`kbc-review/1` can express today. The grading is at the protocol level:
does the vocabulary (front-matter keys, the ref schemes — nine as of
V73-K5's `ci`/`question` addition, findings v2, the timeline lanes, the
`kbc-claim/1` kinds) have a place for this element at all, independent of
any one importer's current bugs.

| element category | kbc-review/1 carrier | coverage |
|---|---|---|
| header identity (repo, PR/change number, title) | `pr_meta` binding + `{{meta}}` + `{{pr_number}}` (V73-K5) | covered-with-degrade — `{{meta}}` still surfaces kb-code's own review id; the EXTERNAL PR number now has its own placeholder rather than sharing `{{meta}}`'s |
| author / timestamp / branch attribution | `pr_meta` snapshot | covered-with-degrade — not folded into the document's own render |
| top-level verdict + prose | review-level `verdict` + `report_json` | covered-with-degrade — two separate mechanisms, not unified in front matter |
| numeric risk score | `report_json.risk_score` (pre-K1 lane) + `{{risk_score}}` (V73-K5) | covered-with-degrade — `risk` stays deliberately level + why, by design; the render placeholder now bridges the two lanes rather than the score having no place in a render at all |
| aggregate finding counts | derivable from findings v2 by `severity` | covered-with-degrade — arithmetic over the list, not a stored or rendered field |
| CI-check counts / status | `ci:` block (V73-K5), authored or DERIVED from `pr_meta_json.checks` | covered-with-degrade — arithmetic over `GET .../doc`'s `ci.checks`, not a separately stored count (the aggregate-finding-counts row's same shape) |
| prose summary | `summary_md` | covered |
| per-finding severity | `severity` (`blocker`\|`concern`\|`ok`) | covered |
| per-finding free-text category | `category` (8-value closed set) | covered-with-degrade — a value outside the set folds to `other` with a stated mapping row |
| per-finding location (path / line(s) / whole-file) | `location{path,kind,lines,removed}`, flat OR nested (V73-K5) | covered — the same shape `kbc-findings/1` already uses, and `import-legacy` now reads both shapes it appears in |
| per-finding id + external deep link | rationale-embedded provenance line only (`legacy_id: …`, V73-K5) | covered-with-degrade — a fresh slug is always minted (never reused), so a link published elsewhere against the old id will not resolve |
| per-finding title / rationale / recommendation | `title` / `rationale` / `recommendation` | covered |
| per-finding cited code excerpt | typed `evidence{lang,source}`, or a live `[[code:path:lines]]` `cites` ref (V73-K5) | covered — a typed excerpt is carried as `evidence`; a flat scraped snippet is never reproduced verbatim, only pointed at live, per D9-a |
| files-changed / diffstat list | diff v2's own file map (`GET /reviews/{id}/files`) | covered-with-degrade — a separate surface, not joined into the document or its render |
| CI checks list | `ci:` block (V73-K5) + `[[ci:<check-name>]]` ref | covered — an inert snapshot card, per-check, authored or derived |
| embedded machine JSON block | `kbc-legacy-import/1`'s intended input, now ALSO `render`'s own output (V73-K5) | covered-with-degrade — depends on a THIRD-PARTY block carrying a recognized id/shape; kb-code's own export always does |
| question ↔ its answer | `questions[].answers` + `[[question:<n>]]` ref (V73-K5, bonus) | covered — not part of the original legacy-artifact vocabulary (no pre-K1 artifact had this concept), added as the rubric's one GitHub-parity win |

**Closed in V73-K5** (this table's degrades above name the residue of each):

- `import-legacy` now reads a finding's cited code excerpt: a typed
  `evidence{lang,source}` object carries straight through; a flat scraped
  snippet string is turned into a live `cites` ref (`code:<path>[:<lines>]`,
  no `@sha` — this verb has no repository to read a blob hash from) rather
  than reproduced as frozen, possibly-stale text. A finding that carries an
  excerpt but no usable location still says so in its `skipped[]` reason.
- Its finding-location lookup now falls back to the nested `{path, kind,
  lines}` object shape `DocFinding` and `kbc-findings/1` already use
  elsewhere in this crate; an out-of-vocabulary nested `kind` is substituted
  with a stated mapping row exactly as an out-of-vocabulary `severity` is.
- Its accepted block-id allowlist now includes `kbc-findings` — kb-code's
  own pre-v7.3 findings-ledger schema — mapped through the same tolerant
  generic path: `severity` is already the kbc vocabulary (zero
  substitution), and a `slug` is kept traceable as `legacy_id` in the
  rationale, never adopted.
- `render` (every template, including the built-in default) now embeds a
  `<script type="application/json" id="kbc-review">` machine block in
  exactly the shape `import-legacy` accepts (front matter's `summary_md`/
  `risk` plus every live, non-superseded finding) — `</` is escaped so a
  finding's own prose can never truncate the block early. `kb-code review
  import-legacy` on that export reproduces the document, modulo freshly
  minted finding slugs (the ONE place the round trip is deliberately
  lossy, by the same "a legacy id is never adopted" rule).
- The `ci:` front-matter block (`{name, status: success|failure|pending|
  skipped, url?, observed_at?}`) and the `[[ci:<check-name>]]` ref scheme
  now name a CI check run — authored, or DERIVED from the review's own
  `pr_meta_json.checks` snapshot when the document names none. A `ci:` ref
  is `is_inert` (a snapshot read, never a fresh GitHub call), the same
  posture `gh:`/`kb:` already take.

## Recipes — `kbc-recipe/1` (v7.4, Track L)

A **recipe** is a named, parameterised, deterministic question asked of
kb-code's own indexes. Two families share one surface:

- **`recipes/1`** (v3.3-Q1, `GET /api/recipes`, `GET /api/recipes/{name}`)
  — six compiled-in ranked queries. **FROZEN** and byte-compatible for the
  SPA's existing Recipes page, with two deliberate behaviour changes from
  the repair round below.
- **`kbc-recipe/1`** (v7.4, `GET /api/recipe*`) — the typed runner: a DAG
  over a closed op set, typed params, kbc-scope/1 scoping, four result
  views, a per-step census, and trust-on-first-use for recipes a repo
  carries. The six `recipes/1` bodies are **adopted** here as native
  adapters, so all fourteen run, show and lint the same way.

The two prefixes are separate for the reason `canvas` and `boards` are:
`/api/recipes/{name}` already owns the depth-2 param slot, so the new
family gets its own singular prefix rather than shadowing a recipe that
happens to be named after a literal segment.

### What a recipe is

```toml
slug = "orient:hot-and-cold"        # the FILE NAME is the slug for a repo file
title = "Hot and cold"
intent = "orienting"                # orienting|reviewing|checking-tests|rails|hygiene
description_md = "…"
scope = "$context.scope"            # a kbc-scope/1 expression; default = the reader's own

params = [
  { name = "top", type = "int", default = 25, min = 1, max = 500 },
]

steps = [
  { id = "churn",  op = "churn",   args = { min_revisions = "$p.min_revisions" } },
  { id = "ranked", op = "set_ops", args = { mode = "sort", from = "$steps.churn", by = "churn" } },
]

views = [
  { id = "hot", kind = "table", step = "ranked", columns = [
    { header = "Path", field = "path" },
    { header = "Churn", field = { scalar = "churn" } },
  ]},
]
```

Param types: `string` · `int` · `float` · `bool` · `enum` (with `values`)
· `path` (rejected by the same lexical guard every content route uses) ·
`symbol` · `ref` (constructed through `git::revspec::Revspec`, so
invariant 3 holds for a recipe-supplied ref too). `min`/`max` are
inclusive and a violation is a 400 naming the field, the value and the
bound.

An arg is a literal, `$p.<name>`, `$context.<field>`
(`repo|path|symbol|ref|scope`) or `$steps.<id>`. There is **no
interpolation** — `"$p.a and $p.b"` is a literal string, because a
template would be the beginning of a query language (D21). A field
projection goes through the `map` op, whose transforms are type-checked;
`$steps.<id>.<field>` is refused at load.

Every result cell is an **address** — `path`, `line`, `symbol`, `entity`,
`commit`, `id`, `blob`, `trust`, `kind`, `address`, or `{ scalar = "…" }`
for one of the emitting op's own derived numbers. `blob` and `trust` are
always present: an engine that reported neither says `unknown`, never a
blank and never a zero.

### The op set (closed)

| op | args | consumes | produces |
|---|---|---|---|
| `search` | `q` (kbcq/1), `lane`, `limit` | — | `file` \| `symbol` \| `line` (per `lane`) |
| `usages` | `from`, `trust`, `limit` | `symbol` | `line` |
| `entity` | `from`, `name`, `kind`, `limit` | `file` | `entity` |
| `outline` | `from`, `kinds`, `limit` | `file` | `symbol` |
| `comments` | `from`, `kind`, `keyword`, `state`, `limit` | `file` | `comment` |
| `facts` | `lane`, `from`, `kind`, `min_severity`, `limit` | `file` | `fact` |
| `rails` | `noun` \| `orphans`, `limit` | — | `entity` \| `file` |
| `tree` | `scope`, `ext`, `limit` | — | `file` |
| `git_log` | `since`, `range`, `emit`, `limit` | — | `commit` \| `file` (per `emit`) |
| `blame` | `from`, `limit` | `file`/`symbol`/`line` | `line` |
| `churn` | `min_revisions`, `limit` | — | `file` |
| `boards` | `status`, `limit` | — | `node` |
| `review` | `kind`, `state`, `severity`, `disposition`, `limit` | — | `finding` |
| `set_ops` | `mode`, `from`, `with`, `by`, `desc`, `n`, `trust`, `path_prefix`, `min`, `max` | any (one kind) | its input's kind |
| `map` | `from`, `to` | varies | varies |

`set_ops` modes: `union` · `intersect` · `diff` · `filter` · `sort` ·
`limit`. `map` transforms: `file-of` · `symbol-of` · `entity-of` ·
`definitions-of`.

**A recipe can never reference an exec lane.** Not because a check
rejects one — because no variant of the op enum names one (D21;
kb-code-server invariant 10, "the daemon never spawns a non-git
process"). The `facts` op reads `lane_facts` rows the operator's own CLI
already ingested and cannot cause a tool to run; its lane must still be
enabled in `[lanes]`, which no recipe can do.

The DAG is **type-checked at load**: a step fed the wrong address kind
fails by NAME (`steps.uses: arg "from" carries file addresses, but op
`usages` accepts symbol`), as does a forward reference, an unknown arg,
an undeclared param, a `set_ops` whose inputs disagree on kind, and a
view over a step that does not exist.

### Where a recipe lives, and trust

| home | source | trust |
|---|---|---|
| `builtin` | the eight DAG recipes + six native adapters this binary ships | always trusted |
| `server` | `kb-code recipe new --from-json -` (loopback) | always trusted |
| `repo` | `.kbc/recipes/<slug>.toml` at the repo's **default ref** | trust-on-first-use |

A repo file is read **only from the default ref, only through the ODB** —
never the working tree, never whatever branch is checked out. First sight
is `untrusted` and refuses to run with a 403 naming the state and the
command that accepts it (`urn:kb:errors:recipe-untrusted`). A changed
content hash is `changed` **with a unified diff**, and does not inherit
the old decision. Caps: 64 KiB per file, 64 files per repo.

A repo file **wins** on a slug collision and the catalog reports what it
shadowed (`shadowed_by`). A file whose declared `slug` disagrees with its
file name is a reported problem, not a silent rename.

### Running one

`GET /api/recipe/{slug}/run?repo=&p.<name>=&ctx.<field>=&scope=&limit=&view=`
— a bearer **read**; it mutates nothing, which is also what makes a run
URL shareable. The response is `kbc-recipe-run/1`:

```jsonc
{
  "schema": "kbc-recipe-run/1",
  "recipe": "…", "home": "repo", "source": "repo:.kbc/recipes/x.toml@<blob>",
  "trust": "trusted",
  "scope": { "expression": "…", "applied": true, "matched": 42, "notes": [] },
  "steps": [ { "id": "…", "op": "churn", "rows": [ /* addresses */ ],
               "total": 120, "truncated": true, "ms": 8,
               "census": { "empty_reason": "lane-disabled",
                           "inputs": { "path_stats": 120 },
                           "filters_applied": [ "…" ], "notes": [] } } ],
  "views": [ { "id": "hot", "kind": "table", "columns": [ … ], "rows": [ [ "…" ] ] } ],
  "honesty": { "generation": 41, "as_of": "…", "budget_ms": 20000,
               "elapsed_ms": 91, "budget_exhausted": false, "notes": [] }
}
```

- **`limit` over 500 is a 400 naming both numbers.** Never a silent clamp.
- **A scope with any diagnostic is not applied**: the run proceeds
  UNSCOPED with the reason captioned (`scope.applied: false`), because a
  silent narrowing is worse than an honest widening.
- **A run is deterministic for a given mirror state.** Every op sorts on
  the address itself, identical step calls are memoised, and two runs
  against one `generation` are byte-identical.

### The census — why is this empty

Every step carries one. `empty_reason` is a closed vocabulary and exactly
**one** value means "nothing to worry about":

| reason | means |
|---|---|
| `filtered-out` | rows existed and every one failed this step's filters — **the only clean one** |
| `no-inputs` | the step reads per-address and got none |
| `upstream-empty` | the previous step returned nothing |
| `scope-excluded` | the scope removed every row |
| `lane-disabled` | an `aug-lane/1` lane is off in `[lanes]` |
| `lane-unknown` | no lane by that name |
| `lane-unavailable` | a known lane could not answer |
| `no-index` | the index this step reads has no rows (the reason names the command that builds it) |
| `not-a-rails-app` | rails/1 found no Rails structure |
| `param-empty` | a param resolved to an empty value |
| `budget-exhausted` | the 20 s run budget was spent before this step started |

`inputs` names what the step actually READ, so a reader can see where the
funnel narrowed; `filters_applied` names what it applied.

### The shipped catalog

| slug | intent | params | views | CLI |
|---|---|---|---|---|
| `orient:entry-points` | orienting | `top` | doors · routes · classes | `kb-code recipe run orient:entry-points --repo R` |
| `orient:hot-and-cold` | orienting | `min_revisions`, `top` | hot · inside | `kb-code recipe run orient:hot-and-cold --repo R` |
| `review:blast-radius` | reviewing | `since`*, `test_prefix`, `top` | callers · tests · changed · prior | `kb-code recipe run review:blast-radius --repo R --p since=30d` |
| `review:untested-changes` | reviewing | `since`*, `test_scope`, `lane` | untested · covered · tests | `kb-code recipe run review:untested-changes --repo R --p since=30d` |
| `tests:flaky-candidates` | checking-tests | `test_scope`, `min_revisions`, `top` | top | `kb-code recipe run tests:flaky-candidates --repo R` |
| `rails:orphans` | rails | `lane` (enum), `top` | orphans | `kb-code recipe run rails:orphans --repo R --p lane=action_without_route` |
| `hygiene:aged-todos` | hygiene | `top` | both · aged | `kb-code recipe run hygiene:aged-todos --repo R` |
| `hygiene:drifted-docs` | hygiene | `top` | drifted · files | `kb-code recipe run hygiene:drifted-docs --repo R` |

Plus the six natives, adapted: `new-public-api` (`since`*),
`god-functions`, `agent-only-symbols`, `complexity-climbers` (`since`*),
`unreviewed-hotspots`, `failure-tainted`. Their bodies stay in
`recipes.rs` — `new-public-api`'s language-specific visibility rules and
`god-functions`' fan-in/fan-out fold are not expressible in the op set,
and growing the set one recipe-shaped variant at a time is exactly what
D21 forbids. What they gain is everything around the body: typed params,
the cap refusal, scoping, a census, and views that render the columns
both old presenters dropped.

### The `recipes/1` repair round

D11's list, each with a test named after it:

| defect | fix |
|---|---|
| `complexity-climbers` mapped every `read_blob` failure to `Complexity{0,0}`, so a file added after `since` topped the list with a delta equal to its whole size | a baseline that could not be read is `terms.then_state` (`absent`/`too-large`/`unreadable`) with `score: null`, listed last and never ranked as a climb |
| `agent-only-symbols`' gate read `author_stats OR commit_sessions` while its row test read only `commit_sessions` | the gate reads the SAME table, and an empty result over a missing input says which one (`commit_sessions` vs `agent_attribution`); the per-sha lookup is memoised |
| `failure-tainted`'s `terms.fail_count` was a permanent zero (its only writer hardcodes `0`) | the term is **retired** rather than reported as measured-and-zero; the note says so |
| `limit > 500` was silently clamped | a 400 naming both numbers, on `recipes/1` **and** `kbc-recipe/1` |
| the CLI's 10 s client timeout made the two blame-heavy recipes unusable | `kb-code recipe` uses a 600 s client |
| the CLI's `error_for_status()` discarded the server's message | error bodies are rendered (`recipe: p.since: required (string) (HTTP 400)`) |
| `new-public-api` returned a confident empty set on the four-fifths of languages it has no rule for | the note names the rule set and the per-language count it did **not** examine; a run with nothing examinable reports `inputs_missing` |
| the note said "working tree HEAD", which is not a thing | it names the working tree |

Two of these change behaviour on the frozen wire on purpose: `limit >
500` now 400s (the SPA's own control tops out at 500, so it is
unaffected), and `complexity-climbers` rows can carry `score: null` (the
SPA already renders a null score as `—`).

### Routes

| route | gate | note |
|---|---|---|
| `GET /api/recipe?repo=` | bearer | catalog: home, trust, CLI line, load problems |
| `GET /api/recipe/{slug}?repo=` | bearer | params, steps, views, trust diff |
| `GET /api/recipe/{slug}/lint?repo=` | bearer | a LIST of problems, never a first error |
| `GET /api/recipe/{slug}/run?repo=&p.…` | bearer | the run — mutates nothing |
| `GET /api/recipe/runs/{id}` | bearer | replay a materialised run; says whether it is stale |
| `POST /api/recipe/{slug}/materialise?…` | loopback | run + store a replayable snapshot |
| `POST /api/recipe/{slug}/trust` | loopback | accept the bytes at the default ref NOW |
| `POST /api/recipe/new` | loopback | store an agent-authored recipe on the daemon |
| `DELETE /api/recipe/{slug}` | loopback | remove a server-stored recipe only |

### CLI

```
kb-code recipe list  --repo R [--intent orienting]
kb-code recipe show  <slug> --repo R
kb-code recipe lint  <slug> --repo R            # exit 3 on a refuse
kb-code recipe run   <slug> --repo R [--p name=value …] [--ctx path=… ]
                                     [--scope 'path:app//*'] [--limit N] [--view id]
                                     [--materialise] [--save-as-set NAME]
kb-code recipe new   --from-json -  [--repo R]
kb-code recipe trust <slug> --repo R
kb-code recipe runs  <run_id>
kb-code recipe delete <slug>
kb-code recipes                                  # the FROZEN recipes/1 catalog
```

**CLI break (v7.4):** the pre-v7.4 `kb-code recipe <NAME> --repo R` is now
`kb-code recipe run <NAME> --repo R`. `--save-as-set` posts the chosen
view's addresses to the existing `POST /api/sets` — this CLI never grows a
second set-creation path, and addresses with no path are reported as
skipped rather than lost.

### Not built here (L3a)

The recipe **home** SPA, the auto-form and the result-view UI are L3c;
tours/trails are L3b. The SHOULDs D11 marks — fixtures, the inbox drift
lane, save-as board/tour, trend and property-diff — are not built, and
`review` in `timeline` mode reports the finding-shaped events only,
saying so in its census rather than inventing an address for a verdict.

## Workspaces, worktrees and the `@ref` frame table (v7.5, V75-M1)

### Two nouns, and one older word that collides with them

D13 splits what the daemon used to call "a repo" in two:

* a **Workspace** is one shared git **object store**. Its id derives from
  the canonical `--git-common-dir` plus the root commit
  (`ws_` + 12 hex). Every blob, every commit and every git-history
  derivation belongs to it, whichever checkout you read them through.
* a **Worktree** is one **checkout**. Its id is the admin-directory name
  (`<common>/worktrees/<id>`, or the reserved `(main)` for the main
  worktree, which has none), and **its path is a mutable attribute** —
  `git worktree move` changes the path and not the identity.

There is a third, older use of the word in this daemon, and conflating the
two is the mistake this section exists to prevent: **D26's kbc-seq/1
already owns `reading_sets.workspace_id`** (V0029), where "workspace" means
a *reading set of kind `workspace`* — the Desk. `kb-code workspace`
(singular) is that one; `kb-code workspaces` (plural) is D13's. The re-key
never adds a column named `workspace_id` to a table that could be read as a
Desk, which is one of the two reasons the authored path-anchored tables
below take `worktree_id`.

### The classification table

Every table carrying a repo key declares which identity owns its rows
(`kb_code_server::rekey::REPO_KEYED_TABLES`; migration `V0040__workspace_
rekey.sql` adds the columns):

| class | column | tables |
| --- | --- | --- |
| `object` — a function of the OBJECT STORE (a blob, a commit, git history); two checkouts would derive the identical row | `workspace_id` | `author_stats` · `behavioral_meta` · `claims` · `cochange_pairs` · `comments` · `commit_sessions` · `doc_refs` · `entity_defs` · `lane_facts` · `lane_runs` · `path_stats` · `rails_edges` · `recipe_trust` · `scip_runs` · `session_signals` |
| `worktree` — a property of a PATH ON DISK in one checkout; sharing it would be a wrong answer, not a saving | `worktree_id` | `annotations` · `bookmarks` · `canvas_boards` · `canvas_sets` · `file_opens` · `files` · `reading_sets` · `recipe_runs` · `reviews` · `trails` |
| `meta` — about the daemon rather than about code | none | `repos` (the canonical HOLDER of both) · `worktrees` (the identity registry itself) · `mutations` (the audit ledger: re-keying it would rewrite history) · `doc_lens_pins` (a cross-daemon pin whose `repo` is a hint re-resolved per read) · `recipes_server` (its `repo` is an optional SCOPE — NULL means every repo — and a row that may belong to no repo cannot be owned by a workspace) |

The blob-keyed derived tables (`symbols`, `highlights`, `occurrences`,
`import_specs`, `call_sites`, `type_relations`, `chunk_status`,
`derived_status`) carry **no repo key at all** and never did — ADR-2 keyed
them by `(blob_hash, salt)` from V0001, so a second checkout of one
repository has always cost zero re-extraction for them. They are outside
the classification because they need no key, not because they were
forgotten.

`store::tests::v75_m1` walks `sqlite_master` against that table from both
ends: **a table added later with a `repo_id`/`repo` column fails the build
until it declares a class.** That walk is the re-key's teeth.

**Where the value comes from.** Not from Rust. Each keyed table carries an
`AFTER INSERT … WHEN NEW.<key> IS NULL` trigger, created next to the column
in V0040, that reads the identity off the `repos` row the row already
points at. One home, unforgettable by a future `INSERT`, and every `Store`
write signature unchanged. `Store::set_repo_identity` is the only writer of
`repos.workspace_id`/`worktree_id`.

**What the re-key does NOT do, and why.** It adds the key. It does not
collapse two checkouts' duplicate rows into one, and no read widens from
`repo_id = ?` to `workspace_id = ?`. Three reasons, and they compose: a
shared read over two repo ids would return each row twice; sharing needs
the PRIMARY KEY to become `(workspace_id, …)`, which in SQLite is a table
REBUILD; and a rebuild is O(every row) inside a migration, i.e. between
`Store::open` and the listener bind, which invariant 11's V72-B0(a)
forbids outright. There is also no surface today that registers two
worktrees of one workspace as two repos, so a widened read would be code
no configuration can reach. `rekey::READS_NOT_WIDENED` is the ledger of
what still needs the collapse, and the collapse belongs with the worktree
lifecycle verbs and their own one-way-door treatment.

The read that DOES go through the new key is the per-workspace derived-row
census on `GET /api/workspaces` (`derived`), and in
`kb-code workspaces --json`.

### Reads

`GET /api/workspaces` → `kbc-workspace/1`: every workspace with its
`common_dir`, `root_commit` (or `null` plus a `note` when HEAD is unborn),
the `[[repos]]` entries resolved to it, its worktrees, and the `derived`
census. `GET /api/workspaces/{id}/worktrees` is the same payload narrowed
to one (404 on an unknown id). Both are ordinary `auth_bearer` reads.

Each worktree row carries `path` (mutable), `branch`, `head_sha`,
`is_main`/`bare`/`detached`, `locked` + the lock reason **verbatim**,
`prunable` + git's own reason, and D13's honesty pair:

* `path_resolution` — `exact` (the worktree path IS a configured
  `[[repos]]` root) · `ancestor` (it is inside one) · `absent`;
* `mounted` — `path_resolution != absent`.

A worktree outside every configured root is listed as **"known, not
mounted"**, never hidden and never browsed. A prunable worktree is listed
too, with git's reason: reads skip it, but an operator needs to see it and
the lifecycle verbs need something to name. The lock reason is stored and
printed and **parsed by nothing** — D13's agent-owner oracle is a later
unit and can only ever mint `likely`.

Not to be confused with D14/M4's per-FILE `path_resolution`, which answers
"does this path exist at the target ref" for the reader. Same three words,
different subject.

`GET /api/repos` gains `workspace_id` and `worktree_id` **additively**,
read off `repos` rather than recomputed, so the two surfaces cannot
disagree. Both are `null` until resolution has run.

### `@ref` frames (D14)

`GET /api/frames` → `kbc-frames/1`, and `kb-code frames [--json]`. A Rust
`const` table (`kb_code_server::frames::FRAMES`), golden-pinned at
`tests/fixtures/frames.golden.json`, saying per lane where its answer comes
from and what it may claim about a ref that is not checked out:

| lane | source | off-HEAD ceiling |
| --- | --- | --- |
| `text`, `symbols`, `files` | working tree (via the mirror) | `refused` |
| `tree`, `file_at_ref` | ODB | `exact` |
| `blame` | git | `exact` |
| `usages` | working tree | `likely` |
| `framework_edges` | working tree | `candidate` |
| `lsp_live` | refused | `refused` |

`off_head` is a **ceiling, never a promise**; `ref_aware: false` means the
lane ignores a ref and answers for the checkout, which a reader must SAY
rather than silently substitute. This unit lays the table and consumes
none of it — the reader's ref chip, compare mode and per-lane banners
derive from these bytes rather than restating them.

### The migration treatment: backup, epoch, rehearsal

A schema epoch is a **one-way door** for a volume: `kb_core::sibling::
refuse_if_volume_ahead` makes an older binary refuse a forward-migrated
volume rather than silently regress it, and the only remedy it offers is
"restore the state backup matching epoch V\<n\>". V0040 therefore ships
with all three of:

**Backup.** `backup::ensure_for_epoch_crossing` runs inside `Store::open`,
ordered deliberately: AFTER `refuse_if_volume_ahead` (a volume this binary
must refuse is never snapshotted) and BEFORE the V72-B1 checksum repair,
which is itself a write — a snapshot taken after it would not be the
pre-migration state an operator would roll back to. The first boot that
would carry a volume across V0040 takes a
`VACUUM INTO` snapshot beside the database, named for the epoch it restores
to (e.g. `index.db.pre-V0038.bak` for a volume that had reached
V0038), and writes a `kbc-backup/1` receipt
(`<state>/kb-code/backup.marker`). If the snapshot cannot be written the
boot is **refused** with the reason, rather than migrating a volume nobody
can roll back. `VACUUM INTO` (not `cp`) because a live WAL that has not
checkpointed would otherwise restore torn. A second crossing boot reuses
the existing snapshot; a receipt whose file was deleted is not fresh.
`KB_CODE_I_HAVE_A_BACKUP=1` proceeds anyway for an operator holding a
filesystem-level snapshot, and logs a warning naming itself every time.

`kb-code backup [--db PATH] [--json]` takes the same snapshot on demand and
writes the same receipt, so the gate can be front-run before a deploy
window. It is a LOCAL FILE operation: no daemon, no route, no new mutation
surface — a backup you can only take through a running daemon is exactly
the one you cannot take when the daemon refuses to boot.

**Epoch.** Nothing is bumped by hand. `store::schema_epoch()` reads the
highest EMBEDDED migration version, so landing `V0040__workspace_rekey.sql`
*is* the bump; it rides `GET /api/identity`'s `schema_epoch`, and
`kb-code doctor`'s `sibling_handshake` check already fails on CLI/daemon
skew.

**Rehearsal.** `kb-code rehearse-migration --from PATH [--keep] [--json]`
→ `rehearsal/1`. It `VACUUM INTO`-copies the volume to a throwaway
directory beside it, runs the real `Store::open` (so the backup gate and
every repair run exactly as they would live), stamps a clearly-fake
identity on the COPY's `repos` rows so the backfill genuinely writes every
row, runs the paged backfill to completion, and reports: the epoch before
and after, the snapshot the gate took, a per-table row census (**a re-key
adds columns, never rows**), per-table keyed/unkeyed counts, and the
backfill's wall clock on a volume that size. The source is never written;
the copy is deleted unless `--keep`; a non-empty `problems` list exits
non-zero.

Two exemptions the census states rather than hides:
`refinery_schema_history` gains exactly one row per applied migration —
that IS the migration happening — and a table the migration CREATED is
noted as such rather than read as a row count that moved. Both were found
by the first real run of this verb against a 2.9 GB volume, which is what a
rehearsal is for; the same run found that the backup gate had been written
and never WIRED into `Store::open`.

### The backfill

Rows that predate the triggers are stamped by a **paged background pass**
(`rekey::spawn_rekey`), spawned by `bind_and_spawn` and never awaited —
V72-B0(a)'s rule, and sharper here because resolution runs `git rev-list
--max-parents=0 HEAD`, which walks the whole reachable history. That walk
is paid ONCE per volume: the recorded root commit is reused on every later
boot.

The pass resolves each `[[repos]]` entry to its workspace + worktree, then
walks each keyed table by `rowid` in pages of 512, one short transaction
each, sleeping between pages so the store's single connection mutex is
genuinely released. The cursor lives in the `rekey_progress` table, written
in the SAME transaction as the page it describes (V72-B0's marker was a
sidecar FILE only to avoid bumping the refinery epoch — this unit bumps it
anyway, and a row cannot disagree with its own page). The UPDATE's
`<key> IS NULL` predicate makes a replayed page a no-op, so a crash costs
one page. A per-boot wall-clock budget stops the pass and the next boot
resumes; a repo added, removed or re-pointed changes the recorded
fingerprint and the tables are walked again (a rowid scan, not a rewrite).

`GET /api/identity` reports `rekey: pending | running | done` and
`kb-code doctor` prints it as an INFORMATIONAL check (it never fails
doctor: `pending` is a normal state on a daemon that has just booted).
Every route answers correctly in all three states — `Store::
workspace_repo_ids` falls back to the caller's own repo id while the
identity is unresolved. What `pending` tells you is that
`GET /api/workspaces` may be EMPTY because resolution has not finished,
which is a different statement from "there are no workspaces".

### Not in this unit

The worktree LIFECYCLE verbs (`create`/`lock`/`unlock`/`repair`/`prune`),
the branch views, the reader's `@ref` chip and compare mode, and the
transcript scrubber are each their own unit. Nothing here mutates a
worktree; `checkout.rs`'s loopback-only working-tree lane is untouched and
remains the only place this daemon changes a checkout.
