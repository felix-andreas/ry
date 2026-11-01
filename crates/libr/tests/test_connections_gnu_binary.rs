//! The connection scenarios with the test package built by GNU R's `R CMD SHLIB`, against GNU
//! R's `R_ext/Connections.h`: a binary that fills in GNU R's `struct Rconn` works in ry. It is
//! its own test binary because the package's routines share names with the ry-built copy.
//! `RY_SKIP_GNU_R=1` skips it on a machine without R.

mod connection_scenarios;
mod harness;

use connection_scenarios::*;
use harness::*;

#[test]
fn a_package_built_against_gnu_r_runs_the_scenarios() {
    if !gnu_r_available() {
        return;
    }
    let library = build_package("ryconn", Headers::GnuR);
    let r = r_with_base();
    r.load(&library);
    let expected = std::fs::read_to_string(expectation_path()).expect("the recorded GNU R output");
    let actual = run_in_ry(&r, "gnu-binary");
    assert_same_transcript(&expected, &actual, "the GNU-built package in ry");
}
