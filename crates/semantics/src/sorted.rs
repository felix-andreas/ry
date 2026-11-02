//! Read-only sorted maps and sets for memoized results.
//!
//! A memoized analysis result is built once and then only read, so it does not need a
//! B-tree's spare node capacity or its per-node allocations. These types hold their entries in
//! one exactly sized, sorted array: a lookup is a binary search, iteration is in key order (the
//! same order a `BTreeMap` gives), and an empty one allocates nothing.
//!
//! The array is a `ThinVec`, which keeps its length beside the elements, so a map is one
//! pointer wide. A per-item record holds a couple of dozen of these and most are empty, so the
//! width of an empty one is most of the record's size.

use {
    std::{
        collections::{BTreeMap, BTreeSet},
        iter, slice,
    },
    thin_vec::ThinVec,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct SortedMap<K, V>(ThinVec<(K, V)>);

impl<K, V> Default for SortedMap<K, V> {
    fn default() -> SortedMap<K, V> {
        SortedMap(ThinVec::new())
    }
}

type EntryRef<'a, K, V> = fn(&'a (K, V)) -> (&'a K, &'a V);

impl<K: Ord, V> SortedMap<K, V> {
    pub fn get(&self, key: &K) -> Option<&V> {
        let index = self.0.binary_search_by(|(probe, _)| probe.cmp(key)).ok()?;
        self.0.get(index).map(|(_, value)| value)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.0.binary_search_by(|(probe, _)| probe.cmp(key)).is_ok()
    }

    pub fn iter(&self) -> iter::Map<slice::Iter<'_, (K, V)>, EntryRef<'_, K, V>> {
        self.0.iter().map(|(key, value)| (key, value))
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &K> + ExactSizeIterator {
        self.0.iter().map(|(key, _)| key)
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &V> + ExactSizeIterator {
        self.0.iter().map(|(_, value)| value)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K: Ord, V> From<BTreeMap<K, V>> for SortedMap<K, V> {
    fn from(map: BTreeMap<K, V>) -> SortedMap<K, V> {
        SortedMap(map.into_iter().collect())
    }
}

/// Later entries win over earlier ones with the same key, as with `BTreeMap::insert`.
impl<K: Ord, V> FromIterator<(K, V)> for SortedMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(entries: I) -> SortedMap<K, V> {
        let mut entries: Vec<(K, V)> = entries.into_iter().collect();
        // A stable sort keeps equal keys in arrival order, so after reversing, the first of
        // each run is the latest entry.
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        entries.reverse();
        entries.dedup_by(|(later, _), (kept, _)| later == kept);
        entries.reverse();
        SortedMap(ThinVec::from(entries))
    }
}

impl<'a, K: Ord, V> IntoIterator for &'a SortedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = iter::Map<slice::Iter<'a, (K, V)>, EntryRef<'a, K, V>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct SortedSet<K>(ThinVec<K>);

impl<K> Default for SortedSet<K> {
    fn default() -> SortedSet<K> {
        SortedSet(ThinVec::new())
    }
}

impl<K: Ord> SortedSet<K> {
    pub fn contains(&self, key: &K) -> bool {
        self.0.binary_search(key).is_ok()
    }

    pub fn iter(&self) -> slice::Iter<'_, K> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K: Ord> From<BTreeSet<K>> for SortedSet<K> {
    fn from(set: BTreeSet<K>) -> SortedSet<K> {
        SortedSet(set.into_iter().collect())
    }
}

impl<K: Ord> FromIterator<K> for SortedSet<K> {
    fn from_iter<I: IntoIterator<Item = K>>(keys: I) -> SortedSet<K> {
        let mut keys: Vec<K> = keys.into_iter().collect();
        keys.sort_unstable();
        keys.dedup();
        SortedSet(ThinVec::from(keys))
    }
}

impl<'a, K: Ord> IntoIterator for &'a SortedSet<K> {
    type Item = &'a K;
    type IntoIter = slice::Iter<'a, K>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
