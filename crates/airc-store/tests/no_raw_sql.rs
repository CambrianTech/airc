//! Ratchet: no raw SQL anywhere in airc. Data goes through sea_orm entities and
//! typed sea_query expressions; the ONE exception is the SQLite-only adapter
//! function `sqlite_only` in `airc-store/src/sqlite_only.rs` (pragmas, the
//! vacuum family, `EXPLAIN QUERY PLAN`), which has no ORM expression.

use std::path::{Path, PathBuf};

/// The adapter that may hold SQL text, and the function inside it that may.
const ADAPTER_FILE: &str = "airc-store/src/sqlite_only.rs";
const ADAPTER_FN: &str = "async fn sqlite_only(";

/// Raw-SQL markers, assembled from pieces so this file does not match itself.
fn markers() -> Vec<String> {
    let q = '"';
    let mut found: Vec<String> = [
        ["Statement::", "from_string"],
        ["Statement::", "from_sql_and_values"],
        ["execute_", "unprepared"],
        ["Expr::", "cust"],
        ["Func::", "cust"],
        ["cust_", "with_values"],
        ["SimpleExpr::", "Custom"],
        ["json_", "each"],
        ["json_", "extract"],
        ["json_", "type("],
        ["sqlx::", "query"],
        ["rus", "qlite"],
        ["PRA", "GMA "],
        ["EXPLAIN ", "QUERY"],
    ]
    .iter()
    .map(|[a, b]| format!("{a}{b}"))
    .collect();
    for [a, b] in [
        ["SEL", "ECT "],
        ["INSERT ", "INTO"],
        ["DELETE ", "FROM"],
        ["UPD", "ATE "],
        ["CREATE ", "TABLE"],
        ["CREATE ", "INDEX"],
        ["DROP ", "TABLE"],
        ["ALTER ", "TABLE"],
        ["VAC", "UUM"],
    ] {
        found.push(format!("{q}{a}{b}"));
    }
    found
}

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

// what this catches: raw SQL (strings, custom expressions, unprepared
// execution) creeping back into any crate outside the one SQLite-only adapter.
#[test]
fn no_raw_sql_outside_the_sqlite_only_adapter() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates directory");
    let mut files = Vec::new();
    rust_files(crates, &mut files);
    assert!(files.len() > 100, "scanned the workspace: {}", files.len());
    let markers = markers();
    let mut sites = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(crates)
            .expect("under crates")
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).expect("read source");
        let mut in_adapter = false;
        for (number, line) in text.lines().enumerate() {
            if relative == ADAPTER_FILE {
                if line.contains(ADAPTER_FN) {
                    in_adapter = true;
                } else if in_adapter && line == "}" {
                    in_adapter = false;
                    continue;
                }
            }
            if in_adapter || line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(marker) = markers.iter().find(|m| line.contains(m.as_str())) {
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
        "raw SQL outside `sqlite_only` (use entities and typed sea_query):\n{}",
        sites.join("\n")
    );
}
