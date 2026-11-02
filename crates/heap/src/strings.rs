//! The weak intern table behind `Rf_mkCharLenCE`. Every `CHARSXP` with the same bytes and
//! encoding is one object, so native code may compare strings by pointer. The table holds its
//! strings weakly: a collection removes the entries whose strings died before it frees them.
//!
//! A `CHARSXP` caches its hash in the high half of the vector capacity field, which strings do not
//! otherwise use, and keeps its `TRUELENGTH` in the low half ([`Sexp::char_truelength`]).

use {
    crate::{
        object::{Sexp, gp},
        space::Space,
    },
    hashbrown::HashTable,
    std::hash::Hasher,
};

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

/// The intern hash of a string, as the `CHARSXP` stores it.
pub(crate) fn hash(bytes: &[u8], encoding_bits: u16) -> u32 {
    let mut hasher = rustc_hash::FxHasher::default();
    hasher.write(bytes);
    hasher.write_u16(encoding_bits);
    (hasher.finish() >> 32) as u32
}

/// The table's hash for a stored 32-bit hash. The table takes its control bits from the top of
/// the hash, so the stored bits are spread over all 64.
fn spread(hash: u32) -> u64 {
    u64::from(hash).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

impl StringTable {
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub unsafe fn find(&self, hash: u32, bytes: &[u8], encoding_bits: u16) -> Option<Sexp> {
        self.table
            .find(spread(hash), |&candidate| unsafe {
                candidate.bytes() == bytes && candidate.gp() & gp::ENCODING == encoding_bits
            })
            .copied()
    }

    pub unsafe fn insert(&mut self, hash: u32, string: Sexp) {
        self.table
            .insert_unique(spread(hash), string, |candidate| unsafe {
                spread(candidate.char_hash())
            });
        self.young.push(string);
    }

    /// The strings interned since the last collection.
    pub fn young(&self) -> &[Sexp] {
        &self.young
    }

    /// Drops the entries for young strings the current minor collection did not mark.
    pub unsafe fn remove_dead_young(&mut self, space: &Space) {
        for string in std::mem::take(&mut self.young) {
            if unsafe { space.is_marked(string) } {
                continue;
            }
            let hash = spread(unsafe { string.char_hash() });
            if let Ok(entry) = self
                .table
                .find_entry(hash, |&candidate| candidate == string)
            {
                entry.remove();
            }
        }
    }

    pub unsafe fn remove_dead(&mut self, space: &Space) {
        self.table
            .retain(|&mut string| unsafe { space.is_marked(string) });
        self.young.clear();
    }
}
