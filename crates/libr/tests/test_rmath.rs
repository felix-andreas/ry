//! Conformance of ry's Rmath against GNU R. `tests/data/rmath_expected.tsv` and
//! `rmath_streams.tsv` hold GNU R's answers, recorded by `tests/data/rmath_cases.R` through
//! the C package `tests/packages/rmathtest`; here the same package, built against ry's
//! headers, evaluates every case in ry's libR.

mod harness;

use harness::*;
use std::{
    collections::BTreeMap,
    ffi::c_int,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

const NA_REAL_BITS: u64 = 0x7ff0_0000_0000_07a2;

/// The relative and absolute tolerance each function is held to. The default means "the same
/// algorithm": a few units in the last place. A looser entry names the reason.
fn tolerance(name: &str, arguments: &[f64], expected: f64) -> (f64, f64) {
    match name {
        // ry uses Temme's and Steed's methods for small arguments, power series where they
        // have no cancellation and Hankel's expansion with the three-term recurrence for large
        // ones, where GNU R uses Cody's RIBESL family; both agree with 40-digit references to
        // about 1e-13. GNU R's scaled I loses 2e-12 around x = 1e4. J and Y also get an
        // absolute part for their zeros.
        "bessel_i" => (2e-12, 0.0),
        "bessel_j" | "bessel_y" => (1e-12, 1e-15),
        // The polygamma functions sum the Hurwitz zeta series rather than running Amos's
        // algorithm; digamma loses relative accuracy near its root at 1.46.
        "digamma" | "trigamma" | "tetragamma" | "pentagamma" | "psigamma" | "dpsifn" => {
            (1e-13, 1e-15)
        }
        // GNU R accumulates these series in long double, which ry does not have. Where a
        // result is the complement of a sum near one, that is visible at about 1e-16
        // absolute, and AS 226 itself stops at an error bound of 1e-9.
        "pnbeta" | "pnf" | "dnbeta" | "dnf" => (1e-9, 1e-15),
        "pnchisq" | "phyper" | "pnt" => (1e-11, 1e-14),
        // dnt is a difference quotient of two values of pnt.
        "dnt" => (1e-6, 1e-14),
        // Bisection on those sums stops within 1e-13 of the root (qnchisq, qnt, qnbeta, qnf),
        // and qtukey's secant method within 1e-4, so where they stop depends on the last bits
        // of the distribution function.
        "qnchisq" | "qnt" | "qnbeta" | "qnf" => (1e-10, 0.0),
        "qtukey" => (1e-11, 0.0),
        "ptukey" => (5e-13, 1e-16),
        // GNU R 4.3 computes the Poisson density with the extended deviance ebd0; ry uses
        // Loader's bd0, which agrees to about 1e-14 at moderate arguments.
        "dpois" | "dpois_raw" | "dgamma" | "dchisq" | "dnbinom_mu" | "dnchisq" => (2e-13, 0.0),
        "pgamma" | "pchisq" | "ppois" => (2e-13, 0.0),
        // qbeta solves pbeta(x) = p by its own Newton iteration rather than GNU R's AS 109
        // based one; both converge to the root up to pbeta's rounding.
        "qbeta" => (1e-12, 0.0),
        // qf = (1/qbeta - 1) df2/df1 magnifies the last-place choice of a qbeta near one by
        // df2/df1 (1 + qf df1/df2)^2.
        "qf" => {
            let ratio = arguments[2] / arguments[1];
            let magnification = ratio * (1.0 + expected / ratio).powi(2) / expected.abs();
            (1e-12f64.max(8e-16 * magnification), 0.0)
        }
        _ => (1e-13, 0.0),
    }
}

/// Quantiles beyond the double range: where the true quantile underflows below the smallest
/// normal number, GNU R's qbeta reports a placeholder (DBL_MIN/2 or DBL_MIN/4) and qf the
/// matching huge value, while ry reports 0 and infinity.
fn beyond_range(name: &str, expected: f64, actual: f64) -> bool {
    let tiny = |value: f64| value.abs() < f64::MIN_POSITIVE;
    let huge = |value: f64| value.abs() > 1e300;
    matches!(name, "qbeta" | "qf")
        && ((tiny(expected) && tiny(actual)) || (huge(expected) && huge(actual)))
}

fn parse(text: &str) -> f64 {
    match text {
        "NA" => f64::from_bits(NA_REAL_BITS),
        "NaN" => f64::NAN,
        "Inf" => f64::INFINITY,
        "-Inf" => f64::NEG_INFINITY,
        _ => text
            .parse()
            .unwrap_or_else(|error| panic!("bad number {text:?}: {error}")),
    }
}

fn is_na(value: f64) -> bool {
    value.is_nan() && (value.to_bits() & 0xffff_ffff) == 1954
}

/// Whether `actual` matches GNU R's `expected`. `NA` must stay `NA` and `NaN` stay `NaN`,
/// except where the arguments hold both: which payload an operation on two NaNs keeps
/// depends on the order the compiler chose for its operands.
fn agrees(expected: f64, actual: f64, arguments: &[f64], (relative, absolute): (f64, f64)) -> bool {
    if expected.is_nan() || actual.is_nan() {
        let mixed = arguments.iter().any(|value| is_na(*value))
            && arguments
                .iter()
                .any(|value| value.is_nan() && !is_na(*value));
        return expected.is_nan() && actual.is_nan() && (mixed || is_na(expected) == is_na(actual));
    }
    if expected.is_infinite() || actual.is_infinite() {
        return expected == actual;
    }
    (expected - actual).abs() <= relative * expected.abs().max(actual.abs()) + absolute
}

/// The functions whose upper tail GNU R computes as one minus a long double sum: on the log
/// scale its rounding shows as large relative differences, so they are compared as
/// probabilities.
fn compare_as_probability(name: &str) -> bool {
    matches!(name, "pnt" | "dnt" | "pnchisq" | "pnf" | "pnbeta")
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn rmathtest(r: &R) {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    let library = BUILT.get_or_init(|| build_package("rmathtest", Headers::Ry));
    r.load(library);
}

fn strings(r: &R, values: &[&str]) -> Sexp {
    let vector = r.keep(unsafe { (r.api().Rf_allocVector)(STRSXP, values.len() as isize) });
    for (index, value) in values.iter().enumerate() {
        let element =
            unsafe { (r.api().Rf_mkCharLenCE)(value.as_ptr().cast(), value.len() as c_int, 0) };
        unsafe { (r.api().SET_STRING_ELT)(vector, index as isize, element) };
    }
    vector
}

fn reals(r: &R, values: &[f64]) -> Sexp {
    let vector = r.keep(unsafe { (r.api().Rf_allocVector)(REALSXP, values.len() as isize) });
    if !values.is_empty() {
        unsafe {
            std::slice::from_raw_parts_mut((r.api().REAL)(vector), values.len())
                .copy_from_slice(values)
        };
    }
    vector
}

struct Case {
    name: String,
    arguments: [f64; 6],
    expected: f64,
}

fn read_cases() -> Vec<Case> {
    let text =
        std::fs::read_to_string(data_dir().join("rmath_expected.tsv")).expect("rmath_expected.tsv");
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 8, "malformed case {line:?}");
            let mut arguments = [0.0; 6];
            for (slot, field) in arguments.iter_mut().zip(&fields[1..7]) {
                *slot = parse(field);
            }
            Case {
                name: fields[0].to_owned(),
                arguments,
                expected: parse(fields[7]),
            }
        })
        .collect()
}

#[test]
fn rmath_agrees_with_gnu_r() {
    let r = r();
    rmathtest(&r);
    let cases = read_cases();
    let names: Vec<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    let arguments: Vec<f64> = cases.iter().flat_map(|case| case.arguments).collect();
    let names = strings(&r, &names);
    let arguments = reals(&r, &arguments);
    let result = r
        .call("rmath_eval", &[names, arguments])
        .expect("rmath_eval");
    let actual = r.reals(result);

    // RMATH_EXAMPLES=n lists up to n failing cases per function instead of 4.
    let examples = std::env::var("RMATH_EXAMPLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4);
    let mut failures: BTreeMap<&str, (usize, usize, Vec<String>)> = BTreeMap::new();
    for (case, &actual) in cases.iter().zip(&actual) {
        let entry = failures.entry(&case.name).or_default();
        entry.1 += 1;
        let tolerance = tolerance(&case.name, &case.arguments, case.expected);
        let as_probability = compare_as_probability(&case.name)
            && case.expected <= 0.0
            && actual <= 0.0
            && agrees(
                case.expected.exp(),
                actual.exp(),
                &case.arguments,
                tolerance,
            );
        if !agrees(case.expected, actual, &case.arguments, tolerance)
            && !beyond_range(&case.name, case.expected, actual)
            && !as_probability
        {
            entry.0 += 1;
            if entry.2.len() < examples {
                entry.2.push(format!(
                    "  {}({:?}) = {actual:e}, GNU R {:e}",
                    case.name, case.arguments, case.expected
                ));
            }
        }
    }
    if std::env::var_os("RMATH_REPORT").is_some() {
        // The largest relative difference per function, to see how much of its tolerance
        // each one uses.
        let mut worst: BTreeMap<&str, f64> = BTreeMap::new();
        for (case, &actual) in cases.iter().zip(&actual) {
            if case.expected.is_finite() && actual.is_finite() && case.expected != actual {
                let difference =
                    (case.expected - actual).abs() / case.expected.abs().max(actual.abs());
                let entry = worst.entry(&case.name).or_default();
                *entry = entry.max(difference);
            }
        }
        for (name, difference) in &worst {
            eprintln!("{name:>20} {difference:.1e}");
        }
    }
    let report: Vec<String> = failures
        .iter()
        .filter(|(_, (failed, _, _))| *failed > 0)
        .map(|(name, (failed, total, examples))| {
            format!(
                "{name}: {failed} of {total} differ\n{}",
                examples.join("\n")
            )
        })
        .collect();
    assert!(
        report.is_empty(),
        "{} functions disagree with GNU R:\n{}",
        report.len(),
        report.join("\n")
    );
}

#[test]
fn random_streams_match_gnu_r() {
    let r = r();
    rmathtest(&r);
    let text =
        std::fs::read_to_string(data_dir().join("rmath_streams.tsv")).expect("rmath_streams.tsv");
    let mut failures = Vec::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 4, "malformed stream {line:?}");
        let seed: c_int = fields[1].parse().expect("a seed");
        let arguments: Vec<f64> = fields[2]
            .split(',')
            .filter(|field| !field.is_empty())
            .map(parse)
            .collect();
        let expected: Vec<f64> = fields[3].split(',').map(parse).collect();
        unsafe { (r.api().ry_set_seed)(seed) };
        let name = r.string(fields[0]);
        let arguments = reals(&r, &arguments);
        let count = r.integer(25);
        let drawn = r
            .call("rmath_random", &[name, arguments, count])
            .unwrap_or_else(|error| panic!("{}: {error}", fields[0]));
        let actual = r.reals(drawn);
        let same = actual.len() == expected.len()
            && actual
                .iter()
                .zip(&expected)
                .all(|(a, e)| a.to_bits() == e.to_bits() || (a.is_nan() && e.is_nan()));
        if !same {
            failures.push(format!(
                "{}({}) with seed {seed}:\n  ry    {actual:?}\n  GNU R {expected:?}",
                fields[0], fields[2]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} streams differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn header_constants_match_gnu_r() {
    let r = r();
    rmathtest(&r);
    let constants = r.call("rmath_constants", &[]).expect("rmath_constants");
    let expected = [
        std::f64::consts::LN_2,
        0.398_942_280_401_432_7,
        0.918_938_533_204_672_8,
        std::f64::consts::LOG10_2,
        std::f64::consts::FRAC_PI_2,
        32f64.sqrt(),
        0.225_791_352_644_727_43,
        2.0 * std::f64::consts::PI,
    ];
    assert_eq!(r.reals(constants), expected);
}

#[test]
fn discrete_densities_warn_about_non_integer_x() {
    let r = r();
    rmathtest(&r);
    let names = strings(&r, &["dbinom", "dpois", "choose"]);
    let arguments = reals(
        &r,
        &[
            2.5, 5.0, 0.5, 0.0, 0.0, 0.0, //
            1.5, 3.0, 0.0, 0.0, 0.0, 0.0, //
            5.0, 2.5, 0.0, 0.0, 0.0, 0.0,
        ],
    );
    let result = r
        .call("rmath_eval", &[names, arguments])
        .expect("rmath_eval");
    assert_eq!(r.reals(result), vec![0.0, 0.0, 10.0]);
    assert_eq!(
        r.warnings(),
        vec![
            "non-integer x = 2.500000".to_owned(),
            "non-integer x = 1.500000".to_owned(),
            "'k' (2.50) must be integer, rounded to 2".to_owned(),
        ]
    );
}

#[test]
fn rmultinom_rejects_probabilities_that_do_not_sum_to_one() {
    let r = r();
    rmathtest(&r);
    unsafe { (r.api().ry_set_seed)(1) };
    let name = r.string("rmultinom");
    let arguments = reals(&r, &[10.0, 0.5, 0.2]);
    let count = r.integer(1);
    let error = r
        .call("rmath_random", &[name, arguments, count])
        .expect_err("probabilities summing to 0.7");
    assert!(
        error.contains("rbinom: probability sum should be 1, but is 0.7"),
        "{error}"
    );
}

/// Re-records the expectations with the installed GNU R and checks that the committed files
/// are what it produces, so a changed case list or a changed GNU R cannot go unnoticed.
#[test]
fn expectations_are_current() {
    if !gnu_r_available() {
        return;
    }
    let committed =
        std::fs::read_to_string(data_dir().join("rmath_expected.tsv")).expect("rmath_expected.tsv");
    let version = Command::new("Rscript")
        .args(["-e", "cat(R.version$major, R.version$minor, sep = '.')"])
        .output()
        .expect("Rscript");
    let version = String::from_utf8_lossy(&version.stdout).into_owned();
    if !committed.starts_with(&format!("# GNU R {version};")) {
        eprintln!("the expectations come from another GNU R than {version}; not re-recording");
        return;
    }
    let output = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rmath-expectations");
    std::fs::create_dir_all(&output).expect("an output directory");
    let status = Command::new("Rscript")
        .arg(data_dir().join("rmath_cases.R"))
        .arg(&output)
        .output()
        .expect("Rscript");
    assert!(
        status.status.success(),
        "rmath_cases.R failed:\n{}",
        String::from_utf8_lossy(&status.stderr)
    );
    for file in ["rmath_expected.tsv", "rmath_streams.tsv"] {
        let fresh = std::fs::read_to_string(output.join(file)).expect("a fresh file");
        let committed = std::fs::read_to_string(data_dir().join(file)).expect("a committed file");
        assert!(
            fresh == committed,
            "{file} is stale: run `Rscript crates/libr/tests/data/rmath_cases.R`"
        );
    }
}
