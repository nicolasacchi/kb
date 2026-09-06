-- DCB W1.C — doc-lens pins + the symbol-name index the doc-lens symbol lane
-- needs. Two unrelated objects in one migration deliberately: both are W1.C's
-- and refinery numbers are a scarce, ordered resource (V0019__canvas_sets.sql
-- was the tip when this landed).

-- ---------------------------------------------------------------------------
-- doc_lens_pins — "the pin IS the remembered read-time choice" (DCB Decision 1).
--
-- Operator-local by ruling (Decision 4): nothing here rides `kb share`, and the
-- table is deliberately NOT a resolution cache — it stores WHICH CHECKOUT a
-- human picked for a document, never what that pick resolved to. No cached
-- verdict may outlive the tree it was computed against.
--
-- `repo` is the configured repo NAME (plain TEXT, not a FK to repos(id)) —
-- V0011 `bookmarks`' argument, deliberately not V0009 `reading_sets`': a pin is
-- an operator-owned preference that must survive a repo re-register, and the
-- read path never needs to join through `repos`.
--
-- `repo_root` records the filesystem root the pin was CHOSEN against.
-- `store::upsert_repo` is `ON CONFLICT(name) DO UPDATE SET root`, so
-- re-pointing `path` under a stable name silently re-uses the repo id — a pin
-- would then select a tree it was never chosen against, which is exactly the
-- silently-wrong-checkout failure Decision 1 exists to prevent. Comparing
-- `repo_root` against the live configured root makes that detectable; the read
-- path refuses such a pin, and W2.A drops it at boot.
--
-- PK (kb, doc_id): one pin per document, daemon-wide. `doc_id` is kb's
-- path-derived artifact id (kb-core invariant #27) and CAN change under
-- `kb mv`; `join::kb_client::code_refs` follows kb's 301 chain and the handler
-- re-keys via `store::rekey_doc_lens_pin`. A pin whose doc 404s on kb is
-- dropped, loudly (never silently repaired).
CREATE TABLE doc_lens_pins (
    kb          TEXT NOT NULL,
    doc_id      TEXT NOT NULL,
    repo        TEXT NOT NULL,
    repo_root   TEXT NOT NULL,
    -- `coderef/1`'s doc_hash at pin time; purely informational in v1 (W3.C's
    -- "doc changed since" is the consumer). NULL when kb served none.
    doc_hash    TEXT,
    pinned_at   INTEGER NOT NULL,
    PRIMARY KEY (kb, doc_id)
);

-- `store::list_doc_lens_pins(repo)` + W2.A's boot prune ("every pin naming a
-- repo that is no longer configured").
CREATE INDEX idx_doc_lens_pins_repo ON doc_lens_pins(repo);

-- ---------------------------------------------------------------------------
-- symbols(name) — the doc-lens symbol lane issues ONE
-- `SELECT … WHERE f.repo_id = ?1 AND s.name IN (…)` per (repo, request)
-- (`store::symbols_named_many`). `symbols`' PK is (blob_hash, salt, ordinal)
-- (V0001) and the only other index is `idx_files_blob_hash` on FILES, so
-- without this every such query is a full scan of the symbols table across
-- EVERY repo. `hierarchy.rs`'s existing `symbols_named_in_repo` calls get the
-- same win for free.
CREATE INDEX idx_symbols_name ON symbols(name);
