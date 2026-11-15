//! The renderer's fuzz contract, which the in-tree harness (`tests/test_fuzz.rs`) runs.

use {
    crate::render::{
        Affine, Clip, Definition, Drawing, Extend, Fill, FillRule, Format, Item, Mark, Operator,
        Outline, Page, Pattern, Rect, Segment, Stop, Stroke, circle_outline, render_files,
    },
    syntax::testing::Entropy,
};

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
                for file in first {
                    check_file_structure(format, file, drawing.pages.len());
                }
            }
            (Err(first), Err(second)) => assert_eq!(first.to_string(), second.to_string()),
            _ => panic!("rendering succeeded only once"),
        }
    }
}

/// Checks that `file` is a well-formed file of `format`, as a reader would parse it: a PDF's
/// cross-reference table points at its objects and its page tree counts every page, an SVG's
/// tags nest, a PNG decodes, and the other formats carry their framing and sizes.
fn check_file_structure(format: Format, file: &[u8], pages: usize) {
    let text = String::from_utf8_lossy(file);
    match format {
        Format::Pdf => {
            assert!(file.starts_with(b"%PDF-"), "a PDF starts with its header");
            assert!(text.trim_end().ends_with("%%EOF"), "a PDF ends with %%EOF");
            let start = text.rfind("startxref").expect("a PDF names its xref table");
            let offset: usize = text[start + "startxref".len()..]
                .split_whitespace()
                .next()
                .and_then(|offset| offset.parse().ok())
                .expect("startxref gives an offset");
            assert!(
                file.get(offset..)
                    .is_some_and(|rest| rest.starts_with(b"xref")),
                "startxref points at the xref table"
            );
            // The lossy text replaces binary stream bytes, so offsets index the bytes.
            let table = String::from_utf8_lossy(&file[offset..]);
            let mut lines = table.lines().skip(1);
            let header = lines.next().expect("the xref table has a subsection");
            let mut header = header
                .split_whitespace()
                .map(|number| number.parse::<usize>());
            let (Some(Ok(first)), Some(Ok(count))) = (header.next(), header.next()) else {
                panic!("the xref subsection header is two numbers");
            };
            for (number, entry) in (first..first + count).zip(lines) {
                let mut fields = entry.split_whitespace();
                let (Some(position), Some(_), Some(kind)) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    panic!("xref entry {number} has three fields: {entry:?}");
                };
                if kind != "n" {
                    continue;
                }
                let position: usize = position.parse().expect("an xref offset is a number");
                let object = format!("{number} 0 obj");
                assert!(
                    file.get(position..)
                        .is_some_and(|rest| rest.starts_with(object.as_bytes())),
                    "xref entry {number} points at `{object}`"
                );
            }
            let counted = text
                .split("/Count ")
                .nth(1)
                .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|count| count.parse::<usize>().ok());
            assert_eq!(counted, Some(pages), "the page tree counts every page");
        }
        Format::Svg => {
            assert!(text.contains("<svg"), "an SVG has an svg element");
            let mut open: Vec<&str> = Vec::new();
            let mut rest = text.as_ref();
            while let Some(start) = rest.find('<') {
                rest = &rest[start + 1..];
                let end = rest.find('>').expect("every tag closes");
                let tag = &rest[..end];
                rest = &rest[end + 1..];
                if tag.starts_with('?') || tag.starts_with('!') {
                    continue;
                }
                let name = |tag: &'_ str| {
                    tag.split(|c: char| c.is_whitespace() || c == '/')
                        .find(|part| !part.is_empty())
                        .unwrap_or("")
                        .to_owned()
                };
                if let Some(closing) = tag.strip_prefix('/') {
                    let opened = open.pop().expect("a closing tag has an opening one");
                    assert_eq!(name(opened), name(closing), "SVG tags nest");
                } else if !tag.ends_with('/') {
                    open.push(tag);
                }
            }
            assert!(open.is_empty(), "every SVG element closes: {open:?}");
            for (index, _) in text.match_indices('&') {
                let entity = &text[index..text.len().min(index + 8)];
                assert!(
                    ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;", "&#"]
                        .iter()
                        .any(|known| entity.starts_with(known)),
                    "an SVG escapes every ampersand: {entity:?}"
                );
            }
        }
        Format::Png => {
            let pixmap = tiny_skia::Pixmap::decode_png(file).expect("the PNG decodes");
            assert!(
                pixmap.width() > 0 && pixmap.height() > 0,
                "a PNG has pixels"
            );
        }
        Format::Bmp => {
            assert!(file.starts_with(b"BM"), "a BMP starts with BM");
            let field = |at: usize| {
                file.get(at..at + 4)
                    .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    .expect("the BMP header is whole")
            };
            assert_eq!(
                field(2) as usize,
                file.len(),
                "a BMP's header gives its size"
            );
            assert!(
                (field(10) as usize) < file.len(),
                "a BMP's pixels start inside it"
            );
        }
        Format::Tiff => assert!(
            file.starts_with(b"II*\0") || file.starts_with(b"MM\0*"),
            "a TIFF starts with its byte order and magic number"
        ),
        Format::Jpeg => {
            assert!(file.starts_with(&[0xff, 0xd8]), "a JPEG starts with SOI");
            assert!(file.ends_with(&[0xff, 0xd9]), "a JPEG ends with EOI");
        }
        Format::Postscript { onefile, .. } => {
            assert!(
                file.starts_with(b"%!PS-Adobe-"),
                "PostScript starts with its header"
            );
            assert!(
                text.trim_end().ends_with("%%EOF"),
                "PostScript ends with %%EOF"
            );
            let showpages = text.matches("showpage").count();
            let expected = if onefile { pages } else { 1 };
            assert!(showpages >= expected, "every page is shown");
            if onefile {
                let declared = text
                    .lines()
                    .find_map(|line| line.strip_prefix("%%Pages: "))
                    .and_then(|count| count.trim().parse::<usize>().ok());
                assert_eq!(declared, Some(pages), "%%Pages counts every page");
            }
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
