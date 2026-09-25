//! RS-U2 — the kb-owned internal review store (README "kb-code reviews
//! that diff like GitHub", §5/§8). This unit lands the security floor the
//! rest of the store is built on:
//!
//! * [`git::StoreGit`] — the ONLY git spawner allowed to carry a
//!   credential: scrubbed env, `-c` hardening, per-call timeout with
//!   process-group kill, bounded capture, redacted + classified stderr,
//!   validated argv atoms, no push.
//! * [`cred`] — fetch-slot credential profiles (`gh-cli`, `token`,
//!   `anonymous`, `inherit`, `none`; deploy keys are Phase 2) and the
//!   ladder that picks one; the api-slot [`cred::ApiCredential`].
//! * [`url`] — the URL allowlist and the argv atom types.
//! * [`classify`] / [`redact`] — failure classes with stable
//!   `urn:kb:errors:<slug>` slugs, and the secret redactor.
//!
//! RS-U3 adds the store itself, on top of those:
//!
//! * [`key`] — `store_key` normalization (`host/owner/name`, `local:<uuid>`);
//! * [`ladder`] — the base-URL ladder and membership (README §5.1);
//! * [`settings`] — `[review.store]` / `[[review.repos]]`, tolerant enums;
//! * [`seed`] — seed-by-fetch (README §5.2) and the sync primitives;
//! * [`manifest`] — `kb-code-store.json` + the per-store `flock`;
//! * [`registry`] — [`ReviewStores`] on `AppState`: registration, the seeding
//!   state machine, the fetch/ops mutexes, and the handle API later units
//!   call (`handle_for_repo`, `admit_mutation`, `fetch_lock`, `ops_lock`,
//!   `resolve_credential`, `fetch_base`);
//! * [`boot`] — the background boot seeding job (D4);
//! * [`routes`] — `GET /api/repos/{name}/store|credentials` and the
//!   loopback-only `store/sync`, `store/base-url`, `credentials/test`.
//!
//! RS-U5 adds the store-WIDE ref family + GC on top of that (README §5.4):
//!
//! * [`gc`] — `keep_set`/`attribute`/`delete_candidates`/`apply`: the
//!   store-wide GC engine, pure classification + one guarded
//!   `update-ref --stdin` transaction. `crate::reviews::{parse_kbc_ref,
//!   KbcRef, patchset_base_ref, prm_ref, hint_ref}` carry the ref-family
//!   parser/builders (extended, not duplicated, per README §5.1/§5.3);
//!   `crate::reviews::delete_review_with_refs`'s `PrRefScope` and
//!   `crate::reviews::gc_patchsets` are the two write paths this unit
//!   makes store-aware (via `GitCtx::primary`, RS-U4).

pub mod boot;
pub mod classify;
pub mod cred;
pub mod gc;
pub mod git;
pub mod key;
pub mod ladder;
pub mod manifest;
mod proc;
pub mod redact;
pub mod registry;
pub mod routes;
pub mod seed;
pub mod settings;
pub mod url;

pub use classify::{AuthContext, FailureClass};
pub use cred::{
    ApiCredential, ApiCredentialSource, CredError, CredentialPin, FetchCredential,
    FetchCredentialConfig, GhCli, GhCliCredential, HttpsCredential, ProfileKind, Resolution,
    SecretToken,
};
pub use git::{FetchAuth, GitArgs, GitCall, GitOutput, StoreGit, StoreGitError, NO_PUSH_URL};
pub use registry::{
    ReviewStores, StoreHandle, StoreRefusal, StoreUnavailable, SEEDING_RETRY_AFTER_SECS,
    URN_STORE_SEEDING,
};
pub use url::{FetchRefspec, RefName, RefSource, RemoteName, RemoteUrl, UrlRejected};
