//! Native routines of corpus packages, called under ry with the arguments GNU R called them
//! with, return what they returned under GNU R. `tests/data/corpus_calls/record.R` records
//! each case's arguments and result in `cases.rds`; this test reads it with ry's own
//! unserializer, loads the package's shared object (with its dependencies) and calls the
//! routine through `.Call`, comparing the result with `identical()`.
//!
//! Each case runs in a child process, so a crash in one routine is reported, not fatal. Without
//! a corpus library the test reports that and passes.

mod harness;

use harness::*;
use std::{
    ffi::CString,
    path::Path,
    process::Command,
};

const CHILD_CASE: &str = "RY_CORPUS_CALL_CASE";

fn cases_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/corpus_calls/cases.rds")
}

/// A named element of a list.
fn field(r: &R, list: Sexp, name: &str) -> Sexp {
    let names = unsafe { (r.api().Rf_getAttrib)(list, r.symbol("names")) };
    let position = r
        .strings(names)
        .iter()
        .position(|candidate| candidate.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("the case has no {name}"));
    r.element(list, position)
}

fn text(r: &R, value: Sexp) -> String {
    r.strings(value)[0].clone().expect("a string")
}

fn read_cases(r: &R) -> Sexp {
    let path = CString::new(cases_path().to_string_lossy().into_owned()).expect("a path");
    r.keep(unsafe { (r.api().ry_read_rds)(path.as_ptr()) })
}

/// The child: replays one case and prints its outcome.
fn child(index: usize) {
    let r = r();
    let cases = read_cases(&r);
    let case = r.element(cases, index);
    let package = text(&r, field(&r, case, "package"));
    let packages = scan(&corpus_libraries());
    // Base packages' native code is the interpreter's (it backs their R code), and a routine
    // of another package needs only its own library and the libraries it calls into.
    for name in load_order(&package, &packages) {
        if packages[&name].base {
            continue;
        }
        let path = r.string(&packages[&name].library.to_string_lossy());
        if let Err(error) = r.eval("dyn.load", &[(None, path)])
            && name == package
        {
            println!("RY-OUTCOME\tload\t{}", error.replace(['\n', '\t'], " "));
            return;
        }
    }
    // Packages that call R_forceSymbols forbid lookup by name, as GNU R does, so the routine
    // is passed as its NativeSymbolInfo, the way the package's own R code passes it.
    let routine_name = CString::new(text(&r, field(&r, case, "routine"))).expect("a name");
    let dll_name = CString::new(text(&r, field(&r, case, "dll"))).expect("a name");
    let routine =
        r.keep(unsafe { (r.api().ry_native_symbol)(routine_name.as_ptr(), dll_name.as_ptr()) });
    let arguments = field(&r, case, "arguments");
    let mut call_arguments = vec![(None, routine)];
    for index in 0..r.length(arguments) {
        call_arguments.push((None, r.quote(r.element(arguments, index))));
    }
    let expected = field(&r, case, "result");
    match r.eval(".Call", &call_arguments) {
        Ok(result) => {
            let same = unsafe { (r.api().R_compute_identical)(result, expected, 16) } != 0;
            if same {
                println!("RY-OUTCOME\tsame\t");
            } else {
                let shown = |value: Sexp| {
                    if r.type_of(value) == STRSXP {
                        format!("{:?}", r.strings(value))
                    } else {
                        format!("type {}", r.type_of(value))
                    }
                };
                println!(
                    "RY-OUTCOME\tdifferent\tgot {} expected {}",
                    shown(result),
                    shown(expected)
                );
            }
        }
        Err(error) => println!("RY-OUTCOME\terror\t{}", error.replace(['\n', '\t'], " ")),
    }
}

#[test]
fn corpus_routines_return_what_they_return_under_gnu_r() {
    if let Ok(index) = std::env::var(CHILD_CASE) {
        child(index.parse().expect("a case index"));
        return;
    }
    if scan(&corpus_libraries()).is_empty() {
        eprintln!("no corpus library found; run `just corpus` to install it");
        return;
    }
    let (count, labels) = {
        let r = r();
        let cases = read_cases(&r);
        let labels: Vec<String> = (0..r.length(cases))
            .map(|index| {
                let case = r.element(cases, index);
                format!(
                    "{}::{}",
                    text(&r, field(&r, case, "package")),
                    text(&r, field(&r, case, "routine"))
                )
            })
            .collect();
        (labels.len(), labels)
    };
    let executable = std::env::current_exe().expect("the test executable");
    let mut same = 0;
    let mut report = String::from("routine\toutcome\tdetail\n");
    for (index, label) in labels.iter().enumerate() {
        let output = Command::new(&executable)
            .args([
                "corpus_routines_return_what_they_return_under_gnu_r",
                "--exact",
                "--nocapture",
            ])
            .env(CHILD_CASE, index.to_string())
            .output()
            .expect("run the child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (outcome, detail) = stdout
            .lines()
            .find_map(|line| {
                let mut fields = line.splitn(3, '\t');
                (fields.next() == Some("RY-OUTCOME")).then(|| {
                    (
                        fields.next().unwrap_or("").to_owned(),
                        fields.next().unwrap_or("").to_owned(),
                    )
                })
            })
            .unwrap_or_else(|| {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let last = stderr.lines().rev().find(|line| !line.trim().is_empty());
                ("crashed".to_owned(), format!("{} {}", output.status, last.unwrap_or("")))
            });
        if outcome == "same" {
            same += 1;
        }
        report.push_str(&format!("{label}\t{outcome}\t{detail}\n"));
    }
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("corpus-calls.tsv");
    std::fs::write(&path, &report).expect("the report");
    eprintln!("{same} of {count} recorded routine calls match GNU R; report: {}", path.display());
    eprint!("{report}");
    // A routine whose library loads must give GNU R's answer. A library that does not load
    // yet is the corpus binary test's to report.
    let wrong: Vec<&str> = report
        .lines()
        .filter(|line| {
            let outcome = line.split('\t').nth(1).unwrap_or("");
            matches!(outcome, "different" | "crashed" | "error")
        })
        .collect();
    assert!(wrong.is_empty(), "routines that disagree with GNU R:\n{}", wrong.join("\n"));
}
