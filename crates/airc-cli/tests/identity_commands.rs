use std::process::Command;

fn airc_rs() -> &'static str {
    env!("CARGO_BIN_EXE_airc-rs")
}

#[test]
fn identity_pretty_matches_whois_shape() {
    let output = Command::new(airc_rs())
        .args([
            "identity",
            "pretty",
            "--name",
            "alice",
            "--identity-json",
            r#"{"pronouns":"she","role":"builder","bio":"makes things","status":"away","integrations":{"continuum":"alice-c"}}"#,
            "--host",
            "alice.local",
        ])
        .output()
        .expect("airc-rs identity pretty must spawn");

    assert!(
        output.status.success(),
        "identity pretty failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        concat!(
            "  name:      alice\n",
            "  pronouns:   she\n",
            "  role:       builder\n",
            "  bio:        makes things\n",
            "  status:     away\n",
            "  integrations:\n",
            "    continuum: alice-c\n",
            "  host:      alice.local\n",
        )
    );
}

#[test]
fn identity_pretty_defaults_unset_fields() {
    let output = Command::new(airc_rs())
        .args(["identity", "pretty", "--name", "alice"])
        .output()
        .expect("airc-rs identity pretty must spawn");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("  pronouns:   (unset)\n"));
    assert!(stdout.contains("  integrations: (none)\n"));
}
