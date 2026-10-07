//! Exercise the shipped CLI with an inherited PATH that predates gh installation.
#![cfg(windows)]
mod common;

#[test]
fn sos_and_gh_share_hidden_discovery_and_authoritative_override() {
    let temp = common::daemon_tempdir();
    let local = temp.path().join("Local App Data");
    let directory = local.join("Programs/GitHub CLI/bin");
    std::fs::create_dir_all(&directory).unwrap();
    let source = temp.path().join("gh_fixture.rs");
    std::fs::write(&source, r#"
#[link(name="kernel32")]
unsafe extern "system" { fn GetConsoleWindow() -> *mut std::ffi::c_void; }
fn main() {
    assert!(unsafe { GetConsoleWindow() }.is_null(), "gh allocated a console");
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--version"] { println!("gh version isolated-fixture"); }
    else if args.iter().any(|a| a == "/gists?per_page=100") { println!("0123456789abcdef"); }
    else if args.iter().any(|a| a.ends_with("/comments")) { println!("[]"); }
    else if args.iter().any(|a| a == ".html_url") { println!("https://example.invalid/sos-fixture"); }
    else { panic!("unexpected command; no publication permitted: {args:?}"); }
}
"#).unwrap();
    let gh = directory.join("gh.exe");
    let built = airc_core::process::background("rustc")
        .args(["--edition=2021", "--crate-name", "gh_fixture"])
        .arg(&source)
        .arg("-o")
        .arg(&gh)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let home = temp.path().join("account");
    let run = |args: &[&str], override_path: Option<&std::path::Path>| {
        let mut command = airc_core::process::background(env!("CARGO_BIN_EXE_airc"));
        command
            .arg("--home")
            .arg(&home)
            .args(args)
            .env("PATH", temp.path().join("stale-path"))
            .env("LOCALAPPDATA", &local)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("AIRC_GH_AUDIT_LOG", temp.path().join("gh-audit.jsonl"))
            .env("AIRC_DISABLE_STALENESS_CHECK", "1")
            .env_remove("AIRC_GH_BIN");
        if let Some(path) = override_path {
            command.env("AIRC_GH_BIN", path);
        }
        command.output().unwrap()
    };
    let direct = run(&["gh", "run", "--", "--version"], None);
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert!(String::from_utf8_lossy(&direct.stdout).contains("isolated-fixture"));
    let sos = run(&["sos", "status"], None);
    assert!(
        sos.status.success(),
        "{}",
        String::from_utf8_lossy(&sos.stderr)
    );
    assert!(String::from_utf8_lossy(&sos.stdout).contains("https://example.invalid/sos-fixture"));
    let missing = temp.path().join("explicit missing gh.exe");
    let refused = run(&["sos", "status"], Some(&missing));
    assert!(
        !refused.status.success(),
        "broken override must not fall back"
    );
    assert!(String::from_utf8_lossy(&refused.stderr).contains("explicit missing gh.exe"));
    let invalid = temp.path().join("invalid gh.exe");
    std::fs::write(&invalid, b"not an executable").unwrap();
    assert!(!run(&["gh", "run", "--", "--version"], Some(&invalid))
        .status
        .success());
}
