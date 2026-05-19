//! Codex hooks.json mutation for AIRC hook installation.

use std::path::Path;

use serde_json::{json, Value};

const RUST_HOOK_COMMAND: &str = "airc-rs codex-hook user-prompt-submit";
const LEGACY_HOOK_COMMAND: &str = "airc codex-hook user-prompt-submit";
const HOOK_STATUS: &str = "Checking AIRC inbox";

pub fn install(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let original = read_json(path)?;
    let mut data = ensure_root_object(original);
    let user_prompt = ensure_user_prompt_array(&mut data)?;
    let before = user_prompt.clone();
    remove_managed_hook_entries(user_prompt);
    user_prompt.push(hook_group());

    let changed = *user_prompt != before;
    if changed {
        write_json(path, &data)?;
    }
    Ok(changed)
}

pub fn uninstall(path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let original = read_json(path)?;
    let mut data = ensure_root_object(original);
    let Some(user_prompt) = data
        .pointer_mut("/hooks/UserPromptSubmit")
        .and_then(Value::as_array_mut)
    else {
        return Ok(false);
    };
    let before = user_prompt.clone();
    remove_managed_hook_entries(user_prompt);
    let changed = *user_prompt != before;
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

fn ensure_user_prompt_array(
    data: &mut Value,
) -> Result<&mut Vec<Value>, Box<dyn std::error::Error>> {
    if !data.is_object() {
        *data = json!({});
    }
    if data.get("hooks").and_then(Value::as_object).is_none() {
        data["hooks"] = json!({});
    }
    if data["hooks"]
        .get("UserPromptSubmit")
        .and_then(Value::as_array)
        .is_none()
    {
        data["hooks"]["UserPromptSubmit"] = json!([]);
    }
    data["hooks"]["UserPromptSubmit"]
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
            command != Some(RUST_HOOK_COMMAND) && command != Some(LEGACY_HOOK_COMMAND)
        });
        !hooks.is_empty()
    });
}

fn hook_group() -> Value {
    json!({
        "hooks": [{
            "type": "command",
            "command": RUST_HOOK_COMMAND,
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
