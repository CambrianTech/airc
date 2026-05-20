//! Transport resolver shell.
//!
//! This module is deliberately policy-driven and transport-agnostic.
//! It does not open sockets, poll GitHub, or probe Reticulum. It
//! accepts measured candidates and applies [`RoutePolicy`]. Later
//! slices can add health probes/discovery without changing the rule
//! that GitHub is bootstrap/rendezvous only.

use crate::route_health::TransportHealthSample;
use crate::route_policy::{
    RouteDecision, RoutePolicy, RoutePurpose, TransportCandidate, TransportKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportRoute {
    pub kind: TransportKind,
}

#[derive(Debug, Clone)]
pub struct TransportResolver {
    policy: RoutePolicy,
    candidates: Vec<TransportCandidate>,
}

impl TransportResolver {
    pub fn new(candidates: impl IntoIterator<Item = TransportCandidate>) -> Self {
        Self {
            policy: RoutePolicy,
            candidates: candidates.into_iter().collect(),
        }
    }

    pub fn from_health(samples: impl IntoIterator<Item = TransportHealthSample>) -> Self {
        Self::new(samples.into_iter().map(TransportHealthSample::candidate))
    }

    pub fn candidates(&self) -> &[TransportCandidate] {
        &self.candidates
    }

    pub fn replace_candidates(&mut self, candidates: impl IntoIterator<Item = TransportCandidate>) {
        self.candidates = candidates.into_iter().collect();
    }

    pub fn replace_health(&mut self, samples: impl IntoIterator<Item = TransportHealthSample>) {
        self.replace_candidates(samples.into_iter().map(TransportHealthSample::candidate));
    }

    pub fn resolve(&self, purpose: RoutePurpose) -> Result<TransportRoute, RouteDecision> {
        match self.policy.choose(purpose, self.candidates.iter().copied()) {
            RouteDecision::Selected(kind) => Ok(TransportRoute { kind }),
            decision @ RouteDecision::NoRoute { .. } => Err(decision),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_health::{TransportHealthSample, TransportHealthState};
    use crate::route_policy::{TransportKind::*, TransportRole};

    fn candidate(kind: TransportKind, role: TransportRole) -> TransportCandidate {
        TransportCandidate {
            kind,
            role,
            healthy: true,
        }
    }

    #[test]
    fn resolver_rejects_github_for_runtime_data() {
        let resolver = TransportResolver::new([candidate(GhGist, TransportRole::BootstrapOnly)]);

        assert_eq!(
            resolver.resolve(RoutePurpose::Data),
            Err(RouteDecision::NoRoute {
                purpose: RoutePurpose::Data
            })
        );
    }

    #[test]
    fn resolver_allows_github_for_bootstrap_only() {
        let resolver = TransportResolver::new([candidate(GhGist, TransportRole::BootstrapOnly)]);

        assert_eq!(
            resolver.resolve(RoutePurpose::Bootstrap),
            Ok(TransportRoute { kind: GhGist })
        );
    }

    #[test]
    fn resolver_selects_reticulum_for_runtime_data() {
        let resolver = TransportResolver::new([
            candidate(GhGist, TransportRole::BootstrapOnly),
            candidate(Reticulum, TransportRole::Direct),
        ]);

        assert_eq!(
            resolver.resolve(RoutePurpose::Data),
            Ok(TransportRoute { kind: Reticulum })
        );
    }

    #[test]
    fn resolver_candidate_set_can_be_replaced_after_health_refresh() {
        let mut resolver = TransportResolver::new([candidate(LanTcp, TransportRole::Direct)]);

        assert_eq!(
            resolver.resolve(RoutePurpose::Data),
            Ok(TransportRoute { kind: LanTcp })
        );

        resolver.replace_candidates([candidate(Reticulum, TransportRole::Direct)]);

        assert_eq!(
            resolver.resolve(RoutePurpose::Data),
            Ok(TransportRoute { kind: Reticulum })
        );
    }

    #[test]
    fn resolver_can_build_candidates_from_health_samples() {
        let resolver = TransportResolver::from_health([
            TransportHealthSample::down(LanTcp, TransportRole::Direct),
            TransportHealthSample {
                kind: Reticulum,
                role: TransportRole::Direct,
                state: TransportHealthState::Healthy,
                rtt_ms: Some(80),
                success_ppm: Some(1_000_000),
            },
        ]);

        assert_eq!(
            resolver.resolve(RoutePurpose::Data),
            Ok(TransportRoute { kind: Reticulum })
        );
    }
}
