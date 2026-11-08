//! A card clone's history, completed when a caller must prove ancestry in it.
//!
//! A clone of a shallow mirror is shallow, and in a shallow clone git's "not an ancestor" can mean
//! only that history is missing. Kimi on the Windows node, 2026-11-08 (continuum f76005e8): her
//! clone was shallow, so whether her creation anchor was an ancestor of HEAD could not be proven,
//! and nothing could safely put her checkout back on it. A caller asks for the history only when
//! a clone is shallow and its ancestry question failed (Codex e302c1d0).

use super::{git_in, git_in_within, NETWORK_MIRROR_BOUND};
use std::path::Path;

/// Complete `path`'s history from its `origin`. A no-op on a complete clone. A full fetch of a
/// repository's history is the network transfer [`NETWORK_MIRROR_BOUND`] was measured for, so it
/// runs under that bound, never the routine one.
pub fn complete_card_history(path: &Path) -> Result<(), String> {
    if git_in(path, &["rev-parse", "--is-shallow-repository"])? != "true" {
        return Ok(());
    }
    git_in_within(
        path,
        &["fetch", "--quiet", "--unshallow", "--no-tags", "origin"],
        NETWORK_MIRROR_BOUND,
    )
    .map(|_| ())
}
