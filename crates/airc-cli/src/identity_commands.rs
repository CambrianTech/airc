use std::error::Error;

use serde_json::Value;

const IDENTITY_FIELDS: &[&str] = &["pronouns", "role", "bio", "status"];

pub fn run_pretty(name: &str, identity_json: &str, host: &str) -> Result<(), Box<dyn Error>> {
    let identity: Value =
        serde_json::from_str(identity_json).unwrap_or_else(|_| Value::Object(Default::default()));

    println!("  name:      {name}");
    for field in IDENTITY_FIELDS {
        let label = format!("{field}:");
        let value = identity
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("(unset)");
        println!("  {label:<11} {value}");
    }

    let integrations = identity.get("integrations").and_then(Value::as_object);
    if let Some(integrations) = integrations.filter(|items| !items.is_empty()) {
        println!("  integrations:");
        for (key, value) in integrations {
            let text = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            println!("    {key}: {text}");
        }
    } else {
        println!("  integrations: (none)");
    }

    if !host.is_empty() {
        println!("  host:      {host}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_identity_is_treated_as_empty() {
        assert!(run_pretty("alice", "not-json", "").is_ok());
    }
}
