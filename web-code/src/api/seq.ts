// `GET /api/seq?repo=` (kbc-seq/1) — the read shape the Collections page uses.
//
// Deliberately NOT in `types.ts`: that file's hand-mirror count is a
// ratchet (`scripts/ci/wire-ratchet.toml`) that may only go down, and the
// right fix for these two shapes is a `#[ts(export)]` on the Rust structs
// (`crates/kb-code-server/src/seq.rs`), which this SPA-only unit does not
// touch. Only the fields the page reads; `projection` and `source` stay
// `string` so a daemon that grows a new value cannot break the parse (the
// page counts the values it knows and ignores the rest).
export interface SeqProjectionOut {
  projection: string;
  id: string;
  name: string;
  size?: number;
  source: string;
  updated_at: number;
}

export interface SeqListOut {
  schema: string;
  repo: string;
  projections: SeqProjectionOut[];
}
