//! Codex hooks.json mutation for AIRC hook installation.

use serde_json::{json, Value};
use std::path::Path;

const HOOK_COMMAND_SUFFIX: &str = "codex-hook user-prompt-submit";
const HOOK_COMMAND: &str = "airc codex-hook user-prompt-submit";
const POST_TOOL_COMMAND: &str = "airc codex-hook post-tool-use";
const HOOK_STATUS: &str = "Checking AIRC inbox";

pub fn install(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let original = read_json(path)?;
    let mut data = ensure_root_object(original);
    let mut changed = false;
    for (event, command) in [
        ("UserPromptSubmit", HOOK_COMMAND),
        ("PostToolUse", POST_TOOL_COMMAND),
    ] {
        let groups = ensure_hook_array(&mut data, event)?;
        let before = groups.clone();
        remove_managed_hook_entries(groups);
        groups.push(hook_group(command));
        changed |= *groups != before;
    }
    if changed {
        write_json(path, &data)?;
    }
    Ok(changed)
}

pub fn uninstall(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let original = read_json(path)?;
    let mut data = ensure_root_object(original);
    let mut changed = false;
    for event in ["UserPromptSubmit", "PostToolUse"] {
        if let Some(groups) = data
            .get_mut("hooks")
            .and_then(|hooks| hooks.get_mut(event))
            .and_then(Value::as_array_mut)
        {
            let before = groups.clone();
            remove_managed_hook_entries(groups);
            changed |= *groups != before;
        }
    }
    if changed {
        write_json(path, &data)?;
    }
    Ok(changed)
}

fn ensure_root_object(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({})
    }
}

fn ensure_hook_array<'a>(
    data: &'a mut Value,
    event: &str,
) -> Result<&'a mut Vec<Value>, Box<dyn std::error::Error>> {
    if !data.is_object() {
        *data = json!({});
    }
    if data.get("hooks").and_then(Value::as_object).is_none() {
        data["hooks"] = json!({});
    }
    if data["hooks"].get(event).and_then(Value::as_array).is_none() {
        data["hooks"][event] = json!([]);
    }
    data["hooks"][event]
        .as_array_mut()
        .ok_or_else(|| "hooks.UserPromptSubmit is not an array".into())
}

fn remove_managed_hook_entries(groups: &mut Vec<Value>) {
    groups.retain_mut(|group| {
        let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        hooks.retain(|hook| {
            let command = hook.get("command").and_then(Value::as_str);
            !is_managed_hook_command(command)
        });
        !hooks.is_empty()
    });
}

fn is_managed_hook_command(command: Option<&str>) -> bool {
    let Some(command) = command else {
        return false;
    };
    command == HOOK_COMMAND
        || command.ends_with(HOOK_COMMAND_SUFFIX)
        || command == POST_TOOL_COMMAND
        || command.ends_with("codex-hook post-tool-use")
}

fn hook_group(command: &str) -> Value {
    json!({
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": 5,
            "statusMessage": HOOK_STATUS
        }]
    })
}

fn read_json(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(error.into()),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_tool_install_is_idempotent_and_uninstall_preserves_other_hooks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hooks.json");
        write_json(&path, &json!({"hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "other-hook"}]}]}})).unwrap();
        assert!(install(&path).unwrap());
        assert!(!install(&path).unwrap());
        let installed = read_json(&path).unwrap();
        assert_eq!(
            installed["hooks"]["PostToolUse"][1]["hooks"][0]["command"],
            POST_TOOL_COMMAND
        );
        assert!(uninstall(&path).unwrap());
        assert!(!uninstall(&path).unwrap());
        let removed = read_json(&path).unwrap();
        assert_eq!(removed["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            removed["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "other-hook"
        );
    }
}
