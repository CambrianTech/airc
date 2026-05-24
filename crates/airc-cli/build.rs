//! Bake git commit + branch into the airc binary at compile time.
//!
//! Closes work card 38c295b8 (installed runtime convergence)
//! prerequisite: `airc doctor` / `airc version` need a way to detect
//! when the installed binary's source matches the current checkout
//! vs has drifted. The pre-card `doctor.rs` had a TODO comment
//! noting this exact gap: "we don't have a reliable way to compare
//! to a canonical 'current' build here without baking commit
//! metadata into the binary."
//!
//! Outputs three compile-time env vars consumable via `env!()`:
//! - `AIRC_BUILD_COMMIT` — full SHA of `HEAD` at compile time, or
//!   the literal string `unknown` if git wasn't available.
//! - `AIRC_BUILD_COMMIT_SHORT` — 12-char short form for display.
//! - `AIRC_BUILD_BRANCH` — branch name at compile time, or `unknown`.
//!
//! The build is **not** re-run when source files change unless the
//! `.git/HEAD` file does — that's `cargo:rerun-if-changed=.git/HEAD`.
//! When git isn't present (e.g. building from a release tarball)
//! the constants fall back to `unknown` rather than failing the
//! build.

use std::process::Command;

fn main() {
    let commit = git(["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let commit_short =
        git(["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let branch =
        git(["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=AIRC_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=AIRC_BUILD_COMMIT_SHORT={commit_short}");
    println!("cargo:rustc-env=AIRC_BUILD_BRANCH={branch}");
    // Re-run when HEAD moves; tells cargo not to rerun on every
    // source change.
    println!("cargo:rerun-if-changed=.git/HEAD");
    // .git/HEAD is a single-line file in a normal checkout that
    // points at refs/heads/<branch>; the branch ref then holds the
    // commit. Watch both so a commit on the current branch also
    // triggers a rebuild.
    println!("cargo:rerun-if-changed=.git/refs/heads/");
}

fn git<I, S>(args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
