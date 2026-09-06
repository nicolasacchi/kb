//! Explicit error type carrying HTTP status + URN suffix so the kb-server
//! `problem_json` middleware can emit RFC 7807 responses without
//! pattern-matching the variant. Topic 11's URN form is `urn:kb:errors:<kind>`.

use std::result::Result as StdResult;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// HTTP 412 — used by review writes when the client's `If-Match`
    /// ETag doesn't match the on-disk version (another writer has
    /// modified it between the GET and the POST).
    #[error("precondition failed: {0}")]
    PreconditionFailed(String),

    /// HTTP 401 — v0.4 bearer-token auth missing or invalid on a
    /// non-loopback `/api/*` request. Loopback bypasses the check
    /// entirely (so local dev still works without a token).
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// HTTP 429 — v0.4 per-token rate limit exceeded on a hot
    /// endpoint (search, atlas recompute, review POST). The middleware
    /// also sets a `Retry-After` header.
    #[error("rate limited: {0}")]
    RateLimited(String),

    /// HTTP 403 — a request that IS authenticated (or auth doesn't apply)
    /// but is refused on origin grounds a bearer token can't satisfy. W7
    /// (R15/LF-5): the live-session lane (`/api/sessions/presence`,
    /// `/api/sessions/{sid}/live`) is loopback-only in v1 — a valid token
    /// from a non-loopback caller still 403s, which is exactly why this is
    /// distinct from `Unauthorized` (401 means "prove who you are"; this
    /// means "no credential admits you here at all").
    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("storage error: {0}")]
    Storage(String),

    /// HTTP 502 — `kb share` failed talking to an upstream host
    /// (Cloudflare / GitHub): a non-2xx API response, a transport error,
    /// or an unexpected payload shape. The detail carries the upstream
    /// message so the operator can act on it.
    #[error("share error: {0}")]
    Share(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialisation error: {0}")]
    Serde(String),

    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl Error {
    /// HTTP status code that the problem+json middleware should emit.
    pub fn http_status(&self) -> u16 {
        match self {
            Error::NotFound(_) => 404,
            Error::BadRequest(_) => 400,
            Error::Conflict(_) => 409,
            Error::PreconditionFailed(_) => 412,
            Error::Unauthorized(_) => 401,
            Error::RateLimited(_) => 429,
            Error::Forbidden(_) => 403,
            Error::Config(_) => 500,
            Error::Storage(_) => 503,
            Error::Share(_) => 502,
            Error::Io(_) => 500,
            Error::Serde(_) => 400,
            Error::Internal(_) => 500,
        }
    }

    /// URN suffix for the `type` field of the problem+json body.
    /// Concatenated with `urn:kb:errors:` to form the full URN.
    pub fn urn_kind(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not-found",
            Error::BadRequest(_) => "bad-request",
            Error::Conflict(_) => "conflict",
            Error::PreconditionFailed(_) => "precondition-failed",
            Error::Unauthorized(_) => "unauthorized",
            Error::RateLimited(_) => "rate-limited",
            Error::Forbidden(_) => "forbidden",
            Error::Config(_) => "config",
            Error::Storage(_) => "storage",
            Error::Share(_) => "share",
            Error::Io(_) => "io",
            Error::Serde(_) => "serde",
            Error::Internal(_) => "internal",
        }
    }

    /// Full RFC 7807 `type` URN.
    pub fn problem_type(&self) -> String {
        format!("urn:kb:errors:{}", self.urn_kind())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serde(e.to_string())
    }
}

impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::Config(e.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Share(e.to_string())
    }
}

pub type Result<T> = StdResult<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_matches_variant() {
        assert_eq!(Error::NotFound("x".into()).http_status(), 404);
        assert_eq!(Error::BadRequest("x".into()).http_status(), 400);
        assert_eq!(Error::Conflict("x".into()).http_status(), 409);
        assert_eq!(Error::Storage("x".into()).http_status(), 503);
        assert_eq!(Error::Forbidden("x".into()).http_status(), 403);
    }

    #[test]
    fn problem_type_formats_urn() {
        assert_eq!(
            Error::NotFound("x".into()).problem_type(),
            "urn:kb:errors:not-found"
        );
        assert_eq!(
            Error::Storage("x".into()).problem_type(),
            "urn:kb:errors:storage"
        );
    }

    #[test]
    fn io_error_converts() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let e: Error = io.into();
        assert_eq!(e.http_status(), 500);
        assert_eq!(e.urn_kind(), "io");
    }
}
