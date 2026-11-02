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

/// What one pass over a string's bytes tells `mk_char`: whether they hold a NUL byte, whether
/// they are all ASCII, and their hash before the encoding is mixed in.
pub(crate) struct Scan {
    pub any_nul: bool,
    pub ascii: bool,
    state: u64,
}

const SEED_LOW: u64 = 0x243F_6A88_85A3_08D3;
const SEED_HIGH: u64 = 0x1319_8A2E_0370_7344;
const ONES: u64 = 0x0101_0101_0101_0101;
const HIGHS: u64 = 0x8080_8080_8080_8080;

/// Reads the bytes once, a word at a time, for the NUL test, the ASCII test and the hash
/// together. Re-interning a string that is already in the table costs little more than this pass
/// and one probe, so the three used to be separate passes and are now one.
#[inline]
pub(crate) fn scan(bytes: &[u8]) -> Scan {
    let length = bytes.len();
    let (mut low, mut high) = (SEED_LOW, SEED_HIGH);
    let (mut zero_bits, mut high_bits) = (0u64, 0u64);
    // The zero-byte test of a word, which is nonzero exactly when some byte of the word is zero.
    let zero_test = |word: u64| word.wrapping_sub(ONES) & !word & HIGHS;
    if length >= 8 {
        let mut rest = bytes;
        while let Some((chunk, tail)) = rest.split_first_chunk::<16>() {
            if tail.is_empty() {
                break;
            }
            let (first, second) = (load(&chunk[..8]), load(&chunk[8..]));
            zero_bits |= zero_test(first) | zero_test(second);
            high_bits |= first | second;
            let mixed = fold(low ^ first, PREVENT_ZERO_COLLAPSE ^ second);
            low = high;
            high = mixed;
            rest = tail;
        }
        // The last one or two words, read so that they end at the last byte: they may overlap
        // each other or bytes already mixed in, which the tests and the hash both tolerate.
        let last = load(&bytes[length - 8..]);
        let before = load(&bytes[length.saturating_sub(16).min(length - 8)..][..8]);
        zero_bits |= zero_test(last) | zero_test(before);
        high_bits |= last | before;
        low ^= before;
        high ^= last;
    } else if length >= 4 {
        let first = u64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        let tail = &bytes[length - 4..];
        let last = u64::from(u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]));
        // The upper halves are padding, so they are set to a nonzero, ASCII byte for the tests.
        let pad = 0x0101_0101_0000_0000;
        zero_bits |= zero_test(first | pad) | zero_test(last | pad);
        high_bits |= first | last;
        low ^= first;
        high ^= last;
    } else if length > 0 {
        let (first, middle, last) = (bytes[0], bytes[length / 2], bytes[length - 1]);
        zero_bits = u64::from(first == 0 || middle == 0 || last == 0);
        high_bits = u64::from(first | middle | last);
        low ^= u64::from(first);
        high ^= (u64::from(last) << 8) | u64::from(middle);
    }
    Scan {
        any_nul: zero_bits != 0,
        ascii: high_bits & HIGHS == 0,
        state: fold(low, high) ^ length as u64,
    }
}

/// The intern hash of a string, as the `CHARSXP` stores it: the scan's hash with the encoding
/// bits mixed in, since the same bytes in two encodings are two strings.
#[inline]
pub(crate) fn hash(scan: &Scan, encoding_bits: u16) -> u32 {
    (fold(scan.state, SEED_HIGH ^ u64::from(encoding_bits)) >> 32) as u32
}

const PREVENT_ZERO_COLLAPSE: u64 = 0xA409_3822_299F_31D0;

/// The folded multiply: the high and low halves of the full 128-bit product, xored.
#[inline]
fn fold(left: u64, right: u64) -> u64 {
    let product = u128::from(left) * u128::from(right);
    (product as u64) ^ ((product >> 64) as u64)
}

#[inline]
fn load(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(word)
}

/// Whether the interned string `candidate` holds exactly `bytes`. Most strings are short, and two
/// overlapping word loads compare them where a call to `memcmp` would cost more than the compare.
#[inline]
unsafe fn same_bytes(candidate: Sexp, bytes: &[u8]) -> bool {
    let length = bytes.len();
    if unsafe { candidate.length() } as usize != length {
        return false;
    }
    let stored = unsafe { std::slice::from_raw_parts(candidate.data() as *const u8, length) };
    if (8..=16).contains(&length) {
        load(stored) == load(bytes) && load(&stored[length - 8..]) == load(&bytes[length - 8..])
    } else {
        stored == bytes
    }
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

    #[inline]
    pub unsafe fn find(&self, hash: u32, bytes: &[u8], encoding_bits: u16) -> Option<Sexp> {
        self.table
            .find(spread(hash), |&candidate| unsafe {
                candidate.gp() & gp::ENCODING == encoding_bits && same_bytes(candidate, bytes)
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
