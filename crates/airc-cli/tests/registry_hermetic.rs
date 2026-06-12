//! Card d793c242 — the hermetic gate, walked through the REAL CLI
//! surfaces a test/temp daemon actually uses.
//!
//! Live evidence: a hermetic Windows test daemon (scope_home
//! `C:\Users\green\AppData\Local\Temp\tmp.YYavgmVUxz\.airc`) inherited
//! the operator's gh auth and published itself to the production
//! joelteply rendezvous (gist 1214fb43d2c00d667c4712e6023b2165). These
//! tests pin that NEITHER the daemon's registry loop NOR the manual
//! `airc registry sync` verb can reach gh from a hermetic scope — and
//! that every suppression is LOUD.
//!
//! gh is replaced by a recording stub on PATH + AIRC_GH_BIN, so even a
//! gate regression cannot touch the real rendezvous from here (a
//! counting stub is the proof medium: zero gist calls = nothing
//! published). Unix-only: the stub is a shell script. Hosted CI runs
//! ubuntu + macos, so the gate is pinned on both.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn airc_bin() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

/// Recording gh stub: every invocation appends argv to `calls.log`;
/// `auth status` reports success so any auth-gate that is reached
/// would PROCEED — proving a quiet log means the hermetic gate fired,
/// not that auth happened to fail.
const STUB_SCRIPT: &str = r#"#!/bin/sh
S="__STATE__"
printf '%s\n' "$*" >> "$S/calls.log"
case "$1" in
  auth) exit 0 ;;
  gist)
    if [ "$2" = "create" ]; then
      cat > /dev/null
      echo "https://gist.github.com/stub/stubgist1"
    elif [ "$2" = "edit" ]; then
      cat > /dev/null
    fi
    ;;
  api) : ;;
esac
exit 0
"#;

struct StubGh {
    dir: PathBuf,
    state: PathBuf,
}

impl StubGh {
    fn install(root: &Path) -> Self {
        let dir = root.join("gh-stub-bin");
        let state = root.join("gh-stub-state");
        std::fs::create_dir_all(&dir).expect("stub bin dir");
        std::fs::create_dir_all(&state).expect("stub state dir");
        let bin = dir.join("gh");
        std::fs::write(
            &bin,
            STUB_SCRIPT.replace("__STATE__", &state.to_string_lossy()),
        )
        .expect("write stub gh");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub gh");
        Self { dir, state }
    }

    fn bin(&self) -> PathBuf {
        self.dir.join("gh")
    }

    fn path_env(&self) -> String {
        format!(
            "{}:{}",
            self.dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.state.join("calls.log"))
            .map(|log| log.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// The pollution-relevant slice of the log: anything that writes
    /// or reads gists. (Identity resolution may legitimately probe
    /// `gh api user` — that is not the account rendezvous.)
    fn rendezvous_calls(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|line| line.starts_with("gist ") || line.contains("/gists"))
            .collect()
    }
}

fn sync_command(home: &Path, stub: &StubGh) -> Command {
    let mut command = Command::new(airc_bin());
    command
        .arg("--home")
        .arg(home)
        .arg("registry")
        .arg("sync")
        .env("PATH", stub.path_env())
        .env("AIRC_GH_BIN", stub.bin())
        .stdin(Stdio::null());
    command
}

/// MUTATION PIN (a): drop the env-gate honor and this fails — with the
/// env var set, the refusal must name AIRC_DISABLE_ACCOUNT_REGISTRY
/// (the env check precedes the temp check by contract), and the stub
/// must record zero rendezvous calls.
#[test]
fn registry_sync_honors_disable_env_and_publishes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubGh::install(tmp.path());
    let home = tmp.path().join("scope/.airc");
    std::fs::create_dir_all(&home).expect("home");

    let output = sync_command(&home, &stub)
        .env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1")
        .output()
        .expect("airc registry sync runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "hermetic skip is a clean exit, got {:?}: {stdout}",
        output.status
    );
    assert!(
        stdout.contains("REFUSED"),
        "the skip must be loud, got: {stdout}"
    );
    assert!(
        stdout.contains("AIRC_DISABLE_ACCOUNT_REGISTRY"),
        "the refusal must name the env gate (not the temp gate), got: {stdout}"
    );
    assert_eq!(
        stub.rendezvous_calls(),
        Vec::<String>::new(),
        "the gh rendezvous must never be touched"
    );
}

/// MUTATION PIN (b): drop the temp-dir refusal and this fails — a
/// temp-rooted home with NO env var and a SUCCEEDING gh auth stub must
/// still refuse, loudly, with zero rendezvous calls.
#[test]
fn registry_sync_refuses_temp_home_even_without_env() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubGh::install(tmp.path());
    let home = tmp.path().join("scope/.airc");
    std::fs::create_dir_all(&home).expect("home");

    let output = sync_command(&home, &stub)
        .env_remove("AIRC_DISABLE_ACCOUNT_REGISTRY")
        .output()
        .expect("airc registry sync runs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "hermetic skip is a clean exit");
    assert!(
        stdout.contains("REFUSED") && stdout.contains("temp-rooted"),
        "temp-rooted home must refuse loudly, got: {stdout}"
    );
    assert_eq!(
        stub.rendezvous_calls(),
        Vec::<String>::new(),
        "the gh rendezvous must never be touched"
    );
}

struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_daemon(home: &Path, socket: &Path, stub: &StubGh, disable_env: bool) -> DaemonGuard {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("daemon-test.log"))
        .expect("daemon log");
    let stderr = log.try_clone().expect("clone log");
    let mut command = Command::new(airc_bin());
    command
        .arg("--home")
        .arg(home)
        .arg("daemon")
        .arg("--socket")
        .arg(socket)
        .env("PATH", stub.path_env())
        .env("AIRC_GH_BIN", stub.bin())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    if disable_env {
        command.env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1");
    } else {
        command.env_remove("AIRC_DISABLE_ACCOUNT_REGISTRY");
    }
    DaemonGuard {
        child: command.spawn().expect("daemon must spawn"),
    }
}

fn wait_for_log(home: &Path, needle: &str, budget: Duration) -> String {
    let log_path = home.join("daemon-test.log");
    let deadline = Instant::now() + budget;
    loop {
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        if log.contains(needle) {
            return log;
        }
        if Instant::now() >= deadline {
            panic!("daemon log never contained {needle:?} within {budget:?}; log:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The DAEMON path of the gate: a hermetic daemon with the env var set
/// disables its account-registry loop at startup with ONE loud line
/// naming the env gate, and never touches gh.
#[test]
fn daemon_with_disable_env_never_runs_registry_loop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubGh::install(tmp.path());
    let home = tmp.path().join("scope/.airc");
    std::fs::create_dir_all(&home).expect("home");
    let socket = tmp.path().join("d.sock");

    let _daemon = spawn_daemon(&home, &socket, &stub, true);
    let log = wait_for_log(
        &home,
        "account-registry loop DISABLED",
        Duration::from_secs(30),
    );

    assert!(
        log.contains("AIRC_DISABLE_ACCOUNT_REGISTRY"),
        "the disable line must name the env gate, got:\n{log}"
    );
    assert_eq!(
        stub.rendezvous_calls(),
        Vec::<String>::new(),
        "a hermetic daemon must never touch the gh rendezvous"
    );
}

/// Defense in depth on the daemon path: even WITHOUT the env var, a
/// temp-rooted home (exactly the live-evidence shape) disables the
/// loop loudly.
#[test]
fn daemon_with_temp_home_never_runs_registry_loop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let stub = StubGh::install(tmp.path());
    let home = tmp.path().join("scope/.airc");
    std::fs::create_dir_all(&home).expect("home");
    let socket = tmp.path().join("d2.sock");

    let _daemon = spawn_daemon(&home, &socket, &stub, false);
    let log = wait_for_log(
        &home,
        "account-registry loop DISABLED",
        Duration::from_secs(30),
    );

    assert!(
        log.contains("temp-rooted"),
        "the disable line must name the temp gate, got:\n{log}"
    );
    assert_eq!(
        stub.rendezvous_calls(),
        Vec::<String>::new(),
        "a hermetic daemon must never touch the gh rendezvous"
    );
}
