//! Shared settled agent feed: typed stream exclusion, bounded snapshot paging.
use crate::{Airc, AircError, EventFilter, EventScan, FilteredEventStream};
use airc_core::{EventId, HeaderFilter, TranscriptCursor};
use airc_ipc::{IpcCursor, COUNTER_BITS, COUNTER_MASK};

fn key(c: &TranscriptCursor) -> (u64, u128) {
    (c.lamport, c.event_id.0.as_u128())
}
fn ipc(c: &TranscriptCursor) -> IpcCursor {
    IpcCursor {
        epoch: c.lamport >> COUNTER_BITS,
        counter: c.lamport & COUNTER_MASK,
        event_id: c.event_id,
    }
}
fn cursor(c: IpcCursor) -> TranscriptCursor {
    TranscriptCursor {
        lamport: (c.epoch << COUNTER_BITS) | c.counter,
        event_id: c.event_id,
    }
}
/// The most events one agent-feed scan reads: the store's page bound for every scan here.
pub(crate) const AGENT_SCAN_LIMIT: usize = 4096;

fn headers() -> HeaderFilter {
    HeaderFilter::Not(Box::new(HeaderFilter::Has {
        key: crate::HEADER_STREAM_ID.into(),
    }))
}

impl Airc {
    /// Same policy in embedded and daemon modes; router attach filters before fanout.
    pub async fn subscribe_agent_feed(&self) -> Result<FilteredEventStream, AircError> {
        let mut predicate = headers();
        if self.daemon_client().is_none() {
            // Embedded frames carry their delivery class in the signed header.
            predicate = HeaderFilter::All(vec![
                predicate,
                HeaderFilter::AnyOf(vec![
                    HeaderFilter::Not(Box::new(HeaderFilter::Has {
                        key: "airc.delivery_class".into(),
                    })),
                    HeaderFilter::Exact {
                        key: "airc.delivery_class".into(),
                        value: "durable".into(),
                    },
                ]),
            ]);
        }
        self.subscribe_subscribed_delivery(
            EventFilter {
                headers_filter: predicate,
                ..EventFilter::default()
            },
            Some(vec![airc_ipc::IpcDelivery::Durable]),
        )
        .await
    }

    /// Forward scan to `through`, or initial latest matching page when `after` is None.
    /// Excluded rows remain in general history. Progress certifies only this predicate.
    pub async fn scan_agent_feed(
        &self,
        after: Option<&TranscriptCursor>,
        through: &TranscriptCursor,
        limit: usize,
    ) -> Result<EventScan, AircError> {
        let channels: Vec<_> = self
            .subscription_set()
            .await?
            .all()
            .map(|s| s.as_room().channel)
            .collect();
        self.scan_agent_feed_in_channels(after, through, limit, &channels, None)
            .await
    }
    /// [`Self::scan_agent_feed`] over `channels`; with `addressed_to`, only the
    /// events that could be addressed to that peer, selected by the store before
    /// `limit` (so ambient traffic in the range costs nothing).
    pub(crate) async fn scan_agent_feed_in_channels(
        &self,
        after: Option<&TranscriptCursor>,
        through: &TranscriptCursor,
        limit: usize,
        channels: &[airc_core::RoomId],
        addressed_to: Option<airc_core::AddresseeFilter>,
    ) -> Result<EventScan, AircError> {
        if limit == 0 || limit > AGENT_SCAN_LIMIT {
            return Err(AircError::Route("agent scan limit must be 1..4096".into()));
        }
        let zero = TranscriptCursor {
            lamport: 0,
            event_id: EventId::from_u128(0),
        };
        let tail = after.is_none();
        let after = after.unwrap_or(&zero);
        if key(after) > key(through) {
            return Ok(EventScan {
                events: vec![],
                scanned_through: Some(after.clone()),
                settlement: airc_core::Settlement::Settled,
            });
        }
        let predicate = headers();
        let rule = airc_core::HistoryMatch {
            headers: &predicate,
            addressed_to,
        };
        let mut events = Vec::new();
        let mut settlement = airc_core::Settlement::Settled;
        let mut safe = through.clone();
        if let Some(client) = self.daemon_client() {
            for channel in channels.iter().copied() {
                let response = client
                    .scan_inbox(airc_ipc::request::ScanInboxRequest {
                        channel,
                        after: ipc(after),
                        through: ipc(through),
                        headers: predicate.clone(),
                        addressed_to,
                        limit,
                        tail,
                    })
                    .await?;
                let scanned = cursor(response.scanned_through);
                if key(&scanned) < key(after) || key(&scanned) > key(through) {
                    return Err(AircError::Route("invalid filtered scan watermark".into()));
                }
                if key(&scanned) < key(&safe) {
                    safe = scanned;
                }
                settlement = settlement.merge(response.settlement);
                for encoded in response.envelopes {
                    let event = crate::daemon::decode_wire_event(encoded)?;
                    if event.room_id != channel
                        || !rule.matches_event(&event)
                        || key(&event.cursor()) <= key(after)
                        || key(&event.cursor()) > key(through)
                    {
                        return Err(AircError::Route(
                            "daemon violated filtered scan contract".into(),
                        ));
                    }
                    events.push(event);
                }
            }
        } else {
            let page = self
                .inner
                .store
                .matching_history(after, through, channels, rule, limit + 1, tail)
                .await?;
            events = page.rows;
            settlement = page.settlement;
        }
        events.sort_by_key(|e| key(&e.cursor()));
        events.dedup_by_key(|e| e.event_id);
        if tail {
            if events.len() > limit {
                events.drain(..events.len() - limit);
            }
            // Each room returned its newest matches; byte-cut tails are refused by daemon.
            safe = through.clone();
        } else {
            // A short byte-bounded room page may certify less than another room returned.
            events.retain(|e| key(&e.cursor()) <= key(&safe));
            if events.len() > limit {
                events.truncate(limit);
                safe = events
                    .last()
                    .ok_or_else(|| AircError::Route("empty truncated agent page".into()))?
                    .cursor();
            }
        }
        Ok(EventScan {
            events,
            scanned_through: Some(safe),
            settlement,
        })
    }

    /// Count the agent-feed events in `(after, through]` of `channels` that also
    /// match `extra`, with their oldest and newest times. The store answers
    /// (one `CountInbox` per channel over the daemon, one query embedded); no
    /// event crosses the socket or is decoded.
    pub(crate) async fn count_agent_feed_in_channels(
        &self,
        after: &TranscriptCursor,
        through: &TranscriptCursor,
        channels: &[airc_core::RoomId],
        extra: HeaderFilter,
        exclude: Option<&airc_core::RuntimeSelf>,
    ) -> Result<airc_core::HistoryCount, AircError> {
        let predicate = HeaderFilter::All(vec![headers(), extra]);
        let mut total = airc_core::HistoryCount::default();
        if key(after) >= key(through) {
            return Ok(total);
        }
        if let Some(client) = self.daemon_client() {
            for channel in channels.iter().copied() {
                let count = client
                    .count_inbox(airc_ipc::request::CountInboxRequest {
                        channel,
                        after: ipc(after),
                        through: ipc(through),
                        headers: predicate.clone(),
                        exclude_self: exclude.cloned(),
                    })
                    .await?;
                total = total.merge(count);
            }
            return Ok(total);
        }
        Ok(self
            .inner
            .store
            .count_history(after, through, channels, &predicate, exclude)
            .await?)
    }
}

/// Why waiting for the next event addressed to a reader ended. Every ending is a reason for the
/// reader to read its delivery now; none is a timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressedWake {
    /// An event addressed to the reader arrived from another runtime.
    Addressed {
        from: airc_core::PeerId,
        event: EventId,
    },
    /// The live feed lagged: something may have been missed, so the delivery reads it.
    Lagged,
    /// The feed closed (the daemon went away).
    FeedClosed,
}

/// A reader's live feed, narrowed to what is addressed to it. Subscribed when it is made, so an
/// event published after [`Airc::addressed_feed`] returns is never missed.
pub struct AddressedFeed {
    feed: FilteredEventStream,
    me: crate::Addressee,
    own: airc_core::RuntimeSelf,
}

impl AddressedFeed {
    /// The next event addressed to the reader that is not its own runtime's send. No deadline: it
    /// ends on an event, a lag or the feed closing.
    pub async fn next(&mut self) -> AddressedWake {
        use futures::StreamExt;
        while let Some(next) = self.feed.next().await {
            match next {
                Ok(event) if !self.own.is_self_event(&event) && self.me.is_addressed(&event) => {
                    return AddressedWake::Addressed {
                        from: event.peer_id,
                        event: event.event_id,
                    }
                }
                Ok(_) => {}
                Err(_lag) => return AddressedWake::Lagged,
            }
        }
        AddressedWake::FeedClosed
    }
}

impl Airc {
    /// This reader's feed of what is addressed to it ([`crate::Addressee::is_addressed`]), with
    /// `runtime_client`'s own sends left out.
    pub async fn addressed_feed(
        &self,
        runtime_client: Option<&str>,
    ) -> Result<AddressedFeed, AircError> {
        Ok(AddressedFeed {
            feed: self.subscribe_agent_feed().await?,
            me: self.addressee().await?,
            own: airc_core::RuntimeSelf::new(self.client_id(), runtime_client),
        })
    }
}

impl Airc {
    /// The first event addressed to this reader that `consumer` has not yet been delivered, if any:
    /// the consumer's whole undelivered range, read without delivering anything. A wake checks it
    /// after its feed is subscribed and its lock is held, so a message that arrived between a stop
    /// hook's look and the wake's subscription still wakes the session, however much other traffic
    /// came with it.
    pub async fn addressed_undelivered(
        &self,
        consumer: &str,
        runtime_client: Option<&str>,
    ) -> Result<Option<AddressedWake>, AircError> {
        let turn = self.prepare_agent_delivery_waiting(consumer, 1, 1).await?;
        let me = self.addressee().await?;
        let own = airc_core::RuntimeSelf::new(self.client_id(), runtime_client);
        let found = turn.first_addressed(&me, &own).await?;
        // Nothing was shown: ownership is released and the cursor stays where it was.
        turn.acknowledge(0, false).await?;
        Ok(found)
    }
}
