//! `impl From<>` blocks bridging airc's SDK transcript vocabulary
//! (`FrameKind`, `MentionTarget`, `TranscriptCursor` — from airc-core /
//! airc-protocol) to the IPC wire vocabulary introduced in the v5
//! owner-core rewrite (`IpcKind`, `IpcTarget`, `IpcCursor`).
//!
//! ### Why these live here
//!
//! The conversions are **substrate surface** — any consumer that builds
//! `PublishRequest` from SDK-shaped state (continuum-core, Hermes,
//! OpenClaw, future grid workers, the airc CLI itself) needs them.
//! Before this module they lived as private `fn framekind_to_ipc` /
//! `fn mention_to_ipc_target` / `pack_seq` / `unpack_seq` helpers in
//! `airc-lib/src/daemon.rs`, which meant every external consumer had
//! to either re-implement the same math (drift risk) or pull all of
//! `airc-lib` to call a private fn (canonical SDK is shaped wrong for
//! drive-by use).
//!
//! Promoting them to `impl From<>` in `airc-ipc` (the smallest crate
//! that depends on both airc-core and airc-protocol) keeps the
//! translation co-located with the IPC vocabulary it produces and lets
//! every consumer write `pub_req.kind = frame_kind.into()` instead of
//! reaching for a free function or duplicating a bit-shift.
//!
//! ### Drift guard
//!
//! [`COUNTER_BITS`] is published as a `pub const` so any consumer that
//! needs to reproduce the layout (e.g. for in-memory cursor math
//! without going through the conversion) can pin against the same
//! constant — never against a literal `40`. If airc ever changes the
//! pack layout, every dependent recompiles and round-trip tests
//! everywhere catch the regression.

use airc_core::{MentionTarget, TranscriptCursor};
use airc_protocol::FrameKind;

use crate::request::{IpcCursor, IpcKind, IpcTarget};

/// Number of low-order bits in the packed `lamport` reserved for the
/// per-epoch `counter`. The high bits hold `epoch`. Pinned at 40 — see
/// `airc-lib/src/daemon.rs` (`pack_seq`/`unpack_seq`) for the historical
/// rationale (40 bits = ~1.1T counter values per epoch, far beyond any
/// real channel; the remaining 24 bits of `epoch` cover ~16M owner
/// generations).
pub const COUNTER_BITS: u32 = 40;

/// Mask for the low [`COUNTER_BITS`] bits — the per-epoch counter range.
pub const COUNTER_MASK: u64 = (1u64 << COUNTER_BITS) - 1;

impl From<FrameKind> for IpcKind {
    /// Lossless: the three chat-flavored `FrameKind` variants are a
    /// subset of the seven `IpcKind` variants. RPC/grid consumers that
    /// need `Command`/`CommandResult`/`Signal`/`StreamChunk` construct
    /// `IpcKind` directly without going through `FrameKind`.
    fn from(kind: FrameKind) -> Self {
        match kind {
            FrameKind::Message => IpcKind::Message,
            FrameKind::Event => IpcKind::Event,
            FrameKind::Control => IpcKind::Control,
        }
    }
}

impl From<MentionTarget> for IpcTarget {
    /// Lossless: room mentions round-trip as a named endpoint
    /// (`room:<uuid>`), which the inverse projection in airc-lib
    /// (`target_to_mention`) parses back into [`MentionTarget::Room`].
    /// Direct peer + broadcast pass straight through.
    fn from(target: MentionTarget) -> Self {
        match target {
            MentionTarget::All => IpcTarget::All,
            MentionTarget::Peer(peer) => IpcTarget::Peer(peer),
            MentionTarget::Room(room) => IpcTarget::Endpoint(format!("room:{}", room.as_uuid())),
        }
    }
}

impl From<TranscriptCursor> for IpcCursor {
    /// Unpack the SDK's single monotonic `lamport` into the wire
    /// vocabulary's `(epoch, counter)` split. Inverse of
    /// `IpcCursor → TranscriptCursor`. Round-trip preserves the
    /// packed value byte-for-byte.
    fn from(cursor: TranscriptCursor) -> Self {
        Self {
            epoch: cursor.lamport >> COUNTER_BITS,
            counter: cursor.lamport & COUNTER_MASK,
            event_id: cursor.event_id,
        }
    }
}

impl From<IpcCursor> for TranscriptCursor {
    /// Pack the wire vocabulary's `(epoch, counter)` into the SDK's
    /// monotonic `lamport`. Inverse of `TranscriptCursor → IpcCursor`.
    /// The daemon always produces values within the
    /// `(epoch << COUNTER_BITS) | counter` invariant, so this conversion
    /// is total + round-trip safe for any cursor the daemon emits.
    fn from(cursor: IpcCursor) -> Self {
        Self {
            lamport: (cursor.epoch << COUNTER_BITS) | (cursor.counter & COUNTER_MASK),
            event_id: cursor.event_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{EventId, RoomId};
    use uuid::Uuid;

    #[test]
    fn frame_kind_message_event_control_map_to_ipc_kind() {
        assert!(matches!(
            IpcKind::from(FrameKind::Message),
            IpcKind::Message
        ));
        assert!(matches!(IpcKind::from(FrameKind::Event), IpcKind::Event));
        assert!(matches!(
            IpcKind::from(FrameKind::Control),
            IpcKind::Control
        ));
    }

    #[test]
    fn mention_all_to_ipc_target_all() {
        assert!(matches!(
            IpcTarget::from(MentionTarget::All),
            IpcTarget::All
        ));
    }

    #[test]
    fn mention_room_to_endpoint_with_room_prefix() {
        let room = RoomId::from_uuid(Uuid::from_u128(0xA1));
        let target: IpcTarget = MentionTarget::Room(room).into();
        match target {
            IpcTarget::Endpoint(name) => {
                assert!(name.starts_with("room:"));
                assert!(name.contains(&room.as_uuid().to_string()));
            }
            other => panic!("expected Endpoint, got {other:?}"),
        }
    }

    #[test]
    fn cursor_round_trip_preserves_lamport_and_event_id() {
        let original = TranscriptCursor {
            lamport: (7u64 << COUNTER_BITS) | 99_999_999,
            event_id: EventId::from_u128(0xc0ffee),
        };
        // TranscriptCursor isn't Copy — save the comparison values
        // before the conversion consumes `original`.
        let (orig_lamport, orig_event_id) = (original.lamport, original.event_id);
        let ipc: IpcCursor = original.into();
        assert_eq!(ipc.epoch, 7);
        assert_eq!(ipc.counter, 99_999_999);
        let back: TranscriptCursor = ipc.into();
        assert_eq!(back.lamport, orig_lamport);
        assert_eq!(back.event_id, orig_event_id);
    }

    #[test]
    fn cursor_round_trip_with_zero_packs_to_zero() {
        let original = TranscriptCursor {
            lamport: 0,
            event_id: EventId::from_u128(0),
        };
        let ipc: IpcCursor = original.into();
        assert_eq!(ipc.epoch, 0);
        assert_eq!(ipc.counter, 0);
        let back: TranscriptCursor = ipc.into();
        assert_eq!(back.lamport, 0);
    }

    #[test]
    fn counter_bits_constant_matches_legacy_airc_lib_layout() {
        // Drift guard: airc-lib/src/daemon.rs hard-codes COUNTER_BITS = 40.
        // If anyone changes either, this test (or an airc-lib test
        // referencing this constant after the inline copy is deleted)
        // will fail loud. Pin via this assertion so the const isn't
        // silently shifted.
        assert_eq!(COUNTER_BITS, 40);
        assert_eq!(COUNTER_MASK, (1u64 << 40) - 1);
    }

    #[test]
    fn cursor_round_trip_at_max_counter_boundary() {
        // Counter is COUNTER_MASK (all 40 bits set), epoch is 0. This
        // is the boundary where adding one to counter would overflow
        // into epoch — the very edge the mask guards.
        let original = TranscriptCursor {
            lamport: COUNTER_MASK,
            event_id: EventId::from_u128(0xb01dface),
        };
        let (orig_lamport, orig_event_id) = (original.lamport, original.event_id);
        let ipc: IpcCursor = original.into();
        assert_eq!(ipc.epoch, 0);
        assert_eq!(ipc.counter, COUNTER_MASK);
        let back: TranscriptCursor = ipc.into();
        assert_eq!(back.lamport, orig_lamport);
        assert_eq!(back.event_id, orig_event_id);
    }

    #[test]
    fn cursor_round_trip_at_high_epoch_boundary() {
        // Epoch occupies the high (64 - 40 = 24) bits. Setting the
        // top epoch bit exercises the entire address space packing,
        // catching any sign-extension or shift-direction regression.
        // 2^23 fits cleanly in u64 << 40 without overflow.
        let epoch = 1u64 << 23;
        let counter = 12345u64;
        let original = TranscriptCursor {
            lamport: (epoch << COUNTER_BITS) | counter,
            event_id: EventId::from_u128(0xfacefeed),
        };
        let (orig_lamport, orig_event_id) = (original.lamport, original.event_id);
        let ipc: IpcCursor = original.into();
        assert_eq!(ipc.epoch, epoch);
        assert_eq!(ipc.counter, counter);
        let back: TranscriptCursor = ipc.into();
        assert_eq!(back.lamport, orig_lamport);
        assert_eq!(back.event_id, orig_event_id);
    }

    #[test]
    fn cursor_round_trip_at_u64_max_lamport() {
        // u64::MAX has every bit set — the absolute upper bound the
        // pack/unpack must handle without overflow. Epoch should be
        // (u64::MAX >> 40), counter should be COUNTER_MASK.
        let original = TranscriptCursor {
            lamport: u64::MAX,
            event_id: EventId::from_u128(0xdeadbeef),
        };
        let (orig_lamport, orig_event_id) = (original.lamport, original.event_id);
        let ipc: IpcCursor = original.into();
        assert_eq!(ipc.epoch, u64::MAX >> COUNTER_BITS);
        assert_eq!(ipc.counter, COUNTER_MASK);
        let back: TranscriptCursor = ipc.into();
        assert_eq!(back.lamport, orig_lamport);
        assert_eq!(back.event_id, orig_event_id);
    }
}
