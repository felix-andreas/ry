//! The interpreter's fuzz contracts, shared by the in-tree harness (`tests/test_fuzz.rs`) and the
//! coverage-guided `fuzz/` targets so both check exactly the same properties. Each one runs
//! without base R or libR, on an interpreter it installs on first use.

use {
    crate::{
        deparse, eval,
        graphics_render::{
            Affine, Clip, Definition, Drawing, Extend, Fill, FillRule, Format, Item, Mark,
            Operator, Outline, Page, Pattern, Rect, Segment, Stop, Stroke, circle_outline,
            render_files,
        },
        lower, serialize,
        unwind::Unwind,
        values,
    },
    heap::{Heap, object::Sexp},
    std::sync::{Mutex, Once},
    syntax::testing::Entropy,
};

/// Runs `body` with the interpreter installed. The interpreter is single-threaded, so callers on
/// different threads take turns.
pub fn with_interpreter<T>(body: impl FnOnce() -> T) -> T {
    static INSTALL: Once = Once::new();
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    INSTALL.call_once(|| {
        let heap: &'static mut Heap = Box::leak(Box::new(Heap::new()));
        // SAFETY: the heap lives for the rest of the process, and the lock above keeps every
        // use of the interpreter on one thread at a time.
        unsafe {
            crate::install(heap, crate::default_hooks());
            crate::interp().show_error_messages = false;
        }
    });
    body()
}

/// Lowering any text must not panic: it either fails with a parse error or produces expressions,
/// and every expression it produces must deparse without panicking.
pub fn check_lowering_input(text: &str) {
    with_interpreter(|| {
        let mark = crate::heap().handle_mark();
        // SAFETY: the interpreter is installed.
        unsafe {
            if let Ok(expressions) = lower::parse_text(text, "<fuzz>") {
                for index in 0..values::length(expressions) {
                    deparse::deparse_lines(
                        expressions.element(index),
                        60,
                        deparse::options::KEEP_INTEGER,
                    );
                }
            }
        }
        crate::heap().release_handles(mark);
    });
}

/// Reading any byte stream as a serialized object must fail with an R error or succeed, never
/// panic.
pub fn check_unserialize_input(bytes: &[u8]) {
    with_interpreter(|| {
        let mark = crate::heap().handle_mark();
        // SAFETY: the interpreter is installed.
        unsafe {
            drop(serialize::unserialize(bytes, values::nil()));
        }
        crate::interp().frames.clear();
        crate::interp().foreign_calls.clear();
        crate::heap().release_handles(mark);
    });
}

/// Generates a program over `a` and `b` and checks that the tier-0 bytecode agrees with the AST
/// evaluator on it: the program's value evaluated directly must equal its value as a closure body
/// on each of the first three calls, the second and later of which run compiled bytecode.
pub fn check_bytecode_agreement(entropy: &mut impl Entropy) {
    let body = program(entropy);
    let arguments = [
        "3L",
        "-2.5",
        "NA",
        "TRUE",
        "2147483647L",
        "0",
        "NA_integer_",
        "[3L]",
        "[-2.5, 1]",
        "[TRUE]",
    ];
    let a = arguments[entropy.below(arguments.len())];
    let b = arguments[entropy.below(arguments.len())];
    with_interpreter(|| {
        // SAFETY: the interpreter is installed.
        let (direct, compiled) = unsafe { outcomes(&body, a, b) };
        for (call, outcome) in compiled.iter().enumerate() {
            assert_eq!(
                &direct,
                outcome,
                "call {}: `{body}` with a = {a}, b = {b}",
                call + 1
            );
        }
    });
}

unsafe fn outcomes(body: &str, a: &str, b: &str) -> (String, Vec<String>) {
    unsafe {
        // The direct run assigns its locals globally, which the closure would then read, so
        // every run starts from the same globals.
        let reset = format!("a <- {a}\nb <- {b}\ns <- 7L\nk <- 0L\ni <- 0L");
        run(&reset);
        let direct = run(&format!("local_value <- {{ {body} }}\nlocal_value"));
        run(&format!("f <- function(a, b) {{ {body} }}"));
        let compiled = (0..3)
            .map(|_| {
                run(&reset);
                run(&format!("f({a}, {b})"))
            })
            .collect();
        (direct, compiled)
    }
}

/// The deparsed value of the last expression of `text` in the global environment, or `error`.
unsafe fn run(text: &str) -> String {
    unsafe {
        let mark = crate::heap().handle_mark();
        let outcome = (|| -> Result<Sexp, Unwind> {
            let expressions = lower::parse_text(text, "<fuzz>")?;
            let mut value = values::nil();
            for index in 0..values::length(expressions) {
                value = eval::eval(expressions.element(index), values::global_env())?;
            }
            Ok(value)
        })();
        let rendered = match outcome {
            Ok(value) => {
                deparse::deparse_lines(value, 60, deparse::options::KEEP_INTEGER).join("\n")
            }
            Err(_) => "error".to_owned(),
        };
        crate::interp().frames.clear();
        crate::interp().foreign_calls.clear();
        crate::heap().release_handles(mark);
        rendered
    }
}

/// Assignments, a loop, and a final expression.
fn program(entropy: &mut impl Entropy) -> String {
    let mut statements = vec![format!("s <- {}", expression(entropy, 2))];
    match entropy.below(4) {
        0 => statements.push(format!(
            "for (i in 1:{}) {{ s <- s + {}; if ({}) next; if ({}) break }}",
            entropy.below(5) + 1,
            expression(entropy, 2),
            condition(entropy),
            condition(entropy)
        )),
        1 => statements.push(format!(
            "k <- 0L\nwhile (k < {}L) {{ k <- k + 1L; s <- {} }}",
            entropy.below(4),
            expression(entropy, 2)
        )),
        2 => statements.push(format!(
            "s <- if ({}) {} else {}",
            condition(entropy),
            expression(entropy, 2),
            expression(entropy, 2)
        )),
        _ => {}
    }
    statements.push(expression(entropy, 3));
    statements.join("\n")
}

fn expression(entropy: &mut impl Entropy, depth: usize) -> String {
    if depth == 0 || entropy.below(3) == 0 {
        return leaf(entropy);
    }
    match entropy.below(5) {
        0 => format!("-({})", expression(entropy, depth - 1)),
        1 => format!(
            "(if ({}) {} else {})",
            condition(entropy),
            expression(entropy, depth - 1),
            expression(entropy, depth - 1)
        ),
        _ => {
            let operators = ["+", "-", "*", "/", "^", "%/%", "%%", "==", "<", ">=", "!="];
            format!(
                "({} {} {})",
                expression(entropy, depth - 1),
                operators[entropy.below(operators.len())],
                expression(entropy, depth - 1)
            )
        }
    }
}

fn condition(entropy: &mut impl Entropy) -> String {
    match entropy.below(4) {
        0 => format!("{} && {}", comparison(entropy), comparison(entropy)),
        1 => format!("{} || {}", comparison(entropy), comparison(entropy)),
        _ => comparison(entropy),
    }
}

fn comparison(entropy: &mut impl Entropy) -> String {
    let operators = ["<", ">", "==", "!=", "<=", ">="];
    format!(
        "{} {} {}",
        leaf(entropy),
        operators[entropy.below(operators.len())],
        leaf(entropy)
    )
}

const LEAVES: [&str; 12] = [
    "a", "b", "s", "1L", "2", "0.5", "NA", "TRUE", "-1L", "1e308", "0L", "3L",
];

fn leaf(entropy: &mut impl Entropy) -> String {
    LEAVES[entropy.below(LEAVES.len())].to_owned()
}

/// Renders a random drawing to every format twice: rendering must not panic, must give the same
/// bytes both times, and must give one file per page except for PDF and single-file
/// PostScript. Drawings nest definitions, refer to definitions that are missing or of the wrong
/// kind, and carry degenerate geometry such as empty outlines, zero-size tiles, coincident
/// gradient ends and non-finite coordinates.
pub fn check_render_drawing(entropy: &mut impl Entropy) {
    let drawing = drawing(entropy);
    let size = [0.5 + entropy.below(4) as f64, 0.5 + entropy.below(4) as f64];
    let formats = [
        Format::Pdf,
        Format::Svg,
        Format::Png,
        Format::Bmp,
        Format::Tiff,
        Format::Jpeg,
        Format::Postscript {
            onefile: true,
            paper: None,
            landscape: false,
        },
        Format::Postscript {
            onefile: false,
            paper: None,
            landscape: true,
        },
    ];
    for format in formats {
        let render = || render_files(format, &drawing, size, 24.0, "fuzz", 75);
        let first = render();
        let second = render();
        match (&first, &second) {
            (Ok(first), Ok(second)) => {
                assert!(first == second, "rendering is not deterministic");
                let single = matches!(
                    format,
                    Format::Pdf | Format::Postscript { onefile: true, .. }
                );
                let expected = if single { 1 } else { drawing.pages.len() };
                assert_eq!(first.len(), expected, "one file per page");
            }
            (Err(first), Err(second)) => assert_eq!(first.to_string(), second.to_string()),
            _ => panic!("rendering succeeded only once"),
        }
    }
}

fn drawing(entropy: &mut impl Entropy) -> Drawing {
    let mut definitions = Vec::new();
    for _ in 0..entropy.below(6) {
        let definition = definition(entropy, definitions.len());
        definitions.push(definition);
    }
    let pages = (0..1 + entropy.below(2))
        .map(|_| Page {
            background: colour(entropy),
            items: items(entropy, definitions.len(), 6),
        })
        .collect();
    Drawing { pages, definitions }
}

/// A definition that refers only to the `defined` definitions before it, as the device builds
/// them, except that a reference is now and then out of range.
fn definition(entropy: &mut impl Entropy, defined: usize) -> Definition {
    match entropy.below(6) {
        0 => Definition::Pattern(Pattern::Linear {
            start: point(entropy),
            end: point(entropy),
            stops: stops(entropy),
            extend: extend(entropy),
        }),
        1 => Definition::Pattern(Pattern::Radial {
            start: point(entropy),
            start_radius: number(entropy).abs(),
            end: point(entropy),
            end_radius: number(entropy).abs(),
            stops: stops(entropy),
            extend: extend(entropy),
        }),
        2 => {
            let [x0, y0] = point(entropy);
            let [x1, y1] = point(entropy);
            Definition::Pattern(Pattern::Tiling {
                rect: Rect {
                    x0: x0.min(x1),
                    x1: x0.max(x1),
                    y0: y0.min(y1),
                    y1: y0.max(y1),
                },
                items: items(entropy, defined, 3),
                extend: extend(entropy),
            })
        }
        3 => Definition::ClipPath {
            outline: outline(entropy),
            rule: rule(entropy),
        },
        4 => Definition::Mask {
            luminance: entropy.chance(1, 2),
            items: items(entropy, defined, 3),
        },
        _ => Definition::Group {
            destination: items(entropy, defined, 2),
            operator: Operator::from_code(1 + entropy.below(25) as i32).unwrap_or(Operator::Over),
            source: items(entropy, defined, 3),
        },
    }
}

fn items(entropy: &mut impl Entropy, defined: usize, most: usize) -> Vec<Item> {
    (0..entropy.below(most + 1))
        .map(|_| Item {
            clip: if entropy.chance(1, 3) {
                Clip::Path(reference(entropy, defined))
            } else {
                let [x0, y0] = point(entropy);
                let [x1, y1] = point(entropy);
                Clip::Rect(Rect {
                    x0: x0.min(x1),
                    x1: x0.max(x1),
                    y0: y0.min(y1),
                    y1: y0.max(y1),
                })
            },
            mask: entropy.chance(1, 4).then(|| reference(entropy, defined)),
            mark: mark(entropy, defined),
        })
        .collect()
}

fn mark(entropy: &mut impl Entropy, defined: usize) -> Mark {
    match entropy.below(5) {
        0 => Mark::Text {
            origin: point(entropy),
            text: ["", "ry", "Wg ÄÖ", "a\u{301}"][entropy.below(4)].to_owned(),
            points: 1.0 + entropy.below(40) as f64,
            font: 1 + entropy.below(5) as i32,
            rotation: entropy.below(360) as f64,
            hadj: entropy.below(3) as f64 / 2.0,
            colour: colour(entropy),
        },
        4 => {
            let (columns, rows) = (1 + entropy.below(4), 1 + entropy.below(4));
            Mark::Image {
                pixels: (0..columns * rows).map(|_| colour(entropy)).collect(),
                columns,
                rows,
                placement: Affine([
                    number(entropy),
                    number(entropy) / 4.0,
                    number(entropy) / 4.0,
                    number(entropy),
                    number(entropy),
                    number(entropy),
                ]),
                interpolate: entropy.chance(1, 2),
            }
        }
        1 => Mark::Group {
            group: reference(entropy, defined),
            transform: Affine([
                number(entropy),
                number(entropy) / 4.0,
                number(entropy) / 4.0,
                number(entropy),
                number(entropy),
                number(entropy),
            ]),
        },
        _ => Mark::Path {
            outline: outline(entropy),
            fill: match entropy.below(3) {
                0 => None,
                1 => Some(Fill::Colour(colour(entropy))),
                _ => Some(Fill::Pattern(reference(entropy, defined))),
            },
            rule: rule(entropy),
            stroke: entropy.chance(1, 2).then(|| Stroke {
                colour: colour(entropy),
                lwd: entropy.below(8) as f64 / 2.0,
                lty: [0, 0x44, 0x1343, 0xffff_ffff][entropy.below(4)],
                lend: 1 + entropy.below(3) as i32,
                ljoin: 1 + entropy.below(3) as i32,
                lmitre: 1.0 + entropy.below(10) as f64,
            }),
        },
    }
}

fn outline(entropy: &mut impl Entropy) -> Outline {
    match entropy.below(4) {
        0 => circle_outline(point(entropy), number(entropy).abs()),
        _ => {
            let mut outline = vec![Segment::Move(point(entropy))];
            for _ in 0..entropy.below(6) {
                outline.push(match entropy.below(5) {
                    0 => Segment::Cubic(point(entropy), point(entropy), point(entropy)),
                    1 => Segment::Close,
                    2 => Segment::Move(point(entropy)),
                    _ => Segment::Line(point(entropy)),
                });
            }
            outline
        }
    }
}

fn stops(entropy: &mut impl Entropy) -> Vec<Stop> {
    let mut stops: Vec<Stop> = (0..entropy.below(4))
        .map(|_| Stop {
            offset: entropy.below(5) as f64 / 4.0,
            colour: colour(entropy),
        })
        .collect();
    stops.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    stops
}

/// A definition index, now and then one past the end or far beyond it.
fn reference(entropy: &mut impl Entropy, defined: usize) -> usize {
    match entropy.below(8) {
        0 => defined,
        1 => usize::MAX,
        _ => entropy.below(defined.max(1)),
    }
}

/// A coordinate in inches, usually on the page and now and then far off it or not finite.
fn point(entropy: &mut impl Entropy) -> [f64; 2] {
    [number(entropy), number(entropy)]
}

fn number(entropy: &mut impl Entropy) -> f64 {
    match entropy.below(20) {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => -1e9,
        3 => 0.0,
        _ => entropy.below(400) as f64 / 100.0 - 0.5,
    }
}

fn colour(entropy: &mut impl Entropy) -> u32 {
    let alpha = [0, 0x80, 0xff][entropy.below(3)];
    (alpha << 24) | (entropy.next_u64() as u32 & 0x00ff_ffff)
}

fn extend(entropy: &mut impl Entropy) -> Extend {
    [Extend::Pad, Extend::Repeat, Extend::Reflect, Extend::None][entropy.below(4)]
}

fn rule(entropy: &mut impl Entropy) -> FillRule {
    if entropy.chance(1, 2) {
        FillRule::Winding
    } else {
        FillRule::EvenOdd
    }
}
