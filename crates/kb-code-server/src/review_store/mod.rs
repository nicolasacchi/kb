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
//! `ReviewStore` itself (store key, membership, seeding, manifest, flock)
//! is RS-U3 and builds on these.

pub mod classify;
pub mod cred;
pub mod git;
mod proc;
pub mod redact;
pub mod url;

pub use classify::{AuthContext, FailureClass};
pub use cred::{
    ApiCredential, ApiCredentialSource, CredError, CredentialPin, FetchCredential,
    FetchCredentialConfig, GhCli, GhCliCredential, HttpsCredential, ProfileKind, Resolution,
    SecretToken,
};
pub use git::{FetchAuth, GitArgs, GitCall, GitOutput, StoreGit, StoreGitError, NO_PUSH_URL};
pub use url::{FetchRefspec, RefName, RefSource, RemoteName, RemoteUrl, UrlRejected};
