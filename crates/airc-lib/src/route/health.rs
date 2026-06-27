//! Health input model for transport resolution.
//!
//! Discovery/probe code feeds this layer; the resolver consumes plain
//! candidates. Keeping the conversion explicit prevents "transport
//! exists" from being confused with "transport is usable now."

use crate::route::policy::{TransportCandidate, TransportKind, TransportRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportHealthState {
    Healthy,
    Degraded,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportHealthSample {
    pub kind: TransportKind,
    pub role: TransportRole,
    pub state: TransportHealthState,
    /// Optional latency observation. `None` means not measured yet.
    pub rtt_ms: Option<u32>,
    /// Optional success rate in parts-per-million. `1_000_000` means
    /// all recent attempts succeeded.
    pub success_ppm: Option<u32>,
}

impl TransportHealthSample {
    pub fn candidate(self) -> TransportCandidate {
        TransportCandidate {
            kind: self.kind,
            role: self.role,
            healthy: matches!(self.state, TransportHealthState::Healthy),
        }
    }

    pub fn healthy_direct(kind: TransportKind) -> Self {
        Self {
            kind,
            role: TransportRole::Direct,
            state: TransportHealthState::Healthy,
            rtt_ms: None,
            success_ppm: None,
        }
    }

    pub fn down(kind: TransportKind, role: TransportRole) -> Self {
        Self {
            kind,
            role,
            state: TransportHealthState::Down,
            rtt_ms: None,
            success_ppm: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransportHealthTable {
    samples: dashmap::DashMap<(TransportKind, TransportRole), TransportHealthSample>,
}

impl TransportHealthTable {
    /// An empty health table. The local same-machine transport is the
    /// daemon's in-memory router (no health to seed); cross-machine
    /// transports register themselves as they come up.
    pub fn local_default() -> Self {
        Self::default()
    }

    pub fn replace(&self, samples: impl IntoIterator<Item = TransportHealthSample>) {
        self.samples.clear();
        for sample in samples {
            self.upsert(sample);
        }
    }

    pub fn upsert(&self, sample: TransportHealthSample) {
        self.samples.insert((sample.kind, sample.role), sample);
    }

    pub fn samples(&self) -> Vec<TransportHealthSample> {
        let mut samples = self
            .samples
            .iter()
            .map(|entry| *entry.value())
            .collect::<Vec<_>>();
        samples.sort_by_key(|sample| {
            (
                transport_kind_order(sample.kind),
                transport_role_order(sample.role),
            )
        });
        samples
    }
}

impl Default for TransportHealthTable {
    fn default() -> Self {
        Self {
            samples: dashmap::DashMap::new(),
        }
    }
}

fn transport_kind_order(kind: TransportKind) -> u8 {
    match kind {
        TransportKind::LanTcp => 1,
        TransportKind::Tailscale => 2,
        TransportKind::Udp => 3,
        TransportKind::WebRtcDataChannel => 4,
        TransportKind::Reticulum => 5,
        TransportKind::Relay => 6,
        TransportKind::Ssh => 7,
        TransportKind::GhGist => 8,
    }
}

fn transport_role_order(role: TransportRole) -> u8 {
    match role {
        TransportRole::Direct => 0,
        TransportRole::Relay => 1,
        TransportRole::InviteBeacon => 2,
        TransportRole::Rendezvous => 3,
        TransportRole::Admin => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn down_health_maps_to_unhealthy_candidate() {
        let candidate =
            TransportHealthSample::down(TransportKind::Reticulum, TransportRole::Direct)
                .candidate();

        assert_eq!(
            candidate,
            TransportCandidate {
                kind: TransportKind::Reticulum,
                role: TransportRole::Direct,
                healthy: false,
            }
        );
    }

    #[test]
    fn degraded_health_is_not_admissible_by_default() {
        let candidate = TransportHealthSample {
            kind: TransportKind::LanTcp,
            role: TransportRole::Direct,
            state: TransportHealthState::Degraded,
            rtt_ms: Some(120),
            success_ppm: Some(800_000),
        }
        .candidate();

        assert!(!candidate.healthy);
    }

    #[test]
    fn health_table_upsert_replaces_same_kind_and_role() {
        let table = TransportHealthTable::local_default();
        table.upsert(TransportHealthSample::healthy_direct(TransportKind::LanTcp));
        table.upsert(TransportHealthSample::down(
            TransportKind::LanTcp,
            TransportRole::Direct,
        ));

        let samples = table.samples();
        // `local_default()` is empty now (same-machine is the daemon), so
        // the two LanTcp upserts collapse to one replaced sample.
        assert_eq!(samples.len(), 1);
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.kind == TransportKind::LanTcp)
                .map(|sample| sample.state),
            Some(TransportHealthState::Down)
        );
    }
}
