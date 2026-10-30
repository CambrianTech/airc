use airc_core::{Body, EventId, Headers, MentionTarget, TranscriptCursor, TranscriptEvent};
use airc_protocol::{Envelope, Frame, FrameKind, Signature};
use std::sync::Arc;

use crate::error::AircError;
use crate::route::{RouteClass, RouteDecision, TransportResolver, TransportRoute};
use crate::stream::{EventFilter, EventScan, EventStream, FilteredEventStream};
use crate::time::now_ms;
use crate::Airc;

/// Event metadata returned by [`Airc::send_frame_to_room`]. Carries
/// enough to build a typed receipt for the public publish API.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SendFrameResult {
    pub event_id: EventId,
    pub lamport: u64,
    pub occurred_at_ms: u64,
}

impl Airc {
    /// Send a plain-text message to the current room.
    pub async fn say(&self, text: &str) -> Result<EventId, AircError> {
        self.say_with_headers(text, Headers::new()).await
    }

    /// Send a plain-text message with envelope headers.
    pub async fn say_with_headers(
        &self,
        text: &str,
        headers: Headers,
    ) -> Result<EventId, AircError> {
        if self.is_daemon_attached() {
            let room = self.current_room().await?;
            return self.daemon_send_text(&room, text, headers).await;
        }
        self.send(Body::text(text), headers).await
    }

    /// Send a frame with typed body and arbitrary headers.
    pub async fn send(&self, body: Body, headers: Headers) -> Result<EventId, AircError> {
        self.send_frame_to(
            FrameKind::Message,
            airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            MentionTarget::All,
            body,
            headers,
        )
        .await
    }

    pub(crate) async fn send_frame(
        &self,
        kind: FrameKind,
        class: airc_core::EventClass,
        body: Body,
        headers: Headers,
    ) -> Result<EventId, AircError> {
        self.send_frame_to(kind, class, MentionTarget::All, body, headers)
            .await
    }

    /// Test-only public alias for [`Airc::send_frame_to`]. Hidden
    /// from docs because the public send surface is owned by
    /// `say`/`request`/`reply`; this exists so transport-wiring
    /// integration tests can target a specific `FrameKind` (and
    /// therefore a specific `RouteClass`) without going through the
    /// command-bus.
    #[doc(hidden)]
    pub async fn send_frame_to_for_test(
        &self,
        kind: FrameKind,
        class: airc_core::EventClass,
        target: MentionTarget,
        body: Body,
        headers: Headers,
    ) -> Result<EventId, AircError> {
        self.send_frame_to(kind, class, target, body, headers).await
    }

    pub(crate) async fn send_frame_to(
        &self,
        kind: FrameKind,
        class: airc_core::EventClass,
        target: MentionTarget,
        body: Body,
        headers: Headers,
    ) -> Result<EventId, AircError> {
        let room = self.current_room().await?;
        self.send_frame_to_room(kind, class, target, body, headers, &room)
            .await
            .map(|receipt| receipt.event_id)
    }

    /// Send a frame to a specific room without changing this scope's
    /// notion of "current room". Returns the full event metadata so
    /// callers can produce a typed receipt.
    ///
    /// This is the substrate-level publish primitive that
    /// [`Airc::publish`](crate::Airc::publish) composes onto a typed
    /// [`PublishTarget`](crate::PublishTarget). Existing
    /// `say`/`send`/`send_frame_to` paths keep their
    /// current-room-only behaviour by funnelling through here.
    pub(crate) async fn send_frame_to_room(
        &self,
        kind: FrameKind,
        class: airc_core::EventClass,
        target: MentionTarget,
        body: Body,
        mut headers: Headers,
        room: &crate::Room,
    ) -> Result<SendFrameResult, AircError> {
        if kind != FrameKind::for_class(class) {
            return Err(AircError::Route(
                "frame kind conflicts with event class".into(),
            ));
        }
        // Cross-machine name reconvergence (blind-room heal): every
        // room send carries the human channel name so a receiver whose
        // identity-scoped channel derivation diverged from ours can
        // re-derive the room under its own identity and still deliver.
        room.stamp_name_header(&mut headers);
        // Daemon-attached: ALL structured sends (publish, work events,
        // lifecycle) route through the daemon's router — not just `say`.
        // Keeps the write path consistent with the daemon read path.
        if self.is_daemon_attached() {
            return self
                .daemon_send_frame(room, kind, class, target, body, headers)
                .await;
        }
        // Debounced: the verifier registry is for INBOUND verification and
        // rarely changes between sends; loading it from disk per outbound
        // message was ~95% of send latency (profiled). Sync at most once/sec.
        self.sync_account_peer_registry_debounced().await?;
        let event_id = EventId::new();
        let occurred_at_ms = now_ms()?;
        let lamport = self.next_lamport(occurred_at_ms);
        let mut frame = Frame {
            kind,
            envelope: Envelope {
                class,
                event_id,
                sender: self.inner.identity.peer_id,
                sender_client: self.inner.identity.client_id,
                channel: room.channel,
                target,
                lamport,
                occurred_at_ms,
                reply_to: None,
                headers,
                body: Some(body),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        };
        frame.envelope.signature = airc_diagnostics::timing::timed("airc.sign", || {
            self.inner.identity.keypair.sign_envelope(
                &frame.envelope,
                self.inner.identity.peer_id,
                0,
            )
        })
        .map_err(|error| AircError::Crypto(error.to_string()))?;
        // Deliver-first, persist-then-transport. `append_sent_frame`
        // persists to the local ORM (durability source of truth) and
        // fans out to in-process subscribers via `live_tx`;
        // `execute_send_route` writes the frame to the wire for
        // cross-process/remote delivery and does a ~27ms `fsync` for
        // Durable/Control frames. Running the wire write FIRST put that
        // fsync stall in front of every local delivery — see
        // docs/realtime-event-bus.md "Decoupled Delivery". Local
        // subscribers now see the event as soon as the ORM append
        // commits (sub-ms); the wire write no longer blocks fan-out.
        //
        // Ordering/dedup is preserved: `append_sent_frame` marks the
        // event_id in the broadcast ring BEFORE the wire write, so the
        // wire subscriber's later re-read of the same frame still skips
        // a duplicate fan-out.
        let __append = airc_diagnostics::timing::start();
        self.append_sent_frame(frame.clone()).await?;
        __append.stop("airc.append_sent");

        // TRANSPORT IS NOT A PRECONDITION FOR MEMORY.
        //
        // Route resolution used to happen ABOVE, before the frame was even
        // built, so `?` on a refusal returned early and the append never ran.
        // Persist-then-transport was already the design (see the comment
        // above); an accidental ordering made durability conditional on the
        // network anyway.
        //
        // What that cost, measured on the M5 2026-08-04: a fresh scope could
        // not say anything into its own room — `say` failed outright with
        // "DataInteractive has no admissible live route" — and the live store
        // held 504,013 events of which ZERO were messages (503,656 were
        // `subscription_advanced` cursor rows). A room that only remembers
        // what it successfully broadcast is not a transcript, and every
        // read-side feature (persona wake-hydration, recall, repetition
        // detection, the transcript UI) was compensating for a room with no
        // history.
        //
        // Now: the event is durable and locally visible FIRST. A transport
        // failure is reported loudly and does not erase what was said. It is
        // deliberately not an `Err` — the send genuinely succeeded as an act
        // of record, and whether a peer RECEIVED it is a different question
        // with its own answer (the delivery-ack ledger, #280). Conflating
        // "wrote it down" with "you heard it" is what made an offline node
        // mute.
        let __route = airc_diagnostics::timing::start();
        match self.resolve_send_route(kind, &frame.envelope.target).await {
            Ok(route) => {
                if let Err(error) = self.execute_send_route(route.kind, frame).await {
                    tracing::warn!(
                        target: "airc::messaging",
                        %event_id,
                        room = %room.name,
                        %error,
                        "message is durable and locally delivered, but the wire write \
                         failed — remote peers have NOT received it; delivery receipts \
                         are the authority on who actually got it"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    target: "airc::messaging",
                    %event_id,
                    room = %room.name,
                    %error,
                    "message is durable and locally delivered, but NO transport route \
                     is admissible right now — nothing was sent to remote peers. The \
                     record stands; delivery is deferred to route recovery"
                );
            }
        }
        __route.stop("airc.exec_route");
        Ok(SendFrameResult {
            event_id,
            lamport,
            occurred_at_ms,
        })
    }

    pub(crate) async fn resolve_send_route(
        &self,
        kind: FrameKind,
        target: &airc_core::MentionTarget,
    ) -> Result<TransportRoute, AircError> {
        let class = route_class_for_frame(kind);
        // Peer-directed frames may ride the per-peer direct rung — but only
        // on the EVIDENCE of an established DataChannel to that exact peer
        // (see RouteTarget docs; a phantom claim would route frames into
        // "no webrtc channel for peer" instead of a live Relay).
        let route_target = match target {
            airc_core::MentionTarget::Peer(peer) => crate::route::policy::RouteTarget::Peer {
                webrtc_channel_ready: self.has_webrtc_channel(*peer).await,
            },
            airc_core::MentionTarget::All | airc_core::MentionTarget::Room(_) => {
                crate::route::policy::RouteTarget::Broadcast
            }
        };
        let mut candidates = self
            .inner
            .route_health
            .samples()
            .into_iter()
            .map(crate::route::health::TransportHealthSample::candidate)
            .collect::<Vec<_>>();
        // The per-peer DataChannel never registers in the kind-keyed health
        // table (one row can't speak for N peers); the registered channel to
        // THIS peer is its own health evidence, injected per-resolution.
        if matches!(
            route_target,
            crate::route::policy::RouteTarget::Peer {
                webrtc_channel_ready: true
            }
        ) {
            candidates.push(crate::route::policy::TransportCandidate {
                kind: crate::route::policy::TransportKind::WebRtcDataChannel,
                role: crate::route::policy::TransportRole::Direct,
                healthy: true,
            });
        }
        TransportResolver::new(candidates)
            .resolve_for_target(class, route_target)
            .map_err(format_route_refusal)
    }

    async fn append_sent_frame(&self, frame: Frame) -> Result<(), AircError> {
        // Persist to the local store AND fan out to live_tx for
        // in-process subscribers. Record the event_id in the
        // recently-broadcast ring so the wire subscriber's later
        // re-read of the same frame (we just wrote it to disk) skips
        // a duplicate fan-out.
        //
        // Without the ring, two paths would broadcast the same
        // event: here (fast, synchronous with send), and the
        // wire-subscriber's tail-loop (50ms later). Subscribers
        // would see every locally-originated message twice.
        //
        // The pair to this is `append_received_frame`, which DOES
        // fan out on duplicate-id when the event isn't in the ring
        // — that's the cross-process delivery path (another
        // process on the same AIRC_HOME wrote the frame, our wire
        // subscriber reads it, the store says DuplicateEventId
        // because the sender already persisted, but our local
        // subscribers haven't seen it).
        self.observe_presence(&frame);
        let event = frame
            .into_transcript_event()
            .map_err(|refusal| AircError::Route(refusal.to_string()))?;
        let event_id = event.event_id;
        // Routed live, never history: a non-durable class (a backfill reply is
        // a whole page of frames) fans out to live subscribers and stops there.
        if !crate::publish::is_transcript_history(&event.headers) {
            if self.mark_broadcast(event_id) {
                let _ = self.inner.live_tx.send(Arc::new(event));
            }
            return Ok(());
        }
        let persist_result = self.inner.store.append(event.clone()).await;
        match persist_result {
            Ok(()) | Err(airc_store::StoreError::DuplicateEventId(_)) => {
                if self.mark_broadcast(event_id) {
                    let _ = self.inner.live_tx.send(Arc::new(event));
                }
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Subscribe to the live event stream.
    pub async fn subscribe(&self) -> Result<EventStream, AircError> {
        let room = self.current_room().await?;
        if self.is_daemon_attached() {
            return self
                .daemon_subscribe(
                    vec![room.channel],
                    None,
                    airc_core::HeaderFilter::Any,
                    airc_core::EventSelection::All,
                )
                .await;
        }
        let rx = self.inner.live_tx.subscribe();
        Ok(EventStream::from_broadcast(rx))
    }

    /// Subscribe to live events matching `filter`. If the filter does
    /// not specify a channel, it is scoped to the current room.
    pub async fn subscribe_filtered(
        &self,
        filter: EventFilter,
    ) -> Result<FilteredEventStream, AircError> {
        let filter = self.scope_filter_to_current_room(filter).await?;
        let inner = if self.is_daemon_attached() {
            let channel = filter
                .channel
                .ok_or_else(|| AircError::Route("subscription requires a channel".into()))?;
            self.daemon_subscribe(
                vec![channel],
                None,
                filter.headers_filter.clone(),
                filter.selection.clone(),
            )
            .await?
        } else {
            EventStream::from_broadcast(self.inner.live_tx.subscribe())
        };
        Ok(FilteredEventStream { inner, filter })
    }

    /// Subscribe to live events from all subscribed rooms. This is
    /// the monitor/hook surface: no hidden narrowing to current room.
    pub async fn subscribe_subscribed_filtered(
        &self,
        filter: EventFilter,
    ) -> Result<FilteredEventStream, AircError> {
        self.subscribe_subscribed_delivery(filter, None).await
    }

    /// [`Self::subscribe_subscribed_filtered`] with a ROUTER-SIDE
    /// delivery-class filter: when `delivery` is `Some`, the daemon only
    /// fans out events of those classes to this subscription — nothing
    /// else ever crosses the socket. The caller's
    /// `filter.headers_filter` also rides the attach, so header
    /// predicates (including `HeaderFilter::Not` exclusions — e.g.
    /// dropping `airc.heartbeat.*`-stamped liveness beacons) are
    /// enforced BEFORE fan-out too, not just client-side after a
    /// decode the consumer paid for. This is the consumer-shaped
    /// subscription [`airc_ipc::AttachRequest`] always supported and
    /// clients never used: a mind that perceives settled room lines
    /// attaches with `[IpcDelivery::Durable]` and stops paying a decode
    /// per peer StreamChunk / ephemeral fan-out (measured on a continuum
    /// node 2026-08-15: 6,736 events crossed to every persona in 40
    /// minutes, 100% discarded post-decode). The `EventFilter` still
    /// applies client-side on top, and is the ONLY filter on the
    /// non-daemon (in-process broadcast) fallback, which has no router
    /// to filter at.
    pub async fn subscribe_subscribed_delivery(
        &self,
        filter: EventFilter,
        delivery: Option<Vec<airc_ipc::IpcDelivery>>,
    ) -> Result<FilteredEventStream, AircError> {
        let filter = self.subscribed_event_filter(filter).await?;
        if self.is_daemon_attached() {
            let channels: Vec<airc_core::RoomId> = self
                .subscription_set()
                .await?
                .all()
                .map(|sub| sub.as_room().channel)
                .collect();
            return Ok(FilteredEventStream {
                inner: self
                    .daemon_subscribe(
                        channels,
                        delivery,
                        filter.headers_filter.clone(),
                        filter.selection.clone(),
                    )
                    .await?,
                filter,
            });
        }
        let rx = self.inner.live_tx.subscribe();
        Ok(FilteredEventStream {
            inner: EventStream::from_broadcast(rx),
            filter,
        })
    }

    /// Fetch the most recent `limit` events from the current room.
    ///
    /// The current room is the DEFAULT scope for this read, not the only
    /// one it can express: this is `page_recent_in` against
    /// `current_room()`. Callers that already know which room they want
    /// (`airc inbox --room`, a consumer paging a room it is subscribed to
    /// but not sitting in) call `page_recent_in` directly and never have
    /// to move the scope's default-room pointer to perform a READ.
    pub async fn page_recent(&self, limit: usize) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        self.page_recent_in(&room, limit).await
    }

    /// Fetch the most recent `limit` events from `room`.
    ///
    /// The room-explicit half of `page_recent` — same daemon-attached
    /// routing, no implicit scope. Resolve `room` with
    /// `room_by_name_or_channel`, which refuses loudly for a room this
    /// scope is not subscribed to (reading must not auto-join).
    pub async fn page_recent_in(
        &self,
        room: &crate::Room,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        if self.is_daemon_attached() {
            return self.daemon_page_recent(room, limit).await;
        }
        Ok(self
            .inner
            .store
            .page_recent(Some(room.channel), limit)
            .await?)
    }

    /// Fetch recent events matching `filter`. If the filter does not
    /// specify a channel, it is scoped to the current room.
    ///
    /// Continuum #297: the kind filter is applied BEFORE the limit, so
    /// the page is the newest `limit` events OF THOSE KINDS — never the
    /// survivors of a raw newest-`limit` (a persona-turn StreamChunk
    /// flood fills a raw page and evicts every durable Message,
    /// deafening working personas to direction). Daemon-attached, the
    /// kinds are pushed down into the daemon's inbox read; the local
    /// store path uses the same typed matching predicate before LIMIT.
    pub async fn page_recent_filtered(
        &self,
        filter: EventFilter,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let filter = self.scope_filter_to_current_room(filter).await?;
        Ok(self.filtered_history(None, filter, limit).await?.events)
    }

    /// Fetch recent events from the subscribed room set.
    pub async fn page_recent_subscribed_filtered(
        &self,
        filter: EventFilter,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let filter = self.subscribed_event_filter(filter).await?;
        Ok(self.filtered_history(None, filter, limit).await?.events)
    }

    /// Fetch up to `limit` events strictly after `cursor` in the current room.
    ///
    /// `resume_from_in` against `current_room()` — see `page_recent` for why
    /// the current room is a default here rather than the only expressible
    /// scope.
    pub async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        self.resume_from_in(&room, cursor, limit).await
    }

    /// Fetch up to `limit` events strictly after `cursor` in `room`.
    pub async fn resume_from_in(
        &self,
        room: &crate::Room,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        if self.is_daemon_attached() {
            return self.daemon_resume_from(room, cursor, limit).await;
        }
        Ok(self
            .inner
            .store
            .resume_from(cursor, Some(room.channel), limit)
            .await?)
    }

    /// Fetch events strictly after `cursor` that match `filter`. If
    /// the filter does not specify a channel, it is scoped to the
    /// current room.
    pub async fn resume_from_filtered(
        &self,
        cursor: &TranscriptCursor,
        filter: EventFilter,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let filter = self.scope_filter_to_current_room(filter).await?;
        Ok(self
            .filtered_history(Some(cursor), filter, limit)
            .await?
            .events)
    }

    /// Fetch events strictly after `cursor` from the subscribed room set.
    pub async fn resume_from_subscribed_filtered(
        &self,
        cursor: &TranscriptCursor,
        filter: EventFilter,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        Ok(self
            .scan_subscribed_events(cursor, filter, limit)
            .await?
            .events)
    }

    /// Return one page of at most `limit` records, retaining subscribed matches.
    /// Unlike the Vec-only resume surface, this preserves raw scan progress
    /// when an entire local-store page belongs to other rooms or is filtered
    /// out. Only `scanned_through: None` means no records were read.
    pub async fn scan_subscribed_events(
        &self,
        cursor: &TranscriptCursor,
        filter: EventFilter,
        limit: usize,
    ) -> Result<EventScan, AircError> {
        let filter = self.subscribed_event_filter(filter).await?;
        self.filtered_history(Some(cursor), filter, limit).await
    }

    /// One store predicate governs paging before LIMIT in both embedded and IPC modes.
    async fn filtered_history(
        &self,
        after: Option<&TranscriptCursor>,
        filter: EventFilter,
        limit: usize,
    ) -> Result<EventScan, AircError> {
        let channels: Vec<_> = match filter.channel {
            Some(channel) if filter.channels.is_empty() || filter.channels.contains(&channel) => {
                vec![channel]
            }
            Some(_) => Vec::new(),
            None => filter.channels.iter().copied().collect(),
        };
        let mut through = None;
        for channel in &channels {
            if let Some(cursor) = self.channel_latest_cursor(*channel).await? {
                if through.as_ref().is_none_or(|old: &TranscriptCursor| {
                    (cursor.lamport, cursor.event_id.as_uuid())
                        > (old.lamport, old.event_id.as_uuid())
                }) {
                    through = Some(cursor);
                }
            }
        }
        let Some(through) = through else {
            return Ok(EventScan {
                events: Vec::new(),
                scanned_through: None,
            });
        };
        let rule = airc_core::HistoryMatch {
            selection: filter.selection.clone(),
            headers: &filter.headers_filter,
            addressed_to: None,
            kinds: &filter.kinds,
            self_echo: filter
                .self_echo
                .map(|value| (value.peer_id, value.client_id, value.filter)),
        };
        self.scan_matching_channels(after, &through, limit, &channels, rule)
            .await
    }

    /// Cursor of the newest event in the current room — via the daemon
    /// when attached (its ORM is the transcript; the local store can be
    /// empty/stale on an attached scope), the local store otherwise
    /// (card 8428ae8c).
    pub async fn latest_cursor(&self) -> Result<Option<TranscriptCursor>, AircError> {
        let room = self.current_room().await?;
        self.channel_latest_cursor(room.channel).await
    }

    /// Append a `TranscriptEvent` to the durable store directly.
    pub async fn append_event(&self, event: TranscriptEvent) -> Result<(), AircError> {
        Ok(self.inner.store.append(event).await?)
    }

    async fn scope_filter_to_current_room(
        &self,
        mut filter: EventFilter,
    ) -> Result<EventFilter, AircError> {
        if filter.channel.is_none() {
            filter.channel = Some(self.current_room().await?.channel);
        }
        Ok(filter)
    }
}

fn route_class_for_frame(kind: FrameKind) -> RouteClass {
    match kind {
        FrameKind::Message
        | FrameKind::Command
        | FrameKind::CommandResult
        | FrameKind::Addressed => RouteClass::DataInteractive,
        FrameKind::Event | FrameKind::Control => RouteClass::ControlInteractive,
    }
}

fn format_route_refusal(decision: RouteDecision) -> AircError {
    match decision {
        RouteDecision::NoRoute { class } => {
            AircError::Route(format!("{class:?} has no admissible live route"))
        }
        RouteDecision::Selected(kind) => AircError::Route(format!(
            "unexpected selected route returned as refusal: {kind:?}"
        )),
    }
}
