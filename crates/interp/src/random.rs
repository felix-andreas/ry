//! Random numbers: GNU R's uniform generators, its normal and exponential deviates, `set.seed`,
//! `RNGkind` and `sample`, reproducing GNU R's streams bit for bit.
//!
//! The state lives where GNU R keeps it, in `.Random.seed` in the global environment: an integer
//! vector whose first element encodes the three kinds (`kind + 100 * normal.kind + 10000 *
//! sample.kind`) and whose rest is the generator's state. Every builtin that draws reads it first
//! ([`get_rng_state`]) and writes it back afterwards ([`put_rng_state`]), so R code that saves and
//! restores `.Random.seed` replays a stream. Native code reaches the same generator through the
//! functions below, which the C API's `unif_rand`, `norm_rand`, `exp_rand` and `R_unif_index` wrap.

use crate::{
    builtins::{BuiltinDef, Call, internal},
    coerce::{coerce_vector, type_name},
    conditions::warning,
    rmath::qnorm,
    eval::{define, eval, find_var_in_frame},
    interp,
    unwind::{R, error},
    values::{
        alloc, as_integer_scalar, as_logical_scalar, as_real_scalar, global_env, integer_vector,
        ints, is_nil, length, missing_arg, nil, real_vector, reals, string_at, symbols, unbound,
    },
};
use heap::object::{NA_INTEGER, NA_LOGICAL, Sexp, SexpType};
use rustc_hash::{FxHashMap, FxHashSet};
use std::time::{SystemTime, UNIX_EPOCH};

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
        }
    }
}

/// A uniform generator and its state. GNU R also offers Knuth's TAOCP generators and a
/// user-supplied one, which ry does not implement, so they have no variant.
enum State {
    WichmannHill([u32; 3]),
    MarsagliaMulticarry([u32; 2]),
    SuperDuper([u32; 2]),
    /// `position` is the index of the next word to temper; at 624 the words are regenerated.
    MersenneTwister {
        position: i32,
        words: Box<[u32; TWISTER_WORDS]>,
    },
    LecuyerCmrg([u32; 6]),
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
    let Some(kind) = read_kinds(seeds)? else {
        return Ok(());
    };
    let count = kind.seed_count();
    let available = length(seeds);
    if available > 1 && available < count + 1 {
        return Err(error("'.Random.seed' has wrong length"));
    }
    if available == 1 {
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

/// `PutRNGstate`: writes the kinds and the generator state to `.Random.seed`.
pub unsafe fn put_rng_state() -> R<()> {
    let rng = rng();
    let code =
        rng.state.kind() as i32 + 100 * rng.normal_kind as i32 + 10000 * rng.sample_kind as i32;
    let words = rng.state.words();
    let seeds = alloc(SexpType::Integer, words.len() + 1)?;
    let slots = seeds.slice_mut::<i32>();
    slots[0] = code;
    for (slot, word) in slots[1..].iter_mut().zip(words) {
        *slot = word as i32;
    }
    define(symbols().dot_random_seed, seeds, global_env())
}

/// A uniform deviate in the open interval (0, 1) from the current generator.
pub unsafe fn unif_rand() -> R<f64> {
    Ok(next_uniform(&mut rng().state))
}

/// A standard normal deviate by the current normal method.
pub unsafe fn norm_rand() -> R<f64> {
    next_normal(rng())
}

/// A standard exponential deviate.
pub unsafe fn exp_rand() -> R<f64> {
    Ok(next_exponential(&mut rng().state))
}

/// `R_unif_index`: a uniform integer in `0..n`, as a double, by the current sampling method.
pub unsafe fn unif_index(n: f64) -> R<f64> {
    Ok(next_index(rng(), n))
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
    let population = as_real_scalar(call.arg(0));
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
    get_rng_state()?;
    let rng = rng();
    let mut drawn = Vec::with_capacity(size as usize);
    let mut seen = FxHashSet::default();
    for _ in 0..size {
        // GNU R gives up after 100 redraws and keeps the duplicate; with size <= n/2 a redraw is
        // needed less than half the time, so the cap is never reached in practice.
        let mut value = 0.0;
        for _ in 0..100 {
            value = next_index(rng, population) + 1.0;
            if seen.insert(value.to_bits()) {
                break;
            }
        }
        drawn.push(value);
    }
    let result = if population > f64::from(i32::MAX) {
        real_vector(&drawn)?
    } else {
        let indices: Vec<i32> = drawn.iter().map(|&value| value as i32).collect();
        integer_vector(&indices)?
    };
    put_rng_state()?;
    Ok(result)
}

/// Sampling with equal probabilities. Populations beyond the integer range give doubles.
unsafe fn sample_uniform(population: Sexp, size: Sexp, replace: bool) -> R<Sexp> {
    let count = as_real_scalar(population);
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
    let rng = rng();
    let mut drawn = Vec::with_capacity(size);
    if replace || size < 2 {
        for _ in 0..size {
            drawn.push(next_index(rng, count) + 1.0);
        }
    } else {
        // Partial Fisher-Yates over `0..count`: the drawn slot takes the value of the last
        // remaining one. Only the slots touched so far differ from their index, so for a
        // population much larger than the sample a map of those slots replaces the full table.
        let mut remaining = count as u64;
        if remaining <= (size as u64).saturating_mul(8).max(1 << 16) {
            let mut slots: Vec<u64> = (0..remaining).collect();
            for _ in 0..size {
                let slot = next_index(rng, remaining as f64) as usize;
                drawn.push(slots[slot] as f64 + 1.0);
                remaining -= 1;
                slots.swap_remove(slot);
            }
        } else {
            let mut moved: FxHashMap<u64, u64> = FxHashMap::default();
            for _ in 0..size {
                let slot = next_index(rng, remaining as f64) as u64;
                drawn.push(moved.get(&slot).copied().unwrap_or(slot) as f64 + 1.0);
                remaining -= 1;
                let last = moved.get(&remaining).copied().unwrap_or(remaining);
                moved.insert(slot, last);
            }
        }
    }
    if count > f64::from(i32::MAX) {
        real_vector(&drawn)
    } else {
        let indices: Vec<i32> = drawn.iter().map(|&value| value as i32).collect();
        integer_vector(&indices)
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
    let rng = rng();
    if replace {
        let likely = probabilities
            .iter()
            .filter(|&&probability| f64::from(count) * probability > 0.1)
            .count();
        if likely > 200 {
            walker_sample(rng, &probabilities, &mut drawn);
        } else {
            inversion_sample(&mut rng.state, &mut probabilities, &mut drawn);
        }
    } else {
        sample_without_replacement(&mut rng.state, &mut probabilities, &mut drawn);
    }
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
fn inversion_sample(state: &mut State, probabilities: &mut [f64], drawn: &mut [i32]) {
    let count = probabilities.len();
    let mut labels: Vec<i32> = (1..=count as i32).collect();
    sort_descending(probabilities, &mut labels);
    for index in 1..count {
        probabilities[index] += probabilities[index - 1];
    }
    for slot in drawn.iter_mut() {
        let uniform = next_uniform(state);
        let chosen = probabilities[..count - 1]
            .iter()
            .position(|&cumulative| uniform <= cumulative)
            .unwrap_or(count - 1);
        *slot = labels[chosen];
    }
}

/// With replacement, many likely items: Walker's alias method, which draws in constant time
/// after linear setup.
fn walker_sample(rng: &mut Rng, probabilities: &[f64], drawn: &mut [i32]) {
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
        let (item, keep) = if rng.sample_kind == SampleKind::Rounding {
            let scaled = next_uniform(&mut rng.state) * count as f64;
            let item = (scaled as usize).min(count - 1);
            (item, scaled < thresholds[item] + item as f64)
        } else {
            let item = next_index(rng, count as f64) as usize;
            (item, next_uniform(&mut rng.state) < thresholds[item])
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
fn sample_without_replacement(state: &mut State, probabilities: &mut [f64], drawn: &mut [i32]) {
    let count = probabilities.len();
    let mut labels: Vec<i32> = (1..=count as i32).collect();
    sort_descending(probabilities, &mut labels);
    let mut total = 1.0;
    let mut last = count.saturating_sub(1);
    for slot in drawn.iter_mut() {
        let target = total * next_uniform(state);
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
                (Some(Kind::UserSupplied), ..) => {
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
            Kind::KnuthTaocp | Kind::KnuthTaocp2002 | Kind::UserSupplied => {
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
        }
    }

    fn words(&self) -> Vec<u32> {
        match self {
            State::WichmannHill(words) => words.to_vec(),
            State::MarsagliaMulticarry(words) | State::SuperDuper(words) => words.to_vec(),
            State::MersenneTwister { position, words } => std::iter::once(*position as u32)
                .chain(words.iter().copied())
                .collect(),
            State::LecuyerCmrg(words) => words.to_vec(),
        }
    }
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
    }
}

/// The next uniform deviate in (0, 1).
// L'Ecuyer's normalizing constant is quoted with every published digit.
#[allow(clippy::excessive_precision)]
fn next_uniform(state: &mut State) -> f64 {
    match state {
        State::WichmannHill(seeds) => {
            seeds[0] = seeds[0] * 171 % 30269;
            seeds[1] = seeds[1] * 172 % 30307;
            seeds[2] = seeds[2] * 170 % 30323;
            let value = f64::from(seeds[0]) / 30269.0
                + f64::from(seeds[1]) / 30307.0
                + f64::from(seeds[2]) / 30323.0;
            fixup(value - value.trunc())
        }
        State::MarsagliaMulticarry(seeds) => {
            seeds[0] = 36969u32
                .wrapping_mul(seeds[0] & 0xffff)
                .wrapping_add(seeds[0] >> 16);
            seeds[1] = 18000u32
                .wrapping_mul(seeds[1] & 0xffff)
                .wrapping_add(seeds[1] >> 16);
            fixup(f64::from((seeds[0] << 16) ^ (seeds[1] & 0xffff)) * INVERSE_2_32_MINUS_1)
        }
        State::SuperDuper(seeds) => {
            seeds[0] ^= (seeds[0] >> 15) & 0x1ffff;
            seeds[0] ^= seeds[0] << 17;
            seeds[1] = seeds[1].wrapping_mul(69069);
            fixup(f64::from(seeds[0] ^ seeds[1]) * INVERSE_2_32_MINUS_1)
        }
        State::MersenneTwister { position, words } => fixup(mersenne_twister(position, words)),
        State::LecuyerCmrg(seeds) => {
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
    }
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
fn mersenne_twister(position: &mut i32, words: &mut [u32; TWISTER_WORDS]) -> f64 {
    const UPPER: u32 = 0x8000_0000;
    const LOWER: u32 = 0x7fff_ffff;
    let twist = |upper: u32, lower: u32, shifted: u32| {
        let mixed = (upper & UPPER) | (lower & LOWER);
        shifted ^ (mixed >> 1) ^ if mixed & 1 == 1 { 0x9908_b0df } else { 0 }
    };
    if *position >= TWISTER_WORDS as i32 {
        if *position == TWISTER_WORDS as i32 + 1 {
            seed_twister(words, 4357);
        }
        for index in 0..TWISTER_WORDS {
            let next = words[(index + 1) % TWISTER_WORDS];
            let shifted = words[(index + TWISTER_SHIFT) % TWISTER_WORDS];
            words[index] = twist(words[index], next, shifted);
        }
        *position = 0;
    }
    let mut value = words[*position as usize];
    *position += 1;
    value ^= value >> 11;
    value ^= (value << 7) & 0x9d2c_5680;
    value ^= (value << 15) & 0xefc6_0000;
    value ^= value >> 18;
    f64::from(value) * 2.3283064365386963e-10
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

fn next_normal(rng: &mut Rng) -> R<f64> {
    match rng.normal_kind {
        NormalKind::Inversion => {
            // One uniform has only 32 bits; GNU R adds a second for the low bits so the tails
            // are reached with full resolution.
            const SCALE: f64 = 134_217_728.0;
            let first = next_uniform(&mut rng.state);
            let combined = f64::from((SCALE * first) as i32) + next_uniform(&mut rng.state);
            Ok(qnorm(combined / SCALE, 0.0, 1.0, true, false))
        }
        NormalKind::BoxMuller => {
            if rng.box_muller_saved != 0.0 {
                let saved = rng.box_muller_saved;
                rng.box_muller_saved = 0.0;
                return Ok(saved);
            }
            let theta = 2.0 * std::f64::consts::PI * next_uniform(&mut rng.state);
            let radius =
                (-2.0 * next_uniform(&mut rng.state).ln()).sqrt() + 10.0 * f64::MIN_POSITIVE;
            rng.box_muller_saved = radius * theta.sin();
            Ok(radius * theta.cos())
        }
        NormalKind::KindermanRamage => Ok(kinderman_ramage(&mut rng.state, false)),
        NormalKind::BuggyKindermanRamage => Ok(kinderman_ramage(&mut rng.state, true)),
        other => Err(error(format!(
            "normal.kind \"{}\" is not supported by ry",
            other.name()
        ))),
    }
}

/// Kinderman and Ramage's (1976) normal generator with Josef Leydold's correction, as GNU R's
/// `norm_rand` runs it. `buggy` keeps the uncorrected version R used before 1.7.0, which
/// `RNGversion` still selects: a mistyped constant in the main region and a first region that
/// accepts every candidate.
#[allow(clippy::excessive_precision)]
fn kinderman_ramage(state: &mut State, buggy: bool) -> f64 {
    const A: f64 = 2.216035867166471;
    let g = |x: f64| 0.398942280401433 * (-x * x / 2.0).exp() - 0.180025191068563 * (A - x);
    let signed = |u2: f64, u3: f64, value: f64| if u2 < u3 { value } else { -value };
    let u1 = next_uniform(state);
    if u1 < 0.884070402298758 {
        let u2 = next_uniform(state);
        let slope = if buggy { 1.13113163544180 } else { 1.131131635444180 };
        return A * (slope * u1 + u2 - 1.0);
    }
    if u1 >= 0.973310954173898 {
        loop {
            let u2 = next_uniform(state);
            let u3 = next_uniform(state);
            let tail = A * A - 2.0 * u3.ln();
            if u2 * u2 < (A * A) / tail {
                return if u1 < 0.986655477086949 { tail.sqrt() } else { -tail.sqrt() };
            }
        }
    }
    if u1 >= 0.958720824790463 {
        loop {
            let u2 = next_uniform(state);
            let u3 = next_uniform(state);
            let value = A - 0.630834801921960 * u2.min(u3);
            if u2.max(u3) <= 0.755591531667601 || 0.034240503750111 * (u2 - u3).abs() <= g(value) {
                return signed(u2, u3, value);
            }
        }
    }
    if u1 >= 0.911312780288703 {
        loop {
            let u2 = next_uniform(state);
            let u3 = next_uniform(state);
            let value = 0.479727404222441 + 1.105473661022070 * u2.min(u3);
            if u2.max(u3) <= 0.872834976671790 || 0.049264496342790 * (u2 - u3).abs() <= g(value) {
                return signed(u2, u3, value);
            }
        }
    }
    loop {
        let u2 = next_uniform(state);
        let u3 = next_uniform(state);
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
fn next_exponential(state: &mut State) -> f64 {
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
    let mut uniform = next_uniform(state);
    while uniform <= 0.0 || uniform >= 1.0 {
        uniform = next_uniform(state);
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
    let mut minimum = next_uniform(state);
    for &quantile in &QUANTILES[1..] {
        let candidate = next_uniform(state);
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
fn next_index(rng: &mut Rng, n: f64) -> f64 {
    if rng.sample_kind == SampleKind::Rounding {
        return (n * next_uniform(&mut rng.state)).floor();
    }
    if n <= 0.0 {
        return 0.0;
    }
    let bits = n.log2().ceil() as u32;
    loop {
        // Sixteen bits per uniform; GNU R's loop runs while the bit count so far is at most
        // `bits`, which draws one chunk more than needed whenever `bits` is a multiple of 16.
        let mut value: i64 = 0;
        let mut drawn = 0;
        while drawn <= bits {
            let chunk = (next_uniform(&mut rng.state) * 65536.0).floor() as i64;
            value = value.wrapping_mul(65536).wrapping_add(chunk);
            drawn += 16;
        }
        let candidate = (value & ((1i64 << bits) - 1)) as f64;
        if candidate < n {
            return candidate;
        }
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
impl crate::distributions::Deviates for SeededDeviates {
    fn uniform(&mut self) -> R<f64> {
        Ok(next_uniform(&mut self.0.state))
    }

    fn normal(&mut self) -> R<f64> {
        next_normal(&mut self.0)
    }

    fn exponential(&mut self) -> R<f64> {
        Ok(next_exponential(&mut self.0.state))
    }

    fn index(&mut self, n: f64) -> R<f64> {
        Ok(next_index(&mut self.0, n))
    }
}

#[cfg(test)]
#[allow(clippy::excessive_precision)]
mod tests {
    use super::*;

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
        let words: Vec<i32> = rng.state.words().iter().map(|&word| word as i32).collect();
        assert_eq!(
            words[..5],
            [624, 507561766, 1260545903, 1362917092, -1772566379]
        );
    }

    #[test]
    fn normal_matches_gnu_r() {
        let mut rng = seeded_rng(10);
        let drawn: Vec<f64> = (0..5)
            .map(|_| next_normal(&mut rng).unwrap_or(f64::NAN))
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
        let drawn: Vec<f64> = (0..5).map(|_| next_exponential(&mut rng.state)).collect();
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
            let slot = next_index(&mut rng, remaining) as usize;
            drawn.push(pool[slot]);
            pool.swap_remove(slot);
            remaining -= 1.0;
        }
        assert_eq!(drawn, [9, 4, 2, 8]);
    }

    #[test]
    fn walker_alias_matches_gnu_r() {
        // `set.seed(7); sample(300, 12, TRUE, prob = 1:300)`, under both samplers.
        let total = (1..=300).sum::<i32>() as f64;
        let probabilities: Vec<f64> = (1..=300).map(|weight| weight as f64 / total).collect();
        let mut drawn = [0; 12];
        let mut rng = seeded_rng(7);
        walker_sample(&mut rng, &probabilities, &mut drawn);
        assert_eq!(
            drawn,
            [238, 250, 211, 118, 168, 271, 268, 236, 287, 134, 150, 290]
        );
        let mut rng = seeded_rng(7);
        rng.sample_kind = SampleKind::Rounding;
        walker_sample(&mut rng, &probabilities, &mut drawn);
        assert_eq!(
            drawn,
            [237, 120, 283, 290, 74, 238, 103, 232, 276, 138, 275, 70]
        );
    }
}
