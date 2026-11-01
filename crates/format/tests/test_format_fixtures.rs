use {
    format::{Config, format},
    std::path::Path,
    syntax::testing::run_fixture_suite,
};

fn render(source: &str) -> String {
    match format(source, Config::default()) {
        Ok(output) => match format(&output, Config::default()) {
            Ok(second) if second != output => {
                format!("NOT IDEMPOTENT\n---- first ----\n{output}\n---- second ----\n{second}")
            }
            Ok(_) => output,
            Err(error) => format!("REFORMAT ERROR: {error}\n---- first ----\n{output}"),
        },
        Err(error) => format!("ERROR: {error}"),
    }
}

#[test]
fn format_fixtures() {
    let suite = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/format");
    run_fixture_suite(&suite, &render);
}
