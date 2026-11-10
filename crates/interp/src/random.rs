// SPDX-License-Identifier: GPL-2.0-or-later
//
// Parts of this file are modified translations into Rust of GNU R's C sources, and keep
// their copyright notices:
//
// GNU R: src/nmath/snorm.c (kinderman_ramage)
//     Copyright (C) 1998 Ross Ihaka
//     Copyright (C) 2000-2022 The R Core Team
//
// GNU R: src/nmath/sexp.c (next_exponential)
//     Copyright (C) 1998 Ross Ihaka
//     Copyright (C) 2000-2002 the R Core Team
//
// GNU R: src/main/RNG.c (the uniform generators, MT19937)
//     Copyright (C) 1997--2021 The R Core Team
//     Copyright (C) 1995, 1996 Robert Gentleman and Ross Ihaka
//     The Mersenne-Twister part is
//     Copyright (C) 1997, 1999 Makoto Matsumoto and Takuji Nishimura
//
// GNU R: src/main/random.c (walker_sample)
//     Copyright (C) 1997--2022 The R Core Team
//     Copyright (C) 1995, 1996 Robert Gentleman and Ross Ihaka
//     Copyright (C) 2003--2018 The R Foundation
//
// Modifications Copyright (C) 2025-2026 Felix Andreas. This file is free software: you can
// redistribute it and/or modify it under the terms of the GNU General Public License as
// published by the Free Software Foundation, either version 2 of the License, or (at your
// option) any later version. It is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
// PARTICULAR PURPOSE. See the GNU General Public License in LICENSE at the repository root.

//! Random numbers: GNU R's uniform generators, its normal and exponential deviates, `set.seed`,
//! `RNGkind` and `sample`, reproducing GNU R's streams bit for bit.
//!
//! The state lives where GNU R keeps it, in `.Random.seed` in the global environment: an integer
//! vector whose first element encodes the three kinds (`kind + 100 * normal.kind + 10000 *
//! sample.kind`) and whose rest is the generator's state. Every builtin that draws reads it first
//! ([`get_rng_state`]) and writes it back afterwards ([`put_rng_state`]), so R code that saves and
//! restores `.Random.seed` replays a stream. Native code reaches the same generator through the
//! functions below, which the C API's `unif_rand`, `norm_rand`, `exp_rand` and `R_unif_index` wrap.

use {
    crate::{
        NativeSymbol,
        builtins::{BuiltinDef, Call, internal},
        coerce::{coerce_vector, type_name},
        conditions::warning,
        eval::{define, eval, find_var_in_frame},
        interp,
        rmath::{qnorm, qnorm_standard_fill},
        unwind::{R, error},
        values::{
            alloc_uninit, as_integer_scalar, as_logical_scalar, as_real_scalar, global_env,
            integer_vector, ints, ints_mut, is_nil, length, missing_arg, nil, reals,
            reals_mut, string_at, symbols, unbound,
        },
    },
    heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType, gp},
    rustc_hash::{FxHashMap, FxHashSet},
    std::time::{SystemTime, UNIX_EPOCH},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("set.seed", 4, do_set_seed).invisible(),
    internal("RNGkind", 3, do_rng_kind),
    internal("sample", 4, do_sample),
    internal("sample2", 2, do_sample2),
];

/// The generator, the normal and sampling methods, and Box-Muller's saved deviate.
pub struct Rng {
    state: State,
    normal_kind: NormalKind,
    sample_kind: SampleKind,
    /// Box-Muller makes deviates in pairs and keeps the second here. Like GNU R, the value is
    /// not part of `.Random.seed`, and reseeding or choosing Box-Muller again clears it.
    box_muller_saved: f64,
    /// The user-supplied generator last initialized, which stays loaded after a switch to
    /// another kind, as GNU R's `User_unif_fun` does, so a saved `.Random.seed` naming it
    /// still restores.
    user: Option<UserGenerator>,
}

impl Default for Rng {
    fn default() -> Rng {
        Rng {
            // A position past the end makes a draw before any state was loaded seed the twister
            // with MT19937's reference seed instead of running from all-zero words.
            state: State::MersenneTwister {
                position: TWISTER_WORDS as i32 + 1,
                words: Box::new([0; TWISTER_WORDS]),
            },
            normal_kind: NormalKind::Inversion,
            sample_kind: SampleKind::Rejection,
            box_muller_saved: 0.0,
            user: None,
        }
    }
}

/// A uniform generator and its state. GNU R also offers Knuth's TAOCP generators, which ry
/// does not implement, so they have no variant.
pub(crate) enum State {
    WichmannHill([u32; 3]),
    MarsagliaMulticarry([u32; 2]),
    SuperDuper([u32; 2]),
    /// `position` is the index of the next word to temper; at 624 the words are regenerated.
    MersenneTwister {
        position: i32,
        words: Box<[u32; TWISTER_WORDS]>,
    },
    LecuyerCmrg([u32; 6]),
    UserSupplied(UserGenerator),
}

/// The routines of `RNGkind("user-supplied")`, found in the loaded DLLs as GNU R finds them:
/// `user_unif_rand` returns a pointer to the next uniform, and the optional `user_unif_nseed`
/// and `user_unif_seedloc` expose the generator's seed words for `.Random.seed` to mirror.
#[derive(Clone, Copy)]
pub(crate) struct UserGenerator {
    unif_rand: NativeSymbol,
    /// The generator's own seed words, null when it exposes none.
    seeds: *mut u32,
    seed_count: usize,
}

/// GNU R's codes for the uniform generators, which `.Random.seed[1] %% 100` and `RNGkind` use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    WichmannHill = 0,
    MarsagliaMulticarry = 1,
    SuperDuper = 2,
    MersenneTwister = 3,
    KnuthTaocp = 4,
    UserSupplied = 5,
    KnuthTaocp2002 = 6,
    LecuyerCmrg = 7,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NormalKind {
    BuggyKindermanRamage = 0,
    AhrensDieter = 1,
    BoxMuller = 2,
    UserSupplied = 3,
    Inversion = 4,
    KindermanRamage = 5,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SampleKind {
    Rounding = 0,
    Rejection = 1,
}

const TWISTER_WORDS: usize = 624;
const TWISTER_SHIFT: usize = 397;
/// `1 / (2^32 - 1)`, the scale of the 32-bit generators.
const INVERSE_2_32_MINUS_1: f64 = 2.328306437080797e-10;
const LECUYER_MODULUS_1: i64 = 4_294_967_087;
const LECUYER_MODULUS_2: i64 = 4_294_944_443;

/// `GetRNGstate`: loads the kinds and the generator state from `.Random.seed`, or seeds from the
/// clock and the process id when the variable does not exist or is unusable.
pub unsafe fn get_rng_state() -> R<()> {
    let seeds = seeds_variable()?;
    if seeds == unbound() {
        randomize(rng().state.kind())?;
        return Ok(());
    }
    // A builtin that draws reads `.Random.seed` back right after the previous one wrote it, so
    // the stored words usually are the current state already and need no rebuilding.
    if seeds.kind() == SexpType::Integer && stores_current_state(rng(), ints(seeds)) {
        return Ok(());
    }
    let Some(kind) = read_kinds(seeds)? else {
        return Ok(());
    };
    let count = match kind {
        Kind::UserSupplied => rng().user.map_or(0, |generator| generator.seed_count),
        kind => kind.seed_count(),
    };
    let available = length(seeds);
    if available > 1 && available < count + 1 {
        return Err(error("'.Random.seed' has wrong length"));
    }
    if available == 1 && kind != Kind::UserSupplied {
        return randomize(kind);
    }
    let words: Vec<u32> = ints(seeds)
        .get(1..=count)
        .unwrap_or_default()
        .iter()
        .map(|&word| word as u32)
        .collect();
    let mut state = State::from_words(kind, &words)?;
    if fixup_seeds(&mut state, false) {
        rng().state = state;
        Ok(())
    } else {
        randomize(kind)
    }
}

/// What a batch of draws changes outside `.Random.seed`, saved so that the batch can be
/// discarded with [`rollback`].
pub struct Checkpoint {
    box_muller_saved: f64,
}

/// A checkpoint before a batch of draws, or `None` when discarding the batch could be seen:
/// `.Random.seed` must hold exactly the generator's state, so that the next
/// [`get_rng_state`] rebuilds the state from it, and no user-supplied generator may run, since
/// its code would see the discarded draws.
pub unsafe fn checkpoint() -> Option<Checkpoint> {
    let rng = rng();
    if rng.state.kind() == Kind::UserSupplied || rng.normal_kind == NormalKind::UserSupplied {
        return None;
    }
    let Some(cell) = crate::heap().frame_cell(global_env(), symbols().dot_random_seed) else {
        return None;
    };
    let seeds = cell.cdr();
    if cell.has_gp(gp::ACTIVE_BINDING)
        || seeds.kind() != SexpType::Integer
        || seeds.is_altrep()
        || !stores_current_state(rng, ints(seeds))
    {
        return None;
    }
    Some(Checkpoint {
        box_muller_saved: rng.box_muller_saved,
    })
}

/// Discards the draws made since `checkpoint`. The generator's words differ from
/// `.Random.seed` afterwards, so the next [`get_rng_state`] reads them back from it.
pub unsafe fn rollback(checkpoint: Checkpoint) {
    rng().box_muller_saved = checkpoint.box_muller_saved;
}

/// `PutRNGstate`: writes the kinds and the generator state to `.Random.seed`.
pub unsafe fn put_rng_state() -> R<()> {
    let rng = rng();
    let (position, words) = rng.state.words();
    let leading = 1 + usize::from(position.is_some());
    let count = leading + words.len();
    let symbol = symbols().dot_random_seed;
    // Rewriting the bound vector is indistinguishable from binding a fresh one when nothing
    // else references it, and it spares every drawing builtin an allocation of the twister's
    // 626 words.
    let reusable = crate::heap()
        .frame_cell(global_env(), symbol)
        .map(|cell| (cell, cell.cdr()))
        .filter(|&(cell, seeds)| {
            !cell.has_gp(gp::ACTIVE_BINDING | gp::BINDING_LOCKED | gp::WATCHED)
                && cell.tracks_references()
                && seeds.kind() == SexpType::Integer
                && !seeds.is_altrep()
                && seeds.refcount() == 1
                && seeds.attrib() == nil()
                && length(seeds) == count
        });
    let seeds = match reusable {
        Some((_, seeds)) => seeds,
        None => alloc_uninit(SexpType::Integer, count)?,
    };
    let slots = seeds.slice_mut::<i32>();
    slots[0] = kinds_code(rng);
    if let Some(position) = position {
        slots[1] = position as i32;
    }
    for (slot, &word) in slots[leading..].iter_mut().zip(words) {
        *slot = word as i32;
    }
    if reusable.is_some() {
        return Ok(());
    }
    define(symbol, seeds, global_env())
}

/// The first element of `.Random.seed`, which encodes the three kinds.
fn kinds_code(rng: &Rng) -> i32 {
    rng.state.kind() as i32 + 100 * rng.normal_kind as i32 + 10000 * rng.sample_kind as i32
}

/// Whether loading `stored` as `.Random.seed` would leave `rng` exactly as it is. A twister
/// whose position is out of range is excluded, because loading repairs or reseeds it.
fn stores_current_state(rng: &Rng, stored: &[i32]) -> bool {
    let Some((&code, stored)) = stored.split_first() else {
        return false;
    };
    if code != kinds_code(rng) {
        return false;
    }
    // Folding every difference instead of stopping at the first lets the comparison of the
    // twister's 624 words run across SIMD lanes; a mismatch is the rare case.
    let same = |stored: &[i32], words: &[u32]| {
        stored.get(..words.len()).is_some_and(|stored| {
            stored
                .iter()
                .zip(words)
                .fold(0, |difference, (&a, &b)| difference | (a as u32 ^ b))
                == 0
        })
    };
    match rng.state.words() {
        (None, words) => same(stored, words),
        (Some(position), words) => {
            (1..=TWISTER_WORDS as u32).contains(&position)
                && stored
                    .first()
                    .is_some_and(|&stored| stored as u32 == position)
                && stored.get(1..).is_some_and(|rest| same(rest, words))
        }
    }
}

/// A uniform deviate in the open interval (0, 1) from the current generator.
pub unsafe fn unif_rand() -> R<f64> {
    rng().draws().uniform()
}

/// A standard normal deviate by the current normal method.
pub unsafe fn norm_rand() -> R<f64> {
    rng().draws().normal()
}

/// A standard exponential deviate.
pub unsafe fn exp_rand() -> R<f64> {
    rng().draws().exponential()
}

/// `R_unif_index`: a uniform integer in `0..n`, as a double, by the current sampling method.
pub unsafe fn unif_index(n: f64) -> R<f64> {
    rng().draws().index(n)
}

/// A source of the uniform, normal and exponential deviates and the uniform indices that the
/// random generators are built from.
pub trait Deviates {
    fn uniform(&mut self) -> R<f64>;
    fn normal(&mut self) -> R<f64>;
    fn exponential(&mut self) -> R<f64>;
    /// A uniform integer in `0..n`, as a double.
    fn index(&mut self, n: f64) -> R<f64>;

    /// Fills `slots` with uniform deviates strictly inside (0, 1), exactly as as many calls of
    /// [`Deviates::uniform`] would, or declines with `false` without drawing when the generator
    /// may return a value outside that interval.
    fn fill_uniform(&mut self, _slots: &mut [f64]) -> R<bool> {
        Ok(false)
    }

    /// Fills `slots` with standard normal deviates, exactly as as many calls of
    /// [`Deviates::normal`] would.
    fn fill_normal(&mut self, slots: &mut [f64]) -> R<()> {
        for slot in slots {
            *slot = self.normal()?;
        }
        Ok(())
    }
}

/// The session's generator for a batch of draws, with the uniform generator resolved to the
/// concrete type `U` once, so a loop over the batch keeps the generator's state in registers
/// instead of dispatching on its kind for every deviate.
pub struct Draws<'a, U> {
    uniform: U,
    normal_kind: NormalKind,
    sample_kind: SampleKind,
    box_muller_saved: &'a mut f64,
}

/// [`Draws`] for each family of generators: the Mersenne twister, which is the default and
/// gets its own instantiation of every batch loop, and every other generator behind one
/// dispatch per deviate.
pub(crate) enum SessionDraws<'a> {
    Twister(Draws<'a, Twister<'a>>),
    Other(Draws<'a, &'a mut State>),
}

/// Runs `$body` with `$deviates` bound to `&mut` the session's [`Draws`], instantiated once
/// for the Mersenne twister and once for the other generators. The caller brackets the
/// draws with `get_rng_state` and `put_rng_state`.
macro_rules! with_deviates {
    (|$deviates:ident| $body:expr) => {
        match $crate::random::session_draws() {
            $crate::random::SessionDraws::Twister(mut draws) => {
                let $deviates = &mut draws;
                $body
            }
            $crate::random::SessionDraws::Other(mut draws) => {
                let $deviates = &mut draws;
                $body
            }
        }
    };
}
pub(crate) use with_deviates;

/// The session's generator, split by family for [`with_deviates`].
pub(crate) fn session_draws() -> SessionDraws<'static> {
    split_draws(rng())
}

fn split_draws(rng: &mut Rng) -> SessionDraws<'_> {
    let Rng {
        state,
        normal_kind,
        sample_kind,
        box_muller_saved,
        ..
    } = rng;
    let (normal_kind, sample_kind) = (*normal_kind, *sample_kind);
    match state {
        State::MersenneTwister { position, words } => SessionDraws::Twister(Draws {
            uniform: Twister { position, words },
            normal_kind,
            sample_kind,
            box_muller_saved,
        }),
        state => SessionDraws::Other(Draws {
            uniform: state,
            normal_kind,
            sample_kind,
            box_muller_saved,
        }),
    }
}

impl Rng {
    /// The generator behind one dispatch per deviate, for single draws.
    fn draws(&mut self) -> Draws<'_, &mut State> {
        Draws {
            uniform: &mut self.state,
            normal_kind: self.normal_kind,
            sample_kind: self.sample_kind,
            box_muller_saved: &mut self.box_muller_saved,
        }
    }
}

impl<U: Uniform> Draws<'_, U> {
    /// [`Deviates::index`] without the result type, which cannot fail.
    #[inline]
    fn index_value(&mut self, n: f64) -> f64 {
        next_index(&mut self.uniform, self.sample_kind, n)
    }

    /// [`Draws::index_value`] for a whole `n` below 2^53, in integers: the rejection sampler's
    /// candidate compares with `n` exactly either way, and integers spare the per-draw float
    /// tests and `floor` calls.
    #[inline]
    fn whole_index(&mut self, n: u64) -> u64 {
        if self.sample_kind == SampleKind::Rounding {
            // Past the integer range one deviate's 32 bits cannot reach every index, so GNU R's
            // `sample` combines two into 25 more bits (its `ru`).
            let deviate = if n > i32::MAX as u64 {
                const SCALE: f64 = 33554432.0;
                ((SCALE * self.uniform.next()).floor() + self.uniform.next()) / SCALE
            } else {
                self.uniform.next()
            };
            // Saturating, as the double's conversion is for a negative user-supplied deviate.
            return (n as f64 * deviate) as u64;
        }
        if n == 0 {
            return 0;
        }
        let bits = if n <= 1 << 40 {
            u64::BITS - (n - 1).leading_zeros()
        } else {
            index_bits(n as f64)
        };
        rejection_index(&mut self.uniform, bits, n as i64) as u64
    }
}

impl<U: Uniform> Deviates for Draws<'_, U> {
    #[inline]
    fn uniform(&mut self) -> R<f64> {
        Ok(self.uniform.next())
    }

    #[inline]
    fn normal(&mut self) -> R<f64> {
        next_normal(&mut self.uniform, self.normal_kind, self.box_muller_saved)
    }

    #[inline]
    fn exponential(&mut self) -> R<f64> {
        Ok(next_exponential(&mut self.uniform))
    }

    #[inline]
    fn index(&mut self, n: f64) -> R<f64> {
        Ok(self.index_value(n))
    }

    fn fill_uniform(&mut self, slots: &mut [f64]) -> R<bool> {
        if !self.uniform.open() {
            return Ok(false);
        }
        self.uniform.fill(slots);
        Ok(true)
    }

    fn fill_normal(&mut self, slots: &mut [f64]) -> R<()> {
        if self.normal_kind != NormalKind::Inversion {
            for slot in slots {
                *slot = self.normal()?;
            }
            return Ok(());
        }
        // Each normal takes two uniforms, which are drawn a chunk at a time so that both the
        // generator's loop and the quantile's loop run over a whole chunk in L1.
        const CHUNK: usize = 256;
        let mut uniforms = [0.0f64; 2 * CHUNK];
        for chunk in slots.chunks_mut(CHUNK) {
            let uniforms = &mut uniforms[..2 * chunk.len()];
            self.uniform.fill(uniforms);
            if self.uniform.open() {
                inversion_fill(uniforms, chunk);
            } else {
                inversion_fill_portable(uniforms, chunk);
            }
            qnorm_standard_fill(chunk);
        }
        Ok(())
    }
}

/// A uniform generator borrowed for a batch of draws.
pub trait Uniform {
    /// The next deviate.
    fn next(&mut self) -> f64;

    /// Fills `slots` with the next deviates, exactly as as many calls of [`Uniform::next`]
    /// would.
    fn fill(&mut self, slots: &mut [f64]) {
        for slot in slots {
            *slot = self.next();
        }
    }

    /// Whether every deviate lies strictly inside (0, 1). A user-supplied generator's
    /// deviates are returned as they are, so they may not.
    fn open(&self) -> bool {
        true
    }

    /// `floor(65536 * next())`: the sixteen bits of an index draw that one deviate gives.
    #[inline]
    fn chunk(&mut self) -> i64 {
        let scaled = self.next() * 65536.0;
        // Truncation is the floor of a non-negative value, without a call to libm's `floor`;
        // only a user-supplied generator can return a negative deviate.
        if scaled >= 0.0 {
            scaled as i64
        } else {
            scaled.floor() as i64
        }
    }
}

/// The Mersenne twister's state, borrowed from the session.
pub struct Twister<'a> {
    position: &'a mut i32,
    words: &'a mut [u32; TWISTER_WORDS],
}

impl Uniform for Twister<'_> {
    #[inline]
    fn next(&mut self) -> f64 {
        fixup(mersenne_twister(self.position, self.words))
    }

    /// The deviate is the tempered word times 2^-32, exact in a double, so `floor(65536 * u)`
    /// is the word's top sixteen bits. The fix-up only moves a zero word to a tiny positive
    /// value, whose chunk is zero as well.
    #[inline]
    fn chunk(&mut self) -> i64 {
        if !(0..TWISTER_WORDS as i32).contains(self.position) {
            regenerate_twister(self.position, self.words);
        }
        let word = self.words.get(*self.position as usize).copied().unwrap_or(0);
        *self.position += 1;
        i64::from(temper_word(word) >> 16)
    }

    fn fill(&mut self, slots: &mut [f64]) {
        let mut rest = slots;
        while !rest.is_empty() {
            if !(0..TWISTER_WORDS as i32).contains(self.position) {
                regenerate_twister(self.position, self.words);
            }
            let start = *self.position as usize;
            let take = (TWISTER_WORDS - start).min(rest.len());
            let (head, tail) = std::mem::take(&mut rest).split_at_mut(take);
            temper_block(&self.words[start..start + take], head);
            *self.position += take as i32;
            rest = tail;
        }
    }
}

impl Uniform for &mut State {
    #[inline]
    fn next(&mut self) -> f64 {
        next_uniform(self)
    }

    fn fill(&mut self, slots: &mut [f64]) {
        // One dispatch on the kind for the whole batch.
        match &mut **self {
            State::WichmannHill(seeds) => slots.fill_with(|| wichmann_hill(seeds)),
            State::MarsagliaMulticarry(seeds) => slots.fill_with(|| marsaglia_multicarry(seeds)),
            State::SuperDuper(seeds) => slots.fill_with(|| super_duper(seeds)),
            State::MersenneTwister { position, words } => Twister { position, words }.fill(slots),
            State::LecuyerCmrg(seeds) => slots.fill_with(|| lecuyer_cmrg(seeds)),
            State::UserSupplied(generator) => slots.fill_with(|| user_supplied(generator)),
        }
    }

    fn open(&self) -> bool {
        !matches!(**self, State::UserSupplied(_))
    }
}

/// `R_sample_kind`: GNU R's `Sampletype` code of the current sampling method.
pub fn sample_kind() -> i32 {
    rng().sample_kind as i32
}

fn rng() -> &'static mut Rng {
    &mut interp().random
}

/// `set.seed(seed)` with the current kinds, for hosts.
pub unsafe fn set_seed(seed: i32) -> R<()> {
    let rng = rng();
    let kind = rng.state.kind();
    rng.state = State::seeded(kind, seed as u32)?;
    rng.box_muller_saved = 0.0;
    put_rng_state()
}

/// `.Internal(set.seed(seed, kind, normal.kind, sample.kind))`. The kinds arrive as GNU R's
/// codes, or `NULL` to keep the current one; `-1` means the default.
unsafe fn do_set_seed(call: &Call<'_>) -> R<Sexp> {
    let seed = if is_nil(call.arg(0)) {
        time_seed()
    } else {
        let seed = as_integer_scalar(call.arg(0));
        if seed == NA_INTEGER {
            return Err(error("supplied seed is not a valid integer"));
        }
        seed as u32
    };
    let seeds = seeds_variable()?;
    let stored_kind = if seeds == unbound() {
        None
    } else {
        read_kinds(seeds)?
    };
    set_kinds(call.arg(1), call.arg(2), call.arg(3))?;
    let rng = rng();
    let kind = match stored_kind {
        Some(kind) if is_nil(call.arg(1)) => kind,
        _ => rng.state.kind(),
    };
    rng.state = State::seeded(kind, seed)?;
    rng.box_muller_saved = 0.0;
    put_rng_state()?;
    Ok(nil())
}

/// `.Internal(RNGkind(kind, normal.kind, sample.kind))`: returns the codes in effect before the
/// call, then switches to the ones given.
unsafe fn do_rng_kind(call: &Call<'_>) -> R<Sexp> {
    get_rng_state()?;
    let rng = rng();
    let previous = integer_vector(&[
        rng.state.kind() as i32,
        rng.normal_kind as i32,
        rng.sample_kind as i32,
    ])?;
    set_kinds(call.arg(0), call.arg(1), call.arg(2))?;
    Ok(previous)
}

/// `.Internal(sample(n, size, replace, prob))`: indices into `1..=n`.
unsafe fn do_sample(call: &Call<'_>) -> R<Sexp> {
    let (population, size, replace, prob) = (call.arg(0), call.arg(1), call.arg(2), call.arg(3));
    if length(replace) != 1 {
        return Err(error("invalid 'replace' argument"));
    }
    let replace = match as_logical_scalar(replace) {
        NA_LOGICAL => return Err(error("invalid 'replace' argument")),
        flag => flag != 0,
    };
    get_rng_state()?;
    let result = if is_nil(prob) {
        sample_uniform(population, size, replace)?
    } else {
        sample_weighted(population, size, replace, prob)?
    };
    put_rng_state()?;
    Ok(result)
}

/// `.Internal(sample2(n, size))`: sampling without replacement by redrawing duplicates, which
/// `sample.int` uses when `size` is small next to a large `n`.
unsafe fn do_sample2(call: &Call<'_>) -> R<Sexp> {
    let population = as_real_scalar(call.arg(0)).floor();
    let size = as_integer_scalar(call.arg(1));
    if !(0.0..=4.5e15).contains(&population) || (size > 0 && population == 0.0) {
        return Err(error("invalid first argument"));
    }
    if size < 0 {
        return Err(error("invalid 'size' argument"));
    }
    if f64::from(size) > population / 2.0 {
        return Err(error("This algorithm is for size <= n/2"));
    }
    let size = size as usize;
    let integer = population <= f64::from(i32::MAX);
    let result = if integer {
        alloc_uninit(SexpType::Integer, size)?
    } else {
        alloc_uninit(SexpType::Real, size)?
    };
    get_rng_state()?;
    // A duplicate is detected by the set of drawn values. A bitset over the population costs
    // population / 8 bytes and one bit test per draw; a hash set costs about 16 bytes per drawn
    // value, so the bitset wins while the population is at most 128 times the sample.
    let population_index = population as u64;
    if population_index / 128 <= size as u64 {
        let mut words = vec![0u64; population_index.div_ceil(64) as usize];
        draw_distinct(result, integer, size, population_index, |value| {
            let bit = value - 1;
            let word = &mut words[(bit / 64) as usize];
            let mask = 1u64 << (bit % 64);
            let fresh = *word & mask == 0;
            *word |= mask;
            fresh
        });
    } else {
        let mut seen = FxHashSet::with_capacity_and_hasher(size, Default::default());
        draw_distinct(result, integer, size, population_index, |value| seen.insert(value));
    }
    put_rng_state()?;
    Ok(result)
}

/// Fills `result` with `size` draws from `1..=population`, redrawing a value `insert` reports as
/// seen before.
unsafe fn draw_distinct(
    result: Sexp,
    integer: bool,
    size: usize,
    population: u64,
    mut insert: impl FnMut(u64) -> bool,
) {
    with_deviates!(|draws| {
        for index in 0..size {
            // GNU R gives up after 100 redraws and keeps the duplicate; with size <= n/2 a
            // redraw is needed less than half the time, so the cap is never reached in practice.
            let mut value = 0;
            for _ in 0..100 {
                value = draws.whole_index(population) + 1;
                if insert(value) {
                    break;
                }
            }
            if integer {
                ints_mut(result)[index] = value as i32;
            } else {
                reals_mut(result)[index] = value as f64;
            }
        }
    });
}

/// Sampling with equal probabilities. Populations beyond the integer range give doubles.
unsafe fn sample_uniform(population: Sexp, size: Sexp, replace: bool) -> R<Sexp> {
    let count = as_real_scalar(population).floor();
    let size = vector_size(size)?;
    if !(0.0..=4.5e15).contains(&count) || (size > 0 && count == 0.0) {
        return Err(error("invalid first argument"));
    }
    if size < 0 {
        return Err(error("invalid 'size' argument"));
    }
    if !replace && size as f64 > count {
        return Err(error(
            "cannot take a sample larger than the population when 'replace = FALSE'",
        ));
    }
    let size = size as usize;
    // The draws go straight into the result: GNU R's working memory here is the result and one
    // integer table, and a scratch buffer of doubles would more than double the peak for a large
    // permutation.
    let integer = count <= f64::from(i32::MAX);
    let result = if integer {
        alloc_uninit(SexpType::Integer, size)?
    } else {
        alloc_uninit(SexpType::Real, size)?
    };
    let mut store = |index: usize, value: u64| {
        if integer {
            ints_mut(result)[index] = value as i32;
        } else {
            reals_mut(result)[index] = value as f64;
        }
    };
    with_deviates!(|draws| draw_uniform_sample(draws, count, size, replace, &mut store));
    Ok(result)
}

/// The draws of [`sample_uniform`], handed to `store` by position.
fn draw_uniform_sample(
    draws: &mut Draws<'_, impl Uniform>,
    count: f64,
    size: usize,
    replace: bool,
    store: &mut impl FnMut(usize, u64),
) {
    if replace || size < 2 {
        for index in 0..size {
            store(index, draws.whole_index(count as u64) + 1);
        }
    } else {
        // Partial Fisher-Yates over `0..count`: the drawn slot takes the value of the last
        // remaining one. Only the slots touched so far differ from their index, so for a
        // population much larger than the sample a map of those slots replaces the full table.
        let mut remaining = count as u64;
        if remaining <= (size as u64).saturating_mul(8).max(1 << 16) {
            // A 32-bit table halves the working memory whenever the population allows it.
            if remaining <= u64::from(u32::MAX) {
                let slots: Vec<u32> = (0..remaining as u32).collect();
                shuffle_prefix(draws, slots, size, store);
            } else {
                let slots: Vec<u64> = (0..remaining).collect();
                shuffle_prefix(draws, slots, size, store);
            }
        } else {
            let mut moved: FxHashMap<u64, u64> = FxHashMap::default();
            for index in 0..size {
                let slot = draws.whole_index(remaining);
                store(index, moved.get(&slot).copied().unwrap_or(slot) + 1);
                remaining -= 1;
                let last = moved.get(&remaining).copied().unwrap_or(remaining);
                moved.insert(slot, last);
            }
        }
    }
}

/// Draws `size` values without replacement from the table `slots`, which holds the population
/// still to draw from.
fn shuffle_prefix<T: Copy + Into<u64>>(
    draws: &mut Draws<'_, impl Uniform>,
    mut slots: Vec<T>,
    size: usize,
    store: &mut impl FnMut(usize, u64),
) {
    for index in 0..size {
        let slot = draws.whole_index(slots.len() as u64) as usize;
        store(index, slots[slot].into() + 1);
        slots.swap_remove(slot);
    }
}

/// Sampling with the probabilities `prob`, which are normalized first.
unsafe fn sample_weighted(population: Sexp, size: Sexp, replace: bool, prob: Sexp) -> R<Sexp> {
    let count = as_integer_scalar(population);
    let size = as_integer_scalar(size);
    if count == NA_INTEGER || count < 0 || (size > 0 && count == 0) {
        return Err(error("invalid first argument"));
    }
    if size == NA_INTEGER || size < 0 {
        return Err(error("invalid 'size' argument"));
    }
    if !replace && size > count {
        return Err(error(
            "cannot take a sample larger than the population when 'replace = FALSE'",
        ));
    }
    let prob = coerce_vector(prob, SexpType::Real)?;
    let mut probabilities = reals(prob).to_vec();
    if probabilities.len() != count as usize {
        return Err(error("incorrect number of probabilities"));
    }
    normalize_probabilities(&mut probabilities, size as usize, replace)?;
    let mut drawn = vec![0; size as usize];
    with_deviates!(|draws| if replace {
        let likely = probabilities
            .iter()
            .filter(|&&probability| f64::from(count) * probability > 0.1)
            .count();
        if likely > 200 {
            walker_sample(draws, &probabilities, &mut drawn);
        } else {
            inversion_sample(draws, &mut probabilities, &mut drawn);
        }
    } else {
        sample_without_replacement(draws, &mut probabilities, &mut drawn);
    });
    integer_vector(&drawn)
}

/// Checks that the probabilities are finite and non-negative with enough positive ones, then
/// scales them to sum to one.
fn normalize_probabilities(probabilities: &mut [f64], size: usize, replace: bool) -> R<()> {
    let mut sum = 0.0;
    let mut positive = 0;
    for &probability in probabilities.iter() {
        if !probability.is_finite() {
            return Err(error("NA in probability vector"));
        }
        if probability < 0.0 {
            return Err(error("negative probability"));
        }
        if probability > 0.0 {
            positive += 1;
            sum += probability;
        }
    }
    if positive == 0 || (!replace && size > positive) {
        return Err(error("too few positive probabilities"));
    }
    for probability in probabilities.iter_mut() {
        *probability /= sum;
    }
    Ok(())
}

/// With replacement, few likely items: a linear search of the cumulative probabilities, most
/// likely first.
fn inversion_sample(
    draws: &mut Draws<'_, impl Uniform>,
    probabilities: &mut [f64],
    drawn: &mut [i32],
) {
    let count = probabilities.len();
    let mut labels: Vec<i32> = (1..=count as i32).collect();
    sort_descending(probabilities, &mut labels);
    for index in 1..count {
        probabilities[index] += probabilities[index - 1];
    }
    for slot in drawn.iter_mut() {
        let uniform = draws.uniform.next();
        let chosen = probabilities[..count - 1]
            .iter()
            .position(|&cumulative| uniform <= cumulative)
            .unwrap_or(count - 1);
        *slot = labels[chosen];
    }
}

/// With replacement, many likely items: Walker's alias method, which draws in constant time
/// after linear setup.
fn walker_sample(draws: &mut Draws<'_, impl Uniform>, probabilities: &[f64], drawn: &mut [i32]) {
    let count = probabilities.len();
    let mut thresholds: Vec<f64> = probabilities
        .iter()
        .map(|probability| probability * count as f64)
        .collect();
    let mut aliases = vec![0usize; count];
    // Items below their fair share fill `order` from the front and the rest from the back; as
    // a large item gives away mass and drops below one, the front scan reaches it in turn.
    let mut order = vec![0usize; count];
    let mut small_end = 0;
    let mut large_start = count;
    for (item, &threshold) in thresholds.iter().enumerate() {
        if threshold < 1.0 {
            order[small_end] = item;
            small_end += 1;
        } else {
            large_start -= 1;
            order[large_start] = item;
        }
    }
    if small_end > 0 && large_start < count {
        for position in 0..count - 1 {
            let small = order[position];
            let large = order[large_start];
            aliases[small] = large;
            thresholds[large] += thresholds[small] - 1.0;
            if thresholds[large] < 1.0 {
                large_start += 1;
            }
            if large_start >= count {
                break;
            }
        }
    }
    for slot in drawn.iter_mut() {
        // GNU R before 3.6 split one uniform into the column and the coin, which the
        // `Rounding` sampler keeps reproducible; the comparison is against the threshold
        // shifted by the column, exactly as that version computed it.
        let (item, keep) = if draws.sample_kind == SampleKind::Rounding {
            let scaled = draws.uniform.next() * count as f64;
            let item = (scaled as usize).min(count - 1);
            (item, scaled < thresholds[item] + item as f64)
        } else {
            let item = draws.whole_index(count as u64) as usize;
            (item, draws.uniform.next() < thresholds[item])
        };
        *slot = if keep {
            item as i32 + 1
        } else {
            aliases[item] as i32 + 1
        };
    }
}

/// Without replacement: each draw searches the remaining mass, most likely first, and removes
/// the item it picks.
fn sample_without_replacement(
    draws: &mut Draws<'_, impl Uniform>,
    probabilities: &mut [f64],
    drawn: &mut [i32],
) {
    let count = probabilities.len();
    let mut labels: Vec<i32> = (1..=count as i32).collect();
    sort_descending(probabilities, &mut labels);
    let mut total = 1.0;
    let mut last = count.saturating_sub(1);
    for slot in drawn.iter_mut() {
        let target = total * draws.uniform.next();
        let mut mass = 0.0;
        let mut chosen = last;
        for (item, &probability) in probabilities[..last].iter().enumerate() {
            mass += probability;
            if target <= mass {
                chosen = item;
                break;
            }
        }
        *slot = labels[chosen];
        total -= probabilities[chosen];
        probabilities.copy_within(chosen + 1..=last, chosen);
        labels.copy_within(chosen + 1..=last, chosen);
        last = last.saturating_sub(1);
    }
}

/// Heapsort into descending order, carrying `labels` along. GNU R orders the probabilities this
/// way, and equal probabilities end up in the order this particular sort leaves them, which
/// decides which item a draw picks, so another sort would change the samples.
fn sort_descending(values: &mut [f64], labels: &mut [i32]) {
    let count = values.len();
    if count <= 1 {
        return;
    }
    // One-based heap positions, as in the textbook formulation.
    let mut left = count / 2 + 1;
    let mut right = count;
    loop {
        let (key, label);
        if left > 1 {
            left -= 1;
            key = values[left - 1];
            label = labels[left - 1];
        } else {
            key = values[right - 1];
            label = labels[right - 1];
            values[right - 1] = values[0];
            labels[right - 1] = labels[0];
            right -= 1;
            if right == 1 {
                values[0] = key;
                labels[0] = label;
                return;
            }
        }
        let mut parent = left;
        let mut child = left * 2;
        while child <= right {
            if child < right && values[child - 1] > values[child] {
                child += 1;
            }
            if key > values[child - 1] {
                values[parent - 1] = values[child - 1];
                labels[parent - 1] = labels[child - 1];
                parent = child;
                child += child;
            } else {
                child = right + 1;
            }
        }
        values[parent - 1] = key;
        labels[parent - 1] = label;
    }
}

/// `asVecSize`: a vector length from the first element of a number or string.
unsafe fn vector_size(object: Sexp) -> R<i64> {
    if length(object) == 0 {
        return Ok(-1);
    }
    let value = match object.kind() {
        SexpType::Integer => {
            let value = ints(object)[0];
            if value == NA_INTEGER {
                return Err(error("vector size cannot be NA"));
            }
            return Ok(i64::from(value));
        }
        SexpType::Real => reals(object)[0],
        SexpType::String => string_at(object, 0)
            .and_then(|text| crate::coerce::parse_real(&text))
            .unwrap_or(f64::NAN),
        _ => return Ok(-1),
    };
    if value.is_nan() {
        return Err(error("vector size cannot be NA/NaN"));
    }
    if !value.is_finite() {
        return Err(error("vector size cannot be infinite"));
    }
    if value > 4_503_599_627_370_496.0 {
        return Err(error("vector size specified is too large"));
    }
    Ok(value as i64)
}

/// `.Random.seed` in the global environment, forced if it is a promise, or `R_UnboundValue`.
unsafe fn seeds_variable() -> R<Sexp> {
    let symbol = symbols().dot_random_seed;
    let seeds = find_var_in_frame(symbol, global_env())?;
    if seeds.kind() == SexpType::Promise {
        return eval(symbol, global_env());
    }
    Ok(seeds)
}

/// `GetRNGkind`: takes the normal and sampling kinds from `.Random.seed[1]` and returns the
/// uniform kind it names. An unusable value warns, resets every kind to its default, reseeds
/// from the clock, writes the result back and returns `None`.
unsafe fn read_kinds(seeds: Sexp) -> R<Option<Kind>> {
    let problem = if seeds.kind() != SexpType::Integer {
        if seeds == missing_arg() {
            return Err(error(
                "'.Random.seed' is a missing argument with no default",
            ));
        }
        format!(
            "'.Random.seed' is not an integer vector but of type '{}', so ignored",
            type_name(seeds)
        )
    } else {
        let code = ints(seeds).first().copied().unwrap_or(NA_INTEGER);
        if code == NA_INTEGER || !(0..=11000).contains(&code) {
            "'.Random.seed[1]' is not a valid integer, so ignored".to_string()
        } else {
            match (
                Kind::from_code(code % 100),
                NormalKind::from_code(code % 10000 / 100),
                SampleKind::from_code(code / 10000),
            ) {
                (_, None, _) => {
                    "'.Random.seed[1]' is not a valid Normal type, so ignored".to_string()
                }
                (Some(Kind::UserSupplied), ..) if rng().user.is_none() => {
                    "'.Random.seed[1] = 5' but no user-supplied generator, so ignored".to_string()
                }
                (Some(kind), Some(normal_kind), Some(sample_kind)) => {
                    let rng = rng();
                    rng.normal_kind = normal_kind;
                    rng.sample_kind = sample_kind;
                    return Ok(Some(kind));
                }
                _ => "'.Random.seed[1]' is not a valid RNG kind so ignored".to_string(),
            }
        }
    };
    warning(problem)?;
    let rng = rng();
    rng.normal_kind = NormalKind::Inversion;
    rng.sample_kind = SampleKind::Rejection;
    randomize(Kind::MersenneTwister)?;
    put_rng_state()?;
    Ok(None)
}

/// Applies the kind arguments of `set.seed` and `RNGkind`, each `NULL` to leave it alone.
unsafe fn set_kinds(kind: Sexp, normal_kind: Sexp, sample_kind: Sexp) -> R<()> {
    if !is_nil(kind) {
        set_generator(as_integer_scalar(kind))?;
    }
    if !is_nil(normal_kind) {
        let code = as_integer_scalar(normal_kind);
        let normal_kind = match code {
            -1 => NormalKind::Inversion,
            code => NormalKind::from_code(code)
                .ok_or_else(|| error("invalid Normal type in 'RNGkind'"))?,
        };
        if !matches!(
            normal_kind,
            NormalKind::Inversion
                | NormalKind::BoxMuller
                | NormalKind::KindermanRamage
                | NormalKind::BuggyKindermanRamage
        ) {
            return Err(error(format!(
                "normal.kind \"{}\" is not supported by ry",
                normal_kind.name()
            )));
        }
        get_rng_state()?;
        if matches!(
            normal_kind,
            NormalKind::KindermanRamage | NormalKind::BuggyKindermanRamage
        ) && rng().state.kind() == Kind::MarsagliaMulticarry
        {
            warning(
                "RNGkind: severe deviations from normality for Kinderman-Ramage + Marsaglia-Multicarry",
            )?;
        }
        let rng = rng();
        if normal_kind == NormalKind::BoxMuller {
            rng.box_muller_saved = 0.0;
        }
        rng.normal_kind = normal_kind;
        put_rng_state()?;
    }
    if !is_nil(sample_kind) {
        let code = as_integer_scalar(sample_kind);
        let sample_kind = match code {
            -1 => SampleKind::Rejection,
            code => SampleKind::from_code(code)
                .ok_or_else(|| error("invalid sample type in 'RNGkind'"))?,
        };
        get_rng_state()?;
        rng().sample_kind = sample_kind;
        put_rng_state()?;
    }
    Ok(())
}

/// Switches the uniform generator, seeding the new one from a draw of the old one as GNU R
/// does, so the switch itself is reproducible.
unsafe fn set_generator(code: i32) -> R<()> {
    let kind = match code {
        -1 => Kind::MersenneTwister,
        code => Kind::from_code(code)
            .ok_or_else(|| error(format!("RNGkind: unimplemented RNG kind {code}")))?,
    };
    if kind == Kind::MarsagliaMulticarry {
        warning("RNGkind: Marsaglia-Multicarry has poor statistical properties")?;
    }
    get_rng_state()?;
    let uniform = unif_rand()?;
    let seed = if (0.0..=1.0).contains(&uniform) {
        (uniform * f64::from(u32::MAX)) as u32
    } else {
        warning("someone corrupted the random-number generator: re-initializing")?;
        time_seed()
    };
    let rng = rng();
    rng.state = State::seeded(kind, seed)?;
    rng.box_muller_saved = 0.0;
    put_rng_state()
}

/// Seeds `kind` from the clock and the process id, as GNU R does when there is no seed.
unsafe fn randomize(kind: Kind) -> R<()> {
    let rng = rng();
    rng.state = State::seeded(kind, time_seed())?;
    rng.box_muller_saved = 0.0;
    Ok(())
}

fn time_seed() -> u32 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mixed = (u64::from(now.subsec_micros()) << 16) ^ now.as_secs();
    (mixed as u32) ^ (std::process::id() << 16)
}

impl State {
    /// `RNG_Init`: GNU R scrambles the seed with 50 steps of a linear congruential generator,
    /// then fills the state with its next outputs.
    fn seeded(kind: Kind, seed: u32) -> R<State> {
        let mut seed = seed;
        loop {
            for _ in 0..50 {
                seed = lcg_step(seed);
            }
            if kind == Kind::UserSupplied {
                // SAFETY: the routines are the loaded package's, called as GNU R calls them.
                return unsafe { init_user_generator(seed) }.map(State::UserSupplied);
            }
            let mut words = vec![0u32; kind.seed_count()];
            for word in words.iter_mut() {
                seed = lcg_step(seed);
                if kind == Kind::LecuyerCmrg {
                    while i64::from(seed) >= LECUYER_MODULUS_2 {
                        seed = lcg_step(seed);
                    }
                }
                *word = seed;
            }
            let mut state = State::from_words(kind, &words)?;
            if fixup_seeds(&mut state, true) {
                return Ok(state);
            }
            seed = time_seed();
        }
    }

    /// The state of `kind` from the words `.Random.seed` stores after its kind code.
    fn from_words(kind: Kind, words: &[u32]) -> R<State> {
        let word = |index: usize| words.get(index).copied().unwrap_or(0);
        Ok(match kind {
            Kind::WichmannHill => State::WichmannHill([word(0), word(1), word(2)]),
            Kind::MarsagliaMulticarry => State::MarsagliaMulticarry([word(0), word(1)]),
            Kind::SuperDuper => State::SuperDuper([word(0), word(1)]),
            Kind::MersenneTwister => {
                let mut state = Box::new([0u32; TWISTER_WORDS]);
                for (index, slot) in state.iter_mut().enumerate() {
                    *slot = word(index + 1);
                }
                State::MersenneTwister {
                    position: word(0) as i32,
                    words: state,
                }
            }
            Kind::LecuyerCmrg => State::LecuyerCmrg(std::array::from_fn(word)),
            Kind::UserSupplied => {
                let generator = rng()
                    .user
                    .ok_or_else(|| error("'user_unif_rand' not in load table"))?;
                if !generator.seeds.is_null() {
                    for (index, &value) in words.iter().take(generator.seed_count).enumerate() {
                        // SAFETY: `user_unif_nseed` vouches for `seed_count` words.
                        unsafe { *generator.seeds.add(index) = value };
                    }
                }
                State::UserSupplied(generator)
            }
            Kind::KnuthTaocp | Kind::KnuthTaocp2002 => {
                return Err(error(format!(
                    "RNG kind \"{}\" is not supported by ry",
                    kind.name()
                )));
            }
        })
    }

    fn kind(&self) -> Kind {
        match self {
            State::WichmannHill(_) => Kind::WichmannHill,
            State::MarsagliaMulticarry(_) => Kind::MarsagliaMulticarry,
            State::SuperDuper(_) => Kind::SuperDuper,
            State::MersenneTwister { .. } => Kind::MersenneTwister,
            State::LecuyerCmrg(_) => Kind::LecuyerCmrg,
            State::UserSupplied(_) => Kind::UserSupplied,
        }
    }

    /// The words `.Random.seed` stores after its kind code: the twister's position, if any,
    /// then the generator's words.
    fn words(&self) -> (Option<u32>, &[u32]) {
        match self {
            State::WichmannHill(words) => (None, words),
            State::MarsagliaMulticarry(words) | State::SuperDuper(words) => (None, words),
            State::MersenneTwister { position, words } => (Some(*position as u32), &words[..]),
            State::LecuyerCmrg(words) => (None, words),
            State::UserSupplied(generator) if generator.seeds.is_null() => (None, &[]),
            // SAFETY: `user_unif_nseed` vouches for `seed_count` words.
            State::UserSupplied(generator) => (None, unsafe {
                std::slice::from_raw_parts(generator.seeds, generator.seed_count)
            }),
        }
    }
}

/// `RNG_Init` for `USER_UNIF`: finds the generator's routines, seeds it with the scrambled
/// seed, and records where its seed words live.
unsafe fn init_user_generator(seed: u32) -> R<UserGenerator> {
    let find = interp().hooks.find_native_symbol;
    let unif_rand =
        find("user_unif_rand").ok_or_else(|| error("'user_unif_rand' not in load table"))?;
    if let Some(init) = find("user_unif_init") {
        let init = std::mem::transmute::<NativeSymbol, unsafe extern "C" fn(u32)>(init);
        init(seed);
    }
    let mut generator = UserGenerator {
        unif_rand,
        seeds: std::ptr::null_mut(),
        seed_count: 0,
    };
    if let Some(seed_location) = find("user_unif_seedloc") {
        match find("user_unif_nseed") {
            None => warning("cannot read seeds unless 'user_unif_nseed' is supplied")?,
            Some(seed_count) => match *seed_count().cast::<i32>() {
                count @ 0..=625 => {
                    generator.seeds = seed_location().cast::<u32>();
                    generator.seed_count = count as usize;
                }
                _ => warning("seed length must be in 0...625; ignored")?,
            },
        }
    }
    rng().user = Some(generator);
    Ok(generator)
}

fn lcg_step(seed: u32) -> u32 {
    seed.wrapping_mul(69069).wrapping_add(1)
}

/// `FixupSeeds`: repairs a state the generator cannot run from where GNU R can, and returns
/// false when only reseeding helps.
fn fixup_seeds(state: &mut State, initial: bool) -> bool {
    match state {
        State::WichmannHill(words) => {
            for (word, modulus) in words.iter_mut().zip([30269, 30307, 30323]) {
                *word %= modulus;
                if *word == 0 {
                    *word = 1;
                }
            }
            true
        }
        State::SuperDuper(words) => {
            if words[0] == 0 {
                words[0] = 1;
            }
            // The congruential half needs an odd multiplier state.
            words[1] |= 1;
            true
        }
        State::MarsagliaMulticarry(words) => {
            for word in words.iter_mut() {
                if *word == 0 {
                    *word = 1;
                }
            }
            true
        }
        State::MersenneTwister { position, words } => {
            if initial || *position <= 0 {
                *position = TWISTER_WORDS as i32;
            }
            words.iter().any(|&word| word != 0)
        }
        State::LecuyerCmrg(words) => {
            let (first, second) = words.split_at(3);
            let valid = |set: &[u32], modulus: i64| {
                set.iter().any(|&word| word != 0)
                    && set.iter().all(|&word| i64::from(word) < modulus)
            };
            valid(first, LECUYER_MODULUS_1) && valid(second, LECUYER_MODULUS_2)
        }
        State::UserSupplied(_) => true,
    }
}

/// The next uniform deviate in (0, 1).
#[inline]
fn next_uniform(state: &mut State) -> f64 {
    match state {
        State::WichmannHill(seeds) => wichmann_hill(seeds),
        State::MarsagliaMulticarry(seeds) => marsaglia_multicarry(seeds),
        State::SuperDuper(seeds) => super_duper(seeds),
        State::MersenneTwister { position, words } => fixup(mersenne_twister(position, words)),
        State::LecuyerCmrg(seeds) => lecuyer_cmrg(seeds),
        State::UserSupplied(generator) => user_supplied(generator),
    }
}

#[inline]
fn wichmann_hill(seeds: &mut [u32; 3]) -> f64 {
    seeds[0] = seeds[0] * 171 % 30269;
    seeds[1] = seeds[1] * 172 % 30307;
    seeds[2] = seeds[2] * 170 % 30323;
    let value = f64::from(seeds[0]) / 30269.0
        + f64::from(seeds[1]) / 30307.0
        + f64::from(seeds[2]) / 30323.0;
    fixup(value - value.trunc())
}

#[inline]
fn marsaglia_multicarry(seeds: &mut [u32; 2]) -> f64 {
    seeds[0] = 36969u32
        .wrapping_mul(seeds[0] & 0xffff)
        .wrapping_add(seeds[0] >> 16);
    seeds[1] = 18000u32
        .wrapping_mul(seeds[1] & 0xffff)
        .wrapping_add(seeds[1] >> 16);
    fixup(f64::from((seeds[0] << 16) ^ (seeds[1] & 0xffff)) * INVERSE_2_32_MINUS_1)
}

#[inline]
fn super_duper(seeds: &mut [u32; 2]) -> f64 {
    seeds[0] ^= (seeds[0] >> 15) & 0x1ffff;
    seeds[0] ^= seeds[0] << 17;
    seeds[1] = seeds[1].wrapping_mul(69069);
    fixup(f64::from(seeds[0] ^ seeds[1]) * INVERSE_2_32_MINUS_1)
}

// L'Ecuyer's normalizing constant is quoted with every published digit.
#[allow(clippy::excessive_precision)]
#[inline]
fn lecuyer_cmrg(seeds: &mut [u32; 6]) -> f64 {
    let mut first = 1_403_580 * i64::from(seeds[1]) - 810_728 * i64::from(seeds[0]);
    first -= first / LECUYER_MODULUS_1 * LECUYER_MODULUS_1;
    if first < 0 {
        first += LECUYER_MODULUS_1;
    }
    seeds[0] = seeds[1];
    seeds[1] = seeds[2];
    seeds[2] = first as u32;
    let mut second = 527_612 * i64::from(seeds[5]) - 1_370_589 * i64::from(seeds[3]);
    second -= second / LECUYER_MODULUS_2 * LECUYER_MODULUS_2;
    if second < 0 {
        second += LECUYER_MODULUS_2;
    }
    seeds[3] = seeds[4];
    seeds[4] = seeds[5];
    seeds[5] = second as u32;
    let difference = if first > second {
        first - second
    } else {
        first - second + LECUYER_MODULUS_1
    };
    difference as f64 * 2.328306549295727688e-10
}

/// GNU R returns the user's uniform as it is, without the fix-up into (0, 1).
fn user_supplied(generator: &UserGenerator) -> f64 {
    // SAFETY: `user_unif_rand` returns a pointer to its current deviate.
    unsafe { *(generator.unif_rand)().cast::<f64>() }
}

/// Keeps a deviate strictly inside (0, 1): a 32-bit generator can produce exactly 0, and its
/// scaled maximum can round to 1.
fn fixup(value: f64) -> f64 {
    if value <= 0.0 {
        0.5 * INVERSE_2_32_MINUS_1
    } else if 1.0 - value <= 0.0 {
        1.0 - 0.5 * INVERSE_2_32_MINUS_1
    } else {
        value
    }
}

/// One output of MT19937 (Matsumoto and Nishimura, 1998), scaled to [0, 1) by 2^-32.
// Inlined into the deviate loops, which draw hundreds of words per regeneration.
#[inline]
fn mersenne_twister(position: &mut i32, words: &mut [u32; TWISTER_WORDS]) -> f64 {
    if !(0..TWISTER_WORDS as i32).contains(position) {
        regenerate_twister(position, words);
    }
    let value = words.get(*position as usize).copied().unwrap_or(0);
    *position += 1;
    temper(value)
}

/// MT19937's tempering of one word, scaled to [0, 1) by 2^-32.
#[inline(always)]
fn temper(value: u32) -> f64 {
    f64::from(temper_word(value)) * 2.3283064365386963e-10
}

#[inline(always)]
fn temper_word(mut value: u32) -> u32 {
    value ^= value >> 11;
    value ^= (value << 7) & 0x9d2c_5680;
    value ^= (value << 15) & 0xefc6_0000;
    value ^ (value >> 18)
}

/// Tempers a run of the twister's words into fixed-up deviates. The loop has no dependence
/// between iterations, so it runs across SIMD lanes, eight words at a time where AVX2 is
/// available.
fn temper_block(words: &[u32], slots: &mut [f64]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2, as just checked.
        return unsafe { temper_block_avx2(words, slots) };
    }
    temper_block_portable(words, slots);
}

/// The tempering four words at a time. The unsigned word converts exactly: flipping its top
/// bit gives the signed value `word - 2^31`, whose conversion plus `2^31` is exact in a double,
/// and the scale is the power of two `2^-32`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn temper_block_avx2(words: &[u32], slots: &mut [f64]) {
    use std::arch::x86_64::{
        __m128i, _CMP_LE_OQ, _mm_and_si128, _mm_loadu_si128, _mm_set1_epi32, _mm_slli_epi32,
        _mm_srli_epi32, _mm_xor_si128, _mm256_add_pd, _mm256_blendv_pd, _mm256_cmp_pd,
        _mm256_cvtepi32_pd, _mm256_mul_pd, _mm256_set1_pd, _mm256_setzero_pd, _mm256_storeu_pd,
        _mm256_sub_pd,
    };
    const LANES: usize = 4;
    let length = words.len().min(slots.len());
    let blocks = length / LANES * LANES;
    let first_mask = _mm_set1_epi32(0x9d2c_5680_u32 as i32);
    let second_mask = _mm_set1_epi32(0xefc6_0000_u32 as i32);
    let sign = _mm_set1_epi32(i32::MIN);
    let offset = _mm256_set1_pd(2_147_483_648.0);
    let scale = _mm256_set1_pd(2.3283064365386963e-10);
    let zero = _mm256_setzero_pd();
    let one = _mm256_set1_pd(1.0);
    let low = _mm256_set1_pd(0.5 * INVERSE_2_32_MINUS_1);
    let high = _mm256_set1_pd(1.0 - 0.5 * INVERSE_2_32_MINUS_1);
    for start in (0..blocks).step_by(LANES) {
        // SAFETY: `start + LANES <= length`, which neither slice exceeds.
        unsafe {
            let mut word = _mm_loadu_si128(words.as_ptr().add(start).cast::<__m128i>());
            word = _mm_xor_si128(word, _mm_srli_epi32::<11>(word));
            word = _mm_xor_si128(word, _mm_and_si128(_mm_slli_epi32::<7>(word), first_mask));
            word = _mm_xor_si128(word, _mm_and_si128(_mm_slli_epi32::<15>(word), second_mask));
            word = _mm_xor_si128(word, _mm_srli_epi32::<18>(word));
            let whole = _mm256_add_pd(_mm256_cvtepi32_pd(_mm_xor_si128(word, sign)), offset);
            let value = _mm256_mul_pd(whole, scale);
            // `fixup`: the first blend is the upper bound, the second the lower one.
            let at_one = _mm256_cmp_pd::<_CMP_LE_OQ>(_mm256_sub_pd(one, value), zero);
            let at_zero = _mm256_cmp_pd::<_CMP_LE_OQ>(value, zero);
            let fixed = _mm256_blendv_pd(_mm256_blendv_pd(value, high, at_one), low, at_zero);
            _mm256_storeu_pd(slots.as_mut_ptr().add(start), fixed);
        }
    }
    temper_block_portable(&words[blocks..length], &mut slots[blocks..length]);
}

#[inline(always)]
fn temper_block_portable(words: &[u32], slots: &mut [f64]) {
    for (slot, &word) in slots.iter_mut().zip(words) {
        *slot = fixup(temper(word));
    }
}

/// Computes the twister's next 624 words and rewinds its position.
#[inline(never)]
fn regenerate_twister(position: &mut i32, words: &mut [u32; TWISTER_WORDS]) {
    if *position == TWISTER_WORDS as i32 + 1 {
        seed_twister(words, 4357);
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2, as just checked.
        unsafe { twist_words_avx2(words) };
        *position = 0;
        return;
    }
    twist_words_portable(words);
    *position = 0;
}

const TWISTER_UPPER: u32 = 0x8000_0000;
const TWISTER_LOWER: u32 = 0x7fff_ffff;
const TWISTER_MATRIX: u32 = 0x9908_b0df;

/// One word of MT19937's recurrence, from the word itself, its successor and the word
/// `TWISTER_SHIFT` ahead.
#[inline(always)]
fn twist(upper: u32, lower: u32, shifted: u32) -> u32 {
    let mixed = (upper & TWISTER_UPPER) | (lower & TWISTER_LOWER);
    shifted ^ (mixed >> 1) ^ (0u32.wrapping_sub(mixed & 1) & TWISTER_MATRIX)
}

fn twist_words_portable(words: &mut [u32; TWISTER_WORDS]) {
    // The three ranges are the wrap-around of `index + 1` and `index + TWISTER_SHIFT`
    // spelled out, which keeps the modulo out of the refill.
    let last = TWISTER_WORDS - 1;
    for index in 0..TWISTER_WORDS - TWISTER_SHIFT {
        words[index] = twist(words[index], words[index + 1], words[index + TWISTER_SHIFT]);
    }
    for index in TWISTER_WORDS - TWISTER_SHIFT..last {
        words[index] = twist(
            words[index],
            words[index + 1],
            words[index + TWISTER_SHIFT - TWISTER_WORDS],
        );
    }
    words[last] = twist(words[last], words[0], words[TWISTER_SHIFT - 1]);
}

/// The recurrence eight words at a time. A block reads its successors before it writes, and
/// in the second range the words `TWISTER_WORDS - TWISTER_SHIFT` behind it, which earlier
/// blocks finished, so every word gets the value the sequential loop gives it.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn twist_words_avx2(words: &mut [u32; TWISTER_WORDS]) {
    use std::arch::x86_64::{
        __m256i, _mm256_and_si256, _mm256_loadu_si256, _mm256_or_si256, _mm256_set1_epi32,
        _mm256_setzero_si256, _mm256_srli_epi32, _mm256_storeu_si256, _mm256_sub_epi32,
        _mm256_xor_si256,
    };
    const LANES: usize = 8;
    let upper = _mm256_set1_epi32(TWISTER_UPPER as i32);
    let lower = _mm256_set1_epi32(TWISTER_LOWER as i32);
    let matrix = _mm256_set1_epi32(TWISTER_MATRIX as i32);
    let one = _mm256_set1_epi32(1);
    let block = |words: &mut [u32; TWISTER_WORDS], index: usize, shifted: usize| {
        let base = words.as_mut_ptr();
        // SAFETY: the callers keep `index + LANES` (the successors' block end) and `shifted +
        // LANES` within the 624 words.
        unsafe {
            let current = _mm256_loadu_si256(base.add(index).cast::<__m256i>());
            let next = _mm256_loadu_si256(base.add(index + 1).cast::<__m256i>());
            let ahead = _mm256_loadu_si256(base.add(shifted).cast::<__m256i>());
            let mixed = _mm256_or_si256(
                _mm256_and_si256(current, upper),
                _mm256_and_si256(next, lower),
            );
            let odd = _mm256_sub_epi32(_mm256_setzero_si256(), _mm256_and_si256(mixed, one));
            let word = _mm256_xor_si256(
                _mm256_xor_si256(ahead, _mm256_srli_epi32::<1>(mixed)),
                _mm256_and_si256(odd, matrix),
            );
            _mm256_storeu_si256(base.add(index).cast::<__m256i>(), word);
        }
    };
    let first_range = TWISTER_WORDS - TWISTER_SHIFT;
    let last = TWISTER_WORDS - 1;
    let mut index = 0;
    while index + LANES <= first_range {
        block(words, index, index + TWISTER_SHIFT);
        index += LANES;
    }
    while index < first_range {
        words[index] = twist(words[index], words[index + 1], words[index + TWISTER_SHIFT]);
        index += 1;
    }
    while index + LANES <= last {
        block(words, index, index - first_range);
        index += LANES;
    }
    while index < last {
        words[index] = twist(words[index], words[index + 1], words[index - first_range]);
        index += 1;
    }
    words[last] = twist(words[last], words[0], words[TWISTER_SHIFT - 1]);
}

/// The original MT19937 initialization from one seed (Knuth's multiplier 69069), which applies
/// only when the twister runs before any state was set.
fn seed_twister(words: &mut [u32; TWISTER_WORDS], seed: u32) {
    let mut seed = seed;
    for word in words.iter_mut() {
        *word = seed & 0xffff_0000;
        seed = lcg_step(seed);
        *word |= (seed & 0xffff_0000) >> 16;
        seed = lcg_step(seed);
    }
}

/// Inversion takes the high bits of one uniform and adds a second for the low bits, as GNU R
/// does, because one uniform has only 32 bits and the tails need full resolution.
const INVERSION_SCALE: f64 = 134_217_728.0;

/// The probability whose normal quantile is an inversion deviate, from the deviate's two
/// uniforms.
#[inline(always)]
fn inversion_probability(first: f64, second: f64) -> f64 {
    (f64::from((INVERSION_SCALE * first) as i32) + second) / INVERSION_SCALE
}

/// Writes the inversion probability of each pair of uniforms. The uniforms must lie in (0, 1),
/// where the truncation to an integer cannot overflow, because the vector truncation and Rust's
/// saturating cast disagree outside it.
fn inversion_fill(uniforms: &[f64], slots: &mut [f64]) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU supports AVX2, as just checked.
        return unsafe { inversion_fill_avx2(uniforms, slots) };
    }
    inversion_fill_portable(uniforms, slots);
}

fn inversion_fill_portable(uniforms: &[f64], slots: &mut [f64]) {
    for (slot, pair) in slots.iter_mut().zip(uniforms.as_chunks::<2>().0) {
        *slot = inversion_probability(pair[0], pair[1]);
    }
}

/// Four pairs at a time: the unpacks split the first and second uniforms of two loads into
/// lanes ordered 0, 2, 1, 3, and the final permute restores the order. Each lane performs the
/// scalar operations: multiply, truncate, convert back, add and divide.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn inversion_fill_avx2(uniforms: &[f64], slots: &mut [f64]) {
    use std::arch::x86_64::{
        _mm256_add_pd, _mm256_cvtepi32_pd, _mm256_cvttpd_epi32, _mm256_div_pd, _mm256_loadu_pd,
        _mm256_mul_pd, _mm256_permute4x64_pd, _mm256_set1_pd, _mm256_storeu_pd, _mm256_unpackhi_pd,
        _mm256_unpacklo_pd,
    };
    const LANES: usize = 4;
    let length = slots.len().min(uniforms.len() / 2);
    let blocks = length / LANES * LANES;
    let scale = _mm256_set1_pd(INVERSION_SCALE);
    for start in (0..blocks).step_by(LANES) {
        // SAFETY: `start + LANES <= length`, so the slots and the `2 * (start + LANES)`
        // uniforms read are in bounds.
        unsafe {
            let left = _mm256_loadu_pd(uniforms.as_ptr().add(2 * start));
            let right = _mm256_loadu_pd(uniforms.as_ptr().add(2 * start + LANES));
            let first = _mm256_unpacklo_pd(left, right);
            let second = _mm256_unpackhi_pd(left, right);
            let whole = _mm256_cvtepi32_pd(_mm256_cvttpd_epi32(_mm256_mul_pd(scale, first)));
            let probability = _mm256_div_pd(_mm256_add_pd(whole, second), scale);
            let ordered = _mm256_permute4x64_pd::<0b11_01_10_00>(probability);
            _mm256_storeu_pd(slots.as_mut_ptr().add(start), ordered);
        }
    }
    inversion_fill_portable(&uniforms[2 * blocks..], &mut slots[blocks..]);
}

fn next_normal(uniform: &mut impl Uniform, kind: NormalKind, saved: &mut f64) -> R<f64> {
    match kind {
        NormalKind::Inversion => {
            let first = uniform.next();
            let probability = inversion_probability(first, uniform.next());
            Ok(qnorm(probability, 0.0, 1.0, true, false))
        }
        NormalKind::BoxMuller => {
            if *saved != 0.0 {
                let value = *saved;
                *saved = 0.0;
                return Ok(value);
            }
            let theta = 2.0 * std::f64::consts::PI * uniform.next();
            let radius = (-2.0 * uniform.next().ln()).sqrt() + 10.0 * f64::MIN_POSITIVE;
            *saved = radius * theta.sin();
            Ok(radius * theta.cos())
        }
        NormalKind::KindermanRamage => Ok(kinderman_ramage(uniform, false)),
        NormalKind::BuggyKindermanRamage => Ok(kinderman_ramage(uniform, true)),
        other => Err(error(format!(
            "normal.kind \"{}\" is not supported by ry",
            other.name()
        ))),
    }
}

/// Kinderman and Ramage's (1976) normal generator with Josef Leydold's correction, as GNU R's
/// `norm_rand` runs it. `buggy` keeps the uncorrected version R used before 1.7.0, which
/// `RNGversion` still selects: a mistyped constant in the main region and a first region that
/// accepts a candidate without the density test whenever max(u2, u3) <= 0.8056.
#[allow(clippy::excessive_precision)]
fn kinderman_ramage(uniform: &mut impl Uniform, buggy: bool) -> f64 {
    const A: f64 = 2.216035867166471;
    let g = |x: f64| 0.398942280401433 * (-x * x / 2.0).exp() - 0.180025191068563 * (A - x);
    let signed = |u2: f64, u3: f64, value: f64| if u2 < u3 { value } else { -value };
    let u1 = uniform.next();
    if u1 < 0.884070402298758 {
        let u2 = uniform.next();
        let slope = if buggy {
            1.13113163544180
        } else {
            1.131131635444180
        };
        return A * (slope * u1 + u2 - 1.0);
    }
    if u1 >= 0.973310954173898 {
        loop {
            let u2 = uniform.next();
            let u3 = uniform.next();
            let tail = A * A - 2.0 * u3.ln();
            if u2 * u2 < (A * A) / tail {
                return if u1 < 0.986655477086949 {
                    tail.sqrt()
                } else {
                    -tail.sqrt()
                };
            }
        }
    }
    if u1 >= 0.958720824790463 {
        loop {
            let u2 = uniform.next();
            let u3 = uniform.next();
            let value = A - 0.630834801921960 * u2.min(u3);
            if u2.max(u3) <= 0.755591531667601 || 0.034240503750111 * (u2 - u3).abs() <= g(value) {
                return signed(u2, u3, value);
            }
        }
    }
    if u1 >= 0.911312780288703 {
        loop {
            let u2 = uniform.next();
            let u3 = uniform.next();
            let value = 0.479727404222441 + 1.105473661022070 * u2.min(u3);
            if u2.max(u3) <= 0.872834976671790 || 0.049264496342790 * (u2 - u3).abs() <= g(value) {
                return signed(u2, u3, value);
            }
        }
    }
    loop {
        let u2 = uniform.next();
        let u3 = uniform.next();
        let value = 0.479727404222441 - 0.595507138015940 * u2.min(u3);
        if buggy {
            if u2.max(u3) <= 0.805577924423817 {
                return signed(u2, u3, value);
            }
            continue;
        }
        if value < 0.0 {
            continue;
        }
        if u2.max(u3) <= 0.805577924423817 || 0.053377549506886 * (u2 - u3).abs() <= g(value) {
            return signed(u2, u3, value);
        }
    }
}

/// Ahrens and Dieter's (1972) algorithm SA for the standard exponential distribution.
// The coefficients are quoted with every published digit.
#[allow(clippy::excessive_precision)]
fn next_exponential(generator: &mut impl Uniform) -> f64 {
    // `QUANTILES[k - 1]` is the sum of ln(2)^j / j! for j = 1..=k. The digits are those of the
    // published table rather than freshly computed sums, which differ in the last place for a few
    // entries, so that the stream matches GNU R's exactly.
    const QUANTILES: [f64; 16] = [
        std::f64::consts::LN_2,
        0.9333736875190459,
        0.9888777961838675,
        0.9984959252914960040,
        0.9998292811061389,
        0.9999833164100727,
        0.9999985691438767,
        0.9999998906925558,
        0.9999999924734159,
        0.9999999995283275,
        0.9999999999728814,
        0.9999999999985598,
        0.9999999999999289,
        0.9999999999999968,
        0.9999999999999999,
        1.0000000000000000,
    ];
    let mut offset = 0.0;
    let mut uniform = generator.next();
    while uniform <= 0.0 || uniform >= 1.0 {
        uniform = generator.next();
    }
    loop {
        uniform += uniform;
        if uniform > 1.0 {
            break;
        }
        offset += std::f64::consts::LN_2;
    }
    uniform -= 1.0;
    if uniform <= std::f64::consts::LN_2 {
        return offset + uniform;
    }
    let mut minimum = generator.next();
    for &quantile in &QUANTILES[1..] {
        let candidate = generator.next();
        if candidate < minimum {
            minimum = candidate;
        }
        if uniform <= quantile {
            break;
        }
    }
    offset + minimum * std::f64::consts::LN_2
}

/// `R_unif_index`. Rejection draws just enough random bits to cover `0..n` and retries values
/// past `n`, which avoids the bias of scaling one uniform.
fn next_index(uniform: &mut impl Uniform, kind: SampleKind, n: f64) -> f64 {
    if kind == SampleKind::Rounding {
        return (n * uniform.next()).floor();
    }
    if n <= 0.0 {
        return 0.0;
    }
    let bits = index_bits(n);
    loop {
        let candidate = rejection_candidate(uniform, bits);
        if (candidate as f64) < n {
            return candidate as f64;
        }
    }
}

/// [`next_index`]'s rejection loop for a whole `n`.
#[inline]
fn rejection_index(uniform: &mut impl Uniform, bits: u32, n: i64) -> i64 {
    loop {
        let candidate = rejection_candidate(uniform, bits);
        if candidate < n {
            return candidate;
        }
    }
}

/// One candidate of `bits` bits, sixteen per uniform. GNU R's loop runs while the bit count so
/// far is at most `bits`, which draws one chunk more than needed whenever `bits` is a multiple
/// of 16.
#[inline]
fn rejection_candidate(uniform: &mut impl Uniform, bits: u32) -> i64 {
    let mut value: i64 = 0;
    let mut drawn = 0;
    while drawn <= bits {
        value = value.wrapping_mul(65536).wrapping_add(uniform.chunk());
        drawn += 16;
    }
    value & ((1i64 << bits) - 1)
}

/// `ceil(log2(n))`, the bits [`next_index`] draws. For a whole `n` up to 2^40 the count of
/// significant bits of `n - 1` is exactly that, since `log2` of such an `n` is either an exact
/// integer or hundreds of units in the last place away from one. Larger or fractional values,
/// which the C API's `R_unif_index` accepts, take `log2` itself, rounding included.
#[inline]
fn index_bits(n: f64) -> u32 {
    if n.fract() == 0.0 && (1.0..=1_099_511_627_776.0).contains(&n) {
        u64::BITS - (n as u64 - 1).leading_zeros()
    } else {
        n.log2().ceil() as u32
    }
}

impl Kind {
    fn from_code(code: i32) -> Option<Kind> {
        Some(match code {
            0 => Kind::WichmannHill,
            1 => Kind::MarsagliaMulticarry,
            2 => Kind::SuperDuper,
            3 => Kind::MersenneTwister,
            4 => Kind::KnuthTaocp,
            5 => Kind::UserSupplied,
            6 => Kind::KnuthTaocp2002,
            7 => Kind::LecuyerCmrg,
            _ => return None,
        })
    }

    /// The number of state words after the kind code in `.Random.seed`.
    fn seed_count(self) -> usize {
        match self {
            Kind::WichmannHill => 3,
            Kind::MarsagliaMulticarry | Kind::SuperDuper => 2,
            Kind::MersenneTwister => TWISTER_WORDS + 1,
            Kind::KnuthTaocp | Kind::KnuthTaocp2002 => 101,
            Kind::UserSupplied => 0,
            Kind::LecuyerCmrg => 6,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::WichmannHill => "Wichmann-Hill",
            Kind::MarsagliaMulticarry => "Marsaglia-Multicarry",
            Kind::SuperDuper => "Super-Duper",
            Kind::MersenneTwister => "Mersenne-Twister",
            Kind::KnuthTaocp => "Knuth-TAOCP",
            Kind::UserSupplied => "user-supplied",
            Kind::KnuthTaocp2002 => "Knuth-TAOCP-2002",
            Kind::LecuyerCmrg => "L'Ecuyer-CMRG",
        }
    }
}

impl NormalKind {
    fn from_code(code: i32) -> Option<NormalKind> {
        Some(match code {
            0 => NormalKind::BuggyKindermanRamage,
            1 => NormalKind::AhrensDieter,
            2 => NormalKind::BoxMuller,
            3 => NormalKind::UserSupplied,
            4 => NormalKind::Inversion,
            5 => NormalKind::KindermanRamage,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            NormalKind::BuggyKindermanRamage => "Buggy Kinderman-Ramage",
            NormalKind::AhrensDieter => "Ahrens-Dieter",
            NormalKind::BoxMuller => "Box-Muller",
            NormalKind::UserSupplied => "user-supplied",
            NormalKind::Inversion => "Inversion",
            NormalKind::KindermanRamage => "Kinderman-Ramage",
        }
    }
}

impl SampleKind {
    fn from_code(code: i32) -> Option<SampleKind> {
        match code {
            0 => Some(SampleKind::Rounding),
            1 => Some(SampleKind::Rejection),
            _ => None,
        }
    }
}

/// A Mersenne Twister seeded as `set.seed(seed)` seeds it, with inversion for normals, for
/// testing the distribution generators without an interpreter.
#[cfg(test)]
pub(crate) struct SeededDeviates(Rng);

#[cfg(test)]
impl SeededDeviates {
    pub(crate) fn new(seed: u32) -> SeededDeviates {
        let mut rng = Rng::default();
        if let Ok(state) = State::seeded(Kind::MersenneTwister, seed) {
            rng.state = state;
        }
        SeededDeviates(rng)
    }
}

#[cfg(test)]
impl Deviates for SeededDeviates {
    fn uniform(&mut self) -> R<f64> {
        self.0.draws().uniform()
    }

    fn normal(&mut self) -> R<f64> {
        self.0.draws().normal()
    }

    fn exponential(&mut self) -> R<f64> {
        self.0.draws().exponential()
    }

    fn index(&mut self, n: f64) -> R<f64> {
        self.0.draws().index(n)
    }

    fn fill_uniform(&mut self, slots: &mut [f64]) -> R<bool> {
        self.0.draws().fill_uniform(slots)
    }

    fn fill_normal(&mut self, slots: &mut [f64]) -> R<()> {
        self.0.draws().fill_normal(slots)
    }
}

#[cfg(test)]
#[allow(clippy::excessive_precision)]
mod tests {
    use {
        super::*,
        syntax::testing::{SplitMix64, for_each_seed},
    };

    fn seeded_rng(seed: u32) -> Rng {
        let mut rng = Rng::default();
        if let Ok(state) = State::seeded(Kind::MersenneTwister, seed) {
            rng.state = state;
        }
        rng
    }

    // Expected values are GNU R 4.3's, printed with `print(x, digits = 17)`.

    #[test]
    fn uniform_matches_gnu_r() {
        let mut rng = seeded_rng(42);
        let drawn: Vec<f64> = (0..3).map(|_| next_uniform(&mut rng.state)).collect();
        assert_eq!(
            drawn,
            [
                0.91480604349635541,
                0.93707541329786181,
                0.28613953478634357
            ]
        );
    }

    #[test]
    fn seed_words_match_gnu_r() {
        let rng = seeded_rng(42);
        let (position, words) = rng.state.words();
        let words: Vec<i32> = position
            .into_iter()
            .chain(words.iter().copied())
            .map(|word| word as i32)
            .collect();
        assert_eq!(
            words[..5],
            [624, 507561766, 1260545903, 1362917092, -1772566379]
        );
    }

    #[test]
    fn normal_matches_gnu_r() {
        let mut rng = seeded_rng(10);
        let drawn: Vec<f64> = (0..5)
            .map(|_| rng.draws().normal().unwrap_or(f64::NAN))
            .collect();
        assert_eq!(
            drawn,
            [
                0.018746170941826425,
                -0.184252542069063663,
                -1.371330549922511466,
                -0.599167715783718080,
                0.294545126567508031
            ]
        );
    }

    #[test]
    fn exponential_matches_gnu_r() {
        let mut rng = seeded_rng(42);
        let drawn: Vec<f64> = (0..5)
            .map(|_| next_exponential(&mut &mut rng.state))
            .collect();
        assert_eq!(
            drawn,
            [
                0.198336811783888428,
                0.660895252134650946,
                0.283491037786006927,
                0.038191898260265589,
                0.473176629282534122
            ]
        );
    }

    /// The integer index draws must give the float path's indices and leave the generator at
    /// the same position: the twister's chunks taken from its words, and the generic path
    /// through the session state, for both sample kinds and every size of population.
    #[test]
    fn whole_indices_match_float_indices() {
        for_each_seed(200, |seed| {
            let mut picks = SplitMix64(0x51de_0000 ^ seed);
            let sample_kind = if picks.below(4) == 0 {
                SampleKind::Rounding
            } else {
                SampleKind::Rejection
            };
            let mut integers = seeded_rng(seed as u32);
            let mut floats = seeded_rng(seed as u32);
            let mut generic = seeded_rng(seed as u32);
            integers.sample_kind = sample_kind;
            floats.sample_kind = sample_kind;
            generic.sample_kind = sample_kind;
            let State::MersenneTwister { position, words } = &mut integers.state else {
                panic!("a seeded generator is the Mersenne twister");
            };
            let mut twister = Draws {
                uniform: Twister { position, words },
                normal_kind: integers.normal_kind,
                sample_kind,
                box_muller_saved: &mut integers.box_muller_saved,
            };
            for _ in 0..1 + picks.below(2000) {
                let n = match picks.below(5) {
                    0 => 1 + picks.below(20) as u64,
                    1 => (1u64 << picks.below(53)) + picks.below(3) as u64,
                    2 => (1u64 << (40 + picks.below(12))) - picks.below(3) as u64,
                    3 => 4_500_000_000_000_000 - picks.below(1000) as u64,
                    _ => 1 + picks.next_u64() % (1 << 32),
                };
                let expected = if sample_kind == SampleKind::Rounding && n > i32::MAX as u64 {
                    let mut draws = floats.draws();
                    let first = draws.uniform.next();
                    let deviate = ((33554432.0 * first).floor() + draws.uniform.next()) / 33554432.0;
                    (n as f64 * deviate).floor() as u64
                } else {
                    floats.draws().index_value(n as f64) as u64
                };
                assert_eq!(twister.whole_index(n), expected, "n = {n}, {sample_kind:?}");
                assert_eq!(generic.draws().whole_index(n), expected, "n = {n}, {sample_kind:?}");
            }
            let State::MersenneTwister { position: expected, .. } = &floats.state else {
                panic!("a seeded generator is the Mersenne twister");
            };
            assert_eq!(*twister.uniform.position, *expected);
        });
    }

    #[test]
    fn rejection_index_matches_gnu_r() {
        // `set.seed(42); sample(1:10, 4)` after three uniforms.
        let mut rng = seeded_rng(42);
        for _ in 0..3 {
            next_uniform(&mut rng.state);
        }
        let mut remaining = 10.0;
        let mut pool: Vec<i32> = (1..=10).collect();
        let mut drawn = Vec::new();
        for _ in 0..4 {
            let slot = rng.draws().index_value(remaining) as usize;
            drawn.push(pool[slot]);
            pool.swap_remove(slot);
            remaining -= 1.0;
        }
        assert_eq!(drawn, [9, 4, 2, 8]);
    }

    /// The AVX2 kernels must give their portable references' bits: the twister's recurrence,
    /// its tempering with the fix-up, and the inversion probabilities, over random words and
    /// lengths that leave every remainder.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn simd_kernels_match_their_portable_versions() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        for_each_seed(300, |seed| {
            let mut picks = SplitMix64(0x5e1d_0000 ^ seed);
            let mut words = [0u32; TWISTER_WORDS];
            for word in &mut words {
                // Zero words exercise the fix-up's lower bound.
                *word = if picks.below(50) == 0 {
                    0
                } else {
                    picks.next_u64() as u32
                };
            }
            let mut expected = words;
            twist_words_portable(&mut expected);
            // SAFETY: the CPU supports AVX2, as checked above.
            unsafe { twist_words_avx2(&mut words) };
            assert_eq!(words, expected);

            let length = picks.below(TWISTER_WORDS + 1);
            let mut vector = vec![0.0; length];
            let mut portable = vec![0.0; length];
            // SAFETY: as above.
            unsafe { temper_block_avx2(&words[..length], &mut vector) };
            temper_block_portable(&words[..length], &mut portable);
            assert_eq!(bits(&vector), bits(&portable));

            let pairs = length / 2;
            let mut vector = vec![0.0; pairs];
            let mut portable = vec![0.0; pairs];
            // SAFETY: as above.
            unsafe { inversion_fill_avx2(&expected_uniforms(&words, length), &mut vector) };
            inversion_fill_portable(&expected_uniforms(&words, length), &mut portable);
            assert_eq!(bits(&vector), bits(&portable));
        });

        fn expected_uniforms(words: &[u32], length: usize) -> Vec<f64> {
            let mut uniforms = vec![0.0; length];
            temper_block_portable(&words[..length], &mut uniforms);
            uniforms
        }

        fn bits(values: &[f64]) -> Vec<u64> {
            values.iter().map(|value| value.to_bits()).collect()
        }
    }

    /// A batch must draw exactly the deviates, and leave exactly the state, that as many single
    /// draws would, for every generator, normal method and starting position in the stream.
    #[test]
    fn batched_draws_match_single_draws() {
        const KINDS: [Kind; 5] = [
            Kind::WichmannHill,
            Kind::MarsagliaMulticarry,
            Kind::SuperDuper,
            Kind::MersenneTwister,
            Kind::LecuyerCmrg,
        ];
        const NORMALS: [NormalKind; 4] = [
            NormalKind::Inversion,
            NormalKind::BoxMuller,
            NormalKind::KindermanRamage,
            NormalKind::BuggyKindermanRamage,
        ];
        for_each_seed(300, |seed| {
            let mut picks = SplitMix64(0xba7c_0000 ^ seed);
            let kind = KINDS[picks.below(KINDS.len())];
            let normal_kind = NORMALS[picks.below(NORMALS.len())];
            let skip = picks.below(1300);
            let length = picks.below(2500);
            let mut rngs = [(); 2].map(|()| {
                let mut rng = Rng::default();
                rng.state = State::seeded(kind, seed as u32).expect("seeding a built-in generator");
                rng.normal_kind = normal_kind;
                rng
            });
            for rng in &mut rngs {
                for _ in 0..skip {
                    next_uniform(&mut rng.state);
                }
            }
            let [single, batched] = &mut rngs;
            let draw_single = |rng: &mut Rng, uniform: bool| -> Vec<u64> {
                (0..length)
                    .map(|_| {
                        let mut draws = rng.draws();
                        let value = if uniform {
                            draws.uniform()
                        } else {
                            draws.normal()
                        };
                        value.expect("a built-in generator draws").to_bits()
                    })
                    .collect()
            };
            let draw_batch = |rng: &mut Rng, uniform: bool| -> Vec<u64> {
                let mut slots = vec![0.0; length];
                let filled = match split_draws(rng) {
                    SessionDraws::Twister(mut draws) => fill(&mut draws, &mut slots, uniform),
                    SessionDraws::Other(mut draws) => fill(&mut draws, &mut slots, uniform),
                };
                assert!(filled, "a built-in generator fills uniforms in bulk");
                slots.iter().map(|value| value.to_bits()).collect()
            };
            for uniform in [false, true] {
                assert_eq!(
                    draw_single(single, uniform),
                    draw_batch(batched, uniform),
                    "{kind:?} {normal_kind:?} uniform={uniform}"
                );
                assert_eq!(single.state.words(), batched.state.words());
                assert_eq!(
                    single.box_muller_saved.to_bits(),
                    batched.box_muller_saved.to_bits()
                );
            }
        });
    }

    fn fill(draws: &mut Draws<'_, impl Uniform>, slots: &mut [f64], uniform: bool) -> bool {
        let outcome = if uniform {
            draws.fill_uniform(slots)
        } else {
            draws.fill_normal(slots).map(|()| true)
        };
        outcome.expect("a built-in generator draws")
    }

    #[test]
    fn walker_alias_matches_gnu_r() {
        // `set.seed(7); sample(300, 12, TRUE, prob = 1:300)`, under both samplers.
        let total = (1..=300).sum::<i32>() as f64;
        let probabilities: Vec<f64> = (1..=300).map(|weight| weight as f64 / total).collect();
        let mut drawn = [0; 12];
        let mut rng = seeded_rng(7);
        walker_sample(&mut rng.draws(), &probabilities, &mut drawn);
        assert_eq!(
            drawn,
            [238, 250, 211, 118, 168, 271, 268, 236, 287, 134, 150, 290]
        );
        let mut rng = seeded_rng(7);
        rng.sample_kind = SampleKind::Rounding;
        walker_sample(&mut rng.draws(), &probabilities, &mut drawn);
        assert_eq!(
            drawn,
            [237, 120, 283, 290, 74, 238, 103, 232, 276, 138, 275, 70]
        );
    }
}
