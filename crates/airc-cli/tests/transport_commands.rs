//! A health read must report missing daemon evidence, not create a local route.
use std::process::Command;
mod common;

#[test]
fn transport_health_missing_daemon_is_visible_even_with_degraded_only(
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = common::daemon_tempdir();
    for flags in [vec![], vec!["--degraded-only"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_airc"))
            .arg("--home")
            .arg(workspace.path())
            .args(["transport", "health"])
            .args(flags)
            .output()?;
        common::assert_success(&output, "transport health");
        let text = String::from_utf8(output.stdout)?;
        assert!(text.contains("attention required"), "{text}");
        assert!(text.contains("UNAVAILABLE"), "{text}");
        assert!(!text.contains("state=healthy"), "{text}");
    }
    Ok(())
}

#[test]
fn transport_health_missing_daemon_json_and_fail_agree() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = common::daemon_tempdir();
    let output = Command::new(env!("CARGO_BIN_EXE_airc"))
        .arg("--home")
        .arg(workspace.path())
        .args(["transport", "health", "--json", "--fail"])
        .output()?;
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["requires_attention"], true);
    assert!(report["findings"]
        .as_array()
        .ok_or("missing findings array")?
        .iter()
        .any(|finding| finding["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("UNAVAILABLE"))));
    Ok(())
}

#[test]
fn transport_health_quiet_fails_without_output_when_daemon_unavailable(
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = common::daemon_tempdir();
    let output = Command::new(env!("CARGO_BIN_EXE_airc"))
        .arg("--home")
        .arg(workspace.path())
        .args(["transport", "health", "--quiet"])
        .output()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    Ok(())
}
