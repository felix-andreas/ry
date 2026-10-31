//! Package binaries built by GNU R's `R CMD SHLIB`, against GNU R's headers and linked to GNU
//! R's `-lR`, load into ry's `libR` and run. This is the ABI promise: prebuilt binaries work.
//! `RY_SKIP_GNU_R=1` skips these on a machine without R.

mod harness;

use harness::*;
use std::process::Command;

#[test]
fn brio_built_against_gnu_r_headers_loads_into_ry() {
    if !gnu_r_available() {
        return;
    }
    let library = build_package("brio", Headers::GnuR);
    let needed = Command::new("readelf")
        .arg("-d")
        .arg(&library)
        .output()
        .expect("readelf");
    assert!(
        String::from_utf8_lossy(&needed.stdout).contains("[libR.so]"),
        "R CMD SHLIB links GNU R's libR, which ry's soname must satisfy"
    );
    let r = r();
    r.load(&library);
    let path = temporary_file("brio-gnu.txt");
    let path_value = r.string(&path.to_string_lossy());
    let text = r.string("built by R CMD SHLIB");
    r.call("brio_write_file", &[text, path_value])
        .expect("brio_write_file");
    let read = r
        .call("brio_read_file", &[path_value])
        .expect("brio_read_file");
    assert_eq!(r.strings(read), vec![Some("built by R CMD SHLIB".into())]);
    brio_round_trip(&r);
}

#[test]
fn rytest_built_against_gnu_r_headers_runs() {
    if !gnu_r_available() {
        return;
    }
    let library = build_package("rytest", Headers::GnuR);
    let r = r();
    r.load(&library);
    let values = r.keep(unsafe { (r.api().Rf_allocVector)(REALSXP, 4) });
    unsafe { std::slice::from_raw_parts_mut((r.api().REAL)(values), 4) }
        .copy_from_slice(&[1.0, 2.5, 3.0, 4.5]);
    let sum = r.call("rytest_sum", &[values]).expect("rytest_sum");
    assert_eq!(r.reals(sum), vec![11.0]);
    let error = r
        .call("rytest_error", &[r.nil()])
        .expect_err("STRING_ELT on NULL is an error");
    assert!(
        error.contains("STRING_ELT() can only be applied to a 'character vector', not a 'NULL'"),
        "{error}"
    );
}
