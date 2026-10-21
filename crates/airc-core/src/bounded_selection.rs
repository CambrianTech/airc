//! Bounded ordered selection for unordered memory stores.
use std::collections::BTreeMap;

/// Inspect all candidates, retain at most `limit` borrowed records, and clone
/// only the selected result. The ordinal preserves equal-key records.
pub fn select<'a, T: Clone + 'a, K: Ord>(
    rows: impl IntoIterator<Item = &'a T>,
    key: impl Fn(&T) -> K,
    limit: usize,
    tail: bool,
) -> Vec<T> {
    if limit == 0 {
        return Vec::new();
    }
    let mut selected = BTreeMap::new();
    for (ordinal, row) in rows.into_iter().enumerate() {
        let key = (key(row), ordinal);
        if selected.len() == limit {
            let boundary = if tail {
                selected.first_key_value()
            } else {
                selected.last_key_value()
            }
            .unwrap()
            .0;
            if if tail {
                &key <= boundary
            } else {
                &key >= boundary
            } {
                continue;
            }
            if tail {
                selected.pop_first();
            } else {
                selected.pop_last();
            }
        }
        selected.insert(key, row);
    }
    selected.into_values().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Row {
        value: usize,
        clones: Rc<Cell<usize>>,
    }
    impl Clone for Row {
        fn clone(&self) -> Self {
            self.clones.set(self.clones.get() + 1);
            Self {
                value: self.value,
                clones: self.clones.clone(),
            }
        }
    }
    #[test]
    fn large_unordered_history_clones_only_selected_rows_in_both_directions() {
        let clones = Rc::new(Cell::new(0));
        let rows: Vec<_> = (0..200_000)
            .rev()
            .map(|value| Row {
                value,
                clones: clones.clone(),
            })
            .collect();
        for tail in [false, true] {
            clones.set(0);
            let selected = select(&rows, |r| r.value, 8, tail);
            assert_eq!(clones.get(), 8);
            let expected = if tail { 199_992..200_000 } else { 0..8 };
            assert_eq!(
                selected.iter().map(|r| r.value).collect::<Vec<_>>(),
                expected.collect::<Vec<_>>()
            );
        }
        clones.set(0);
        assert!(select(&rows, |r| r.value, 0, false).is_empty());
        assert_eq!(clones.get(), 0);
    }
}
