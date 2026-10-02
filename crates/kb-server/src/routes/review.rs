//! `GET /api/kb/{kb}/review/{id}` + `POST .../review/{id}/export` —
//! per-artifact comment reads (kb-comments/1). Topic 11 §B.3.
//!
//! **GET** returns the on-disk file, or — since W1.D (board D7) — a 200
//! with the canonical empty kb-comments/1 skeleton when none exists yet
//! (no-comments is a state, not an error; consumers used to synthesize
//! the same skeleton from a 404 and stay tolerant of older daemons).
//! The `ETag` here is a REPRESENTATION token — sha256 over the bytes this
//! response actually returns, computed AFTER the visibility filter. It was
//! `kb_core::review::etag_for(path)`, a disk-revision token over the
//! UNFILTERED sidecar; see `review_response` for why that was an existence
//! oracle and which token took its place on the write path.
//!
//! v0.40 TN2 — **GET takes `?visibility=public|all`, default `public`.**
//! This is one of only two reads (with `list_reviews`) that can surface a
//! private note, and it does so without a parameter being an error of
//! omission: absent means public.
//!
//! The whole-document write POST was retired in R8: every mutation now
//! goes through the fine-grained endpoints in `routes::comments` (add /
//! reply / resolve / unresolve / edit / delete), which run the load →
//! typed-mutation → save sequence under the daemon-wide `review_lock`.
//! `GET` here stays the canonical read (initial SPA load, `/export`,
//! `kb comments show`, and the `comments.updated` SSE refetch).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::review::{self, ExportFormat, ReviewFile, Visibility};
use kb_core::types::KbName;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use super::is_safe_id;

/// v0.40 TN2 — query for the per-artifact review read. `visibility` is the
/// only opt-in on this route; every other value is part of the kb-comments/1
/// document itself.
#[derive(Debug, Default, Deserialize)]
pub struct ReviewGetQuery {
    /// `public` (default) | `all`. Absent ⇒ public.
    pub visibility: Option<String>,
}

/// Memory cap for the rendered `/export` body. v0.7 P2 dropped the v0.5
/// 1 MB cap entirely (it 413'd legit 200-comment reviews) — but with
/// nothing in its place a pathologically large review drove unbounded
/// allocation. 32 MiB is generous: a realistic review is 1-2 MB (a
/// 600-comment review is ~1.2 MB), and 32 MiB is ~16k comments — well
/// past anything a human authors — while still bounding peak memory.
/// v0.7.1 H7.
const EXPORT_MAX_BYTES: usize = 32 * 1024 * 1024;

/// `GET /api/kb/{kb}/review/{id}[?visibility=public|all]`
///
/// v0.40 TN2 — `visibility` is the ONE opt-in that can surface a private
/// note on a read route, and it is a query param rather than identity
/// plumbing on purpose: the only safe identity discriminator (loopback vs.
/// `Token`, `middleware.rs`) breaks the operator's own remote SPA behind
/// traefik, where its requests arrive as `Legacy`. Gating on an explicit
/// param is honest and testable; gating on a guess is neither. Absent means
/// `public`, never "everything" — a client that forgets the param must not
/// see a note. An unrecognised value is a 400
/// (`Visibility::from_query` → `None` → refuse rather than guess), mirroring
/// `ExportFormat::from_query` on the export route below.
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(q): Query<ReviewGetQuery>,
) -> Response<Body> {
    let (kb_name, _ctx) = match super::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "artifact id {id:?} contains illegal characters"
        )));
    }
    let visibility = match q.visibility.as_deref() {
        None => Visibility::Public,
        Some(raw) => match Visibility::from_query(raw) {
            Some(v) => v,
            None => {
                return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "visibility {raw:?} not one of public|all"
                )))
            }
        },
    };
    let path = state.paths.kb_review_file(&kb_name, &id);
    // D7 (W1.D) — an artifact with no comments yet is an ordinary state,
    // not an error: 200 + the canonical empty kb-comments/1 skeleton (the
    // same shape routes/artifact.rs injects and every consumer already
    // synthesized locally on the old 404). `Ok(None)` carries that case
    // through to `review_response`, which — like the `etag_for` on a
    // missing file it replaces — sends NO ETag for it, so conditional
    // reloads stay correct.
    match review::load(&path) {
        Ok(loaded) => review_response(&kb_name, &id, loaded, visibility),
        Err(e) => error_to_problem_json(&e),
    }
}

/// v0.40 TN2 — drop the comments this reader may not see, IN PLACE on the
/// loaded document (no clone; `review::load` already produced an owned
/// `ReviewFile` that is about to be serialised and dropped).
fn filter_visibility(file: ReviewFile, v: Visibility) -> ReviewFile {
    if v == Visibility::All {
        return file;
    }
    file.into_public_view().0
}

/// O1 (adversarial review 2026-09-30) — the `ETag` is a REPRESENTATION
/// token: sha256 over the exact bytes this response returns, taken AFTER
/// `filter_visibility` has dropped the notes this reader may not see. It
/// was `review::etag_for(path)`, a DISK-REVISION token over the unfiltered
/// sidecar, and that made the header an existence oracle for private
/// notes: creating or editing one moved the token over a BYTE-IDENTICAL
/// public body, so a conditional GET dated the note — forever, to the
/// microsecond — without ever reading it, and the same token was inlined
/// into the UNAUTHENTICATED artifact-subdomain `?cm=on` payload (see
/// `routes::artifact::build_comments_payload`). It is the same "a count is
/// a leak too" rule the feature already applies to `open_count` /
/// `total_count`: what a reader cannot see must not be observable at all.
/// It is also one read cheaper per GET: `etag_for` does its own
/// `std::fs::read` of the sidecar, so the old handler parsed the file and
/// then hashed a second full copy of it.
///
/// A PUBLIC comment's create/edit still moves the token, because the bytes
/// the reader received really did change — that is the whole job of an
/// ETag, and the token stays a pure function of the body, so a repeat read
/// of an unchanged review reproduces it exactly (no per-request churn).
/// `?visibility=public` and `?visibility=all` now get DIFFERENT tokens for
/// the same file, which is correct rather than a bug: they are different
/// representations at different URLs, and a representation's validator
/// must describe that representation. The old comment here argued the
/// opposite — that one disk token for both visibilities was required — but
/// it inverted the property it was protecting. A body token can only 304
/// a client holding those exact bytes, which is precisely the guarantee
/// `If-None-Match` is supposed to give; a disk token 304s a client that
/// may be holding a filtered body it never compared against.
///
/// The disk-revision token is NOT lost: it is what `If-Match` on a write
/// must compare against (`kb_core::review::save_atomic`), because there
/// the question is "did anyone touch this file on disk since I read it",
/// including a write whose visible body did not change. `body_etag`'s
/// domain prefix keeps the two in separate hash domains so one can never
/// be mistaken for the other at that comparison.
///
/// So do not read the split as "the ETag no longer guards writes". It never
/// did: `If-Match` reaches `save_atomic` only from in-process callers — the
/// whole-document POST that carried the header was retired in R8 and every
/// mutation now runs under the per-kb `review_lock` — so no HTTP route
/// hands a client the disk token to echo back, and none reads one. The GET
/// `ETag` is a representation validator, full stop; the disk token is the
/// write path's, and the two are deliberately not interchangeable.
///
/// `loaded` is `None` when there is no sidecar: the skeleton stands in for
/// the body but gets NO ETag. That is not only a leak guard — the
/// skeleton's `generated_at` is `Utc::now()`, so hashing it would mint a
/// fresh token on every single read of an artifact with no comments yet,
/// which is most artifacts. The pre-O1 code got this for free (a missing
/// file has no `etag_for`); keep it.
fn review_response(
    kb_name: &KbName,
    id: &str,
    loaded: Option<ReviewFile>,
    visibility: Visibility,
) -> Response<Body> {
    let (file, is_disk_backed) = match loaded {
        Some(f) => (f, true),
        None => (ReviewFile::public_skeleton(kb_name, id), false),
    };
    // v0.44 P2 (A2-5) — a PUBLIC read of a sidecar with nothing public in it
    // (notes only) is the very skeleton an absent sidecar gets, ETag
    // included (none), so the first private note is not observable here.
    let blank_public = visibility == Visibility::Public
        && is_disk_backed
        && file.comments.iter().all(|c| c.is_private())
        && file.verdict.is_none();
    let bytes = match serde_json::to_vec(&filter_visibility(file, visibility)) {
        Ok(b) => b,
        Err(e) => return error_to_problem_json(&kb_core::Error::from(e)),
    };
    // Hashed before `bytes` moves into the body, and only when a sidecar
    // exists — the skeleton case would be pure waste (see the
    // `generated_at` note above) as well as a lie.
    let etag = (is_disk_backed && !blank_public).then(|| body_etag(&bytes));
    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(etag) = etag {
        // Cannot fail in practice — `body_etag` emits 32 hex digits inside
        // a pair of quotes, all of them valid header-value bytes — and an
        // unreachable 500 branch would be worse than the silent skip this
        // keeps (which is what the pre-O1 code did too).
        if let Ok(v) = HeaderValue::from_str(&etag) {
            resp.headers_mut().insert(header::ETAG, v);
        }
    }
    // Reviews change frequently — never cache.
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Domain prefix for [`body_etag`], hashed in before the body so a
/// representation token can never collide with a `review::etag_for`
/// disk-revision token. `save_atomic` compares `If-Match` as an opaque
/// string against the disk token; without the split, the two live in one
/// 128-bit space and a future caller that fed a GET's ETag to a write
/// would be relying on nothing.
const ETAG_DOMAIN: &[u8] = b"kb-comments/1 public-representation\0";

/// sha256 over the response bytes, truncated to 16 bytes and hex-quoted —
/// the same shape `review::etag_for` emits, so any client parsing an
/// entity-tag keeps working. See [`review_response`] for why the public
/// read hashes the body and the write path does not.
pub(super) fn body_etag(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(ETAG_DOMAIN);
    h.update(bytes);
    format!("\"{}\"", hex::encode(&h.finalize()[..16]))
}

// --- v0.5 P3 — server-side review export ---------------------------------

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    /// `claude` (default Claude prompt) | `json` (raw kb-comments/1) |
    /// `md` (human-readable summary including resolved comments).
    pub format: Option<String>,
}

/// `POST /api/kb/{kb}/review/{id}/export?format=claude|json|md`. Streams
/// the formatted body (chunked transfer, no Content-Length), rejecting
/// with 413 above `EXPORT_MAX_BYTES`. Reuses `kb_core::review::export` —
/// the same impl kb-cli's `kb comments export` calls.
pub async fn post_export(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(params): Query<ExportQuery>,
) -> Response<Body> {
    let (kb_name, _ctx) = match super::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "artifact id {id:?} contains illegal characters"
        )));
    }
    let format = match params.format.as_deref().and_then(ExportFormat::from_query) {
        Some(f) => f,
        None => {
            return error_to_problem_json(&kb_core::Error::BadRequest(
                "?format=claude|json|md required".into(),
            ));
        }
    };
    let path = state.paths.kb_review_file(&kb_name, &id);
    let file = match review::load(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no comments yet for {kb_name}/{id}"
            )));
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let body = match review::export(&file, kb_name.as_str(), format) {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };
    // v0.7.1 H7 — bound peak memory. `review::export` buffers the whole
    // rendered body; reject anything past the cap rather than let a
    // pathological review drive unbounded allocation.
    if body.len() > EXPORT_MAX_BYTES {
        return export_too_large(body.len());
    }
    // Hand the body to a single-frame stream: chunked transfer, no
    // Content-Length, and no redundant second copy (the pre-H7 code
    // re-chunked `body` into a fresh `Vec<Vec<u8>>`, doubling the
    // footprint). `into_bytes` is a zero-cost String → Vec<u8> move.
    let stream = futures::stream::iter([Ok::<_, std::io::Error>(body.into_bytes())]);
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    if let Ok(disp) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{id}-review.{}\"",
        format.extension()
    )) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, disp);
    }
    resp
}

/// 413 problem+json for an export whose rendered body exceeds
/// `EXPORT_MAX_BYTES`. `kb_core::Error` has no payload-too-large variant,
/// so the response is built here (same shape as middleware's `forbidden`).
fn export_too_large(len: usize) -> Response<Body> {
    let body = serde_json::json!({
        "type": "urn:kb:errors:payload-too-large",
        "title": "Payload Too Large",
        "status": 413,
        "detail": format!(
            "rendered export is {len} bytes; the cap is {EXPORT_MAX_BYTES} bytes"
        ),
    });
    let mut resp = (StatusCode::PAYLOAD_TOO_LARGE, Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::review::{Anchor, Author, NewComment};

    const ID: &str = "aaaaaaaaaaaa";

    fn kb() -> KbName {
        KbName::new("smoke").expect("valid kb name")
    }

    fn with_comment(file: &mut ReviewFile, body: &str, private: bool) {
        file.add_comment(NewComment {
            file: ID.to_string(),
            file_label: "main".to_string(),
            anchor: Anchor::File,
            author: Author::Claude,
            body: body.to_string(),
            choices: Vec::new(),
            attachments: Vec::new(),
            user: None,
            tags: Vec::new(),
            private,
        });
    }

    /// Drive the real handler body and hand back what a public reader
    /// actually observes: the `ETag` header and the response bytes.
    async fn read_as(
        kb_name: &KbName,
        path: &std::path::Path,
        v: Visibility,
    ) -> (Option<String>, Vec<u8>) {
        let resp = review_response(kb_name, ID, review::load(path).unwrap(), v);
        let etag = resp
            .headers()
            .get(header::ETAG)
            .map(|h| h.to_str().expect("ascii etag").to_string());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("in-memory body")
            .to_vec();
        (etag, body)
    }

    /// O1 — the ETag is a validator for the bytes the reader got, so a
    /// private note must be invisible to it. The sidecar really does change
    /// (asserted: otherwise this test would pass on a broken filter) — what
    /// must not move is the public token, over a byte-identical body.
    #[tokio::test]
    async fn private_note_does_not_move_the_public_etag() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(format!("{ID}.json"));
        let mut file = ReviewFile::empty_skeleton(&kb(), ID, "t");
        with_comment(&mut file, "public body", false);
        let disk_before = review::save_atomic(&path, &file, None).unwrap();

        let (e0, body0) = read_as(&kb(), &path, Visibility::Public).await;
        let e0 = e0.expect("a sidecar-backed read carries an ETag");
        // Re-reading an unchanged review must reproduce the token exactly: a
        // token that moved per request would be no validator at all.
        assert_eq!(
            Some(e0.clone()),
            read_as(&kb(), &path, Visibility::Public).await.0,
            "the public ETag must be a pure function of the body"
        );

        with_comment(&mut file, "PRIVATE NOTE BODY", true);
        let disk_after = review::save_atomic(&path, &file, None).unwrap();
        assert_ne!(
            disk_before, disk_after,
            "sanity: the sidecar really was rewritten — otherwise this test \\
             proves nothing about the filter"
        );

        let (e1, body1) = read_as(&kb(), &path, Visibility::Public).await;
        assert_eq!(
            Some(e0.clone()),
            e1,
            "O1: a private note moved the public ETag"
        );
        assert_eq!(body0, body1, "the public body must be byte-identical too");
        assert!(
            !String::from_utf8_lossy(&body1).contains("PRIVATE NOTE BODY"),
            "sanity: the private note really is filtered out of this body"
        );

        // The token is hashed AFTER the filter, so the reader who IS
        // allowed to see the note gets a different one for the same file.
        let e_all = read_as(&kb(), &path, Visibility::All).await.0;
        assert_ne!(
            Some(e0),
            e_all,
            "?visibility=all must not share the public read's validator"
        );
    }

    /// The other half of the contract: a PUBLIC comment's edit has to move
    /// the token, or the fix above would have "solved" the leak by making
    /// the ETag useless.
    #[tokio::test]
    async fn public_edit_moves_the_public_etag() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(format!("{ID}.json"));
        let mut file = ReviewFile::empty_skeleton(&kb(), ID, "t");
        with_comment(&mut file, "public body", false);
        review::save_atomic(&path, &file, None).unwrap();
        let e0 = read_as(&kb(), &path, Visibility::Public).await.0;

        let mut edited = review::load(&path).unwrap().unwrap();
        edited.comments[0].body = "edited".into();
        review::save_atomic(&path, &edited, None).unwrap();

        assert_ne!(
            e0,
            read_as(&kb(), &path, Visibility::Public).await.0,
            "a public comment's edit must move the public ETag"
        );
    }

    /// The disk-revision token is what `If-Match` compares, and it must
    /// still see a write whose PUBLIC body did not change — that is the
    /// one case where "did anyone touch the file" and "did the reader's
    /// bytes change" are different questions with different answers.
    #[test]
    fn if_match_disk_token_still_sees_an_invisible_concurrent_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(format!("{ID}.json"));
        let mut file = ReviewFile::empty_skeleton(&kb(), ID, "t");
        with_comment(&mut file, "public body", false);
        let mine = review::save_atomic(&path, &file, None).unwrap();

        // A concurrent writer adds a note this reader must never learn of.
        let mut theirs = review::load(&path).unwrap().unwrap();
        with_comment(&mut theirs, "their private note", true);
        review::save_atomic(&path, &theirs, None).unwrap();

        let err = review::save_atomic(&path, &file, Some(&mine))
            .expect_err("If-Match must reject a write that raced a disk edit");
        assert!(
            matches!(err, kb_core::Error::PreconditionFailed(_)),
            "expected 412, got {err:?}"
        );

        // And the fresh disk token round-trips, so this is a rejection and
        // not a permanent lockout.
        let fresh = review::etag_for(&path).unwrap().expect("file exists");
        review::save_atomic(&path, &file, Some(&fresh)).expect("fresh If-Match succeeds");
    }

    /// v0.44 P2 (A2-5): a notes-only sidecar answers a PUBLIC read with the
    /// same bytes and the same (absent) ETag as no sidecar at all, and the
    /// answer is stable across requests.
    #[tokio::test]
    async fn notes_only_sidecar_is_indistinguishable_from_absent_on_public_get() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(format!("{ID}.json"));
        let absent = read_as(&kb(), &path, Visibility::Public).await;
        assert!(absent.0.is_none());

        let mut file = ReviewFile::empty_skeleton(&kb(), ID, "A Real Title");
        with_comment(&mut file, "PRIVATE NOTE BODY", true);
        review::save_atomic(&path, &file, None).unwrap();
        let notes_only = read_as(&kb(), &path, Visibility::Public).await;
        assert_eq!(absent, notes_only, "first note changed the public GET");

        // Stable: the bytes do not carry a per-request timestamp.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert_eq!(absent, read_as(&kb(), &path, Visibility::Public).await);
    }

    /// No sidecar ⇒ no ETag. The skeleton stamps `generated_at: Utc::now()`,
    /// so hashing it would mint a fresh token on every read of an artifact
    /// with no comments yet — the majority of artifacts — and a validator
    /// that changes per request is worse than none.
    #[tokio::test]
    async fn absent_review_sends_no_etag() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(format!("{ID}.json"));
        let resp = review_response(&kb(), ID, review::load(&path).unwrap(), Visibility::Public);
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get(header::ETAG).is_none(),
            "the synthesized skeleton must not carry a validator"
        );
    }
}
