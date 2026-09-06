# HTTP API

The narrative canon for the daemon's HTTP surface: what each endpoint is
for, its parameters, its response shape and the behaviour behind it.
Everything lives under `/api` unless noted; auth is a bearer token with a
loopback bypass ([self-host.md](self-host.md)), and errors are RFC 7807
`application/problem+json`.

Two generated companions keep this page honest:
[`api-routes.md`](api-routes.md) is the complete method/path/handler table
extracted from `router.rs` (`just api-docs`; CI fails when it is stale) and
never trails a new endpoint, and
[`web/src/api/generated/`](../web/src/api/generated/) carries the wire types
for SPA-facing responses. Read this page for meaning, the table for
completeness.

Related: [CLI reference](cli.md) · [kb-code](kb-code.md) ·
[project overview](../README.md).

```
GET  /healthz                               unauthenticated liveness probe → {status,daemon,uptime_secs,kbs}
                                            (top-level route, NOT behind bearer-auth or rate-limiting)
GET  /api/identity                          daemon name + version + kbs, plus
                                            the caller's resolved `user`,
                                            `identity_source` (header|token|
                                            legacy|loopback) and the configured
                                            `operator` (v0.34). Also the
                                            kb-sibling/1 HELLO: `build_sha`,
                                            `sibling_protocol` ("kb-sibling/1"),
                                            `sibling_major` (1) and
                                            `schema_epoch` (this binary's
                                            highest embedded migration — the
                                            number the boot guard compares each
                                            volume against). A sibling daemon
                                            handshakes here before its first
                                            call and fails closed on a mismatch;
                                            /healthz stays pure liveness.
GET  /api/users                             v0.34 — configured ∪ observed users
                                            {name,display,configured,observed}
GET  /api/kbs                               list configured kbs
GET  /api/kb/{kb}/sources                   sources in a kb
GET  /api/kb/{kb}/docs[?offset=N&limit=N&projection=slim|default|atlas
   &folder=PATH&folder_exact=1&tags=CSV&caps=CSV&since=7d|30d|all&index=1
   &category=KB-CATEGORY&from=UNIX&to=UNIX
   &read=never-opened|unread|in_progress|read (csv any-of)
   &sort=recent|indexed|created|title|words|residue&dir=asc|desc&group=folder]
                                            list/filter artifacts. Bare array
                                            by default; pass offset= (or
                                            envelope=1) for the paginated
                                            {docs,total,offset,limit,has_more}
                                            envelope. projection=atlas (or the
                                            legacy include=atlas) adds
                                            atlas_x/y/cluster fields. category=
                                            is an exact kb-category include;
                                            from=/to= bound mtime_unix (the
                                            reader's "modified around" pivot).
                                            W1.A: default/full rows carry
                                            read_state/read_pct/last_opened_unix
                                            (page-scoped rollup join, never
                                            memoized — #15); read= filters on it
                                            (never-opened = absent from the
                                            rollup; the antilibrary facet).
                                            CT-F4: default rows also carry
                                            session_residue — how many OTHER
                                            memories were born in this doc's own
                                            kb-session, summed across the
                                            memory-scoped corpora (page-scoped,
                                            absent when zero, SURFACED never
                                            scored). sort=residue orders by it
                                            ("docs whose conclusions someone
                                            kept"); docs with no kb-session sort
                                            last under either dir.
GET  /api/kb/{kb}/docs/{id}                 single artifact metadata. A MOVED
                                            old id 301s to the new id via the
                                            moves chain (F3).
GET  /api/kb/{kb}/docs/by-path/{*path}      resolve a source-relative path to
                                            the artifact (path-based permalink)
POST /api/kb/{kb}/docs/{id}/move            move/rename ONE artifact. Body:
                                            {to: source-rel path}. Runs the
                                            relocate engine (id changes with the
                                            path; comments/history/lists/edges/
                                            sessions migrate; moves row serves
                                            redirects). Returns {old_id, new_id,
                                            old_source_rel, new_source_rel}.
                                            409 target exists · 404 unknown ·
                                            400 escape/invalid. Parent dirs are
                                            created (no mkdir op).
POST /api/kb/{kb}/folders/rename            batch-relocate every artifact under
                                            {from} to {to} (deterministic order,
                                            stop-on-first-error; completed items
                                            stay consistent). Returns {moved:[…]}.
PATCH /api/kb/{kb}/artifacts/{id}/meta      edit kb-tags / kb-category in the
                                            artifact's source ({tags?:[],
                                            category?:""}); rewrites the HTML
                                            <meta> or .md frontmatter in place
                                            (byte-preserving) then re-indexes.
                                            Returns the effective {tags,
                                            kb_category} after re-parse.
PUT  /api/kb/{kb}/artifacts/{id}/content    replace-in-place: ONE multipart
                                            file. Extension must resolve via
                                            the kb's ext map AND the same
                                            pipeline family as the existing
                                            file (.md cannot replace .html
                                            → 415). Capture byte caps.
                                            Atomic write; watcher reindexes.
                                            Returns {id, source_relative,
                                            bytes}. 401 on token-less
                                            non-loopback.
GET  /api/kb/{kb}/artifacts/{id}/versions[?at=<unix>]
                                            version timeline: {mode, versions:
                                            [{ref, source: working|git|index,
                                            label, author, ts_unix, short}]},
                                            per the kb's `versions` mode.
                                            CT-F6 (RFC 7089 Memento): `at`
                                            (unix SECONDS) additionally
                                            resolves the timeline AT that
                                            instant, adding `memento:
                                            {at_unix, relation:
                                            "nearest-prior", found, exact,
                                            version?, oldest_ts_unix?,
                                            note?}` and a `Memento-Datetime`
                                            header carrying the RESOLVED
                                            version's own datetime. The
                                            answer is always the newest
                                            version at or before `at` —
                                            `exact` says whether it landed
                                            on that version's own second,
                                            and an instant older than every
                                            version is `found:false` + a
                                            `note` naming the oldest, NEVER
                                            a silent fallback to it.
                                            Per-artifact resolution only —
                                            not a corpus timeline, and it
                                            touches no ranking (`recall
                                            --as-of` stays rejected). Absent
                                            `at`, the response is unchanged.
GET  /api/kb/{kb}/artifacts/{id}/diff[?from=&to=&mode=text|raw]
                                            diff between two version refs
                                            (defaults: most recent prior
                                            version → working tree; mode=text
                                            is rendered prose, raw is source
                                            bytes). Returns {from, to, mode,
                                            hunks:[{old_start, new_start,
                                            lines:[{tag, old_lineno,
                                            new_lineno, text}]}]}.
GET  /api/kb/{kb}/lookup?q=<id|path|file>   resolve a 12-hex id, path, or
                                            unique filename; returns {kind:
                                            exact|unique_suffix|ambiguous|
                                            not_found, ...}. Backs `kb find`.
GET  /api/kb/{kb}/artifact/{id}[?download=1] raw artifact HTML bytes (the body
                                            `kb get --format html` fetches).
                                            `?download=1` adds a
                                            Content-Disposition: attachment
                                            header (basename filename) so a
                                            browser saves the raw source
                                            instead of rendering it.
GET  /api/kb/{kb}/download[?folder=<path>]  .zip of the source artifacts in
                                            <folder> (descendant-inclusive,
                                            same filter as ?folder= on /docs);
                                            omit folder for the whole kb.
                                            application/zip + Content-Disposition
                                            attachment; entries keyed by
                                            source-relative path. 404 when the
                                            folder is empty; 413 over the size /
                                            file-count cap.
POST   /api/kb/{kb}/share                   publish a source-relative file/folder
                                            to a static host. Body: {target, host,
                                            gate[], public, links, update,
                                            no_scrub}. Runs the engine server-side
                                            (deploy + gate); returns {name, url,
                                            host, gate, danglers, files, updated}.
                                            Host API tokens come from the daemon's
                                            environment. 400 on a bad host/gate
                                            combo or unconfigured host; 502 on an
                                            upstream Cloudflare/GitHub failure.
POST   /api/kb/{kb}/share/export/page       stage ONE artifact uncompressed in
                                            its native format. Body: {target,
                                            no_scrub}. Returns the bytes (HTML:
                                            scrubbed standalone .html; Markdown:
                                            raw .md source) with Content-Type +
                                            Content-Disposition attachment; the
                                            x-kb-share-danglers header lists
                                            cross-artifact links dead offline.
                                            No host/registry/tokens. 400 on a
                                            folder target; 404 if missing.
POST /api/kb/{kb}/lists/{id}/share/export   stage a READING LIST as a
                                            self-contained offline .zip: ordered
                                            entries are the export set and a
                                            generated index.html TOC (title,
                                            notes, Section-anchor #fragments) is
                                            the entry page. Same scrub/links
                                            knobs + zip shape as /share/export;
                                            tombstoned/unresolvable entries are
                                            skipped and reported via the
                                            x-kb-share-skipped header (entry
                                            ids); 400 when nothing resolves
                                            (never an empty zip).
GET    /api/kb/{kb}/shares                  list recorded shares (newest-first).
DELETE /api/kb/{kb}/shares/{name}           revoke: tear down host objects + drop
                                            the registry row. 404 if unknown.
POST /api/kb/{kb}/sources/{src}/reindex     trigger reindex
POST /api/kb/{kb}/reindex                   reindex every source in the kb
POST /api/kb/{kb}/sources/{src}/{pause,resume}
                                            v0.24 D6: paused is ENFORCED at the
                                            ingest gate (watcher + reconcile +
                                            reindex all stop until resume)
GET    /api/kb/{kb}/exclusions              v0.24 X3: per-file exclusions (path,
                                            note, artifact_id, present_on_disk)
POST   /api/kb/{kb}/exclusions              exclude {path, note?} — KeepUserData
                                            cascade (comments + history survive)
DELETE /api/kb/{kb}/exclusions/{path}       re-include + forced reindex; {path}
                                            is ONE percent-encoded segment (%2F)
GET  /api/kb/{kb}/errors                    list open errors
POST /api/kb/{kb}/errors/{err}/{dismiss,apply-fix}
GET  /api/kb/{kb}/runs[?limit=N]            v0.6 R1: recent indexing runs.
                                            One row per run id with status
                                            (running|complete), ok/err counts,
                                            duration_ms. Cap 256 per kb.
GET  /api/kb/{kb}/queries[?limit=N]         v0.6 R1: recent search queries.
     [&zero_hit=true[&min_count=N]]         One row per `query` envelope
                                            (newest first). Cap 256 per kb.
                                            GC-B3: zero_hit=true switches to
                                            the corpus-gap report — the ring's
                                            zero-hit queries grouped by
                                            normalized text (count + last_seen
                                            per group, sorted count-desc);
                                            limit then caps GROUPS (top-N by
                                            count); min_count drops groups
                                            seen fewer than N times (default 1).
GET  /api/queries/zero-hit[?min_count=N&limit=M]
                                            GC-B3: the same zero-hit report
                                            fanned out across every kb on the
                                            daemon (#28, sibling to /stats) —
                                            one {kb, groups} row per corpus in
                                            BTreeMap order. Backs `kb queries
                                            --zero-hit --scope all`.

# v0.6+ H — per-user activity log (history)
GET  /api/kb/{kb}/history[?limit=N&before=UNIX&kind=open|search|comment|all]
                                            newest-first activity log: artifact
                                            opens (with scroll position),
                                            search queries (5s dedup), and
                                            comments authored. open + comment
                                            rows enriched with the artifact
                                            title via lance.
POST /api/kb/{kb}/history/open              register an artifact-view visit.
                                            body: {artifact_id}; returns
                                            {visit_id, scroll_y}. 30-min gap
                                            rule: re-opening within the window
                                            bumps the same row + returns its
                                            scroll for the runtime to resume.
POST /api/kb/{kb}/history/scroll            UPDATE scroll on an open visit.
                                            body: {visit_id, scroll_y,
                                            scroll_max}; 204 No Content, or
                                            404 if the visit is unknown / not
                                            an open row.
POST /api/kb/{kb}/history/search            record a search query (dedup'd
                                            5s server-side). body: {query};
                                            returns {id}.
# RP — reading-progress (per-section dwell, read vs skim, stop-point)
POST /api/kb/{kb}/history/reading           per-section reading beacon. body:
                                            {visit_id, artifact_id, sections[],
                                            active_ms, last_section} (cumulative
                                            per visit; server max-merges). 204;
                                            404 stale visit; no-op 204 when the
                                            kb has reading_progress=false. No SSE.
                                            (history/open also echoes the prior
                                            reading state for seed-on-open.)
GET  /api/kb/{kb}/artifacts/{id}/reading[?lite=true]
                                            reading summary merged across visits:
                                            completion % (furthest scroll),
                                            words-weighted read %, active time,
                                            stop-point, and per-section
                                            read/skim/unseen + interest ranking.
                                            ?lite omits the per-section breakdown.
GET  /api/search?q=…&mode=hybrid|keyword|semantic[&kb=NAME]
                                            [&scope=one|all][&limit=N][&detail=full]
                                            Q-track filters (all optional, csv where
                                            multi): category, folder, tags, exclude_tags,
                                            status, severity, caps, since=day|week|month|year
                                            (&since_field=created|modified), session, list,
                                            read=unread|in_progress|read,
                                            read_from/read_to=UNIX (W1.C: keep only
                                            artifacts with a history open row in the
                                            window — per-corpus in scope=all). Sort:
                                            sort=relevance(default)|opened|modified|created|
                                            indexed|title|words|progress (&dir=asc|desc).
                                            Empty q + any filter/sort = browse listing.
                                            Hits carry snippet (W1.C: query-time
                                            match window from the body; null for
                                            pure-semantic hits) + bm25_rank/vec_rank
                                            (pre-RRF per-arm positions, additive).
GET  /api/stats                             cross-kb summary
GET  /api/kb/{kb}/stats                     per-kb stats
GET  /api/kb/{kb}/slo                       CT-F5 corpus-health SLOs: four
                                            indicators over EXISTING tables
                                            (code-ref path shape, orphan
                                            kb_session docs, recall-ledger
                                            parse-failure rate, capture
                                            freshness), each with value +
                                            optional [kb.*.slo] target +
                                            ok|warn|unknown. SURFACED, NEVER
                                            ENFORCED — nothing changes
                                            behaviour on a miss. A null value
                                            means not measurable, never 0.
POST /api/kb/{kb}/slo/snapshot              append one reading to the per-kb
                                            append-only slo_snapshots log
                                            (one row per indicator, sharing
                                            taken_at_unix); echoes the report
                                            it stored. Every run lands — no
                                            skip-if-unchanged, a flat line is
                                            itself the signal. The daemon
                                            never snapshots on its own.
GET  /api/kb/{kb}/slo/snapshots             newest-first page over that log
                                            (&limit=N, clamped 1..=1000)
GET  /api/metrics                           request + pipeline timing snapshot.
                                            Coarse per-route latency histograms
                                            (count + p50/p95/p99 + raw buckets)
                                            always present, plus embedder health
                                            (embedder_degraded +
                                            embedder_respawn_count, v0.24 T1,
                                            1 Hz-ticker-refreshed); the detailed
                                            block (per-search-stage, per-kb,
                                            ingest pipeline) is non-null only
                                            when [server] metrics = true.
                                            curl-able companion to the
                                            metrics.tick SSE.
GET  /api/settings                          ui prefs (theme/accent/density)
PATCH /api/settings                         update ui prefs
GET  /api/config                            running kb.toml as JSON + its
                                            source path + share-token env
                                            presence (names only) + the
                                            embedding-model registry
PUT  /api/config                            validate + write the whole config
                                            back to its source file (comments
                                            preserved), then restart the daemon
                                            in-process to apply; rolls back to
                                            last-good if the new config won't
                                            boot. A changed [daemon] name or an
                                            unbindable addr is rejected (400).
GET  /api/log-level                         L2: current FILE-layer log filter
                                            {filter, installed, env}; filter
                                            is null when the daemon process
                                            never initialised file logging
PUT  /api/log-level                         L2: {"filter": "debug"} flips the
                                            ndjson file layer's EnvFilter
                                            LIVE (no restart; stderr/RUST_LOG
                                            untouched). 400 bad directives;
                                            409 when file logging is off
POST /api/kb/{kb}/atlas/recompute           v0.3: real PCA + k-means; returns
                                            202 + run; tail SSE for
                                            atlas.recompute.complete payload
                                            {points, clusters, duration_ms}
POST /api/kb/{kb}/atlas/recluster[?k=N]     Q5: re-runs only k-means on
                                            existing atlas coords (no UMAP).
                                            Same 202 + run shape; emits
                                            atlas.recluster.{start,complete}.
GET  /api/kb/{kb}/atlas/labels              W1.B: deterministic c-TF-IDF cluster
                                            labels (top-5 terms/cluster with the
                                            tf/ft/score decomposition + avg_tokens),
                                            recomputed whole-kb on every atlas
                                            recompute/recluster (V0027 table);
                                            empty clusters list before the first
                                            recompute. CLI: kb atlas labels.
GET  /api/kb/{kb}/atlas/similar/{id}[?limit=N]
                                            W2.3a: true cosine neighbors from the
                                            ORIGINAL embedding space (in-route
                                            cosine over real vectors, self-hit
                                            dropped; honest no-embedding shape;
                                            atlas coords per neighbor back the
                                            SPA's 2-D-vs-high-D honesty badge).
                                            CLI: kb similar <id>.
GET  /api/kb/{kb}/atlas/points               M-a: the FULL-corpus atlas point
                                            set (id/coords/cluster/title) in
                                            ONE memoised scan — fixes the
                                            gallery's paged ?projection=atlas
                                            silently truncating the map past
                                            its 200-doc default limit. MI-W4.5:
                                            each point additionally carries
                                            salience/decay_bucket/pinned/
                                            forgotten/supersedes ONLY when the
                                            kb is memory-scoped ([kb.*]
                                            memory_scope set) — byte-unchanged
                                            for an ordinary corpus. Backs the
                                            SPA atlas's salience/decay color
                                            modes + supersede-chain edges.
                                            CLI: kb atlas points.
GET  /api/kb/{kb}/atlas/history             W3 T-b: the corpus time-lapse
                                            frame list (V0028), newest first
                                            — id/when/points/clusters/layout/
                                            provenance, no points. Frames
                                            start EMPTY (kb retains no past
                                            layout/embedding, so history is
                                            unwatchable until recomputes
                                            accumulate going forward); an
                                            empty frames:[] is 200, never
                                            404. Emits atlas.snapshot.recorded
                                            {kb, id, points} when a recompute/
                                            recluster actually appends a new
                                            frame (a dedup no-op emits
                                            nothing). CLI: kb atlas history.
GET  /api/kb/{kb}/atlas/history/{id}[?align_to=N]
                                            W3 T-b: one frame's points,
                                            Procrustes-aligned SERVER-SIDE
                                            (kb_core::procrustes — closed-form,
                                            no atan2/sin/cos, bit-identical
                                            across libcs) against align_to
                                            (default: newest frame) — the CLI
                                            and SPA time-lapse then agree on
                                            the SAME aligned coordinates byte
                                            for byte. Response carries the
                                            fitted transform + the residual
                                            over the id-joined overlap; the
                                            residual is EXPECTED to stay
                                            nonzero even for identical
                                            geometry (atlas coords are x/y
                                            min-max-normalised per axis
                                            INDEPENDENTLY, an anisotropic
                                            stretch no similarity transform
                                            can fully undo — this is not a
                                            bug). 404 when {id} or align_to
                                            doesn't name a stored frame. CLI:
                                            kb atlas show <id> [--align-to N].
POST /api/kb/{kb}/atlas/history/backfill[?frames=N]
                                            W3 T-d: seed the time-lapse with
                                            RECONSTRUCTED frames — for each of
                                            N evenly-spaced mtime_unix cut
                                            points (N<=12, default 8; 400
                                            outside that range), lay TODAY's
                                            embeddings out over the docs that
                                            existed then and store the result
                                            with provenance='reconstructed'.
                                            NOT history: nothing retains a
                                            past layout or embedding, so this
                                            answers "where would these docs
                                            have sat, had I run the atlas
                                            then, knowing what I know now" —
                                            every surface prints the word.
                                            Time axis is mtime_unix, never
                                            indexed_at_unix (one kb reindex
                                            stamps the whole corpus with
                                            today). Same deterministic layout
                                            kernel as recompute, same insert
                                            path (coord_hash dedup +
                                            prune-to-24); re-running is
                                            idempotent (skip on coord_hash).
                                            202 + the PLAN (cut points + doc
                                            counts) computed synchronously,
                                            then the work is spawned: shares
                                            recompute's single-flight slot and
                                            rate-limit bucket. Emits atlas.
                                            backfill.{start,complete} +
                                            atlas.snapshot.recorded per frame
                                            written. Never touches the live
                                            lance atlas columns or labels.
                                            CLI: kb atlas backfill [--frames N].
POST /api/kb/{kb}/atlas/history/prune?keep=N
                                            W3 T-b: explicit operator
                                            retention — drop all but the
                                            newest N frames (points cascade).
                                            The insert path already
                                            self-prunes to
                                            DEFAULT_ATLAS_FRAMES_KEEP=24 on
                                            every recompute/recluster; this
                                            is for tightening the bound on
                                            demand. keep= is required (400
                                            without it); response is honest
                                            about how many it actually
                                            removed, never N itself. CLI:
                                            kb atlas prune --keep N.
GET  /api/kb/{kb}/atlas/field                W3 F-b: the dual-field atlas's
PUT  /api/kb/{kb}/atlas/field                operator half — a JSON Canvas
                                            sidecar (<src>/atlas/operator.
                                            canvas — ONE per kb, index-inert,
                                            store-what-parses, 256 KiB cap,
                                            stored verbatim) the operator
                                            hand-positions islands/artifacts
                                            into, overlaid on the machine
                                            layout above. No membership: a
                                            node exists only because a hand
                                            placed it (not a list — see the
                                            route module doc); atlas.field.
                                            updated SSE on PUT. CLI: kb atlas
                                            field / kb atlas field set.
GET  /api/kb/{kb}/atlas/field/disagreement  W3 F-b: machine-vs-operator
                                            displacement — Procrustes-aligned
                                            server-side (same fit atlas/
                                            history/{id} uses), sorted
                                            largest-disagreement-first, ids
                                            present on only one side dropped
                                            (never invented). CLI: kb atlas
                                            field diff.
GET  /api/kb/{kb}/echoes[?limit=N]          W2.2: on-this-day pull surface —
                                            created/read/worked-on this UTC day
                                            at 6mo/1-3y anniversaries (29-Feb +
                                            short-month days skip, never clamp;
                                            sessions newest-capture collapsed).
GET  /api/kb/{kb}/history/calendar[?from=&to=]
                                            W2.10: per-UTC-day open/search/
                                            comment counts (bounded GROUP BY,
                                            ~400-day span cap) behind the
                                            history view's density grid.
GET  /api/kb/{kb}/timeline[?from=&to=]      C-a: four synchronized, UTC-day-
                                            bucketed lanes (created/read/
                                            session/comment) sharing one axis,
                                            each with a capped resolved
                                            artifact-id set for gallery pivot
                                            (?ids=). CLI: kb timeline.
GET  /api/kb/{kb}/daycard[?day=&since=       Unit 2: the e-ink desk radiator —
     &format=]                               a deterministic, per-(kb, UTC day)
                                            "day at a glance" view built ONLY
                                            from existing primitives (top
                                            resurface items, today's
                                            open/search/comment counts, a
                                            couple of recent/never-opened
                                            docs). Content-negotiated: no
                                            Accept: application/json header
                                            and no ?format= => a
                                            self-contained HTML+inline-SVG
                                            document (one URL, no JS, no
                                            external assets — an e-ink panel's
                                            whole diet); Accept: application/
                                            json or ?format=json => the same
                                            data as JSON (SPA's /ambient, `kb
                                            daycard`). day defaults to today
                                            (UTC). No daemon bitmap pipeline —
                                            the daemon emits markup only; pull-
                                            only (no push, no streaks/goals/
                                            completion %, no badges). CLI: kb
                                            daycard. CT-E1 — ?since=<unix|
                                            YYYY-MM-DD> ("what happened while
                                            I was away") replaces the day view
                                            with a since..now window,
                                            mutually exclusive with day=
                                            (400 on both): sessions captured
                                            in-window, memory-note docs by
                                            created time, every other
                                            artifact by mtime, and comments
                                            raised in-window + how many of
                                            those are still open. `to_unix` is
                                            wall-clock "now", NOT reproducible
                                            like day mode. Every lane is
                                            capped + honestly flagged
                                            `_truncated`. CLI: kb daycard
                                            --since <unix|YYYY-MM-DD>.
GET  /api/kb/{kb}/boards/{list_id}/canvas   W2.4: a reading list's JSON Canvas
PUT  /api/kb/{kb}/boards/{list_id}/canvas   geometry sidecar (<src>/boards/
                                            <list_id>.canvas — index-inert,
                                            corpus-versioned, stored verbatim
                                            after a store-what-parses gate,
                                            256 KiB cap; board.updated SSE).
                                            CLI: kb board / kb board set.
GET  /api/kb/{kb}/artifacts/{id}/prompt     W2.11: the stored kb-prompt column,
                                            behind the EXACT artifact-bytes
                                            scrub gate — strip_kb_prompt +
                                            non-loopback => {prompt:null,
                                            stripped:true}; redactions applied;
                                            loopback verbatim. CLI: kb prompt.
POST /api/kb/{kb}/review/{id}/verdict       W2.15a: three-state review verdict
DEL  /api/kb/{kb}/review/{id}/verdict       (comment|approve|request_changes,
                                            note?) in the kb-comments/1 sidecar
                                            under review_lock; kb-tags mirror
                                            status-approved/-changes-requested.
                                            CLI: kb comments verdict.
POST /api/kb/{kb}/proposals                 W2.15b: the tribal-knowledge
GET  /api/proposals[?kb=]                   proposal inbox — agent-authored
POST /api/kb/{kb}/proposals/{id}/approve    kb-proposal/1 candidates in a
POST /api/kb/{kb}/proposals/{id}/reject     per-kb .proposals/ queue; approve
                                            fires the existing memory ingest
                                            with session provenance; the human
                                            gate is the point. CLI: kb propose,
                                            kb proposals.
GET  /api/anchors/stale                     Q1: fleet-wide cold load for the
                                            SPA stale-anchors dashboard.
                                            Reads each kb's persisted
                                            .anchors-stale.json sidecar.

# v0.14 — sessions (track S)
GET  /api/sessions[?cursor=<unix>&limit=N   Cross-kb list of captured Claude
     &folder=&q=&project=&substance=         Code transcripts (memory-session
     &harness=]                              artifacts), newest-first. Cursor
                                            paginated; default limit 50, cap
                                            1000. Response carries
                                            `next_cursor` (omitted at EOL).
                                            `folder=` (a full cwd path) is
                                            LEGACY — the SPA no longer emits
                                            it (`web/src/lib/sessionsUrl.ts`
                                            is the sole builder). W3.A —
                                            `project=` is a `[projects.*]`
                                            registry id OR a raw derived
                                            `project_key`; filtered in SQL
                                            WHERE (keyset-cursor-safe).
                                            `substance=` (W3.C/S1) is a csv
                                            over `trivial|routine|
                                            substantive`; absent/empty =
                                            no filter (every session shown,
                                            including un-backfilled `NULL`
                                            rows, which always count as
                                            substantive). `harness=` (W5/I)
                                            is a csv over the closed set
                                            `claude|codex|opencode|grok|kimi`
                                            (`kb_core::sessions::HARNESSES`),
                                            closed-set validated (unknown
                                            tokens dropped, never a 400);
                                            absent/empty = no filter. CLI:
                                            `kb sessions list`/`kb sessions
                                            search <q>` (`--folder`/
                                            `--project`/`--substance`/
                                            `--harness`/`--limit`).
GET  /api/sessions/projects                 W3.A/P4 — one card per
                                            `[projects.*]` registry entry or
                                            auto-project (`source:
                                            "registry"|"derived"`), merged
                                            cross-corpus: `count`, `latest`/
                                            `earliest`, `edited_total`,
                                            `token_total`, `commit_total`,
                                            `error_sessions`,
                                            `active_secs_total`, and a
                                            `harness_mix` breakdown. Registry
                                            entries collapse every raw
                                            `project_key`/cwd whose
                                            `repo_root`/cwd falls under a
                                            declared root; everything else
                                            becomes its own auto-project
                                            (`id = project_key`, `label =
                                            basename`). Powers the SPA
                                            projects home
                                            (`/sessions?view=projects`).
GET  /api/sessions/{session_id}             Single session + memory_ids list.
                                            404 when no kb holds a matching
                                            enrichment row. CLI: `kb sessions
                                            show <id>` (fetches this + the 8
                                            sub-resources below — scope with
                                            `--section a,b,...` to fetch only
                                            some of them) and `kb sessions
                                            resume <id> [--json]`.
GET  /api/sessions/{session_id}/memories    Full memory rows for the session
                                            (transcript itself excluded).
GET  /api/sessions/{session_id}/recalls     MI-W4.2c — the PULL side of
                                            /memories' WRITE side: every
                                            memory this session's kb-recall
                                            hook actually injected, read
                                            from the session's OWN
                                            memory_recalls ledger (a single-
                                            kb read — the ledger lives with
                                            the RECALLING session's kb).
                                            title/source_relative are best-
                                            effort (absent when the memory
                                            no longer resolves — the ledger
                                            row still stands regardless).
                                            {recalls:[{kb, id, title?,
                                            source_relative?, turn_id?,
                                            recalled_at?}]}.
GET  /api/sessions/{session_id}/touches     Artifact ids the transcript body
                                            references (12-hex literal hits =
                                            confidence=exact;
                                            source-relative path substring =
                                            fuzzy). LRU-cached on
                                            (kb, artifact_id, mtime_unix).
GET  /api/sessions/{session_id}/readings    RP — what the HUMAN read in the SPA
                                            during the session's time window
                                            (history opens + read %), the
                                            read-counterpart to /touches (what
                                            the agent referenced). Cross-kb.
GET  /api/sessions/{session_id}/research    R4 — the kb/web searches, subagents,
                                            skills, and plan spans the session
                                            ran (detected, not ground truth).
GET  /api/sessions/{session_id}/comments    R5 — open review comments on the
                                            in-corpus artifacts the session
                                            touched (computed join; #6-safe).
                                            Cross-kb.
GET  /api/sessions/{session_id}/commits/{sha}/files
                                            MI-W4.6 — the provenance thread's
                                            3rd hop: the files `sha` (one of
                                            the session's recorded commits)
                                            touched, each with a best-effort
                                            "changed again since" flag + last-
                                            touched timestamp (one bounded,
                                            on-demand git read via
                                            kb_core::vcs::commit_touched_files
                                            — no new storage). `available:
                                            false` (never a 404) when the
                                            commit's capture-time resolution
                                            never ran or the repo isn't
                                            resolvable on this host.
GET  /api/sessions/{session_id}/replay      W3.R-b — the `session-replay/1`
     [?artifact=<id>&limit=N&scrub=…]        timeline: one beat per narrative
                                            moment (prompt · assistant · read/
                                            edit/write · bash · commit ·
                                            decision · search · subagent) on
                                            the transcript's own clock, run-
                                            collapsed with honest truncated/
                                            dropped counters. Built from the
                                            NEWEST capture (#11); each beat's
                                            raw path resolves to
                                            (kb, artifact_id, source_relative)
                                            and — for a ranged read — the
                                            nearest preceding heading id
                                            (`kb:scroll-to-id`); an out-of-
                                            corpus path keeps its plain path.
                                            **Fail-closed like /export**: a
                                            non-loopback client always gets the
                                            `secrets` redaction floor before
                                            the timeline is built (#4), with
                                            `x-kb-session-scrubbed` /
                                            `x-kb-redactions` on the response.
                                            LRU-cached on (kb, artifact_id,
                                            mtime_unix, scrub-posture).
                                            CLI: `kb sessions replay <id>`
                                            (`--json` = this wire verbatim).
                                            R7/S6: `?from_seq=&limit=` is a
                                            SERVE window over the FULL computed
                                            timeline (kb-core's compute cap
                                            raised 2,000→50,000, a safety
                                            ceiling only); response gains
                                            `window:{from_seq,returned,total}`.
                                            Beats gain `turn` (a best-effort
                                            `t-<uuid12>` ref into
                                            `session-view/1`) and the
                                            `outcome` kind (the closure beat).
GET  /api/sessions/{session_id}/view        W2/R1 — the `session-view/1`
     [?fields=header,turns,outline,          interpreted IR (ONE engine, three
       tasks,subagents,minimap,stats         presenters — this is the wire
       &turns=A..B&scrub=…]                  presenter): joined turns, outline,
                                            task board, subagent summaries,
                                            minimap, and honesty stats.
                                            `?fields=` projects to a subset
                                            (absent = everything); `?turns=A..B`
                                            windows the `turns`/`side_lanes`
                                            sections by turn ORDINAL. Newest-
                                            capture-scoped (#11), scrub-first
                                            then interpret, same fail-closed
                                            non-loopback floor as `/replay`
                                            (#4). LRU-cached on (kb,
                                            artifact_id, mtime_unix,
                                            scrub-posture) — the FULL view is
                                            what's cached; `?fields=`/`?turns=`
                                            are response-time projections.
                                            CLI: `kb sessions read <id>`
                                            (`--json` prints this wire
                                            verbatim; `--full`/`--tail N`/
                                            `--turn A..B`/`--grep PAT` select
                                            what's shown).
GET  /api/sessions/{session_id}/raw         W2/R1 — the decoded JSONL
     [?scrub=…]                              transcript, `text/plain`, byte-
                                            identical to what `claude -r`
                                            would read (`?raw=1` companion at
                                            the wire grain). The LEAST
                                            mediated surface kb serves: a
                                            non-loopback client's floor is
                                            STRICTER than `/export`/`/replay`/
                                            `/view` — `entropy` is forced on
                                            alongside `secrets` (#4).
                                            `Cache-Control: no-store`.
                                            Newest-capture-scoped (#11). CLI:
                                            `kb sessions read <id> --raw`.
GET  /api/sessions/presence                 W7 (sessions-rethink R15/LF-1) —
                                            the Tier-1 stat-only presence
                                            probe over `[sessions]
                                            live_transcripts_dir` (opt-in,
                                            daemon-wide, unset by default):
                                            `{enabled, live:[{session_id,
                                            mtime_unix, bytes,
                                            project_slug}]}`. Bounded
                                            `readdir` walk, no file opened.
                                            `enabled:false` (the stable, cheap
                                            answer) when unconfigured — every
                                            prod/phone deployment today.
                                            **Loopback-only HARD** (403 even
                                            with a valid token — `#4`'s
                                            fail-closed ethos extended;
                                            stricter than every other
                                            sessions route, since
                                            `project_slug` discloses
                                            filesystem paths and there is no
                                            redaction layer for it).
                                            `Cache-Control: no-store`. Static
                                            route.
GET  /api/sessions/{session_id}/live        W7 (R15/LF-3b) — a loopback-only,
     [?from=<byte>&raw=1]                    request-response DELTA over the
                                            LIVE JSONL transcript (same
                                            `[sessions]
                                            live_transcripts_dir` as
                                            `/presence`). No standing stream
                                            (#24) — poll every few seconds,
                                            loop `from = next_from` until
                                            `next_from === size`. Interpreted
                                            `session-view/1` IR events by
                                            default (`events:
                                            [ViewEvent...]`, incremental via
                                            a server-side carry LRU — always
                                            falls back to a stateless
                                            2 MiB-window bootstrap on a
                                            miss); `?raw=1` returns decoded
                                            JSONL lines instead
                                            (`raw_lines:[...]`), no
                                            interpretation. Response also
                                            carries `{sid, from, next_from,
                                            size, live, ended,
                                            truncated_restart,
                                            parse_failures}`. **Loopback-
                                            only HARD** (403, same posture as
                                            `/presence` — a live transcript
                                            is unscrubbed mid-flight content,
                                            stricter than `/raw`'s forced-
                                            secrets-floor). `Cache-Control:
                                            no-store`. CLI: `kb sessions read
                                            --live [--follow] <id>` (direct-
                                            disk, zero daemon required — the
                                            SAME `resolve_live_transcript`
                                            resolver this route uses).
POST /api/sessions/beat                     LSC-2/3 (live-sessions cockpit
                                            §5/§6) — adapter push intake: one
                                            lifecycle EVENT (`start|prompt|
                                            tool|turn_end|blocked|unblocked|
                                            end`) per POST, never a state —
                                            the daemon alone DERIVES state via
                                            `kb_core::sessions::live::
                                            derive_state`. Body `{v,
                                            session_id, harness, event, at,
                                            host?, pid?, cwd?, model?,
                                            lease_secs?, title?, last_line?,
                                            detail?}`; unknown keys ignored
                                            (hook/daemon version-skew is
                                            expected), a malformed beat 400s
                                            (problem+json), never a 500.
                                            Daemon-wide, NOT `{kb}`-scoped (a
                                            beat arrives before any capture
                                            exists to attribute it to).
                                            Storage is NOTHING — an in-memory
                                            registry (LF-7), 7-day TTL, capped
                                            at 4096 sessions. Fires
                                            `session.state` SSE only when the
                                            derived state actually CHANGES
                                            (never per beat, #24). Response
                                            `{ok, session_id, state, holder,
                                            confidence}`. `auth_bearer` — see
                                            invariant #11's LSC amendment for
                                            why this lane is NOT
                                            loopback-only like `/presence`/
                                            `/{id}/live` above. CLI:
                                            `plugins/kb-memory/hooks/
                                            kb-beat.sh <harness> <event>`
                                            (fire-and-forget, hard 2s
                                            timeout, unconditional exit 0 —
                                            see
                                            docs/live-sessions.md).
GET  /api/sessions/live-status              LSC-2 — the merged cockpit read:
     [?state=&harness=&project=&limit=]     every beat-tracked registry row
                                            (`source:"hook"`,
                                            `confidence:"observed"`) PLUS a
                                            Tier-0 degraded layer rebuilt from
                                            recent captures with NO registry
                                            entry (`source:"capture"`,
                                            `confidence:"presumed"`), fanned
                                            out over `state.kbs` via
                                            `buffered_join` (#28) — so a
                                            freshly-restarted daemon is honest
                                            rather than empty. Ordered/capped
                                            server-side per lane
                                            (working+stalled
                                            most-recently-active-first,
                                            waiting longest-wait-first,
                                            cold+finished+presumed_ended
                                            newest-first); `limit` caps EACH
                                            lane independently. `auth_bearer`,
                                            deliberately NOT loopback-only
                                            (unlike `/presence`/`/{id}/live`
                                            above — see invariant #11's LSC
                                            amendment): a non-loopback caller
                                            gets the SAME rows through a
                                            forced-scrub floor (`cwd`
                                            collapsed to its basename,
                                            `last_line` re-capped) rather
                                            than a 403. `Cache-Control:
                                            no-store`. CLI: `kb sessions
                                            status` (this IS the default
                                            mode; `--local` is the
                                            direct-disk, every-harness twin).
GET  /api/sessions/by-artifact/{kb}/{aid}   W2/R11 — the canonical
                                            artifact→session join
                                            (`SessionsGetByArtifactIds`, #11):
                                            `{session, newest}` where `newest`
                                            is whether `{aid}` IS the session's
                                            newest capture. Single-kb (no
                                            fan-out); 404 when the artifact
                                            isn't a session capture at all.
                                            Replaces the SPA's old filename-
                                            regex `SessionSelfLink`.
GET  /api/sessions/recollect?q=…            R3 — episodic "has this been done?"
     [&folder=&project=&since=day|week|      semantic search over session
      month|year&limit=N]                    DIGESTS (not raw JSONL), re-ranked
                                            relevance × recency; each hit
                                            surfaces staleness/errors/commits
                                            + (V0029) `outcome`/`harness`/
                                            `project_key`. Pull-only. Cross-kb.
                                            `project=` (W3.A/C, P5's "deep
                                            search" lane) composes (AND) with
                                            `folder=`. CLI: `kb recollect <q>
                                            [--folder] [--project] [--since]
                                            [--raw]` (`--raw` prints the
                                            digest excerpt that matched —
                                            `RecollectSessionOut.summary`,
                                            the R1 rank surface).
GET  /api/why?path=<file>                   R2 — the WHY assembler: the past
                                            sessions that touched a file + their
                                            prompt/decisions/commits. Basename-
                                            matched, exact/fuzzy labelled.
                                            Cross-kb. CLI: `kb why <file>`.
GET  /api/artifacts/{kb}/{id}/sessions      AS — the sessions that worked with
                                            THIS artifact, newest-first. Matched
                                            EXACTLY on the corpus-resolved
                                            `session_files.target_artifact_id`
                                            (no same-name false positives). Each
                                            session reports the DISTINCT actions
                                            it took (`read`/`wrote`/`edited` —
                                            not collapsed to one), an `authored`
                                            flag for the artifact's origin
                                            session (lance `kb_session`, unioned
                                            in even with no file row), its
                                            `first_user_prompt`, and — for the
                                            top mutating/authoring sessions —
                                            `decisions`+`commits`. Powers the
                                            reader rail's filterable Sessions
                                            panel. Cross-kb. CLI: `kb sessions
                                            of <artifact-id-or-path> [--kb]`
                                            (W4 — previously unreachable from
                                            the CLI at all).
GET  /api/sessions/by-commit?sha=<sha>      kb-code Wave 0 (W0.6) — the
                                            sha→session reverse lookup: every
                                            session_commits row (newest
                                            capture only, #11) whose `sha` OR
                                            `sha_full` (V0025) starts with the
                                            given full/short sha (>=7 hex
                                            chars — shorter is 400). Cross-kb.
                                            CLI: `kb sessions by-commit <sha>`.
GET  /api/sessions/commit-map               kb-code Wave 0 (W0.6) — the flat,
     [?since=<unix>&limit=N&offset=N]        offset-paginated bulk feed of
                                            every session_commits row (newest
                                            capture only, #11), across every
                                            corpus. Default limit 500, cap
                                            5000. Feeds kb-code's wave-3
                                            sha→session join precomputation —
                                            no per-row title/lance resolution.
                                            CLI: `kb sessions commit-map`.
GET  /api/sessions/by-job/{ulid}            W4/memo R8/ADD-2 — the grokclaude
                                            job join: every session whose
                                            transcript recorded a
                                            `session_research` row
                                            `kind="grok_job"` for this job
                                            ulid, tagged `driver` (a Claude
                                            Code session that INVOKED the job
                                            via a `grokclaude research|
                                            session|build|panel|fleet` Bash
                                            call) or `child` (the grokclaude
                                            job's own capture, W5). Cross-kb.
                                            CLI: `kb sessions by-job <ulid>`.
GET  /api/sessions/ledger                   W6/moonshots M4 — one project's
     [?project=<key>&days=N]                sessions/commits/decisions/
                                            research grouped by UTC calendar
                                            day over a trailing window
                                            (default 7 days, max 31). A pure
                                            VIEW over existing primitives
                                            (the daycard precedent) — no new
                                            tables, newest-capture-scoped
                                            (#11), federated (#28). `project`
                                            absent = every project in the
                                            window. Every calendar day in the
                                            window is present in the response
                                            even when empty. CLI:
                                            `kb sessions ledger [--project]
                                            [--days]`.
GET  /api/sessions/folders                                              W3.A/P4 — the per-folder activity list (one row per folder). Powers the session gallery filter. CLI: `kb sessions folders`.
GET  /api/sessions/funnel                                               R9 — the activity funnel: searched → opened → edited → committed → commented. CLI: `kb sessions funnel`.
GET  /api/sessions/research-rollup                                      R9 — top research queries per project. CLI: `kb sessions rollup`.
POST /api/sessions/threads/save                                         P8 — save a folder's most-recent thread as an editable reading list. CLI: `kb sessions save-thread`. Body `{kb,title,artifact_ids[,narrative]}`; `narrative:true` (CT-E5) makes the list NARRATIVE-ordered — per session, four lanes: capture → artifacts touched (edits before reads) → memories produced (`kb_session`) → memories recalled (`memory_recalls`), each entry noting its lane, duplicates told once at their earliest lane. Only artifacts that resolve in the list's own corpus become entries (lists are single-corpus); anything else is COUNTED in the description, never listed as a tombstone. The description carries the ordering contract, each session's id + capture date, and the kb-code session-diff link when `[kb.*] code_url` is configured (a rendered link — kb never calls kb-code). Absent/false keeps the flat one-entry-per-session shape.


# slate — kb-slate/1, the per-project blackboard (SL2). DAEMON-WIDE, not
# {kb}-scoped: a slate keys on a PROJECT slug and a project maps to several
# kbs or none (D2), so the store is <state>/slates/<slug>/{ledger.jsonl,
# meta.json} — an append-only JSONL ledger under its own per-slug lock
# (invariant #6's SLATE amendment), never sqlite, never the storage actor,
# never a generation bump. Design: docs/research/kb-slate-design-2026-09.html.
#
# Posture (§8), the SAME graduation `/sessions/beat` + `/sessions/live-status`
# took and for the same reason: every route below rides `auth_bearer` and is
# deliberately NOT loopback-only, so the SPA board behind an identity-aware
# reverse proxy works. A non-loopback caller gets the SAME
# posts through a forced-scrub floor — `cwd` collapsed to its basename and
# NOTHING else (re-capping lines/bodies at OUTCOME_WIRE_MAX_CHARS would
# truncate every body and every sketch at kb.example.com; they already carry
# their own 200/2,000 caps). The ONE exception
# is the purge below, loopback-only HARD and checked inside the handler like
# `/sessions/presence`. Every response is `Cache-Control: no-store`. One trust
# tier: any identity may done/take-over/close; the two frictions (`anyway`,
# and pin reserved to `origin: human`) are the human-vs-agent role split kb
# already has, NOT an ACL. Reads never lock and never write — the cursor is
# client-side and `?since=` only drives the `seen` header.
GET  /api/slates                            every slate: {slug, head_seq, generation, updated_unix, closed, topics[], counts{now,warn,hand_unack,ask_open,take_live,take_stale,take_contested,found,idea,tried}, sessions_served}. The board's attention chip = hand_unack + ask_open + take_contested + take_stale, summed CLIENT-side — the daemon never sums an attention number for you.
GET  /api/slates/{slug}                     the digest (THE projection, `kb_core::slate::project`). `text` is byte-identical to what `kb slate open` prints. `?mode=full|hybrid` (hybrid = the session-start injection block), `?budget=` CHARACTERS not tokens (default 6,000 full / 2,000 hybrid — the daemon links no tokenizer, so a token budget would be a character budget in costume), `?topic=`, `?all=1` (no truncation), `?session=` (marks "your own asks with new answers"; NEVER written), `?since=` (the client-side cursor; drives the `seen #a → #b` header only). Response = SlateDigest + {generation, closed}.
GET  /api/slates/{slug}?view=board[&topic=] the SPA board projection: every shown post as a BoardCard = Projected + {body, marks_by[], has_sketch}. NEVER budget-truncated — the board scrolls. `has_sketch` is a ```mermaid fence in the body (D21: there is no `sketch` kind).
GET  /api/slates/{slug}/posts[?since=&limit=]   RAW Post[] after a sequence number — the watch loop's refetch, the one read that hands back the ledger as written (minus the scrub floor).
GET  /api/slates/{slug}/posts/{id-or-seq}   one post unfolded: the body, its refs as display strings, and the thread beneath it (answers, done, drops, the edit that replaced it). `post:#n` resolves to that post's line, `kb:<kb>/<id>` and `mem:<id>` to a title, `session:<sid>` to the newest capture's title; `path:`/`job:`/`commit:`/`plan:` render verbatim with `resolved:false` — the daemon has no tree and never guesses.
GET  /api/slates/{slug}/delta[?since=&session=&budget=&limit=&kinds=]   the per-prompt lane: `{posts, hides:[{hide,by,reason,who,why}], text, truncated, filtered}`. `?kinds=` (D26) is a csv over the twelve kind words that narrows the posts AND the hides (a hide passes when its TARGET carries one of the kinds) and renders `text` from the narrowed set — the push adapters ask for `now,warn,hand,ask,answer` and leave found/idea/tried pull-only; an unknown word is a 400 `bad-kind`, never a silently empty delta, and an absent/empty `?kinds=` is `filtered:false`. `hides` is what makes a delta consumer's fold agree with a full projection (a later post can hide an earlier one). NO echo line and no header — that is the full digest's job. Hooks and `kb slate delta` read this, never `…/posts`.
GET  /api/slates/{slug}/history[?since=&limit=] dropped and SUPERSEDED posts only, newest first, each naming the hiding post, its author and its reason. A `done` closes an item, it does not tombstone it, so it is never here. Current generation only; the archives are for distill.
POST /api/slates/{slug}/posts               append any of the twelve kinds (now·warn·take·done·hand·ask·answer·found·idea·tried·drop·mark). Creates the slate on first post — there is no create verb. UNDER THE PER-SLUG LOCK: read meta → mint seq → validate → conflict + live-author checks against the #11 live registry → O_APPEND + fsync → atomic meta → project before/after at the DEFAULT budget → 201 `{post, displaced[≤5], displaced_total, nudge, head_seq}`. `displaced` is the poster's only feedback loop with the finite surface and never blocks the write (D18: room is never a reason to refuse a post); NOW, WARN, unacknowledged HAND and pinned posts are never displaced. Refusals are problem+json carrying `code` as an RFC 7807 extension member AND as `detail`'s `<code>: <text>` prefix: 409 slate-taken (with `holder{seq,line,harness,session_short,age_secs,liveness}`) · 409 slate-live-author · 409 slate-closed · 413 slate-full + the size/count caps · 429 slate-rate · 400 pin-is-human / no-ascii-art / already-done / kind-mismatch / bad-ref / ask-needs-question / found-needs-ref / self-mark / unknown-harness. A duplicate plain `mark` by the same session returns 200 with the EXISTING post and writes nothing. A post with no session id is stamped `origin: unattributed`, never refused; `prov.user` comes from the resolved Identity (attribution, not authorization).
POST /api/slates/{slug}/close               append a terminal `now` (topic null) with `{line, prov}` and set `closed_unix` — appends refuse 409 slate-closed until reopen.
POST /api/slates/{slug}/reopen              clear `closed_unix`; appends NOTHING.
POST /api/slates/{slug}/cursor              D27 — a session REPORTS the newest seq it has been SERVED: `{session_id, seq, harness?}` → 201 `{session_id, seq, head_seq}`. Under the per-slug lock, `meta.json` `cursors` only; MONOTONIC (a lower report is ignored and rewrites nothing), bounded by `head_seq` (a higher one is 400 `bad-cursor`), and it EMITS NOTHING — `slate.updated` fires once per append (#24). Fire-and-forget from the CLI after a successful `open`/`delta`; NO read ever writes it. The projection derives `Projected.seen_by` (session_short of every other session whose cursor reached the post) and renders `seen by N` on whole-tier NOW/HAND/ASK lines; `GET /api/slates` reports `sessions_served`. A cursor is attribution, not acknowledgement of reading, and nothing expires it (D5).
POST /api/slates/{slug}/rotate              archive `ledger.jsonl` as `ledger.<gen>.jsonl`, start a fresh one, `generation+1`, `rotated_from`. ONE directory per slug, always (never a `<slug>@2`, which the slug grammar refuses). `head_seq` does NOT reset — seqs stay unique across generations so a parked cursor still advances — and rotating does not close.
DELETE /api/slates/{slug}?purge=true        LOOPBACK-ONLY hard delete of the whole slate (checked inside the handler, like `/sessions/presence`); emits `slate.deleted`. `?purge=true` is required: an unqualified DELETE is a 400. There is no soft delete — the remedies for "too much" are drop, close and rotate.
                                            # SSE: slate.updated {slug, seq, kind, id, topic, re, hide, pin}
                                            # fires ONCE PER APPEND, never per read (#24); `kind` is the
                                            # post's kind or the lifecycle verb (close|reopen|rotate), and
                                            # `hide` is the seq this post removed from the shown set (a drop
                                            # or a supersede) or null. slate.deleted {slug} on purge.
                                            # Filter server-side with `?filter=slug:<slug>` on /api/events.

# v0.2 — comments + graph
GET  /api/kb/{kb}/review/{id}               kb-comments/1 JSON + ETag (read).
                                            R8: the whole-doc write POST is
                                            retired; mutate via the fine-grained
                                            endpoints below (each runs load →
                                            mutate → save under review_lock +
                                            emits comments.updated, no If-Match).
POST   /api/kb/{kb}/review/{id}/comments              add a comment → 201
PATCH  /api/kb/{kb}/review/{id}/comments/{cid}        edit a comment body
DELETE /api/kb/{kb}/review/{id}/comments/{cid}        delete a comment
POST   /api/kb/{kb}/review/{id}/comments/{cid}/replies        add a reply → 201
PATCH  /api/kb/{kb}/review/{id}/comments/{cid}/replies/{rid}  edit a reply body
PATCH  /api/kb/{kb}/review/{id}/comments/{cid}/anchor         re-point the anchor (R9)
DELETE /api/kb/{kb}/review/{id}/comments/{cid}/replies/{rid}  delete a reply
POST   /api/kb/{kb}/review/{id}/comments/{cid}/resolve        resolve one
POST   /api/kb/{kb}/review/{id}/comments/{cid}/unresolve      reopen one
POST   /api/kb/{kb}/review/{id}/resolve-all                   bulk resolve
POST   /api/kb/{kb}/review/{id}/unresolve-all                 bulk reopen
POST   /api/kb/{kb}/review/{id}/apply                         v0.19: atomic batch of
                                            ordered comment mutations (add/reply/edit/
                                            set_anchor/resolve/…/delete) under ONE
                                            review_lock + one save + one comments.updated
                                            SSE. All-or-nothing — any failing op rolls
                                            the whole batch back. Body {ops:[{op,…}]};
                                            creates the file when the first op adds.
POST   /api/kb/{kb}/review/{id}/import[?force=]               v0.19: write a whole
                                            kb-comments/1 doc to the sidecar (inverse of
                                            `kb comments export --embed`), preserving
                                            ids/statuses/replies/timestamps. Refuses to
                                            overwrite existing non-empty comments unless
                                            force=true; re-pins the artifact ref.
POST   /api/kb/{kb}/review/{id}/attachments                   stage upload(s) (multipart) → 201 [Attachment+url]
GET    /api/kb/{kb}/review/{id}/attachments/{aid}             serve a blob (sniffed CT + nosniff; raster inline, else download)
POST   /api/kb/{kb}/review/{id}/comments/{cid}/attachments            upload + adopt → comment
POST   /api/kb/{kb}/review/{id}/comments/{cid}/replies/{rid}/attachments  upload + adopt → reply
DELETE /api/kb/{kb}/review/{id}/comments/{cid}/attachments/{aid}            detach (comment)
DELETE /api/kb/{kb}/review/{id}/comments/{cid}/replies/{rid}/attachments/{aid}  detach (reply)
                                            Y-track: file/image attachments.
                                            add/reply gain attachment_ids[] to
                                            adopt staged blobs; body markdown
                                            embeds them with ![](attachment:aid).
GET  /api/kb/{kb}/reviews[?folder=&status=open|resolved|all&author=you|claude&stale=]
                                            list/query comments across a kb.
GET  /api/inbox[?kb=NAME&limit=N]           Z4: fleet-wide inbox of OPEN comments
                                            across EVERY kb (fanned out, #28),
                                            newest activity first. {items,total_open}
                                            — each item carries kb, artifact id +
                                            source-rel + title, comment id, excerpt,
                                            reply_count, anchor scope + stale,
                                            created_at/updated_at.
POST /api/kb/{kb}/review/{id}/export?format=claude|json|md
                                            v0.5 P3: server-side render via
                                            kb_core::review::export. Chunked
                                            streaming, 32 MB cap (v0.7.1 H7).
GET  /api/kb/{kb}/graph/{id}[?depth=N]      v0.2: heading hierarchy.
                                            v0.3: ?depth=1..3 adds
                                            cross-artifact `link` edges from
                                            sqlite edges table (F1).
GET  /api/kb/{kb}/graph/report[?top=N]      GS-track: the deterministic corpus
                                            graph report — hubs, orphans (never
                                            linked AND never opened), dead-edge
                                            link-rot, and dangling/ambiguous
                                            wikilinks re-resolved over the
                                            Markdown sources. Pure read; never
                                            bumps the index generation. The
                                            Markdown re-parse reads source bytes
                                            from disk, so cost scales with the
                                            corpus's Markdown footprint — an
                                            explicit operator verb (`kb graph
                                            report`), not a hot-path dependency.
GET  /api/kb/{kb}/tags                      distinct tag → doc-count rollup
                                            (drives gallery facet chips).
GET  /api/kb/{kb}/facets                    {categories,statuses,severities}
                                            distinct value → doc-count rollups
                                            (search rail meta facets).
GET  /api/kb/{kb}/resurface?limit=          {items,now_unix} deterministic
                                            pull-only resurfacing queue (open
                                            comments + unfinished reads),
                                            reasons on every item.
GET  /api/kb/{kb}/folders                   distinct folder → doc-count rollup
                                            (gallery folder picker).
GET  /api/kb/{kb}/edges                     {edges:[{src,dst}]} for atlas
                                            cross-artifact link layer.

GET  /api/events[?types=glob,glob&filter=run:ID,kb:NAME,artifact:ID,slug:SLUG]
                                            SSE firehose. Last-Event-ID resume;
                                            ?types= globs (e.g. comment.*,
                                            index.*); ?filter= payload tokens,
                                            ANDed (kb:/artifact: v0.24 T1,
                                            slug: SL2 for the slate family —
                                            for CLI consumers; the SPA worker
                                            stays on the unfiltered stream,
                                            inv #24); synthetic lag/gap
                                            frames. Frame data is enveloped:
                                            {payload:{…}, ts, v}.
GET  /api/events.schema.json                event-type registry (the
                                            authoritative list; ~30 types)
GET  /api/events/schema/{kind}/{version}    per-type payload schema
                                            # history.recorded — v0.6+ H, fires
                                            # on every history INSERT (open's
                                            # 30-min bumps + scroll updates
                                            # are silent); payload carries
                                            # {kb, kind, id, artifact_id?,
                                            # query?, comment_id?}.

# v0.38 CT-D1 — the ONE context pack
GET  /api/context?q=…[&cwd=PATH&budget=N&session=SID&no_floor=true
                    &memory_project=SLUG&memory_visible_to=CSV]
                                            the deterministic, budgeted,
                                            provenance-annotated context pack
                                            for a task. COMPOSES existing
                                            reads in-process — it is not a
                                            fifth assembler and adds no new
                                            storage, no LLM, no kb-code call:
                                            `memories` = /api/memory/recall
                                            with its full invariant-#10 score
                                            decomposition (+ flagged / warns /
                                            drift_open / linked_kbs). The
                                            memories lane was hardcoded
                                            `scope=all`; MS threads
                                            `memory_project=SLUG` and
                                            `memory_visible_to=CSV` straight
                                            into that recall call (project
                                            narrows the corpus set the same
                                            way `kb recall --scope project`
                                            does, visible_to is the same
                                            per-id link filter documented on
                                            /api/memory/recall above) — both
                                            absent ⇒ byte-identical to
                                            pre-MS,
                                            `sessions` = /api/sessions/
                                            recollect narrowed to POINTERS
                                            (id, name, ONE-line digest
                                            excerpt, and R3's surfaced
                                            stale / error_count / commit_count
                                            signals — never a transcript, so
                                            #11's R0/R1/R3 are untouched),
                                            `comments` = the /api/inbox
                                            collect_open walk SCOPED to the
                                            artifacts this query matched
                                            (`[kb-flag]`/`[kb-drift]` bodies
                                            excluded — already surfaced on
                                            their memory row), `code_hints` =
                                            the kb-LOCAL code_refs summary
                                            (invariant #2 HINTS, never a trust
                                            class). Fanned out per corpus
                                            (#28). `scent` is a COUNTS-ONLY
                                            line ("3 prior sessions · 2 open
                                            comments · 5 memories") — the only
                                            thing kb-recall.sh injects on a
                                            session's FIRST turn, so the agent
                                            PULLS substance rather than having
                                            it auto-injected. `budget` (chars,
                                            default 4000, clamped 200..=32000)
                                            is HARD and split into fixed
                                            per-lane shares; every drop is
                                            EXPLICIT — `<lane>_total` is the
                                            pre-cap count, `<lane>_truncated`
                                            flags a short list, and
                                            budget_exceeded says the char
                                            budget (not an item cap) caused
                                            it. `cwd` is not a filter: it
                                            stably floats same-cwd sessions
                                            and sets `same_cwd`. `session` is
                                            YOUR session id, excluded so the
                                            pack can't report you to yourself
                                            (#11 multi-capture). `no_floor`
                                            passes through to recall's floor
                                            bypass (dedup-oracle use only,
                                            ranking math unchanged) and is
                                            echoed on the response. CLI:
                                            `kb context <query> [--cwd]
                                            [--budget] [--session]
                                            [--no-floor] [--json]`.

# v0.9+ M — agent memory (Claude Code's persistent memory)
GET    /api/memory/recall?q=…[&scope=global|project|all&for_kb=NAME&visible_to=CSV&limit=N]
                                            cross-corpus recall ranked on
                                            rank-position × salience × decay
                                            (supersede/forget drops applied at
                                            recall time). HTML by default;
                                            {hits:[…]} when Accept=json. RP: each
                                            json hit also carries read_pct /
                                            last_read_at / stopped_at (the
                                            human's reading state) when
                                            recorded; MI-W1.3 adds recall_count
                                            / last_recalled_at per hit — a
                                            DISPLAY-only enrichment applied
                                            strictly AFTER ranking, from the
                                            memory-recall ledger fanned out
                                            across every kb (invariant #28).
                                            MI-W2.1/2.2, split MI-W5.R — with
                                            `[memory] scoring_v2_relevance`
                                            (daemon-wide, default TRUE —
                                            measured on the live corpus) each
                                            hit ALSO carries relevance_factor
                                            (a per-corpus min-max normalized
                                            search-engine relevance term);
                                            with `scoring_v2_stability`
                                            (default false — unmeasured,
                                            pending a bench over the sessions
                                            corpus) each hit ALSO carries
                                            stability (an FSRS-inspired
                                            ledger-derived decay multiplier,
                                            capped, never immortal). Each
                                            factor is folded into score and
                                            surfaced (never silently
                                            absorbed) independently, iff its
                                            own flag is on, for `kb recall
                                            --explain`. Both flags off ⇒
                                            byte-identical to pre-W2; the
                                            deprecated single `scoring_v2`
                                            key still works as an alias that
                                            sets both. MI-W4.1 adds decay_k
                                            (the per-day rate
                                            `decay`'s exponent used — lets a
                                            client project the decay curve
                                            forward without duplicating the
                                            slow/fast constants). MI-W4.2a
                                            adds an opt-in
                                            `&with_weekly=true` param
                                            populating recall_weekly (an
                                            8-bucket per-week injection-count
                                            histogram per hit, `[]` when not
                                            requested) — the ONE caller that
                                            sets it is the /memory SPA row
                                            sparkline; the hot per-turn
                                            kb-recall hook path leaves it off.
                                            CT-C1 adds `flagged` (bool,
                                            omitted when false): true when
                                            the memory has an OPEN `[kb-flag]`
                                            comment (`kb memory flag`).
                                            Computed strictly AFTER ranking,
                                            bounded to the returned page (one
                                            `.review/<id>.json` read per hit)
                                            — SURFACED, NEVER SCORED.
                                            CT-C3 adds `warns` (bool, omitted
                                            when false): true when the memory
                                            records a FAILED approach
                                            (`kb remember --failed`, the
                                            outcome:failed tag). Zero extra
                                            IO (the tag rides the projected
                                            tags column), applied strictly
                                            AFTER ranking — SURFACED, NEVER
                                            SCORED; the recall hook renders
                                            such hits as "✗ didn't work:"
                                            (composed after "⚠ disputed:"
                                            when a hit is both).
                                            CT-C4 adds `code_hints` (string
                                            list, omitted when empty) +
                                            `code_hints_total` (u32, omitted
                                            when 0): the hit's own extracted
                                            code-ref PATH hints from the
                                            kb-LOCAL code_refs table —
                                            kb-side extraction only, HINTS
                                            not verdicts (invariant #2; no
                                            kb-code call anywhere on this
                                            route), capped at 5 per hit with
                                            the pre-cap distinct total kept
                                            explicit (total > list length ⇒
                                            truncated, never silent) — and
                                            `drift_open` (u32, omitted when
                                            0): count of OPEN `[kb-drift]`
                                            comments filed by the /kb-verify
                                            sweep, read in the SAME single
                                            per-hit .review read as
                                            `flagged` (one read, both
                                            marks). Honesty caveats: hints
                                            are what the memory CITES, not
                                            what exists in any checkout;
                                            drift is at most one /kb-verify
                                            sweep stale (sweep-then-flag —
                                            recall never live-verifies);
                                            neither is ever a score input
                                            (SURFACED, NEVER SCORED). The
                                            recall hook renders drift as a
                                            " [⚠ N drift-flagged
                                            citation(s)]" suffix and never
                                            renders code_hints (json
                                            consumers only).
                                            MS adds `visible_to` (csv of
                                            names, e.g. a project slug):
                                            a per-id link FILTER at the
                                            SAME L7 stage as `for_kb` but
                                            with INVERSE semantics —
                                            unlinked memories and `*`-linked
                                            (global) memories always pass;
                                            a memory linked to specific kbs
                                            passes only when that link set
                                            intersects `visible_to`. Where
                                            `for_kb` answers "what is
                                            visible to kb X's /memory page"
                                            (strict allowlist, unlinked =
                                            invisible), `visible_to`
                                            answers "what should leak into
                                            project X's session" (opt-out,
                                            unlinked = visible) — the two
                                            are deliberately asymmetric, not
                                            a bug. SURFACED-FILTER only,
                                            never a score term (same footing
                                            as the forget/supersede drops).
                                            `kb recall`'s new default
                                            `--scope auto` (CT-MS, see the
                                            CLI entry below) is what
                                            actually sets this param day to
                                            day; the route's own default is
                                            UNCHANGED (`scope=all`, no
                                            `visible_to`) for compatibility
                                            with every existing caller.
GET    /api/memory/census?kb=NAME[&offset=N&limit=N&type=TYPE&sort=unverified]
                                            MI-W1.2 — single-kb, uncapped-total
                                            paginated scan of ONE memory
                                            corpus's artifacts: salience, decay
                                            bucket, age, pin/link/supersede
                                            state (+ a corpus-local supersedes
                                            reverse lookup), tags, origin
                                            session, plus recall_count /
                                            last_recalled_at (same ledger
                                            fan-out as /recall above). MI-W2.3
                                            adds forgotten (true when
                                            kb-status=forgotten) — a census
                                            row DELIBERATELY still lists a
                                            soft-forgotten memory (the whole
                                            point of a tombstone: visible,
                                            not vanished) even though recall
                                            drops it. MI-W3.3a adds memory_type
                                            + `?type=` (episodic | semantic |
                                            procedural), an exact facet filter
                                            applied BEFORE pagination (total
                                            reflects the filtered count).
                                            MI-W3.4 adds source (the write-time
                                            trust tag), display-only.
                                            CT-A1 (U3 parse-back) adds author /
                                            source_kb / source_artifact /
                                            source_anchor — the highlight-
                                            provenance metas a "highlight →
                                            save as memory" write records,
                                            parsed back and surfaced (display
                                            only, never a scoring input) on
                                            both /recall and /census rows.
                                            CT-C3 adds failed (bool, omitted
                                            when false): true when the row
                                            carries the outcome:failed tag
                                            (`kb remember --failed` — a
                                            recorded tried-and-did-NOT-work
                                            approach; recall surfaces the
                                            same rows as `warns`). Display
                                            only.
                                            CT-E4 adds ?sort=unverified —
                                            agent-hot-human-cold rows first:
                                            the (recall_count > 0 AND never
                                            opened by this request's user —
                                            read_pct absent/0, the same
                                            CT-B6 condition the SPA tension
                                            badge uses) bucket leads,
                                            recall_count DESC within/after
                                            it, id ASC ties. Absent sort
                                            keeps the default id-ASC order
                                            byte-identical; any other value
                                            is a 400. Ordering only — never
                                            a scoring input.
                                            {rows:[…], total} — total is the
                                            TRUE uncapped corpus count, not
                                            just this page's length.
GET    /api/kb/{kb}/docs/{id}/memories-from
                                            CT-A1 — every memory highlighted
                                            FROM this artifact (the reverse of
                                            the author/source_kb/
                                            source_artifact/source_anchor
                                            provenance above). Fans out across
                                            every memory-scoped corpus on the
                                            daemon (invariant #28); one
                                            corpus's storage error is logged
                                            and dropped, never a 500 for the
                                            whole response. {rows:[{id, kb,
                                            title, summary, author, anchor,
                                            created_unix}, …]}.
GET    /api/kb/{kb}/memories/{id}/lineage  MI-W2.4a — `kb memory log`'s data
                                            source: walks one supersede chain
                                            both directions (what {id}
                                            supersedes, forward; what
                                            superseded it, reverse), each
                                            node carrying id/title/created/
                                            forgotten. {id, start, 
                                            supersedes_chain, 
                                            superseded_by_chain}. MI-W4.3
                                            adds salience/decay_k/age_days/
                                            pinned per node — the SPA
                                            lineage viewer's per-node inline
                                            decay sparkline reuses the SAME
                                            projection ingredients /recall
                                            carries, never a third
                                            implementation.
GET    /api/kb/{kb}/memories/{id}/recalled-by
                                            CT-B2 — the memory-side reverse
                                            of the memory_recalls ledger (the
                                            session-side view is GET …/sessions/
                                            {sid}/recalls): every session that
                                            recalled this memory, newest-first
                                            (`kb memory recalled-by`'s data
                                            source). Fans out across every kb
                                            on the daemon (invariant #28 — the
                                            ledger lives with the RECALLING
                                            session's kb, not necessarily this
                                            memory's own {kb}); {id} need not
                                            still exist there (a forgotten/
                                            deleted memory's history stays
                                            readable, matching lineage's and
                                            memories-from's own posture).
                                            Capped at 200 rows total, newest-
                                            recalled-first. Labelled "recalls
                                            the capture pipeline saw"
                                            everywhere it's rendered — a
                                            best-effort census, never a
                                            complete injection log.
                                            {rows:[{session_kb, session_id,
                                            session_title?, turn_id?,
                                            recalled_at?}]}.
GET    /api/kb/{kb}/memories/{id}/commits   CT-F1 — the memory↔commit EXACT-ID
                                            join: every commit whose OWN
                                            message named this memory's id in
                                            a `Kb-Memory:` trailer, parsed
                                            back out of the capture envelope's
                                            already-resolved commits block
                                            into `memory_commits` (V0038).
                                            Distinct from the session→commits
                                            heuristic hop (`kb why-memory`'s
                                            chain): that says "the session
                                            that produced this memory also
                                            produced these commits", this says
                                            "this commit cited this memory".
                                            Fans out across every kb (#28 —
                                            the rows live with the RECORDING
                                            session's kb); {kb} is validated
                                            but not a filter (the trailer
                                            carries no kb name), and {id} need
                                            not still exist there. Capped at
                                            50 rows, newest-recorded-first,
                                            deduped by sha across corpora.
                                            **An empty list is a NON-SIGNAL**
                                            — the trailer is opt-in per repo
                                            (`git config --local
                                            kb.memoryTrailers true`) and OFF
                                            by default, so "no rows" almost
                                            always means "that repo never
                                            opted in". `recorded_at` is when
                                            the row was DERIVED, never a
                                            commit date (the envelope carries
                                            none and CT-F1 refuses a second
                                            git read). {rows:[{session_kb,
                                            session_id, sha_full, sha?,
                                            subject?, repo_root?,
                                            recorded_at}]}.
GET    /api/memory/triage[?kb=NAME&limit=N] MI-W4.4 — the bounded, DERIVED
                                            hygiene queue (`kb memory
                                            triage`'s data source). Gathers
                                            candidates from the SAME reads
                                            census/dupes/lineage already use
                                            (list_docs + pinned set per
                                            corpus, the corpus-local reverse
                                            kb-supersedes map, the MI-W3.1
                                            duplicate scan at its own 0.90
                                            default threshold, and the
                                            recall-usage ledger fan-out),
                                            scores with
                                            kb_core::triage::build_queue, and
                                            returns AT MOST `limit` items
                                            (default ~10). Pinned and
                                            forgotten memories never appear;
                                            a candidate with more than one
                                            applicable reason keeps only its
                                            MOST URGENT one. Never mutates.
                                            {items:[{kb, id, title,
                                            source_relative, reason_kind,
                                            reason, urgency, …the ranking
                                            terms behind reason}], scanned}.
GET    /api/memory/tombstone-era           MI-W2.4c — EPOCH HONESTY marker:
                                            {started_unix} — the moment this
                                            daemon became able to soft-forget
                                            (MI-W2.3) rather than hard-delete.
                                            `kb memory log` / `kb diff
                                            --between` caveat a window that
                                            starts before it.
POST   /api/kb/{kb}/artifacts               write a memory artifact (write-only;
                                            the watcher indexes it once). Returns
                                            {id, path}. U3: four ADDITIVE optional
                                            provenance fields — author ("you" |
                                            "claude", the ROLE split, not an
                                            identity), source_kb, source_artifact,
                                            source_anchor (a review::Anchor). The
                                            SPA's highlight → save-as-memory sets
                                            them; every other caller omits them and
                                            gets byte-identical output. Recorded as
                                            kb-author / kb-source-* metas in the
                                            artifact source — SURFACED provenance,
                                            never a recall score term. MI-W3.3a adds
                                            an optional memory_type (episodic |
                                            semantic | procedural, validated closed
                                            set, 400 on an unknown value) — absent
                                            (untyped) by default, never inferred.
                                            MI-W3.4 adds an optional source
                                            (fetched-web | user-dictated |
                                            agent-inference, same closed-set
                                            validation) — the write-time TRUST tag,
                                            recorded as kb-source, SURFACED (census/
                                            recall), NEVER a score term.
GET    /api/memory/dupes[?threshold=0.90&limit=50&kb=NAME]
                                            MI-W3.1 — ON-DEMAND cross-corpus
                                            duplicate report: likely-redundant
                                            memory PAIRS (high embedding
                                            similarity, not already linked by
                                            kb-supersedes in either direction,
                                            neither forgotten), fanned out across
                                            every memory corpus (invariant #28).
                                            NOT a contradiction detector — see
                                            `kb_core::memory::find_duplicate_pairs`'s
                                            doc comment. Flags cross_corpus pairs
                                            distinctly (the high-value case a
                                            same-corpus search can't see). `?kb=`
                                            restricts to one corpus (disables
                                            cross-corpus comparison). NEVER
                                            mutates anything. {pairs:[…],
                                            threshold, scanned}.
PATCH  /api/kb/{kb}/memories/{id}/salience  MI-W3.2b — edit ONLY a memory's
                                            salience (the one mutable memory meta
                                            via the API; patch_meta stays scoped to
                                            tags/category). Body {salience}, clamped
                                            to [0,1]. Splices kb-salience into the
                                            source via the same generic byte-
                                            preserving editors soft-forget uses.
                                            {id, salience}.
DELETE /api/kb/{kb}/artifacts/{id}[?purge=true]
                                            MI-W2.3 — forget a memory.
                                            Default: SOFT forget — splices
                                            kb-status=forgotten +
                                            kb-forgotten-at into the source
                                            (still on disk, still `kb search`-
                                            able, still listed by census
                                            flagged, dropped from recall).
                                            ?purge=true: the pre-W2.3 HARD
                                            delete (file + row gone, no
                                            trace). {id, purged}.
POST   /api/kb/{kb}/memories/{id}/pin       M2: pin a memory above the decay floor
DELETE /api/kb/{kb}/memories/{id}/pin       unpin
POST   /api/kb/{kb}/memories/{id}/promote   promote a project memory to global
GET    /api/kb/{kb}/memories/{id}/links     L6: kbs a memory is linked-visible to
PUT    /api/kb/{kb}/memories/{id}/links     replace the whole link set
POST   /api/kb/{kb}/memories/{id}/links/{target_kb}    add a single link
DELETE /api/kb/{kb}/memories/{id}/links/{target_kb}    remove one
GET    /api/memory/policy                   daemon-wide decay policy. MI-W4.1
                                            adds drop_threshold (the active
                                            policy's salience floor; absent
                                            for loose — never a non-finite
                                            JSON number) so a client's decay-
                                            sparkline reference line never
                                            duplicates the strict/balanced/
                                            loose → threshold mapping.
PUT    /api/memory/policy                   set it (strict|balanced|loose)

# v0.10 K — anchor corkboard
GET    /api/anchors                         cross-kb pinned anchors
POST   /api/kb/{kb}/anchors/{artifact_id}   pin an anchor
DELETE /api/kb/{kb}/anchors/{artifact_id}   unpin

# RL-track (v0.18) — reading lists (ordered, section-aware, derived read state)
GET    /api/lists[?include_archived=true]   cross-kb list index (counts + minutes)
POST   /api/kb/{kb}/lists                   create {title, description?, pinned?}
GET    /api/kb/{kb}/lists/{id}              detail: ordered enriched entries
PATCH  /api/kb/{kb}/lists/{id}              {title?, description?|null, pinned?, archived?}
DELETE /api/kb/{kb}/lists/{id}              delete (entries cascade)
POST   /api/kb/{kb}/lists/{id}/entries      add {artifact_id|path, anchor?, note?, before?|after?|position?}
PATCH  /api/kb/{kb}/lists/{id}/entries/{eid} {note?|null, anchor?|null, read_override?: read|unread|clear, before?|after?|position?}
DELETE /api/kb/{kb}/lists/{id}/entries/{eid} remove (idempotent)
POST   /api/kb/{kb}/lists/{id}/prune         remove every tombstoned entry (artifact no
                                             longer resolves) in one tx → {removed}; one
                                             list.updated when removed > 0 (v0.33)
GET    /api/kb/{kb}/lists/{id}/export        ?format=json|md — portable kb-list/1 doc
POST   /api/kb/{kb}/lists/{id}/import        ?format=md|json&mode=replace|append (body = the doc)

# N-track — notes / todo-lists (Markdown artifacts, kb-category=note;
# excluded from the gallery grid but searchable + commentable)
GET    /api/notes[?folder=&status=]         cross-kb notes fan-out
GET    /api/kb/{kb}/notes[?folder=&status=] per-kb notes list
GET    /api/kb/{kb}/notes/{id}              one note (raw body_md + counts)
POST   /api/kb/{kb}/notes                   create {folder?,title?,body_md?,tags?,status?,notepad?}
PATCH  /api/kb/{kb}/notes/{id}              update {title?,body_md?,status?,tags?}
POST   /api/kb/{kb}/notes/{id}/toggle       flip Nth checklist item {index,on}
POST   /api/kb/{kb}/notes/{id}/tasks        append a task {text}
DELETE /api/kb/{kb}/notes/{id}              delete (file + row)

# Wikilinks / backlinks — a note's [[target]] resolves to a corpus artifact;
# edges ride the existing graph (kind="link"). NoteDetail also carries `links`.
GET    /api/kb/{kb}/notes/{id}/links        outgoing [[…]] (resolved) + backlinks
GET    /api/kb/{kb}/backlinks/{id}          inbound refs to any artifact
GET    /api/kb/{kb}/wikilinks/suggest?q=    [[ autocomplete (title/basename)

# CT-F3 — unlinked mentions ("the graph you wrote is half the graph you
# meant"): docs whose PROSE names another artifact's exact title or unique
# basename with no kind="link" edge to show for it. DERIVED per request,
# never persisted (the dupes/triage posture); a human applies one row at a
# time. Code spans/fences, existing [[…]], Markdown link labels, self-
# mentions, names under 12 chars, ambiguous names and memory-session
# transcripts are all excluded. A memory (or any HTML artifact) may be a
# link TARGET but never a link SOURCE — invariant #29 — so its rows are
# reported with an honest `note` and refused by apply.
GET    /api/kb/{kb}/links/suggest[?limit=]  { suggestions[{src,dst,matched,
                                            match_kind,target,applicable,
                                            note?}], scanned, min_length,
                                            limit }. limit clamps to 1..=200
                                            (default 50); applicable rows sort
                                            first.
POST   /api/kb/{kb}/links/apply             {src,dst} artifact ids — splices
                                            [[target]] (or [[target|matched]])
                                            into src's Markdown source. The
                                            mention is RE-DERIVED first, so a
                                            stale row can't misfire: 404 = no
                                            unlinked mention (already linked /
                                            text changed), 400 = the source is
                                            HTML or a memory body (detail =
                                            the #29 note), 409 = the text is
                                            no longer spliceable. Every
                                            refusal leaves the file untouched.

# DCB — code references extracted from a doc's own bytes (coderef/1). HINTS
# ONLY: kb has no working tree and never resolves them (invariant #2/#4); the
# code daemon's /api/doc-lens turns these into resolved, trust-labelled links.
GET    /api/kb/{kb}/docs/{id}/code-refs      one doc's refs + heading groups
GET    /api/kb/{kb}/code-refs?cursor=&limit=&refs=0
                                            corpus cursor feed (ASC keyset on
                                            extracted_at, artifact_id; cursor is
                                            ONE opaque string). `limit` clamps to
                                            1..=100 server-side. `refs=0` returns
                                            headers only — EVERY doc's `refs: []`
                                            AND `groups: []` (groups are
                                            reconstructed from ref rows; headers-
                                            only mode fetches none), counts
                                            (ref_count/group_count/…) stay intact.
                                            Any other numeric `refs=` value, or the
                                            param absent, returns full bodies;
                                            non-numeric `refs=`/`limit=` 400s.
                                            `?by_target=<path>` (CT-B3) flips to a
                                            reverse lookup instead — every doc whose
                                            extracted refs cite that EXACT path
                                            (`path_hint` match): "every doc citing
                                            config/importmap.rb" resolves to an
                                            artifact-id set in one request. Bypasses
                                            `cursor`/`limit` entirely (a complete
                                            resolution, not a page) and the response
                                            never carries `next_cursor`. `kb refs
                                            --by-target <path>` is the CLI verb;
                                            `--gallery` on either mode prints a ready
                                            `/?kb=&ids=` gallery deep-link over the
                                            resolved doc-id set (invariant #35).

# v0.13 Q4 — daemon-wide saved-queries store (DSL query persistence). CLI
# parity (W3 C-c): `kb queries list|save|rm`. A "scene" (the reflection
# canvas's named brush) is just a row here with path=/ — no separate store.
GET    /api/saved-queries                   list
POST   /api/saved-queries                   upsert (case-insensitive name)
DELETE /api/saved-queries/{name}            delete (case-insensitive, idempotent)

# U-track (v0.25) — quick capture: real files, staged-by-convention, into a
# per-kb capture/ folder, provenance-stamped at write time (kb-category=
# capture; kb-tags source:upload, from:<cli|spa|share>). The watcher indexes
# a capture like any other artifact — nothing new downstream (gallery,
# search, versions, share); `?category=capture` finds them in the gallery.
POST   /api/kb/{kb}/capture                 multipart: files[] (0..n) +
                                            title?, tags? (CSV), sanitize?
                                            (bool, default OFF), from?
                                            (defaults from the X-Requested-By
                                            header, kb-cli/kb-spa → cli/spa),
                                            url?, text?. Files present → each
                                            captured, gated on the kb's
                                            resolved extension→pipeline map
                                            (415 on an unmapped extension);
                                            no files but url/text → a small
                                            .md stub (kind:url-stub).
                                            Returns 201 {items:[{kb, id,
                                            source_relative, title, url?}]}.
                                            400 empty (no files/url/text);
                                            413 over the configured per-file
                                            cap ([server.capture]
                                            max_file_bytes, default 10 MiB)
                                            OR over the combined-request cap
                                            ([server.capture]
                                            max_request_bytes, default 64
                                            MiB — a multi-file batch's total
                                            size, checked up front against
                                            Content-Length); 409 on a
                                            read-only corpus.
POST   /capture                             the Web Share Target action
                                            (`manifest.webmanifest`'s
                                            `share_target` — Android's share
                                            sheet POSTs here directly, no
                                            `/api` prefix). Same handler core
                                            as above; destination kb =
                                            `[server.capture].default_kb`
                                            else the first configured kb.
                                            Shared `.html` FILES default
                                            sanitize=ON here (untrusted saved
                                            pages) — everywhere else it's
                                            opt-in. Success → `303 See Other`
                                            to `/?captured=<kb>:
                                            <source_relative>` (percent-
                                            encoded) rather than the artifact
                                            detail, since indexing is async
                                            (the SPA shows a toast; no
                                            manual SSE event — the watcher's
                                            `artifact.indexed` is the
                                            authoritative confirm). **Lives
                                            OUTSIDE the /api nest** but
                                            carries the SAME bearer-auth +
                                            loopback bypass via an explicit
                                            `route_layer` — invariant #4's
                                            fail-closed guarantee still
                                            applies on a public bind.
POST   /api/kb/{kb}/desk                    ephemeral LLM↔human handoff.
                                            Multipart: name (required stable
                                            slug) + one file (or `text`) +
                                            title?, tags?, session?,
                                            ttl_secs? (u64; 400 if 0 or
                                            > 10 years), sanitize?. Writes
                                            `handoff/<slug>.<ext>` (overwrite
                                            keeps the path-derived id).
                                            Stamps kb-category: handoff,
                                            tag `draft`, optional kb-session
                                            / kb-expires-at (display-only).
                                            Returns {kb, id, source_relative,
                                            title, url?, created} (201 create
                                            / 200 overwrite). 415 unmapped
                                            ext; 409 read-only corpus. Inside
                                            `/api` (inherits auth_bearer).
GET    /api/desk[?kb=]                      federated handoff aggregate: every
                                            indexed `handoff/` doc across kbs
                                            (#28 fan-out; `?kb=` restricts,
                                            404 unknown). {items, attention} —
                                            items carry kb/id/source_relative/
                                            title/updated_unix/comments_open/
                                            comments_total/read_state (per-user
                                            rollup)/last_opened_unix?/
                                            changed_since_read + expires_at?/
                                            session_id?; attention = never-
                                            opened + changed-since-read count.
                                            Backs the SPA desk pill + the
                                            changed-since-read reader banner.

# admin
GET    /api/kb/{kb}/quarantine              parser-quarantined artifacts
POST   /api/kb/{kb}/compact                 force a lance compaction (202 + run)
POST   /api/kb/{kb}/history/purge           wipe the history table
DELETE /api/kb/{kb}                         drop a kb's transient data (lance +
                                            sqlite history/errors/edges; keeps
                                            shares + .review user state)
POST   /api/shutdown                        graceful daemon shutdown (loopback)

GET  /                                      SPA shell
GET  /a/{kb}/{id}                           SPA permalink (no-cache)
GET  /assets/*                              hashed SPA assets (immutable cache)

GET  /                                      with Host=<id>.artifacts.localhost
                                            (or the v2-qualified
                                            <kb_enc>--<id>.artifacts.localhost,
                                            disambiguating a same-id artifact
                                            across kbs — invariant #7) sandbox-
                                            isolated artifact serve
GET  /  (with ?cm=on)                       same, plus injected window.__KB_COMMENTS
                                            + /_kb/annotate.js (v0.2 annotator)
GET  /_kb/annotate.js                       served on artifact subdomains
                                            from web/dist/annotate.js
GET  /_kb/probe.js                          served on artifact subdomains
                                            (v0.0.1 probe; iframe smoke uses it)
GET  /_kb/runtime.js                        scroll capture/resume runtime
                                            injected into iframed artifacts
                                            (v0.6+ H3; drives history scroll)
```

The Origin allowlist accepts `localhost:4000` (back-compat), any `*.artifacts.localhost`, the daemon's own `Host:` (same-origin SPA → daemon), and any `localhost`/`127.0.0.1` when `KB_DEV_ORIGIN_ANY=1`. Errors are RFC 7807 `application/problem+json`.

## Instrumentation events

Two event kinds on `/api/events` carry the daemon's own telemetry rather than
corpus state. The reader's Settings → Traffic page, its degraded chip, and
`kb metrics` are all consumers.

- **`metrics.tick`** — emitted at 1 Hz from a dedicated task in the daemon.
  Fields: `requests_total` (cumulative HTTP requests since boot, counted by
  the `count_requests` middleware on the `/api/*` tree), `requests_last_sec`
  (delta since the previous tick), `storage_channel_depth` (max-over-kbs of
  the storage actor's `mpsc::Sender` pending slots),
  `storage_channel_capacity` (the cap — `kb_core::storage::actor::
  CHANNEL_CAPACITY` = 1024 — so a consumer can render a ratio),
  `embedder_degraded` (true when an embedder subprocess is unrecoverable and
  semantic search has fallen back to keyword-only) and
  `embedder_respawn_count` (cumulative subprocess respawns; a climbing value
  flags a crash-looping embedder). The last two come from a proactive 1 Hz
  liveness probe that `try_wait`s each embedder and respawns an idle-dead one
  *before* the next search rather than on the user's request.
- **`index.embedding`** — carries an `index.embedding.bytes` field with the
  body byte count about to be embedded; consumers sum it per daemon.

## Comment anchor tunables

The reindex pass re-resolves every open comment's anchor
(`kb_core::review::fuzzy_resolve_anchor`) and emits `comment.anchor_stale` /
`comment.anchor_resolved` on a transition. Two environment variables tune it:
`KB_COMMENT_FUZZY_THRESHOLD` (default `0.85`, the Selection Jaro-Winkler
floor) and `KB_COMMENT_CONTEXT_CHARS` (default `200`, the snippet length an
anchor stores). Detection never rewrites an anchor — the frozen original is
re-evaluated each pass; the only rewrite is the explicit `PATCH
…/comments/{cid}/anchor`. Full workflow:
[`comment-workflow.md`](comment-workflow.md).

## What `POST /api/kb/{kb}/atlas/recompute` computes

`kb_core::atlas::compute_layout` tries UMAP first — brute-force KNN with
k = 15, random init, and 200 SGD iterations of attractive/repulsive forces
(closer to LargeVis than canonical UMAP, but visibly better cluster separation
than PCA on noisy embeddings). It falls back to PCA on degenerate output (any
NaN, fewer than three distinct coordinates, or n < 3); the PCA path is power
iteration with Gram-Schmidt deflation for the top two components, 100
iterations. Cluster labels come from k-means (Lloyd's, 50 iterations, at most
12 clusters), with empty clusters reseeded to random unassigned points.
Determinism is a contract: the same inputs and seed produce bit-identical
output across machines. Per-kb `k` and `layout` overrides live in
[`configuration.md`](configuration.md); the reader's view of the result is in
[`web-ui.md`](web-ui.md).

