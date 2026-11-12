//! Model tests for the typed core's native values: random operation sequences from one seed,
//! checked against plain Rust models, plus fixed tables of GNU R's own output.
//!
//! Replay a failure with `FUZZ_SEED=<seed> cargo test -p typed --test test_values`, and run
//! longer with `FUZZ_ITERS=<n>`.

use {std::collections::HashMap, typed::values::*};

const NA: i32 = i32::MIN;
const NA_REAL_BITS: u64 = 0x7FF0_0000_0000_07A2;

#[test]
fn strings_switch_to_heap_past_fifteen_bytes() {
    let before = live();
    for text in [
        "",
        "a",
        "fifteen bytes!!",
        "sixteen bytes!!!",
        "seventeen bytes!!",
        "a considerably longer string that needs a heap buffer of its own",
        "ééééééé",
        "éééééééé",
        "日本語のテキスト",
    ] {
        let string = str_of(text);
        assert_eq!(string.is_inline(), text.len() <= 15, "{text:?}");
        assert_eq!(string.len(), text.len());
        assert_eq!(text_of(string), text);
        unsafe {
            assert_eq!(
                ry_str_nchar(string.lo, string.hi),
                text.chars().count() as i32
            );
            let again = str_of(text);
            assert_eq!(ry_str_equal(string.lo, string.hi, again.lo, again.hi), 1);
            ry_str_drop(again.lo, again.hi);
            ry_str_dup(string.lo, string.hi);
            ry_str_drop(string.lo, string.hi);
            ry_str_drop(string.lo, string.hi);
        }
    }
    let inline = str_of("abc");
    assert_eq!(inline.lo, u64::from_le_bytes(*b"abc\0\0\0\0\0"));
    assert_eq!(inline.hi, 0x83 << 56);
    // Every inline length holds its bytes in order with zeros after them, read from the end of
    // a buffer so that a read past the string's last byte would show under a memory checker.
    let source = "abcdefghijklmno";
    for length in 0..=source.len() {
        let text = source[..length].to_owned().into_boxed_str();
        let string = str_of(&text);
        let mut expected = [0u8; 16];
        expected[..length].copy_from_slice(text.as_bytes());
        expected[15] = 0x80 | length as u8;
        let words = [string.lo.to_le_bytes(), string.hi.to_le_bytes()].concat();
        assert_eq!(words, expected, "{text:?}");
    }
    assert_eq!(live(), before, "strings leaked");
}

#[test]
fn static_strings_are_never_counted() {
    let before = live();
    let text = "a constant the compiler bakes into code";
    let constant = unsafe { ry_str_static(text.as_ptr(), text.len() as u64) };
    assert_eq!(live(), before);
    unsafe {
        ry_str_dup(constant.lo, constant.hi);
        ry_str_drop(constant.lo, constant.hi);
        ry_str_drop(constant.lo, constant.hi);
    }
    assert_eq!(text_of(constant), text);
}

#[test]
fn lists_follow_their_models() {
    let mut rng = Rng::from_env();
    let iterations = iterations(300);
    let before = live();
    for _ in 0..iterations {
        let kind = [INT, DBL, STR][rng.below(3)];
        let mut values: Vec<(*mut List, Model)> = Vec::new();
        for _ in 0..20 {
            let choice = rng.below(12);
            if values.is_empty() || choice == 0 {
                let model = random_model(&mut rng, kind);
                values.push((build(&model), model));
                continue;
            }
            let at = rng.below(values.len());
            let owned = rng.below(2) == 1;
            let (list, model) = values[at].clone();
            if owned {
                values.swap_remove(at);
            }
            let flag = owned as u32;
            let (result, expected) = unsafe {
                match choice {
                    1 => {
                        let item = random_item(&mut rng, kind);
                        let (word0, word1) = item.words();
                        let mut expected = model.clone();
                        expected.push(item);
                        (ry_list_push(list, flag, word0, word1), expected)
                    }
                    2 if !model.is_empty() => {
                        let index = rng.below(model.len());
                        let item = random_item(&mut rng, kind);
                        let (word0, word1) = item.words();
                        let mut expected = model.clone();
                        expected.set(index, item);
                        (
                            ry_list_set(list, flag, index as u64, word0, word1),
                            expected,
                        )
                    }
                    3 => {
                        let other = rng.below(values.len().max(1));
                        let (b, b_model) = match values.get(other) {
                            Some((b, b_model)) => {
                                ry_list_dup(*b);
                                (*b, b_model.clone())
                            }
                            None => {
                                let b_model = random_model(&mut rng, kind);
                                (build(&b_model), b_model)
                            }
                        };
                        let mut expected = model.clone();
                        expected.extend(&b_model);
                        (ry_list_concat(list, flag, b, 1), expected)
                    }
                    4 => (ry_list_reverse(list, flag), model.reversed()),
                    5 if kind != STR => {
                        let decreasing = rng.below(2) == 1;
                        let sorted = ry_list_sort(list, flag, decreasing as u32);
                        match model.sorted(decreasing) {
                            Some(expected) => (sorted, expected),
                            None => {
                                assert!(sorted.is_null(), "a sort of a list with `NA` succeeded");
                                continue;
                            }
                        }
                    }
                    6 => (ry_list_unique(list, flag), model.unique()),
                    7 => {
                        let start = rng.below(model.len() + 1);
                        let count = rng.below(model.len() - start + 1);
                        (
                            ry_list_slice(list, flag, start as u64, count as u64),
                            model.slice(start, count),
                        )
                    }
                    8 => (ry_list_unshare(list, flag), model.clone()),
                    9 => {
                        if !owned {
                            ry_list_dup(list);
                        }
                        (list, model.clone())
                    }
                    _ => {
                        if owned {
                            ry_list_drop(list);
                        }
                        continue;
                    }
                }
            };
            assert!(!result.is_null(), "operation {choice} failed");
            values.push((result, expected));
            for (list, model) in &values {
                assert_eq!(
                    &read(*list),
                    model,
                    "after operation {choice} (owned: {owned})"
                );
            }
        }
        for (list, _) in values {
            unsafe { ry_list_drop(list) };
        }
        assert_eq!(live(), before, "lists leaked");
    }
}

#[test]
fn mutating_a_shared_list_never_shows_through() {
    let before = live();
    unsafe {
        let original = build(&Model::Str(vec![
            "first item, long enough for the heap".into(),
            "b".into(),
        ]));
        ry_list_dup(original);
        let unshared = ry_list_unshare(original, 1);
        assert_ne!(unshared, original);
        let replacement = str_of("replaced");
        let changed = ry_list_set(unshared, 1, 0, replacement.lo, replacement.hi);
        assert_eq!(changed, unshared, "a unique list is set in place");
        assert_eq!(
            read(original),
            Model::Str(vec![
                "first item, long enough for the heap".into(),
                "b".into()
            ])
        );
        assert_eq!(
            read(changed),
            Model::Str(vec!["replaced".into(), "b".into()])
        );
        let pushed = ry_list_push(original, 0, Str::EMPTY.lo, Str::EMPTY.hi);
        assert_eq!(read(original).len(), 2);
        assert_eq!(read(pushed).len(), 3);
        for list in [original, changed, pushed] {
            ry_list_drop(list);
        }
    }
    assert_eq!(live(), before);
}

#[test]
fn owned_unique_operands_are_reused() {
    let before = live();
    unsafe {
        let a = build(&Model::Dbl(vec![1.0, 2.0, 3.0]));
        let b = build(&Model::Dbl(vec![10.0, 20.0, 30.0]));
        let sum = ry_arith(ADD, DBL as u32, a, 1, b, 0);
        assert_eq!(sum, a, "owned unique left operand reused");
        let product = ry_arith(MUL, DBL as u32, b, 0, sum, 1);
        assert_eq!(product, sum, "owned unique right operand reused");
        assert_eq!(read(product), Model::Dbl(vec![110.0, 440.0, 990.0]));
        let fresh = ry_arith(SUB, DBL as u32, b, 0, product, 0);
        assert_ne!(fresh, product);
        assert_ne!(fresh, b);
        ry_list_dup(fresh);
        let shared = ry_arith_vs(MUL, DBL as u32, fresh, 1, 2f64.to_bits());
        assert_ne!(shared, fresh, "a shared operand is never written");
        assert_eq!(read(fresh), Model::Dbl(vec![-100.0, -420.0, -960.0]));
        let scaled = ry_arith_vs(MUL, DBL as u32, fresh, 1, 0.5f64.to_bits());
        assert_eq!(scaled, fresh);
        let negated = ry_math(NEG, scaled, 1);
        assert_eq!(negated, scaled);
        for count in [0u64, 1, 3] {
            let sequence = ry_list_seq_len(count);
            assert_eq!(
                read(sequence),
                Model::Int((1..=count as i32).collect()),
                "seq_len({count})"
            );
            ry_list_drop(sequence);
        }
        let ints = ry_list_seq(1, 4);
        let squared = ry_arith(MUL, INT as u32, ints, 1, ints, 0);
        assert_ne!(
            squared, ints,
            "an operand is not written while the other side reads it"
        );
        let pushed = ry_list_push(squared, 1, 5, 0);
        let pushed_again = ry_list_push(pushed, 1, 6, 0);
        assert_eq!(
            pushed_again, pushed,
            "pushing within capacity stays in place"
        );
        assert_eq!(read(pushed_again), Model::Int(vec![1, 4, 9, 16, 5, 6]));
        for list in [b, product, shared, negated, ints, pushed_again] {
            ry_list_drop(list);
        }
    }
    assert_eq!(live(), before);
}

#[test]
fn growing_past_the_pooled_sizes() {
    let before = live();
    unsafe {
        let mut list = ry_list_new(DBL as u32, 0);
        for index in 0..20_000 {
            list = ry_list_push(list, 1, (index as f64).to_bits(), 0);
        }
        assert_eq!((*list).length, 20_000);
        assert_eq!(ry_sum_dbl(list), (0..20_000).sum::<i64>() as f64);
        let sliced = ry_list_slice(list, 1, 19_998, 2);
        assert_eq!(read(sliced), Model::Dbl(vec![19_998.0, 19_999.0]));
        ry_list_drop(sliced);
    }
    assert_eq!(live(), before);
}

#[test]
fn integer_arithmetic_follows_r() {
    let before = live();
    let int = |values: &[i32]| build(&Model::Int(values.to_vec()));
    type Case<'a> = (u32, &'a [i32], &'a [i32], Option<Model>);
    let cases: &[Case] = &[
        (
            ADD,
            &[1, NA, 3],
            &[1, 1, 1],
            Some(Model::Int(vec![2, NA, 4])),
        ),
        (ADD, &[i32::MAX], &[1], None),
        (SUB, &[-i32::MAX], &[1], None),
        (MUL, &[i32::MAX], &[2], None),
        (
            IDIV,
            &[5, 0, -7, NA],
            &[0, 0, 2, 1],
            Some(Model::Int(vec![NA, NA, -4, NA])),
        ),
        (
            MOD,
            &[-7, 7, 5],
            &[3, -3, 0],
            Some(Model::Int(vec![2, -2, NA])),
        ),
        (
            DIV,
            &[1, 1, NA],
            &[2, 0, 1],
            Some(Model::Dbl(vec![
                0.5,
                f64::INFINITY,
                f64::from_bits(NA_REAL_BITS),
            ])),
        ),
        (
            POW,
            &[2, NA, 0],
            &[31, 0, -1],
            Some(Model::Dbl(vec![2147483648.0, 1.0, f64::INFINITY])),
        ),
        (
            MIN2,
            &[3, NA, 1],
            &[2, 1, 5],
            Some(Model::Int(vec![2, NA, 1])),
        ),
        (MAX2, &[3, 1], &[2], Some(Model::Int(vec![3, 2]))),
        (MAX2, &[3, 1], &[2, 2, 2], None),
        (ADD, &[1, 2], &[1, 2, 3], None),
        (ADD, &[], &[1, 2, 3], Some(Model::Int(vec![]))),
    ];
    for (op, a, b, expected) in cases {
        let result = unsafe { ry_arith(*op, INT as u32, int(a), 1, int(b), 1) };
        match expected {
            Some(expected) => {
                assert_eq!(&read(result), expected, "op {op} on {a:?} and {b:?}");
                unsafe { ry_list_drop(result) };
            }
            None => assert!(result.is_null(), "op {op} on {a:?} and {b:?} should fail"),
        }
    }
    assert_eq!(
        live(),
        before,
        "failing kernels must consume their operands"
    );
}

#[test]
fn double_arithmetic_follows_r() {
    let before = live();
    let na = f64::from_bits(NA_REAL_BITS);
    let cases: &[(u32, f64, f64, f64)] = &[
        (MOD, 7.0, -3.0, -2.0),
        (MOD, 5.5, 2.0, 1.5),
        (MOD, 1e17, 7.0, 5.0),
        (IDIV, -7.0, 2.0, -4.0),
        (POW, na, 0.0, 1.0),
        (POW, f64::NAN, 0.0, 1.0),
        (POW, 1.0, na, 1.0),
        (POW, -8.0, 1.0 / 3.0, f64::NAN),
        (DIV, 1.0, 0.0, f64::INFINITY),
    ];
    for &(op, x, y, expected) in cases {
        let a = build(&Model::Dbl(vec![x]));
        let result = unsafe { ry_arith_vs(op, DBL as u32, a, 1, y.to_bits()) };
        let Model::Dbl(values) = read(result) else {
            panic!("not a double list")
        };
        assert!(
            same_double(values[0], expected),
            "op {op} on {x} and {y}: {} != {expected}",
            values[0]
        );
        unsafe { ry_list_drop(result) };
    }
    // GNU R: pmin(c(NaN, 1, NA, -0), c(1, NaN, NaN, 0)) is NaN NaN NaN -0, and pmin(NaN, NA) is NA.
    unsafe {
        let a = build(&Model::Dbl(vec![f64::NAN, 1.0, na, -0.0, f64::NAN]));
        let b = build(&Model::Dbl(vec![1.0, f64::NAN, f64::NAN, 0.0, na]));
        let result = ry_arith(MIN2, DBL as u32, a, 1, b, 1);
        let Model::Dbl(values) = read(result) else {
            panic!("not a double list")
        };
        assert!(values[0].is_nan() && !is_na(values[0]));
        assert!(values[1].is_nan() && !is_na(values[1]));
        assert!(values[2].is_nan() && !is_na(values[2]));
        assert!(values[3] == 0.0 && values[3].is_sign_negative());
        assert!(is_na(values[4]));
        ry_list_drop(result);
    }
    assert_eq!(live(), before);
}

#[test]
fn comparisons_and_logic_follow_r() {
    let before = live();
    unsafe {
        let a = build(&Model::Dbl(vec![1.0, f64::NAN, 3.0]));
        let result = ry_compare_vs(LT, DBL as u32, a, 1, 2f64.to_bits(), 0);
        assert_eq!(read(result), Model::Lgl(vec![1, NA, 0]));
        ry_list_drop(result);
        let a = build(&Model::Lgl(vec![1, 0, NA, NA, 1]));
        let b = build(&Model::Lgl(vec![NA, NA, 0, 1, 1]));
        let both = ry_logic(AND, a, 0, b, 0);
        let either = ry_logic(OR, a, 0, b, 0);
        assert_eq!(read(both), Model::Lgl(vec![NA, 0, 0, NA, 1]));
        assert_eq!(read(either), Model::Lgl(vec![1, NA, NA, 1, 1]));
        let negated = ry_not(a, 1);
        assert_eq!(read(negated), Model::Lgl(vec![0, 1, NA, NA, 0]));
        let strings = build(&Model::Str(vec![
            "apple".into(),
            "a long string past the inline limit".into(),
        ]));
        let key = str_of("a long string past the inline limit");
        let equal = ry_compare_vs(EQ, STR as u32, strings, 0, key.lo, key.hi);
        assert_eq!(read(equal), Model::Lgl(vec![0, 1]));
        // A space orders before a letter, so "apple" is not before "a long ...".
        let less = ry_compare_vs(LT, STR as u32, strings, 0, key.lo, key.hi);
        assert_eq!(read(less), Model::Lgl(vec![0, 0]));
        let at_most = ry_compare_vs(LE, STR as u32, strings, 0, key.lo, key.hi);
        assert_eq!(read(at_most), Model::Lgl(vec![0, 1]));
        for list in [both, either, negated, equal, less, at_most, strings, b] {
            ry_list_drop(list);
        }
        ry_str_drop(key.lo, key.hi);
    }
    assert_eq!(live(), before);
}

/// Fused arithmetic against the same steps run one item at a time: random programs of `+`, `-`,
/// `*` and `/` over `F64` or `I64` lists and scalars, as a list and as a sum, over lists that
/// cross chunk boundaries. A `NaN` result, an `I64` overflow, a length mismatch and an empty
/// list result give up.
#[test]
fn fused_arithmetic_agrees_with_one_item_at_a_time() {
    let mut rng = Rng::from_env();
    let before = live();
    for _ in 0..iterations(400) {
        let doubles = rng.below(2) == 0;
        let sum = rng.below(2) == 0;
        let length = [0, 1, 7, 127, 128, 129, 300][rng.below(7)];
        // An item as its word: a double's bits or an `I64`. Doubles are halves, so a sum of
        // products is exact and does not depend on the order or width of the additions.
        let item = |rng: &mut Rng| -> u64 {
            match rng.below(60) {
                0 if doubles => f64::NAN.to_bits(),
                0 => (i64::MAX - 1) as u64,
                _ if doubles => (rng.below(11) as f64 / 2.0 - 2.5).to_bits(),
                _ => (rng.below(7) as i64 - 3) as u64,
            }
        };
        // Each input is a list of its items, or one item for a scalar.
        let mut inputs: Vec<Vec<u64>> = Vec::new();
        let mut lists = 0u32;
        for index in 0..1 + rng.below(4) {
            if index == 0 || rng.below(3) != 0 {
                // The first input is a list of the usual length, so a longer one is a mismatch.
                let size = if index > 0 && rng.below(30) == 0 {
                    length + 1
                } else {
                    length
                };
                inputs.push((0..size).map(|_| item(&mut rng)).collect());
                lists |= 1 << index;
            } else {
                inputs.push(vec![item(&mut rng)]);
            }
        }
        let fits =
            (0..inputs.len()).all(|index| lists & 1 << index == 0 || inputs[index].len() == length);
        let mut program: Vec<u32> = Vec::new();
        // Each step's items, or `None` once a step fails.
        let mut done: Option<Vec<Vec<u64>>> = fits.then(Vec::new);
        for step in 0..1 + rng.below(5) {
            let reach = inputs.len() + step;
            let (left, right) = (rng.below(reach), rng.below(reach));
            // A sum of quotients would depend on how wide the additions are.
            let op = rng.below(if doubles && !sum { 4 } else { 3 }) as u32;
            program.push(op | (left as u32) << 8 | (right as u32) << 16);
            let Some(steps) = &mut done else { continue };
            let read = |operand: usize, at: usize| match inputs.get(operand) {
                Some(items) if lists & 1 << operand != 0 => items[at],
                Some(items) => items[0],
                None => steps[operand - inputs.len()][at],
            };
            let items: Option<Vec<u64>> = (0..length)
                .map(|at| {
                    let (x, y) = (read(left, at), read(right, at));
                    if doubles {
                        let (x, y) = (f64::from_bits(x), f64::from_bits(y));
                        let value = match op {
                            ADD => x + y,
                            SUB => x - y,
                            MUL => x * y,
                            _ => x / y,
                        };
                        (!value.is_nan()).then_some(value.to_bits())
                    } else {
                        let (x, y) = (x as i64, y as i64);
                        match op {
                            ADD => x.checked_add(y),
                            SUB => x.checked_sub(y),
                            _ => x.checked_mul(y),
                        }
                        .map(|value| value as u64)
                    }
                })
                .collect();
            match items {
                Some(items) => steps.push(items),
                None => done = None,
            }
        }
        let expected: Option<Vec<u64>> = match done.as_ref().and_then(|steps| steps.last()) {
            Some(items) if sum && doubles => {
                let total = items
                    .iter()
                    .fold(0.0, |total, &item| total + f64::from_bits(item));
                Some(vec![total.to_bits()])
            }
            Some(items) if sum => items
                .iter()
                .try_fold(0i64, |total, &item| total.checked_add(item as i64))
                .map(|total| vec![total as u64]),
            Some(items) if length > 0 => Some(items.clone()),
            _ => None,
        };
        unsafe {
            let kind = if doubles { DBL } else { I64 };
            let words: Vec<u64> = inputs
                .iter()
                .enumerate()
                .map(|(index, items)| {
                    if lists & 1 << index == 0 {
                        return items[0];
                    }
                    items
                        .iter()
                        .fold(ry_list_new(kind as u32, 0), |list, &item| {
                            ry_list_push(list, 1, item, 0)
                        }) as u64
                })
                .collect();
            let mut out = 0u64;
            let status = ry_elementwise(
                kind as u32,
                program.as_ptr(),
                program.len() as u32,
                words.as_ptr(),
                words.len() as u32,
                lists,
                sum as u32,
                &mut out,
            );
            let found: Option<Vec<u64>> = (status == 0).then(|| {
                if sum {
                    return vec![out];
                }
                let list = out as *mut List;
                let length = (*list).length as usize;
                let items = std::slice::from_raw_parts((*list).data as *const u64, length).to_vec();
                ry_list_drop(list);
                items
            });
            assert_eq!(
                found.as_ref().map(|items| &items[..items.len().min(8)]),
                expected.as_ref().map(|items| &items[..items.len().min(8)]),
                "the first items of program {program:?} over {} inputs of length {length}, \
                 lists {lists:#b}, sum {sum}, doubles {doubles}",
                inputs.len()
            );
            assert!(found == expected, "program {program:?}, length {length}");
            for (index, &word) in words.iter().enumerate() {
                if lists & 1 << index != 0 {
                    ry_list_drop(word as *mut List);
                }
            }
        }
    }
    assert_eq!(live(), before);
}

/// A fused count against R's three-valued logic item by item: random comparisons of `I64` and
/// double lists and scalars, combined with `&`, `|` and `!`, over lists that cross chunk
/// boundaries, with `NA`, `NaN` and a length mismatch, which give up.
#[test]
fn fused_counts_follow_three_valued_logic() {
    #[derive(Clone)]
    enum Input {
        Wholes(Vec<i64>),
        Doubles(Vec<f64>),
        Logicals(Vec<i32>),
        Whole(i64),
        Double(f64),
        Logical(i32),
    }
    let mut rng = Rng::from_env();
    let before = live();
    for _ in 0..iterations(300) {
        let length = [0, 1, 7, 128, 129, 300][rng.below(6)];
        let mut inputs: Vec<Input> = Vec::new();
        for _ in 0..2 + rng.below(5) {
            let size = if rng.below(30) == 0 {
                length + 1
            } else {
                length
            };
            inputs.push(match rng.below(6) {
                0 => Input::Wholes((0..size).map(|_| rng.below(5) as i64 - 2).collect()),
                1 => Input::Doubles(
                    (0..size)
                        .map(|_| match rng.below(40) {
                            0 => f64::NAN,
                            _ => rng.below(5) as f64 - 2.0,
                        })
                        .collect(),
                ),
                2 => Input::Logicals(
                    (0..size)
                        .map(|_| match rng.below(40) {
                            0 => NA,
                            _ => rng.below(2) as i32,
                        })
                        .collect(),
                ),
                3 => Input::Whole(rng.below(5) as i64 - 2),
                4 => Input::Double(rng.below(5) as f64 - 2.0),
                _ => Input::Logical([0, 1, 1, NA][rng.below(4)]),
            });
        }
        let numbered = |want: fn(&Input) -> bool, rng: &mut Rng| {
            let found: Vec<usize> = (0..inputs.len()).filter(|&i| want(&inputs[i])).collect();
            (!found.is_empty()).then(|| found[rng.below(found.len())])
        };
        // Each step's lanes as `Option<bool>`, `None` for `NA`.
        let mut program: Vec<u32> = Vec::new();
        let mut lanes: Vec<Vec<Option<bool>>> = Vec::new();
        let logical_lanes = |index: usize, lanes: &[Vec<Option<bool>>]| -> Vec<Option<bool>> {
            if index >= inputs.len() {
                return lanes[index - inputs.len()].clone();
            }
            let lane = |item: i32| (item != NA).then_some(item != 0);
            match &inputs[index] {
                Input::Logicals(items) => {
                    items.iter().take(length).map(|&item| lane(item)).collect()
                }
                Input::Logical(item) => vec![lane(*item); length],
                _ => unreachable!(),
            }
        };
        for _ in 0..1 + rng.below(6) {
            let logical_operand = |rng: &mut Rng, steps: usize| {
                let input = numbered(
                    |input| matches!(input, Input::Logicals(_) | Input::Logical(_)),
                    rng,
                );
                match (input, steps) {
                    (Some(input), 0) => Some(input),
                    (Some(input), _) if rng.below(3) == 0 => Some(input),
                    (_, 0) => None,
                    _ => Some(inputs.len() + rng.below(steps)),
                }
            };
            let choice = rng.below(4);
            let op = rng.below(6) as u32;
            // A program may open with a logical step over logical inputs alone.
            let logical_inputs = inputs
                .iter()
                .any(|input| matches!(input, Input::Logicals(_) | Input::Logical(_)));
            if choice == 0 || (lanes.is_empty() && !logical_inputs) {
                let wholes = rng.below(2) == 0;
                let want: fn(&Input) -> bool = if wholes {
                    |input| matches!(input, Input::Wholes(_) | Input::Whole(_))
                } else {
                    |input| matches!(input, Input::Doubles(_) | Input::Double(_))
                };
                let (Some(left), Some(right)) =
                    (numbered(want, &mut rng), numbered(want, &mut rng))
                else {
                    continue;
                };
                let value = |index: usize, at: usize| match &inputs[index] {
                    Input::Wholes(items) => items[at] as f64,
                    Input::Whole(item) => *item as f64,
                    Input::Doubles(items) => items[at],
                    Input::Double(item) => *item,
                    _ => unreachable!(),
                };
                let lists_fit = [left, right].iter().all(|&index| match &inputs[index] {
                    Input::Wholes(items) => items.len() >= length,
                    Input::Doubles(items) => items.len() >= length,
                    _ => true,
                });
                if !lists_fit {
                    continue;
                }
                let result = (0..length)
                    .map(|at| {
                        let (x, y) = (value(left, at), value(right, at));
                        (!x.is_nan() && !y.is_nan()).then_some(match op {
                            EQ => x == y,
                            NE => x != y,
                            LT => x < y,
                            LE => x <= y,
                            GT => x > y,
                            _ => x >= y,
                        })
                    })
                    .collect();
                let base = if wholes { MASK_I64 } else { MASK_DBL };
                program.push((base + op) | (left as u32) << 8 | (right as u32) << 16);
                lanes.push(result);
                continue;
            }
            let (Some(left), Some(right)) = (
                logical_operand(&mut rng, lanes.len()),
                logical_operand(&mut rng, lanes.len()),
            ) else {
                continue;
            };
            let fits = |index: usize| match inputs.get(index) {
                Some(Input::Logicals(items)) => items.len() >= length,
                _ => true,
            };
            if !fits(left) || !fits(right) {
                continue;
            }
            let (x, y) = (logical_lanes(left, &lanes), logical_lanes(right, &lanes));
            let (code, result) = match choice {
                1 => (
                    MASK_AND,
                    x.iter()
                        .zip(&y)
                        .map(|(&a, &b)| match (a, b) {
                            (Some(false), _) | (_, Some(false)) => Some(false),
                            (Some(true), Some(true)) => Some(true),
                            _ => None,
                        })
                        .collect(),
                ),
                2 => (
                    MASK_OR,
                    x.iter()
                        .zip(&y)
                        .map(|(&a, &b)| match (a, b) {
                            (Some(true), _) | (_, Some(true)) => Some(true),
                            (Some(false), Some(false)) => Some(false),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => (MASK_NOT, x.iter().map(|&a| a.map(|a| !a)).collect()),
            };
            program.push(code | (left as u32) << 8 | (right as u32) << 16);
            lanes.push(result);
        }
        if program.is_empty() {
            continue;
        }
        // The runtime rejects an input no step reads, as the pass never builds one, so keep only
        // the inputs the program reads and renumber its operands.
        let operands = |step: u32| {
            let mut found = vec![(step >> 8 & 0xFF) as usize];
            if step & 0xFF != MASK_NOT {
                found.push((step >> 16 & 0xFF) as usize);
            }
            found
        };
        let kept: Vec<usize> = (0..inputs.len())
            .filter(|&index| program.iter().any(|&step| operands(step).contains(&index)))
            .collect();
        let renumber = |operand: usize| match kept.iter().position(|&index| index == operand) {
            Some(position) => position as u32,
            None => (kept.len() + operand - inputs.len()) as u32,
        };
        let program: Vec<u32> = program
            .iter()
            .map(|&step| {
                let found = operands(step);
                let right = found.get(1).map_or(0, |&operand| renumber(operand));
                step & 0xFF | renumber(found[0]) << 8 | right << 16
            })
            .collect();
        let inputs: Vec<Input> = kept.iter().map(|&index| inputs[index].clone()).collect();
        // Lists that all have the odd length agree with each other, and the model's lanes have
        // the usual length, so the case says nothing.
        let odd = inputs.iter().all(|input| match input {
            Input::Wholes(items) => items.len() != length,
            Input::Doubles(items) => items.len() != length,
            Input::Logicals(items) => items.len() != length,
            _ => true,
        });
        if odd {
            continue;
        }
        unsafe {
            let words: Vec<u64> = inputs
                .iter()
                .map(|input| match input {
                    Input::Wholes(items) => items
                        .iter()
                        .fold(ry_list_new(I64 as u32, 0), |list, &item| {
                            ry_list_push(list, 1, item as u64, 0)
                        }) as u64,
                    Input::Doubles(items) => build(&Model::Dbl(items.clone())) as u64,
                    Input::Logicals(items) => build(&Model::Lgl(items.clone())) as u64,
                    Input::Whole(item) => *item as u64,
                    Input::Double(item) => item.to_bits(),
                    Input::Logical(item) => *item as u32 as u64,
                })
                .collect();
            let lists = inputs
                .iter()
                .enumerate()
                .fold(0u32, |lists, (index, input)| match input {
                    Input::Wholes(_) | Input::Doubles(_) | Input::Logicals(_) => lists | 1 << index,
                    _ => lists,
                });
            let lengths: Vec<usize> = inputs
                .iter()
                .filter_map(|input| match input {
                    Input::Wholes(items) => Some(items.len()),
                    Input::Doubles(items) => Some(items.len()),
                    Input::Logicals(items) => Some(items.len()),
                    _ => None,
                })
                .collect();
            let last = &lanes[lanes.len() - 1];
            let expected = if lengths.is_empty()
                || lengths.iter().any(|&found| found != length)
                || last.contains(&None)
            {
                None
            } else {
                Some(last.iter().filter(|&&lane| lane == Some(true)).count() as i64)
            };
            let mut out = -1i64;
            let status = ry_count_true(
                program.as_ptr(),
                program.len() as u32,
                words.as_ptr(),
                words.len() as u32,
                lists,
                &mut out,
            );
            let found = (status == 0).then_some(out);
            assert_eq!(found, expected, "program {program:x?}, seed {}", rng.seed);
            for (index, &word) in words.iter().enumerate() {
                if lists & (1 << index) != 0 {
                    ry_list_drop(word as *mut List);
                }
            }
        }
    }
    assert_eq!(live(), before);
}

#[test]
fn math_and_round_follow_r() {
    let before = live();
    let na = f64::from_bits(NA_REAL_BITS);
    unsafe {
        let a = build(&Model::Dbl(vec![0.5, 1.5, 2.5, -0.5, na]));
        let rounded = ry_math(ROUND0, a, 0);
        assert_eq!(read(rounded), Model::Dbl(vec![0.0, 2.0, 2.0, -0.0, na]));
        let to_one = ry_round(a, 1, 1);
        let Model::Dbl(values) = read(to_one) else {
            panic!("not a double list")
        };
        assert_eq!(&values[..4], &[0.5, 1.5, 2.5, -0.5]);
        let negative = build(&Model::Dbl(vec![4.0, -1.0]));
        assert!(ry_math(SQRT, negative, 1).is_null(), "sqrt(-1) warns in R");
        let missing = build(&Model::Dbl(vec![na, f64::NAN, 4.0]));
        let roots = ry_math(SQRT, missing, 1);
        let Model::Dbl(values) = read(roots) else {
            panic!("not a double list")
        };
        assert!(is_na(values[0]) && values[1].is_nan() && values[2] == 2.0);
        let ints = build(&Model::Int(vec![-3, NA, 4]));
        let absolute = ry_math(ABS, ints, 1);
        assert_eq!(read(absolute), Model::Int(vec![3, NA, 4]));
        let signs = build(&Model::Dbl(vec![-2.0, 0.0, 3.0]));
        let signs = ry_math(SIGN, signs, 1);
        assert_eq!(read(signs), Model::Dbl(vec![-1.0, 0.0, 1.0]));
        let rounded_digits = build(&Model::Dbl(vec![0.15, 2.675, 99.21 + 1e-14]));
        let rounded_digits = ry_round(rounded_digits, 1, 2);
        let Model::Dbl(values) = read(rounded_digits) else {
            panic!("not a double list")
        };
        assert_eq!(values, vec![0.15, 2.67, 99.21]);
        for list in [rounded, to_one, roots, absolute, signs, rounded_digits] {
            ry_list_drop(list);
        }
    }
    assert_eq!(live(), before);
}

#[test]
fn reductions_follow_r() {
    let before = live();
    let na = f64::from_bits(NA_REAL_BITS);
    unsafe {
        let big = build(&Model::Dbl(vec![1e308, 1e308, -1e308]));
        assert_eq!(
            ry_sum_dbl(big),
            1e308,
            "sum accumulates in extended precision"
        );
        let pair = build(&Model::Dbl(vec![1e308, 1e308]));
        assert_eq!(ry_mean_dbl(pair), 1e308);
        let tenths = build(&Model::Dbl(vec![0.1; 10]));
        assert_eq!(ry_sum_dbl(tenths), 1.0);
        let with_na = build(&Model::Dbl(vec![1.0, f64::NAN, na]));
        assert!(ry_sum_dbl(with_na).is_nan() && !is_na(ry_sum_dbl(with_na)));
        let mut out = 0.0;
        assert_eq!(ry_max_dbl(with_na, &mut out), 0);
        assert!(is_na(out));
        let empty = ry_list_new(DBL as u32, 0);
        assert_eq!(ry_max_dbl(empty, &mut out), 2);
        assert!(ry_mean_dbl(empty).is_nan());
        assert_eq!(ry_prod_dbl(empty), 1.0);
        let ints = build(&Model::Int(vec![i32::MAX, 1]));
        let mut total = 0;
        assert_eq!(ry_sum_int(ints, &mut total), 2);
        let ints_na = build(&Model::Int(vec![1, NA]));
        assert_eq!(ry_sum_int(ints_na, &mut total), 1);
        assert_eq!(total, NA);
        let small = ry_list_seq(5, -2);
        assert_eq!(read(small), Model::Int(vec![5, 4, 3, 2, 1, 0, -1, -2]));
        assert_eq!(ry_sum_int(small, &mut total), 0);
        assert_eq!(total, 12);
        assert_eq!(ry_min_int(small, &mut total), 0);
        assert_eq!(total, -2);
        assert_eq!(ry_mean_dbl(small), 1.5);
        let logicals = build(&Model::Lgl(vec![0, NA]));
        let mut flag = 0;
        ry_any(logicals, &mut flag);
        assert_eq!(flag, NA);
        ry_all(logicals, &mut flag);
        assert_eq!(flag, 0);
        let a = build(&Model::Dbl(vec![0.0, f64::NAN]));
        let b = build(&Model::Dbl(vec![-0.0, f64::NAN]));
        assert_eq!(ry_list_equal(a, b), 1);
        assert_eq!(ry_list_equal(a, with_na), 0);
        for list in [
            big, pair, tenths, with_na, empty, ints, ints_na, small, logicals, a, b,
        ] {
            ry_list_drop(list);
        }
    }
    assert_eq!(live(), before);
}

#[test]
fn formatting_matches_r() {
    // Each expected text is GNU R 4.3.3's output for the expression in the comment.
    let doubles: &[(f64, &str)] = &[
        (0.1, "0.1"),                                   // as.character(0.1)
        (1.0 / 3.0, "0.333333333333333"),               // as.character(1/3)
        (1e6, "1e+06"),                                 // as.character(1e6)
        (100000.0, "1e+05"),                            // as.character(100000)
        (123456.0, "123456"),                           // as.character(123456)
        (123456.5, "123456.5"),                         // as.character(123456.5)
        (1e-20, "1e-20"),                               // as.character(1e-20)
        (1e15, "1e+15"),                                // as.character(1e15)
        (1e15 + 2.0, "1e+15"),                          // as.character(1e15 + 2)
        (123456789012.0, "123456789012"),               // as.character(123456789012)
        (0.1 + 0.2, "0.3"),                             // as.character(0.1 + 0.2)
        (100000.1, "100000.1"),                         // as.character(1e5 + 0.1)
        (-1.5, "-1.5"),                                 // as.character(-1.5)
        (2147483648.0, "2147483648"),                   // as.character(2^31)
        (1e-4, "1e-04"),                                // as.character(0.0001)
        (1.0 / 7.0, "0.142857142857143"),               // as.character(1/7)
        (5e-324, "4.94065645841247e-324"),              // as.character(5e-324)
        (-0.0, "0"),                                    // as.character(-0)
        (f64::NAN, "NaN"),                              // as.character(NaN)
        (f64::INFINITY, "Inf"),                         // as.character(Inf)
        (f64::NEG_INFINITY, "-Inf"),                    // as.character(-Inf)
        (1234567890123456789.0, "1234567890123456768"), // as.character(1234567890123456789)
    ];
    for &(value, expected) in doubles {
        assert_eq!(
            text_of(unsafe { ry_format_dbl(value) }),
            expected,
            "as.character({value:e})"
        );
    }
    assert_eq!(
        text_of(unsafe { ry_format_dbl(f64::from_bits(NA_REAL_BITS)) }),
        "NA"
    );
    let fixed: &[(f64, i32, &str)] = &[
        (1.005, 2, "1.00"),          // sprintf("%.2f", 1.005)
        (2.675, 2, "2.67"),          // sprintf("%.2f", 2.675)
        (-0.001, 2, "-0.00"),        // sprintf("%.2f", -0.001)
        (0.125, 2, "0.12"),          // sprintf("%.2f", 0.125)
        (1e10, 2, "10000000000.00"), // sprintf("%.2f", 1e10)
        (2.5, 0, "2"),               // sprintf("%.0f", 2.5)
        (-0.0, 3, "-0.000"),         // sprintf("%.3f", -0)
    ];
    for &(value, decimals, expected) in fixed {
        assert_eq!(
            text_of(unsafe { ry_format_fixed(value, decimals) }),
            expected
        );
    }
    let padded: &[(i32, i32, u32, &str)] = &[
        (42, 6, 1, "000042"),       // formatC(42L, width = 6L, flag = "0")
        (-42, 6, 1, "-00042"),      // formatC(-42L, width = 6L, flag = "0")
        (1234567, 6, 1, "1234567"), // formatC(1234567L, width = 6L, flag = "0")
        (42, 6, 0, "    42"),       // formatC(42L, width = 6L)
        (-42, 6, 0, "   -42"),      // formatC(-42L, width = 6L)
        (5, -4, 1, "5   "),         // formatC(5L, width = -4L, flag = "0")
        (NA, 6, 1, "    NA"),       // formatC(NA_integer_, width = 6L, flag = "0")
    ];
    for &(value, width, zero, expected) in padded {
        assert_eq!(
            text_of(unsafe { ry_format_padded_int(value, width, zero) }),
            expected
        );
    }
    assert_eq!(
        text_of(unsafe { ry_format_int(i32::MIN + 1) }),
        "-2147483647"
    );
    assert_eq!(text_of(unsafe { ry_format_int(0) }), "0");
}

#[test]
fn coercion_follows_r() {
    let before = live();
    unsafe {
        let logicals = build(&Model::Lgl(vec![1, 0, NA]));
        let as_double = ry_list_coerce(logicals, 0, DBL as u32);
        assert_eq!(
            read(as_double),
            Model::Dbl(vec![1.0, 0.0, f64::from_bits(NA_REAL_BITS)])
        );
        let as_integer = ry_list_coerce(logicals, 1, INT as u32);
        assert_eq!(
            as_integer, logicals,
            "a unique logical list becomes integer in place"
        );
        assert_eq!(read(as_integer), Model::Int(vec![1, 0, NA]));
        assert!(
            ry_list_coerce(as_integer, 1, STR as u32).is_null(),
            "as.character(NA) has no Str"
        );
        let doubles = build(&Model::Dbl(vec![0.1, 1e5, 123456.0]));
        let strings = ry_list_coerce(doubles, 1, STR as u32);
        assert_eq!(
            read(strings),
            Model::Str(vec!["0.1".into(), "1e+05".into(), "123456".into()])
        );
        let ints = ry_list_seq(-1, 1);
        let int_strings = ry_list_coerce(ints, 1, STR as u32);
        assert_eq!(
            read(int_strings),
            Model::Str(vec!["-1".into(), "0".into(), "1".into()])
        );
        for list in [as_double, strings, int_strings] {
            ry_list_drop(list);
        }
    }
    assert_eq!(live(), before);
}

#[test]
fn string_functions_follow_r_and_the_standard_library() {
    let before = live();
    unsafe {
        let parts = [
            str_of("a"),
            str_of("a part long enough for the heap"),
            str_of("é"),
        ];
        let separator = str_of("-");
        let joined = ry_str_concat(parts.as_ptr(), 3, separator.lo, separator.hi);
        assert_eq!(text_of(joined), "a-a part long enough for the heap-é");
        let pieces = ry_str_split(joined.lo, joined.hi, separator.lo, separator.hi);
        assert_eq!(
            read(pieces),
            Model::Str(vec![
                "a".into(),
                "a part long enough for the heap".into(),
                "é".into()
            ])
        );
        let empty = str_of("");
        let whole = ry_str_split(joined.lo, joined.hi, empty.lo, empty.hi);
        assert_eq!(
            read(whole),
            Model::Str(vec!["a-a part long enough for the heap-é".into()])
        );
        let edges = str_of("-a--");
        let edge_pieces = ry_str_split(edges.lo, edges.hi, separator.lo, separator.hi);
        assert_eq!(
            read(edge_pieces),
            Model::Str(vec!["".into(), "a".into(), "".into(), "".into()])
        );
        let nothing = ry_str_split(empty.lo, empty.hi, separator.lo, separator.hi);
        assert_eq!(read(nothing), Model::Str(vec!["".into()]));
        let wide = str_of("--");
        let wide_pieces = ry_str_split(edges.lo, edges.hi, wide.lo, wide.hi);
        assert_eq!(read(wide_pieces), Model::Str(vec!["-a".into(), "".into()]));
        let accented = str_of("é,a part long enough for the heap,");
        let comma = str_of(",");
        let accented_pieces = ry_str_split(accented.lo, accented.hi, comma.lo, comma.hi);
        assert_eq!(
            read(accented_pieces),
            Model::Str(vec![
                "é".into(),
                "a part long enough for the heap".into(),
                "".into()
            ])
        );
        ry_str_drop(accented.lo, accented.hi);
        // Separators at every offset of an eight-byte word, alone and in runs, in texts whose
        // length covers the word-at-a-time part and the tail.
        for length in 0..40usize {
            for stride in [1, 2, 3, 5, 7, 8, 9, 41] {
                let text: String = (0..length)
                    .map(|index| {
                        if index % stride == stride - 1 {
                            ','
                        } else {
                            'x'
                        }
                    })
                    .collect();
                let subject = str_of(&text);
                let split = ry_str_split(subject.lo, subject.hi, comma.lo, comma.hi);
                let expected = text.split(',').map(String::from).collect();
                assert_eq!(read(split), Model::Str(expected), "splitting {text:?}");
                ry_list_drop(split);
                ry_str_drop(subject.lo, subject.hi);
            }
        }
        let back = ry_list_join(pieces, separator.lo, separator.hi);
        assert_eq!(text_of(back), text_of(joined));

        let upper = ry_str_toupper(joined.lo, joined.hi, 0);
        assert_eq!(text_of(upper), "A-A PART LONG ENOUGH FOR THE HEAP-É");
        let reused = ry_str_tolower(upper.lo, upper.hi, 1);
        assert_eq!(
            reused.lo, upper.lo,
            "a unique buffer is case-mapped in place"
        );
        assert_eq!(text_of(reused), "a-a part long enough for the heap-é");
        let sharp = str_of("straße");
        let sharp_upper = ry_str_toupper(sharp.lo, sharp.hi, 0);
        assert_eq!(
            text_of(sharp_upper),
            "STRAßE",
            "a mapping that expands keeps the character"
        );

        let word = str_of("héllo");
        assert_eq!(text_of(ry_str_substr(word.lo, word.hi, 2, 3)), "él");
        assert_eq!(text_of(ry_str_substr(word.lo, word.hi, 0, 10)), "héllo");
        assert_eq!(text_of(ry_str_substr(word.lo, word.hi, 3, 2)), "");
        assert_eq!(text_of(ry_str_substr(word.lo, word.hi, -1, 2)), "hé");
        let prefix = str_of("hé");
        assert_eq!(
            ry_str_starts_with(word.lo, word.hi, prefix.lo, prefix.hi),
            1
        );
        assert_eq!(ry_str_ends_with(word.lo, word.hi, prefix.lo, prefix.hi), 0);
        let a = str_of("abc");
        let b = str_of("abd");
        assert_eq!(ry_str_compare(a.lo, a.hi, b.lo, b.hi), -1);
        assert_eq!(ry_str_compare(b.lo, b.hi, a.lo, a.hi), 1);

        let mut integer = 0;
        for (text, expected) in [
            ("42", Some(42)),
            ("+7", Some(7)),
            ("-2147483647", Some(-2147483647)),
        ] {
            let string = str_of(text);
            assert_eq!(
                ry_str_to_int(string.lo, string.hi, &mut integer),
                0,
                "{text}"
            );
            assert_eq!(Some(integer), expected);
        }
        for text in [" 42", "4.0", "", "-2147483648", "2147483648", "0x10"] {
            let string = str_of(text);
            assert_eq!(
                ry_str_to_int(string.lo, string.hi, &mut integer),
                1,
                "{text:?}"
            );
        }
        let mut double = 0.0;
        for (text, expected) in [("1.5", 1.5), ("1e3", 1000.0), ("-inf", f64::NEG_INFINITY)] {
            let string = str_of(text);
            assert_eq!(
                ry_str_to_dbl(string.lo, string.hi, &mut double),
                0,
                "{text}"
            );
            assert_eq!(double, expected);
        }
        for text in ["NaN", "1.5x", " 1"] {
            let string = str_of(text);
            assert_eq!(
                ry_str_to_dbl(string.lo, string.hi, &mut double),
                1,
                "{text:?}"
            );
        }

        // paste(c("a", "b"), "x", character(0), sep = "-") is "a-x-" "b-x-" in R.
        let left = build(&Model::Str(vec!["a".into(), "b".into()]));
        let middle = build(&Model::Str(vec!["x".into()]));
        let none = ry_list_new(STR as u32, 0);
        let lists = [left, middle, none];
        let pasted = ry_list_paste(lists.as_ptr(), 3, separator.lo, separator.hi);
        assert_eq!(read(pasted), Model::Str(vec!["a-x-".into(), "b-x-".into()]));
        let counts = ry_list_nchar(pieces);
        assert_eq!(read(counts), Model::Int(vec![1, 31, 1]));
        let starts = ry_list_starts_with(pieces, a.lo, a.hi);
        assert_eq!(read(starts), Model::Lgl(vec![0, 0, 0]));
        let shouted = ry_list_toupper(pieces, 0);
        assert_eq!(
            read(shouted),
            Model::Str(vec![
                "A".into(),
                "A PART LONG ENOUGH FOR THE HEAP".into(),
                "É".into()
            ])
        );
        let item = ry_list_get_str(shouted, 1);
        assert_eq!(text_of(item), "A PART LONG ENOUGH FOR THE HEAP");

        for list in [
            pieces,
            whole,
            edge_pieces,
            nothing,
            wide_pieces,
            accented_pieces,
            left,
            middle,
            none,
            pasted,
            counts,
            starts,
            shouted,
        ] {
            ry_list_drop(list);
        }
        for string in
            parts
                .into_iter()
                .chain([joined, back, reused, sharp, sharp_upper, word, prefix, item])
        {
            ry_str_drop(string.lo, string.hi);
        }
    }
    assert_eq!(live(), before);
}

#[test]
fn views_borrow_foreign_items() {
    let before = live();
    let mut foreign = vec![1.0f64, 2.0, 3.0];
    unsafe {
        let view = ry_list_view(DBL as u32, foreign.as_mut_ptr() as *mut u8, 3);
        assert_eq!(live(), before, "a view is not counted");
        let doubled = ry_arith_vs(MUL, DBL as u32, view, 1, 2f64.to_bits());
        assert_ne!(doubled, view, "a view is never written");
        assert_eq!(read(doubled), Model::Dbl(vec![2.0, 4.0, 6.0]));
        let copy = ry_list_unshare(view, 0);
        assert_ne!(copy, view);
        ry_list_drop(view);
        ry_view_release(view);
        ry_list_drop(doubled);
        ry_list_drop(copy);
    }
    assert_eq!(foreign, vec![1.0, 2.0, 3.0]);
    assert_eq!(live(), before);
}

#[test]
fn dicts_follow_their_models() {
    let mut rng = Rng::from_env();
    let iterations = iterations(60);
    let before = live();
    for iteration in 0..iterations {
        set_hash_seed(rng.next());
        let string_keys = iteration % 2 == 0;
        let key_kind = if string_keys { STR } else { INT };
        let mut dict = unsafe { ry_dict_new(key_kind as u32, STR as u32) };
        let mut model: HashMap<String, String> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut snapshots: Vec<(*mut u8, Vec<(String, String)>)> = Vec::new();
        let key_space = 1 + rng.below(60);
        for step in 0..300 {
            let key_number = rng.below(key_space);
            let key_text = if string_keys {
                if key_number % 3 == 0 {
                    format!("a key long enough to live on the heap {key_number}")
                } else {
                    format!("k{key_number}")
                }
            } else {
                key_number.to_string()
            };
            let key = if string_keys {
                str_of(&key_text)
            } else {
                Str {
                    lo: key_number as u64,
                    hi: 0,
                }
            };
            match rng.below(10) {
                0..=5 => {
                    let value_text = if rng.below(2) == 0 {
                        format!("v{step}")
                    } else {
                        format!("a value long enough to need a heap buffer {step}")
                    };
                    let value = str_of(&value_text);
                    let owned = rng.below(4) != 0;
                    if !owned {
                        snapshots.push((dict, entries(&model, &order)));
                    }
                    dict = unsafe {
                        if rng.below(2) == 0 {
                            ry_dict_insert(dict, owned as u32, key.lo, key.hi, value.lo, value.hi)
                        } else {
                            let position = ry_dict_find(dict, key.lo, key.hi);
                            ry_dict_put(
                                dict,
                                owned as u32,
                                position,
                                key.lo,
                                key.hi,
                                value.lo,
                                value.hi,
                            )
                        }
                    };
                    if model.insert(key_text.clone(), value_text).is_none() {
                        order.push(key_text.clone());
                    }
                }
                6..=8 => {
                    let owned = rng.below(4) != 0;
                    if !owned {
                        snapshots.push((dict, entries(&model, &order)));
                    }
                    dict = unsafe { ry_dict_remove(dict, owned as u32, key.lo, key.hi) };
                    if model.remove(&key_text).is_some() {
                        order.retain(|existing| existing != &key_text);
                    }
                }
                _ => {
                    let found = unsafe { ry_dict_find(dict, key.lo, key.hi) } >= 0;
                    assert_eq!(found, model.contains_key(&key_text), "find {key_text}");
                    assert_eq!(
                        unsafe { ry_dict_contains(dict, key.lo, key.hi) } == 1,
                        found
                    );
                }
            }
            if string_keys {
                unsafe { ry_str_drop(key.lo, key.hi) };
            }
            assert_eq!(
                read_dict(dict, string_keys),
                entries(&model, &order),
                "after step {step}"
            );
        }
        for (snapshot, expected) in snapshots {
            assert_eq!(
                read_dict(snapshot, string_keys),
                expected,
                "a shared dict changed"
            );
            unsafe { ry_list_drop(snapshot as *mut List) };
        }
        unsafe { ry_list_drop(dict as *mut List) };
        assert_eq!(live(), before, "dicts leaked");
    }
}

#[test]
fn dict_order_survives_removal_reinsertion_and_compaction() {
    let before = live();
    set_hash_seed(7);
    unsafe {
        let mut dict = ry_dict_new(DBL as u32, INT as u32);
        for key in 0..100i32 {
            dict = ry_dict_insert(dict, 1, f64::from(key).to_bits(), 0, key as u64, 0);
        }
        dict = ry_dict_insert(dict, 1, 5f64.to_bits(), 0, 500, 0);
        for key in 0..60i32 {
            dict = ry_dict_remove(dict, 1, f64::from(key).to_bits(), 0);
        }
        dict = ry_dict_insert(dict, 1, 70f64.to_bits(), 0, 7000, 0);
        dict = ry_dict_insert(dict, 1, 3f64.to_bits(), 0, 3, 0);
        dict = ry_dict_insert(dict, 1, (-0.0f64).to_bits(), 0, 1, 0);
        let keys = ry_dict_keys(dict);
        let values = ry_dict_values(dict);
        let mut expected_keys: Vec<f64> = (60..100).map(f64::from).collect();
        expected_keys.extend([3.0, -0.0]);
        let mut expected_values: Vec<i32> = (60..100).collect();
        expected_values[10] = 7000;
        expected_values.extend([3, 1]);
        assert_eq!(read(keys), Model::Dbl(expected_keys));
        assert_eq!(read(values), Model::Int(expected_values));
        assert_eq!(ry_dict_len(dict), 42);
        assert_eq!(
            ry_dict_contains(dict, 0f64.to_bits(), 0),
            1,
            "-0 and 0 are one key"
        );
        ry_list_drop(keys);
        ry_list_drop(values);
        ry_list_drop(dict as *mut List);
    }
    assert_eq!(live(), before);
}

// Models and helpers.

#[derive(Clone, Debug)]
enum Model {
    Int(Vec<i32>),
    Dbl(Vec<f64>),
    Lgl(Vec<i32>),
    Str(Vec<String>),
}

#[derive(Clone, Debug)]
enum Item {
    Int(i32),
    Dbl(f64),
    Str(String),
}

impl PartialEq for Model {
    fn eq(&self, other: &Model) -> bool {
        match (self, other) {
            (Model::Int(a), Model::Int(b)) | (Model::Lgl(a), Model::Lgl(b)) => a == b,
            (Model::Dbl(a), Model::Dbl(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(&x, &y)| same_double(x, y))
            }
            (Model::Str(a), Model::Str(b)) => a == b,
            _ => false,
        }
    }
}

impl Model {
    fn len(&self) -> usize {
        match self {
            Model::Int(values) | Model::Lgl(values) => values.len(),
            Model::Dbl(values) => values.len(),
            Model::Str(values) => values.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn push(&mut self, item: Item) {
        match (self, item) {
            (Model::Int(values), Item::Int(value)) => values.push(value),
            (Model::Dbl(values), Item::Dbl(value)) => values.push(value),
            (Model::Str(values), Item::Str(value)) => values.push(value),
            _ => panic!("item of the wrong kind"),
        }
    }

    fn set(&mut self, index: usize, item: Item) {
        match (self, item) {
            (Model::Int(values), Item::Int(value)) => values[index] = value,
            (Model::Dbl(values), Item::Dbl(value)) => values[index] = value,
            (Model::Str(values), Item::Str(value)) => values[index] = value,
            _ => panic!("item of the wrong kind"),
        }
    }

    fn extend(&mut self, other: &Model) {
        match (self, other) {
            (Model::Int(values), Model::Int(other)) => values.extend(other),
            (Model::Dbl(values), Model::Dbl(other)) => values.extend(other),
            (Model::Str(values), Model::Str(other)) => values.extend(other.iter().cloned()),
            _ => panic!("lists of different kinds"),
        }
    }

    fn reversed(&self) -> Model {
        let mut model = self.clone();
        match &mut model {
            Model::Int(values) | Model::Lgl(values) => values.reverse(),
            Model::Dbl(values) => values.reverse(),
            Model::Str(values) => values.reverse(),
        }
        model
    }

    /// The sorted model, or `None` when it holds an `NA` or NaN, which the sort refuses.
    fn sorted(&self, decreasing: bool) -> Option<Model> {
        match self {
            Model::Int(values) => {
                if values.contains(&NA) {
                    return None;
                }
                let mut values = values.clone();
                values.sort();
                if decreasing {
                    values.reverse();
                }
                Some(Model::Int(values))
            }
            Model::Dbl(values) => {
                if values.iter().any(|value| value.is_nan()) {
                    return None;
                }
                let mut values = values.clone();
                values.sort_by(|x, y| {
                    let order = x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal);
                    if decreasing { order.reverse() } else { order }
                });
                Some(Model::Dbl(values))
            }
            _ => panic!("sorting an unsortable kind"),
        }
    }

    fn unique(&self) -> Model {
        match self {
            Model::Int(values) => {
                let mut seen = Vec::new();
                for &value in values {
                    if !seen.contains(&value) {
                        seen.push(value);
                    }
                }
                Model::Int(seen)
            }
            Model::Dbl(values) => {
                let mut seen: Vec<f64> = Vec::new();
                for &value in values {
                    if !seen.iter().any(|&kept| same_double_key(kept, value)) {
                        seen.push(value);
                    }
                }
                Model::Dbl(seen)
            }
            Model::Str(values) => {
                let mut seen: Vec<String> = Vec::new();
                for value in values {
                    if !seen.contains(value) {
                        seen.push(value.clone());
                    }
                }
                Model::Str(seen)
            }
            Model::Lgl(_) => panic!("unique of a logical model"),
        }
    }

    fn slice(&self, start: usize, count: usize) -> Model {
        let range = start..start + count;
        match self {
            Model::Int(values) => Model::Int(values[range].to_vec()),
            Model::Lgl(values) => Model::Lgl(values[range].to_vec()),
            Model::Dbl(values) => Model::Dbl(values[range].to_vec()),
            Model::Str(values) => Model::Str(values[range].to_vec()),
        }
    }
}

impl Item {
    /// The item's words; a string item is a fresh string whose count moves with the words.
    fn words(&self) -> (u64, u64) {
        match self {
            Item::Int(value) => (*value as u32 as u64, 0),
            Item::Dbl(value) => (value.to_bits(), 0),
            Item::Str(value) => {
                let string = str_of(value);
                (string.lo, string.hi)
            }
        }
    }
}

fn random_item(rng: &mut Rng, kind: u8) -> Item {
    match kind {
        INT => Item::Int(if rng.below(8) == 0 {
            NA
        } else {
            rng.below(20) as i32 - 10
        }),
        DBL => Item::Dbl(match rng.below(10) {
            0 => f64::from_bits(NA_REAL_BITS),
            1 => f64::NAN,
            2 => -0.0,
            3 => 0.0,
            _ => (rng.below(40) as f64 - 20.0) / 4.0,
        }),
        _ => Item::Str(match rng.below(4) {
            0 => String::new(),
            1 => format!("s{}", rng.below(10)),
            2 => "é".repeat(rng.below(12)),
            _ => format!("a string long enough for the heap {}", rng.below(10)),
        }),
    }
}

fn random_model(rng: &mut Rng, kind: u8) -> Model {
    let length = match rng.below(8) {
        0 => 0,
        1 => 40 + rng.below(40),
        _ => rng.below(8),
    };
    let mut model = match kind {
        INT => Model::Int(Vec::new()),
        DBL => Model::Dbl(Vec::new()),
        _ => Model::Str(Vec::new()),
    };
    for _ in 0..length {
        model.push(random_item(rng, kind));
    }
    model
}

fn build(model: &Model) -> *mut List {
    unsafe {
        let (kind, length) = match model {
            Model::Int(values) => (INT, values.len()),
            Model::Lgl(values) => (LGL, values.len()),
            Model::Dbl(values) => (DBL, values.len()),
            Model::Str(values) => (STR, values.len()),
        };
        let list = ry_list_new(kind as u32, 0);
        let list = (0..length).fold(list, |list, index| {
            let (word0, word1) = match model {
                Model::Int(values) | Model::Lgl(values) => (values[index] as u32 as u64, 0),
                Model::Dbl(values) => (values[index].to_bits(), 0),
                Model::Str(values) => {
                    let string = str_of(&values[index]);
                    (string.lo, string.hi)
                }
            };
            ry_list_push(list, 1, word0, word1)
        });
        assert_eq!((*list).length as usize, length);
        list
    }
}

fn read(list: *mut List) -> Model {
    unsafe {
        let length = (*list).length as usize;
        let data = (*list).data;
        let slice = |width: usize| std::slice::from_raw_parts(data, length * width);
        match (*list).kind {
            INT | LGL => {
                let values = slice(4)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|chunk| i32::from_le_bytes(*chunk))
                    .collect();
                if (*list).kind == INT {
                    Model::Int(values)
                } else {
                    Model::Lgl(values)
                }
            }
            DBL => Model::Dbl(
                (0..length)
                    .map(|index| *(data as *const f64).add(index))
                    .collect(),
            ),
            _ => Model::Str(
                (0..length)
                    .map(|index| text_of(*(data as *const Str).add(index)))
                    .collect(),
            ),
        }
    }
}

fn read_dict(dict: *mut u8, string_keys: bool) -> Vec<(String, String)> {
    unsafe {
        let keys = ry_dict_keys(dict);
        let values = ry_dict_values(dict);
        let keys_model = read(keys);
        let Model::Str(values_model) = read(values) else {
            panic!("values are strings")
        };
        let keys_text: Vec<String> = match keys_model {
            Model::Str(keys) if string_keys => keys,
            Model::Int(keys) if !string_keys => keys.iter().map(i32::to_string).collect(),
            other => panic!("unexpected key column {other:?}"),
        };
        assert_eq!(ry_dict_len(dict) as usize, keys_text.len());
        ry_list_drop(keys);
        ry_list_drop(values);
        keys_text.into_iter().zip(values_model).collect()
    }
}

fn entries(model: &HashMap<String, String>, order: &[String]) -> Vec<(String, String)> {
    order
        .iter()
        .map(|key| (key.clone(), model[key].clone()))
        .collect()
}

fn str_of(text: &str) -> Str {
    unsafe { ry_str_from_bytes(text.as_ptr(), text.len() as u64) }
}

fn text_of(string: Str) -> String {
    unsafe { string.as_str().to_owned() }
}

fn live() -> i64 {
    unsafe { ry_live_objects() }
}

fn is_na(value: f64) -> bool {
    value.to_bits() & 0xFFFF_FFFF == 1954 && value.is_nan()
}

/// Bitwise equality for numbers, with every NaN that is not `NA` alike.
fn same_double(a: f64, b: f64) -> bool {
    if a.is_nan() || b.is_nan() {
        a.is_nan() && b.is_nan() && is_na(a) == is_na(b)
    } else {
        a.to_bits() == b.to_bits()
    }
}

fn same_double_key(a: f64, b: f64) -> bool {
    if a.is_nan() || b.is_nan() {
        same_double(a, b)
    } else {
        a == b
    }
}

fn iterations(default: usize) -> usize {
    std::env::var("FUZZ_ITERS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(default)
}

/// splitmix64, seeded from `FUZZ_SEED`. It prints the replay command when the test fails.
struct Rng {
    seed: u64,
    state: u64,
}

impl Rng {
    fn from_env() -> Rng {
        let seed = std::env::var("FUZZ_SEED")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(0x5EED_1234);
        Rng { seed, state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

impl Drop for Rng {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "replay: FUZZ_SEED={} cargo test -p typed --test test_values",
                self.seed
            );
        }
    }
}
