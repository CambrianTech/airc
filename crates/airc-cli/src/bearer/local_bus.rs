use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
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

pub fn read_from(gist_id: &str, mut byte_offset: u64) -> (Vec<String>, u64) {
    let path = path_for(gist_id);
    let Ok(mut file) = fs::File::open(path) else {
        return (Vec::new(), byte_offset);
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if byte_offset > size {
        byte_offset = 0;
    }
    if file.seek(SeekFrom::Start(byte_offset)).is_err() {
        return (Vec::new(), byte_offset);
    }
    let mut chunk = Vec::new();
    if file.read_to_end(&mut chunk).is_err() || chunk.is_empty() {
        return (Vec::new(), byte_offset);
    }
    let Some(last_newline) = chunk.iter().rposition(|byte| *byte == b'\n') else {
        return (Vec::new(), byte_offset);
    };
    let complete = &chunk[..=last_newline];
    let Ok(text) = String::from_utf8(complete.to_vec()) else {
        return (Vec::new(), byte_offset);
    };
    (
        text.lines().map(str::to_string).collect(),
        byte_offset + complete.len() as u64,
    )
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
