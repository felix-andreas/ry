//! Range-formatting property tests: the invariant battery over every R source
//! in the repository.

use format::check_range_format_invariants as check_invariants;

#[test]
fn fixture_sources_hold_range_invariants() {
    for (id, source) in syntax::testing::fixture_case_sources() {
        std::panic::catch_unwind(|| check_invariants(&source))
            .unwrap_or_else(|_| panic!("fixture case `{id}` broke a range-format invariant"));
    }
}

#[test]
fn legacy_corpus_holds_range_invariants() {
    for (id, source) in syntax::testing::legacy_corpus_sources() {
        std::panic::catch_unwind(|| check_invariants(&source))
            .unwrap_or_else(|_| panic!("legacy corpus case `{id}` broke a range-format invariant"));
    }
}
