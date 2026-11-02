//! Locate persisted snapshots in isolated projection fixtures without copying
//! production format numbers or namespace layout into each consumer test.
use std::path::{Path, PathBuf};

pub fn only_snapshot(root: &Path) -> PathBuf {
    fn collect(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("projection cache directory") {
            let entry = entry.expect("cache entry");
            let kind = entry.file_type().expect("cache entry type");
            if kind.is_dir() {
                collect(&entry.path(), found);
            } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "json") {
                found.push(entry.path());
            }
        }
    }
    let mut found = Vec::new();
    collect(root, &mut found);
    assert_eq!(
        found.len(),
        1,
        "expected one fixture snapshot under {}",
        root.display()
    );
    found.pop().unwrap()
}
