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
    // Joel's ruling 2026-08-08: versions must ALWAYS auto-increment and display
    // with the sha, in EVERY repo, on every connection/health/query surface —
    // stale binaries have repeatedly poisoned testing. A sha NAMES a build but
    // does not ORDER it, so "am I newer than that peer?" was unanswerable
    // without a git checkout to compare against. The commit count does order
    // it: squash-merge keeps canary linear, so deploys totally order, and an
    // equal number with a different sha is a loud divergence rather than a
    // quiet one.
    let build_number = git(["rev-list", "--count", "HEAD"]).unwrap_or_else(|| "0".to_string());
    println!("cargo:rustc-env=AIRC_BUILD_NUMBER={build_number}");
    // Third leg: WHEN this binary was compiled. Number orders the source, sha
    // names the source, built-at catches what both miss — a binary rebuilt from
    // OLD source after a fix landed, where number and sha both look plausible
    // and only the timestamp says the binary predates the fix.
    //
    // Computed from `SystemTime`, deliberately NOT by shelling out to `date`.
    // `date -u +FORMAT` is coreutils; on Windows `date` is a cmd builtin with
    // different semantics entirely, and a spawn only finds a compatible one if
    // Git-for-Windows' `usr\bin` happens to be on PATH. Where it is not, the
    // timestamp silently degrades to "unknown" — losing the third leg on the
    // platform where stale binaries have bitten hardest, and losing it QUIETLY,
    // which is the whole failure mode this trio exists to end. No process, no
    // PATH assumption, identical on every platform.
    println!("cargo:rustc-env=AIRC_BUILD_AT={}", built_at_utc());
    // Re-run when HEAD moves; tells cargo not to rerun on every
    // source change.
    println!("cargo:rerun-if-changed=.git/HEAD");
    // .git/HEAD is a single-line file in a normal checkout that
    // points at refs/heads/<branch>; the branch ref then holds the
    // commit. Watch both so a commit on the current branch also
    // triggers a rebuild.
    println!("cargo:rerun-if-changed=.git/refs/heads/");
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`, computed from the system clock
/// with no external process.
///
/// The civil-date conversion is Howard Hinnant's `civil_from_days`, the same
/// algorithm every date library uses: shift the epoch to 0000-03-01 so leap day
/// lands at the end of the cycle, then do exact integer arithmetic over the
/// 400-year Gregorian cycle. Correct for any date this program will ever see,
/// and it cannot fail — which is the point, since a timestamp that can silently
/// become "unknown" defeats the staleness check it exists for.
fn built_at_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // days since 1970-01-01 → civil date
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
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
