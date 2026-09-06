-- v0.3 (F1): edges table is now actually populated by the indexer's
-- link-graph extraction step. The original schema indexed dst only
-- (handy for "what links to me?" queries that don't ship in v0.3);
-- the routes::graph cross-artifact path needs the inverse — outbound
-- BFS from a starting artifact id — so add the src index.
CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src_artifact);
