use sha1::{Digest, Sha1};

pub fn default_scope() -> String {
    if let Some(home) = std::env::var_os("AIRC_HOME") {
        return home.to_string_lossy().into_owned();
    }
    if let Some(home) = std::env::var_os("HOME") {
        return format!("{}/.airc", home.to_string_lossy());
    }
    ".airc".to_string()
}

pub fn scope_id(scope: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(scope.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_id_matches_legacy_sha1_prefix() {
        assert_eq!(scope_id("/tmp/airc"), "dcb77ec809c5");
    }

    #[test]
    fn scope_id_is_stable_and_distinguishes_scopes() {
        assert_eq!(scope_id("/tmp/airc"), scope_id("/tmp/airc"));
        assert_ne!(scope_id("/tmp/airc"), scope_id("/tmp/airc-other"));
    }
}
