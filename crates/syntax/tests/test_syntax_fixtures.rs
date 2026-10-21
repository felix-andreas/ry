use std::path::Path;

#[test]
fn syntax_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/syntax");
    syntax::testing::run_fixture_suite(&suite, &|source| syntax::parse(source).debug_dump());
}

/// tree-sitter-r's test corpus, with ry's own trees as the expectations.
#[test]
fn tree_sitter_r_corpus_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tsr");
    syntax::testing::run_fixture_suite(&suite, &|source| syntax::parse(source).debug_dump());
}
