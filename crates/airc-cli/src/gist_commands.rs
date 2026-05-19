use std::error::Error;
use std::io::{self, Read};

use serde_json::Value;

pub fn run_get(path: &str, default: &str) -> Result<(), Box<dyn Error>> {
    let value = read_stdin_json()
        .and_then(|json| navigate(&json, path).cloned())
        .unwrap_or(Value::Null);
    emit_value(&value, default)?;
    Ok(())
}

pub fn run_get_json(path: &str) -> Result<(), Box<dyn Error>> {
    let value = read_stdin_json()
        .and_then(|json| navigate(&json, path).cloned())
        .unwrap_or(Value::Null);
    match value {
        Value::Null => println!(),
        other => println!("{}", serde_json::to_string(&other)?),
    }
    Ok(())
}

pub fn run_get_first_of(paths: &[String], default: &str) -> Result<(), Box<dyn Error>> {
    let Some(json) = read_stdin_json() else {
        println!("{default}");
        return Ok(());
    };
    for path in paths {
        if let Some(value) = navigate(&json, path) {
            emit_value(value, default)?;
            return Ok(());
        }
    }
    println!("{default}");
    Ok(())
}

pub fn run_pick_addr(scope: &str) -> Result<(), Box<dyn Error>> {
    let Some(json) = read_stdin_json() else {
        return Ok(());
    };
    if let Some(pick) = pick_addr(&json, |entry_scope| entry_scope == scope) {
        println!("{pick}");
    }
    Ok(())
}

pub fn run_pick_addr_first() -> Result<(), Box<dyn Error>> {
    let Some(json) = read_stdin_json() else {
        return Ok(());
    };
    if let Some(pick) = pick_addr(&json, |_| true) {
        println!("{pick}");
    }
    Ok(())
}

pub fn run_pick_addr_nonlocal_first() -> Result<(), Box<dyn Error>> {
    let Some(json) = read_stdin_json() else {
        return Ok(());
    };
    if let Some(pick) = pick_addr(&json, |scope| scope != "localhost") {
        println!("{pick}");
    }
    Ok(())
}

pub fn run_pick_addr_excluding(exclude_scopes: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(json) = read_stdin_json() else {
        return Ok(());
    };
    if let Some(pick) = pick_addr(&json, |scope| {
        !exclude_scopes.iter().any(|excluded| excluded == scope)
    }) {
        println!("{pick}");
    }
    Ok(())
}

pub fn run_list_lan_entries() -> Result<(), Box<dyn Error>> {
    let Some(json) = read_stdin_json() else {
        return Ok(());
    };
    let Some(entries) = json.as_array() else {
        return Ok(());
    };
    for entry in entries {
        if entry.get("scope").and_then(Value::as_str) == Some("lan") {
            println!("{}", serde_json::to_string(entry)?);
        }
    }
    Ok(())
}

pub fn run_gist_content(channel: &str) -> Result<(), Box<dyn Error>> {
    let Some(json) = read_stdin_json() else {
        return Ok(());
    };
    if let Some(content) = gist_content(&json, channel) {
        println!("{content}");
    }
    Ok(())
}

fn read_stdin_json() -> Option<Value> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&raw).ok()
}

fn navigate<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path == "." {
        return Some(value);
    }

    let mut current = value;
    for segment in path.strip_prefix('.')?.split('.') {
        if segment.is_empty() {
            return None;
        }
        let (key, index) = parse_segment(segment)?;
        if !key.is_empty() {
            current = current.as_object()?.get(key)?;
        }
        if let Some(index) = index {
            current = current.as_array()?.get(index)?;
        }
    }
    Some(current)
}

fn parse_segment(segment: &str) -> Option<(&str, Option<usize>)> {
    if segment.starts_with('[') {
        return Some(("", Some(parse_index(segment)?)));
    }
    let Some(open) = segment.find('[') else {
        return valid_key(segment).then_some((segment, None));
    };
    let key = &segment[..open];
    valid_key(key).then_some((key, Some(parse_index(&segment[open..])?)))
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '-' || ch.is_ascii_alphanumeric())
}

fn parse_index(segment: &str) -> Option<usize> {
    let inner = segment.strip_prefix('[')?.strip_suffix(']')?;
    inner.parse().ok()
}

fn emit_value(value: &Value, default: &str) -> Result<(), Box<dyn Error>> {
    match value {
        Value::Null => println!("{default}"),
        Value::String(text) => println!("{text}"),
        Value::Bool(true) => println!("true"),
        Value::Bool(false) => println!("false"),
        other => println!("{}", serde_json::to_string(other)?),
    }
    Ok(())
}

fn pick_addr<F>(json: &Value, allowed_scope: F) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    json.as_array()?.iter().find_map(|entry| {
        let scope = entry.get("scope").and_then(Value::as_str).unwrap_or("");
        if !allowed_scope(scope) {
            return None;
        }
        let addr = entry.get("addr").and_then(Value::as_str)?;
        let port = entry.get("port")?;
        (!addr.is_empty() && !port.is_null()).then(|| format!("{addr}|{}", port_as_text(port)))
    })
}

fn port_as_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn gist_content(json: &Value, channel: &str) -> Option<String> {
    let files = json.get("files")?.as_object()?;
    if !channel.is_empty() {
        let exact_name = format!("airc-room-{channel}.json");
        if let Some((_, _, content)) = files
            .iter()
            .filter_map(|(name, entry)| matching_channel_content(name, entry, channel, &exact_name))
            .max_by_key(|(heartbeat, exact, _)| (*heartbeat, *exact))
        {
            return Some(content);
        }
    }
    files
        .values()
        .find_map(|entry| entry.get("content").and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn matching_channel_content(
    name: &str,
    entry: &Value,
    channel: &str,
    exact_name: &str,
) -> Option<(i64, bool, String)> {
    let content = entry.get("content")?.as_str()?.to_owned();
    let envelope: Value = serde_json::from_str(&content).ok()?;
    let channels = envelope.get("channels")?.as_array()?;
    channels
        .iter()
        .any(|item| item.as_str() == Some(channel))
        .then(|| {
            (
                heartbeat_epoch(envelope.get("last_heartbeat")),
                name == exact_name,
                content,
            )
        })
}

fn heartbeat_epoch(value: Option<&Value>) -> i64 {
    let Some(timestamp) = value.and_then(Value::as_str) else {
        return 0;
    };
    let normalized = timestamp
        .strip_suffix("+00:00")
        .map_or_else(|| timestamp.to_owned(), |prefix| format!("{prefix}Z"));
    airc_core::iso_to_epoch(&normalized).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn navigate_reads_paths_and_indexes() {
        let value = json!({"host":{"addresses":[{"addr":"127.0.0.1"}]}});

        assert_eq!(
            navigate(&value, ".host.addresses[0].addr").and_then(Value::as_str),
            Some("127.0.0.1")
        );
        assert!(navigate(&value, ".host.addresses[1].addr").is_none());
    }

    #[test]
    fn pick_addr_excludes_unreachable_scopes() {
        let value = json!([
            {"scope":"localhost","addr":"127.0.0.1","port":"7547"},
            {"scope":"lan","addr":"192.168.1.42","port":"7547"},
            {"scope":"tailscale","addr":"100.79.156.3","port":"7547"}
        ]);

        assert_eq!(
            pick_addr(&value, |scope| scope != "localhost" && scope != "tailscale"),
            Some("192.168.1.42|7547".to_string())
        );
    }

    #[test]
    fn gist_content_selects_freshest_matching_channel() {
        let stale = json!({
            "airc": 1,
            "kind": "mesh",
            "channels": ["general", "acme"],
            "last_heartbeat": "2026-05-04T12:54:15Z",
            "invite": "stale-host@example"
        });
        let fresh = json!({
            "airc": 1,
            "kind": "mesh",
            "channels": ["acme", "general"],
            "last_heartbeat": "2026-05-04T17:14:09Z",
            "invite": "fresh-host@example"
        });
        let gist = json!({
            "files": {
                "airc-room-acme.json": {"content": stale.to_string()},
                "airc-room-general.json": {"content": fresh.to_string()}
            }
        });

        let content = gist_content(&gist, "acme").unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["invite"], "fresh-host@example");
    }

    #[test]
    fn heartbeat_parse_accepts_utc_offset() {
        assert_eq!(
            heartbeat_epoch(Some(&json!("2026-05-04T17:14:09+00:00"))),
            heartbeat_epoch(Some(&json!("2026-05-04T17:14:09Z")))
        );
    }
}
