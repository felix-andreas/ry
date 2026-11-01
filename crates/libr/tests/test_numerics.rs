//! Conformance of R's numerical C API (`R_ext/Applic.h`, `R_ext/Linpack.h`, `R_ext/Utils.h`)
//! with GNU R. The `numerics` test package runs fixed cases through the API; GNU R's answers
//! are recorded in `tests/data/numerics.expected` by `tests/data/numerics.R`, so the comparison
//! runs without R. With GNU R installed, a second test rebuilds the package against GNU R and
//! checks that the recording is current. A bounded property pass checks the sorting, interval
//! and decomposition routines against reference implementations.

mod harness;

use harness::*;
use std::{
    ffi::{c_char, c_int},
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};
use syntax::testing::{SplitMix64, fuzz_iterations};

struct Case {
    name: String,
    values: Vec<String>,
    printed: Vec<String>,
}

struct PrettyCall {
    input: (f64, f64, c_int),
    min_n: c_int,
    shrink: f64,
    factors: [f64; 3],
    eps_correction: c_int,
    return_bounds: c_int,
    output: (f64, f64, c_int),
}

fn expected_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/numerics.expected")
}

fn parse_expectations(text: &str) -> (Vec<Case>, Vec<PrettyCall>) {
    let mut cases: Vec<Case> = Vec::new();
    let mut pretty = Vec::new();
    for line in text.lines() {
        let (kind, rest) = line.split_once('\t').expect("a tab-separated line");
        match kind {
            "case" => cases.push(Case {
                name: rest.to_owned(),
                values: Vec::new(),
                printed: Vec::new(),
            }),
            "value" => cases
                .last_mut()
                .expect("a case")
                .values
                .push(rest.to_owned()),
            "print" => cases
                .last_mut()
                .expect("a case")
                .printed
                .push(rest.to_owned()),
            "pretty" => {
                let fields: Vec<&str> = rest.split('\t').collect();
                let number = |index: usize| fields[index].parse::<f64>().expect("a number");
                let integer = |index: usize| fields[index].parse::<c_int>().expect("an integer");
                pretty.push(PrettyCall {
                    input: (number(0), number(1), integer(2)),
                    min_n: integer(3),
                    shrink: number(4),
                    factors: [number(5), number(6), number(7)],
                    eps_correction: integer(8),
                    return_bounds: integer(9),
                    output: (number(10), number(11), integer(12)),
                });
            }
            other => panic!("unknown line kind {other:?}"),
        }
    }
    (cases, pretty)
}

/// How closely a case must agree with GNU R. Direct algorithms follow GNU R's operation order,
/// so they agree to rounding; the iterative ones compound rounding differences over their
/// iterations (and the system BLAS GNU R uses for `dnrm2` and friends rounds differently).
fn tolerance(case: &str) -> f64 {
    match case {
        "lbfgsb" | "optif9" | "quadrature" => 1e-6,
        _ => 1e-10,
    }
}

fn tokens_agree(expected: &str, actual: &str, tolerance: f64) -> bool {
    if expected == actual {
        return true;
    }
    match (expected.parse::<f64>(), actual.parse::<f64>()) {
        (Ok(expected), Ok(actual)) => {
            let scale = expected.abs().max(actual.abs()).max(1e-300);
            (expected - actual).abs() <= tolerance * scale.max(1.0)
        }
        _ => false,
    }
}

fn numerics(r: &R) {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    let library = BUILT.get_or_init(|| build_package("numerics", Headers::Ry));
    r.load(library);
}

/// The package as GNU R's toolchain builds it, once per test binary, because tests run in
/// parallel and would otherwise build into the same directory at the same time.
fn gnu_r_build() -> &'static PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| build_package("numerics", Headers::GnuR))
}

fn has_symbol(r: &R, name: &str) -> bool {
    let name = format!("{name}\0");
    unsafe { r.api().library.get::<*const ()>(name.as_bytes()).is_ok() }
}

fn console_lines() -> Vec<String> {
    let console = CONSOLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let text = String::from_utf8_lossy(&console).into_owned();
    text.lines().map(str::to_owned).collect()
}

fn clear_console() {
    CONSOLE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// Runs every recorded case in ry and lists the disagreements.
fn compare_cases(r: &R, cases: &[Case]) -> Vec<String> {
    let mut problems = Vec::new();
    // samin draws from norm_rand, which the Rmath module provides.
    let has_norm_rand = has_symbol(r, "norm_rand");
    for case in cases {
        if case.name == "samin" && !has_norm_rand {
            continue;
        }
        unsafe { (r.api().ry_set_seed)(1) };
        clear_console();
        let name = r.string(&case.name);
        let result = match r.call("num_case", &[name]) {
            Ok(result) => result,
            Err(error) => {
                problems.push(format!("{}: {error}", case.name));
                continue;
            }
        };
        let actual: Vec<String> = r
            .strings(result)
            .into_iter()
            .map(|token| token.unwrap_or_else(|| "NA".into()))
            .collect();
        if actual.len() != case.values.len() {
            problems.push(format!(
                "{}: {} values, GNU R has {}",
                case.name,
                actual.len(),
                case.values.len()
            ));
            continue;
        }
        let tolerance = tolerance(&case.name);
        for (position, (expected, actual)) in case.values.iter().zip(&actual).enumerate() {
            if !tokens_agree(expected, actual, tolerance) {
                problems.push(format!(
                    "{}[{position}]: ry {actual}, GNU R {expected}",
                    case.name
                ));
            }
        }
        let printed = console_lines();
        if printed != case.printed {
            problems.push(format!(
                "{}: printed {printed:?}, GNU R {:?}",
                case.name, case.printed
            ));
        }
    }
    problems
}

#[test]
fn numerics_agree_with_gnu_r() {
    let text = std::fs::read_to_string(expected_path()).expect("the recorded expectations");
    let (cases, _) = parse_expectations(&text);
    let r = r();
    numerics(&r);
    let problems = compare_cases(&r, &cases);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn numerics_survive_collection_stress() {
    let text = std::fs::read_to_string(expected_path()).expect("the recorded expectations");
    let (cases, _) = parse_expectations(&text);
    let r = r();
    numerics(&r);
    r.stress(true);
    let problems = compare_cases(&r, &cases);
    r.stress(false);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

type PrettyFn = unsafe extern "C" fn(
    *mut f64,
    *mut f64,
    *mut c_int,
    c_int,
    f64,
    *const f64,
    c_int,
    c_int,
) -> f64;

#[test]
fn r_pretty_agrees_with_gnu_r() {
    let text = std::fs::read_to_string(expected_path()).expect("the recorded expectations");
    let (_, calls) = parse_expectations(&text);
    let r = r();
    let pretty: PrettyFn = unsafe {
        *r.api()
            .library
            .get::<PrettyFn>(b"R_pretty\0")
            .expect("R_pretty")
    };
    let mut problems = Vec::new();
    for call in calls {
        let (mut low, mut high, mut cells) = call.input;
        unsafe {
            pretty(
                &mut low,
                &mut high,
                &mut cells,
                call.min_n,
                call.shrink,
                call.factors.as_ptr(),
                call.eps_correction,
                call.return_bounds,
            );
        }
        // GNU R returns -0 for some unit counts, which equals 0 here.
        if (low, high, cells) != call.output {
            problems.push(format!(
                "R_pretty({:?}, min_n {}, shrink {}, factors {:?}, eps {}, bounds {}): \
                 ry {:?}, GNU R {:?}",
                call.input,
                call.min_n,
                call.shrink,
                call.factors,
                call.eps_correction,
                call.return_bounds,
                (low, high, cells),
                call.output
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The recorded expectations are what GNU R answers today.
#[test]
fn expectations_match_a_fresh_gnu_r_run() {
    if !gnu_r_available() {
        return;
    }
    let library = gnu_r_build();
    let output = temporary_file("numerics.expected");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/numerics.R");
    let status = Command::new("Rscript")
        .arg(&script)
        .arg(library)
        .arg(&output)
        .status()
        .expect("Rscript runs");
    assert!(status.success());
    let fresh = std::fs::read_to_string(&output).expect("the fresh recording");
    let recorded = std::fs::read_to_string(expected_path()).expect("the recorded expectations");
    // GNU R's routines call the system BLAS, whose summation order varies between builds and
    // even between runs, so a fresh run can differ from the recording in the last digits. The
    // recording counts as current when it agrees with the fresh run under each case's
    // tolerance, and ry must agree with the fresh run under the same tolerances.
    let (fresh_cases, fresh_pretty) = parse_expectations(&fresh);
    let (recorded_cases, recorded_pretty) = parse_expectations(&recorded);
    let mut stale = Vec::new();
    if fresh_cases.len() != recorded_cases.len() || fresh_pretty.len() != recorded_pretty.len() {
        stale.push(format!(
            "{} fresh cases and {} pretty calls, {} and {} recorded",
            fresh_cases.len(),
            fresh_pretty.len(),
            recorded_cases.len(),
            recorded_pretty.len()
        ));
    }
    for (fresh, recorded) in fresh_cases.iter().zip(&recorded_cases) {
        let tolerance = tolerance(&fresh.name);
        let values_agree = fresh.values.len() == recorded.values.len()
            && fresh
                .values
                .iter()
                .zip(&recorded.values)
                .all(|(fresh, recorded)| tokens_agree(fresh, recorded, tolerance));
        if fresh.name != recorded.name || !values_agree || fresh.printed != recorded.printed {
            stale.push(format!(
                "case {:?} (recorded as {:?})",
                fresh.name, recorded.name
            ));
        }
    }
    for (fresh, recorded) in fresh_pretty.iter().zip(&recorded_pretty) {
        if fresh.input != recorded.input || fresh.output != recorded.output {
            stale.push(format!("R_pretty{:?}", fresh.input));
        }
    }
    assert!(
        stale.is_empty(),
        "tests/data/numerics.expected is stale; regenerate it with tests/data/numerics.R:\n{}",
        stale.join("\n")
    );
    let r = r();
    numerics(&r);
    let problems = compare_cases(&r, &fresh_cases);
    assert!(
        problems.is_empty(),
        "ry disagrees with a fresh GNU R run:\n{}",
        problems.join("\n")
    );
}

/// The package built by GNU R's toolchain against GNU R's headers runs in ry with the same
/// answers, which is how prebuilt binaries reach these entry points.
#[test]
fn gnu_r_binary_agrees_too() {
    if !gnu_r_available() {
        return;
    }
    let text = std::fs::read_to_string(expected_path()).expect("the recorded expectations");
    let (cases, _) = parse_expectations(&text);
    let library = gnu_r_build();
    let r = r();
    r.load(library);
    let problems = compare_cases(&r, &cases);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn errors_inside_callbacks_unwind_cleanly() {
    let r = r();
    numerics(&r);
    let depth = unsafe { (r.api().ry_context_depth)() };
    let protect = r.runtime_protect_depth();
    let expectations = [
        (0, "objective failed at"),
        (1, "integrand failed with 21 points"),
        (2, "fortran says"),
    ];
    for (kind, message) in expectations {
        let which = r.integer(kind);
        let error = r
            .call("num_failing", &[which])
            .expect_err("the callback's error propagates");
        assert!(error.contains(message), "{error}");
        assert_eq!(unsafe { (r.api().ry_context_depth)() }, depth);
        assert_eq!(r.runtime_protect_depth(), protect);
    }
    let which = r.integer(3);
    r.call("num_failing", &[which]).expect("a warning returns");
    assert!(
        r.warnings().iter().any(|warning| warning == "fortran warn"),
        "{:?}",
        r.warnings()
    );
}

#[test]
fn tilde_expansion_uses_home() {
    let r = r();
    type Expand = unsafe extern "C" fn(*const c_char) -> *const c_char;
    let expand: Expand = unsafe {
        *r.api()
            .library
            .get::<Expand>(b"R_ExpandFileName\0")
            .expect("symbol")
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let expanded = unsafe { std::ffi::CStr::from_ptr(expand(c"~/data.csv".as_ptr())) };
    if !home.is_empty() {
        assert_eq!(expanded.to_string_lossy(), format!("{home}/data.csv"));
    }
    let untouched = unsafe { std::ffi::CStr::from_ptr(expand(c"data/~x".as_ptr())) };
    assert_eq!(untouched.to_string_lossy(), "data/~x");
}

// Bounded property checks against reference implementations.

type SortReal = unsafe extern "C" fn(*mut f64, c_int);
type SortInt = unsafe extern "C" fn(*mut c_int, c_int);
type SortIndexed = unsafe extern "C" fn(*mut f64, *mut c_int, c_int);
type QsortReal = unsafe extern "C" fn(*mut f64, usize, usize);
type QsortRealIndexed = unsafe extern "C" fn(*mut f64, *mut c_int, c_int, c_int);
type QsortIntIndexed = unsafe extern "C" fn(*mut c_int, *mut c_int, c_int, c_int);
type PartialReal = unsafe extern "C" fn(*mut f64, c_int, c_int);
type PartialInt = unsafe extern "C" fn(*mut c_int, c_int, c_int);
type FindInterval2 =
    unsafe extern "C" fn(*mut f64, c_int, f64, c_int, c_int, c_int, c_int, *mut c_int) -> c_int;
type Dqrls = unsafe extern "C" fn(
    *mut f64,
    *const c_int,
    *const c_int,
    *mut f64,
    *const c_int,
    *const f64,
    *mut f64,
    *mut f64,
    *mut f64,
    *mut c_int,
    *mut c_int,
    *mut f64,
    *mut f64,
);
type Dsvdc = unsafe extern "C" fn(
    *mut f64,
    *const c_int,
    *const c_int,
    *const c_int,
    *mut f64,
    *mut f64,
    *mut f64,
    *const c_int,
    *mut f64,
    *const c_int,
    *mut f64,
    *const c_int,
    *mut c_int,
);

fn symbol<T: Copy>(r: &R, name: &str) -> T {
    let name = format!("{name}\0");
    unsafe {
        *r.api()
            .library
            .get::<T>(name.as_bytes())
            .unwrap_or_else(|error| panic!("libR does not export {name}: {error}"))
    }
}

const NA_INTEGER: c_int = c_int::MIN;

fn na_real() -> f64 {
    f64::from_bits(0x7FF0_0000_0000_07A2)
}

/// A value from a small pool, so ties are common, with `NA`, `NaN` and infinities sometimes.
fn real(rng: &mut SplitMix64, special: bool) -> f64 {
    if special && rng.chance(1, 10) {
        return [na_real(), f64::NAN, f64::INFINITY, f64::NEG_INFINITY][rng.below(4)];
    }
    rng.below(20) as f64 - 10.0 + if rng.chance(1, 3) { 0.5 } else { 0.0 }
}

/// R's order: ascending with `NA` and `NaN` last.
fn reference_order(left: f64, right: f64) -> std::cmp::Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => left.partial_cmp(&right).expect("ordered"),
    }
}

fn same_class(left: f64, right: f64) -> bool {
    (left.is_nan() && right.is_nan()) || left == right
}

#[test]
fn fuzz_sorting_matches_reference_sorts() {
    let r = r();
    let rsort: SortReal = symbol(&r, "R_rsort");
    let isort: SortInt = symbol(&r, "R_isort");
    let with_index: SortIndexed = symbol(&r, "rsort_with_index");
    let revsort: SortIndexed = symbol(&r, "Rf_revsort");
    let qsort: QsortReal = symbol(&r, "R_qsort");
    let qsort_indexed: QsortRealIndexed = symbol(&r, "R_qsort_I");
    let qsort_int_indexed: QsortIntIndexed = symbol(&r, "R_qsort_int_I");
    let rpsort: PartialReal = symbol(&r, "Rf_rPsort");
    let ipsort: PartialInt = symbol(&r, "Rf_iPsort");
    let mut rng = SplitMix64(0x5eed_5047);
    for _ in 0..fuzz_iterations(300) {
        let count = rng.below(70);
        let values: Vec<f64> = (0..count).map(|_| real(&mut rng, true)).collect();
        let mut reference = values.clone();
        reference.sort_by(|a, b| reference_order(*a, *b));

        let mut sorted = values.clone();
        unsafe { rsort(sorted.as_mut_ptr(), count as c_int) };
        assert!(
            sorted
                .iter()
                .zip(&reference)
                .all(|(a, b)| same_class(*a, *b)),
            "R_rsort {values:?}"
        );

        let mut sorted = values.clone();
        let mut index: Vec<c_int> = (1..=count as c_int).collect();
        unsafe { with_index(sorted.as_mut_ptr(), index.as_mut_ptr(), count as c_int) };
        assert!(
            sorted
                .iter()
                .zip(&reference)
                .all(|(a, b)| same_class(*a, *b))
        );
        for (value, &original) in sorted.iter().zip(&index) {
            assert!(same_class(*value, values[original as usize - 1]));
        }

        let integers: Vec<c_int> = values
            .iter()
            .map(|value| {
                if value.is_finite() {
                    *value as c_int
                } else {
                    NA_INTEGER
                }
            })
            .collect();
        let mut sorted_integers = integers.clone();
        unsafe { isort(sorted_integers.as_mut_ptr(), count as c_int) };
        let mut reference_integers = integers.clone();
        reference_integers.sort_by_key(|&value| (value == NA_INTEGER, value));
        assert_eq!(sorted_integers, reference_integers);

        // The quicksorts and revsort compare with plain < and >, so they get no NaN.
        let finite: Vec<f64> = values
            .iter()
            .copied()
            .filter(|value| !value.is_nan())
            .collect();
        let n = finite.len();
        let mut descending = finite.clone();
        let mut index: Vec<c_int> = (1..=n as c_int).collect();
        unsafe { revsort(descending.as_mut_ptr(), index.as_mut_ptr(), n as c_int) };
        assert!(
            descending.windows(2).all(|pair| pair[0] >= pair[1]),
            "revsort {finite:?}"
        );
        for (value, &original) in descending.iter().zip(&index) {
            assert_eq!(*value, finite[original as usize - 1]);
        }
        if n >= 2 {
            let first = 1 + rng.below(n);
            let last = first + rng.below(n - first + 1);
            let mut partial = finite.clone();
            unsafe { qsort(partial.as_mut_ptr(), first, last) };
            assert_eq!(partial[..first - 1], finite[..first - 1]);
            assert_eq!(partial[last..], finite[last..]);
            assert!(
                partial[first - 1..last]
                    .windows(2)
                    .all(|pair| pair[0] <= pair[1])
            );
            let mut sorted = finite.clone();
            let mut index: Vec<c_int> = (1..=n as c_int).collect();
            unsafe { qsort_indexed(sorted.as_mut_ptr(), index.as_mut_ptr(), 1, n as c_int) };
            assert!(sorted.windows(2).all(|pair| pair[0] <= pair[1]));
            for (value, &original) in sorted.iter().zip(&index) {
                assert_eq!(*value, finite[original as usize - 1]);
            }
            let mut sorted: Vec<c_int> = finite
                .iter()
                .map(|value| value.clamp(-1e9, 1e9) as c_int)
                .collect();
            let original = sorted.clone();
            let mut index: Vec<c_int> = (1..=n as c_int).collect();
            unsafe { qsort_int_indexed(sorted.as_mut_ptr(), index.as_mut_ptr(), 1, n as c_int) };
            assert!(sorted.windows(2).all(|pair| pair[0] <= pair[1]));
            for (value, &position) in sorted.iter().zip(&index) {
                assert_eq!(*value, original[position as usize - 1]);
            }
        }

        if count > 0 {
            let k = rng.below(count);
            let mut partial = values.clone();
            unsafe { rpsort(partial.as_mut_ptr(), count as c_int, k as c_int) };
            assert!(
                same_class(partial[k], reference[k]),
                "rPsort {values:?} at {k}"
            );
            assert!(
                partial[..k]
                    .iter()
                    .all(|value| reference_order(*value, partial[k]).is_le())
            );
            assert!(
                partial[k + 1..]
                    .iter()
                    .all(|value| reference_order(*value, partial[k]).is_ge())
            );
            let mut partial = integers.clone();
            unsafe { ipsort(partial.as_mut_ptr(), count as c_int, k as c_int) };
            assert_eq!(partial[k], reference_integers[k]);
        }
    }
}

#[test]
fn fuzz_find_interval_matches_a_linear_scan() {
    let r = r();
    let find: FindInterval2 = symbol(&r, "findInterval2");
    let mut rng = SplitMix64(0x01a7_e0a1);
    for _ in 0..fuzz_iterations(500) {
        let count = rng.below(12);
        let mut knots: Vec<f64> = (0..count).map(|_| real(&mut rng, false)).collect();
        knots.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        let x = real(&mut rng, false);
        let flags = rng.below(8);
        let (closed, inside, left_open) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
        let mut flag = 99;
        let hint = 1 + rng.below(count.max(1)) as c_int;
        let index = unsafe {
            find(
                knots.as_mut_ptr(),
                count as c_int,
                x,
                closed as c_int,
                inside as c_int,
                left_open as c_int,
                hint,
                &mut flag,
            )
        };
        if count == 0 {
            assert_eq!(index, 0);
            continue;
        }
        let below = knots
            .iter()
            .filter(|&&knot| if left_open { knot < x } else { knot <= x })
            .count() as c_int;
        let n = count as c_int;
        let expected = if below == 0 {
            (inside || (left_open && closed && x == knots[0])) as c_int
        } else if below == n {
            if inside || (!left_open && closed && x == knots[count - 1]) {
                n - 1
            } else {
                n
            }
        } else {
            below
        };
        assert_eq!(index, expected, "{knots:?} x={x} flags={flags}");
        let expected_flag = if below == 0 {
            -1
        } else if below == n {
            1
        } else {
            0
        };
        assert_eq!(flag, expected_flag);
    }
}

#[test]
fn fuzz_decompositions_reconstruct_their_input() {
    let r = r();
    let dqrls: Dqrls = symbol(&r, "dqrls_");
    let dsvdc: Dsvdc = symbol(&r, "dsvdc_");
    let mut rng = SplitMix64(0xdec0_0005);
    let uniform =
        |rng: &mut SplitMix64| (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
    for _ in 0..fuzz_iterations(200) {
        let n = 1 + rng.below(8);
        let p = 1 + rng.below(6);
        let x: Vec<f64> = (0..n * p).map(|_| uniform(&mut rng)).collect();
        let y: Vec<f64> = (0..n).map(|_| uniform(&mut rng)).collect();

        // Least squares: the residuals are orthogonal to the columns used.
        let mut qr = x.clone();
        let mut response = y.clone();
        let (mut b, mut rsd, mut qty) = (vec![0.0; p], vec![0.0; n], vec![0.0; n]);
        let (mut rank, mut pivot) = (0, (1..=p as c_int).collect::<Vec<_>>());
        let (mut qraux, mut work) = (vec![0.0; p], vec![0.0; 2 * p]);
        let (rows, columns, one, tol) = (n as c_int, p as c_int, 1, 1e-7);
        unsafe {
            dqrls(
                qr.as_mut_ptr(),
                &rows,
                &columns,
                response.as_mut_ptr(),
                &one,
                &tol,
                b.as_mut_ptr(),
                rsd.as_mut_ptr(),
                qty.as_mut_ptr(),
                &mut rank,
                pivot.as_mut_ptr(),
                qraux.as_mut_ptr(),
                work.as_mut_ptr(),
            )
        };
        assert!(rank as usize <= n.min(p));
        let mut sorted_pivot = pivot.clone();
        sorted_pivot.sort();
        assert_eq!(sorted_pivot, (1..=p as c_int).collect::<Vec<_>>());
        for &position in &pivot[..rank as usize] {
            let column = position as usize - 1;
            let dot: f64 = (0..n).map(|i| x[i + column * n] * rsd[i]).sum();
            assert!(dot.abs() < 1e-8, "residuals not orthogonal: {dot}");
        }
        for i in 0..n {
            let fitted: f64 = (0..p)
                .map(|j| x[i + (pivot[j] as usize - 1) * n] * b[j])
                .sum();
            assert!((fitted + rsd[i] - y[i]).abs() < 1e-8);
        }

        // SVD: U S V' reconstructs X.
        let mut a = x.clone();
        let mut s = vec![0.0; n.min(p) + 1];
        let mut e = vec![0.0; p];
        let mut u = vec![0.0; n * n];
        let mut v = vec![0.0; p * p];
        let mut work = vec![0.0; n];
        let (job, mut info) = (11, 0);
        unsafe {
            dsvdc(
                a.as_mut_ptr(),
                &rows,
                &rows,
                &columns,
                s.as_mut_ptr(),
                e.as_mut_ptr(),
                u.as_mut_ptr(),
                &rows,
                v.as_mut_ptr(),
                &columns,
                work.as_mut_ptr(),
                &job,
                &mut info,
            )
        };
        assert_eq!(info, 0);
        let k = n.min(p);
        assert!(s[..k].windows(2).all(|pair| pair[0] >= pair[1]));
        for i in 0..n {
            for j in 0..p {
                let value: f64 = (0..k).map(|l| u[i + l * n] * s[l] * v[j + l * p]).sum();
                assert!(
                    (value - x[i + j * n]).abs() < 1e-10,
                    "SVD does not reconstruct X"
                );
            }
        }
    }
}
