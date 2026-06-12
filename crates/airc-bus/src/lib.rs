//! airc-bus — the owner-daemon event-server core (slice 1 of
//! `docs/architecture/AIRC-EVENT-SERVER.md`).
//!
//! This crate is the **in-memory, deterministic heart** of the owner daemon:
//! the sharded router (§3.1), the per-channel hot ring (§3.2), the coalesced
//! ephemeral cache (§3.4), the cursor engine (§3.5), and the write-behind path
//! to a durable tier (§3.3) — with the crash-safety / ordering / backpressure
//! invariants (§3.8) baked into the types, not assumed away.
//!
//! ## What this crate deliberately is NOT
//!
//! - **No IPC wiring.** The attach/publish/ack session protocol (§3.6) is a
//!   later vertical. The public API here is the in-process router.
//! - **No `airc-store` dependency.** The durable tier (§3.3) is behind the
//!   [`DurableSink`] trait; the ORM-backed impl lands in a later slice. Tests
//!   use [`InMemoryDurableSink`].
//! - **No file-poll deletion.** Removing `frames.jsonl` / `LocalFsAdapter`
//!   (§7) is a separate vertical and is not touched here.
//!
//! Its purpose is to **de-risk the hardest correctness seams** — generational
//! order (§3.8), the no-gap cursor (§3.5), and the slow-subscriber guarantee
//! (§3.5) — in pure, deterministic code with injectable [`Clock`] +
//! [`SeqSource`] (§9), no global mutable state, and no lock held across an
//! `.await`.
//!
//! ## Module layout
//!
//! - [`envelope`] — the generic [`Envelope`] (§2), [`Seq`], [`Cursor`],
//!   [`Target`], [`Kind`], [`DeliveryClass`].
//! - [`clock`] — injectable [`Clock`] (+ [`ManualClock`] for tests).
//! - [`seq`] — generational [`SeqSource`] + persisted [`EpochStore`].
//! - [`sink`] — the [`DurableSink`] trait + [`InMemoryDurableSink`].
//! - [`ring`] — the per-channel [`HotRing`] (pinned-until-persisted).
//! - [`ephemeral`] — the coalesced [`EphemeralCache`] (latest-wins + TTL).
//! - [`filter`] — the subscription [`Filter`] predicate.
//! - [`router`] — the [`EventRouter`] (publish + subscribe + write-behind).

pub mod clock;
pub mod envelope;
pub mod ephemeral;
pub mod error;
pub mod filter;
pub mod ring;
pub mod router;
pub mod seq;
pub mod sink;

pub use clock::{Clock, ManualClock, SystemClock};
pub use envelope::{Cursor, DeliveryClass, Envelope, Kind, Seq, Target};
pub use ephemeral::EphemeralCache;
pub use error::{BusError, Result};
pub use filter::Filter;
pub use ring::HotRing;
pub use router::{EventRouter, LagFlag, PublishIfNew, RouterConfig};
pub use seq::{EpochStore, InMemoryEpochStore, SeqSource};
pub use sink::{DurableSink, InMemoryDurableSink};
