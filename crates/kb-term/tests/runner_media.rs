//! The runner's `media` lookup over the omp blob directory (T21), through
//! the public `kb_term::runner::media` API with an explicit blob dir (no
//! process-env mutation, so the tests run in parallel safely). Every
//! picture is synthetic: a PNG/JPEG/WebP magic header plus filler.

use kb_term::runner::media::{read_media_with, MAX_MEDIA_BYTES, MAX_OMP_BLOB_BYTES};
use sha2::{Digest, Sha256};
use std::path::Path;

fn sha_of(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    hex::encode(&d[..])
}

fn png(tag: u8) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.extend(std::iter::repeat_n(tag, 64));
    v
}

fn jpeg(tag: u8) -> Vec<u8> {
    let mut v = vec![0xff, 0xd8, 0xff, 0xe0];
    v.extend(std::iter::repeat_n(tag, 64));
    v
}

fn webp(tag: u8) -> Vec<u8> {
    let mut v = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
    v.extend(std::iter::repeat_n(tag, 64));
    v
}

fn put(dir: &Path, name: &str, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

/// A sessions dir with no tap stores in it.
fn empty_sessions() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn code(r: Result<Vec<u8>, kb_term::runner::RunnerError>) -> String {
    r.expect_err("expected a refusal").code().to_string()
}

#[test]
fn serves_bare_and_suffixed_blob_by_content_hash() {
    let sessions = empty_sessions();
    let blobs = tempfile::tempdir().unwrap();
    let cases: [(&str, Vec<u8>); 4] = [
        ("", png(1)),
        (".jpg", jpeg(2)),
        (".png", png(3)),
        (".webp", webp(4)),
    ];
    for (suffix, bytes) in &cases {
        let sha = sha_of(bytes);
        put(blobs.path(), &format!("{sha}{suffix}"), bytes);
        let got = read_media_with(sessions.path(), Some(blobs.path()), &sha).unwrap();
        assert_eq!(&got, bytes, "suffix {suffix:?}");
    }
}

#[test]
fn rejects_non_hex64() {
    let sessions = empty_sessions();
    // The blob dir does not exist: the grammar check comes first.
    let blobs = sessions.path().join("no-such-blob-dir");
    let good = "a".repeat(64);
    let bad = [
        good.to_uppercase(),
        "a".repeat(63),
        "a".repeat(65),
        "../x".to_string(),
        "%2e%2e".to_string(),
        String::new(),
        format!("{}\0", "a".repeat(63)),
        format!("../{}", &good[..61]),
    ];
    for b in &bad {
        let r = read_media_with(sessions.path(), Some(&blobs), b);
        assert_eq!(code(r), "bad-request", "{b:?}");
    }
}

#[test]
fn rejects_blob_whose_content_does_not_hash_to_its_name() {
    let sessions = empty_sessions();
    let blobs = tempfile::tempdir().unwrap();
    let claimed = sha_of(b"something else entirely");
    put(blobs.path(), &claimed, &png(9));
    let r = read_media_with(sessions.path(), Some(blobs.path()), &claimed);
    assert_eq!(code(r), "not-found");
}

#[test]
fn rejects_symlinked_blob() {
    let sessions = empty_sessions();
    let blobs = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let bytes = png(5);
    let sha = sha_of(&bytes);
    // The link target is a perfectly valid, correctly named picture.
    put(outside.path(), &sha, &bytes);
    std::os::unix::fs::symlink(outside.path().join(&sha), blobs.path().join(&sha)).unwrap();
    let r = read_media_with(sessions.path(), Some(blobs.path()), &sha);
    assert_eq!(code(r), "not-found");
}

#[test]
fn rejects_svg_and_text_bytes_even_when_hash_matches() {
    let sessions = empty_sessions();
    let blobs = tempfile::tempdir().unwrap();
    let svg: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>";
    let text: &[u8] = b"just some synthetic text, not a picture";
    for bytes in [svg, text] {
        let sha = sha_of(bytes);
        put(blobs.path(), &sha, bytes);
        let r = read_media_with(sessions.path(), Some(blobs.path()), &sha);
        assert_eq!(code(r), "not-found");
    }
}

#[test]
fn rejects_over_20_mib() {
    let sessions = empty_sessions();
    let blobs = tempfile::tempdir().unwrap();
    let mut big = png(6);
    big.resize(MAX_OMP_BLOB_BYTES as usize + 1, 0);
    let sha = sha_of(&big);
    put(blobs.path(), &sha, &big);
    let r = read_media_with(sessions.path(), Some(blobs.path()), &sha);
    assert_eq!(code(r), "not-found");
    // Exactly at the cap is served.
    let mut at_cap = png(7);
    at_cap.resize(MAX_OMP_BLOB_BYTES as usize, 0);
    let sha2 = sha_of(&at_cap);
    put(blobs.path(), &sha2, &at_cap);
    let got = read_media_with(sessions.path(), Some(blobs.path()), &sha2).unwrap();
    assert_eq!(got.len(), at_cap.len());
    assert!(MAX_OMP_BLOB_BYTES < MAX_MEDIA_BYTES);
}

#[test]
fn tap_store_wins_over_omp_blob_when_both_exist() {
    let sessions = empty_sessions();
    let blobs = tempfile::tempdir().unwrap();
    let omp_bytes = png(8);
    let sha = sha_of(&omp_bytes);
    put(blobs.path(), &sha, &omp_bytes);
    // The tap store holds DIFFERENT bytes under the same key (the tap
    // names its files by the decoded picture's hash, which can differ).
    let tap_bytes = b"tap-store-bytes".to_vec();
    put(
        &sessions.path().join("abcd1234").join("store").join("img"),
        &sha,
        &tap_bytes,
    );
    let got = read_media_with(sessions.path(), Some(blobs.path()), &sha).unwrap();
    assert_eq!(got, tap_bytes);
    // Without the tap file the omp blob answers.
    let sessions2 = empty_sessions();
    let got = read_media_with(sessions2.path(), Some(blobs.path()), &sha).unwrap();
    assert_eq!(got, omp_bytes);
}

#[test]
fn unknown_hash_and_missing_blob_dir_are_not_found() {
    let sessions = empty_sessions();
    let r = read_media_with(sessions.path(), None, &"c".repeat(64));
    assert_eq!(code(r), "not-found");
    let gone = sessions.path().join("gone");
    let r = read_media_with(sessions.path(), Some(&gone), &"c".repeat(64));
    assert_eq!(code(r), "not-found");
}

#[test]
fn only_the_fixed_suffixes_are_tried() {
    let sessions = empty_sessions();
    let blobs = tempfile::tempdir().unwrap();
    let bytes = png(10);
    let sha = sha_of(&bytes);
    put(blobs.path(), &format!("{sha}.gif"), &bytes);
    put(blobs.path(), &format!("{sha}.png.bak"), &bytes);
    let r = read_media_with(sessions.path(), Some(blobs.path()), &sha);
    assert_eq!(code(r), "not-found");
}
