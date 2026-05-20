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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportHealthTable {
    samples: Vec<TransportHealthSample>,
}

impl TransportHealthTable {
    pub fn local_default() -> Self {
        Self {
            samples: vec![TransportHealthSample::healthy_direct(
                TransportKind::LocalFs,
            )],
        }
    }

    pub fn replace(&mut self, samples: impl IntoIterator<Item = TransportHealthSample>) {
        self.samples = samples.into_iter().collect();
    }

    pub fn upsert(&mut self, sample: TransportHealthSample) {
        match self
            .samples
            .iter_mut()
            .find(|existing| existing.kind == sample.kind && existing.role == sample.role)
        {
            Some(existing) => *existing = sample,
            None => self.samples.push(sample),
        }
    }

    pub fn samples(&self) -> Vec<TransportHealthSample> {
        self.samples.clone()
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
        let mut table = TransportHealthTable::local_default();
        table.upsert(TransportHealthSample::healthy_direct(TransportKind::LanTcp));
        table.upsert(TransportHealthSample::down(
            TransportKind::LanTcp,
            TransportRole::Direct,
        ));

        let samples = table.samples();
        assert_eq!(samples.len(), 2);
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.kind == TransportKind::LanTcp)
                .map(|sample| sample.state),
            Some(TransportHealthState::Down)
        );
    }
}
