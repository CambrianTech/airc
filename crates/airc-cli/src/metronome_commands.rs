use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

pub fn run_recent_senders(
    messages_log: &Path,
    window_seconds: u64,
    exclude_name: &str,
) -> Result<(), Box<dyn Error>> {
    for sender in recent_senders(messages_log, window_seconds, exclude_name)? {
        println!("{sender}");
    }
    Ok(())
}

fn recent_senders(
    messages_log: &Path,
    window_seconds: u64,
    exclude_name: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    let file = match File::open(messages_log) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Box::new(error)),
    };

    let now_ms = Utc::now().timestamp_millis();
    let window_ms = (window_seconds as i64).saturating_mul(1000);
    let mut seen: HashMap<String, i64> = HashMap::new();

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(sender) = message.get("from").and_then(Value::as_str) else {
            continue;
        };
        if sender.is_empty() || sender == "airc" || sender == exclude_name {
            continue;
        }
        let Some(sent_ms) = message.get("ts").and_then(parse_timestamp_ms) else {
            continue;
        };
        if now_ms.saturating_sub(sent_ms) > window_ms {
            continue;
        }
        let entry = seen.entry(sender.to_string()).or_insert(i64::MIN);
        *entry = (*entry).max(sent_ms);
    }

    let mut senders: Vec<_> = seen.into_iter().collect();
    senders.sort_by(|(left_name, left_ts), (right_name, right_ts)| {
        right_ts
            .cmp(left_ts)
            .then_with(|| left_name.cmp(right_name))
    });
    Ok(senders.into_iter().map(|(sender, _)| sender).collect())
}

fn parse_timestamp_ms(value: &Value) -> Option<i64> {
    if let Some(integer) = value.as_i64() {
        return Some(epoch_number_to_ms(integer as f64));
    }
    if let Some(float) = value.as_f64() {
        return Some(epoch_number_to_ms(float));
    }
    let timestamp = value.as_str()?;
    DateTime::parse_from_rfc3339(&timestamp.replace('Z', "+00:00"))
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}

fn epoch_number_to_ms(value: f64) -> i64 {
    if value > 1_000_000_000_000.0 {
        value as i64
    } else {
        (value * 1000.0) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn recent_senders_filters_self_and_orders_by_latest_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("messages.jsonl");
        let mut file = File::create(&path).expect("messages log");
        let now = Utc::now().timestamp();
        writeln!(file, r#"{{"from":"alice","ts":{}}}"#, now - 5).expect("write alice");
        writeln!(file, r#"{{"from":"self","ts":{}}}"#, now - 1).expect("write self");
        writeln!(file, r#"{{"from":"bob","ts":{}}}"#, now - 3).expect("write bob");
        writeln!(file, r#"{{"from":"alice","ts":{}}}"#, now - 2).expect("write alice latest");
        writeln!(file, r#"{{"from":"airc","ts":{}}}"#, now).expect("write airc");

        let senders = recent_senders(&path, 60, "self").expect("recent senders");

        assert_eq!(senders, vec!["alice", "bob"]);
    }

    #[test]
    fn recent_senders_accepts_epoch_ms_and_rfc3339() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("messages.jsonl");
        let mut file = File::create(&path).expect("messages log");
        let now = Utc::now();
        writeln!(
            file,
            r#"{{"from":"millis","ts":{}}}"#,
            now.timestamp_millis()
        )
        .expect("write ms");
        writeln!(file, r#"{{"from":"iso","ts":"{}"}}"#, now.to_rfc3339()).expect("write iso");

        let senders = recent_senders(&path, 60, "").expect("recent senders");

        assert_eq!(senders.len(), 2);
        assert!(senders.contains(&"millis".to_string()));
        assert!(senders.contains(&"iso".to_string()));
    }
}
