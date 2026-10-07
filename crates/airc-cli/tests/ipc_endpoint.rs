//! Resolve-only binary contract for installer diagnostics. No daemon is needed.
use std::path::Path;
use std::process::Command;

mod common;

#[test]
fn native_endpoint_uses_transport_resolver_without_starting_daemon() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("scope");
    let run = |native: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_airc"));
        command
            .current_dir(workspace.path())
            .env("HOME", workspace.path())
            .env("USERPROFILE", workspace.path())
            .env("AIRC_HOME", &home)
            .env("AIRC_NO_STALENESS", "1")
            .env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1")
            .args(["--home", home.to_str().unwrap(), "ipc-endpoint"]);
        if native {
            command.arg("--native");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let output = command.output().expect("run fresh public CLI");
        assert!(output.status.success(), "{:?}", output);
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let logical = run(false);
    let native = run(true);
    assert_eq!(
        native,
        airc_ipc::transport::native_endpoint(Path::new(&logical))
    );
    assert!(!Path::new(&logical).exists(), "resolver created a listener");
    assert!(
        !home.join("daemon.pid").exists(),
        "resolver started a daemon"
    );
    assert!(!workspace.path().join(".airc/daemon.pid").exists());
}
