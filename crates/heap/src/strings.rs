//! The weak intern table behind `Rf_mkCharLenCE`. Every `CHARSXP` with the same bytes and
//! encoding is one object, so native code may compare strings by pointer. The table holds its
//! strings weakly: a collection removes the entries whose strings died before it frees them.
//!
//! A `CHARSXP` caches its hash in the high half of the vector capacity field, which strings do not
//! otherwise use, and keeps its `TRUELENGTH` in the low half ([`Sexp::char_truelength`]).

use {
    crate::{
        object::{Header, Sexp, gp},
        space::Space,
    },
    std::ptr::NonNull,
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

/// The intern table, open-addressed with linear probing over one dense array of 8-byte slots.
///
/// A slot packs a string's address and a tag of its hash, so a probe reads one cache line and
/// dereferences only a string whose tag matches. A lookup that finds its string costs two
/// dependent memory reads (the slot and the string), and one that misses costs one, however
/// many strings are live. A chained or control-byte table would add a third read, which in a
/// table of millions of strings is a cache miss on every call to `mk_char`.
pub(crate) struct StringTable {
    /// A power-of-two count of slots: 0 for empty, [`TOMBSTONE`] for a removed entry, and
    /// otherwise a string's address over 8 in the low [`ADDRESS_BITS`] with the low
    /// [`TAG_BITS`] of its hash above.
    slots: Vec<u64>,
    /// Live entries, and removed ones still marked by a tombstone.
    len: usize,
    tombstones: usize,
    /// Strings interned since the last collection that are too long for the small-object
    /// space; the space's interned bits track the others.
    young_large: Vec<Sexp>,
}

/// Objects are 8-byte aligned (a short string takes a 40-byte slot) and user-space addresses fit
/// in 47 bits on every supported platform, so an address over 8 takes 44 bits and leaves 20 for
/// the tag.
const ADDRESS_BITS: u32 = 44;
const ADDRESS_MASK: u64 = (1 << ADDRESS_BITS) - 1;
const TAG_BITS: u32 = 64 - ADDRESS_BITS;
const EMPTY: u64 = 0;
/// No string lives at the top of the address space, so this slot names none.
const TOMBSTONE: u64 = ADDRESS_MASK;
const INITIAL_SLOTS: usize = 1 << 12;

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

impl Default for StringTable {
    fn default() -> StringTable {
        StringTable {
            slots: vec![EMPTY; INITIAL_SLOTS],
            len: 0,
            tombstones: 0,
            young_large: Vec::new(),
        }
    }
}

impl StringTable {
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub unsafe fn find(&self, hash: u32, bytes: &[u8], encoding_bits: u16) -> Option<Sexp> {
        let mask = self.slots.len() - 1;
        let tag = tag(hash);
        let mut index = home(hash, mask);
        loop {
            let slot = self.slots[index];
            if slot == EMPTY {
                return None;
            }
            if slot & !ADDRESS_MASK == tag && slot != TOMBSTONE {
                let candidate = unsafe { unpack(slot) };
                if unsafe { candidate.gp() } & gp::ENCODING == encoding_bits
                    && unsafe { same_bytes(candidate, bytes) }
                {
                    return Some(candidate);
                }
            }
            index = (index + 1) & mask;
        }
    }

    /// Adds `string`, which the table must not hold, and records it as young.
    pub unsafe fn insert(&mut self, hash: u32, string: Sexp, space: &mut Space) {
        if (self.len + self.tombstones + 1) * 8 > self.slots.len() * 7 {
            unsafe { self.rebuild() };
        }
        let mask = self.slots.len() - 1;
        let mut index = home(hash, mask);
        // The string is absent, so it may take the first tombstone on its probe path.
        while self.slots[index] != EMPTY && self.slots[index] != TOMBSTONE {
            index = (index + 1) & mask;
        }
        if self.slots[index] == TOMBSTONE {
            self.tombstones -= 1;
        }
        self.slots[index] = pack(string, hash);
        self.len += 1;
        if !space.set_interned(string) {
            self.young_large.push(string);
        }
    }

    /// The young strings outside the small-object space.
    pub fn young_large(&self) -> &[Sexp] {
        &self.young_large
    }

    /// Drops the entries for young strings the current minor collection did not mark. Runs
    /// before the sweep, while the space's interned bits still name the dead strings.
    pub unsafe fn remove_dead_young(&mut self, space: &Space) {
        for string in space.unmarked_young_interned() {
            unsafe { self.remove(string) };
        }
        for string in std::mem::take(&mut self.young_large) {
            if unsafe { !space.is_marked(string) } {
                unsafe { self.remove(string) };
            }
        }
    }

    pub unsafe fn remove_dead(&mut self, space: &Space) {
        // A pass in slot order reads the slots sequentially, and the marks it checks are
        // independent loads the processor overlaps.
        let mut removed = 0;
        for slot in &mut self.slots {
            if *slot != EMPTY && *slot != TOMBSTONE && unsafe { !space.is_marked(unpack(*slot)) } {
                *slot = TOMBSTONE;
                removed += 1;
            }
        }
        self.len -= removed;
        self.tombstones += removed;
        self.young_large.clear();
        if self.tombstones * 8 > self.slots.len() {
            unsafe { self.rebuild() };
        }
    }

    unsafe fn remove(&mut self, string: Sexp) {
        let mask = self.slots.len() - 1;
        let hash = unsafe { string.char_hash() };
        let wanted = pack(string, hash);
        let mut index = home(hash, mask);
        loop {
            let slot = self.slots[index];
            if slot == wanted {
                self.slots[index] = TOMBSTONE;
                self.len -= 1;
                self.tombstones += 1;
                return;
            }
            if slot == EMPTY {
                return;
            }
            index = (index + 1) & mask;
        }
    }

    /// Re-places every live entry into the smallest power-of-two table they fill to at most 5/8,
    /// which drops the tombstones. Growth at 7/8 then keeps the table between 5/16 and 7/8 full,
    /// close to GNU R's chained table, which grows at 0.85. Near the top of that range a probe
    /// for an absent string scans a few consecutive cache lines, but it compares only the tags in
    /// them; a sparser table would double the slots' memory, 32 MB at four million strings, and
    /// a growth step taken while the old slots are still live briefly costs both. Each entry's
    /// hash is read from its string, in slot order, so the reads are independent and overlap.
    unsafe fn rebuild(&mut self) {
        let wanted = ((self.len + 1) * 8 / 5)
            .next_power_of_two()
            .max(INITIAL_SLOTS);
        let old = std::mem::replace(&mut self.slots, vec![EMPTY; wanted]);
        let mask = wanted - 1;
        for slot in old {
            if slot == EMPTY || slot == TOMBSTONE {
                continue;
            }
            let hash = unsafe { unpack(slot).char_hash() };
            let mut index = home(hash, mask);
            while self.slots[index] != EMPTY {
                index = (index + 1) & mask;
            }
            self.slots[index] = slot;
        }
        self.tombstones = 0;
    }
}

/// The first slot to probe for a hash. The bits above the tag pick it, after a multiply spreads
/// all 32 hash bits into them.
#[inline]
fn home(hash: u32, mask: usize) -> usize {
    (u64::from(hash).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & mask
}

#[inline]
fn tag(hash: u32) -> u64 {
    (u64::from(hash) & ((1 << TAG_BITS) - 1)) << ADDRESS_BITS
}

#[inline]
fn pack(string: Sexp, hash: u32) -> u64 {
    let address = string.as_ptr() as u64;
    assert!(
        address & 7 == 0 && address >> (ADDRESS_BITS + 3) == 0,
        "a string's address {address:#x} does not fit the intern table's slot"
    );
    (address >> 3) | tag(hash)
}

/// # Safety
/// `slot` must hold a string, neither empty nor a tombstone.
#[inline]
unsafe fn unpack(slot: u64) -> Sexp {
    let pointer = ((slot & ADDRESS_MASK) << 3) as *mut Header;
    Sexp::from_non_null(unsafe { NonNull::new_unchecked(pointer) })
}
