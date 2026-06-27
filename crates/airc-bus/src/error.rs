//! Error types for the owner-core.

use thiserror::Error;

/// Errors surfaced by the durable tier ([`crate::DurableSink`]) and the
/// router's write-behind path.
#[derive(Debug, Error)]
pub enum BusError {
    /// The write-behind queue is full and a fire-and-forget durable publish
    /// was shed rather than block or OOM (§3.8 bounded write-behind). A
    /// `Durable` publisher that opted into backpressure blocks instead of
    /// seeing this.
    #[error("write-behind queue saturated; durable event shed")]
    WriteBehindSaturated,

    /// The durable tier rejected or failed an append/page.
    #[error("durable sink error: {0}")]
    Sink(String),

    /// A subscriber's bounded live channel filled and the subscriber was
    /// marked lagged (§3.5). Not fatal — the subscriber resumes from the sink.
    #[error("subscriber lagged behind the live stream")]
    Lagged,
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, BusError>;
