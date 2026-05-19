//! Helpers for reading legacy bearer state JSON from shell commands.

use std::error::Error;
use std::fs;
use std::path::Path;

use serde_json::Value;

pub fn run(path: &Path) -> Result<(), Box<dyn Error>> {
    let state = read_state(path)?;
    println!(
        "{} {}",
        int_timestamp(state.get("last_recv_ts")),
        int_timestamp(state.get("last_heartbeat_ts"))
    );
    Ok(())
}

fn read_state(path: &Path) -> Result<Value, Box<dyn Error>> {
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn int_timestamp(value: Option<&Value>) -> u64 {
    let Some(value) = value else {
        return 0;
    };
    let parsed = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse::<f64>().ok(),
        _ => None,
    };
    parsed
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{int_timestamp, read_state};
    use serde_json::json;

    #[test]
    fn timestamp_accepts_numbers_and_numeric_strings() {
        assert_eq!(int_timestamp(Some(&json!(123.9))), 123);
        assert_eq!(int_timestamp(Some(&json!("456.7"))), 456);
    }

    #[test]
    fn timestamp_rejects_missing_negative_and_non_numeric_values() {
        assert_eq!(int_timestamp(None), 0);
        assert_eq!(int_timestamp(Some(&json!(-1))), 0);
        assert_eq!(int_timestamp(Some(&json!("nope"))), 0);
        assert_eq!(int_timestamp(Some(&json!({}))), 0);
    }

    #[test]
    fn read_state_parses_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"last_recv_ts": 10}"#).unwrap();

        assert_eq!(read_state(&path).unwrap()["last_recv_ts"], 10);
    }
}
