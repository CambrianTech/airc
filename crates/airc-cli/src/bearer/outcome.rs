use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendKind {
    Delivered,
    AuthFailure,
    TransientFailure,
    SecondaryRateLimit,
    Gone,
}

#[derive(Debug, Serialize)]
pub struct SendOutcome {
    kind: SendKind,
    detail: String,
}

impl SendOutcome {
    pub fn new(kind: SendKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub fn delivered() -> Self {
        Self::new(SendKind::Delivered, "")
    }
}

pub fn kind_name(kind: SendKind) -> &'static str {
    match kind {
        SendKind::Delivered => "delivered",
        SendKind::AuthFailure => "auth_failure",
        SendKind::TransientFailure => "transient_failure",
        SendKind::SecondaryRateLimit => "secondary_rate_limit",
        SendKind::Gone => "gone",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_outcome_serializes_legacy_kind() {
        let outcome = SendOutcome::new(SendKind::SecondaryRateLimit, "slow down");
        let encoded = serde_json::to_string(&outcome).unwrap();
        assert_eq!(
            encoded,
            r#"{"kind":"secondary_rate_limit","detail":"slow down"}"#
        );
    }
}
