use std::error::Error;

use airc_core::ChatLogEnvelope;

pub fn run_build(
    from: &str,
    to: &str,
    ts: &str,
    channel: &str,
    msg: &str,
    client_id: &str,
    kind: &str,
) -> Result<(), Box<dyn Error>> {
    let payload = ChatLogEnvelope::new(from, to, ts, channel, msg)
        .with_client_id(client_id)
        .with_kind(kind);
    println!("{}", serde_json::to_string(&payload)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn command_uses_core_chat_log_shape() {
        let encoded = serde_json::to_string(
            &ChatLogEnvelope::new(
                "alice",
                "all",
                "2026-05-19T00:00:00Z",
                "general",
                "line\nquote \" slash \\",
            )
            .with_client_id("client")
            .with_kind("heartbeat"),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(parsed["msg"], "line\nquote \" slash \\");
    }
}
