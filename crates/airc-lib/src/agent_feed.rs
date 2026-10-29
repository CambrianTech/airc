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
        if limit == 0 || limit > 4096 {
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
            });
        }
        let predicate = headers();
        let rule = airc_core::HistoryMatch {
            headers: &predicate,
            addressed_to,
        };
        let mut events = Vec::new();
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
            events = self
                .inner
                .store
                .matching_history(after, through, channels, rule, limit + 1, tail)
                .await?;
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
