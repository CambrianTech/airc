//! Public updater against isolated IPC owners, without the account mesh or LAN.
mod common;
use airc_ipc::{
    codec::{read_frame, write_frame},
    request::Request,
    response::{Response, StatusResponse},
    transport::IpcListener,
};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

fn hidden(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
fn cli(account: &Path, source: &Path, args: &[&str]) -> Output {
    cli_with_isolation(account, source, args, true)
}
fn cli_with_isolation(account: &Path, source: &Path, args: &[&str], explicit: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_airc"));
    if explicit {
        command.env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1");
    } else {
        command.env_remove("AIRC_DISABLE_ACCOUNT_REGISTRY");
    }
    hidden(&mut command)
        .args(["--home"])
        .arg(account.join(".airc"))
        .args(args)
        .env("HOME", account)
        .env("USERPROFILE", account)
        .env("AIRC_DIR", source)
        .env("AIRC_RUNTIME_DIR", account.join(".airc/runtime"))
        .env("AIRC_UPDATE_CHANNEL", "canary")
        .env("AIRC_NO_STALENESS", "1")
        .output()
        .unwrap()
}
fn git(source: &Path, args: &[&str]) {
    let out = hidden(&mut Command::new("git"))
        .arg("-C")
        .arg(source)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
struct Fixture {
    child: Child,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn fixture(socket: &Path, ready: &Path, mode: &str, sha: &str) -> Fixture {
    let child = hidden(&mut Command::new(std::env::current_exe().unwrap()))
        .args(["--exact", "ipc_owner_fixture", "--nocapture"])
        .env("UPDATE_FIXTURE_SOCKET", socket)
        .env("UPDATE_FIXTURE_READY", ready)
        .env("UPDATE_FIXTURE_MODE", mode)
        .env("UPDATE_FIXTURE_SHA", sha)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let fixture = Fixture { child };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "IPC fixture did not bind");
        std::thread::sleep(Duration::from_millis(20));
    }
    fixture
}
#[test]
fn ipc_owner_fixture() {
    let Some(socket) = std::env::var_os("UPDATE_FIXTURE_SOCKET") else {
        return;
    };
    let mode = std::env::var("UPDATE_FIXTURE_MODE").unwrap();
    let sha = std::env::var("UPDATE_FIXTURE_SHA").unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let listener = IpcListener::bind(&PathBuf::from(socket)).await.unwrap();
            #[cfg(windows)]
            if mode == "access-denied" {
                let IpcListener::Windows { ref pipe_name, .. } = listener;
                let name = pipe_name.clone();
                drop(listener);
                // An outbound-only server rejects the client's duplex open with
                // ERROR_ACCESS_DENIED. No ACL or machine security is modified.
                let _pipe = tokio::net::windows::named_pipe::ServerOptions::new()
                    .access_inbound(false)
                    .access_outbound(true)
                    .first_pipe_instance(true)
                    .create(name)
                    .unwrap();
                std::fs::write(std::env::var_os("UPDATE_FIXTURE_READY").unwrap(), "ready").unwrap();
                std::future::pending::<()>().await;
                return;
            }
            std::fs::write(std::env::var_os("UPDATE_FIXTURE_READY").unwrap(), "ready").unwrap();
            loop {
                let Ok(mut stream) = listener.accept().await else {
                    continue;
                };
                let Ok(Some(request)) = read_frame::<_, Request>(&mut stream).await else {
                    continue;
                };
                if mode == "timeout" {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
                let response = if mode == "wrong-response" {
                    Response::Pong
                } else {
                    match request {
                        Request::Status => Response::Status(StatusResponse {
                            peer_id: "fixture".into(),
                            uptime_seconds: 100,
                            ipc_protocol_version: Some(1),
                            build_commit: Some(sha.clone()),
                            build_branch: Some("canary".into()),
                            executable: None,
                            connected_lan_peers: 0,
                            connections: None,
                        }),
                        Request::Stop if mode == "stale" => {
                            let _ = write_frame(&mut stream, &Response::Ok).await;
                            return;
                        }
                        // A current owner must never receive stop or a mutating request.
                        _ => panic!("unexpected updater mutation: {request:?}"),
                    }
                };
                let _ = write_frame(&mut stream, &response).await;
            }
        });
}
#[test]
fn public_update_verifies_current_owner_and_preserves_stopped_state() {
    let temp = common::daemon_tempdir();
    let account = temp.path().join("account");
    std::fs::create_dir_all(account.join(".airc")).unwrap();
    let source = temp.path().join("source");
    let version = cli(&account, &source, &["version"]);
    assert!(version.status.success());
    let text = String::from_utf8(version.stdout).unwrap();
    let sha = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("build:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let head = hidden(&mut Command::new("git"))
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(head.status.success());
    assert!(
        String::from_utf8_lossy(&head.stdout)
            .trim()
            .starts_with(sha),
        "refusing stale integration binary: {text}"
    );
    println!("Testing freshly built updater: {text}");
    let out = hidden(&mut Command::new("git"))
        .args(["clone", "--shared", "--no-checkout"])
        .arg(&repo)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(&source, &["checkout", "-B", "canary", sha]);
    git(
        &source,
        &["remote", "set-url", "origin", source.to_str().unwrap()],
    );
    let endpoint = cli(&account, &source, &["ipc-endpoint"]);
    assert!(endpoint.status.success());
    let socket = PathBuf::from(String::from_utf8(endpoint.stdout).unwrap().trim());
    for args in [&["update"][..], &["update", "--auto"][..]] {
        let out = cli(&account, &source, args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("left stopped"));
        assert!(!cli(&account, &source, &["ping"]).status.success());
    }
    let ready = temp.path().join("ready");
    let mut owner = fixture(&socket, &ready, "current", sha);
    for args in [
        &["update"][..],
        &["update", "--auto"][..],
        &["update", "--adopt-installed"][..],
    ] {
        let out = cli(&account, &source, args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            owner.child.try_wait().unwrap().is_none(),
            "healthy owner was replaced"
        );
    }
    drop(owner);
    for args in [&["update"][..], &["update", "--auto"][..]] {
        #[cfg(unix)]
        let _ = std::fs::remove_file(&socket);
        std::fs::remove_file(&ready).unwrap();
        std::fs::write(account.join(".airc/airc-daemon.log"), "").unwrap();
        let mut stale = fixture(&socket, &ready, "stale", "000000000000");
        // Exercise both established isolation contracts: explicit disable and
        // temp-home alone. Neither may open a LAN listener during adoption.
        let out = cli_with_isolation(&account, &source, args, args.len() == 1);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stale.child.try_wait().unwrap().is_some(),
            "stale owner survived adoption"
        );
        let status = cli(&account, &source, &["status"]);
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert!(String::from_utf8_lossy(&status.stdout).contains(sha));
        let log_path = account.join(".airc/airc-daemon.log");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            let reason = if args.len() == 1 {
                "AIRC_DISABLE_ACCOUNT_REGISTRY"
            } else {
                "temp-rooted"
            };
            if log.contains("automatic listener acquisition disabled") && log.contains(reason) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "isolated owner did not acknowledge route isolation: {log}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        #[cfg(windows)]
        {
            let pid = std::fs::read_to_string(account.join(".airc/daemon.pid")).unwrap();
            let output = hidden(&mut Command::new("netstat"))
                .args(["-ano"])
                .output()
                .unwrap();
            assert!(output.status.success());
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                assert_ne!(
                    line.split_whitespace().last(),
                    Some(pid.trim()),
                    "isolated owner unexpectedly opened a network socket: {line}"
                );
            }
        }
        assert!(cli(&account, &source, &["stop"]).status.success());
        let deadline = Instant::now() + Duration::from_secs(10);
        while cli(&account, &source, &["ping"]).status.success() {
            assert!(Instant::now() < deadline, "isolated owner failed to stop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // Explicit installation still starts an absent owner and is idempotent.
    for _ in 0..2 {
        let out = cli(&account, &source, &["update", "--adopt-installed"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(cli(&account, &source, &["stop"]).status.success());
}
#[test]
fn public_update_refuses_unknown_ipc_without_installing_or_starting() {
    let modes = if cfg!(windows) {
        vec!["wrong-response", "timeout", "access-denied"]
    } else {
        vec!["wrong-response", "timeout"]
    };
    for mode in modes {
        let temp = tempfile::tempdir().unwrap();
        let account = temp.path().join("account");
        std::fs::create_dir_all(account.join(".airc")).unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init"]);
        std::fs::write(source.join("install.sh"), "exit 98").unwrap();
        let endpoint = cli(&account, &source, &["ipc-endpoint"]);
        let socket = PathBuf::from(String::from_utf8(endpoint.stdout).unwrap().trim());
        let mut owner = fixture(&socket, &temp.path().join("ready"), mode, "old");
        let out = cli(&account, &source, &["update"]);
        assert!(!out.status.success());
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(
            error.contains("Cannot establish daemon state"),
            "{mode}: {error}"
        );
        if mode == "access-denied" {
            assert!(
                error.contains("os error 5"),
                "expected Windows AccessDenied: {error}"
            );
        }
        assert!(owner.child.try_wait().unwrap().is_none());
        assert!(!account.join(".airc/daemon.pid").exists());
    }
}

/// Real public manual/automatic updater, with a local installer fixture that
/// fails after copying its candidate. No package acquisition, UAC, or LAN.
#[test]
fn both_update_modes_restore_the_executing_binary_after_publication_failure() {
    let temp = common::daemon_tempdir();
    let account = temp.path().join("account");
    std::fs::create_dir_all(account.join(".airc")).unwrap();
    let source = temp.path().join("source");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let original = Path::new(env!("CARGO_BIN_EXE_airc"));
    let version = hidden(&mut Command::new(original))
        .arg("version")
        .output()
        .unwrap();
    assert!(version.status.success());
    let text = String::from_utf8(version.stdout).unwrap();
    let sha = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("build:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let head = hidden(&mut Command::new("git"))
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(head.status.success());
    assert!(
        String::from_utf8_lossy(&head.stdout)
            .trim()
            .starts_with(sha),
        "stale test binary: {text}"
    );
    println!("Testing publication rollback with {text}");
    let clone = hidden(&mut Command::new("git"))
        .args(["clone", "--shared", "--no-checkout"])
        .arg(&repo)
        .arg(&source)
        .output()
        .unwrap();
    assert!(clone.status.success());
    git(&source, &["checkout", "-B", "canary", sha]);
    git(
        &source,
        &["remote", "set-url", "origin", source.to_str().unwrap()],
    );
    std::fs::write(
        source.join("install.sh"),
        r#"#!/usr/bin/env bash
set -eu
case "$1" in
  --prepare-artifact) cp "$UPDATE_TEST_ORIGINAL" "$2" ;;
  --prebuilt)
    cp "$2" "$UPDATE_TEST_CURRENT"
    echo 'fixture installer failed after publication' >&2
    exit 23 ;;
  *) exit 91 ;;
esac
"#,
    )
    .unwrap();
    git(&source, &["add", "install.sh"]);
    git(
        &source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "isolated failing installer",
        ],
    );
    let current = temp
        .path()
        .join(format!("current{}", std::env::consts::EXE_SUFFIX));
    for args in [&["update"][..], &["update", "--auto"][..]] {
        std::fs::copy(original, &current).unwrap();
        let result = hidden(&mut Command::new(&current))
            .arg("--home")
            .arg(account.join(".airc"))
            .args(args)
            .env("HOME", &account)
            .env("USERPROFILE", &account)
            .env("AIRC_DIR", &source)
            .env("AIRC_RUNTIME_DIR", account.join(".airc/runtime"))
            .env("AIRC_UPDATE_CHANNEL", "canary")
            .env("AIRC_NO_STALENESS", "1")
            .env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1")
            .env("UPDATE_TEST_ORIGINAL", original)
            .env("UPDATE_TEST_CURRENT", &current)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            !result.status.success(),
            "failed installer must remain failure"
        );
        assert!(
            stderr.contains("fixture installer failed after publication"),
            "{stderr}"
        );
        assert!(stderr.contains("restored and verified"), "{stderr}");
        assert!(!stderr.contains("os error 32"), "{stderr}");
        assert_eq!(
            std::fs::read(&current).unwrap(),
            std::fs::read(original).unwrap()
        );
        let restored = hidden(&mut Command::new(&current))
            .arg("version")
            .output()
            .unwrap();
        assert!(restored.status.success());
        assert!(String::from_utf8_lossy(&restored.stdout).contains(sha));
        assert!(
            !cli(&account, &source, &["ping"]).status.success(),
            "stopped daemon must stay stopped"
        );
    }
}
