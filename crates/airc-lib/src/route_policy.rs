//! Transport route policy for consumer-facing AIRC embeddings.
//!
//! The important invariant is negative: GitHub is not a transparent
//! runtime fallback. It can carry bootstrap/rendezvous frames when a caller
//! explicitly chooses that purpose, but normal chat/data routing must use
//! direct transports or fail loudly.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoutePurpose {
    /// Normal durable conversation / work traffic.
    Data,
    /// Interrupt-style live events such as presence or turn steering.
    LiveEvent,
    /// Initial peer discovery / rendezvous metadata.
    Bootstrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportKind {
    LocalFs,
    LanTcp,
    Tailscale,
    Reticulum,
    Relay,
    GhGist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportRole {
    Direct,
    Relay,
    BootstrapOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportCandidate {
    pub kind: TransportKind,
    pub role: TransportRole,
    pub healthy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    Selected(TransportKind),
    NoRoute { purpose: RoutePurpose },
}

#[derive(Debug, Clone, Default)]
pub struct RoutePolicy;

impl RoutePolicy {
    pub fn choose(
        &self,
        purpose: RoutePurpose,
        candidates: impl IntoIterator<Item = TransportCandidate>,
    ) -> RouteDecision {
        candidates
            .into_iter()
            .filter(|candidate| candidate.healthy)
            .filter(|candidate| allows(purpose, *candidate))
            .min_by_key(|candidate| priority(purpose, candidate.kind))
            .map(|candidate| RouteDecision::Selected(candidate.kind))
            .unwrap_or(RouteDecision::NoRoute { purpose })
    }
}

fn allows(purpose: RoutePurpose, candidate: TransportCandidate) -> bool {
    match candidate.kind {
        TransportKind::GhGist => {
            purpose == RoutePurpose::Bootstrap && candidate.role == TransportRole::BootstrapOnly
        }
        TransportKind::LocalFs
        | TransportKind::LanTcp
        | TransportKind::Tailscale
        | TransportKind::Reticulum => candidate.role == TransportRole::Direct,
        TransportKind::Relay => candidate.role == TransportRole::Relay,
    }
}

fn priority(purpose: RoutePurpose, kind: TransportKind) -> u8 {
    match purpose {
        RoutePurpose::Data | RoutePurpose::LiveEvent => match kind {
            TransportKind::LocalFs => 0,
            TransportKind::LanTcp => 1,
            TransportKind::Tailscale => 2,
            TransportKind::Reticulum => 3,
            TransportKind::Relay => 4,
            TransportKind::GhGist => 255,
        },
        RoutePurpose::Bootstrap => match kind {
            TransportKind::LocalFs => 0,
            TransportKind::LanTcp => 1,
            TransportKind::Tailscale => 2,
            TransportKind::Reticulum => 3,
            TransportKind::Relay => 4,
            TransportKind::GhGist => 5,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(kind: TransportKind, role: TransportRole) -> TransportCandidate {
        TransportCandidate {
            kind,
            role,
            healthy: true,
        }
    }

    #[test]
    fn data_routes_never_select_github_as_fallback() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RoutePurpose::Data,
            [candidate(
                TransportKind::GhGist,
                TransportRole::BootstrapOnly,
            )],
        );

        assert_eq!(
            decision,
            RouteDecision::NoRoute {
                purpose: RoutePurpose::Data
            }
        );
    }

    #[test]
    fn bootstrap_can_use_github_when_no_direct_route_exists() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RoutePurpose::Bootstrap,
            [candidate(
                TransportKind::GhGist,
                TransportRole::BootstrapOnly,
            )],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::GhGist));
    }

    #[test]
    fn direct_routes_beat_github_even_for_bootstrap() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RoutePurpose::Bootstrap,
            [
                candidate(TransportKind::GhGist, TransportRole::BootstrapOnly),
                candidate(TransportKind::LanTcp, TransportRole::Direct),
            ],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::LanTcp));
    }

    #[test]
    fn unhealthy_candidates_are_ignored() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RoutePurpose::Data,
            [
                TransportCandidate {
                    kind: TransportKind::LocalFs,
                    role: TransportRole::Direct,
                    healthy: false,
                },
                candidate(TransportKind::LanTcp, TransportRole::Direct),
            ],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::LanTcp));
    }

    #[test]
    fn reticulum_is_a_direct_transport_not_a_github_fallback() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RoutePurpose::Data,
            [
                candidate(TransportKind::GhGist, TransportRole::BootstrapOnly),
                candidate(TransportKind::Reticulum, TransportRole::Direct),
            ],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::Reticulum));
    }
}
