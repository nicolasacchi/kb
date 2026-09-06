//! kb-lip — **lip/1**: a generic LSP-to-HTTP adapter (design-lip.md,
//! Track L1). Spawns and supervises ONE language server as a stdio child
//! process and speaks a small closed HTTP surface (`GET /lip/identity`,
//! `POST /lip/{hover,definition,references,symbols,diagnostics}` — the
//! fifth, `diagnostics`, an AMENDED addition, design-addendum-2.md §D)
//! guarded by a git-blob-hash freshness check on every request. Zero
//! heavy deps by design — see Cargo.toml's package description for the
//! full rationale; this crate never links kb-core/kb-server/ONNX/
//! tree-sitter, and nothing in kb-code-server ever links THIS crate (it
//! talks lip/1 over HTTP — that's L2, a separate track).

pub mod blob;
pub mod config;
pub mod http;
pub mod lsp;
pub mod position;
pub mod rpc;
pub mod server;
pub mod supervisor;

pub use config::Config;
pub use supervisor::Supervisor;
