//! Exact-header buckets inside the existing channel router. Payloads are never
//! inspected; unmatched buckets incur neither predicate evaluation nor delivery.
use std::collections::HashMap;

use airc_core::{HeaderFilter, Headers};

#[derive(Default)]
pub(crate) struct SubscriberIndex<T> {
    general: HashMap<u64, T>,
    exact: HashMap<String, HashMap<String, HashMap<u64, T>>>,
    len: usize,
}

pub(crate) fn exact_key(filter: &HeaderFilter) -> Option<(&str, &str)> {
    match filter {
        HeaderFilter::Exact { key, value } => Some((key, value)),
        HeaderFilter::All(filters) => filters.iter().find_map(exact_key),
        HeaderFilter::Any
        | HeaderFilter::Prefix { .. }
        | HeaderFilter::AnyOf(_)
        | HeaderFilter::Has { .. }
        | HeaderFilter::Not(_) => None,
    }
}

impl<T> SubscriberIndex<T> {
    pub(crate) fn new() -> Self {
        Self {
            general: HashMap::new(),
            exact: HashMap::new(),
            len: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn insert(&mut self, id: u64, headers: &HeaderFilter, value: T) {
        match exact_key(headers) {
            Some((key, expected)) => {
                self.exact
                    .entry(key.to_owned())
                    .or_default()
                    .entry(expected.to_owned())
                    .or_default()
                    .insert(id, value);
            }
            None => {
                self.general.insert(id, value);
            }
        }
        self.len += 1;
    }

    pub(crate) fn remove(&mut self, id: u64, headers: &HeaderFilter) {
        let removed = match exact_key(headers) {
            None => self.general.remove(&id).is_some(),
            Some((key, expected)) => {
                let Some(values) = self.exact.get_mut(key) else {
                    return;
                };
                let removed = if let Some(bucket) = values.get_mut(expected) {
                    let removed = bucket.remove(&id).is_some();
                    if bucket.is_empty() {
                        values.remove(expected);
                    }
                    removed
                } else {
                    false
                };
                if values.is_empty() {
                    self.exact.remove(key);
                }
                removed
            }
        };
        if removed {
            self.len -= 1;
        }
    }

    pub(crate) fn visit(&self, headers: &Headers, mut visit: impl FnMut(&T)) {
        for entry in self.general.values() {
            visit(entry);
        }
        for (key, value) in headers {
            if let Some(bucket) = self
                .exact
                .get(key.as_str())
                .and_then(|values| values.get(value.as_str()))
            {
                for entry in bucket.values() {
                    visit(entry);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: indexing a non-mandatory exact term in AnyOf/Not
    // would lose legitimate subscribers; compound predicates must stay sound.
    #[test]
    fn compound_filter_candidates_preserve_full_predicate_semantics() {
        let exact = HeaderFilter::Exact {
            key: "id".into(),
            value: "one".into(),
        };
        let filters = [
            HeaderFilter::All(vec![
                HeaderFilter::All(vec![exact.clone()]),
                HeaderFilter::Has {
                    key: "reply".into(),
                },
            ]),
            HeaderFilter::AnyOf(vec![
                exact.clone(),
                HeaderFilter::Has {
                    key: "reply".into(),
                },
            ]),
            HeaderFilter::Not(Box::new(exact)),
        ];
        let mut index = SubscriberIndex::new();
        for (id, filter) in filters.iter().enumerate() {
            index.insert(id as u64, filter, id);
        }
        for headers in [
            Headers::new(),
            Headers::from([("id".into(), "one".into())]),
            Headers::from([("reply".into(), "yes".into())]),
            Headers::from([("id".into(), "one".into()), ("reply".into(), "yes".into())]),
        ] {
            let mut actual = Vec::new();
            index.visit(&headers, |id| {
                if filters[*id].matches(&headers) {
                    actual.push(*id)
                }
            });
            actual.sort_unstable();
            let expected: Vec<_> = filters
                .iter()
                .enumerate()
                .filter_map(|(id, filter)| filter.matches(&headers).then_some(id))
                .collect();
            assert_eq!(actual, expected);
        }
    }

    // what this catches: exact command handles must not scan every pending
    // subscriber or deliver unrelated bulk content before client-side decode.
    #[test]
    fn exact_handles_select_only_matching_bucket_and_remove_by_generation() {
        let mut index = SubscriberIndex::new();
        let filters: Vec<_> = (0..128)
            .map(|n| HeaderFilter::Exact {
                key: "airc.correlation_id".into(),
                value: n.to_string(),
            })
            .collect();
        for (id, filter) in filters.iter().enumerate() {
            index.insert(id as u64, filter, id);
        }
        let mut visited = 0;
        for n in 128..256 {
            index.visit(
                &Headers::from([("airc.correlation_id".into(), n.to_string())]),
                |_| visited += 1,
            );
        }
        assert_eq!(visited, 0, "unrelated correlations visit no pending handle");
        for n in 0..128 {
            index.visit(
                &Headers::from([("airc.correlation_id".into(), n.to_string())]),
                |id| {
                    assert_eq!(*id, n);
                    visited += 1;
                },
            );
        }
        assert_eq!(visited, 128);
        index.insert(129, &filters[0], 129);
        index.remove(0, &filters[0]);
        index.remove(0, &filters[0]); // stale guard must not remove its replacement
        let mut matched = Vec::new();
        index.visit(
            &Headers::from([("airc.correlation_id".into(), "0".into())]),
            |id| matched.push(*id),
        );
        assert_eq!(matched, [129]);
        index.remove(129, &filters[0]);
        for (id, filter) in filters.iter().enumerate().skip(1) {
            index.remove(id as u64, filter);
        }
        assert_eq!(index.len(), 0);
        assert!(index.exact.is_empty(), "empty keys must not accumulate");
    }
}
