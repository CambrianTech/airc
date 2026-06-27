use std::collections::{HashSet, VecDeque};

use airc_core::EventId;

/// Bounded O(1) duplicate guard for events already fanned out by this process.
pub(crate) struct BroadcastDeduper {
    capacity: usize,
    order: VecDeque<EventId>,
    seen: HashSet<EventId>,
}

impl BroadcastDeduper {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::with_capacity(capacity),
            seen: HashSet::with_capacity(capacity),
        }
    }

    /// Mark `event_id` as seen. Returns true only for the first mark.
    pub(crate) fn mark(&mut self, event_id: EventId) -> bool {
        if self.seen.contains(&event_id) {
            return false;
        }
        if self.capacity == 0 {
            return true;
        }
        if self.order.len() >= self.capacity {
            if let Some(expired) = self.order.pop_front() {
                self.seen.remove(&expired);
            }
        }
        self.order.push_back(event_id);
        self.seen.insert(event_id);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_marks_are_rejected() {
        let event = EventId::from_u128(1);
        let mut deduper = BroadcastDeduper::with_capacity(4);

        assert!(deduper.mark(event));
        assert!(!deduper.mark(event));
    }

    #[test]
    fn expired_marks_can_be_seen_again() {
        let first = EventId::from_u128(1);
        let mut deduper = BroadcastDeduper::with_capacity(2);

        assert!(deduper.mark(first));
        assert!(deduper.mark(EventId::from_u128(2)));
        assert!(deduper.mark(EventId::from_u128(3)));
        assert!(deduper.mark(first));
    }
}
