use std::process::Command;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn daemon_scope_id_matches_legacy_sha1_prefix() {
    let output = Command::new(airc_core())
        .args(["daemon-scope-id", "/tmp/airc"])
        .output()
        .expect("airc-core daemon-scope-id must spawn");

    assert!(
        output.status.success(),
        "daemon-scope-id failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "dcb77ec809c5\n");
}
