//! Transport route policy for consumer-facing AIRC embeddings.
//!
//! The important invariant is negative: GitHub is not a transparent
//! runtime fallback. Gists can carry invite/rendezvous beacons when a
//! caller explicitly chooses that route class, but normal chat, event,
//! media-signaling, and bulk routing must use admitted live transports
//! or fail loudly.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteClass {
    /// Publish a shareable invite beacon.
    InviteAdvertise,
    /// Find or refresh candidate routes for a peer.
    PeerRendezvous,
    /// Durable low-latency control traffic.
    ControlInteractive,
    /// Durable chat/work traffic.
    DataInteractive,
    /// Larger payload handoff metadata.
    DataBulk,
    /// WebRTC/LiveKit/game session signaling.
    MediaSignaling,
    /// Typing/thinking/presence-style ephemeral state.
    PresenceEphemeral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportKind {
    LocalFs,
    LanTcp,
    Tailscale,
    Udp,
    WebRtcDataChannel,
    Reticulum,
    Relay,
    Ssh,
    GhGist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportRole {
    Direct,
    Relay,
    InviteBeacon,
    Rendezvous,
    Admin,
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
    NoRoute { class: RouteClass },
}

#[derive(Debug, Clone, Default)]
pub struct RoutePolicy;

impl RoutePolicy {
    pub fn choose(
        &self,
        class: RouteClass,
        candidates: impl IntoIterator<Item = TransportCandidate>,
    ) -> RouteDecision {
        candidates
            .into_iter()
            .filter(|candidate| candidate.healthy)
            .filter(|candidate| allows(class, *candidate))
            .min_by_key(|candidate| priority(class, candidate.kind, candidate.role))
            .map(|candidate| RouteDecision::Selected(candidate.kind))
            .unwrap_or(RouteDecision::NoRoute { class })
    }
}

fn allows(class: RouteClass, candidate: TransportCandidate) -> bool {
    match candidate.kind {
        TransportKind::GhGist => matches!(
            (class, candidate.role),
            (RouteClass::InviteAdvertise, TransportRole::InviteBeacon)
                | (RouteClass::PeerRendezvous, TransportRole::Rendezvous)
        ),
        TransportKind::LocalFs | TransportKind::LanTcp | TransportKind::Tailscale => {
            candidate.role == TransportRole::Direct
                && (is_live_class(class) || class == RouteClass::PeerRendezvous)
        }
        TransportKind::Udp | TransportKind::WebRtcDataChannel => {
            candidate.role == TransportRole::Direct
                && matches!(
                    class,
                    RouteClass::ControlInteractive
                        | RouteClass::DataInteractive
                        | RouteClass::MediaSignaling
                        | RouteClass::PresenceEphemeral
                )
        }
        TransportKind::Reticulum => {
            (candidate.role == TransportRole::Direct
                && (is_live_class(class) || class == RouteClass::PeerRendezvous))
                || matches!(
                    (class, candidate.role),
                    (RouteClass::PeerRendezvous, TransportRole::Rendezvous)
                )
        }
        TransportKind::Relay => {
            (candidate.role == TransportRole::Relay && is_live_class(class))
                || matches!(
                    (class, candidate.role),
                    (RouteClass::InviteAdvertise, TransportRole::InviteBeacon)
                        | (RouteClass::PeerRendezvous, TransportRole::Rendezvous)
                )
        }
        TransportKind::Ssh => false,
    }
}

fn is_live_class(class: RouteClass) -> bool {
    matches!(
        class,
        RouteClass::ControlInteractive
            | RouteClass::DataInteractive
            | RouteClass::DataBulk
            | RouteClass::MediaSignaling
            | RouteClass::PresenceEphemeral
    )
}

fn priority(class: RouteClass, kind: TransportKind, role: TransportRole) -> u8 {
    match class {
        RouteClass::ControlInteractive
        | RouteClass::DataInteractive
        | RouteClass::PresenceEphemeral => match kind {
            TransportKind::LanTcp => 0,
            TransportKind::LocalFs => 1,
            TransportKind::Tailscale => 2,
            TransportKind::Udp => 3,
            TransportKind::WebRtcDataChannel => 4,
            TransportKind::Reticulum => 5,
            TransportKind::Relay => 6,
            TransportKind::Ssh | TransportKind::GhGist => 255,
        },
        RouteClass::MediaSignaling => match kind {
            TransportKind::LocalFs => 0,
            TransportKind::Udp => 1,
            TransportKind::WebRtcDataChannel => 2,
            TransportKind::LanTcp => 3,
            TransportKind::Tailscale => 4,
            TransportKind::Reticulum => 5,
            TransportKind::Relay => 6,
            TransportKind::Ssh | TransportKind::GhGist => 255,
        },
        RouteClass::DataBulk => match kind {
            TransportKind::LanTcp => 0,
            TransportKind::LocalFs => 1,
            TransportKind::Tailscale => 2,
            TransportKind::Reticulum => 3,
            TransportKind::Relay => 4,
            TransportKind::Udp
            | TransportKind::WebRtcDataChannel
            | TransportKind::Ssh
            | TransportKind::GhGist => 255,
        },
        RouteClass::PeerRendezvous => match (kind, role) {
            (TransportKind::LanTcp, TransportRole::Direct) => 0,
            (TransportKind::LocalFs, TransportRole::Direct) => 1,
            (TransportKind::Tailscale, TransportRole::Direct) => 2,
            (TransportKind::Reticulum, TransportRole::Direct) => 3,
            (TransportKind::Reticulum, TransportRole::Rendezvous) => 4,
            (TransportKind::Relay, TransportRole::Rendezvous) => 5,
            (TransportKind::GhGist, TransportRole::Rendezvous) => 6,
            _ => 255,
        },
        RouteClass::InviteAdvertise => match (kind, role) {
            (TransportKind::Relay, TransportRole::InviteBeacon) => 0,
            (TransportKind::GhGist, TransportRole::InviteBeacon) => 1,
            _ => 255,
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
    fn live_routes_never_select_github_as_fallback() {
        let policy = RoutePolicy;
        for class in [
            RouteClass::ControlInteractive,
            RouteClass::DataInteractive,
            RouteClass::DataBulk,
            RouteClass::MediaSignaling,
            RouteClass::PresenceEphemeral,
        ] {
            assert_eq!(
                policy.choose(
                    class,
                    [
                        candidate(TransportKind::GhGist, TransportRole::InviteBeacon),
                        candidate(TransportKind::GhGist, TransportRole::Rendezvous),
                    ],
                ),
                RouteDecision::NoRoute { class }
            );
        }
    }

    #[test]
    fn invite_advertise_can_use_github_when_no_relay_beacon_exists() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RouteClass::InviteAdvertise,
            [candidate(
                TransportKind::GhGist,
                TransportRole::InviteBeacon,
            )],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::GhGist));
    }

    #[test]
    fn peer_rendezvous_can_use_github_when_no_better_route_exists() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RouteClass::PeerRendezvous,
            [candidate(TransportKind::GhGist, TransportRole::Rendezvous)],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::GhGist));
    }

    #[test]
    fn direct_routes_beat_github_for_peer_rendezvous() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RouteClass::PeerRendezvous,
            [
                candidate(TransportKind::GhGist, TransportRole::Rendezvous),
                candidate(TransportKind::LanTcp, TransportRole::Direct),
            ],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::LanTcp));
    }

    #[test]
    fn live_peer_delivery_prefers_lan_over_local_storage_when_both_are_healthy() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RouteClass::DataInteractive,
            [
                candidate(TransportKind::LocalFs, TransportRole::Direct),
                candidate(TransportKind::LanTcp, TransportRole::Direct),
            ],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::LanTcp));
    }

    #[test]
    fn relay_beacon_beats_github_invite() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RouteClass::InviteAdvertise,
            [
                candidate(TransportKind::GhGist, TransportRole::InviteBeacon),
                candidate(TransportKind::Relay, TransportRole::InviteBeacon),
            ],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::Relay));
    }

    #[test]
    fn unhealthy_candidates_are_ignored() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RouteClass::DataInteractive,
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
            RouteClass::DataInteractive,
            [
                candidate(TransportKind::GhGist, TransportRole::Rendezvous),
                candidate(TransportKind::Reticulum, TransportRole::Direct),
            ],
        );

        assert_eq!(decision, RouteDecision::Selected(TransportKind::Reticulum));
    }

    #[test]
    fn ssh_is_not_admissible_for_product_delivery() {
        let policy = RoutePolicy;
        let decision = policy.choose(
            RouteClass::DataInteractive,
            [candidate(TransportKind::Ssh, TransportRole::Admin)],
        );

        assert_eq!(
            decision,
            RouteDecision::NoRoute {
                class: RouteClass::DataInteractive
            }
        );
    }

    #[test]
    fn udp_is_interactive_not_bulk() {
        let policy = RoutePolicy;

        assert_eq!(
            policy.choose(
                RouteClass::MediaSignaling,
                [candidate(TransportKind::Udp, TransportRole::Direct)],
            ),
            RouteDecision::Selected(TransportKind::Udp)
        );
        assert_eq!(
            policy.choose(
                RouteClass::DataBulk,
                [candidate(TransportKind::Udp, TransportRole::Direct)],
            ),
            RouteDecision::NoRoute {
                class: RouteClass::DataBulk
            }
        );
    }
}
