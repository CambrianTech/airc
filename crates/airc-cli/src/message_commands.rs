use std::error::Error;

use serde_json::{json, Value};

pub fn run_build_legacy(
    from: &str,
    to: &str,
    ts: &str,
    channel: &str,
    msg: &str,
    client_id: &str,
    kind: &str,
) -> Result<(), Box<dyn Error>> {
    let mut payload = json!({
        "from": from,
        "to": to,
        "ts": ts,
        "channel": channel,
        "msg": msg,
    });
    let object = payload
        .as_object_mut()
        .expect("json object literal is an object");
    if !client_id.is_empty() {
        object.insert(
            "client_id".to_string(),
            Value::String(client_id.to_string()),
        );
    }
    if !kind.is_empty() {
        object.insert("kind".to_string(), Value::String(kind.to_string()));
    }
    println!("{}", serde_json::to_string(&payload)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_legacy_owns_json_serialization() {
        let mut out = Vec::new();
        let payload = json!({
            "from": "alice",
            "to": "all",
            "ts": "2026-05-19T00:00:00Z",
            "channel": "general",
            "msg": "line\nquote \" slash \\",
            "client_id": "client",
            "kind": "heartbeat",
        });
        serde_json::to_writer(&mut out, &payload).unwrap();
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["msg"], "line\nquote \" slash \\");

        run_build_legacy(
            "alice",
            "all",
            "2026-05-19T00:00:00Z",
            "general",
            "line\nquote \" slash \\",
            "client",
            "heartbeat",
        )
        .unwrap();
    }
}
