//! Error types for the owner-core.

use thiserror::Error;

/// Errors surfaced by the durable tier ([`crate::DurableSink`]) and the
/// router's write-behind path.
#[derive(Debug, Error)]
pub enum BusError {
    #[error("event class {0:?} cannot enter the ordinary event bus")]
    UnsupportedClass(airc_core::EventClass),
    #[error("event class {class:?} does not permit bus kind {kind:?}")]
    ClassKindMismatch {
        class: airc_core::EventClass,
        kind: crate::Kind,
    },
    /// The write-behind queue is full and a fire-and-forget durable publish
    /// was shed rather than block or OOM (§3.8 bounded write-behind). A
    /// `Durable` publisher that opted into backpressure blocks instead of
    /// seeing this.
    #[error("write-behind queue saturated; durable event shed")]
    WriteBehindSaturated,

    /// The durable tier rejected or failed an append/page.
    #[error("durable sink error: {0}")]
    Sink(String),

    /// An underlying sink failure whose typed cause remains inspectable.
    #[error("durable sink error: {0}")]
    SinkSource(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A subscriber's bounded live channel filled and the subscriber was
    /// marked lagged (§3.5). Not fatal — the subscriber resumes from the sink.
    #[error("subscriber lagged behind the live stream")]
    Lagged,
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, BusError>;

impl BusError {
    pub fn sink_source(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::SinkSource(Box::new(error))
    }
}
