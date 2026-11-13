//! Deterministic fuzzing of the renderer, with no dependency on base R or libR. Random drawings
//! must render without panicking, identically twice, and as files a reader can parse. The
//! contract lives in `graphics::testing`.
//!
//! `FUZZ_ITERS` raises the budget.

use {
    graphics::{
        render::{
            Clip, Drawing, FillRule, Format, Item, Mark, Page, Rect, Segment, Stroke, render_files,
        },
        testing::check_render_drawing,
    },
    syntax::testing::{SplitMix64, for_each_seed},
};

/// Random drawings, with nested and dangling definitions and degenerate geometry, render to
/// every format without panicking, identically twice, and as files a reader can parse.
#[test]
fn fuzz_render_drawing() {
    for_each_seed(40, |seed| check_render_drawing(&mut SplitMix64(seed)));
}

/// The fuzzer's first find: tiny-skia asserts that a stroked cubic's points are finite, so a path
/// with a NaN point is skipped rather than handed to it.
#[test]
fn render_skips_a_path_with_a_non_finite_point() {
    let drawing = Drawing {
        pages: vec![Page {
            background: 0xffff_ffff,
            items: vec![Item {
                clip: Clip::Rect(Rect {
                    x0: 0.0,
                    x1: 1.0,
                    y0: 0.0,
                    y1: 1.0,
                }),
                mask: None,
                mark: Mark::Path {
                    outline: vec![
                        Segment::Move([f64::NAN, 0.18]),
                        Segment::Cubic([1.46, 0.55], [2.29, 0.62], [0.98, 1.17]),
                    ],
                    fill: None,
                    rule: FillRule::Winding,
                    stroke: Some(Stroke {
                        colour: 0xff44_2266,
                        lwd: 1.5,
                        lty: 0,
                        lend: 2,
                        ljoin: 1,
                        lmitre: 4.0,
                    }),
                },
            }],
        }],
        definitions: Vec::new(),
    };
    let files = render_files(Format::Png, &drawing, [1.0, 1.0], 24.0, "", 75);
    assert!(files.is_ok_and(|files| files.len() == 1));
}
