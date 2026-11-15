//! Columns and bitsets keyed by a dense id.
//!
//! An item numbers its expressions, patterns and bindings from zero, so a fact about one of
//! them is an array slot rather than a hash probe. A [`Column`] holds one optional value per id
//! and a [`Bitset`] one bit per id. Both live in an arena that the owning phase frees in one
//! step, and neither allocates before its first write, because most of a check's columns stay
//! empty for most items.

use {
    bumpalo::{Bump, collections::Vec as ArenaVec},
    std::{iter, marker::PhantomData},
};

/// An id that numbers the entries of one array from zero.
pub trait DenseId: Copy {
    fn from_index(index: usize) -> Self;
    fn index(self) -> usize;
}

impl DenseId for usize {
    fn from_index(index: usize) -> usize {
        index
    }

    fn index(self) -> usize {
        self
    }
}

/// One optional value per id.
pub struct Column<'a, K, V> {
    slots: ArenaVec<'a, Option<V>>,
    /// How many ids the owner has minted. The first write sizes the column for all of them, so
    /// later writes never grow it unless the owner mints more.
    key_count: usize,
    key: PhantomData<fn(K) -> K>,
}

impl<'a, K: DenseId, V> Column<'a, K, V> {
    pub fn new_in(arena: &'a Bump, key_count: usize) -> Column<'a, K, V> {
        Column {
            slots: ArenaVec::new_in(arena),
            key_count,
            key: PhantomData,
        }
    }

    pub fn get(&self, key: K) -> Option<&V> {
        self.slots.get(key.index())?.as_ref()
    }

    pub fn contains_key(&self, key: K) -> bool {
        self.get(key).is_some()
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let index = key.index();
        if index >= self.slots.len() {
            let missing = self.key_count.max(index + 1) - self.slots.len();
            self.slots.extend(iter::repeat_with(|| None).take(missing));
        }
        self.slots[index].replace(value)
    }

    pub fn remove(&mut self, key: K) -> Option<V> {
        self.slots.get_mut(key.index())?.take()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    pub fn clear(&mut self) {
        self.slots.clear();
    }

    /// The present entries in id order.
    pub fn iter(&self) -> impl Iterator<Item = (K, &V)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| Some((K::from_index(index), slot.as_ref()?)))
    }

    /// Takes the present entries out in id order, leaving the column empty.
    pub fn drain(&mut self) -> impl Iterator<Item = (K, V)> {
        self.slots
            .drain(..)
            .enumerate()
            .filter_map(|(index, slot)| Some((K::from_index(index), slot?)))
    }
}

/// One bit per id.
pub struct Bitset<'a, K> {
    words: ArenaVec<'a, u64>,
    key_count: usize,
    key: PhantomData<fn(K) -> K>,
}

impl<'a, K: DenseId> Bitset<'a, K> {
    pub fn new_in(arena: &'a Bump, key_count: usize) -> Bitset<'a, K> {
        Bitset {
            words: ArenaVec::new_in(arena),
            key_count,
            key: PhantomData,
        }
    }

    pub fn contains(&self, key: K) -> bool {
        let index = key.index();
        self.words
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }

    /// Sets the bit and returns whether it was clear, as `HashSet::insert` does.
    pub fn insert(&mut self, key: K) -> bool {
        let index = key.index();
        let word = index / 64;
        if word >= self.words.len() {
            self.words
                .resize(self.key_count.div_ceil(64).max(word + 1), 0);
        }
        let bit = 1 << (index % 64);
        let fresh = self.words[word] & bit == 0;
        self.words[word] |= bit;
        fresh
    }

    /// Clears the bit and returns whether it was set.
    pub fn remove(&mut self, key: K) -> bool {
        let index = key.index();
        let Some(word) = self.words.get_mut(index / 64) else {
            return false;
        };
        let bit = 1 << (index % 64);
        let present = *word & bit != 0;
        *word &= !bit;
        present
    }

    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|word| *word == 0)
    }

    /// The set ids in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = K> + '_ {
        self.words.iter().enumerate().flat_map(|(position, &word)| {
            let mut rest = word;
            iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                let bit = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                Some(K::from_index(position * 64 + bit))
            })
        })
    }
}
