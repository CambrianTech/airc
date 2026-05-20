use std::error::Error;

use serde_json::Value;

pub fn navigate<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
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

pub fn emit_value(value: &Value, default: &str) -> Result<(), Box<dyn Error>> {
    match value {
        Value::Null => println!("{default}"),
        Value::String(text) => println!("{text}"),
        Value::Bool(true) => println!("true"),
        Value::Bool(false) => println!("false"),
        other => println!("{}", serde_json::to_string(other)?),
    }
    Ok(())
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
    fn navigate_rejects_invalid_paths() {
        let value = json!({"host":{"name":"alpha"}});

        assert!(navigate(&value, "host.name").is_none());
        assert!(navigate(&value, ".host..name").is_none());
        assert!(navigate(&value, ".host.name[bad]").is_none());
    }
}
