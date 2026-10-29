//! Ratchet: every store open and every write goes through airc-store's
//! admission (`custody`). Outside airc-store's own source no crate connects to
//! SQLite itself, and only `airc-lib/src/store_admission.rs` decides how a
//! process's writes are admitted (installs the resolver, names an Admission).

use std::path::{Path, PathBuf};

const STORE_SRC: &str = "airc-store/src/";
const ADMISSION_OWNER: &str = "airc-lib/src/store_admission.rs";

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

// what this catches: a store opened or written around the admission (a raw
// SQLite connection, or a crate choosing its own Admission), which would let a
// write land during an update or under an obsolete schema.
#[test]
fn no_store_open_or_admission_outside_their_owners() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory");
    let mut files = Vec::new();
    rust_files(crates, &mut files);
    assert!(files.len() > 100, "scanned the workspace: {}", files.len());
    // Assembled from pieces so this file does not match itself.
    let connect = [
        format!("Database::{}", "connect"),
        format!("SqliteConnect{}", "Options"),
        format!("sqlx::{}", "Sqlite"),
    ];
    let admission = [
        format!("install_{}(", "resolver"),
        format!("Admission{}", "::"),
        format!("WriteGate{}", "::"),
    ];
    let mut sites = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(crates)
            .expect("under crates")
            .to_string_lossy()
            .replace('\\', "/");
        if relative.starts_with(STORE_SRC) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read source");
        for (number, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            let bypass = connect
                .iter()
                .find(|m| line.contains(m.as_str()))
                .or_else(|| {
                    (relative != ADMISSION_OWNER)
                        .then(|| admission.iter().find(|m| line.contains(m.as_str())))
                        .flatten()
                });
            if let Some(marker) = bypass {
                sites.push(format!(
                    "{relative}:{}: {marker}: {}",
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        sites.is_empty(),
        "store access outside airc-store's admission:\n{}",
        sites.join("\n")
    );
}
