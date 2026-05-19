use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const MESSAGES_FILE: &str = "messages.jsonl";

pub fn append_batch(gist_id: &str, payloads: &[String]) -> (bool, String) {
    let mut ok = true;
    let mut detail = String::new();
    for payload in payloads {
        let (line_ok, line_detail) = append(gist_id, payload);
        if !line_ok {
            ok = false;
            detail = line_detail;
        }
    }
    (ok, detail)
}

fn append(gist_id: &str, line: &str) -> (bool, String) {
    if truthy(env::var("AIRC_DISABLE_LOCAL_BUS").ok().as_deref()) {
        return (false, "local bus disabled".to_string());
    }
    let path = path_for(gist_id);
    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return (false, format!("local bus mkdir failed: {error}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
            }
            if let Err(error) = file.write_all(line.as_bytes()) {
                return (false, format!("local bus append failed: {error}"));
            }
            (true, String::new())
        }
        Err(error) => (false, format!("local bus append failed: {error}")),
    }
}

fn path_for(gist_id: &str) -> PathBuf {
    env::var_os("AIRC_LOCAL_BUS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".airc").join("bus").join("gh"))
        .join(safe_gist_id(gist_id))
        .join(MESSAGES_FILE)
}

fn user_home() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn safe_gist_id(gist_id: &str) -> String {
    gist_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn truthy(value: Option<&str>) -> bool {
    matches!(
        value.map(|raw| raw.trim().to_ascii_lowercase()),
        Some(raw) if matches!(raw.as_str(), "1" | "true" | "yes" | "on")
    )
}
