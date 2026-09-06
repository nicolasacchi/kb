# kb CLI reference

The complete `kb` verb surface, one block per family. The CLI is the only
supported write path into a running daemon: every plugin hook, skill and
agent workflow shells out to these verbs rather than touching daemon state
directly. `kb tools` prints the same manifest from the binary you have
installed, so it never trails the build; this page is the readable copy.

Verbs that talk to a daemon take `--daemon URL` and read the bearer token
from `~/.config/kb/token`; see [self-host.md](self-host.md) for the token
and [configuration.md](configuration.md) for `kb.toml`.

Related: [HTTP API](http-api.md) · [kb-code](kb-code.md) ·
[project overview](../README.md).

```
kb daemon [--config kb.toml]        run an axum daemon (foreground)
kb daemon stop                      SIGTERM a running daemon via its pid
                                    file; waits up to 10s for clean exit
kb daemon doctor [--endpoint URL]   green/yellow/red health report
   [--json] [--watch SECS]          (identity, kbs, errors, stats,
                                    embedder/semantic probe, bus)
kb daemon log-level [FILTER]        read (no arg) or set the daemon's FILE
   [--endpoint URL] [--json]        log filter (the ndjson layer) live —
                                    no restart, stderr/RUST_LOG untouched.
                                    FILTER = EnvFilter directives, e.g.
                                    debug or info,kb_core=debug
kb doctor --hooks [--repo PATH]     v0.38 CT-C6: the provenance-chain
   [--daemon URL] [--json] [--fix]  integrity check, distinct from `kb
                                    daemon doctor` above — read-only,
                                    PASS/WARN/SKIP + a one-line fix per
                                    link: session marker files, the git
                                    Kb-Session trailer hook, the
                                    memory_recalls ledger, the provenance
                                    lint (orphaned origin sessions +
                                    dangling kb-source-artifact highlight
                                    origins, sampled — an unreadable
                                    census or an unserved source kb SKIPs,
                                    never a false WARN), and kb-code's
                                    why-hook. --repo defaults to cwd.
                                    v1 checks Claude Code only (codex/
                                    kimi adapters unchecked). D30 (v0.42)
                                    adds one more, unrelated to provenance:
                                    `~/.cache/kb/slate-cursor-*` and
                                    `slate-topic-*` markers older than 30
                                    days are flagged, and `--fix` removes
                                    THOSE ONLY — every other check here
                                    stays read-only.
kb slo status [--kb NAME] [--json]  v0.38 CT-F5: corpus-health SLOs — four
   [--daemon URL]                   indicators over EXISTING tables (code-ref
                                    path shape, orphan kb_session docs,
                                    recall-ledger parse-failure rate, capture
                                    freshness) against optional
                                    [kb.<name>.slo] targets. SURFACED, NEVER
                                    ENFORCED: nothing changes behaviour on a
                                    miss, and this EXITS 0 even on a warn —
                                    deliberately not a check command. An
                                    unmeasurable indicator prints — (never 0)
                                    and reads `unknown`; so does a measured
                                    one with no target.
kb slo snapshot [--kb NAME]         append one reading to the per-kb
   [--json] [--daemon URL]          append-only slo_snapshots log. Every run
                                    lands (no skip-if-unchanged — a flat line
                                    is itself the signal); the daemon never
                                    snapshots on its own.
kb slo log [--kb NAME] [--limit N]  read that log back, newest first
   [--json] [--daemon URL]          (server clamps N to 1..=1000)
kb add <path>                       add a source folder to the active kb
kb status [--json]                  show daemon health + kb summaries
kb metrics [--daemon URL] [--json]  print the daemon's request + pipeline
                                    timing tables (GET /api/metrics). Detailed
                                    tables need [server] metrics = true.
kb queries --zero-hit [--kb NAME] [--scope one|all] [--min-count N] [--daemon URL] [--json]
                                    GC-B3: zero-hit search queries as a
                                    corpus-gap signal — the daemon's per-kb
                                    queries ring grouped by normalized text,
                                    count + last-seen per group. --scope one
                                    (default) requires --kb and hits
                                    /api/kb/{kb}/queries?zero_hit=true;
                                    --scope all fans out server-side via
                                    GET /api/queries/zero-hit. --zero-hit is
                                    required (the only report today);
                                    --min-count drops groups seen fewer
                                    than N times (default 1).
kb queries list [--daemon URL] [--json]
                                    W3 C-c: CLI parity for the daemon-wide
                                    saved-query store — GET
                                    /api/saved-queries. A distinct surface
                                    from the zero-hit report above (a
                                    different in-memory ring); this is the
                                    same store the SPA's saved-query ribbon
                                    and the reflection canvas's "save as
                                    scene" chip both write.
kb queries save <name> [--path P] [--search S] [--daemon URL] [--json]
                                    Upsert (overwrite by case-insensitive
                                    name) — POST /api/saved-queries. --path
                                    defaults to / (the gallery route);
                                    --search defaults to empty (no filter).
kb queries rm <name> [--daemon URL] [--json]
                                    Delete by case-insensitive name —
                                    DELETE /api/saved-queries/{name}.
                                    Idempotent: succeeds even if the name
                                    isn't there.
kb search <q> [--mode hybrid|keyword|semantic] [--kb NAME]
   [--limit N] [--category C] [--offline] [--daemon URL] [--json]
                                    hybrid (default) = BM25 + vector (RRF
                                    k=60); --offline reads lance directly
                                    (keyword-only); --json for parseable
                                    stdout. --category filters to an exact
                                    kb-category (R0 escape hatch) — e.g.
                                    --category memory-session surfaces
                                    captured session transcripts, excluded
                                    from search by default; --offline
                                    applies it client-side (over-fetches,
                                    since lance carries no R0 filter)
kb get <id> --kb NAME [--format json|md|html] [--daemon URL]
                                    v0.4 D2: single-artifact lookup;
                                    --format html via the new
                                    /api/kb/{kb}/artifact/{id} route
kb download [<id-or-path>] [--folder DIR | --all] [--kb NAME] [-o FILE] [--daemon URL]
                                    download an artifact's raw source, or a
                                    folder / whole kb as a .zip. Streams to
                                    stdout (pipe-friendly:
                                    `kb download --folder x | tar`,
                                    `… > out.zip`) unless -o FILE; refuses to
                                    write a .zip to a terminal. Single via
                                    /artifact/{id}; folder via
                                    /api/kb/{kb}/download?folder=DIR
kb share [<target>] [--host cloudflare-pages|github-pages] [--gate RULE]...
         [--public] [--links warn|absolute] [--update] [--no-scrub] [--open]
         [--local PATH] [--list ID_OR_TITLE] [--kb NAME] [--json] [--daemon URL]
kb share list [--kb NAME] [--json] [--daemon URL]
kb share revoke <name> [--kb NAME] [--json] [--daemon URL]
                                    publish a source-relative file/folder to a
                                    gated (Cloudflare Pages + Access) or public
                                    (GitHub Pages, --public) static URL; list +
                                    revoke manage them. --gate is repeatable
                                    (email:DOMAIN | email:a@x,b@y | google |
                                    github). The engine runs daemon-side; the
                                    host API tokens come from the daemon's env
                                    (not the CLI). In-share cross-artifact links
                                    are rewritten to relative paths so the export
                                    is self-contained; --links governs only
                                    out-of-share danglers. --local PATH writes an
                                    OFFLINE bundle instead of publishing (PATH.zip
                                    → a zip; any other PATH → a directory it
                                    extracts into). --page PATH writes a single
                                    UNCOMPRESSED artifact in its native format
                                    (scrubbed standalone .html, or raw .md source)
                                    to PATH — one file, no asset closure/zip
                                    (mutually exclusive with --local). --list
                                    ID_OR_TITLE exports a whole READING LIST as
                                    the offline bundle (requires --local;
                                    mutually exclusive with a path target;
                                    publish flags ignored): ordered entries +
                                    a generated index.html TOC as the entry
                                    page; skipped tombstones + danglers are
                                    printed. POST/GET/
                                    DELETE /api/kb/{kb}/share[s], POST
                                    .../share/export[/page], POST
                                    .../lists/{id}/share/export.
kb mv <target> <new-path> [--kb NAME] [--json] [--daemon URL]
                                    move/rename an artifact — or a whole folder
                                    (trailing / forces folder mode) — WITHOUT
                                    losing comments/history/lists/edges/session
                                    joins: drives the daemon relocate engine
                                    (id changes with the path; old permalinks
                                    301 via the moves log). Target resolves like
                                    kb find (id, source-rel path, unique
                                    filename). POST .../docs/{id}/move,
                                    .../folders/rename.
kb related <id> --kb NAME [--depth N] [--json] [--daemon URL]
                                    v0.4 D2: cross-artifact link graph
                                    (uses v0.3 F2 graph endpoint)
kb versions <target> [--at DATE] [--kb NAME] [--json] [--daemon URL]
                                    Track V: an artifact's version timeline
                                    (git commits + kb index snapshots +
                                    working tree, per the kb's versions
                                    mode). GET .../artifacts/{id}/versions.
                                    CT-F6: --at resolves the timeline AT an
                                    instant (RFC 7089 Memento) — which
                                    version of THIS artifact stood then.
                                    Same date grammar as `kb diff --between`
                                    (YYYY-MM-DD = end of that day UTC; RFC
                                    3339 is exact). Prints what it resolved
                                    to, says whether the hit was exact or
                                    the nearest prior, and marks the row `→`.
                                    A date older than the oldest known
                                    version says so and names that floor —
                                    never a silent fall back to the oldest.
                                    Per-artifact only (not a corpus
                                    timeline, and not `recall --as-of`,
                                    which stays rejected).
kb diff <target> [--from REF] [--to REF] [--raw] [--kb NAME] [--json] [--daemon URL]
   | diff <target> --between <D1> <D2> [--raw] [--kb NAME] [--json] [--daemon URL]
                                    what changed between two versions.
                                    Defaults to "most recent prior version →
                                    working tree". MI-W2.4b: --between
                                    resolves BOTH sides from calendar dates
                                    (YYYY-MM-DD = end of that day, UTC) or
                                    RFC 3339 instants via a pure nearest-
                                    version-at-or-before resolver over the
                                    SAME timeline `kb versions` lists (zero
                                    server delta); echoes what each date
                                    resolved to; a date older than the
                                    oldest known version is a hard error
                                    naming it, never a silent empty diff.
                                    Conflicts with --from/--to. MI-W2.4c:
                                    prints an EPOCH HONESTY caveat when the
                                    window starts before this daemon's
                                    tombstone era (see `kb memory log`).
kb memory log <id> [--kb NAME] [--daemon URL] [--json]
                                    MI-W2.4a: walk one supersede chain, both
                                    directions (what {id} supersedes; what
                                    superseded it), with timestamps and
                                    forgotten-state. The endorsed salvage
                                    from the 2026-07 temporal-query design
                                    in place of a rejected `recall --as-of`
                                    (kb forget's pre-W2.3 hard delete made
                                    that answer undetectably incomplete).
                                    MI-W2.4c: prints the same EPOCH HONESTY
                                    caveat as `kb diff --between` when the
                                    chain reaches back before the tombstone
                                    era. GET .../memories/{id}/lineage.
kb memory dupes [--threshold F32] [--limit N] [--kb NAME] [--daemon URL] [--json]
                                    MI-W3.1: ON-DEMAND cross-corpus duplicate
                                    report (NOT a contradiction detector — see
                                    the route doc). Likely-redundant memory
                                    PAIRS, flagging cross-corpus ones
                                    distinctly. --threshold defaults to 0.90
                                    (documented reasoning inline). NEVER
                                    mutates — resolve with `kb remember
                                    --supersedes` or `kb forget`. GET
                                    /api/memory/dupes.
kb memory triage [--kb NAME] [--limit N] [--daemon URL] [--json]
                                    MI-W4.4: the bounded, DERIVED hygiene
                                    queue — the memories most worth 90
                                    seconds right now, each with a one-line
                                    justification ("salience 0.10 is
                                    at/below the 0.15 floor — excluded from
                                    recall now" / "salience 0.90 but never
                                    recalled" / "flagged duplicate of <id>"
                                    / "superseded by <id> — not yet
                                    forgotten" / "flagged: <reason excerpt>"
                                    — CT-C1, `kb memory flag`; an
                                    operator-action item that ranks ABOVE
                                    every heuristic reason, incl. duplicates).
                                    Decay never lowers the
                                    value the floor tests (raw salience is
                                    constant), so "below the floor" is
                                    always a CURRENT state, never a
                                    predicted future date. Pinned and forgotten
                                    memories never appear. NEVER mutates —
                                    act on an item with `pin`/`salience`/
                                    `remember --supersedes`/`forget`. GET
                                    /api/memory/triage.
kb why-memory <id> [--kb NAME] [--daemon URL] [--json]
                                    CT-B1: the fact → origin session →
                                    commits → files-changed-since chain as
                                    ONE verb. ZERO new server surface — pure
                                    composition of GET .../docs/{id},
                                    /api/memory/census (the origin
                                    session-id fallback), and GET
                                    /api/sessions/{sid}[/commits[/…/files]].
                                    The CLI's terminal twin of the SPA's
                                    ProvenanceThread (MI-W4.6). Degrades
                                    honestly at every hop: "no origin
                                    session recorded" when the memory
                                    carries no kb_session, "purged" when
                                    the session capture is no longer
                                    indexed, per-file "changed since:
                                    yes/no/unverifiable" — the footer is
                                    explicit that these labels mean "commit
                                    still in history / files changed since",
                                    never "fact still true".
                                    CT-F1 adds a "committed in (exact-id
                                    citations)" section above the chain, from
                                    GET .../memories/{id}/commits: commits
                                    whose OWN message named this memory's id
                                    (a `Kb-Memory:` trailer) — a strictly
                                    stronger claim than the session hop
                                    below it, and rendered even when the
                                    origin session is absent or purged. The
                                    section is SILENT when empty (unlike
                                    every other hop here, which prints an
                                    explicit absence): the trailer is opt-in
                                    per repo and off by default, so an empty
                                    list is a configuration fact, not a
                                    finding. `--json` still always carries
                                    `exact_commits` (possibly `[]`) plus
                                    `exact_commits_note`.
kb memory expand <id> [--kb NAME] [--daemon URL] [--json]
                                    CT-B4: from a highlight-born memory's
                                    one-liner back to the origin passage it
                                    was lifted from. Reads the origin
                                    (kb/artifact/anchor) off the memory's OWN
                                    `kb-source-*` meta tags (written at
                                    highlight time; also surfaced on
                                    census/recall since CT-A1), fetches the origin
                                    artifact's CURRENT source the same way
                                    `kb cat` does, and re-resolves the stored
                                    anchor through kb-comments/1's EXISTING
                                    ladder (`fuzzy_resolve_anchor` — never a
                                    second heuristic). Prints the surrounding
                                    passage on a resolve; an honest "anchor no
                                    longer resolves" line (never a guessed
                                    passage) plus the origin's title on a
                                    miss; "no origin recorded" for an
                                    ordinary (non-highlight-born) memory.
kb memory flag <id> --reason TEXT [--kb NAME] [--daemon URL] [--json]
                                    CT-C1: the missing in-session correction
                                    verb — an agent that discovers a
                                    recalled memory is WRONG posts an
                                    ordinary `[kb-flag] <reason>` comment
                                    (author claude) through the EXISTING
                                    kb-comments/1 `add` route (no new
                                    storage). --kb omitted searches every
                                    memory-scoped kb for the id. Refuses when
                                    an OPEN flag already exists on this
                                    memory. Surfaces on `kb recall` as
                                    `flagged: true` (display only, never
                                    scored) and on `kb memory triage` as the
                                    TOP-ranked `reason_kind: "flagged"` item.
                                    Resolve via the ordinary comment workflow
                                    (`kb comments resolve`/`reply`) — see
                                    docs/comment-workflow.md. POST
                                    .../review/{id}/comments.
kb model {list,download,set,rm}     manage embedding models
kb model set <name> --kb NAME [--in-place]
                                    v0.3: --in-place clears the embedding
                                    column for same-dim swaps
kb comments {list,inbox,show,export,resolve,unresolve,add,reply,edit,reanchor,delete,watch,apply,import}
                                    every verb takes <kb> <id> positionally
                                    OR --path <file> (resolved via /lookup);
                                    all talk to the daemon over HTTP (R6: no
                                    .review/ disk reads). see docs/comment-workflow.md.
                                    list [--all] [--json] [--author] [--stale]
                                    [--folder]: rows via GET /reviews
                                    inbox [--kb NAME] [--limit N] [--json]:
                                    fleet-wide OPEN comments across every kb,
                                    newest activity first, via GET /api/inbox (Z4)
                                    show <kb> <id>: full thread (replies+choices)
                                    export <kb> <id> [--format claude|json|md]
                                    [--embed -o FILE]: v0.19 --embed bakes the
                                    review state into a standalone HTML copy
                                    import <file.html> [--path|--artifact-id]
                                    [--force]: v0.19 read an embedded copy back
                                    into the sidecar (ids/replies preserved)
                                    apply [--path] (--ops-file F | --ops-json S):
                                    v0.19 atomic batch of comment ops in one call
                                    resolve|unresolve <kb> <id> <comment> | --all
                                    add <kb> <id> --body --anchor [--author]
                                    [--page] [--choice-json]: Claude-Code write path
                                    reply <comment> --path F --body
                                    [--choice-json]: append a Claude reply
                                    (renders live in the SPA)
                                    edit <comment> [--reply R] --body: amend body
                                    reanchor <comment> --anchor SPEC: re-point a
                                    comment's anchor after its target moved (R9)
                                    delete <comment> [--reply R] --yes: remove
                                    watch --path <artifact|folder> [--json]
                                    [--once] [--timeout S] [--backlog]:
                                    SSE monitor for new `you` comments,
                                    scoped client-side; drives a /loop
kb atlas recompute --kb NAME        v0.3: PCA + k-means atlas; tails SSE
                                    for atlas.recompute.complete
kb atlas recluster --kb NAME [--k N] Q5: fast path that re-runs only
                                    k-means on existing coords (no UMAP).
                                    Use to retune cluster count without
                                    paying the O(n²) KNN cost.
kb atlas history|show|prune|backfill W3: the corpus time-lapse. history
   --kb NAME [--frames N]           lists frames (newest first, each
                                    labelled recorded|reconstructed); show
                                    <id> prints one frame Procrustes-aligned
                                    against another; prune --keep N tightens
                                    retention; backfill reconstructs up to
                                    N (<=12) back-dated frames from TODAY's
                                    embeddings over each mtime cut point's
                                    docs — printing the plan first, and
                                    saying "reconstructed", because that is
                                    not recorded history.
kb token {generate,rotate,show,path}
                                    v0.4 A3: bearer-token lifecycle for
                                    self-host. Writes ~/.config/kb/token
                                    mode 0600.
kb token issue <user> [--force] / kb token revoke <user>
                                    v0.34: per-user API tokens. `issue`
                                    appends <user>:sha256:<hex> to
                                    ~/.config/kb/tokens (0600) and prints
                                    the plaintext ONCE; `revoke` removes
                                    the user's lines. Daemon restart to
                                    load. Attribution only — every token
                                    carries the same full authority.
kb whoami [--output json]           v0.34: the caller's resolved identity
                                    + how it resolved (header|token|
                                    legacy|loopback).
kb users [--output json]            v0.34: configured ∪ observed roster.
kb history [--user NAME] [--kb KB]  v0.34: recent activity, optionally
                                    filtered to one user's rows.
kb push [--filter EVENTKIND] [--daemon URL]
                                    v0.4 C2: tail /api/events as Claude-
                                    prompt-formatted blocks. Reconnects
                                    with Last-Event-ID + exponential
                                    backoff. Pipe to `claude code -- ...`.
kb events --follow [--types GLOB]…  v0.24 T1: operator tail of /api/events,
   [--kb NAME] [--artifact ID]      one line per event (NDJSON with --json).
   [--json] [--daemon URL]          Same reconnect loop as kb push; --types/
                                    --kb/--artifact filter SERVER-SIDE.
kb tools                            v0.4 D4: emit Claude-prompt-friendly
                                    markdown manifest of every CLI verb.
                                    Drop into a system prompt to teach
                                    Claude how to drive kb.
kb status [--json] [--watch SECS]   sqlite-backed observability snapshot
                                    Q7: --watch loops with ANSI clear
                                    (mutually exclusive with --json).
kb fleet status [--json]            v0.24 T1: health sweep across every
                                    daemons.toml entry — identity (version/
                                    build/uptime) + per-kb docs and open
                                    errors. Unreachable daemons are report
                                    rows, not failures.
kb fleet replicate --kb NAME        Q4: cross-daemon coverage report.
   [--src NAME] [--copy-to DIR]     Reads daemons.toml, diffs each
                                    daemon's artifact ids. --copy-to
                                    downloads missing-anywhere artifacts
                                    into a local dir (rsync-friendly).

— Artifact access & authoring —
kb find <input> [--kb NAME] [--json] resolve a 12-hex id, source-relative
                                    path, or unique filename to an id via
                                    /lookup. Exit 1 (ambiguous) / 2 (none).
                                    Composes: `kb find atlas.html | xargs kb cat`
kb get <id> --kb NAME [--format json|md|html] [--daemon URL]
                                    v0.4 D2: single-artifact lookup;
                                    --format html via /api/kb/{kb}/artifact/{id}
kb cat <id> [--kb NAME]             dump artifact HTML to stdout
kb read <id> [--kb NAME]            open the artifact in $BROWSER
kb related <id> --kb NAME [--depth N] [--json] [--daemon URL]
                                    v0.4 D2: cross-artifact link graph
                                    (uses v0.3 F2 graph endpoint)
kb new --template <path|name> --title T [--out PATH] [--var k=v ...] [--kb NAME]
                                    scaffold an HTML artifact from a
                                    template; substitutes {{title}} {{date}}
                                    {{slug}} {{key}}. Name resolves against
                                    [kb.<name>.templates] in kb.toml
kb index-page --kb NAME [--filter k=v] [--group-by FIELD] [--out PATH]
   [--template PATH] [--limit N] [--title T] [--daemon URL]
                                    generate a self-contained HTML index of
                                    a kb (replaces hand-maintained INDEX.md);
                                    group by kb-status|kb-category|kb-severity

— Index maintenance & ops —
kb reindex [--kb NAME] [--daemon URL] [--json]
                                    force a re-walk + re-emit watch.modify
                                    for every HTML file (bypasses hash dedup)
kb exclude <target> [--kb NAME] [--rm] [--list] [--note TEXT] [--json]
                                    v0.24 X3: per-file index exclusion.
                                    Excluded = dropped from the index but
                                    comments + reading history survive;
                                    --rm re-includes (immediate reindex,
                                    comments re-anchor); --list shows the
                                    table. Path-shaped unindexed targets
                                    are excluded verbatim (pre-emptive)
kb pause [--kb NAME] [--json]       stop a source's ingest until `kb resume`
kb resume [--kb NAME] [--json]      (v0.24 D6: paused is now enforced at
                                    the ingest gate — stale by design)
kb backup <kb> [--out PATH]         consistent tar.gz snapshot of a kb's state
                                    (sqlite via VACUUM INTO + validated lance +
                                    .review); → <state>/exports/<kb>-<ts>.tar.gz
kb restore <tarball> --kb NAME [--force]
                                    extract a backup into the kb's state dir;
                                    stop the daemon first; --force replaces a
                                    non-empty state (wipes it first)
kb reset --kb NAME [--yes] [--all] [--force]
                                    wipe a kb's index state (Lance + SQLite);
                                    keeps .review/ unless --all; refuses while
                                    the daemon is up unless --force

— Testing & evaluation —
kb synth --out DIR [--docs N] [--seed S]
                                    generate a deterministic synthetic corpus
                                    for stress-testing (same seed = same bytes)
kb bench {init,discover,run}        retrieval-quality bake-off: scaffold a
                                    queries.jsonl, discover relevant ids, then
                                    compute Recall@k / MRR / nDCG per (kb × mode)
kb sessions capture                    Build a session capture artifact from a transcript file — the Rust engine `kb-capture.sh` shells out to (W0.4). Resolves each detected git commit sha before writing. Filesystem-only: never talks to the daemon
kb sessions list [--limit N] [--json] List captured sessions newest-first. Filters: --project, --substance (trivial|routine|substantive), --harness (claude|codex|opencode|grok|kimi), --folder
kb sessions folders                    List the folders (working directories) sessions ran in, with counts
kb sessions rollup                     R9 — top research queries per project folder (or overall)
kb sessions funnel                     R9 — the activity funnel: searched → opened → edited → committed → commented
kb sessions ledger [--project] [--days]
                                    W6 — one project's sessions/commits/decisions/research grouped by UTC day (default 7, max 31)
kb sessions search <q> [--limit N] [--json]
                                    Search sessions by keyword (title / first prompt / folder)
kb sessions of <id|path> [--kb NAME]   Which session(s) touched this artifact, and how
kb sessions commit-map [--offset] [--limit]
                                    Flat bulk commit↔session feed, offset-paginated
kb sessions by-job <ulid>              W4/R8 — the grokclaude job join: every Claude Code session whose transcript invoked this job. Scopes: driver (invoked) or child (job's own capture)
kb sessions by-commit <sha> [--json]   W0.6 — the sha→session reverse lookup (>= 7 hex chars)
kb sessions read <id> [--json] [--live] [--follow]
                                    [--full|--tail N] [--turn A..B] [--grep PAT]
                                    Interpreted terminal transcript reader (header + turns + footer). --live/--follow poll the daemon (W7)
kb sessions status [--local] [--json] [--no-color]
                                    [--state S,…] [--project P] [--harness H]
                                    [--limit N] [--root DIR] [--daemon URL]
                                    LSC-1/2/5 — live-sessions cockpit snapshot: who holds the ball,
                                    right now, grouped into IN PROGRESS / WAITING ON YOU /
                                    FINISHED·COLD. Default = daemon-backed (`GET
                                    /api/sessions/live-status`); `--local` is the direct-disk twin,
                                    zero daemon, fanned out across every harness
                                    (claude/codex/opencode/grok/kimi — `--root` overrides only the
                                    Claude Code root). `--state` is csv over
                                    working/stalled/waiting/cold/finished/presumed_ended. No
                                    `--watch`: pipe `kb events --follow --types session.state --json`
                                    for a live stream instead. See
                                    docs/live-sessions.md.
kb sessions show <id> [--json]         Single session detail + memories + artifacts + decisions + effort
kb sessions replay <id> [--json]       Replay beat by beat on the transcript's clock: prompts, reads/edits/writes, searches, commits, decisions
kb sessions resume <id> [--json]       Resume-context block (goal + branch + edited files + decisions)
kb sessions export <id> [--from URL]   Bundle a session as a portable `<sid>.kbsession.zip`
kb sessions rehydrate <bundle>         Place a bundle's transcript under `~/.claude/projects/` so `claude -r <id>` finds it
kb sessions pull <id> [--from URL]     Fetch a session bundle from a remote kb daemon, optionally rehydrate
kb sessions provenance-report --repo PATH
                                    W0.6 — wedge instrument: classify every commit (trailer / recorded / pre-capture / non-session / orphan)
kb sessions why-line <file>:<line> --repo PATH
                                    W0.6 — thin probe: git blame → Kb-Session trailer → by-commit lookup
kb sessions threads                    Narrative threads — sessions clustered by folder + time
kb sessions save-thread <project> [--narrative]
                                    Save a folder's most-recent thread as an editable reading list.
                                    --narrative (CT-E5) orders it as each session's STORY — capture,
                                    artifacts touched (edits before reads), memories produced, memories
                                    recalled — oldest session first, and writes the session ids, capture
                                    dates and (with `[kb.*] code_url`) kb-code session-diff links into
                                    the list description
kb sessions watch --path <artifact|folder>
                                    SSE monitor for new session comments
kb import claude-history [--dir PATH] [--into DIR] [--dry-run]
   [--limit N] [--json] [--quiet]
                                    Z5: retroactive backfill — wrap every
                                    historical Claude Code transcript under
                                    --dir (default ~/.claude/projects) in the
                                    live capture hook's envelope and drop it in
                                    --into (default $KB_SESSIONS_DIR; should be a
                                    sessions corpus's source dir) so the daemon
                                    indexes months of past sessions as episodic
                                    memory. Canonical id is recovered from each
                                    transcript's own JSONL sessionId; deduped +
                                    idempotent (re-run imports 0). Filesystem-only
                                    (the watcher indexes); --dry-run reports plan.
kb remember <text> [--title T] [--summary S]
   [--kb NAME | --scope global|project] [--category C]
   [--tags T,T] [--salience 0..1] [--decay slow|fast]
   [--supersedes ID] [--type episodic|semantic|procedural]
   [--source fetched-web|user-dictated|agent-inference]
   [--failed] [--session-id ID] [--no-session]
   [--global | --link KB,KB] [--daemon URL] [--json]
                                    v0.9 M5: store an agent-explicit memory —
                                    renders an HTML artifact and POSTs it to
                                    a memory corpus. --global (default)
                                    makes it recallable from every kb;
                                    --link KB,KB scopes it explicitly
                                    instead. --session-id auto-stamps from
                                    ~/.cache/kb/current-session unless
                                    --no-session is passed. MI-W3.3a: --type
                                    is an OPTIONAL CoALA-minimal
                                    classification (absent = untyped, never
                                    inferred/backfilled). MI-W3.4: --source
                                    records where the CONTENT came from —
                                    SURFACED, NEVER a recall score term.
                                    Threat model: a memory written from
                                    untrusted fetched content
                                    (--source fetched-web) persists
                                    indefinitely and, once global, is
                                    recallable from every project on this
                                    daemon by default; passing --source
                                    fetched-web WITHOUT an explicit --global
                                    flips the default to non-global (a
                                    one-line stderr notice marks it whenever
                                    the flip fires) — narrowing the blast
                                    radius without adding a new trust tier.
                                    --global always wins when passed
                                    explicitly. CT-C3: --failed records an
                                    approach that was tried and did NOT
                                    work — it will surface on recall with
                                    an explicit warning ("✗ didn't work:"
                                    in the recall hook). Writes the
                                    kb-outcome: failed meta (the durable
                                    declaration; absent = ok, never an
                                    "outcome: ok" noise meta) paired with
                                    the outcome:failed tag (the indexed
                                    carrier, stored/filterable as
                                    ?tags=outcome-failed). SURFACED, NEVER
                                    SCORED — a failed memory ranks exactly
                                    like an ordinary one.
kb recall <query> [--scope auto|all|global|project] [--project NAME]
   [--cwd PATH] [--limit N] [--for-kb NAME] [--no-floor] [--explain]
   [--daemon URL] [--json]
                                    v0.9 M5: recall memories relevant to a
                                    query, ranked across in-scope memory
                                    corpora by recency × salience × decay.
                                    --no-floor bypasses the salience/decay
                                    floor (a dedup oracle for /kb-reflect).
                                    --explain prints the per-hit arithmetic
                                    (rel × salience × decay[, MI-W2.1/2.2's
                                    × relevance when scoring_v2_relevance is
                                    on (default) and/or × stability when
                                    scoring_v2_stability is on (off by
                                    default)] → score). MS: the DEFAULT
                                    scope is now `auto` (was `all`) — it
                                    resolves to `scope=all` PLUS
                                    `project=memory-<slug>` and
                                    `visible_to=<slug>,memory-<slug>`,
                                    where `slug` is the git MAIN-checkout
                                    root's basename (worktree-safe via
                                    `--git-common-dir`, so a linked
                                    worktree still resolves to the same
                                    project as its main tree); outside a
                                    repo it falls back to today's
                                    fleet-wide behaviour (plain
                                    `scope=all`, byte-identical to pre-MS).
                                    --cwd overrides the directory the slug
                                    is derived from (default: the CLI's own
                                    process cwd) — a caller like the
                                    kb-recall hook, whose process cwd may
                                    not be the project directory, passes
                                    the real one through explicitly.
                                    Explicit --scope all|global|project is
                                    UNCHANGED and always wins over auto's
                                    derivation.
kb context <query> [--cwd PATH] [--budget N] [--session SID] [--no-floor]
   [--daemon URL] [--json]
                                    CT-D1 (v0.38): the ONE context pack for a
                                    task — recalled memories WITH their score
                                    decomposition, prior-session POINTERS
                                    (never transcripts: #11 R0/R1/R3), open
                                    comments on artifacts the query matched,
                                    and the kb-local code paths those
                                    artifacts cite. One `GET /api/context`
                                    call; the daemon composed it. Hard char
                                    --budget whose every truncation is
                                    reported ("…N more, not shown"), never
                                    silent. --cwd floats prior sessions from
                                    that directory (marked `here`), it does
                                    not filter. MS: --cwd (or the process
                                    cwd when omitted) ALSO derives the same
                                    repo slug `kb recall --scope auto`
                                    uses, sent as `memory_project=memory-
                                    <slug>` + `memory_visible_to=<slug>,
                                    memory-<slug>` so a daemon that
                                    understands them narrows the pack's
                                    `memories` lane the same way — an
                                    older daemon's serde `Query` ignores
                                    the unrecognised params, so this
                                    degrades gracefully. --session is YOUR
                                    id, so the pack skips your own
                                    in-flight session.
                                    --no-floor = recall's dedup-oracle flag
                                    (/kb-distill Step 5 arm 1). This is what
                                    kb-recall.sh's turn-1 scent line ("3 prior
                                    sessions · 2 open comments — run
                                    `kb context`") points at: the hook injects
                                    COUNTS, this verb is the substance.
kb slate open [--topic T] [--budget N] [--all|--hybrid] [--json]
   [--slate SLUG] [--cwd PATH] [--session-id SID] [--daemon URL]
                                    SL3 (v0.41): this project's shared
                                    WORKING state — who is on what, open
                                    questions, hypotheses, dead ends. NOT
                                    memory: mutable through later posts,
                                    per-project (the slug is the git MAIN
                                    checkout's basename, so linked
                                    worktrees share one slate), and read by
                                    sessions of every harness. `open` is
                                    the digest — read it first and again
                                    after /compact. Budgets are CHARACTERS
                                    (6000 default, 2000 `--hybrid`), never
                                    tokens. Prints the daemon's rendered
                                    text verbatim.
kb slate show #n | delta [--since SEQ] [--limit N] [--kinds a,b]
   | history [--since SEQ] | cursor [--seq N]
                                    unfold one post + its thread · what
                                    changed since your cursor (the
                                    per-prompt hook lane; the cursor lives
                                    at ~/.cache/kb/slate-cursor-<sid> and
                                    only `open`/`delta` write it — `--kinds`,
                                    D26, is a csv over the twelve kind words
                                    forwarded verbatim as `?kinds=`, an
                                    unknown word is the server's 400
                                    `bad-kind`) · what was dropped or
                                    edited away, by whom · `cursor` is D27's
                                    explicit report for adapters that don't
                                    route through `open`/`delta`, defaulting
                                    `--seq` to the local cursor marker's
                                    value. Every SUCCESSFUL `open`/`delta`
                                    also fires this report itself,
                                    fire-and-forget (1s timeout, silent on
                                    any failure, never touching the exit
                                    code or the printed output, skipped with
                                    no session id resolved) — `cursor` is
                                    for callers that need to report on
                                    their own schedule instead.
kb slate now "…" | warn "…" | ask "…?" | answer #n "…"
kb slate found "…" --ref R | idea "…" | tried "…" --failed "…" [--was #n]
kb slate take <subject|#n> "…" [--anyway|--over #n]
kb slate done #n "…" [--abandoned "…"] | hand <subject> "…" [--to H]
kb slate drop #n "…" [--anyway] | edit #n "…" [--body -|MD] [--anyway]
kb slate mark #n [--pin|--unpin] | pin #n | unpin #n
                                    The twelve kinds. A refused `take`
                                    means a live session holds an
                                    overlapping subject: read their line,
                                    ask, or `--anyway` to contest. Every
                                    mutating verb prints what its post
                                    pushed off the finite board
                                    ("pushed off the board: #41 idea …")
                                    plus a nudge once a session is past
                                    eight undropped found/idea posts.
                                    Exit codes: 0 ok · 1 error · 2 not
                                    found · 3 refused (`slate-taken` or
                                    `slate-live-author`), so a loop caller
                                    branches without parsing stderr.
kb slate promote #n --to memory|note|plan [--plan PATH] [--kb K] [--note N]
kb slate close ["…"] | reopen | rotate
kb slate watch [--once] [--timeout S] | stats | ls | doctor
                                    `promote` composes the existing write
                                    (`kb remember` / `kb notes append` / a
                                    dated plan-file line) and then posts
                                    `done #n "promoted → …"`. `watch`
                                    follows `slate.updated` over
                                    `?filter=slug:<slug>`, skipping your
                                    own session's posts. `stats` counts the
                                    lifecycle (hands acknowledged, takes
                                    done/expired/contested, asks answered,
                                    tried echoes, posts per harness and
                                    session) — counts, never a verdict.
                                    `doctor` is a structural lint. See
                                    `## kb slate` below.
kb forget <id> [--kb NAME] [--purge] [--daemon URL]
                                    v0.9 M5, MI-W2.3: forget a memory by id.
                                    Default SOFT-forgets (tombstones —
                                    kb-status=forgotten in the source; still
                                    on disk, still `kb search`-able, still
                                    listed by census flagged, dropped from
                                    recall). --purge HARD-deletes instead
                                    (the pre-W2.3 behavior, irreversible).
kb why <path> [--daemon URL] [--json]
                                    R2: why is a file the way it is? Pulls
                                    the past sessions that touched it
                                    (episodic memory) and inlines the
                                    prompt/decisions/commits that produced
                                    it. GET /api/why?path=.
kb recollect [<query>] [--similar-to SESSION_ID] [--folder NAME]
   [--since day|week|month|year] [--limit N] [--daemon URL] [--json]
                                    R3: "has something like this been done?"
                                    — semantic search over past-session
                                    DIGESTS, surfacing recency/staleness,
                                    errors, and commits per hit. Pull-only;
                                    never asserted as truth (unlike recall).
kb reading <id|path>                RP-track: reading-progress for an artifact —
  [--kb N] [--json] [--lite]        how far it was read, per-section read vs
                                    skimmed vs unseen, where the reader stopped,
                                    and which sections held their attention.
                                    `--lite` = whole-page only; `--json` for
                                    Claude Code (consult before revising a doc).
kb list {create,ls,show,add,rm,     RL-track (v0.18): reading lists — named,
         update,move,rename,edit,   ordered, per-kb; entries target a whole
         delete,reanchor,prune,     artifact or a §section (`add <list>
         import,export}             <target> [--section ID]`). Read state is
                                    DERIVED from reading progress (override
                                    with `update --read/--unread/--clear-read`).
                                    `<list>` = l_… id or unique title;
                                    `<entry>` = le_… id or 1-based index.
                                    `import <file|-> [--into L] [--mode
                                    replace|append] [--dry-run]` /
                                    `export <list> [--format md|json]` speak
                                    the portable kb-list/1 document — one
                                    heredoc materializes a curated list.
                                    See docs/reading-lists.md.
kb notes {list,show,new,edit,       N-track: free-standing notes / todo-lists
          check,uncheck,append,     attached to a kb or a folder within it. A
          done,archive,rm,links}    note is a Markdown artifact (`kb-category=
                                    note`), so it's searchable + commentable
                                    like any artifact. `new [--folder D]
                                    [--title T] [--body TXT|--stdin] [--notepad]`
                                    creates an ad-hoc note (or the scope's
                                    canonical `_notepad.md`); `check <note>
                                    --item N` / `uncheck` toggle GFM checklist
                                    items; `append <note> --item TEXT` adds one.
                                    `<note>` is an id / source-relative path /
                                    unique filename. (Comment on a note with the
                                    ordinary `kb comments` verbs.) Write
                                    `[[title]]` in a body to link any artifact;
                                    `links <note>` shows outgoing + backlinks.
kb backlinks <target>               What references this artifact — notes that
                                    `[[wikilink]]` it (or artifacts that link
                                    it). Works on any artifact; `--json`.
kb links {suggest,apply}            CT-F3 unlinked mentions: the graph you
                                    wrote is half the graph you meant.
                                    `suggest [--kb K] [--limit N] [--json]`
                                    lists docs whose prose names another
                                    artifact's exact title or unique basename
                                    with no link edge for it — derived per
                                    request, nothing stored, nothing rewritten
                                    (code spans/fences, existing `[[…]]`,
                                    link labels, self-mentions, names under 12
                                    chars and ambiguous names are all
                                    excluded). `apply <src> <dst> [--kb K]`
                                    authors ONE of them as a real
                                    `[[wikilink]]` in `<src>`'s Markdown
                                    source (both are ids / paths / unique
                                    filenames). An HTML artifact or a memory
                                    body can be a link TARGET but never a
                                    SOURCE (invariant #29): those rows are
                                    listed with the reason and refused by
                                    `apply`, file untouched.
kb refs [<target>]                  Code references a doc cites — paths,
                                    path:line, Namespace::Class, Class#method,
                                    gem paths, GitHub issues. Hints only (kb
                                    has no checkout). No <target> walks the
                                    corpus. `--by-target <path>` flips to the
                                    reverse lookup — every doc citing that
                                    exact path. `--lint` reports inferred refs
                                    with the `data-kb-ref` you'd paste to
                                    declare them. `--gallery` prints a ready
                                    `/?kb=&ids=` gallery link over the
                                    resolved doc-id set (invariant #35).
                                    `--json`.
```

Verbs that talk to the daemon accept `--daemon URL` (default
`http://127.0.0.1:4000`) and pick up the bearer token from
`~/.config/kb/token` automatically. `--config PATH` is global (default
`~/.config/kb/kb.toml`). For the full Claude-prompt manifest of every
verb and flag, run `kb tools`.

`kb events --follow` prints each SSE event from the daemon as one JSON
line, useful for `| grep` / `| jq` pipelines (server-side `--types` /
`--kb` / `--artifact` filters; Last-Event-ID reconnect + backoff).

