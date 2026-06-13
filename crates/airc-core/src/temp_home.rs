//! Temp-rooted scope-home detection (#1150, card d793c242).
//!
//! A scope home rooted under a temp directory is the signature of a
//! hermetic test / CI daemon — NEVER a production scope. This single
//! definition is consulted by every layer that must treat such scopes
//! differently:
//!
//!   - `airc-lib`'s account-registry publish/refresh gates (the
//!     original #1150 consumer — temp daemons must never publish to
//!     the production gh rendezvous).
//!   - `airc-daemon`'s temp-home idle self-exit watchdog (card
//!     f122b5b5 — a daemon whose home is temp-rooted exits on its own
//!     after an idle window so killed test runners can't strand it).
//!
//! It lives in `airc-core` (std-only, no deps) because both crates
//! above need it and `airc-daemon` must not depend on `airc-lib`.

use std::path::Path;

/// True when `scope_home` is rooted under a temp directory — the
/// signature of a hermetic test / CI daemon, NEVER a production scope.
///
/// Two layers, because this is consulted on BOTH sides of the wire:
///
/// 1. **Local resolution** (publish gate / self-exit policy):
///    canonicalized-prefix match against this machine's
///    `std::env::temp_dir()` — the same check `machine_account_home`
///    uses for state isolation (card b0a81c31).
/// 2. **Cross-platform markers** (reader hygiene): a beacon read from
///    the rendezvous carries a `scope_home` minted on a DIFFERENT
///    machine/OS, where our local `temp_dir()` prefix is meaningless.
///    Recognize the well-known temp roots of every platform airc runs
///    on: live evidence for card d793c242 was a beacon with scope_home
///    `C:\Users\green\AppData\Local\Temp\tmp.YYavgmVUxz\.airc` published
///    to the production joelteply rendezvous.
pub fn scope_home_is_temp_rooted(scope_home: &Path) -> bool {
    // Layer 1: this machine's temp root (canonicalize both sides so
    // macOS's `/tmp` -> `/private/tmp` symlink can't dodge the check).
    let temp = std::env::temp_dir();
    let temp = temp.canonicalize().unwrap_or(temp);
    let resolved = scope_home
        .canonicalize()
        .unwrap_or_else(|_| scope_home.to_path_buf());
    if resolved.starts_with(&temp) {
        return true;
    }

    // Layer 2: cross-platform markers, for paths minted elsewhere.
    // Normalize separators + case so `C:\Users\…\AppData\Local\Temp\…`
    // and `/tmp/…` both land in one comparison space.
    let lossy = scope_home
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    lossy == "/tmp"
        || lossy.starts_with("/tmp/")
        || lossy.starts_with("/private/tmp/")
        || lossy.starts_with("/var/folders/")
        || lossy.starts_with("/private/var/folders/")
        || lossy.contains("/appdata/local/temp/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn local_temp_dir_is_temp_rooted() {
        let scope = std::env::temp_dir().join("airc-test-scope/.airc");
        assert!(scope_home_is_temp_rooted(&scope));
    }

    #[test]
    fn cross_platform_markers_are_temp_rooted() {
        for marker in [
            "/tmp/x/.airc",
            "/private/tmp/x/.airc",
            "/var/folders/zz/abc/T/.tmpX/.airc",
            "/private/var/folders/zz/abc/T/.tmpX/.airc",
            "C:\\Users\\green\\AppData\\Local\\Temp\\tmp.Y\\.airc",
        ] {
            assert!(
                scope_home_is_temp_rooted(&PathBuf::from(marker)),
                "{marker} must be recognized as temp-rooted"
            );
        }
    }

    #[test]
    fn production_homes_are_not_temp_rooted() {
        for home in [
            "/Users/jane/.airc",
            "/Users/jane/Development/airc/.airc",
            "/home/runner/work/airc/airc/.airc",
            "C:\\Users\\green\\.airc",
        ] {
            assert!(
                !scope_home_is_temp_rooted(&PathBuf::from(home)),
                "{home} must NOT be recognized as temp-rooted"
            );
        }
    }
}
