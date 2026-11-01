//! Read-only sorted maps and sets for memoized results.
//!
//! A memoized analysis result is built once and then only read, so it does not need a
//! B-tree's spare node capacity or its per-node allocations. These types hold their entries in
//! one exactly sized, sorted boxed slice: a lookup is a binary search, iteration is in key
//! order (the same order a `BTreeMap` gives), and an empty one allocates nothing.

use std::{
    collections::{BTreeMap, BTreeSet},
    iter, slice,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SortedMap<K, V>(Box<[(K, V)]>);

impl<K, V> Default for SortedMap<K, V> {
    fn default() -> SortedMap<K, V> {
        SortedMap(Box::default())
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
        SortedMap::from(entries.into_iter().collect::<BTreeMap<K, V>>())
    }
}

impl<'a, K: Ord, V> IntoIterator for &'a SortedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = iter::Map<slice::Iter<'a, (K, V)>, EntryRef<'a, K, V>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SortedSet<K>(Box<[K]>);

impl<K> Default for SortedSet<K> {
    fn default() -> SortedSet<K> {
        SortedSet(Box::default())
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
        SortedSet::from(keys.into_iter().collect::<BTreeSet<K>>())
    }
}

impl<'a, K: Ord> IntoIterator for &'a SortedSet<K> {
    type Item = &'a K;
    type IntoIter = slice::Iter<'a, K>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
