//! Compile-time build info baked in by `build.rs`.
//!
//! These constants are populated by `build.rs` via
//! `cargo:rustc-env=AIRC_BUILD_*`. When git isn't available at
//! compile time (e.g. release tarball builds) the constants hold
//! the literal string `unknown`.

/// Full git commit SHA at compile time, or `unknown`.
pub const COMMIT: &str = env!("AIRC_BUILD_COMMIT");
/// 12-char short commit for compact display, or `unknown`.
pub const COMMIT_SHORT: &str = env!("AIRC_BUILD_COMMIT_SHORT");
/// Git branch at compile time, or `unknown`.
pub const BRANCH: &str = env!("AIRC_BUILD_BRANCH");
/// Auto-incrementing build number — `git rev-list --count HEAD` at compile
/// time, or `0` when git was unavailable.
///
/// A sha NAMES a build; it does not ORDER one. Without this, "is that peer
/// newer than me?" needed a git checkout to answer, so nobody asked and stale
/// binaries kept poisoning tests. Canary is squash-merged and therefore linear,
/// so these totally order — and an equal number with a DIFFERENT sha is a loud
/// divergence rather than a quiet one.
pub const BUILD_NUMBER: &str = env!("AIRC_BUILD_NUMBER");
/// UTC compile timestamp, `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Catches what number and sha both miss: a binary rebuilt from OLD source
/// after a fix landed, where both look plausible and only the timestamp says
/// the binary predates the fix.
pub const BUILT_AT: &str = env!("AIRC_BUILD_AT");

/// One-line version identity for any surface that reports what it is running:
/// `#1234 a1b2c3d4e5f6 (canary) built 2026-08-08T13:00:00Z`.
///
/// ONE formatter, so connection banners, health output and version queries
/// cannot drift into describing the same binary three different ways — which is
/// exactly how a stale node reads as current on one surface and stale on
/// another.
pub fn version_line() -> String {
    format!("#{BUILD_NUMBER} {COMMIT_SHORT} ({BRANCH}) built {BUILT_AT}")
}
/// Cargo package version (semver).
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether build info was actually captured (vs. fell back to
/// `unknown` because git wasn't available at compile time).
pub fn is_unknown() -> bool {
    COMMIT == "unknown"
}

pub fn is_unknown_or_matches(commit: Option<&str>) -> bool {
    is_unknown() || commit == Some(COMMIT)
}
