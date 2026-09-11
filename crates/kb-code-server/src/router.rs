//! Top-level router: `/api/*` behind the imported `auth_bearer` layer,
//! `/healthz` on the top-level router outside the `/api` nest (so it stays
//! unauthenticated) — mirrors kb-server's router shape
//! (`crates/kb-server/src/router.rs`). W1.2 shipped `/api/identity`; W1.6
//! adds the browsing surface (`repos`/`tree`/`file`/`symbols`/`events`);
//! W2.1 adds the INSTANT search lanes (`search/{files,symbols,text}`);
//! W2.3 adds the SEMANTIC lane (`search/semantic`, `routes::search_semantic`
//! — 400s when `[semantic]` is off or the requested repo isn't opted in,
//! rather than silently returning empty results).
//!
//! **W2.5** adds `search/transcripts` + `transcripts/status` — mounted as a
//! SEPARATE sub-router layered with `transcripts::search::loopback_only`
//! INSTEAD OF `auth_bearer` (then `.merge`d into the same `/api` nest): a
//! valid bearer token must never open the raw-transcripts lane to a
//! non-loopback caller (see that module's doc). Two disjoint route sets
//! under one nest, each with its own middleware stack — `Router::merge`
//! preserves per-route layering, so this is NOT the same as layering both
//! middlewares onto one combined router.
//!
//! **W2.4** adds `GET /search` (the unified Search-Everywhere box,
//! `routes::search_unified` / `search::unified`) — mounted on the ordinary
//! `auth_bearer`-gated `api` router (NOT the transcripts sub-router): the
//! box itself is an ordinary authenticated route, but its handler
//! separately re-derives loopback-ness (via `state.auth`) to decide whether
//! ITS OWN transcripts SECTION is present, mirroring `loopback_only`'s rule
//! without replacing `auth_bearer` for the whole route (see
//! `search::unified`'s module doc).
//!
//! **W3.1** adds `GET /blame` + `GET /blame/timeline` (the BLAME service,
//! `crate::blame` — `routes::blame` / `routes::blame_timeline`), mounted on
//! the same ordinary `auth_bearer`-gated `api` router as every other
//! browsing route.
//!
//! **W3.2** adds `GET /join/commit` (`routes::join_commit` /
//! `join::ladder::resolve_commit`) — the join ladder's HTTP surface,
//! ordinary `auth_bearer`-gated (no loopback carve-out; unlike the
//! transcripts lane, session↔commit attribution carries nothing more
//! sensitive than the repo browsing surface already does).
//!
//! **W3.3 + W3.4** add `GET /provenance-report` (`provenance::report::
//! provenance_report_route`), `GET /why` (`provenance::why::why`), and
//! `GET /story` (`provenance::story::story`) — same ordinary
//! `auth_bearer`-gated `api` router as `/join/commit` above. `/why`'s
//! uncommitted-line path reads the W2.5 transcripts store, but ONLY ever
//! returns opaque session ids (never transcript text) — the same
//! information class `/join/commit` already exposes over this same gate —
//! so it deliberately does NOT move to the transcripts lane's stricter
//! `loopback_only` guard below. `/why`'s line-grade path additionally folds
//! in kb's own `kb_context` (prompt excerpt + decision text) when it
//! resolves a `session_id` — that field carries real transcript-derived
//! TEXT, not an opaque id, so the handler re-derives loopback-ness itself
//! (`kb_server::middleware::is_loopback_origin`, same technique
//! `routes::search_unified` uses for its transcripts section) and skips the
//! `kb_context` follow-up entirely for a non-loopback caller — attribution
//! (session ids, confidence, `via`) stays available either way. See
//! `provenance::why`'s module doc ("Sensitivity") for the full rule.
//!
//! **W3.5** adds `GET /session-diff` (`routes::session_diff_route` /
//! `sessiondiff::session_diff`) — mounted on the transcripts sub-router,
//! LOOPBACK-ONLY like `search/transcripts`/`transcripts/status` above (NOT
//! `auth_bearer`): unlike `join/commit`, this route's payload carries raw
//! transcript prompt text (truncated, but still operator-authored chat
//! content), so it inherits the same "never in any non-loopback response"
//! rule the raw-transcripts lane already enforces — see `sessiondiff`'s
//! module doc.
//!
//! **W3.6** adds `POST /backfill` (`routes::backfill_route` /
//! `join::backfill::backfill_repo`) — the join ladder's PRECOMPUTE, same
//! ordinary `auth_bearer`-gated `api` router as `/join/commit`: its
//! response is a bulk aggregation of the SAME `Attribution` shape
//! `/join/commit` already exposes over this gate, so it inherits that
//! route's (not the transcripts lane's) sensitivity class.
//!
//! **W4.1/W4.2** add `GET /refs` (`routes::refs`) and
//! `GET /diff` (`routes::diff_route`) to the same ordinary `auth_bearer`-
//! gated `api` router — neither exposes anything more sensitive than
//! `tree`/`file` already do (see each route's own doc) — and a top-level
//! `.fallback(spa::serve)` that serves the built `web-code/` SPA (or a
//! friendly 404 JSON body when the daemon booted without one). The
//! fallback is registered AFTER `/api` and `/healthz` (axum tries explicit
//! routes first; a fallback only ever sees what neither matched), mirroring
//! kb-server's `Router::fallback(routes::dispatch::fallback)` placement
//! (`crates/kb-server/src/router.rs`) — see `spa`'s own module doc for what
//! kb-code's version deliberately trims from that precedent.
//!
//! **W4.6** adds `GET`/`POST /annotations` + `PATCH`/`DELETE
//! /annotations/{id}` (`routes::{list,create,patch,delete}_annotation`) —
//! same ordinary `auth_bearer`-gated `api` router as the rest of the
//! browsing surface: an annotation is a durable comment, no more sensitive
//! than a `/blame`/`/why` response already is.
//!
//! **Phase D-server** ("The Operable Reader," annotations v2) adds
//! `GET /annotations/open` (`routes::list_open_annotations`) — the
//! repo-wide unresolved-annotations query surface (the agent hook's +
//! a future dashboard's data source), same `auth_bearer`-gated `api` router
//! as `/annotations` itself: it exposes nothing `GET /annotations?repo=&
//! path=` doesn't already, just fanned out across every path in the repo at
//! once. Registered as its own literal route (not folded into
//! `/annotations/{id}`'s path-param route) — axum's router already
//! prioritizes a literal segment over a param one at the same position, so
//! registration order here doesn't matter, but it's listed first below for
//! readability.
//!
//! **W4.7** adds `POST /checkout` (`routes::checkout_route` /
//! `crate::checkout::switch_repo`) — mounted on the SAME LOOPBACK-ONLY
//! sub-router `search/transcripts`/`session-diff` already use, NOT
//! `auth_bearer`: it is the daemon's first sanctioned working-tree
//! mutation (wave-4 operator ruling; V4.S1 apply is the second), so it
//! gets a strictly stronger gate than every other route in this router —
//! loopback-only regardless of any bearer token, not just `auth_bearer`'s
//! loopback-bypasses-the-token-check rule.
//!
//! **W5.1 + W5.2** add the agent context verbs — `GET /map`
//! (`agentview::map::map_route`), `GET /pack` (`agentview::pack::
//! pack_route`), `GET /defs` (`agentview::xref::defs_route`), `GET /xrefs`
//! (`agentview::xref::refs_route`), `GET /similar` (`agentview::similar::
//! similar_route`), `GET /impact` (`agentview::impact::impact_route`) —
//! all on the SAME ordinary `auth_bearer`-gated `api` router as everything
//! above: none of them exposes anything more sensitive than `GET
//! /api/file`/`GET /api/why` already do (see `agentview`'s module doc).
//! `xref::refs_route` mounts at `/xrefs`, NOT `/refs` — W4.1 (just above)
//! already claimed `/refs` for the git ref-picker before this Wave's own
//! commit was written (the two were developed in parallel against
//! different bases), so this cherry-pick resolution renames the newer
//! route rather than let `Router::route` panic on a duplicate `GET /refs`
//! registration; `kb-code-cli`'s `xrefs` verb (as opposed to the
//! pre-existing `refs` verb) already anticipated this naming at the CLI
//! layer.
//!
//! **B2** adds `GET /resolve` (`resolve::resolve_route`) — the token-level
//! position-resolve endpoint, same ordinary `auth_bearer`-gated `api`
//! router as `defs`/`xrefs`: it exposes nothing more sensitive than those
//! two already do (symbol/occurrence names + locations), just keyed by a
//! caller-given position instead of a caller-given name.
//!
//! **Phase C-server** ("The Operable Reader," time-first-class) adds
//! `GET /commit` (`routes::commit_route`), `GET /compare`
//! (`routes::compare_route`), `GET /branches` (`routes::branches_route`),
//! and `GET /file-history` (`routes::file_history_route`) — same ordinary
//! `auth_bearer`-gated `api` router as `/join/commit`/`/diff`: see
//! `routes.rs`'s own doc comment on that section for why none of these
//! needs a stronger gate.
//!
//! **V3.3-S2** adds `GET /stacks` (`routes::stacks_route`) and
//! `GET /stacks/layer-diff` (`routes::stacks_layer_diff_route`) — stack
//! awareness over local branch tips (dependent-branch detection +
//! per-layer incremental `base_tip..tip` diff). Same ordinary
//! `auth_bearer` gate as `/branches`/`/compare`; pure derivation, no
//! ref writes.
//!
//! **Phase G-server** ("The Operable Reader," the review-workflow
//! endpoints) adds `GET /merge-check` (`routes::merge_check_route`),
//! `GET /range-diff` (`routes::range_diff_route`), and `GET /repo-state`
//! (`routes::repo_state_route`) to the same ordinary `auth_bearer`-gated
//! `api` router as `/commit`/`/compare` — none of these exposes anything
//! more sensitive than a `git merge-tree`/`git range-diff`/`git status` at
//! a terminal already would. `GET /prs` (`routes::prs_route`) and
//! `GET /prs/{number}/comments` (`routes::pr_comments_route`) join them on
//! the same gate — a read-only overlay onto the repo's already-public(ish)
//! GitHub origin, degrading to `unavailable_reason` rather than erroring.
//! `POST /prs/fetch` (`routes::prs_fetch_route`) is different: it is the
//! ONE new git ref-write this daemon performs outside `checkout::
//! switch_repo` (into `refs/kbc/pr/<n>`, never touching the working tree
//! or HEAD), so it rides the SAME loopback-only sub-router `checkout`/
//! `session-diff` use, not `auth_bearer`.
//!
//! **Phase E3** adds `GET`/`POST /sets`, `GET`/`PATCH`/`DELETE
//! /sets/{id}`, and `POST /sets/{id}/spans` (`reading_sets::{list_sets,
//! create_set, get_set, patch_set, delete_set, append_span}`) — reading
//! sets: named, ordered collections of file/span references. Same ordinary
//! `auth_bearer`-gated `api` router as `/annotations`: a reading set is no
//! more sensitive than the browsing surface it references. `POST
//! /sets/from-session` (`reading_sets::from_session_route`) is different —
//! it reuses `sessiondiff::session_diff`'s narrative assembly (raw
//! transcript prompt text), so it rides the SAME loopback-only sub-router
//! `session-diff`/`checkout` use instead. `/sets/from-session` (a literal
//! route on that sub-router) and `/sets/{id}` (a param route on the
//! ordinary one) coexist the same way `/annotations/open` and
//! `/annotations/{id}` already do — axum prioritizes the literal segment
//! regardless of which sub-router registered it or merge order.
//!
//! **S1** adds `POST /scip/ingest` (`scip::scip_ingest_route`) — the SCIP
//! precision tier's ingest endpoint (`kb-code scip ingest`). Rides the SAME
//! loopback-only sub-router `checkout`/`session-diff` use, NOT `auth_bearer`:
//! it is a bulk WRITE of exact-tier occurrence data (same class of
//! sensitivity as `checkout`'s working-tree mutation — an operator-trusted
//! bulk operation, not an ordinary browsing-class read), so it gets the
//! strictly stronger loopback-only gate rather than the ordinary bearer
//! check every `GET` route above rides.
//!
//! **Phase N** ("Navigate") adds `GET`/`POST /bookmarks`,
//! `PATCH`/`DELETE /bookmarks/{id}` (`bookmarks::{list,create,patch,delete}_
//! bookmark`), `GET /todos` (`routes::list_todos` — a filtered view over
//! `comments/1` since V72-J1), the four `GET /comments*` reads, and `GET /scopes`
//! (`routes::list_scopes`) — same ordinary `auth_bearer`-gated `api` router
//! as `/sets`/`/annotations`: bookmarks are operator-owned places no more
//! sensitive than a reading set; todos/scopes are pure index reads.
//!
//! **V3.R1** ("Review cockpit") adds local Gerrit-lite review sessions
//! (`crate::reviews`): read routes (`GET /reviews`, `GET /reviews/{id}`,
//! `GET /reviews/{id}/files`, `GET /reviews/{id}/interdiff`,
//! `GET /reviews/{id}/annotations`) on the ordinary `auth_bearer`-gated
//! `api` router; mutation routes (`POST /reviews`, `POST
//! /reviews/{id}/snapshot`, `PATCH`/`DELETE /reviews/{id}`,
//! `PUT /reviews/{id}/viewed`, `DELETE /reviews/{id}/viewed/{path}`, and
//! V73-K2a's per-hunk twin `PUT /reviews/{id}/hunk-viewed` /
//! `DELETE /reviews/{id}/hunk-viewed/{hunk_id}`) on the
//! SAME loopback-only sub-router `checkout`/`prs/fetch` use — they write
//! `refs/kbc/review/<id>/ps<n>` (the second ref-write namespace after
//! `refs/kbc/pr/<n>`).
//!
//! **V4.C1** adds `GET /reviews/{id}/comments` (`crate::review_comments`)
//! on the ordinary `auth_bearer` read surface — review-scoped annotations
//! with lazy carry-forward resolution. Distinct from
//! `/reviews/{id}/annotations` (the existing path-intersection listing,
//! left untouched).
//!
//! **V4.C2** adds the verdict write surface (`PUT`/`DELETE
//! /reviews/{id}/verdict`) on the SAME loopback-only sub-router review
//! mutations use (S2-B below later graduates this pair onto the gated
//! `review_remote` sub-router — see that unit's own paragraph), and the
//! suggestion + batch write surface (`PUT`/`DELETE /annotations/{id}/
//! suggestion`, `POST /annotations/batch`) on the ordinary `auth_bearer`
//! annotation-mutation family (beside `PATCH /annotations/{id}`).
//!
//! **V4.S1** adds `POST /annotations/{id}/apply` (`crate::suggestions`)
//! on the SAME LOOPBACK-ONLY sub-router `checkout` uses — the daemon's
//! second sanctioned working-tree mutation. Bearer suggestion *storage*
//! stays on `auth_bearer`; the splice itself is loopback-only regardless
//! of any token. See that module's doc for the exact-match guard, the
//! `applied`-is-an-act rule, and the no-mirror-wiring principle.
//!
//! **V3.3-S1 R7** adds `GET /reviews/{id}/map` + `GET /reviews/{id}/reading-order`
//! (`crate::review_map`) on the ordinary `auth_bearer` read surface — pure
//! composition over import/call edges + the patchset file list; no mutations.
//!
//! **V3.4-C1** adds canvas-set persistence (`crate::canvas`): `GET /canvas`
//! and `GET /canvas/{id}` on the ordinary `auth_bearer` read surface;
//! mutations (`POST /canvas`, `PUT`/`DELETE /canvas/{id}`) on the SAME
//! loopback-only sub-router review mutations use — server durability only,
//! opaque client payload (256 KiB hard cap). Also
//! `GET /behavioral/timeseries` on the ordinary read surface —
//! derived-at-request-time weekly activity buckets (attention signal, never
//! a "health" verdict); reuses behavioral ingest's `git log --numstat -M`
//! walk + parse, no storage.
//!
//! **DCB W1.C** adds the doc↔code lens (`crate::doclens`): `GET /doc-lens`,
//! `GET /doc-lens/repos` and `PUT`/`DELETE /doc-lens/pin` (W2.A adds
//! `GET /doc-lens/pins`). All of them ride
//! `auth_bearer` like the rest of the browsing surface, but they are the
//! ONLY routes in this router that carry CORS headers — mounted on TWO
//! dedicated sub-routers so the layer can never reach `/api/file` or the
//! loopback-only transcripts lane. See the block comment at the merge point
//! below, `crate::doclens`'s module doc for the full route table, and
//! `tests/doclens/doclens_route.rs`'s `cors_layer_route_set_is_pinned` for the
//! assertion that keeps the two in lock-step.
//!
//! **SL7e** (v0.42) adds `GET /doc-lens/path` — the path-addressed lens kb's
//! slate board captions with — to `doclens_read`, CORS'd like the reads
//! beside it.
//!
//! **DCB W2.B** adds `GET /doc-lens/resolve-path` (`doclens::wire::
//! resolve_path_route`) — the path-addressed lens entry ramp's server side
//! (R2/R20). Registered on THIS plain `auth_bearer` `api` router below,
//! deliberately NOT on either `doclens_read`/`doclens_pin` sub-router: it is
//! a `doclens`-owned handler that must NOT carry CORS headers (same-origin
//! is all web-code's own `LensEntryByPath.tsx` needs, and a corpus-wide
//! path→id existence oracle behind an exact-origin ACAO is a surface no
//! consumer asked for) — `cors_layer_route_set_is_pinned` asserts it absent
//! from the ACAO-carrying set.
//!
//! **DCB W3.A** adds the reverse "cited by" index (`crate::doclens::sync`):
//! `GET /doc-refs` on the plain `auth_bearer` `api` router (same reasoning as
//! `/doc-lens/resolve-path` — same-origin only, asserted ABSENT from the
//! ACAO set) and `POST /doc-lens/sync` on the LOOPBACK-ONLY
//! `transcripts_api` sub-router. The sync trigger deliberately does not
//! follow the pin onto `auth_bearer` (D-A: "`/api/doc-lens/sync` STAYS
//! loopback-only") — it drives a bulk pull of document prose into this
//! daemon's store, not a one-checkout preference.
//!
//! **2026-08-16 drift repair** adds `GET /events.schema.json` +
//! `GET /events/schema/{kind}/{version}` (`crate::schema`, the new module
//! mirroring kb-server's own `routes/schema.rs`) beside `/events` on this
//! SAME ordinary `auth_bearer`-gated `api` router — same sensitivity class
//! as the event stream it introspects (a schema enum, no repo content).
//!
//! **CT-E7** adds `GET /reviews/{id}/distill` (`crate::review_distill`) on
//! the SAME ordinary `auth_bearer` read surface the rest of the review
//! family uses — one deterministic JSON dump of a review's full local
//! record (meta, patchsets, files, verdict, every comment thread,
//! suggestions incl. applied audit) for the AGENT layer to journal into
//! kb. Composed from existing tables/refs only; no new storage, no kb
//! call — the doc↔code bridge's ONE call direction (kb-code→kb) stays
//! untouched because this route doesn't use that lane at all.
//!
//! **PRR-R2** ("The PR Room," kb v0.39 T2, Phase 2) adds `POST /reviews/pr`
//! (`reviews::create_review_pr`) and `PUT /reviews/{id}/report`
//! (`reviews::put_review_report`) to the loopback-only review-mutation
//! family below (git-fetch/row-write and wholesale-report-replace are both
//! real side effects, same tier as `create_review`/`put_verdict`); `GET
//! /reviews/{id}/report` (`reviews::get_review_report`) and `GET
//! /reviews/{id}/artifact` (`reviews::get_review_artifact` — the ONE new
//! kb-code→kb call this unit adds, `join::kb_client::KbClient::doc_meta`,
//! invariant #2's call-direction law unchanged) join the ordinary
//! `auth_bearer` review-read surface beside `/distill`/`/comments`. `GET
//! /prs/{number}`, `GET /prs/{number}/checks`, and `GET /prs/{number}/
//! reviews` (addendum-2 §A) extend the existing read-only GitHub overlay
//! (`/prs`, `/prs/{number}/comments`) with the same `unavailable_reason`
//! degrade posture — no route in this unit introduces a third auth tier.
//!
//! **PRR-R3** ("The PR Room," kb v0.39 T2, Phase 3 — `crate::
//! review_findings`) adds the findings surface. Bearer read: `GET
//! /reviews/{id}/findings` ([`crate::review_findings::list_findings_
//! route`]), same ordinary `auth_bearer` review-read gate as `/comments`/
//! `/distill`/`/report` — a finding is no more sensitive than any other
//! review comment. Mutations (originally ALL loopback, `transcripts_api`
//! below — S2-B later graduates two of the three onto the gated
//! `review_remote` sub-router, see that unit's own paragraph): `POST
//! /reviews/{id}/findings/import` ([`import_findings_
//! route`](crate::review_findings::import_findings_route), STILL
//! loopback-only — an agent-side batch-reconcile verb, not a mobile-
//! mutation candidate), `POST /reviews/{id}/findings`
//! ([`create_manual_finding_route`](crate::
//! review_findings::create_manual_finding_route), addendum §E — a single
//! human-authored finding, NOW on `review_remote`), `PUT`/`DELETE
//! /reviews/{id}/findings/{slug}/disposition`
//! ([`set_finding_disposition_route`](crate::review_findings::
//! set_finding_disposition_route) / [`clear_finding_disposition_route`]
//! (crate::review_findings::clear_finding_disposition_route), NOW on
//! `review_remote`) — same tier as `/verdict`/`/report` (a real store
//! write, batch-transactional for import). `/findings/import` and
//! `/findings` both write via the SAME `POST /reviews/{id}/findings...`
//! path prefix as bare `/reviews/{id}` writes elsewhere in this router,
//! but neither collides — regardless of which of the THREE sub-routers
//! (`api`/`transcripts_api`/`review_remote`) each verb ends up on:
//! `/findings` (literal) and `/findings/import` (literal) both sit ahead
//! of `/findings/{slug}/disposition` (nested param) — axum resolves
//! literals over params regardless of registration order, the SAME
//! precedent `/annotations/open`-before-`/annotations/{id}` and
//! `/reviews/gc`-before-`/reviews/{id}` already established in this file.
//!
//! **PRR-R4** ("The PR Room," kb v0.39 T2, Phase 4) adds three bearer reads
//! to the SAME ordinary `auth_bearer` review-read surface: `GET
//! /reviews/{id}/pr-status` ([`reviews::pr_status_route`], design doc §2
//! row 12 — the staleness probe; its LIVE half degrades to `unavailable_
//! reason` on any GitHub-side failure, same posture as `/artifact`/`/prs/*`),
//! `GET /reviews/inbox` ([`review_inbox::list_inbox_route`], design doc §2
//! row 13 — a cross-repo attention queue, LOCAL-only, zero live GitHub
//! calls), and `GET /reviews/{id}/timeline`
//! ([`review_timeline::review_timeline_route`], milestone plan arbitration
//! #7 — pure composition of existing rows, no new storage). `GET /reviews`
//! and `GET /reviews/{id}` also gain additive PR-binding/report-summary
//! fields (the R2 leftover, see `reviews::pr_binding_and_report_fields`'s
//! own doc) — no route shape changes, only new keys on the existing
//! responses.
//!
//! **PRR-N5** adds `GET /framework/edges` (`crate::framework_edges::
//! framework_edges_route`), `GET /hover` (`crate::hover::hover_route`), and
//! `GET /resolve-symbol` (`crate::symbol_addr::resolve_symbol_route`) — all
//! on the SAME ordinary `auth_bearer`-gated `api` router as `/resolve`/
//! `/usages`: none exposes anything more sensitive than those two already
//! do (rails_edges rows + symbols-table rows, keyed by path/position/name
//! instead of only position).
//!
//! **PRR-R7** (design-addendum-2.md §A — GitHub thread import) adds `GET
//! /reviews/{id}/github-threads` ([`review_github_threads::
//! github_threads_route`], `crate::review_github_threads`'s own doc has the
//! position-mapping contract). Same ordinary `auth_bearer` review-read
//! surface as `/pr-status`/`/timeline`; a live GitHub read, nothing
//! persisted — degrades to `unavailable_reason` on any GitHub-side failure,
//! same posture as `/prs/*`/`/artifact`.
//!
//! **S2-B** ("Mobile mutations," `/tmp/design-s2.md`, kb-code v6.0) moves
//! FIVE route families off pure loopback-only onto a NEW third sub-router,
//! `review_remote`, gated by `[review] remote_mutations`
//! (`config::ReviewSection`, default OFF — see [`review_gate::
//! review_mutations_gate`]'s own module doc for the full admission table):
//! `PUT`/`DELETE /reviews/{id}/findings/{slug}/disposition` (PRR-R3, was on
//! `transcripts_api`), `PUT`/`DELETE /reviews/{id}/verdict` (V4.C2, was on
//! `transcripts_api`), `POST /reviews/{id}/findings/{slug}/published` and
//! `POST /reviews/{id}/verdict/published` (PRR-R5, was on `transcripts_api`),
//! and `POST /reviews/{id}/findings` (manual create, PRR-R3, was on
//! `transcripts_api`). `review_remote` layers `auth_bearer` FIRST (chained
//! first — INNER) and `review_mutations_gate` LAST (chained last — OUTER,
//! the SAME "chained-last-is-outermost" rule the `doclens_read`
//! CORS-vs-`auth_bearer` ordering comment below documents), so the gate
//! always decides admission before `auth_bearer`'s token check runs: a
//! loopback peer admits unconditionally (unchanged); a non-loopback caller
//! 404s when the flag is off (byte-identical to `loopback_only`'s refusal
//! — `auth_bearer` never runs); a non-loopback caller falls through to the
//! ordinary bearer 401/200 check when the flag is on. EVERY OTHER review
//! mutation (create/snapshot/patch/delete/viewed/gc, `/reviews/pr`,
//! `/reviews/sweep`, `/reviews/{id}/report` PUT, and `/findings/import`)
//! stays on `transcripts_api`, untouched by this flag — the working-tree
//! mutation lane (`checkout`, suggestion apply/apply-batch, `scip/ingest`,
//! `prs/fetch`) is likewise UNTOUCHED, pinned by
//! `tests/review/remote_mutations_gate.rs`'s never-moves coverage.
//!
//! **S2-A** ("One Inbox," kb-code v6.0, design doc `/tmp/design-s2.md` §
//! S2-A) adds `GET /inbox` ([`unified_inbox::unified_inbox_route`]) — a
//! federated three-lane attention queue (reviews + working-tree
//! annotations + a live kb pull); see that module's own doc. Same
//! ordinary `auth_bearer`-gated `api` router as `/reviews/inbox`, right
//! beside it.
//!
//! **S2-C** (design-s2.md § S2-C) adds `POST /api/code-actions`
//! (`crate::code_actions::code_actions_route`) beside `/diagnostics` on
//! the same ordinary `auth_bearer`-gated `api` router — a caller-chosen
//! RANGE instead of a caller-chosen position, same sensitivity class.
//! NOTHING persisted; converting an action into a durable suggestion rides
//! the EXISTING `POST /api/annotations/batch` op, not a new mutation route.
//!
//! **V70-A2** ("local-daemon hardening," the v7 security critique's
//! SEC-02/SEC-20) adds a FOURTH layer class, and it is the first one that
//! is not per-sub-router: three middlewares layered on the MERGED `/api`
//! nest, so they cover `api`, `transcripts_api`, `review_remote` and both
//! `doclens` sub-routers at once, and a route added to any of them
//! inherits them without a line of new wiring.
//!
//! Outermost first (chained LAST is outermost — the same rule the
//! `doclens_read` CORS-vs-`auth_bearer` comment below documents):
//!
//! 1. [`security::origin::origin_host_guard`] — the Origin + Host
//!    allowlist. Runs before every existing gate, so a rebound `Host` or a
//!    foreign `Origin` is refused whether the route is `auth_bearer`'d,
//!    loopback-only or `review_gate`d.
//! 2. [`security::origin::mutation_header_guard`] — `X-Kbc-Request: 1` on
//!    mutating requests.
//! 3. [`security::audit::audit_mutations`] — one `mutations` row per
//!    mutating request, success or failure. INSIDE the two guards on
//!    purpose: a request the allowlist refused never reached a handler, so
//!    it is not a mutation that happened.
//!
//! ## Why there is no CSRF token in this router
//!
//! The critique's fix list asks for one. `security::origin`'s module doc
//! carries the full argument; the short form is that `X-Kbc-Request` IS
//! the credential a cross-origin page cannot produce (it forces a
//! preflight this daemon answers only on the operator-configured doc-lens
//! sub-routers), and a token minted into the SPA bootstrap would add a
//! durable readable secret while defending against nothing the header +
//! allowlist pair does not already cover. A same-origin XSS defeats both
//! equally — that is CSP's job (`crate::spa`), not a token's.
//!
//! It also adds `GET /api/audit` ([`security::audit::audit_route`]) on the
//! ordinary `auth_bearer` read surface — see that fn's own doc for why the
//! ledger is a bearer read rather than loopback-only.

use crate::agentview;
use crate::bookmarks;
use crate::canvas;
use crate::claims;
use crate::doclens;
use crate::provenance;
use crate::reading_sets;
use crate::resolve;
use crate::review_analytics;
use crate::review_distill;
use crate::review_gate;
use crate::review_github_threads;
use crate::review_impact;
use crate::review_inbox;
use crate::review_map;
use crate::review_pseudo;
use crate::review_sweep;
use crate::review_timeline;
use crate::reviews;
use crate::routes;
use crate::scip;
use crate::security;
use crate::spa;
use crate::state::SharedState;
use crate::suggestions;
use crate::transcripts;
use crate::unified_inbox;
use axum::{
    middleware::from_fn_with_state,
    routing::{delete, get, patch, post, put},
    Router,
};
use kb_server::middleware::auth_bearer;
use kb_server::state::AuthConfig;
use std::sync::Arc;

pub fn build_router(state: SharedState, auth: Arc<AuthConfig>) -> Router {
    let api = Router::new()
        .route("/identity", get(routes::identity))
        // V70-A2 (SEC-20) — the mutations audit ledger's read half.
        .route("/audit", get(security::audit::audit_route))
        // V70-A8 (D20) — the self-description surface: `kb-code tools`
        // walks the CLI's own clap tree (no daemon round trip); `kb-code
        // schema` drives these two.
        .route("/schemas", get(crate::api_schemas::list_schemas_route))
        .route("/schemas/{name}", get(crate::api_schemas::get_schema_route))
        .route("/repos", get(routes::repos))
        // V75-M1 (D13/D14) — the Workspace re-key's two reads and the
        // `@ref` frame table. Ordinary `auth_bearer` reads on the same
        // sub-router as `/repos`: they carry repository PATHS and git
        // state, which `/repos` already reports, and no file content at
        // all. `workspace::WORKSPACES_ROUTE` and `frames::FRAMES_ROUTE`
        // declare them for invariant 15's contract walk; the `{id}`
        // sub-route is registered here and covered by its own route test
        // (a `RouteContract` describes a query-param surface and has
        // nothing to say about a path segment).
        .route("/workspaces", get(crate::workspace::workspaces_route))
        .route(
            "/workspaces/{id}/worktrees",
            get(crate::workspace::workspace_worktrees_route),
        )
        .route("/frames", get(crate::frames::frames_route))
        // V70-A7 — `GET /api/themes` (`routes::themes`) serves the
        // kbc-theme/1 registry verbatim from `include_str!`, on the same
        // ordinary `auth_bearer`-gated `api` router as `/repos`: it is a
        // pure read of build-time data, carries no repo content, and has no
        // loopback concern.
        .route("/themes", get(routes::themes))
        // V70-A5 — `GET /api/commands` (`routes::commands`) serves the
        // kbc-cmd/1 registry verbatim from `include_str!`, ETag-validated,
        // on the same ordinary `auth_bearer` surface as `/themes`: build-time
        // data, no repo content, no loopback concern. The CLI embeds the SAME
        // file, so `kb-code commands …` and the SPA can never disagree.
        .route("/commands", get(routes::commands))
        // V72-H1 (D7) — `syntax/1`: `GET /api/syntax` is the file-type
        // registry (grammar, extraction tier, injection host, extensions/
        // stems/shebangs) and `GET /api/parity` is the Parity Grid derived
        // from it. Both are build-time data with no params and no repo
        // content, on the same ordinary `auth_bearer` surface as
        // `/themes` and `/commands`. `crate::syntax::V72_H1_ROUTES`
        // declares the pair; a unit test walks that declaration against
        // THIS file, so a route added there without a registration here
        // fails the build rather than shipping dead.
        .route("/syntax", get(crate::syntax::syntax_route))
        .route("/parity", get(crate::syntax::parity_route))
        // V72-H4a (D7, §P8) — `aug-lane/1`'s three READS. A bearer caller
        // may read facts; only loopback may write them, so the ingest
        // route lives on `transcripts_api` below rather than here.
        // `crate::lanes::V72_H4A_ROUTES` declares all four and a unit test
        // walks that declaration against THIS file.
        .route("/lanes", get(crate::lanes::routes::lanes_route))
        .route("/lanes/facts", get(crate::lanes::routes::facts_route))
        .route("/lanes/summary", get(crate::lanes::routes::summary_route))
        .route("/tree", get(routes::tree))
        .route("/file", get(routes::file))
        .route("/symbols", get(routes::symbols))
        // V72-H2a — `outline/1`, the universal per-file outline. An
        // ordinary `auth_bearer` read beside `/symbols`, whose rows it is
        // a VIEW of; `crate::outline::V72_H2A_ROUTES` declares it and a
        // unit test walks that declaration against THIS file.
        .route("/outline", get(crate::outline::outline_route))
        // V72-H2b (D7) — `reextract-bill/1`. An ordinary read: it times
        // the extractors over a bounded sample and writes nothing.
        .route("/reextract/bill", get(crate::reextract::bill_route))
        .route("/refs", get(routes::refs))
        // V70-A3X — `GET /api/status` (`git_status`'s module doc): working-
        // tree/index status, same ordinary `auth_bearer`-gated `api` router
        // as every other read above (a bearer read, no loopback concern).
        .route("/status", get(routes::status_route))
        .route("/diff", get(routes::diff_route))
        .route("/search/files", get(routes::search_files))
        .route("/search/symbols", get(routes::search_symbols))
        .route("/search/text", get(routes::search_text_route))
        .route("/search/semantic", get(routes::search_semantic))
        .route("/search", get(routes::search_unified))
        .route("/blame", get(routes::blame))
        .route("/blame/timeline", get(routes::blame_timeline))
        .route("/join/commit", get(routes::join_commit))
        .route("/backfill", post(routes::backfill_route))
        .route(
            "/provenance-report",
            get(provenance::report::provenance_report_route),
        )
        .route("/why", get(provenance::why::why))
        .route("/story", get(provenance::story::story))
        // W4.6 — code annotations (see the module doc above).
        // Phase D-server — the repo-wide open-annotations listing (see the
        // module doc above); a literal route, so it's unambiguous next to
        // `/annotations/{id}` below regardless of registration order.
        .route("/annotations/open", get(routes::list_open_annotations))
        .route(
            "/annotations",
            get(routes::list_annotations).post(routes::create_annotation),
        )
        .route(
            "/annotations/{id}",
            patch(routes::patch_annotation).delete(routes::delete_annotation),
        )
        // V4.C2 — suggestion storage + batch (bearer, annotation-mutation
        // family). Literal `/batch` so it never collides with `/{id}`.
        .route("/annotations/batch", post(routes::batch_annotations))
        .route(
            "/annotations/{id}/suggestion",
            put(routes::put_annotation_suggestion).delete(routes::delete_annotation_suggestion),
        )
        // W5.1 + W5.2 — the agent context verbs (see the module doc above).
        .route("/map", get(agentview::map::map_route))
        .route("/pack", get(agentview::pack::pack_route))
        .route("/defs", get(agentview::xref::defs_route))
        .route("/xrefs", get(agentview::xref::refs_route))
        .route("/similar", get(agentview::similar::similar_route))
        .route("/impact", get(agentview::impact::impact_route))
        // V3.1-H2 — compositional impact analysis (distinct from co-change
        // `/impact` above). Exposed as `/impact/analysis` (not `/impact2`).
        .route(
            "/impact/analysis",
            get(crate::impact_analysis::impact_analysis_route),
        )
        // V3.1-H2 — Code Vision lens aggregation (per-declaration counts).
        .route("/lenses", get(crate::lenses::lenses_route))
        // V3.2-B1 — behavioral attention signals (counters, never git walk
        // at query time for hotspots/coupling/ownership; age uses blame).
        .route(
            "/behavioral/hotspots",
            get(crate::behavioral::hotspots_route),
        )
        .route(
            "/behavioral/coupling",
            get(crate::behavioral::coupling_route),
        )
        .route(
            "/behavioral/ownership",
            get(crate::behavioral::ownership_route),
        )
        .route("/behavioral/age", get(crate::behavioral::age_route))
        // V3.2-B2 fusion signals.
        .route("/behavioral/tainted", get(crate::behavioral::tainted_route))
        .route(
            "/behavioral/session-coupling",
            get(crate::behavioral::session_coupling_route),
        )
        // V3.4-C1 — per-week activity time-series (derived at request time;
        // reuses ingest's log walk/parse; ordinary auth_bearer read).
        .route(
            "/behavioral/timeseries",
            get(crate::behavioral::timeseries_route),
        )
        // V3.3-Q1 — named deterministic recipes (attention signals; enum, no
        // free query language). Ordinary auth_bearer — not loopback-only.
        .route("/recipes", get(crate::recipes::recipes_catalog_route))
        .route("/recipes/{name}", get(crate::recipes::recipe_run_route))
        // V74-L3a (D11 + D21, Track L) — `kbc-recipe/1`, the typed
        // runner. Its own SINGULAR prefix, because `recipes/1` above
        // already owns `/api/recipes/{name}` (the depth-2 param slot):
        // a new depth-2 literal would shadow any recipe named after it,
        // and a depth-3 family under `{name}` would read as a
        // sub-resource of one recipe. Same decision, same reason, as the
        // `canvas` → `boards` split further down. All four READS are
        // ordinary `auth_bearer` — a run mutates nothing, which is also
        // what makes a run URL shareable; the four WRITERS
        // (`materialise`/`trust`/`new`/`delete`) are POST/DELETE on the
        // loopback-only sub-router below. Literal `/recipe/runs/{id}`
        // sits beside `/recipe/{slug}`; axum resolves literals over
        // params regardless of registration order, and
        // `recipe::routes::RESERVED_SLUGS` refuses a stored recipe named
        // `runs` or `new` so nothing is silently shadowed.
        // `crate::recipe::routes::V74_L3A_ROUTES` declares the four
        // reads and a unit test walks that declaration against THIS file.
        .route("/recipe", get(crate::recipe::routes::catalog_route))
        .route(
            "/recipe/runs/{id}",
            get(crate::recipe::routes::replay_route),
        )
        .route("/recipe/{slug}", get(crate::recipe::routes::show_route))
        .route("/recipe/{slug}/run", get(crate::recipe::routes::run_route))
        .route(
            "/recipe/{slug}/lint",
            get(crate::recipe::routes::lint_route),
        )
        // B2 — token-level position resolve (see the module doc above).
        .route("/resolve", get(resolve::resolve_route))
        .route("/usages", get(crate::usages::usages_route))
        // V71-E1 (D4) — `usages/2`: the same ladder, richer wire (closed
        // kind vocabulary, SCIP-style role bitset, per-row precision,
        // enclosing symbol, blob_sha, in-band true totals). ADDITIVE:
        // `/usages` above is frozen and unchanged.
        .route("/usages/2", get(crate::usages2::usages2_route))
        // PRR-N5 — the direct rails_edges read, the hover composition
        // route, and symbol-level deep-link resolution (see the module doc
        // above). Appended here, beside `/resolve`/`/usages`, rather than
        // interleaved with any in-flight route additions elsewhere in this
        // file.
        .route(
            "/framework/edges",
            get(crate::framework_edges::framework_edges_route),
        )
        .route("/hover", get(crate::hover::hover_route))
        // PRR-L2 — lip/1 diagnostics (design-addendum-2.md §D). Appended
        // here, delimited, beside `/hover` — never interleaved with any
        // concurrent route addition elsewhere in this file.
        .route("/diagnostics", get(crate::lip::diagnostics_route))
        // S2-C (design-s2.md § S2-C) — LSP code actions as suggestions.
        // Appended here, delimited, beside `/diagnostics` — never
        // interleaved with any concurrent route addition elsewhere in this
        // file (other Stage-2 units also touch router.rs).
        .route(
            "/code-actions",
            post(crate::code_actions::code_actions_route),
        )
        .route(
            "/resolve-symbol",
            get(crate::symbol_addr::resolve_symbol_route),
        )
        // V3.1-H1 — call/type hierarchy (single-level; see hierarchy.rs).
        .route("/hierarchy/callees", get(crate::hierarchy::callees_route))
        .route("/hierarchy/callers", get(crate::hierarchy::callers_route))
        .route("/hierarchy/types", get(crate::hierarchy::types_route))
        // Phase C-server — the time-first-class routes (see the module
        // doc above).
        .route("/commit", get(routes::commit_route))
        .route("/compare", get(routes::compare_route))
        .route("/branches", get(routes::branches_route))
        // V75-M3 (D15) — `branch-facts/1` and its siblings. Ordinary
        // `auth_bearer` reads, the same gate as `/branches` above:
        // `/facts` shells out only to read, and `/conflicts` writes
        // exclusively into a PER-REQUEST scratch object directory
        // (SEC-15), so neither touches the browsed repo. `/favourites` is
        // an operator PREFERENCE — the `bookmarks` (V0011) / doc-lens-pin
        // (V0020) precedent, not the checkout/review-ref one — so it is a
        // bearer mutation here rather than loopback-only. The ONE route in
        // this family that creates a review lives on the loopback-only
        // sub-router below, beside `POST /reviews` itself.
        .route("/branches/facts", get(crate::branches::facts_route))
        .route("/branches/conflicts", get(crate::branches::conflicts_route))
        .route(
            "/branches/favourites",
            get(crate::branches::list_favourites).post(crate::branches::set_favourite),
        )
        .route("/file-history", get(routes::file_history_route))
        // V3.3-S2 — stack awareness (dependent-branch detection +
        // per-layer incremental diff). Ordinary auth_bearer reads, same
        // gate as `/branches`/`/compare`; pure derivation over existing
        // refs (never writes outside refs/kbc/*).
        .route("/stacks", get(routes::stacks_route))
        .route("/stacks/layer-diff", get(routes::stacks_layer_diff_route))
        // Phase G-server — the review-workflow endpoints (see the module
        // doc above): merge readiness, range-diff, and repo op-state are
        // all ordinary browsing-class reads, same gate as `/commit`/
        // `/compare` just above. The GitHub PR list/comments lanes sit
        // here too — an honest `unavailable_reason` degrade on any
        // GitHub-side failure, never a stronger gate than the rest of the
        // browsing surface (the ONE ref-write, `POST /prs/fetch`, is on
        // the loopback-only sub-router below instead).
        .route("/merge-check", get(routes::merge_check_route))
        .route("/range-diff", get(routes::range_diff_route))
        .route("/repo-state", get(routes::repo_state_route))
        .route("/prs", get(routes::prs_route))
        .route("/prs/{number}/comments", get(routes::pr_comments_route))
        // PRR-R2 (design doc §2 rows 2-3) — one PR's full detail (works for
        // open/closed/merged, unlike `/prs`'s state=open-only list) and its
        // check-runs. Same ordinary auth_bearer gate as `/prs`/`/prs/{n}/
        // comments` — a read-only GitHub overlay, degrading to
        // `unavailable_reason` rather than erroring. `/prs/{number}/reviews`
        // (PRR addendum-2 §A) joins them: per-reviewer states + requested
        // reviewers + a locally-computed review_decision approximation —
        // raw read only, the `/github-threads` composition route that
        // anchors these into the diff is a LATER unit.
        .route("/prs/{number}", get(routes::pr_route))
        .route("/prs/{number}/checks", get(routes::pr_checks_route))
        .route("/prs/{number}/reviews", get(routes::pr_reviews_route))
        // Phase E3 — reading sets (see the module doc above).
        .route(
            "/sets",
            get(reading_sets::list_sets).post(reading_sets::create_set),
        )
        .route(
            "/sets/{id}",
            get(reading_sets::get_set)
                .patch(reading_sets::patch_set)
                .delete(reading_sets::delete_set),
        )
        .route("/sets/{id}/spans", post(reading_sets::append_span))
        // Phase N — bookmarks (durable places; same gate as /sets).
        .route(
            "/bookmarks",
            get(bookmarks::list_bookmarks).post(bookmarks::create_bookmark),
        )
        .route(
            "/bookmarks/{id}",
            patch(bookmarks::patch_bookmark).delete(bookmarks::delete_bookmark),
        )
        // Phase N — TODO index + named scopes (read-only browsing surface).
        .route("/todos", get(routes::list_todos))
        .route("/scopes", get(routes::list_scopes))
        // V72-J1 (D8) — `comments/1`: the comment index. Ordinary
        // `auth_bearer` READS of already-indexed metadata plus, for the
        // `doc` lane, a bounded `git blame` behind the daemon-wide
        // `git_fanout` semaphore — strictly less than `GET /api/blame`
        // already serves, and nothing here mutates. `/todos` above is now
        // a FILTERED VIEW over the same table (there is exactly one
        // scanner over these lines); `crate::comments::V72_J1_ROUTES`
        // declares this family and a unit test walks that declaration
        // against THIS file.
        .route("/comments", get(crate::comments::routes::list_comments))
        .route(
            "/comments/file",
            get(crate::comments::routes::comments_file),
        )
        .route(
            "/comments/summary",
            get(crate::comments::routes::comments_summary),
        )
        .route(
            "/comments/keywords",
            get(crate::comments::routes::comments_keywords),
        )
        // V71-G0 — `entities/1` (`GET /api/entity?repo=&ent=`) and
        // kbc-seq/1 (`GET /api/seq?repo=`). Both are ordinary
        // `auth_bearer` READS of already-indexed metadata — no working
        // tree is touched, no transcript text is served, nothing mutates —
        // so they sit on this router beside `/todos`/`/scopes` rather than
        // in the loopback-only family. `crate::entities::V71_G0_ROUTES`
        // declares this pair; a unit test walks that declaration against
        // THIS file, so a route added there without a registration here
        // fails the build rather than shipping dead.
        // V71-F1 — kbc-tree/1 (`GET /api/tree/2`): the PROJECTED,
        // decorated tree, computed once server-side so `kb-code tree` and
        // the SPA's left dock render the same rows (the evidence report's
        // "two renderers of one projection will diverge" risk). `/tree`
        // above is the per-directory ODB listing and is FROZEN — the same
        // one-ladder-two-wires treatment `/usages/2` got. An ordinary
        // `auth_bearer` read: no working tree is mutated, no transcript
        // text is served, and every path it names is already reachable
        // through `/tree` and `/file`.
        .route("/tree/2", get(crate::tree::tree_v2_route))
        .route("/entity", get(crate::entities::entity_route))
        // V72-G1.1 — `entity/1` (`GET /api/entity/dossier`): the D6
        // dossier over the SAME index. A sibling path rather than a
        // widened `/entity` — the `/usages/2` and `/tree/2` treatment —
        // because `entities/1` answers an ADDRESSING question and
        // legitimately returns many entities, while a dossier is
        // everything about exactly one. Same ordinary `auth_bearer` read:
        // it reads strictly less than `GET /api/file` already does.
        // `crate::entities::dossier::V72_G1_ROUTES` declares it; a unit
        // test walks that declaration against THIS file.
        .route(
            "/entity/dossier",
            get(crate::entities::dossier::dossier_route),
        )
        .route("/seq", get(crate::seq::seq_route))
        // V71-E2 (D5) — `kbc-actions/1` (`GET /api/actions?repo=&path=&
        // line=&col=`): the typed dispatch table over the thing under the
        // pointer, computed per request, nothing persisted. An ordinary
        // `auth_bearer` READ — it reads strictly less than `GET /api/file`
        // already does — but it is the ONE route in this family whose
        // ANSWER depends on the caller's own loopback verdict, since the
        // mutating rows are ABSENT (not disabled) for a caller the server
        // did not clear. `crate::actions::V71_E2_ROUTES` declares it; a
        // unit test walks that declaration against THIS file.
        .route("/actions", get(crate::actions::actions_route))
        // V3.R1 — local review sessions (reads; mutations on loopback-only).
        .route("/reviews", get(reviews::list_reviews))
        // PRR-R4 — the cross-repo attention inbox (design doc §2 row 13,
        // `crate::review_inbox`). Literal `/reviews/inbox` ahead of
        // `/reviews/{id}` — documentation, not a functional requirement
        // (axum resolves literals over params regardless of registration
        // order, per the module doc's PRR-R3 note).
        .route("/reviews/inbox", get(review_inbox::list_inbox_route))
        // S2-A — the federated three-lane attention queue (see the module
        // doc above); a literal `/inbox`, unambiguous with `/reviews/
        // inbox` above (different path entirely, not a param collision).
        .route("/inbox", get(unified_inbox::unified_inbox_route))
        // PRR-R9 — the disposition-analytics calibration instrument
        // (design-addendum-2 §C, `crate::review_analytics`). Same
        // literal-before-param placement as `/reviews/inbox` just above.
        .route("/reviews/analytics", get(review_analytics::analytics_route))
        // V76-R1a — the start-pr job read (`crate::review_jobs`; `POST
        // /api/reviews/pr?async=1`'s polling target). An ordinary bearer
        // READ on this sub-router: the envelope a `done` job returns is
        // exactly what the loopback-only POST would have returned to the
        // same operator's CLI. `crate::review_jobs::V76_R1A_ROUTES`
        // declares it; a unit test walks that declaration against THIS
        // file. Literal `/reviews/jobs` at the same depth as
        // `/reviews/{id}` — axum resolves literals over params, same as
        // `/reviews/inbox` above.
        .route(
            "/reviews/jobs/{id}",
            get(crate::review_jobs::review_job_route),
        )
        .route("/reviews/{id}", get(reviews::get_review))
        .route("/reviews/{id}/files", get(reviews::review_files))
        .route("/reviews/{id}/interdiff", get(reviews::review_interdiff))
        .route(
            "/reviews/{id}/annotations",
            get(reviews::review_annotations),
        )
        // V4.C1 — review-scoped comments with lazy carry-forward. Literal
        // `/comments` so it never collides with `/annotations`.
        .route(
            "/reviews/{id}/comments",
            get(crate::review_comments::review_comments),
        )
        .route("/reviews/{id}/risk", get(reviews::review_risk_route))
        // V3.3-S1 R7 — review map + deterministic reading order (reads).
        .route("/reviews/{id}/map", get(review_map::review_map_route))
        .route(
            "/reviews/{id}/reading-order",
            get(review_map::review_reading_order_route),
        )
        // PRR-F (design-ui.md §12.2, "Reviewer X-ray") — per-changed-file
        // blast-radius chips (`crate::review_impact`'s own doc has the
        // "why a new small aggregate, not a loop over /api/impact/analysis"
        // rationale). Same ordinary auth_bearer review-read gate as `/map`.
        .route(
            "/reviews/{id}/impact",
            get(review_impact::review_impact_route),
        )
        // CT-E7 — one deterministic JSON dump of the review's full local
        // record, for the agent layer to journal into kb (see the module
        // doc above + `crate::review_distill`'s own doc).
        .route(
            "/reviews/{id}/distill",
            get(review_distill::review_distill_route),
        )
        // PRR-R2 (design doc §2 rows 4/7) — the agent-authored review
        // report (GET half; PUT is loopback, on transcripts_api below) and
        // the live, unpersisted artifact-hint verification (the ONE
        // kb-code->kb call this unit adds, via `join::kb_client::KbClient::
        // doc_meta`). Same ordinary auth_bearer read gate as `/distill`/
        // `/comments` — neither exposes anything more sensitive than the
        // rest of the review-read surface already does.
        // V73-K1 (`kbc-review/1`, design D9/D9-a) — the review DOCUMENT's
        // three reads. `/doc/lint` and `/doc/render` are literal segments
        // under the `{id}` param, so they never collide with `/doc` itself
        // (axum resolves the longer literal path). Ordinary `auth_bearer`
        // reads: a document is prose about a diff, not transcript text, and
        // the AUTHORING half (`compose`, `POST /doc/render`) stays
        // loopback-only on `transcripts_api` below — D22 unchanged.
        .route(
            "/reviews/{id}/doc",
            get(crate::review_doc::routes::get_review_doc),
        )
        .route(
            "/reviews/{id}/doc/lint",
            get(crate::review_doc::routes::lint_review_doc),
        )
        .route(
            "/reviews/{id}/doc/render",
            get(crate::review_doc::routes::render_review_doc),
        )
        .route("/reviews/{id}/report", get(reviews::get_review_report))
        .route("/reviews/{id}/artifact", get(reviews::get_review_artifact))
        // PRR-R3 — the findings list read (see the module doc above).
        // Literal `/findings`, sits ahead of the loopback sub-router's
        // `/findings/import` and `/findings/{slug}/disposition` below —
        // axum dispatches by method+exact-path match per sub-router, so a
        // GET here and a POST/PUT/DELETE on the same literal prefix on the
        // OTHER sub-router coexist without collision (same `Router::merge`
        // per-route-layering property `/checkout`-vs-`/repos` etc. already
        // rely on throughout this file).
        .route(
            "/reviews/{id}/findings",
            get(crate::review_findings::list_findings_route),
        )
        // PRR-F (design-ui.md §12.4, "recurring-finding memory") — which of
        // this review's own findings recur across other reviews, via the
        // SAME shared `Store::recurrence_pairs` query `/reviews/analytics`
        // uses (design-addendum-2 §C's own note). Deeper literal segment
        // than `/findings/{slug}/disposition` on the loopback sub-router
        // below (three vs. two path segments after `/findings`) — distinct
        // route templates, no collision regardless of registration order.
        .route(
            "/reviews/{id}/findings/recurrence",
            get(crate::review_findings::findings_recurrence_route),
        )
        // PRR-R5 (design doc §2 row 14 / §3.2) — the GitHub-shaped export.
        // Pure computation, zero GitHub calls, same bearer gate as the rest
        // of the review-read surface (see the module doc above).
        .route(
            "/reviews/{id}/export/github",
            get(crate::review_github_export::export_github_route),
        )
        // PRR-R4 (Phase 4) — the staleness probe (`reviews::pr_status_
        // route`, design doc §2 row 12) and the pure-composition timeline
        // (`crate::review_timeline`, arbitration #7). Same ordinary
        // auth_bearer review-read gate as `/report`/`/findings` above —
        // neither exposes anything more sensitive than the rest of the
        // review-read surface already does.
        .route("/reviews/{id}/pr-status", get(reviews::pr_status_route))
        .route(
            "/reviews/{id}/timeline",
            get(review_timeline::review_timeline_route),
        )
        // PRR-R7 (design-addendum-2.md §A) — the GitHub PR's own review
        // conversation, position-mapped onto the review's latest patchset
        // and merged into the Room (see `crate::review_github_threads`'s
        // own doc). Same ordinary auth_bearer review-read gate as
        // `/pr-status`/`/timeline` above — a live GitHub read, nothing
        // persisted.
        .route(
            "/reviews/{id}/github-threads",
            get(review_github_threads::github_threads_route),
        )
        // V73-K3 — review-scoped PSEUDO-FILES (`kbc-pseudo/1`): the PR
        // body, the kbc-review/1 document, the findings sidecar and the
        // commit list, each with a real git blob hash so a `code:` ref can
        // pin to one. Rendered per request from rows that already exist;
        // nothing is stored (`crate::review_pseudo`'s own doc).
        .route("/reviews/{id}/pseudo", get(review_pseudo::list_pseudo))
        .route(
            "/reviews/{id}/pseudo/{name}",
            get(review_pseudo::get_pseudo),
        )
        // V73-K3 — `kbc-claim/1`, the agent-prose claim register. The READS
        // are ordinary bearer reads; the WRITE is loopback-only, below.
        // Every row rides a per-request Ladder and is SURFACED, NEVER
        // SCORED (`crate::claims`'s own doc, migration V0035's header).
        .route("/claims", get(claims::list_claims))
        .route("/claims/{id}", get(claims::get_claim))
        // V3.4-C1 — canvas sets (reads; mutations on loopback-only).
        .route("/canvas", get(canvas::list_canvas))
        .route("/canvas/{id}", get(canvas::get_canvas))
        // V74-L1 (D10, Track L) — `kbc-canvas/1` BOARDS. A separate family
        // from the v3.4-C1 canvas sets directly above, on its own prefix:
        // `GET /api/canvas/{id}` (an i64 row id) and `GET
        // /api/canvas/{slug}` are the same axum route pattern, so the two
        // could not coexist, and `canvas_sets` is FROZEN rather than
        // redefined (the `/api/usages` -> `/api/usages/2` treatment).
        // Literal `/boards/sweep` sits ahead of the `{slug}` param route —
        // axum resolves literals over params regardless of registration
        // order; `boards::routes::RESERVED_SLUGS` refuses a board named
        // `sweep` or `apply` at apply time so nothing is silently shadowed.
        // `crate::boards::V74_L1_ROUTES` declares the four READS and a unit
        // test walks that declaration against THIS file.
        .route("/boards", get(crate::boards::routes::list_boards))
        .route("/boards/sweep", get(crate::boards::routes::sweep_boards))
        .route("/boards/{slug}", get(crate::boards::routes::get_board))
        .route(
            "/boards/{slug}/export",
            get(crate::boards::routes::export_board),
        )
        // V74-L3b (D12 + D10, Track L) — `kbc-tour/1`'s four READS. A tour
        // is a `canvas_boards` row with `kind = 'tour'` (migration V0037),
        // so these are the SAME storage, the SAME lint and the SAME Ladder
        // resolver the board routes above use — see `crate::tours`' module
        // doc. `crate::tours::V74_L3B_TOUR_ROUTES` declares them and a
        // unit test walks that declaration against THIS file.
        .route("/tours", get(crate::tours::routes::list_tours))
        .route("/tours/{slug}", get(crate::tours::routes::get_tour))
        .route("/tours/{slug}/pack", get(crate::tours::routes::pack_tour))
        .route(
            "/tours/{slug}/export",
            get(crate::tours::routes::export_tour),
        )
        // V74-L3b — `kbc-trail/1`'s TWO bearer-readable surfaces. The
        // STATE read feeds the SPA's mandatory opt-in indicator (D17), and
        // the AGGREGATE is the one agent-facing read: counts per
        // file/symbol, never per-line spans, never an ordering below the
        // day. The two HUMAN reads (`GET /api/trails`, `GET
        // /api/trails/{id}`) return the operator's own movement record and
        // are LOOPBACK-ONLY, registered on the sub-router below — a
        // stricter gate than any other read in this crate, for invariant
        // 23(b)'s reason applied to attention data.
        //
        // Literal `/trails/state` and `/trails/aggregate` sit ahead of the
        // `{id}` param route; axum resolves literals over params
        // regardless of registration order, and a trail id is `trl_` + 12
        // hex, so neither can ever be shadowed by a real id.
        .route("/trails/state", get(crate::trails::routes::get_state))
        .route(
            "/trails/aggregate",
            get(crate::trails::routes::aggregate_trails),
        )
        .route("/events", get(routes::events))
        // 2026-08-16 drift repair — the SSE schema surface (see the module
        // doc above and `crate::schema`'s own doc).
        .route("/events.schema.json", get(crate::schema::enum_get))
        .route(
            "/events/schema/{kind}/{version}",
            get(crate::schema::per_type),
        )
        // DCB W2.B (R2/R20) — the path-addressed lens entry ramp's server
        // side. Deliberately on THIS plain `auth_bearer` router, NOT the
        // `doclens_read` CORS'd sub-router below: same-origin is all
        // `LensEntryByPath.tsx` needs, and a corpus-wide path→id oracle
        // behind an exact-origin ACAO is a surface no consumer asked for.
        // `cors_layer_route_set_is_pinned` (tests/doclens/doclens_route.rs) asserts
        // this route carries no ACAO.
        .route(
            "/doc-lens/resolve-path",
            get(doclens::wire::resolve_path_route),
        )
        // DCB W3.A — the reverse "cited by" lookup. Same placement rationale
        // as `/doc-lens/resolve-path` just above: web-code's own `CitedBy`
        // strip is this daemon's frontend and is always same-origin to it, so
        // a cross-origin "which documents mention this file" oracle is a
        // surface no consumer asked for. `cors_layer_route_set_is_pinned`
        // asserts this route carries no ACAO.
        .route("/doc-refs", get(doclens::sync::doc_refs_route))
        // V72-I1 — `rails/1` (`GET /api/rails/*`): the Rails ENTITY INDEX,
        // a per-request join over `entity_defs` + `rails_edges` + the
        // mirror index, plus the orphan triage report. Ordinary
        // `auth_bearer` browsing reads on THIS router beside
        // `/framework/edges` and `/entity`, for the same reason: every fact
        // they return is derived from what those two already serve, nothing
        // mutates, and no transcript text is involved. Appended here,
        // delimited, rather than interleaved with any concurrent route
        // addition elsewhere in this file.
        // `crate::rails::routes::V72_I1_ROUTES` declares this family; the
        // same unit test that walks V71-G0's declaration walks this one
        // against THIS file, so a route declared there and never registered
        // here fails the build rather than shipping dead.
        .route("/rails/home", get(crate::rails::routes::home_route))
        .route("/rails/models", get(crate::rails::routes::models_route))
        .route(
            "/rails/controllers",
            get(crate::rails::routes::controllers_route),
        )
        .route("/rails/actions", get(crate::rails::routes::actions_route))
        .route("/rails/routes", get(crate::rails::routes::routes_route))
        .route("/rails/jobs", get(crate::rails::routes::jobs_route))
        .route("/rails/mailers", get(crate::rails::routes::mailers_route))
        .route("/rails/views", get(crate::rails::routes::views_route))
        .route("/rails/concerns", get(crate::rails::routes::concerns_route))
        .route("/rails/orphans", get(crate::rails::routes::orphans_route))
        // invariant #4 — the SAME `auth_bearer` middleware kb-server runs,
        // imported rather than copied (see the Cargo.toml dependency note).
        .layer(from_fn_with_state(auth.clone(), auth_bearer));

    // W2.5 — loopback-only, NOT auth_bearer (see the module doc above).
    let transcripts_api = Router::new()
        .route(
            "/search/transcripts",
            get(transcripts::search::search_transcripts),
        )
        .route(
            "/transcripts/status",
            get(transcripts::search::transcripts_status),
        )
        // W3.5 — the session diff: raw transcript prompt text in its own
        // payload, so it rides the SAME loopback-only gate, not auth_bearer.
        .route("/session-diff", get(routes::session_diff_route))
        // W4.7 — the first sanctioned working-tree mutation; see the
        // module doc above for why this rides loopback-only rather than
        // auth_bearer. V4.S1's apply route (below) is the second.
        .route("/checkout", post(routes::checkout_route))
        // V72-H4a — `aug-lane/1` claim ingest. Third member of the
        // loopback-only mutation family, and for the design's own reason
        // (§P8 security posture #2): the OPERATOR ran the tool on their
        // box, and only a loopback caller may put its output into this
        // daemon. A bearer caller reads facts through `/api/lanes/facts`
        // and writes nothing. The body limit is raised above axum's 2 MiB
        // default so THIS route's own 413 — the one that states the byte
        // count and the cap — is what a large tool run hits.
        .route(
            "/lanes/{lane}/ingest",
            post(crate::lanes::ingest::ingest_route).layer(axum::extract::DefaultBodyLimit::max(
                crate::lanes::ingest::MAX_BODY_BYTES + crate::lanes::ingest::BODY_LIMIT_SLACK,
            )),
        )
        // V4.S1 — splice a stored suggestion into the working tree. Same
        // loopback-only family as checkout: the exact-match guard is the
        // dirty-tree policy, and the live-mirror watcher sees the write
        // itself (`crate::suggestions` module doc).
        .route(
            "/annotations/{id}/apply",
            post(suggestions::apply_suggestion_route),
        )
        // PRR-R10 — multi-file batch apply. Same loopback-only family as
        // the single apply just above (`crate::suggestions`'s PRR-R10
        // section doc).
        .route(
            "/annotations/apply-batch",
            post(suggestions::apply_suggestions_batch_route),
        )
        // Phase G-server — the ONE new git ref-write outside `checkout`
        // (into the dedicated `refs/kbc/pr/<n>` namespace, never touching
        // the working tree or HEAD) — same loopback-only family.
        .route("/prs/fetch", post(routes::prs_fetch_route))
        // DCB W3.A — the doc_refs sync trigger. D-A moved the doc-lens PIN
        // onto `auth_bearer` so kb's own reader can write it; this route
        // deliberately did NOT follow (D-A says so in as many words):
        // it drives a BULK pull of document prose across the process
        // boundary into this daemon's store, a different blast radius from
        // remembering one checkout. Same loopback-only family as the
        // transcript lane and every other mutation here.
        .route("/doc-lens/sync", post(doclens::sync::sync_route))
        // DCB-W3.C — from-doc materialization: consistency with its one
        // sibling route below (`/sets/from-session`) argues for the same
        // loopback-only family, even though this route carries no raw
        // transcript/doc-prose text of its own (`reading_sets::
        // from_doc_route`'s own doc).
        .route("/sets/from-doc", post(reading_sets::from_doc_route))
        // Phase E3 — from-session materialization reuses `sessiondiff::
        // session_diff`'s raw-transcript-carrying assembly, so it rides
        // this same loopback-only family instead of `auth_bearer` (see the
        // module doc above).
        .route("/sets/from-session", post(reading_sets::from_session_route))
        // S1 — the SCIP ingest route (see the module doc above).
        .route("/scip/ingest", post(scip::scip_ingest_route))
        // V3.2-B1 — behavioral full-window rebuild (bulk op, loopback-only).
        .route(
            "/behavioral/backfill",
            post(crate::behavioral::behavioral_backfill_route),
        )
        // V3.R1 — review mutations (ref writes under refs/kbc/review/*).
        // Literal `/reviews/gc` before `/{id}` so it never collides with
        // the param route (axum prioritizes literals regardless, but list
        // it first for readability — same pattern as `/annotations/open`).
        .route("/reviews/gc", post(reviews::gc_reviews))
        // PRR-R2 (design doc §2 row 1) — bind a NEW review to a GitHub PR
        // (git-fetch into `refs/kbc/pr/<n>` + ps1 capture + best-effort
        // metadata enrichment). A literal `/reviews/pr` segment, so it
        // never collides with the `/reviews/{id}` param route below
        // (same `/reviews/gc`-before-`/{id}` precedent just above) — this
        // is a WRITE (git ref + row), same loopback-only family as
        // `create_review`/`prs/fetch`, not a branch inside `POST
        // /reviews` (preserves that route's existing contract byte-for-
        // byte, per the design doc).
        .route("/reviews/pr", post(reviews::create_review_pr))
        // PRR-R8 (design-addendum-2 §B) — the stale-backlog sweep. Same
        // literal-before-param family as `/reviews/pr`/`/reviews/gc` just
        // above; a WRITE (refreshes stored `pr_meta_json`/`pr_head_sha`
        // snapshots), so loopback-only like the rest of this sub-router,
        // NOT `auth_bearer` (unlike its read-only sibling `pr-status`).
        .route("/reviews/sweep", post(review_sweep::sweep_route))
        // PRR-R3 — the ONE findings mutation that stays loopback-only (an
        // agent-side batch-reconcile verb, not a mobile-mutation candidate
        // — see the module doc above). `POST /reviews/{id}/findings`
        // (manual create) and `PUT`/`DELETE .../disposition` GRADUATED to
        // the gated `review_remote` sub-router below (S2-B); literal
        // `/findings/import` still sits ahead of that nested param route on
        // ITS OWN sub-router — same literal-before-param precedent as
        // `/reviews/pr`/`/reviews/gc` above, now just split across two
        // routers instead of one (axum resolves literals over params
        // regardless of which sub-router or registration order).
        .route(
            "/reviews/{id}/findings/import",
            post(crate::review_findings::import_findings_route),
        )
        .route("/reviews", post(reviews::create_review))
        // V75-M3 (D15) — "Compare with common base": start a review whose
        // base came off `branch-facts/1`'s CLASSED ladder. It composes
        // `reviews::create_review_value`, so it must not be a WEAKER gate
        // than `POST /reviews` directly above — hence loopback-only here
        // rather than on `review_remote`. The ref rides the body because a
        // branch name contains `/` and axum's wildcard must be terminal.
        .route(
            "/branches/review",
            post(crate::branches::start_branch_review),
        )
        .route("/reviews/{id}/snapshot", post(reviews::snapshot_review))
        .route(
            "/reviews/{id}",
            patch(reviews::patch_review).delete(reviews::delete_review),
        )
        .route("/reviews/{id}/viewed", put(reviews::put_viewed))
        .route(
            "/reviews/{id}/viewed/{*path}",
            delete(reviews::delete_viewed),
        )
        // V73-K2a — per-HUNK viewed state (diff v2). Same loopback-only
        // review-mutation family as `/viewed` directly above, deliberately
        // NOT the `review_remote` sub-router: `review_gate`'s own module
        // doc lists `viewed` among the mutations `remote_mutations` never
        // reaches, and a per-hunk twin of that state must not quietly ride
        // a wider gate than the state it refines.
        // V73-K3 — the hunk↔turn join. LOOPBACK-ONLY because it reads raw
        // captured transcript content (`old_string`/`new_string` are file
        // bytes) — D19's `raw-transcript` sensitivity class, the same gate
        // `/search/transcripts` and `/session-diff` already ride. It is a
        // READ on a loopback-only router, which is exactly what those two
        // are as well.
        .route(
            "/reviews/{id}/hunks/{hunk}/turns",
            get(crate::review_turns::hunk_turns_route),
        )
        // V73-K3 — appending a `kbc-claim/1` row. Loopback-only (D22's
        // local-canonical ruling; root invariant #4 unamended) and audited
        // by the crate-wide `audit_mutations` layer with no per-route
        // wiring (invariant 1).
        .route("/claims", post(claims::create_claim))
        .route("/reviews/{id}/hunk-viewed", put(reviews::put_hunk_viewed))
        .route(
            "/reviews/{id}/hunk-viewed/{hunk_id}",
            delete(reviews::delete_hunk_viewed),
        )
        // PRR-R2 (design doc §2 row 5) — wholesale-replace the
        // agent-authored review report. Same loopback-only review-mutation
        // family as `/reviews`/`/snapshot` above; the GET half lives on
        // `auth_bearer` (see the block above the `.layer` call).
        .route("/reviews/{id}/report", put(reviews::put_review_report))
        // V70-R — `review compose` v0 (design doc D9 scoped down): findings
        // + report + an optional review-level verdict in ONE sqlite
        // transaction. Same loopback-only review-mutation family as
        // `/findings/import` and `/report` just above — NOT auth_bearer,
        // no `[review] remote_mutations` gate (D22: root invariant #4 is
        // not amended, no new mutation surface graduates off loopback).
        .route(
            "/reviews/{id}/compose",
            post(crate::review_findings::compose_review_route),
        )
        // V73-K1 — render a review document through the OPERATOR'S OWN
        // template bytes. Writes nothing; it is a POST only because a
        // template is a whole HTML file rather than a query parameter, and
        // it sits on this loopback-only family because the caller is the
        // operator's own CLI on their own box (D22 local-canonical). The
        // bearer-read twin above renders through a REGISTERED name instead,
        // so no route ever takes a caller-supplied file PATH.
        .route(
            "/reviews/{id}/doc/render",
            post(crate::review_doc::routes::render_review_doc_with_template),
        )
        // V3.4-C1 — canvas mutations (opaque layout write; same loopback-only
        // family as review mutations — NOT auth_bearer).
        .route("/canvas", post(canvas::create_canvas))
        .route(
            "/canvas/{id}",
            put(canvas::update_canvas).delete(canvas::delete_canvas),
        )
        // V74-L1 (D10 + D21, Track L) — `kbc-canvas/1` board mutations.
        // Same loopback-only family as the canvas-set mutations directly
        // above and as every review mutation: NOT `auth_bearer`, and NOT
        // the `review_remote` gate. D10 sketches a later graduation onto a
        // named-family `[review] remote_mutations = ["review", "canvas"]`
        // allowlist; that is its own unit, and nothing here weakens kb root
        // invariant #4. `accept` is the ONLY writer of the `accepted`
        // status (D21: an agent-proposed board is PENDING until a human
        // accepts it) — `apply`'s own lint refuses to author that status at
        // all, so the rule holds for a loopback caller too, not just by
        // virtue of this gate.
        // V74-L3a — kbc-recipe/1's four WRITERS. `materialise` stores a
        // run snapshot, `trust` records a trust-on-first-use decision,
        // `new` writes a server-stored recipe and `delete` removes one:
        // each records a decision this daemon honours later, so all four
        // are loopback-only (D22's local-canonical ruling, unchanged).
        .route(
            "/recipe/{slug}/materialise",
            post(crate::recipe::routes::materialise_route),
        )
        .route(
            "/recipe/{slug}/trust",
            post(crate::recipe::routes::trust_route),
        )
        .route("/recipe/new", post(crate::recipe::routes::new_route))
        .route(
            "/recipe/{slug}",
            delete(crate::recipe::routes::delete_route),
        )
        .route("/boards/apply", post(crate::boards::routes::apply_board))
        .route(
            "/boards/{slug}/accept",
            post(crate::boards::routes::accept_board),
        )
        .route(
            "/boards/{slug}/archive",
            post(crate::boards::routes::archive_board),
        )
        .route(
            "/boards/{slug}",
            delete(crate::boards::routes::delete_board),
        )
        // V74-L3b — `kbc-tour/1`'s two mutations, the same loopback-only
        // family as the board mutations directly above (a tour IS a board
        // row). `accepted`/`archived` stay reachable only through the
        // BOARD transition routes, and `apply`'s lint refuses to author
        // either, so D21's pending-until-a-human-accepts rule holds for a
        // loopback caller too.
        .route("/tours/apply", post(crate::tours::routes::apply_tour))
        .route("/tours/{slug}", delete(crate::tours::routes::delete_tour))
        // V74-L3b — `kbc-trail/1`. EVERY trail write is here, and so are
        // the two HUMAN reads: a trail is a record of where a person went,
        // and the design's Security §Privacy line ("read-tracking never
        // leaves the operator's box") makes that a stricter class than an
        // ordinary bearer read. The aggregate and the state read are on
        // the bearer router above.
        //
        // `audit_mutations` (invariant 1) runs inside this gate, so every
        // opt-in transition, every purge and every fork lands in the
        // ledger with its actual outcome for free — which is what makes
        // D17's "purge is audited" true without a line of its own.
        .route(
            "/trails",
            get(crate::trails::routes::list_trails).post(crate::trails::routes::create_authored),
        )
        .route("/trails/steps", post(crate::trails::routes::ingest_steps))
        .route("/trails/purge", post(crate::trails::routes::purge_trails))
        .route("/trails/state", post(crate::trails::routes::set_state))
        .route("/trails/{id}", get(crate::trails::routes::get_trail))
        .route("/trails/{id}/fork", post(crate::trails::routes::fork_trail))
        .layer(from_fn_with_state(
            auth.clone(),
            transcripts::search::loopback_only,
        ));

    // S2-B ("Mobile mutations," `/tmp/design-s2.md`) — the five
    // review-mutation route families graduated off pure loopback-only,
    // gated by `[review] remote_mutations` (default OFF — see
    // `review_gate::review_mutations_gate`'s own module doc for the full
    // admission table, and the top-of-file S2-B paragraph above for why
    // this is a THIRD sub-router rather than a branch inside
    // `transcripts_api`). `auth_bearer` is layered FIRST (chained
    // first — INNER); `review_mutations_gate` is layered LAST (chained
    // last — OUTER, same "chained-last-is-outermost" rule
    // `doclens_read`'s CORS-vs-`auth_bearer` ordering comment below
    // documents), so the gate always runs before `auth_bearer`'s token
    // check: loopback admits unconditionally; non-loopback 404s when the
    // flag is off (byte-identical to `loopback_only`'s refusal,
    // `auth_bearer` never runs); non-loopback falls through to the
    // ordinary bearer 401/200 check when the flag is on.
    let review_remote = Router::new()
        // PRR-R3 (addendum §E) — a single human-authored finding. Was on
        // `transcripts_api`; GRADUATED here by S2-B. Literal `/findings`,
        // same literal-before-param precedent as `/findings/{slug}/
        // disposition` just below (axum resolves literals over params
        // regardless of registration order or which sub-router).
        .route(
            "/reviews/{id}/findings",
            post(crate::review_findings::create_manual_finding_route),
        )
        // PRR-R3 — finding disposition set/clear. Was on `transcripts_api`;
        // GRADUATED here by S2-B.
        .route(
            "/reviews/{id}/findings/{slug}/disposition",
            put(crate::review_findings::set_finding_disposition_route)
                .delete(crate::review_findings::clear_finding_disposition_route),
        )
        // PRR-R5 (design doc §2 rows 15-16) — publish recording, advisory
        // only (Risk #5). Was on `transcripts_api`; GRADUATED here by S2-B,
        // same family as `/disposition` just above.
        .route(
            "/reviews/{id}/findings/{slug}/published",
            post(crate::review_github_export::publish_finding_route),
        )
        .route(
            "/reviews/{id}/verdict/published",
            post(crate::review_github_export::publish_verdict_route),
        )
        // V4.C2 — verdict set/clear. Was on `transcripts_api`; GRADUATED
        // here by S2-B.
        .route(
            "/reviews/{id}/verdict",
            put(reviews::put_verdict).delete(reviews::delete_verdict),
        )
        .layer(from_fn_with_state(auth.clone(), auth_bearer))
        .layer(from_fn_with_state(
            state.clone(),
            review_gate::review_mutations_gate,
        ));

    // DCB W1.C — the doc-lens lane rides `auth_bearer` like the rest of the
    // browsing surface, but is the ONLY part of `/api` that carries CORS
    // headers. Two SEPARATE sub-routers, each with its own `CorsLayer`,
    // merged into the nest: kb-server layers its own CorsLayer over one
    // UNIFORM `/api` nest, but THIS crate's `/api` is not uniform — copying
    // that placement would put `access-control-allow-origin` on the
    // loopback-only transcripts lane (`/api/search/transcripts`,
    // `/api/transcripts/status`, `/api/session-diff` are all GETs) and on
    // `/api/file`, i.e. it would hand any page served on any localhost port
    // the full contents of every configured repo. `Router::merge` preserves
    // per-route layering — the same property the transcripts sub-router
    // above already relies on, and `cors_layer_route_set_is_pinned`
    // (tests/doclens/doclens_route.rs) asserts the exact ACAO-carrying route set so
    // a later `/doc-lens/*` route cannot join it by accident.
    //
    // The CorsLayer is OUTERMOST on each (chained last), mirroring
    // kb-server's "SW3: outermost" placement: an OPTIONS preflight carries
    // no credentials, so an `auth_bearer` layer outside CORS would 401 it
    // before the browser ever saw the allow-headers.
    let doclens_origins = state.doclens.normalized_origins();
    let doclens_read = Router::new()
        .route("/doc-lens", get(doclens::wire::doc_lens_route))
        .route("/doc-lens/repos", get(doclens::wire::doc_lens_repos_route))
        // W2.A (E8) — the pin LIST joins the CORS'd read set deliberately: it
        // returns the same information class the scorecard already does, and
        // W1.D/W2.B want it cross-origin. `cors_layer_route_set_is_pinned`
        // flips this route's probe row to asserted-PRESENT in the same commit,
        // so the widening is a reviewed decision rather than a side effect of
        // which `Router::new()` the `.route()` call landed in.
        .route("/doc-lens/pins", get(doclens::pins::list_pins_route))
        // SL7e (v0.42, slate D29) — the PATH-addressed lens joins the CORS'd
        // read set deliberately, for the same reason `/doc-lens` is on it: kb's
        // SPA is cross-origin to this daemon and this is a strictly SMALLER
        // read than the doc lens it already gets there (one path the caller
        // itself named, no document, no ref feed). Its same-origin-only
        // sibling `/doc-lens/resolve-path` stays off the set — that one
        // enumerates kb DOCUMENTS from a path. `cors_layer_route_set_is_pinned`
        // flips this route's probe row to asserted-PRESENT in the same commit,
        // so the widening is reviewed, not incidental.
        .route("/doc-lens/path", get(doclens::wire::path_lens_route))
        .layer(from_fn_with_state(auth.clone(), auth_bearer))
        .layer(doclens::cors::read_cors(doclens_origins.clone()));
    // D-A moved the pin OFF the loopback-only sub-router onto `auth_bearer`
    // so kb's own reader can write it (Decision 1: the pin IS the remembered
    // read-time choice). It is an operator PREFERENCE, not a tree/ref
    // mutation — the bookmarks (V0011) precedent, not the checkout/review-ref
    // one. `POST /api/doc-lens/sync` (W3.A) does NOT follow it: that route
    // drives a bulk pull of doc prose into this daemon's store and STAYS
    // loopback-only, on `transcripts_api`.
    let doclens_pin = Router::new()
        .route(
            "/doc-lens/pin",
            put(doclens::pins::put_pin_route).delete(doclens::pins::delete_pin_route),
        )
        .layer(from_fn_with_state(auth, auth_bearer))
        .layer(doclens::cors::pin_cors(doclens_origins));

    // V70-A2 — the three crate-wide guards, on the MERGED nest (see the
    // module doc). Chained in this order, so `origin_host_guard` ends up
    // OUTERMOST and `audit_mutations` innermost of the three.
    let api_all = api
        .merge(transcripts_api)
        .merge(review_remote)
        .merge(doclens_read)
        .merge(doclens_pin)
        .layer(from_fn_with_state(
            state.clone(),
            security::audit::audit_mutations,
        ))
        .layer(from_fn_with_state(
            state.clone(),
            security::origin::mutation_header_guard,
        ))
        .layer(from_fn_with_state(
            state.clone(),
            security::origin::origin_host_guard,
        ));

    Router::new()
        .nest("/api", api_all)
        // Top-level, NOT nested under /api — bypasses auth_bearer, mirroring
        // kb-server's `/healthz` (`crates/kb-server/src/routes/health.rs`).
        .route("/healthz", get(routes::healthz))
        // W4.1 — anything that missed both of the above (i.e. every
        // non-/api, non-/healthz path) falls through to the SPA.
        .fallback(spa::serve)
        .with_state(state)
}
