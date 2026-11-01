//! Conformance of the QR routines behind `qr()`, `lm.fit()` and the `qr.*` helpers (`dqrdc2`,
//! `dqrls`, `dqrqy`, `dqrqty`, `dqrcf`, `dqrrsd` and `dqrxb`) with GNU R. GNU R's answers are
//! recorded in `tests/data/linpack.expected` by `tests/data/linpack.R`, so the comparison runs
//! without R; with GNU R installed, a second test checks that the recording is current.
//!
//! `lm()` reads the pivot order and rank, so those must agree exactly, including for columns
//! that sit right at the tolerance. The decomposition itself agrees to rounding.

mod harness;

use {
    R::linpack::{dchdc_, dqrcf_, dqrdc2_, dqrls_, dqrqty_, dqrqy_, dqrrsd_, dqrxb_},
    harness::{gnu_r_available, temporary_file},
    std::{collections::HashMap, ffi::c_int, path::Path, process::Command},
};

/// The agreement asked of every floating-point result, relative to the largest entry of the
/// vector it belongs to. GNU R computes norms and dot products with whatever BLAS it links
/// against, which rounds differently from ry, and an ill-conditioned matrix magnifies that
/// difference, so the bound grows with the ratio of the largest to the smallest kept
/// diagonal entry of `R`.
const TOLERANCE: f64 = 1e-12;
const CONDITIONED_TOLERANCE: f64 = 1e-14;

struct Case {
    name: String,
    n: c_int,
    p: c_int,
    ny: c_int,
    tol: f64,
    vectors: HashMap<String, Vec<String>>,
}

impl Case {
    fn numbers(&self, label: &str) -> Vec<f64> {
        self.tokens(label)
            .iter()
            .map(|token| token.parse().expect("a number"))
            .collect()
    }

    fn integers(&self, label: &str) -> Vec<c_int> {
        self.tokens(label)
            .iter()
            .map(|token| token.parse().expect("an integer"))
            .collect()
    }

    fn tokens(&self, label: &str) -> &[String] {
        self.vectors
            .get(label)
            .unwrap_or_else(|| panic!("{}: no {label} recorded", self.name))
    }

    fn has(&self, label: &str) -> bool {
        self.vectors.contains_key(label)
    }
}

fn expected_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linpack.expected")
}

fn parse_cases(text: &str) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let label = fields.next().expect("a label");
        let values: Vec<String> = fields.map(str::to_owned).collect();
        if label == "case" {
            let field = |index: usize| values.get(index).expect("a case field");
            cases.push(Case {
                name: field(0).clone(),
                n: field(1).parse().expect("n"),
                p: field(2).parse().expect("p"),
                ny: field(3).parse().expect("ny"),
                tol: field(4).parse().expect("tol"),
                vectors: HashMap::new(),
            });
        } else {
            let case = cases.last_mut().expect("a case before its vectors");
            case.vectors.insert(label.to_owned(), values);
        }
    }
    cases
}

/// Compares the entries of one vector that `meaningful` selects and records any disagreement
/// under `label`.
fn compare(
    problems: &mut Vec<String>,
    case: &Case,
    label: &str,
    actual: &[f64],
    relative: f64,
    meaningful: impl Fn(usize) -> bool,
) {
    let expected = case.numbers(label);
    if expected.len() != actual.len() {
        problems.push(format!(
            "{}: {label} has {} values, GNU R {}",
            case.name,
            actual.len(),
            expected.len()
        ));
        return;
    }
    let scale = (0..expected.len())
        .filter(|&position| meaningful(position))
        .fold(0.0f64, |largest, position| {
            largest.max(expected[position].abs())
        });
    for (position, (&expected, &actual)) in expected.iter().zip(actual).enumerate() {
        if !meaningful(position) {
            continue;
        }
        // Written so that a NaN on either side counts as a disagreement.
        let agrees = (expected - actual).abs() <= relative * scale;
        if !agrees {
            problems.push(format!(
                "{}: {label}[{position}] is {actual:e}, GNU R {expected:e}",
                case.name
            ));
        }
    }
}

fn compare_exact(problems: &mut Vec<String>, case: &Case, label: &str, actual: &[c_int]) {
    let expected = case.integers(label);
    if expected != actual {
        problems.push(format!(
            "{}: {label} is {actual:?}, GNU R {expected:?}",
            case.name
        ));
    }
}

fn check_case(case: &Case) -> Vec<String> {
    let mut problems = Vec::new();
    let (n, p, ny) = (case.n, case.p, case.ny);
    let input = case.numbers("x");
    let responses = case.numbers("y");
    let expected_rank = case.integers("dqrdc2_rank")[0] as usize;
    let recorded_qr = case.numbers("dqrdc2_qr");
    // A zero tolerance can keep a zero column, whose zero diagonal says nothing about how
    // the other columns are conditioned.
    let diagonal: Vec<f64> = (0..expected_rank)
        .map(|index| recorded_qr[index + index * n as usize].abs())
        .filter(|&value| value > 0.0)
        .collect();
    let largest = diagonal.iter().fold(0.0f64, |held, &value| held.max(value));
    let smallest = diagonal
        .iter()
        .fold(f64::INFINITY, |held, &value| held.min(value));
    let condition = if diagonal.is_empty() {
        1.0
    } else {
        largest / smallest
    };
    let relative = TOLERANCE.max(CONDITIONED_TOLERANCE * condition);
    let rows = n as usize;
    // Below the rank, a dropped column holds only rounding noise, which the reflections
    // then scale up; every other entry is determined by the data.
    let determined =
        |position: usize| position % rows < expected_rank || position / rows < expected_rank;
    let kept = |position: usize| position < expected_rank;
    let all = |_: usize| true;

    let mut qr = input.clone();
    let mut rank: c_int = -1;
    let mut qraux = vec![0.0; p as usize];
    let mut pivot: Vec<c_int> = (1..=p).collect();
    let mut work = vec![0.0; 2 * p as usize];
    unsafe {
        dqrdc2_(
            qr.as_mut_ptr(),
            &n,
            &n,
            &p,
            &case.tol,
            &mut rank,
            qraux.as_mut_ptr(),
            pivot.as_mut_ptr(),
            work.as_mut_ptr(),
        );
    }
    compare_exact(&mut problems, case, "dqrdc2_rank", &[rank]);
    compare_exact(&mut problems, case, "dqrdc2_pivot", &pivot);
    compare(&mut problems, case, "dqrdc2_qr", &qr, relative, determined);
    compare(&mut problems, case, "dqrdc2_qraux", &qraux, relative, kept);

    let mut fit_qr = input.clone();
    let mut observed = responses.clone();
    let mut coefficients = vec![f64::NAN; (p * ny) as usize];
    let mut residuals = vec![f64::NAN; (n * ny) as usize];
    let mut effects = responses.clone();
    let mut fit_rank: c_int = -1;
    let mut fit_pivot: Vec<c_int> = (1..=p).collect();
    let mut fit_qraux = vec![0.0; p as usize];
    let mut fit_work = vec![0.0; 2 * p as usize];
    unsafe {
        dqrls_(
            fit_qr.as_mut_ptr(),
            &n,
            &p,
            observed.as_mut_ptr(),
            &ny,
            &case.tol,
            coefficients.as_mut_ptr(),
            residuals.as_mut_ptr(),
            effects.as_mut_ptr(),
            &mut fit_rank,
            fit_pivot.as_mut_ptr(),
            fit_qraux.as_mut_ptr(),
            fit_work.as_mut_ptr(),
        );
    }
    if observed != responses {
        problems.push(format!("{}: dqrls changed y", case.name));
    }
    compare_exact(&mut problems, case, "dqrls_rank", &[fit_rank]);
    compare_exact(&mut problems, case, "dqrls_pivot", &fit_pivot);
    compare(
        &mut problems,
        case,
        "dqrls_qr",
        &fit_qr,
        relative,
        determined,
    );
    compare(
        &mut problems,
        case,
        "dqrls_coefficients",
        &coefficients,
        relative,
        all,
    );
    compare(
        &mut problems,
        case,
        "dqrls_residuals",
        &residuals,
        relative,
        all,
    );
    compare(
        &mut problems,
        case,
        "dqrls_effects",
        &effects,
        relative,
        all,
    );
    compare(
        &mut problems,
        case,
        "dqrls_qraux",
        &fit_qraux,
        relative,
        kept,
    );

    if !case.has("dqrqy") {
        return problems;
    }
    type Helper = unsafe extern "C" fn(
        *mut f64,
        *const c_int,
        *const c_int,
        *mut f64,
        *mut f64,
        *const c_int,
        *mut f64,
    );
    let helpers: [(&str, Helper); 4] = [
        ("dqrqy", dqrqy_),
        ("dqrqty", dqrqty_),
        ("dqrrsd", dqrrsd_),
        ("dqrxb", dqrxb_),
    ];
    for (label, helper) in helpers {
        let mut target = responses.clone();
        let mut output = responses.clone();
        unsafe {
            helper(
                qr.as_mut_ptr(),
                &n,
                &rank,
                qraux.as_mut_ptr(),
                target.as_mut_ptr(),
                &ny,
                output.as_mut_ptr(),
            );
        }
        compare(&mut problems, case, label, &output, relative, all);
        compare(
            &mut problems,
            case,
            &format!("{label}_y"),
            &target,
            relative,
            all,
        );
    }
    let mut target = responses.clone();
    let mut solved = vec![0.0; (rank * ny) as usize];
    let mut info: c_int = -1;
    unsafe {
        dqrcf_(
            qr.as_mut_ptr(),
            &n,
            &rank,
            qraux.as_mut_ptr(),
            target.as_mut_ptr(),
            &ny,
            solved.as_mut_ptr(),
            &mut info,
        );
    }
    compare(&mut problems, case, "dqrcf", &solved, relative, all);
    compare(&mut problems, case, "dqrcf_y", &target, relative, all);
    compare_exact(&mut problems, case, "dqrcf_info", &[info]);
    problems
}

#[test]
fn qr_routines_agree_with_gnu_r() {
    let text = std::fs::read_to_string(expected_path()).expect("the recorded expectations");
    let cases = parse_cases(&text);
    assert!(!cases.is_empty());
    let problems: Vec<String> = cases.iter().flat_map(check_case).collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// ry agrees with what this machine's GNU R answers today, not only with the recording.
#[test]
fn expectations_match_a_fresh_gnu_r_run() {
    if !gnu_r_available() {
        return;
    }
    let output = temporary_file("linpack.expected");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/linpack.R");
    let status = Command::new("Rscript")
        .arg(&script)
        .arg(&output)
        .status()
        .expect("Rscript runs");
    assert!(status.success());
    let fresh = std::fs::read_to_string(&output).expect("the fresh recording");
    // GNU R's QR routines call the system BLAS, whose summation order varies between builds,
    // so a fresh run differs from the recording in the last bits, and in the arbitrary parts of
    // a rank-deficient factorization. The check that matters is the one the recording feeds:
    // ry agrees with this machine's GNU R under the same tolerances.
    let problems: Vec<String> = parse_cases(&fresh).iter().flat_map(check_case).collect();
    assert!(
        problems.is_empty(),
        "ry disagrees with a fresh GNU R run (regenerate tests/data/linpack.expected with \
         tests/data/linpack.R if GNU R changed):\n{}",
        problems.join("\n")
    );
}

/// Reconstructs `P'AP` from the upper factor `dchdc` leaves in `a`, as an `n` by `n` matrix.
fn reconstruct(factor: &[f64], n: usize) -> Vec<f64> {
    let upper = |row: usize, column: usize| {
        if row <= column {
            factor[row + column * n]
        } else {
            0.0
        }
    };
    let mut product = vec![0.0; n * n];
    for row in 0..n {
        for column in 0..n {
            product[row + column * n] = (0..n).map(|k| upper(k, row) * upper(k, column)).sum();
        }
    }
    product
}

#[test]
fn dchdc_factors_with_and_without_pivoting() {
    let n = 4usize;
    // A full-rank symmetric matrix whose largest diagonal entry is last, so pivoting moves it.
    let matrix = [
        4.0, 2.0, 0.6, 1.0, //
        2.0, 5.0, 1.0, 2.0, //
        0.6, 1.0, 6.0, 3.0, //
        1.0, 2.0, 3.0, 9.0,
    ];
    let order = n as c_int;
    let mut work = vec![0.0; n];
    let mut info = 0;

    let mut plain = matrix;
    let mut pivots = vec![0 as c_int; n];
    unsafe {
        dchdc_(
            plain.as_mut_ptr(),
            &order,
            &order,
            work.as_mut_ptr(),
            pivots.as_mut_ptr(),
            &0,
            &mut info,
        )
    };
    assert_eq!(info, 4);
    let product = reconstruct(&plain, n);
    for (actual, expected) in product.iter().zip(matrix.iter()) {
        assert!((actual - expected).abs() < 1e-12, "{product:?}");
    }

    let mut pivoted = matrix;
    let mut pivots = vec![0 as c_int; n];
    unsafe {
        dchdc_(
            pivoted.as_mut_ptr(),
            &order,
            &order,
            work.as_mut_ptr(),
            pivots.as_mut_ptr(),
            &1,
            &mut info,
        )
    };
    assert_eq!(info, 4);
    assert_eq!(
        pivots[0], 4,
        "the largest diagonal entry comes first: {pivots:?}"
    );
    let product = reconstruct(&pivoted, n);
    for row in 0..n {
        for column in 0..n {
            let expected = matrix[(pivots[row] - 1) as usize + (pivots[column] - 1) as usize * n];
            assert!(
                (product[row + column * n] - expected).abs() < 1e-12,
                "{product:?} {pivots:?}"
            );
        }
    }

    // A rank-2 matrix stops after two positive pivots. The entries keep the arithmetic exact,
    // because LINPACK has no tolerance and a rounding residue would count as a third pivot.
    let mut singular = [4.0, 2.0, 0.0, 2.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let order = 3;
    let mut pivots = vec![0 as c_int; 3];
    unsafe {
        dchdc_(
            singular.as_mut_ptr(),
            &order,
            &order,
            work.as_mut_ptr(),
            pivots.as_mut_ptr(),
            &1,
            &mut info,
        )
    };
    assert_eq!(info, 2, "{singular:?} {pivots:?}");
}
