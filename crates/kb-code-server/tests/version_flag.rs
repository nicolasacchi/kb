//! VS1 — the `kb-code-server --version` flag reports the build stamp (v-prefix
//! stripped, non-version refs refused) or the documented dev fallback.

#[test]
fn version_flag_prints_stamp_or_dev_fallback() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kb-code-server"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let ver = text.trim().strip_prefix("kb-code-server ").unwrap();
    let expected = match option_env!("KB_BUILD_VERSION") {
        Some(raw) => kb_code_server::normalize_stamp_version(raw),
        None => kb_code_server::UNSTAMPED_VERSION,
    };
    assert_eq!(ver, expected);
    assert_ne!(ver, "0.0.0");
}
