//! The encode / decode / read-view surface.
//!
//! ## Zero-copy payload — the whole point
//!
//! [`encode`] lays the opaque `payload` bytes into the FlatBuffer once.
//! [`decode`] takes ownership of the buffer as [`bytes::Bytes`], gets the
//! payload as a `&[u8]` slice *into that buffer* from the FlatBuffers
//! accessor, then reconstructs `Envelope.payload` with
//! [`bytes::Bytes::slice_ref`] — which returns a `Bytes` that shares the
//! original allocation (a refcount bump + pointer/len, no `memcpy`). The
//! payload is never decoded, re-encoded, or copied.
//!
//! [`WireEnvelope`] reads routing fields straight out of the buffer in
//! place, and its [`WireEnvelope::payload`] returns the same in-buffer
//! slice — so a router can fan out without ever materializing the full
//! [`Envelope`].

use bytes::Bytes;
use uuid::Uuid;

use airc_bus::envelope::{DeliveryClass, Envelope, Kind, Seq, Target};
use airc_core::{ClientId, EventId, Headers, PeerId, RoomId};

use crate::error::WireError;
use crate::wire_generated::airc::wire as fb;

// ---------------------------------------------------------------------------
// UUID <-> inline struct
// ---------------------------------------------------------------------------

/// Split a `Uuid` into the inline 16-byte FlatBuffers struct. `hi`/`lo` are
/// the big-endian 64-bit halves (`hi` = most-significant), via
/// [`Uuid::as_u64_pair`] so the mapping is exact and reversible.
fn uuid_to_fb(id: Uuid) -> fb::Uuid {
    let (hi, lo) = id.as_u64_pair();
    fb::Uuid { hi, lo }
}

/// Rebuild a `Uuid` from the inline struct read view (`hi`/`lo` halves).
fn uuid_from_ref(r: fb::UuidRef<'_>) -> Result<Uuid, WireError> {
    Ok(Uuid::from_u64_pair(r.hi(), r.lo()))
}

/// Read a required UUID id field: absent => [`WireError::MissingField`].
fn require_uuid(field: Option<fb::UuidRef<'_>>, name: &'static str) -> Result<Uuid, WireError> {
    match field {
        Some(r) => uuid_from_ref(r),
        None => Err(WireError::MissingField(name)),
    }
}

// ---------------------------------------------------------------------------
// Kind / DeliveryClass <-> FlatBuffers enums (exhaustive, no wildcard)
// ---------------------------------------------------------------------------

fn kind_to_fb(kind: Kind) -> fb::Kind {
    match kind {
        Kind::Message => fb::Kind::Message,
        Kind::Event => fb::Kind::Event,
        Kind::Command => fb::Kind::Command,
        Kind::CommandResult => fb::Kind::CommandResult,
        Kind::Signal => fb::Kind::Signal,
        Kind::StreamChunk => fb::Kind::StreamChunk,
        Kind::Control => fb::Kind::Control,
    }
}

fn kind_from_fb(kind: fb::Kind) -> Kind {
    match kind {
        fb::Kind::Message => Kind::Message,
        fb::Kind::Event => Kind::Event,
        fb::Kind::Command => Kind::Command,
        fb::Kind::CommandResult => Kind::CommandResult,
        fb::Kind::Signal => Kind::Signal,
        fb::Kind::StreamChunk => Kind::StreamChunk,
        fb::Kind::Control => Kind::Control,
    }
}

fn delivery_to_fb(delivery: DeliveryClass) -> fb::DeliveryClass {
    match delivery {
        DeliveryClass::Durable => fb::DeliveryClass::Durable,
        DeliveryClass::EphemeralLatest => fb::DeliveryClass::EphemeralLatest,
        DeliveryClass::EphemeralWindow => fb::DeliveryClass::EphemeralWindow,
        DeliveryClass::RequestResponse => fb::DeliveryClass::RequestResponse,
        DeliveryClass::StreamChunk => fb::DeliveryClass::StreamChunk,
    }
}

fn delivery_from_fb(delivery: fb::DeliveryClass) -> DeliveryClass {
    match delivery {
        fb::DeliveryClass::Durable => DeliveryClass::Durable,
        fb::DeliveryClass::EphemeralLatest => DeliveryClass::EphemeralLatest,
        fb::DeliveryClass::EphemeralWindow => DeliveryClass::EphemeralWindow,
        fb::DeliveryClass::RequestResponse => DeliveryClass::RequestResponse,
        fb::DeliveryClass::StreamChunk => DeliveryClass::StreamChunk,
    }
}

// ---------------------------------------------------------------------------
// encode
// ---------------------------------------------------------------------------

/// Encode an [`Envelope`] into the airc wire FlatBuffer — the one
/// sanctioned encode.
///
/// This builds the FlatBuffer once and returns its bytes as [`Bytes`]. The
/// opaque `payload` is laid into the buffer verbatim (it is the only large
/// field); every other field is a small scalar / string / id. The returned
/// `Bytes` is a standalone, self-describing buffer that [`decode`] or
/// [`WireEnvelope::read`] can read back.
///
/// `encode` is infallible: a valid `Envelope` always maps to a well-formed
/// FlatBuffer. `headers` is emitted sorted by key (the source `Headers` is a
/// `BTreeMap`, already key-ordered) so the bytes are deterministic.
pub fn encode(env: &Envelope) -> Bytes {
    let mut builder = planus::Builder::new();

    // Strings & vectors must be prepared into the builder before the table
    // that references them. planus' generated `create` does this for us by
    // taking `impl WriteAsOptional<...>`; we hand it owned/borrowed values
    // and it prepares them in the right order.

    // Headers: BTreeMap<String, String> -> [Header]. Already key-sorted.
    let headers: Vec<fb::Header> = env
        .headers
        .iter()
        .map(|(k, v)| fb::Header {
            key: Some(k.clone()),
            value: Some(v.clone()),
        })
        .collect();

    // Target -> (tag, per-variant fields). Exhaustive; the variant that
    // owns each field is documented in `schema/wire.fbs`.
    let (target_tag, target_peer, target_reply, target_text): (
        fb::TargetTag,
        Option<fb::Uuid>,
        Option<fb::Uuid>,
        Option<String>,
    ) = match &env.target {
        Target::All => (fb::TargetTag::All, None, None, None),
        Target::Peer(p) => (
            fb::TargetTag::Peer,
            Some(uuid_to_fb(p.as_uuid())),
            None,
            None,
        ),
        Target::Reply(id) => (fb::TargetTag::Reply, None, Some(uuid_to_fb(*id)), None),
        Target::Endpoint(s) => (fb::TargetTag::Endpoint, None, None, Some(s.clone())),
        Target::Capability(s) => (fb::TargetTag::Capability, None, None, Some(s.clone())),
    };

    let offset = fb::Envelope::create(
        &mut builder,
        Some(uuid_to_fb(env.event_id.as_uuid())),
        Some(uuid_to_fb(env.channel.as_uuid())),
        Some(uuid_to_fb(env.from.0.as_uuid())),
        Some(uuid_to_fb(env.from.1.as_uuid())),
        kind_to_fb(env.kind),
        delivery_to_fb(env.delivery),
        env.seq.epoch,
        env.seq.counter,
        env.occurred_at_ms,
        env.correlation_id.map(uuid_to_fb),
        env.coalesce_key.as_deref(),
        target_tag,
        target_peer,
        target_reply,
        target_text.as_deref(),
        Some(headers.as_slice()),
        Some(env.payload.as_ref()),
    );

    // `finish` borrows the builder's internal buffer; copy it out into an
    // owned `Bytes` (one allocation for the whole FlatBuffer — the
    // sanctioned encode cost). The payload bytes were already laid in by
    // `create`; decode re-shares them zero-copy.
    Bytes::copy_from_slice(builder.finish(offset, None))
}

// ---------------------------------------------------------------------------
// decode
// ---------------------------------------------------------------------------

/// Decode a wire buffer back into a Rust [`Envelope`].
///
/// Takes the buffer by value as [`Bytes`] because the reconstructed
/// `Envelope.payload` is a **zero-copy** slice into it: the metadata fields
/// are read out (cheap copies of a few scalars / short strings), then
/// `payload` is set via [`Bytes::slice_ref`], which shares `buf`'s
/// allocation — the opaque payload is never copied.
pub fn decode(buf: Bytes) -> Result<Envelope, WireError> {
    // Borrow the buffer to read the FlatBuffer; the resulting refs are tied
    // to this borrow and dropped before we move `buf` into the payload.
    let view = WireEnvelope::read(&buf)?;

    let event_id = EventId::from_uuid(require_uuid(view.event_id_ref()?, "event_id")?);
    let channel = RoomId::from_uuid(require_uuid(view.channel_ref()?, "channel")?);
    let peer_id = PeerId::from_uuid(require_uuid(view.peer_id_ref()?, "peer_id")?);
    let client_id = ClientId::from_uuid(require_uuid(view.client_id_ref()?, "client_id")?);

    let kind = view.kind()?;
    let delivery = view.delivery()?;
    let seq = view.seq()?;
    let occurred_at_ms = view.occurred_at_ms()?;
    let correlation_id = view.correlation_id()?;
    let coalesce_key = view.coalesce_key()?.map(str::to_owned);
    let target = view.target()?;
    let headers = view.headers_map()?;

    // The one byte range we must locate before dropping the borrow: where
    // the payload lives inside `buf`. We compute it as a sub-slice of the
    // borrowed buffer, then `slice_ref` it against the owned `buf` — same
    // allocation, no copy.
    let payload = match view.payload()? {
        Some(slice) => buf.slice_ref(slice),
        // A well-formed envelope always carries `payload` (possibly empty);
        // its absence means the field was dropped from the table.
        None => return Err(WireError::MissingField("payload")),
    };

    Ok(Envelope {
        event_id,
        channel,
        from: (peer_id, client_id),
        target,
        kind,
        delivery,
        seq,
        occurred_at_ms,
        correlation_id,
        coalesce_key,
        headers,
        payload,
    })
}

// ---------------------------------------------------------------------------
// WireEnvelope — zero-copy read view
// ---------------------------------------------------------------------------

/// A zero-copy read view over an encoded wire buffer.
///
/// Wraps the `planus` [`EnvelopeRef`](fb::EnvelopeRef): every accessor reads
/// its field straight out of `buf` in place, allocating nothing for the
/// routing scalars. [`WireEnvelope::payload`] returns a `&'buf [u8]` slice
/// INTO the buffer — the router can fan a media chunk out without ever
/// building the full [`Envelope`].
///
/// Field accessors return [`WireError`] because a hostile / truncated buffer
/// can have a malformed offset for any field; reading is fallible by design.
#[derive(Clone, Copy)]
pub struct WireEnvelope<'buf> {
    inner: fb::EnvelopeRef<'buf>,
}

impl<'buf> WireEnvelope<'buf> {
    /// Parse the FlatBuffer root from a borrowed buffer. Cheap: validates
    /// the root offset/vtable, reads no field bodies.
    pub fn read(buf: &'buf [u8]) -> Result<Self, WireError> {
        use planus::ReadAsRoot;
        let inner = fb::EnvelopeRef::read_as_root(buf)?;
        Ok(Self { inner })
    }

    /// The opaque payload as a slice INTO the buffer — zero copy. `None`
    /// only if the field is absent (a malformed envelope; [`decode`] treats
    /// that as an error).
    pub fn payload(&self) -> Result<Option<&'buf [u8]>, WireError> {
        Ok(self.inner.payload()?)
    }

    /// Envelope category (§2).
    pub fn kind(&self) -> Result<Kind, WireError> {
        Ok(kind_from_fb(self.inner.kind()?))
    }

    /// Delivery / retention class (§2, §3.3, §3.4).
    pub fn delivery(&self) -> Result<DeliveryClass, WireError> {
        Ok(delivery_from_fb(self.inner.delivery()?))
    }

    /// The channel/room id this envelope belongs to.
    pub fn channel(&self) -> Result<RoomId, WireError> {
        Ok(RoomId::from_uuid(require_uuid(
            self.inner.channel()?,
            "channel",
        )?))
    }

    /// The stable event id.
    pub fn event_id(&self) -> Result<EventId, WireError> {
        Ok(EventId::from_uuid(require_uuid(
            self.inner.event_id()?,
            "event_id",
        )?))
    }

    /// Owner-assigned total order `seq = (epoch, counter)` (§2, §3.8).
    pub fn seq(&self) -> Result<Seq, WireError> {
        Ok(Seq::new(self.inner.epoch()?, self.inner.counter()?))
    }

    /// Owner-stamped wall clock (ms).
    pub fn occurred_at_ms(&self) -> Result<u64, WireError> {
        Ok(self.inner.occurred_at_ms()?)
    }

    /// Command ↔ result / request ↔ response correlation, if present.
    pub fn correlation_id(&self) -> Result<Option<Uuid>, WireError> {
        match self.inner.correlation_id()? {
            Some(r) => Ok(Some(uuid_from_ref(r)?)),
            None => Ok(None),
        }
    }

    /// Coalescing key for [`DeliveryClass::EphemeralLatest`] (§3.4), if set.
    /// Returns a borrow into the buffer — no allocation.
    pub fn coalesce_key(&self) -> Result<Option<&'buf str>, WireError> {
        Ok(self.inner.coalesce_key()?)
    }

    /// Addressing target (§2), reconstructed from the tag + per-variant
    /// field. Allocates only for the string-bearing variants (`Endpoint`,
    /// `Capability`).
    pub fn target(&self) -> Result<Target, WireError> {
        match self.inner.target_tag()? {
            fb::TargetTag::All => Ok(Target::All),
            fb::TargetTag::Peer => {
                let p = require_uuid(self.inner.target_peer()?, "target_peer")?;
                Ok(Target::Peer(PeerId::from_uuid(p)))
            }
            fb::TargetTag::Reply => {
                let id = require_uuid(self.inner.target_reply()?, "target_reply")?;
                Ok(Target::Reply(id))
            }
            fb::TargetTag::Endpoint => match self.inner.target_text()? {
                Some(s) => Ok(Target::Endpoint(s.to_owned())),
                None => Err(WireError::MissingField("target_text")),
            },
            fb::TargetTag::Capability => match self.inner.target_text()? {
                Some(s) => Ok(Target::Capability(s.to_owned())),
                None => Err(WireError::MissingField("target_text")),
            },
        }
    }

    /// A lazy, zero-copy view over the header vector. Iterating reads each
    /// `(key, value)` pair straight out of the buffer. If the headers field
    /// is absent, the iterator is empty.
    pub fn headers(&self) -> WireHeaders<'buf> {
        WireHeaders {
            inner: self.inner.headers().ok().flatten(),
            pos: 0,
        }
    }

    /// Materialize the headers into a Rust [`Headers`] (`BTreeMap`). This
    /// allocates the map; use [`WireEnvelope::headers`] to read in place.
    pub fn headers_map(&self) -> Result<Headers, WireError> {
        let mut map = Headers::new();
        if let Some(vec) = self.inner.headers()? {
            for entry in vec.iter() {
                let entry = entry?;
                let key = match entry.key()? {
                    Some(k) => k,
                    None => return Err(WireError::MissingField("header.key")),
                };
                // `value` is schema-optional; a null value decodes to "" so
                // the rebuilt map matches the encoded shape.
                let value = entry.value()?.unwrap_or("");
                map.insert(key.to_owned(), value.to_owned());
            }
        }
        Ok(map)
    }

    // --- internal id-ref accessors used by `decode` (avoid re-parsing) ---

    fn event_id_ref(&self) -> Result<Option<fb::UuidRef<'buf>>, WireError> {
        Ok(self.inner.event_id()?)
    }
    fn channel_ref(&self) -> Result<Option<fb::UuidRef<'buf>>, WireError> {
        Ok(self.inner.channel()?)
    }
    fn peer_id_ref(&self) -> Result<Option<fb::UuidRef<'buf>>, WireError> {
        Ok(self.inner.peer_id()?)
    }
    fn client_id_ref(&self) -> Result<Option<fb::UuidRef<'buf>>, WireError> {
        Ok(self.inner.client_id()?)
    }
}

/// A lazy iterator over an envelope's header `(key, value)` pairs, reading
/// each entry straight out of the buffer with no intermediate allocation.
///
/// `key` is required per the schema; a header missing its `key` ends
/// iteration with an error item. `value` defaults to `""` if absent (a
/// FlatBuffers string field can be null), matching how [`decode`] rebuilds
/// the `BTreeMap`.
pub struct WireHeaders<'buf> {
    inner: Option<planus::Vector<'buf, Result<fb::HeaderRef<'buf>, planus::Error>>>,
    pos: usize,
}

impl<'buf> Iterator for WireHeaders<'buf> {
    type Item = Result<(&'buf str, &'buf str), WireError>;

    fn next(&mut self) -> Option<Self::Item> {
        let vec = self.inner.as_ref()?;
        let entry = vec.get(self.pos)?;
        self.pos += 1;
        let mapped = (|| {
            let entry = entry?;
            let key = match entry.key()? {
                Some(k) => k,
                None => return Err(WireError::MissingField("header.key")),
            };
            let value = entry.value()?.unwrap_or("");
            Ok((key, value))
        })();
        Some(mapped)
    }
}
