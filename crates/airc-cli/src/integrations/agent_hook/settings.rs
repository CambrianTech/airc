//! Installing AIRC's hooks into a runtime's JSON hook configuration: Codex's
//! `hooks.json` and Claude Code's `settings.json` share the same `hooks` shape
//! (`{"hooks": {"<Event>": [{"matcher"?, "hooks": [{"type": "command", ...}]}]}}`).
//!
//! An entry is AIRC's when its command ends with one of the managed verbs
//! (`codex-hook user-prompt-submit`, `claude-hook post-tool-use`, ...): the command
//! is the mark, so a path-qualified install from an older release is recognized
//! too, and nothing else in the file is ever removed. The edit goes through the
//! JSON parser; every top-level key other than `hooks` keeps its position and its
//! original text. The file is replaced atomically, never left half-written.

use std::path::Path;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use serde_json::{json, Value};

/// Seconds a runtime waits for one hook call. Unchanged from the Codex install.
const HOOK_TIMEOUT_SECS: u64 = 5;

/// One hook AIRC installs.
pub struct ManagedHook {
    pub event: &'static str,
    /// The arguments after `airc`; also the suffix that marks an entry as AIRC's.
    pub args: &'static str,
    pub matcher: Option<&'static str>,
}

/// Every hook one runtime gets, and how its entries are labelled.
pub struct ManagedHooks {
    pub hooks: &'static [ManagedHook],
    pub status_message: Option<&'static str>,
}

/// Install (or refresh) the managed hooks. Returns whether the file changed.
pub fn install(path: &Path, spec: &ManagedHooks) -> Result<bool, Box<dyn std::error::Error>> {
    edit(path, spec, true)
}

/// Remove only the managed hooks; an event left empty by that is removed, and so
/// is `hooks` when nothing else remains in it. Returns whether the file changed.
pub fn uninstall(path: &Path, spec: &ManagedHooks) -> Result<bool, Box<dyn std::error::Error>> {
    edit(path, spec, false)
}

fn edit(
    path: &Path,
    spec: &ManagedHooks,
    install: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut root = read(path)?;
    let mut hooks: serde_json::Map<String, Value> = match root.get("hooks") {
        Some(raw) => serde_json::from_str(raw.get())
            .map_err(|e| format!("{}: `hooks` is not a JSON object: {e}", path.display()))?,
        None => serde_json::Map::new(),
    };
    let mut changed = false;
    let mut events: Vec<&str> = spec.hooks.iter().map(|hook| hook.event).collect();
    events.dedup();
    for event in events {
        let before = match hooks.get(event) {
            Some(Value::Array(groups)) => groups.clone(),
            Some(other) => {
                return Err(format!(
                    "{}: hooks.{event} is not an array ({other}); left unchanged",
                    path.display()
                )
                .into())
            }
            None => Vec::new(),
        };
        let mut groups = before.clone();
        remove_managed(&mut groups, spec);
        if install {
            groups.extend(
                spec.hooks
                    .iter()
                    .filter(|hook| hook.event == event)
                    .map(|hook| group(hook, spec)),
            );
        }
        if groups != before {
            changed = true;
            if groups.is_empty() {
                hooks.remove(event);
            } else {
                hooks.insert(event.to_string(), Value::Array(groups));
            }
        }
    }
    if !changed {
        return Ok(false);
    }
    if hooks.is_empty() {
        root.remove("hooks");
    } else {
        // Nested one level in the pretty-printed file: indent continuation lines to match.
        let text = serde_json::to_string_pretty(&hooks)?.replace('\n', "\n  ");
        root.set("hooks", RawValue::from_string(text)?);
    }
    write(path, &root)?;
    Ok(true)
}

fn remove_managed(groups: &mut Vec<Value>, spec: &ManagedHooks) {
    groups.retain_mut(|group| {
        let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        let before = hooks.len();
        hooks.retain(|hook| {
            let command = hook.get("command").and_then(Value::as_str);
            !command.is_some_and(|command| spec.hooks.iter().any(|m| command.ends_with(m.args)))
        });
        // A group that held only AIRC's entries goes with them; a user's group stays.
        !hooks.is_empty() || hooks.len() == before
    });
}

fn group(hook: &ManagedHook, spec: &ManagedHooks) -> Value {
    let mut entry = json!({
        "type": "command",
        "command": format!("airc {}", hook.args),
        "timeout": HOOK_TIMEOUT_SECS,
    });
    if let Some(status) = spec.status_message {
        entry["statusMessage"] = json!(status);
    }
    let mut group = json!({ "hooks": [entry] });
    if let Some(matcher) = hook.matcher {
        group["matcher"] = json!(matcher);
    }
    group
}

/// A JSON object whose members keep their order and their original text.
struct OrderedObject(Vec<(String, Box<RawValue>)>);

impl OrderedObject {
    fn get(&self, key: &str) -> Option<&RawValue> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_ref())
    }
    fn set(&mut self, key: &str, value: Box<RawValue>) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some((_, slot)) => *slot = value,
            None => self.0.push((key.to_string(), value)),
        }
    }
    fn remove(&mut self, key: &str) {
        self.0.retain(|(k, _)| k != key);
    }
}

impl<'de> Deserialize<'de> for OrderedObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Members;
        impl<'de> Visitor<'de> for Members {
            type Value = OrderedObject;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrderedObject, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry::<String, Box<RawValue>>()? {
                    members.push(member);
                }
                Ok(OrderedObject(members))
            }
        }
        deserializer.deserialize_map(Members)
    }
}

impl Serialize for OrderedObject {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(k, v)| (k, v)))
    }
}

fn read(path: &Path) -> Result<OrderedObject, Box<dyn std::error::Error>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(OrderedObject(Vec::new())),
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            format!(
                "{} is not a JSON object ({e}); left unchanged",
                path.display()
            )
            .into()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OrderedObject(Vec::new())),
        Err(error) => Err(error.into()),
    }
}

fn write(path: &Path, value: &OrderedObject) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staged = path.with_extension("json.airc-edit");
    std::fs::write(
        &staged,
        format!("{}\n", serde_json::to_string_pretty(value)?),
    )?;
    std::fs::rename(&staged, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: ManagedHooks = ManagedHooks {
        hooks: &[
            ManagedHook {
                event: "UserPromptSubmit",
                args: "test-hook user-prompt-submit",
                matcher: None,
            },
            ManagedHook {
                event: "PostToolUse",
                args: "test-hook post-tool-use",
                matcher: Some("*"),
            },
        ],
        status_message: None,
    };

    // what this catches: install/uninstall round-trips a file with unrelated keys,
    // their order, and a user's own hook in the same event untouched.
    #[test]
    fn round_trip_preserves_everything_it_does_not_own() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let original = "{\n  \"zeta\": {\"keep\": [1, 2]},\n  \"hooks\": {\n    \"PostToolUse\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"other-hook\"}]}]\n  },\n  \"alpha\": true\n}\n";
        std::fs::write(&path, original).unwrap();
        assert!(install(&path, &SPEC).unwrap());
        assert!(!install(&path, &SPEC).unwrap(), "idempotent");
        let installed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(installed["hooks"]["PostToolUse"][1]["matcher"], "*");
        assert_eq!(
            installed["hooks"]["PostToolUse"][1]["hooks"][0]["command"],
            "airc test-hook post-tool-use"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.find("\"zeta\"").unwrap() < text.find("\"alpha\"").unwrap());
        assert!(
            text.contains("{\"keep\": [1, 2]}"),
            "untouched values keep their text"
        );
        assert!(uninstall(&path, &SPEC).unwrap());
        assert!(!uninstall(&path, &SPEC).unwrap());
        let removed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let expected: Value = serde_json::from_str(original).unwrap();
        assert_eq!(removed, expected);
    }

    // what this catches: a file that is not an object is refused, never replaced.
    #[test]
    fn refuses_a_non_object_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        std::fs::write(&path, "[1]").unwrap();
        assert!(install(&path, &SPEC).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1]");
    }
}
