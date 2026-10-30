//! Shared settled agent feed: typed stream exclusion, bounded snapshot paging.
use crate::{Airc, AircError, EventFilter, EventScan, FilteredEventStream};
use airc_core::{EventId, HeaderFilter, TranscriptCursor};
use airc_ipc::{IpcCursor, COUNTER_BITS, COUNTER_MASK};

/// Native agent subscriptions: speech and actionable work-state changes.
/// Presence, work receipts and stream traffic do not enter this feed.
pub fn agent_event_selection() -> airc_core::EventSelection {
    airc_core::EventSelection::AnyOf(vec![
        airc_core::EventSelection::Correspondence,
        airc_core::EventSelection::Exact(airc_core::EventClass::Activity(
            airc_core::ActivityKind::WorkState,
        )),
    ])
}

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

impl Airc {
    /// Same policy in embedded and daemon modes; router attach filters before fanout.
    pub async fn subscribe_agent_feed(&self) -> Result<FilteredEventStream, AircError> {
        self.subscribe_subscribed_delivery(
            EventFilter {
                selection: agent_event_selection(),
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
        let kinds = std::collections::BTreeSet::new();
        let rule = airc_core::HistoryMatch {
            sender: None,
            selection: agent_event_selection(),
            headers: &HeaderFilter::Any,
            addressed_to,
            kinds: &kinds,
            self_echo: None,
        };
        self.scan_matching_channels(after, through, limit, channels, rule)
            .await
    }

    pub(crate) async fn scan_matching_channels(
        &self,
        after: Option<&TranscriptCursor>,
        through: &TranscriptCursor,
        limit: usize,
        channels: &[airc_core::RoomId],
        rule: airc_core::HistoryMatch<'_>,
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
        let mut events = Vec::new();
        let mut safe = through.clone();
        if let Some(client) = self.daemon_client() {
            for channel in channels.iter().copied() {
                let response = client
                    .scan_inbox(airc_ipc::request::ScanInboxRequest {
                        sender: rule.sender,
                        channel,
                        after: ipc(after),
                        through: ipc(through),
                        headers: rule.headers.clone(),
                        selection: rule.selection.clone(),
                        kinds: rule.kinds.clone(),
                        self_echo: rule.self_echo,
                        addressed_to: rule.addressed_to,
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
        filter: crate::EventFilter,
        exclude: Option<&airc_core::RuntimeSelf>,
    ) -> Result<airc_core::HistoryCount, AircError> {
        let predicate = filter.headers_filter;
        let kinds = filter.kinds;
        let rule = airc_core::HistoryMatch {
            sender: filter.sender,
            selection: filter.selection.clone(),
            headers: &predicate,
            addressed_to: None,
            kinds: &kinds,
            self_echo: filter.self_echo.map(|v| (v.peer_id, v.client_id, v.filter)),
        };
        let mut total = airc_core::HistoryCount::default();
        if key(after) >= key(through) {
            return Ok(total);
        }
        if let Some(client) = self.daemon_client() {
            for channel in channels.iter().copied() {
                let count = client
                    .count_inbox(airc_ipc::request::CountInboxRequest {
                        sender: rule.sender,
                        channel,
                        after: ipc(after),
                        through: ipc(through),
                        headers: predicate.clone(),
                        selection: rule.selection.clone(),
                        kinds: kinds.clone(),
                        addressed_to: None,
                        self_echo: rule.self_echo,
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
            .count_history(after, through, channels, rule, exclude)
            .await?)
    }
}
