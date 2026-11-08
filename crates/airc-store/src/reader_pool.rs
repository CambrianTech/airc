//! How many read-only connections the durable sink opens beside its one writer.

use std::num::NonZeroUsize;

use airc_bus::BusError;

/// The size of the sink's reader pool, derived from the host.
///
/// SQLite in WAL mode (set through the ORM connect options, `sqlite::file_options`) lets any
/// number of readers run beside the one writer, and a read is CPU-bound work in the driver's
/// worker thread, so more readers than the host has hardware threads only adds contention. The
/// host's own parallelism is therefore the bound: one connection (the old pool) queued every
/// scan from every client behind the slowest, which starved the pool (card 1b0e2b64). The
/// writer stays exactly one connection (`bus_sink`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReaderPool(NonZeroUsize);

impl ReaderPool {
    /// The pool this host supports: its available parallelism. A host that cannot report it
    /// refuses the open, since a guessed size is the constant this replaces.
    pub(crate) fn of_host() -> Result<Self, BusError> {
        std::thread::available_parallelism()
            .map(Self)
            .map_err(BusError::sink_source)
    }

    /// Connections for the driver's `max_connections`.
    pub(crate) fn connections(self) -> u32 {
        u32::try_from(self.0.get()).unwrap_or(u32::MAX)
    }
}
