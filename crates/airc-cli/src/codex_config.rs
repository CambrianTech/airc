//! Codex TOML config mutation for AIRC hook installation.

use std::path::Path;

use toml_edit::{value, DocumentMut, Item, Table};

const INSTRUCTIONS_START: &str = "# AIRC-CODEX-INSTRUCTIONS-START";
const INSTRUCTIONS_END: &str = "# AIRC-CODEX-INSTRUCTIONS-END";

pub fn enable_hooks_feature(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let original = read_text(path)?;
    let without_legacy = remove_legacy_codex_hooks_key(&original)?;
    let mut doc = parse_toml_document(&without_legacy)?;
    let features = ensure_table(&mut doc, "features")?;
    let already_enabled = features
        .get("hooks")
        .and_then(Item::as_bool)
        .unwrap_or(false);
    if !already_enabled {
        features["hooks"] = value(true);
    }
    let rendered = doc.to_string();
    if rendered != original {
        write_text(path, &rendered)?;
        return Ok(true);
    }
    Ok(false)
}

pub fn disable_managed_hooks_feature(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let original = read_text(path)?;
    if original.is_empty() {
        return Ok(false);
    }
    let mut doc = parse_toml_document(&original)?;
    if let Some(features) = doc.get_mut("features").and_then(Item::as_table_mut) {
        features.remove("hooks");
        features.remove("codex_hooks");
        if features.is_empty() {
            doc.as_table_mut().remove("features");
        }
    }
    let rendered = doc.to_string();
    if rendered != original {
        write_text(path, &rendered)?;
        return Ok(true);
    }
    Ok(false)
}

pub fn remove_managed_developer_instructions(
    path: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    let original = read_text(path)?;
    if !original.contains(INSTRUCTIONS_START) {
        return Ok(false);
    }
    let mut out = Vec::new();
    let mut skipping = false;
    for line in original.lines() {
        if line.starts_with(INSTRUCTIONS_START) {
            skipping = true;
            continue;
        }
        if skipping {
            if line.starts_with(INSTRUCTIONS_END) {
                skipping = false;
            }
            continue;
        }
        out.push(line);
    }
    let rendered = out.join("\n").trim().to_string();
    write_text(path, &(rendered + "\n"))?;
    Ok(true)
}

fn parse_toml_document(text: &str) -> Result<DocumentMut, Box<dyn std::error::Error>> {
    if text.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    Ok(text.parse::<DocumentMut>()?)
}

fn ensure_table<'a>(
    doc: &'a mut DocumentMut,
    key: &str,
) -> Result<&'a mut Table, Box<dyn std::error::Error>> {
    let table = doc
        .as_table_mut()
        .entry(key)
        .or_insert_with(|| Item::Table(Table::new()));
    table
        .as_table_mut()
        .ok_or_else(|| format!("{key} exists but is not a table").into())
}

fn remove_legacy_codex_hooks_key(text: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut doc = parse_toml_document(text)?;
    if let Some(features) = doc.get_mut("features").and_then(Item::as_table_mut) {
        features.remove("codex_hooks");
    }
    Ok(doc.to_string())
}

fn read_text(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn write_text(path: &Path, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}
