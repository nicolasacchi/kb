-- V0038 (V74-L3a, design of record docs/research/
-- kb-code-v7-continuum-2026-09.html §Decisions D11 + D21, Track L) —
-- `kbc-recipe/1`: trust-on-first-use for repo-versioned recipes, the
-- server-stored recipe home, and materialised runs.
--
-- schema-epoch: rides the SAME refinery epoch sequence as every other
-- kb-code-server migration (kb invariant #2's `kb_core::sibling::
-- refuse_if_volume_ahead` boot guard) — no bump of its own, V0038 IS the
-- next epoch. Every statement is O(1) DDL on three brand-new EMPTY
-- tables: nothing scans, nothing backfills, so this adds no boot latency
-- (the V0027/V0028/V72-B0 lesson — no whole-corpus work between
-- `Store::open` and the `TcpListener::bind`).
--
-- ## What is NOT stored here
--
-- A recipe's RESULT is not stored — a run is recomputed from the mirror
-- on every request, exactly as `boards::resolve` re-resolves a node and
-- `lanes::classing` re-mints a class (invariants 13/21/22, root
-- invariant #2's "kb-code mints classes, nothing is cached"). The one
-- exception is a MATERIALISED run, which the operator asks for
-- explicitly over loopback and which records the mirror generation it
-- was computed at so a replay can say plainly that it is a SNAPSHOT
-- rather than pretending to be fresh.
--
-- A repo-versioned recipe's BODY is not stored either, except as the
-- exact bytes a human trusted (`recipe_trust.trusted_body`) — the file in
-- `.kbc/recipes/*.toml` at the repo's DEFAULT ref is the truth, and the
-- stored copy exists for exactly one purpose: rendering the unified diff
-- when the content hash changes. Persisting the live body would make the
-- daemon a second, forkable home for a document the repo already owns.
--
-- ## Why TOFU at all
--
-- `.kbc/recipes/*.toml` arrives by `git clone`/`git pull`. The op set is
-- closed and structurally cannot reach an exec lane (D21, invariant 10),
-- so a hostile recipe cannot run a process — but it CAN cost a great deal
-- of IO and it CAN point a reader at the wrong evidence. Trust-on-first-
-- use keyed on the content hash is the `.vscode/tasks.json` posture that
-- `[lanes]` already takes one layer down (invariant 21(a)): the repo
-- proposes, the operator accepts, and a CHANGE re-arms the prompt with a
-- diff instead of silently inheriting the old decision.

CREATE TABLE recipe_trust (
    repo_id      INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    -- The recipe's slug, which is the file stem (`.kbc/recipes/<slug>.toml`).
    slug         TEXT    NOT NULL,
    -- Repo-relative source path, recorded so a rename reads as a NEW
    -- untrusted recipe rather than inheriting a decision made about a
    -- file that no longer exists.
    source_path  TEXT    NOT NULL,
    -- The git blob oid of the trusted bytes. A content address, not a
    -- timestamp: the same bytes at a different commit are still trusted,
    -- and one changed byte is not.
    content_hash TEXT    NOT NULL,
    -- The exact bytes trusted, for the unified diff shown on change.
    -- Capped by the loader (`recipe::loader::MAX_RECIPE_BYTES`), never by
    -- SQL.
    trusted_body TEXT    NOT NULL,
    trusted_unix INTEGER NOT NULL,
    PRIMARY KEY (repo_id, slug)
);
CREATE INDEX idx_recipe_trust_repo ON recipe_trust(repo_id);

CREATE TABLE recipes_server (
    -- Agent-authored and UI-saved recipes (`recipe new --from-json -`),
    -- which write HERE and never into the tree (D11). A repo file WINS on
    -- a slug collision and every catalog row says which home it came
    -- from, so a server row shadowed by a repo file is reported rather
    -- than silently ignored.
    slug         TEXT    PRIMARY KEY,
    -- NULL = available on every repo. A named repo scopes the row to it.
    repo         TEXT,
    title        TEXT    NOT NULL,
    -- serde of `recipe::RecipeDoc` — this daemon's OWN typed document,
    -- written only after the loader's type-check has passed, and parsed
    -- on every read. The `canvas_nodes.ref_json` posture (invariant
    -- 22), deliberately the opposite of `reading_sets.desk_json`
    -- (invariant 9): a body the daemon could not interpret could not be
    -- run, and an unrunnable recipe is what this validation exists to
    -- prevent.
    body_json    TEXT    NOT NULL,
    created_unix INTEGER NOT NULL,
    updated_unix INTEGER NOT NULL
);

CREATE TABLE recipe_runs (
    -- `run_` + 12 hex, minted the same way every other id in this crate
    -- is (`annotations::new_annotation_id`).
    id           TEXT    PRIMARY KEY,
    repo_id      INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    slug         TEXT    NOT NULL,
    -- The resolved params/scope the run actually used, so a replay is a
    -- replay of THIS run and not of whatever the defaults say today.
    params_json  TEXT    NOT NULL,
    scope        TEXT,
    -- The store's monotonic mirror generation at run time. A replay
    -- compares it to the CURRENT generation and captions itself stale;
    -- it is never a commit distance (invariant 16(d)'s rule, restated).
    generation   INTEGER NOT NULL,
    -- serde of the `kbc-recipe-run/1` response. A SNAPSHOT — see this
    -- file's header.
    result_json  TEXT    NOT NULL,
    created_unix INTEGER NOT NULL
);
CREATE INDEX idx_recipe_runs_slug ON recipe_runs(repo_id, slug, created_unix DESC);
