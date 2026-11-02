//! A bounded pass of the plot gallery: a seeded sample of `crates/oracle/gallery` figures
//! rendered by GNU R and by ry's runtime on each available device, and compared pixel by
//! pixel within the device's tolerance (`oracle gallery` runs the whole gallery).
//!
//! `FUZZ_SEED` (default 0) picks the sample and `FUZZ_ITERS` sets its size. The pass skips
//! when GNU R or ry's runtime is missing, so build them first with
//! `cargo build -p ry-lang -p libr`. Set `R_LIBS` to the corpus library (`.corpus/library`)
//! to include the figures that need ragg, PerformanceAnalytics or ggplot2.

use {
    oracle::{
        gallery::{self, Options, available_devices, render_report},
        gnu_r_available, ry_runtime_available,
    },
    std::path::PathBuf,
    syntax::testing::fuzz_iterations,
};

#[test]
fn sampled_gallery_figures_match_gnu_r() {
    if !gnu_r_available() {
        eprintln!("skipped: GNU R is not installed");
        return;
    }
    if !ry_runtime_available() {
        eprintln!("skipped: ry's runtime is not built; run `cargo build -p ry-lang -p libr`");
        return;
    }
    let seed: u64 = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    // ragg rasterizes identically under both engines, so it is the sharpest check; the
    // built-in png device stands in when ragg is not installed.
    let devices = available_devices(&["agg".to_owned()]);
    let devices = if devices.is_empty() {
        vec!["png".to_owned()]
    } else {
        devices
    };
    let output: PathBuf =
        std::env::temp_dir().join(format!("ry-gallery-test-{}-{seed}", std::process::id()));
    let options = Options {
        seed,
        count: Some(fuzz_iterations(4)),
        figures: Vec::new(),
        devices: devices.clone(),
        repeats: 1,
        output: output.clone(),
    };
    let renders = match gallery::run(&options) {
        Ok(renders) => renders,
        Err(error) => panic!("the gallery did not run: {error}"),
    };
    let failing: Vec<String> = renders
        .iter()
        .filter(|render| render.fails())
        .map(|render| render.figure.label())
        .collect();
    if failing.is_empty() {
        if let Err(error) = std::fs::remove_dir_all(&output) {
            eprintln!("could not remove {}: {error}", output.display());
        }
        return;
    }
    panic!(
        "{}\nreplay with: cargo run -p oracle -- gallery --figures {} --devices {} --output {}",
        render_report(&renders),
        failing.join(","),
        devices.join(","),
        output.display(),
    );
}
