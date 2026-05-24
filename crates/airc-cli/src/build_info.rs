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
/// Cargo package version (semver).
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether build info was actually captured (vs. fell back to
/// `unknown` because git wasn't available at compile time).
pub fn is_unknown() -> bool {
    COMMIT == "unknown"
}
