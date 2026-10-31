//! The weak intern table behind `Rf_mkCharLenCE`. Every `CHARSXP` with the same bytes and
//! encoding is one object, so native code may compare strings by pointer. The table holds its
//! strings weakly: a collection removes the entries whose strings died before it frees them.
//!
//! A `CHARSXP` caches its hash in the vector capacity field, which strings do not otherwise use.

use crate::object::{Sexp, gp};
use hashbrown::HashTable;
use std::hash::Hasher;

/// R's `cetype_t`, numbered as in GNU R.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Native = 0,
    Utf8 = 1,
    Latin1 = 2,
    Bytes = 3,
    Symbol = 5,
    Any = 99,
}

impl Encoding {
    pub fn from_code(code: u32) -> Option<Encoding> {
        Some(match code {
            0 => Encoding::Native,
            1 => Encoding::Utf8,
            2 => Encoding::Latin1,
            3 => Encoding::Bytes,
            5 => Encoding::Symbol,
            99 => Encoding::Any,
            _ => return None,
        })
    }

    pub(crate) fn gp_bits(self) -> u16 {
        match self {
            Encoding::Utf8 => gp::UTF8,
            Encoding::Latin1 => gp::LATIN1,
            Encoding::Bytes => gp::BYTES,
            Encoding::Native | Encoding::Symbol | Encoding::Any => 0,
        }
    }

    /// The encoding a `CHARSXP` is marked with, as `Rf_getCharCE` reports it.
    pub unsafe fn of(charsxp: Sexp) -> Encoding {
        let bits = unsafe { charsxp.gp() };
        if bits & gp::UTF8 != 0 {
            Encoding::Utf8
        } else if bits & gp::LATIN1 != 0 {
            Encoding::Latin1
        } else if bits & gp::BYTES != 0 {
            Encoding::Bytes
        } else {
            Encoding::Native
        }
    }
}

#[derive(Default)]
pub(crate) struct StringTable {
    table: HashTable<Sexp>,
    /// Strings interned since the last collection, the only ones a minor collection can free.
    young: Vec<Sexp>,
}

pub(crate) fn hash(bytes: &[u8], encoding_bits: u16) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    hasher.write(bytes);
    hasher.write_u16(encoding_bits);
    // The capacity field is signed, so keep the hash non-negative.
    hasher.finish() >> 1
}

impl StringTable {
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub unsafe fn find(&self, hash: u64, bytes: &[u8], encoding_bits: u16) -> Option<Sexp> {
        self.table
            .find(hash, |&candidate| unsafe {
                candidate.bytes() == bytes && candidate.gp() & gp::ENCODING == encoding_bits
            })
            .copied()
    }

    pub unsafe fn insert(&mut self, hash: u64, string: Sexp) {
        self.table.insert_unique(hash, string, |candidate| unsafe {
            candidate.capacity() as u64
        });
        self.young.push(string);
    }

    /// Drops the entries for young strings the current minor collection did not mark.
    pub unsafe fn remove_dead_young(&mut self) {
        for string in std::mem::take(&mut self.young) {
            if unsafe { string.is_marked() } {
                continue;
            }
            let hash = unsafe { string.capacity() } as u64;
            if let Ok(entry) = self
                .table
                .find_entry(hash, |&candidate| candidate == string)
            {
                entry.remove();
            }
        }
    }

    pub unsafe fn remove_dead(&mut self) {
        self.table.retain(|string| unsafe { string.is_marked() });
        self.young.clear();
    }
}
