//! The `Transport` trait — what every adapter (local-fs, lan-tcp,
//! tailscale, ...) implements.
//!
//! The substrate is intentionally agnostic about the wire underneath.
//! Adapters carry `airc-protocol::Frame`s in both directions; the
//! substrate enforces semantics (subscriptions, signature verification,
//! fan-out). One trait, many impls, swap freely.
//!
//! Designed agent-to-agent-first: the canonical caller is an AI peer
//! (Claude Code, Codex, vHSM session, persona) sending a frame to
//! other AI peers on the same Mac, same LAN, or across a mesh. Latency
//! and concurrency dominate the design; legacy human-chat features
//! (typing indicators, presence) ride on top via `FrameKind::Event`
//! and headers, not as core trait surface.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use airc_protocol::{Frame, Subscription};

/// A stream of frames matching a subscription, with transport-level
/// errors surfaced inline so receivers see I/O failures rather than
/// silently lose frames.
///
/// `Pin<Box<dyn Stream + Send>>` is dyn-friendly so adapters can be
/// stored as `Box<dyn Transport<Error = ...>>` and the receive surface
/// stays generic.
pub type FrameStream<E> = Pin<Box<dyn Stream<Item = Result<Frame, E>> + Send>>;

/// The contract an adapter implements.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Adapter-specific error type. Must be `Send + Sync + 'static` so
    /// errors can cross `await` points and propagate from spawned
    /// tasks; must implement `std::error::Error` so callers can use
    /// the standard error-chain idioms.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Send a frame onto the wire.
    ///
    /// **Delivery semantics depend on `frame.kind`** — adapters MUST
    /// respect this split so consumers like the Codex bridge can rely
    /// on it:
    ///
    /// - `FrameKind::Message` / `FrameKind::Control`: **durable**. By
    ///   the time `Ok` returns the frame is persisted (fsync'd for
    ///   local-fs; ACKed for lan-tcp). Readers attaching after this
    ///   call MUST be able to read it. These kinds carry transcript
    ///   content + session lifecycle — losing one is a bug.
    ///
    /// - `FrameKind::Event`: **interrupt-style live**. The wire makes
    ///   the frame visible to currently-attached readers as fast as
    ///   possible (skipping fsync, etc.). Crash-survival is NOT
    ///   guaranteed for events — they're meant for typing indicators,
    ///   turn/steer, turn/interrupt, presence transitions, work-offer
    ///   pings. The frame may be dropped by slow subscribers (see
    ///   `subscribe` for the lag policy).
    ///
    /// Frames are bytes-equal to what receivers see; the substrate
    /// does NOT mutate the envelope in transit. (Adapters MAY add or
    /// strip transport-level framing around the envelope — that's
    /// invisible to the protocol layer.)
    async fn send(&self, frame: Frame) -> Result<(), Self::Error>;

    /// Subscribe and receive frames matching `subscription`.
    ///
    /// The returned stream:
    ///   - Yields frames matching the subscription criteria (`channel`,
    ///     `kinds`, `headers_filter`).
    ///   - Yields `from_cursor`-anchored replay first (when set), then
    ///     transitions to live frames.
    ///   - Yields `Err(transport-error)` on transport-level failures.
    ///   - Closes when the transport is torn down OR the stream is
    ///     dropped (unsubscribes the receiver).
    ///
    /// **Lag policy (per Codex's feedback — Codex turn/interrupt and
    /// turn/steer must not get stuck behind transcript catch-up):**
    ///
    /// - `FrameKind::Message` / `FrameKind::Control` frames apply
    ///   **backpressure** — a slow consumer slows the wire's drain
    ///   rate but no frames are lost. Durable kinds get reliable
    ///   delivery.
    ///
    /// - `FrameKind::Event` frames are **lossy past the per-subscriber
    ///   buffer** — if the subscriber's queue is full, the event is
    ///   dropped at the boundary. Interrupt-style delivery: better to
    ///   miss a stale interrupt than to back up the whole wire. The
    ///   wire keeps moving; subscribers MUST poll their stream
    ///   promptly to see events.
    ///
    /// Multiple subscribers may attach concurrently; the transport
    /// fans out. Each subscriber gets its own stream — no cross-talk
    /// and no shared backpressure (a slow subscriber affects only its
    /// own backlog).
    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<FrameStream<Self::Error>, Self::Error>;
}
