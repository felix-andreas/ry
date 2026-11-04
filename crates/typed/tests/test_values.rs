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
                        (
                            ry_list_sort(list, flag, decreasing as u32),
                            model.sorted(decreasing),
                        )
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
        assert!(ry_compare_vs(LT, STR as u32, strings, 0, key.lo, key.hi).is_null());
        for list in [both, either, negated, equal, strings, b] {
            ry_list_drop(list);
        }
        ry_str_drop(key.lo, key.hi);
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
                        ry_dict_insert(dict, owned as u32, key.lo, key.hi, value.lo, value.hi)
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
                    let mut out = [0u64; 2];
                    let found = unsafe { ry_dict_get(dict, key.lo, key.hi, out.as_mut_ptr()) };
                    assert_eq!(found == 1, model.contains_key(&key_text), "get {key_text}");
                    assert_eq!(unsafe { ry_dict_contains(dict, key.lo, key.hi) }, found);
                    if found == 1 {
                        let value = Str {
                            lo: out[0],
                            hi: out[1],
                        };
                        assert_eq!(Some(&text_of(value)), model.get(&key_text));
                        unsafe { ry_str_drop(value.lo, value.hi) };
                    }
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

    fn sorted(&self, decreasing: bool) -> Model {
        match self {
            Model::Int(values) => {
                let mut values: Vec<i32> = values
                    .iter()
                    .copied()
                    .filter(|&value| value != NA)
                    .collect();
                values.sort();
                if decreasing {
                    values.reverse();
                }
                Model::Int(values)
            }
            Model::Dbl(values) => {
                let mut values: Vec<f64> = values
                    .iter()
                    .copied()
                    .filter(|value| !value.is_nan())
                    .collect();
                if decreasing {
                    values.sort_by(|x, y| y.partial_cmp(x).unwrap_or(std::cmp::Ordering::Equal));
                } else {
                    values.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
                }
                Model::Dbl(values)
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
