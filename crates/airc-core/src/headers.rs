//! Envelope headers — small routable metadata carried beside opaque bodies.
//!
//! Headers are the airc equivalent of HTTP headers: optional, deterministic,
//! cheap to inspect, and independent from the body payload. The substrate may
//! route, filter, retain, or diagnose using these values without parsing
//! consumer JSON. Consumers own their own namespaces (`forge.*`,
//! `openclaw.*`, `hermes.*`, `continuum.*`, `x-*`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Deterministically ordered string headers.
pub type Headers = BTreeMap<String, String>;

/// Match predicate for subscription fan-out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderFilter {
    /// Matches every envelope — the default (no header scoping).
    #[default]
    Any,
    Exact {
        key: String,
        value: String,
    },
    Prefix {
        key: String,
        value_prefix: String,
    },
    All(Vec<HeaderFilter>),
    AnyOf(Vec<HeaderFilter>),
}

impl HeaderFilter {
    pub fn matches(&self, headers: &Headers) -> bool {
        match self {
            HeaderFilter::Any => true,
            HeaderFilter::Exact { key, value } => headers.get(key) == Some(value),
            HeaderFilter::Prefix { key, value_prefix } => headers
                .get(key)
                .is_some_and(|value| value.starts_with(value_prefix)),
            HeaderFilter::All(filters) => filters.iter().all(|filter| filter.matches(headers)),
            HeaderFilter::AnyOf(filters) => filters.iter().any(|filter| filter.matches(headers)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn external_agent_headers() -> Headers {
        Headers::from([
            (
                "forge.body_hint".to_string(),
                "forge.persona.turn".to_string(),
            ),
            (
                "openclaw.channel".to_string(),
                "discord:continuum-lab".to_string(),
            ),
            ("hermes.skill".to_string(), "calendar".to_string()),
        ])
    }

    #[test]
    fn header_filter_matches_exact_and_prefix_without_body_parse() {
        let headers = external_agent_headers();

        assert!(HeaderFilter::Exact {
            key: "openclaw.channel".to_string(),
            value: "discord:continuum-lab".to_string(),
        }
        .matches(&headers));

        assert!(HeaderFilter::Prefix {
            key: "forge.body_hint".to_string(),
            value_prefix: "forge.persona.".to_string(),
        }
        .matches(&headers));

        assert!(!HeaderFilter::Exact {
            key: "hermes.skill".to_string(),
            value: "memory".to_string(),
        }
        .matches(&headers));
    }

    #[test]
    fn namespaced_headers_are_pass_through_and_deterministic() {
        let headers = external_agent_headers();
        let encoded = serde_json::to_string(&headers).unwrap();

        assert!(encoded.find("forge.body_hint").unwrap() < encoded.find("hermes.skill").unwrap());
        assert!(encoded.contains("openclaw.channel"));
    }
}
