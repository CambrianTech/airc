//! The IPC vocabulary mapped onto the bus's: one owner (`handlers.rs` and
//! `server.rs` each kept an identical copy).
//!
//! `IpcKind::Addressed` has no bus kind. Addressed traffic is readable by its
//! two ends only and never enters the bus or any transcript, so publishing it
//! or filtering for it is refused, never mapped: a filter that silently dropped
//! it would widen an "addressed only" subscription into every kind.

use airc_bus::envelope::{DeliveryClass, Kind};
use airc_ipc::request::{IpcDelivery, IpcKind};

/// An IPC request named addressed traffic where only bus traffic belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AddressedIsNotBusTraffic;

impl std::fmt::Display for AddressedIsNotBusTraffic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "addressed traffic never enters the bus or a transcript; send and receive it \
             through the addressed path"
        )
    }
}

pub(crate) fn bus_kind(kind: IpcKind) -> Result<Kind, AddressedIsNotBusTraffic> {
    Ok(match kind {
        IpcKind::Message => Kind::Message,
        IpcKind::Event => Kind::Event,
        IpcKind::Command => Kind::Command,
        IpcKind::CommandResult => Kind::CommandResult,
        IpcKind::Signal => Kind::Signal,
        IpcKind::StreamChunk => Kind::StreamChunk,
        IpcKind::Control => Kind::Control,
        IpcKind::Addressed => return Err(AddressedIsNotBusTraffic),
    })
}

pub(crate) fn bus_kinds(
    kinds: impl IntoIterator<Item = IpcKind>,
) -> Result<Vec<Kind>, AddressedIsNotBusTraffic> {
    kinds.into_iter().map(bus_kind).collect()
}

pub(crate) fn bus_delivery(delivery: IpcDelivery) -> DeliveryClass {
    match delivery {
        IpcDelivery::Durable => DeliveryClass::Durable,
        IpcDelivery::EphemeralLatest => DeliveryClass::EphemeralLatest,
        IpcDelivery::EphemeralWindow => DeliveryClass::EphemeralWindow,
        IpcDelivery::RequestResponse => DeliveryClass::RequestResponse,
        IpcDelivery::StreamChunk => DeliveryClass::StreamChunk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: a filter naming Addressed is refused whole. Dropping
    // the one unmappable kind would leave an empty kind set, which the router
    // reads as "every kind": an addressed-only subscriber would get the room.
    #[test]
    fn a_filter_naming_addressed_is_refused_not_widened() {
        assert_eq!(
            bus_kinds([IpcKind::Addressed]),
            Err(AddressedIsNotBusTraffic)
        );
        assert_eq!(
            bus_kinds([IpcKind::Message, IpcKind::Addressed]),
            Err(AddressedIsNotBusTraffic)
        );
        assert_eq!(bus_kinds([IpcKind::Message]), Ok(vec![Kind::Message]));
    }
}
