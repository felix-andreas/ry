//! What a graphics device draws and the files it writes. A built-in device records each page
//! as a list of marks in inches from the device's bottom-left corner, together with the
//! patterns, clipping paths, masks and compositing groups the marks refer to, and closing the
//! device renders the pages into its file format: PDF, PostScript and SVG as vector graphics,
//! PNG, BMP, TIFF and JPEG as rasterized pixels.

use {
    crate::{
        eval,
        graphics::{Colour, parse_colour_spec, text_width_ems},
        graphics_engine::{
            DevDesc, GEContext, Rboolean, TRANSPARENT_WHITE, VERSION_GLYPHS, VERSION_GROUP,
            add_device_named, check_device_available, create_device, fail, fail_with,
        },
        unwind::{R, error},
        values::{self, length, nil},
    },
    heap::object::{Sexp, SexpType},
    std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
        ffi::{CStr, c_char, c_int, c_uint},
        io::Write,
        rc::Rc,
        sync::OnceLock,
    },
};

/// Everything a device has drawn: its pages, and the definitions their marks refer to by
/// index. Definitions are only ever appended, so an index stays valid after R releases the
/// reference it was handed, and pages rendered at close still find what they used.
pub struct Drawing {
    pub pages: Vec<Page>,
    pub definitions: Vec<Definition>,
}

/// One page of a device: its background and the marks drawn on it, in drawing order.
pub struct Page {
    pub background: Colour,
    pub items: Vec<Item>,
}

/// A mark with the clipping region and mask in effect when it was drawn.
pub struct Item {
    pub clip: Clip,
    /// The index of a [`Definition::Mask`].
    pub mask: Option<usize>,
    pub mark: Mark,
}

/// A clipping region. Setting a clipping rectangle replaces a clipping path and the other way
/// round, as on GNU R's devices, so a region is one or the other.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Clip {
    Rect(Rect),
    /// The index of a [`Definition::ClipPath`].
    Path(usize),
}

/// A rectangle in inches, `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
}

pub enum Mark {
    /// An outline, filled by `rule` and stroked; an open subpath is filled as if closed.
    Path {
        outline: Outline,
        fill: Option<Fill>,
        rule: FillRule,
        stroke: Option<Stroke>,
    },
    /// One line of text whose baseline starts at `origin` once it is moved back along the
    /// baseline by `hadj` of its width, rotated `rotation` degrees anticlockwise.
    Text {
        origin: [f64; 2],
        text: String,
        points: f64,
        font: i32,
        rotation: f64,
        hadj: f64,
        colour: Colour,
    },
    /// A [`Definition::Group`] drawn through `transform`.
    Group { group: usize, transform: Affine },
    /// A raster image of `columns` by `rows` pixels, row by row from the top, drawn into the
    /// parallelogram `placement` maps the unit square onto, with the image's bottom-left corner
    /// at `(0, 0)`. `interpolate` asks for smooth scaling rather than hard pixel edges.
    Image {
        pixels: Vec<Colour>,
        columns: usize,
        rows: usize,
        placement: Affine,
        interpolate: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Fill {
    Colour(Colour),
    /// The index of a [`Definition::Pattern`].
    Pattern(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillRule {
    Winding,
    EvenOdd,
}

/// A path as subpaths of straight and cubic Bézier segments, each starting with a move.
pub type Outline = Vec<Segment>;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Segment {
    Move([f64; 2]),
    Line([f64; 2]),
    Cubic([f64; 2], [f64; 2], [f64; 2]),
    Close,
}

/// An affine map `[a, b, c, d, e, f]` taking `(x, y)` to `(a x + c y + e, b x + d y + f)`, in
/// PDF's matrix order.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Affine(pub [f64; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    /// This map followed by `after`.
    pub fn then(self, after: Affine) -> Affine {
        let [a, b, c, d, e, f] = self.0;
        let [p, q, r, s, t, u] = after.0;
        Affine([
            p * a + r * b,
            q * a + s * b,
            p * c + r * d,
            q * c + s * d,
            p * e + r * f + t,
            q * e + s * f + u,
        ])
    }

    pub fn apply(self, [x, y]: [f64; 2]) -> [f64; 2] {
        let [a, b, c, d, e, f] = self.0;
        [a * x + c * y + e, b * x + d * y + f]
    }

    pub(crate) fn invert(self) -> Option<Affine> {
        let [a, b, c, d, e, f] = self.0;
        let determinant = a * d - b * c;
        if determinant == 0.0 || !determinant.is_finite() {
            return None;
        }
        let [a, b, c, d] = [d, -b, -c, a].map(|value| value / determinant);
        Some(Affine([a, b, c, d, -(a * e + c * f), -(b * e + d * f)]))
    }

    /// How much the map scales lengths, on average over directions.
    fn scale(self) -> f64 {
        let [a, b, c, d, _, _] = self.0;
        (a * d - b * c).abs().sqrt()
    }

    fn skia(self) -> tiny_skia::Transform {
        let [a, b, c, d, e, f] = self.0.map(|value| value as f32);
        tiny_skia::Transform::from_row(a, b, c, d, e, f)
    }
}

/// What a pattern, clipping path, mask or group reference stands for.
pub enum Definition {
    Pattern(Pattern),
    ClipPath {
        outline: Outline,
        rule: FillRule,
    },
    /// Marks whose opacity, or luminance when `luminance`, masks what is drawn under it.
    Mask {
        luminance: bool,
        items: Vec<Item>,
    },
    /// Marks drawn onto `destination` with `operator`, painted where the group is used.
    Group {
        destination: Vec<Item>,
        operator: Operator,
        source: Vec<Item>,
    },
}

pub enum Pattern {
    Linear {
        start: [f64; 2],
        end: [f64; 2],
        stops: Vec<Stop>,
        extend: Extend,
    },
    /// Circles interpolated from the start circle to the end circle.
    Radial {
        start: [f64; 2],
        start_radius: f64,
        end: [f64; 2],
        end_radius: f64,
        stops: Vec<Stop>,
        extend: Extend,
    },
    /// Marks drawn in `rect`, repeated across the plane.
    Tiling {
        rect: Rect,
        items: Vec<Item>,
        extend: Extend,
    },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stop {
    pub offset: f64,
    pub colour: Colour,
}

/// What a gradient paints beyond its ends: the end colours, the gradient again, the gradient
/// mirrored, or nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Extend {
    Pad,
    Repeat,
    Reflect,
    None,
}

/// A compositing operator, numbered from 1 as `R_GE_composite*` numbers them: the Porter-Duff
/// operators, then `add` and `saturate`, then the blend modes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Operator {
    Clear = 1,
    Source,
    Over,
    In,
    Out,
    Atop,
    Dest,
    DestOver,
    DestIn,
    DestOut,
    DestAtop,
    Xor,
    Add,
    Saturate,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

const OPERATORS: [Operator; 25] = [
    Operator::Clear,
    Operator::Source,
    Operator::Over,
    Operator::In,
    Operator::Out,
    Operator::Atop,
    Operator::Dest,
    Operator::DestOver,
    Operator::DestIn,
    Operator::DestOut,
    Operator::DestAtop,
    Operator::Xor,
    Operator::Add,
    Operator::Saturate,
    Operator::Multiply,
    Operator::Screen,
    Operator::Overlay,
    Operator::Darken,
    Operator::Lighten,
    Operator::ColorDodge,
    Operator::ColorBurn,
    Operator::HardLight,
    Operator::SoftLight,
    Operator::Difference,
    Operator::Exclusion,
];

impl Operator {
    pub(crate) fn from_code(code: c_int) -> Option<Operator> {
        usize::try_from(code - 1)
            .ok()
            .and_then(|index| OPERATORS.get(index).copied())
    }

    /// The blend mode's name in PDF, `None` for an operator other than a blend mode.
    fn blend_name(self) -> Option<&'static str> {
        Some(match self {
            Operator::Multiply => "Multiply",
            Operator::Screen => "Screen",
            Operator::Overlay => "Overlay",
            Operator::Darken => "Darken",
            Operator::Lighten => "Lighten",
            Operator::ColorDodge => "ColorDodge",
            Operator::ColorBurn => "ColorBurn",
            Operator::HardLight => "HardLight",
            Operator::SoftLight => "SoftLight",
            Operator::Difference => "Difference",
            Operator::Exclusion => "Exclusion",
            _ => return None,
        })
    }

    /// tiny-skia has every operator but `saturate`, which `draw_items` composites by hand; it
    /// maps to `add` here only so the match is total.
    fn skia(self) -> tiny_skia::BlendMode {
        use tiny_skia::BlendMode;
        match self {
            Operator::Clear => BlendMode::Clear,
            Operator::Source => BlendMode::Source,
            Operator::Over => BlendMode::SourceOver,
            Operator::In => BlendMode::SourceIn,
            Operator::Out => BlendMode::SourceOut,
            Operator::Atop => BlendMode::SourceAtop,
            Operator::Dest => BlendMode::Destination,
            Operator::DestOver => BlendMode::DestinationOver,
            Operator::DestIn => BlendMode::DestinationIn,
            Operator::DestOut => BlendMode::DestinationOut,
            Operator::DestAtop => BlendMode::DestinationAtop,
            Operator::Xor => BlendMode::Xor,
            Operator::Add | Operator::Saturate => BlendMode::Plus,
            Operator::Multiply => BlendMode::Multiply,
            Operator::Screen => BlendMode::Screen,
            Operator::Overlay => BlendMode::Overlay,
            Operator::Darken => BlendMode::Darken,
            Operator::Lighten => BlendMode::Lighten,
            Operator::ColorDodge => BlendMode::ColorDodge,
            Operator::ColorBurn => BlendMode::ColorBurn,
            Operator::HardLight => BlendMode::HardLight,
            Operator::SoftLight => BlendMode::SoftLight,
            Operator::Difference => BlendMode::Difference,
            Operator::Exclusion => BlendMode::Exclusion,
        }
    }
}

/// How a line is drawn. `lwd` 1 is 1/96 inch, and each hex digit of `lty` is a dash or gap
/// length in units of the line width, the first dash in the low bits.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stroke {
    pub colour: Colour,
    pub lwd: f64,
    pub lty: u32,
    pub lend: i32,
    pub ljoin: i32,
    pub lmitre: f64,
}

/// The file formats ry renders.
#[derive(Clone, Copy, PartialEq)]
pub enum Format {
    Pdf,
    Svg,
    Png,
    Bmp,
    Tiff,
    Jpeg,
    /// The page is centred on `paper`, and turned a quarter anticlockwise when `landscape`;
    /// without paper, the paper is the page itself. `onefile = FALSE` writes each page to its
    /// own file as EPS.
    Postscript {
        onefile: bool,
        paper: Option<Paper>,
        landscape: bool,
    },
}

impl Format {
    fn is_raster(self) -> bool {
        matches!(
            self,
            Format::Png | Format::Bmp | Format::Tiff | Format::Jpeg
        )
    }
}

/// A sheet of paper, named as PostScript's `%%DocumentMedia` comment names it, `size` inches
/// across.
#[derive(Clone, Copy, PartialEq)]
pub struct Paper {
    pub name: &'static str,
    pub size: [f64; 2],
}

/// How deeply definitions may nest when they are rendered: a group used inside a pattern used
/// inside a mask, and so on. A reference cycle cannot arise from R code, which only sees a
/// reference once its definition is complete, but the bound keeps a malformed drawing finite.
/// It bounds depth only: the raster writer renders a definition at each use, so a chain of
/// groups that each use the one before twice costs twice as much per link.
const MAX_NESTING: usize = 32;

/// Renders a device `size` inches across into one file for PDF and PostScript (unless it was
/// opened with `onefile = FALSE`), or into one file per page for the others, naming page `n`
/// by filling `file`'s `%d` conversion with `n`. Raster formats use `resolution` pixels per
/// inch, and JPEG compresses at `quality`.
pub fn write_pages(
    format: Format,
    file: &str,
    drawing: &Drawing,
    size: [f64; 2],
    resolution: f64,
    title: &str,
    quality: u8,
) -> std::io::Result<()> {
    let files = render_files(format, drawing, size, resolution, title, quality)?;
    for (index, bytes) in files.iter().enumerate() {
        std::fs::write(file_for_page(file, index + 1), bytes)?;
    }
    Ok(())
}

/// The contents of the files [`write_pages`] writes, in page order.
pub fn render_files(
    format: Format,
    drawing: &Drawing,
    size: [f64; 2],
    resolution: f64,
    title: &str,
    quality: u8,
) -> std::io::Result<Vec<Vec<u8>>> {
    match format {
        Format::Pdf => return Ok(vec![pdf_document(drawing, size, title)]),
        Format::Postscript {
            onefile: true,
            paper,
            landscape,
        } => {
            let document =
                postscript_document(&drawing.pages, size, paper, landscape, false, title);
            return Ok(vec![document]);
        }
        _ => {}
    }
    let mut files = Vec::with_capacity(drawing.pages.len());
    for page in &drawing.pages {
        files.push(match format {
            Format::Svg => svg_document(page, &drawing.definitions, size).into_bytes(),
            Format::Png | Format::Bmp | Format::Tiff | Format::Jpeg => {
                let pixels = rasterize(page, &drawing.definitions, size, resolution)
                    .ok_or_else(|| std::io::Error::other("the page is too large to rasterize"))?;
                match format {
                    Format::Png => pixels
                        .encode_png()
                        .map_err(|failure| std::io::Error::other(failure.to_string()))?,
                    Format::Bmp => bmp(&pixels),
                    Format::Jpeg => jpeg(&pixels, resolution, quality)?,
                    _ => tiff(&pixels, resolution),
                }
            }
            Format::Postscript {
                onefile: false,
                paper,
                landscape,
            } => {
                let page = std::slice::from_ref(page);
                postscript_document(page, size, paper, landscape, true, title)
            }
            Format::Postscript { onefile: true, .. } | Format::Pdf => Vec::new(),
        });
    }
    Ok(files)
}

/// A file name with its `%d` (or `%03d`) conversion filled in with `page`, `%%` unescaped,
/// and a leading `~/` expanded.
pub fn file_for_page(name: &str, page: usize) -> String {
    let mut result = String::with_capacity(name.len());
    let mut characters = name.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '%' {
            result.push(character);
            continue;
        }
        let mut spec = String::new();
        while let Some(next) = characters.peek().copied() {
            if next.is_ascii_digit() {
                spec.push(next);
                characters.next();
            } else {
                break;
            }
        }
        match characters.next() {
            Some('d') => {
                // GNU R formats the name into a buffer of `PATH_MAX` bytes, so no longer
                // width can matter.
                let width: usize = spec.parse().map_or(0, |width: usize| width.min(4096));
                result.push_str(&format!("{page:0width$}"));
            }
            Some('%') => result.push('%'),
            Some(other) => {
                result.push('%');
                result.push_str(&spec);
                result.push(other);
            }
            None => result.push('%'),
        }
    }
    if let Some(rest) = result.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    result
}

fn channels(colour: Colour) -> [u8; 4] {
    colour.to_le_bytes()
}

fn alpha(colour: Colour) -> u8 {
    channels(colour)[3]
}

fn visible(stroke: &Option<Stroke>) -> Option<&Stroke> {
    stroke
        .as_ref()
        .filter(|stroke| alpha(stroke.colour) > 0 && stroke.lty != u32::MAX && stroke.lwd > 0.0)
}

/// A fill that paints something: a pattern, or a colour that is not fully transparent.
fn painted(fill: &Option<Fill>) -> Option<Fill> {
    fill.filter(|fill| match fill {
        Fill::Colour(colour) => alpha(*colour) > 0,
        Fill::Pattern(_) => true,
    })
}

/// A stroke's dash pattern in inches, empty for a solid line.
fn dashes(stroke: &Stroke) -> Vec<f64> {
    let unit = stroke.lwd.max(1.0) / 96.0;
    let mut pattern = Vec::new();
    let mut rest = stroke.lty;
    while rest != 0 && pattern.len() < 8 {
        pattern.push(f64::from(rest & 0xF) * unit);
        rest >>= 4;
    }
    pattern
}

/// A closed polygon through `points`, or a polyline when not `closed`.
pub fn polygon_outline(points: &[[f64; 2]], closed: bool) -> Outline {
    let mut outline = Vec::with_capacity(points.len() + 1);
    for (index, point) in points.iter().enumerate() {
        outline.push(if index == 0 {
            Segment::Move(*point)
        } else {
            Segment::Line(*point)
        });
    }
    if closed && !points.is_empty() {
        outline.push(Segment::Close);
    }
    outline
}

/// A circle as four cubic Bézier arcs, starting from its rightmost point.
pub fn circle_outline([x, y]: [f64; 2], radius: f64) -> Outline {
    let handle = radius * 0.552_284_749_830_793_4;
    vec![
        Segment::Move([x + radius, y]),
        Segment::Cubic(
            [x + radius, y + handle],
            [x + handle, y + radius],
            [x, y + radius],
        ),
        Segment::Cubic(
            [x - handle, y + radius],
            [x - radius, y + handle],
            [x - radius, y],
        ),
        Segment::Cubic(
            [x - radius, y - handle],
            [x - handle, y - radius],
            [x, y - radius],
        ),
        Segment::Cubic(
            [x + handle, y - radius],
            [x + radius, y - handle],
            [x + radius, y],
        ),
        Segment::Close,
    ]
}

fn rect_outline(rect: Rect) -> Outline {
    polygon_outline(
        &[
            [rect.x0, rect.y0],
            [rect.x1, rect.y0],
            [rect.x1, rect.y1],
            [rect.x0, rect.y1],
        ],
        true,
    )
}

/// A gradient's stops sorted, clamped to `[0, 1]` and covering it, so every renderer can rely
/// on stops at both ends. `Extend::None` paints nothing beyond the ends, so transparent stops
/// right at each end stand in for it in renderers that only pad.
fn gradient_stops(stops: &[Stop], extend: Extend) -> Vec<Stop> {
    let mut sorted: Vec<Stop> = stops
        .iter()
        .filter(|stop| stop.offset.is_finite())
        .map(|stop| Stop {
            offset: stop.offset.clamp(0.0, 1.0),
            colour: stop.colour,
        })
        .collect();
    sorted.sort_by(|left, right| left.offset.total_cmp(&right.offset));
    let (Some(first), Some(last)) = (sorted.first().copied(), sorted.last().copied()) else {
        return vec![
            Stop {
                offset: 0.0,
                colour: TRANSPARENT_WHITE as Colour,
            },
            Stop {
                offset: 1.0,
                colour: TRANSPARENT_WHITE as Colour,
            },
        ];
    };
    if first.offset > 0.0 {
        sorted.insert(
            0,
            Stop {
                offset: 0.0,
                colour: first.colour,
            },
        );
    }
    if last.offset < 1.0 {
        sorted.push(Stop {
            offset: 1.0,
            colour: last.colour,
        });
    }
    if extend == Extend::None {
        let clear = |stop: Stop| Stop {
            offset: stop.offset,
            colour: stop.colour & 0x00FF_FFFF,
        };
        if let Some(first) = sorted.first().copied() {
            sorted.insert(0, clear(first));
        }
        if let Some(last) = sorted.last().copied() {
            sorted.push(clear(last));
        }
    }
    sorted
}

// PDF.

const POINTS_PER_INCH: f64 = 72.0;

/// The PDF base fonts for R's font faces 1 to 5. The first four use WinAnsiEncoding.
const PDF_FONTS: [&str; 5] = [
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Symbol",
];

/// A PDF document of a drawing's pages, each `size` inches, with the standard Helvetica fonts.
/// A document without pages is still valid, which is what a device nothing was drawn on writes.
fn pdf_document(drawing: &Drawing, size: [f64; 2], title: &str) -> Vec<u8> {
    // Objects 1 to 4 are the catalog, the page tree, the shared resources and the document
    // information; each page then takes a page object and a content stream, and the
    // definitions the pages use follow.
    let page_count = drawing.pages.len();
    let first_page = 5;
    let mut writer = PdfWriter::new(
        &drawing.definitions,
        size,
        first_page + 2 * page_count,
        false,
    );
    let contents: Vec<Vec<u8>> = drawing
        .pages
        .iter()
        .map(|page| writer.page(page).into_bytes())
        .collect();
    let mut objects: Vec<Vec<u8>> = Vec::with_capacity(4 + 2 * page_count);
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = (0..page_count)
        .map(|index| format!("{} 0 R", first_page + 2 * index))
        .collect();
    objects.push(
        format!(
            "<< /Type /Pages /Kids [{}] /Count {page_count} >>",
            kids.join(" ")
        )
        .into_bytes(),
    );
    objects.push(writer.resources().into_bytes());
    objects.push(
        format!(
            "<< /Title ({}) /Producer (ry) /Creator (R) >>",
            pdf_string(title)
        )
        .into_bytes(),
    );
    let [width, height] = size.map(|inches| inches * POINTS_PER_INCH);
    for (index, content) in contents.into_iter().enumerate() {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width:.2} {height:.2}] \
                 /Resources 3 0 R /Contents {} 0 R >>",
                first_page + 2 * index + 1
            )
            .into_bytes(),
        );
        objects.push(pdf_stream("", &content));
    }
    objects.extend(writer.objects);
    let mut document = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(document.len());
        document.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        document.extend_from_slice(object);
        document.extend_from_slice(b"\nendobj\n");
    }
    let table = document.len();
    document.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        document.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    document.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{table}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    document
}

/// A compressed stream object whose dictionary holds `entries` besides its length and filter.
/// The stream stays uncompressed if compression fails, which writing into memory never does.
fn pdf_stream(entries: &str, content: &[u8]) -> Vec<u8> {
    let (body, filter) = match deflate(content) {
        Ok(compressed) => (compressed, " /Filter /FlateDecode"),
        Err(_) => (content.to_vec(), ""),
    };
    let mut stream =
        format!("<< {entries} /Length {}{filter} >>\nstream\n", body.len()).into_bytes();
    stream.extend_from_slice(&body);
    stream.extend_from_slice(b"\nendstream");
    stream
}

fn deflate(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = flate2::write::ZlibEncoder::new(
        Vec::with_capacity(bytes.len() / 4),
        flate2::Compression::default(),
    );
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// Writes content streams, and the objects for the definitions they use, which all pages and
/// definitions share through one resource dictionary, object 3. With `postscript` set, the
/// content is the page body of a PostScript document instead, whose prolog defines PDF's
/// operators; images are then written inline, since PostScript has no objects.
struct PdfWriter<'a> {
    definitions: &'a [Definition],
    size: [f64; 2],
    postscript: bool,
    /// The objects after the pages', the first numbered `first_object`.
    objects: Vec<Vec<u8>>,
    first_object: usize,
    /// The graphics state setting each pair of stroke and fill opacities, by its number.
    opacities: BTreeMap<(u8, u8), usize>,
    blend_modes: BTreeSet<&'static str>,
    /// The resource name and object number of each definition written so far; a gradient's
    /// opacity mask is a resource of its own under the definition's name with a prefix.
    written: BTreeMap<(char, usize), usize>,
    /// The object number of each image written so far, also its resource name's suffix.
    images: Vec<usize>,
}

impl<'a> PdfWriter<'a> {
    fn new(
        definitions: &'a [Definition],
        size: [f64; 2],
        first_object: usize,
        postscript: bool,
    ) -> Self {
        PdfWriter {
            definitions,
            size,
            postscript,
            objects: Vec::new(),
            first_object,
            opacities: BTreeMap::new(),
            blend_modes: BTreeSet::new(),
            written: BTreeMap::new(),
            images: Vec::new(),
        }
    }

    /// The content stream of one page.
    fn page(&mut self, page: &Page) -> String {
        let mut out = String::new();
        if alpha(page.background) > 0 {
            let [red, green, blue, a] = channels(page.background);
            let state = self.opacity(255, a);
            out.push_str(&format!(
                "q /GS{state} gs {} rg 0 0 10000 10000 re f Q\n",
                pdf_rgb(red, green, blue)
            ));
        }
        self.items(&mut out, &page.items, 0);
        out
    }

    fn resources(&self) -> String {
        let fonts: Vec<String> = PDF_FONTS
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let encoding = if index < 4 {
                    " /Encoding /WinAnsiEncoding"
                } else {
                    ""
                };
                format!(
                    "/F{} << /Type /Font /Subtype /Type1 /BaseFont /{name}{encoding} >>",
                    index + 1
                )
            })
            .collect();
        let mut states: Vec<String> = self
            .opacities
            .iter()
            .map(|((stroke, fill), index)| {
                format!(
                    "/GS{index} << /Type /ExtGState /CA {:.3} /ca {:.3} >>",
                    f64::from(*stroke) / 255.0,
                    f64::from(*fill) / 255.0
                )
            })
            .collect();
        states.extend(
            self.blend_modes
                .iter()
                .map(|mode| format!("/BM{mode} << /Type /ExtGState /BM /{mode} >>")),
        );
        let mut patterns = Vec::new();
        let mut xobjects = Vec::new();
        for ((kind, index), object) in &self.written {
            let entry = format!("/{kind}{index} {object} 0 R");
            match kind {
                'P' => patterns.push(entry),
                'X' => xobjects.push(entry),
                _ => states.push(entry),
            }
        }
        xobjects.extend(
            self.images
                .iter()
                .map(|object| format!("/Im{object} {object} 0 R")),
        );
        format!(
            "<< /ProcSet [/PDF /Text /ImageC /ImageB] /Font << {} >> /ExtGState << {} >> /Pattern << {} >> \
             /XObject << {} >> >>",
            fonts.join(" "),
            states.join(" "),
            patterns.join(" "),
            xobjects.join(" ")
        )
    }

    fn opacity(&mut self, stroke: u8, fill: u8) -> usize {
        let next = self.opacities.len();
        *self.opacities.entry((stroke, fill)).or_insert(next)
    }

    /// Writes `items`, each run of items with the same clipping region and mask in its own
    /// graphics state, so the region and mask are set once per run.
    fn items(&mut self, out: &mut String, items: &[Item], depth: usize) {
        if depth > MAX_NESTING {
            return;
        }
        let mut current: Option<(Clip, Option<usize>)> = None;
        for item in items {
            if current != Some((item.clip, item.mask)) {
                if current.is_some() {
                    out.push_str("Q\n");
                }
                out.push_str("q ");
                match item.clip {
                    Clip::Rect(clip) => out.push_str(&format!(
                        "{:.2} {:.2} {:.2} {:.2} re W n\n",
                        pt(clip.x0),
                        pt(clip.y0),
                        pt(clip.x1 - clip.x0),
                        pt(clip.y1 - clip.y0)
                    )),
                    Clip::Path(index) => {
                        if let Some(Definition::ClipPath { outline, rule }) =
                            self.definitions.get(index)
                        {
                            pdf_outline(out, outline);
                            out.push_str(if *rule == FillRule::EvenOdd {
                                "W* n\n"
                            } else {
                                "W n\n"
                            });
                        }
                    }
                }
                if let Some(mask) = item.mask
                    && self.define('M', mask, depth)
                {
                    out.push_str(&format!("/M{mask} gs\n"));
                }
                current = Some((item.clip, item.mask));
            }
            self.mark(out, &item.mark, item.mask.is_some(), depth);
        }
        if current.is_some() {
            out.push_str("Q\n");
        }
    }

    /// Writes one mark in its own graphics state, so its opacity and colours do not reach the
    /// marks after it. `masked` says whether the item's mask is in force.
    fn mark(&mut self, out: &mut String, mark: &Mark, masked: bool, depth: usize) {
        match mark {
            Mark::Path {
                outline,
                fill,
                rule,
                stroke,
            } => {
                let fill = painted(fill);
                let stroke = visible(stroke);
                if (fill.is_none() && stroke.is_none()) || outline.is_empty() {
                    return;
                }
                let fill_alpha = match fill {
                    Some(Fill::Colour(colour)) => alpha(colour),
                    _ => 255,
                };
                let stroke_alpha = stroke.map_or(255, |stroke| alpha(stroke.colour));
                let state = self.opacity(stroke_alpha, fill_alpha);
                out.push_str("q\n");
                // A gradient's own opacity is a soft mask, which would replace the item's mask;
                // GNU R's pdf device sets the item's mask last, so the item's mask wins.
                if !masked
                    && let Some(Fill::Pattern(index)) = fill
                    && self.gradient_opacity(index)
                {
                    out.push_str(&format!("/A{index} gs\n"));
                }
                out.push_str(&format!("/GS{state} gs\n"));
                match fill {
                    Some(Fill::Colour(colour)) => {
                        let [red, green, blue, _] = channels(colour);
                        out.push_str(&format!("{} rg\n", pdf_rgb(red, green, blue)));
                    }
                    Some(Fill::Pattern(index)) => {
                        if self.define('P', index, depth) {
                            out.push_str(&format!("/Pattern cs /P{index} scn\n"));
                        }
                    }
                    None => {}
                }
                if let Some(stroke) = stroke {
                    let [red, green, blue, _] = channels(stroke.colour);
                    let dash: Vec<String> = dashes(stroke)
                        .iter()
                        .map(|length| format!("{:.2}", length * POINTS_PER_INCH))
                        .collect();
                    out.push_str(&format!(
                        "{} RG {:.2} w {} J {} j {:.2} M [{}] 0 d\n",
                        pdf_rgb(red, green, blue),
                        stroke.lwd * 0.75,
                        pdf_cap(stroke.lend),
                        pdf_join(stroke.ljoin),
                        stroke.lmitre,
                        dash.join(" ")
                    ));
                }
                pdf_outline(out, outline);
                let even_odd = *rule == FillRule::EvenOdd;
                out.push_str(match (fill.is_some(), stroke.is_some(), even_odd) {
                    (true, true, false) => "B\n",
                    (true, true, true) => "B*\n",
                    (true, false, false) => "f\n",
                    (true, false, true) => "f*\n",
                    _ => "S\n",
                });
                out.push_str("Q\n");
            }
            Mark::Text {
                origin,
                text,
                points,
                font,
                rotation,
                hadj,
                colour,
            } => {
                let [red, green, blue, a] = channels(*colour);
                if a == 0 || text.is_empty() {
                    return;
                }
                let state = self.opacity(255, a);
                out.push_str(&format!("q /GS{state} gs\n"));
                let face = if (1..=5).contains(font) { *font } else { 1 };
                let width = text_width_ems(text, face) * points / POINTS_PER_INCH;
                let (sin, cos) = rotation.to_radians().sin_cos();
                let x = origin[0] - hadj * width * cos;
                let y = origin[1] - hadj * width * sin;
                out.push_str(&format!(
                    "BT {} rg /F{face} 1 Tf {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} Tm ({}) Tj ET\n",
                    pdf_rgb(red, green, blue),
                    points * cos,
                    points * sin,
                    -points * sin,
                    points * cos,
                    pt(x),
                    pt(y),
                    pdf_string(text)
                ));
                out.push_str("Q\n");
            }
            Mark::Image {
                pixels,
                columns,
                rows,
                placement,
                interpolate,
            } => {
                let [a, b, c, d, e, f] = placement.0.map(pt);
                out.push_str(&format!("q {a:.4} {b:.4} {c:.4} {d:.4} {e:.2} {f:.2} cm\n"));
                if self.postscript {
                    postscript_image(out, pixels, *columns, *rows, *placement, *interpolate);
                } else {
                    let object = self.image(pixels, *columns, *rows, *interpolate);
                    out.push_str(&format!("/Im{object} Do\n"));
                }
                out.push_str("Q\n");
            }
            Mark::Group { group, transform } => {
                if !self.define('X', *group, depth) {
                    return;
                }
                let state = self.opacity(255, 255);
                let [a, b, c, d, e, f] = transform.0;
                out.push_str(&format!(
                    "q /GS{state} gs {a:.4} {b:.4} {c:.4} {d:.4} {:.2} {:.2} cm /X{group} Do Q\n",
                    pt(e),
                    pt(f)
                ));
            }
        }
    }

    /// Writes an image XObject, with a soft mask holding its alpha when any pixel is not
    /// opaque, and returns its object number.
    fn image(
        &mut self,
        pixels: &[Colour],
        columns: usize,
        rows: usize,
        interpolate: bool,
    ) -> usize {
        let interpolate = if interpolate {
            " /Interpolate true"
        } else {
            ""
        };
        let entries = |colour_space: &str| {
            format!(
                "/Type /XObject /Subtype /Image /Width {columns} /Height {rows} \
                 /ColorSpace {colour_space} /BitsPerComponent 8{interpolate}"
            )
        };
        let mask = if pixels.iter().all(|pixel| alpha(*pixel) == 255) {
            String::new()
        } else {
            let alphas: Vec<u8> = pixels.iter().map(|pixel| alpha(*pixel)).collect();
            let mask = self.add(pdf_stream(&entries("/DeviceGray"), &alphas));
            format!(" /SMask {mask} 0 R")
        };
        let colours: Vec<u8> = pixels
            .iter()
            .flat_map(|pixel| {
                let [red, green, blue, _] = channels(*pixel);
                [red, green, blue]
            })
            .collect();
        let object = self.add(pdf_stream(
            &format!("{}{mask}", entries("/DeviceRGB")),
            &colours,
        ));
        self.images.push(object);
        object
    }

    /// Writes the object a resource named `kind` and `index` stands for, unless it has been
    /// written: `P` a pattern, `X` a group's form, `M` a mask's graphics state. Whether the
    /// resource exists, which it does not when `index` names another kind of definition.
    fn define(&mut self, kind: char, index: usize, depth: usize) -> bool {
        if self.written.contains_key(&(kind, index)) {
            return true;
        }
        let definitions = self.definitions;
        let Some(definition) = definitions.get(index) else {
            return false;
        };
        let object = match (kind, definition) {
            ('P', Definition::Pattern(pattern)) => {
                let object = self.reserve((kind, index));
                let bytes = self.pattern(pattern, depth);
                self.fill(object, bytes);
                object
            }
            (
                'X',
                Definition::Group {
                    destination,
                    operator,
                    source,
                },
            ) => {
                let object = self.reserve((kind, index));
                let mut content = String::new();
                self.items(&mut content, destination, depth + 1);
                if let Some(mode) = operator.blend_name() {
                    self.blend_modes.insert(mode);
                    content.push_str(&format!("/BM{mode} gs\n"));
                }
                self.items(&mut content, source, depth + 1);
                let bytes = pdf_stream(
                    &self.form_entries("/S /Transparency /I true"),
                    content.as_bytes(),
                );
                self.fill(object, bytes);
                object
            }
            ('M', Definition::Mask { luminance, items }) => {
                let object = self.reserve((kind, index));
                let mut content = String::new();
                self.items(&mut content, items, depth + 1);
                let form = self.add(pdf_stream(
                    &self.form_entries("/S /Transparency /CS /DeviceRGB"),
                    content.as_bytes(),
                ));
                let subtype = if *luminance { "Luminosity" } else { "Alpha" };
                let bytes = format!(
                    "<< /Type /ExtGState /SMask << /Type /Mask /S /{subtype} /G {form} 0 R >> >>"
                )
                .into_bytes();
                self.fill(object, bytes);
                object
            }
            _ => return false,
        };
        self.written.insert((kind, index), object);
        true
    }

    /// The dictionary entries of a form XObject covering the page, a transparency group with
    /// `group` entries.
    fn form_entries(&self, group: &str) -> String {
        let [width, height] = self.size.map(|inches| inches * POINTS_PER_INCH);
        format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 {width:.2} {height:.2}] \
             /Group << {group} >> /Resources 3 0 R"
        )
    }

    /// Reserves the next object number for the resource `key`, recording it at once so a
    /// definition that refers to itself finds the reservation instead of recursing.
    fn reserve(&mut self, key: (char, usize)) -> usize {
        let object = self.add(Vec::new());
        self.written.insert(key, object);
        object
    }

    fn add(&mut self, bytes: Vec<u8>) -> usize {
        self.objects.push(bytes);
        self.first_object + self.objects.len() - 1
    }

    fn fill(&mut self, object: usize, bytes: Vec<u8>) {
        if let Some(slot) = self.objects.get_mut(object - self.first_object) {
            *slot = bytes;
        }
    }

    fn pattern(&mut self, pattern: &Pattern, depth: usize) -> Vec<u8> {
        match pattern {
            Pattern::Linear { .. } | Pattern::Radial { .. } => format!(
                "<< /Type /Pattern /PatternType 2 /Shading {} >>",
                pdf_shading(pattern, false)
            )
            .into_bytes(),
            Pattern::Tiling { rect, items, .. } => {
                let mut content = String::new();
                self.items(&mut content, items, depth + 1);
                pdf_stream(
                    &format!(
                        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
                         /BBox [{:.2} {:.2} {:.2} {:.2}] /XStep {:.2} /YStep {:.2} \
                         /Resources 3 0 R",
                        pt(rect.x0),
                        pt(rect.y0),
                        pt(rect.x1),
                        pt(rect.y1),
                        pt(rect.x1 - rect.x0),
                        pt(rect.y1 - rect.y0)
                    ),
                    content.as_bytes(),
                )
            }
        }
    }

    /// Whether the gradient at `index` has stops that are not opaque, after writing the soft
    /// mask, resource `A` `index`, that paints its opacity.
    fn gradient_opacity(&mut self, index: usize) -> bool {
        let definitions = self.definitions;
        let Some(Definition::Pattern(pattern)) = definitions.get(index) else {
            return false;
        };
        let stops = match pattern {
            Pattern::Linear { stops, .. } | Pattern::Radial { stops, .. } => stops,
            Pattern::Tiling { .. } => return false,
        };
        if stops.iter().all(|stop| alpha(stop.colour) == 255) {
            return false;
        }
        if self.written.contains_key(&('A', index)) {
            return true;
        }
        let object = self.reserve(('A', index));
        let [width, height] = self.size.map(|inches| inches * POINTS_PER_INCH);
        let form = self.add(pdf_stream(
            &format!(
                "/Type /XObject /Subtype /Form /BBox [0 0 {width:.2} {height:.2}] \
                 /Group << /S /Transparency /CS /DeviceGray >> \
                 /Resources << /Shading << /Sh {} >> >>",
                pdf_shading(pattern, true)
            ),
            b"/Sh sh\n",
        ));
        let bytes =
            format!("<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G {form} 0 R >> >>")
                .into_bytes();
        self.fill(object, bytes);
        true
    }
}

fn pt(inches: f64) -> f64 {
    inches * POINTS_PER_INCH
}

fn pdf_outline(out: &mut String, outline: &[Segment]) {
    for segment in outline {
        match segment {
            Segment::Move([x, y]) => out.push_str(&format!("{:.2} {:.2} m\n", pt(*x), pt(*y))),
            Segment::Line([x, y]) => out.push_str(&format!("{:.2} {:.2} l\n", pt(*x), pt(*y))),
            Segment::Cubic(first, second, end) => out.push_str(&format!(
                "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c\n",
                pt(first[0]),
                pt(first[1]),
                pt(second[0]),
                pt(second[1]),
                pt(end[0]),
                pt(end[1])
            )),
            Segment::Close => out.push_str("h\n"),
        }
    }
}

/// A gradient as a PDF shading dictionary, in RGB, or in grey levels that are the stops'
/// opacities when `opacity`. PDF only pads or stops at a gradient's ends, so a repeating or
/// reflecting gradient stops, as on GNU R's pdf device (which warns when it is defined).
fn pdf_shading(pattern: &Pattern, opacity: bool) -> String {
    let (kind, coordinates, stops, extend) = match pattern {
        Pattern::Linear {
            start,
            end,
            stops,
            extend,
        } => (
            2,
            format!(
                "{:.2} {:.2} {:.2} {:.2}",
                pt(start[0]),
                pt(start[1]),
                pt(end[0]),
                pt(end[1])
            ),
            stops,
            *extend,
        ),
        Pattern::Radial {
            start,
            start_radius,
            end,
            end_radius,
            stops,
            extend,
        } => (
            3,
            format!(
                "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                pt(start[0]),
                pt(start[1]),
                pt(*start_radius),
                pt(end[0]),
                pt(end[1]),
                pt(*end_radius)
            ),
            stops,
            *extend,
        ),
        Pattern::Tiling { .. } => return "<< >>".to_owned(),
    };
    let stops = gradient_stops(stops, Extend::Pad);
    let components = |colour: Colour| -> String {
        let [red, green, blue, a] = channels(colour);
        if opacity {
            format!("{:.3}", f64::from(a) / 255.0)
        } else {
            pdf_rgb(red, green, blue)
        }
    };
    let pieces: Vec<String> = stops
        .windows(2)
        .map(|pair| {
            format!(
                "<< /FunctionType 2 /Domain [0 1] /C0 [{}] /C1 [{}] /N 1 >>",
                components(pair[0].colour),
                components(pair[1].colour)
            )
        })
        .collect();
    let bounds: Vec<String> = stops[1..stops.len() - 1]
        .iter()
        .map(|stop| format!("{:.4}", stop.offset))
        .collect();
    let encode = vec!["0 1"; pieces.len()].join(" ");
    let extends = if extend == Extend::Pad {
        "true true"
    } else {
        "false false"
    };
    format!(
        "<< /ShadingType {kind} /ColorSpace /{} /Coords [{coordinates}] /Extend [{extends}] \
         /Function << /FunctionType 3 /Domain [0 1] /Functions [{}] /Bounds [{}] \
         /Encode [{encode}] >> >>",
        if opacity { "DeviceGray" } else { "DeviceRGB" },
        pieces.join(" "),
        bounds.join(" ")
    )
}

fn pdf_rgb(red: u8, green: u8, blue: u8) -> String {
    format!(
        "{:.3} {:.3} {:.3}",
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0
    )
}

/// PDF's line cap code for R's `lend`: round, butt or square.
fn pdf_cap(lend: i32) -> i32 {
    match lend {
        1 => 0,
        2 => 2,
        _ => 1,
    }
}

/// PDF's line join code for R's `ljoin`: round, mitre or bevel.
fn pdf_join(ljoin: i32) -> i32 {
    match ljoin {
        1 => 0,
        2 => 2,
        _ => 1,
    }
}

/// Text as a PDF literal string in WinAnsiEncoding: Latin-1 characters map to themselves and
/// anything else becomes `?`.
fn pdf_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(character);
            }
            ' '..='~' => out.push(character),
            '\u{a0}'..='\u{ff}' => out.push_str(&format!("\\{:03o}", character as u32)),
            _ => out.push('?'),
        }
    }
    out
}

// PostScript.

/// Procedures named after the PDF content operators that [`PdfWriter`] writes, so that a page's
/// PDF content stream is also a PostScript program. PDF keeps separate stroke and fill colours
/// where PostScript has one current colour, so `RG` and `rg` store theirs and each painting
/// operator sets the one it needs. The PostScript device draws only opaque colours, as GNU R's
/// does, so `gs` drops the opacity it would set.
const POSTSCRIPT_PROLOG: &str = "\
/FillColor [0 0 0] def /StrokeColor [0 0 0] def
/rg { 3 array astore /FillColor exch def } bind def
/RG { 3 array astore /StrokeColor exch def } bind def
/m { moveto } bind def /l { lineto } bind def /c { curveto } bind def /h { closepath } bind def
/re { 4 2 roll moveto 1 index 0 rlineto 0 exch rlineto neg 0 rlineto closepath } bind def
/f { FillColor aload pop setrgbcolor fill } bind def
/f* { FillColor aload pop setrgbcolor eofill } bind def
/S { StrokeColor aload pop setrgbcolor stroke } bind def
/B { gsave f grestore S } bind def /B* { gsave f* grestore S } bind def
/q { gsave } bind def /Q { grestore } bind def /W { clip } bind def /W* { eoclip } bind def
/n { newpath } bind def
/w { setlinewidth } bind def /J { setlinecap } bind def /j { setlinejoin } bind def
/M { setmiterlimit } bind def /d { setdash } bind def /gs { pop } bind def
/cm { 6 array astore concat } bind def
/BT { gsave } bind def /ET { grestore } bind def
/Tf { exch findfont exch scalefont setfont } bind def
/Tm { 6 array astore concat 0 0 moveto } bind def
/Tj { FillColor aload pop setrgbcolor show } bind def
/reencode { findfont dup length dict begin { 1 index /FID ne { def } { pop pop } ifelse } forall
  /Encoding ISOLatin1Encoding def currentdict end definefont pop } bind def
/F1 /Helvetica reencode /F2 /Helvetica-Bold reencode /F3 /Helvetica-Oblique reencode
/F4 /Helvetica-BoldOblique reencode /F5 /Symbol findfont definefont pop
";

/// Writes a raster image into the unit square of a PostScript page's current space. PostScript
/// has no alpha, so the image is drawn opaque, and no interpolation, so an image that asks for
/// it is first scaled smoothly to one pixel per point of `placement`'s size, as GNU R does.
fn postscript_image(
    out: &mut String,
    pixels: &[Colour],
    columns: usize,
    rows: usize,
    placement: Affine,
    interpolate: bool,
) {
    let resampled;
    let (pixels, columns, rows) = match interpolate.then(|| point_size(placement)).flatten() {
        Some([width, height]) => {
            resampled = interpolate_raster(pixels, [columns, rows], [width, height]);
            (resampled.as_slice(), width, height)
        }
        None => (pixels, columns, rows),
    };
    out.push_str(&format!(
        "/DeviceRGB setcolorspace\n<< /ImageType 1 /Width {columns} /Height {rows} \
         /BitsPerComponent 8 /Decode [0 1 0 1 0 1] /DataSource currentfile /ASCIIHexDecode \
         filter /ImageMatrix [{columns} 0 0 -{rows} 0 {rows}] >> image\n"
    ));
    for (index, pixel) in pixels.iter().enumerate() {
        let [red, green, blue, _] = channels(*pixel);
        out.push_str(&format!("{red:02x}{green:02x}{blue:02x}"));
        if index % 13 == 12 {
            out.push('\n');
        }
    }
    out.push_str(">\n");
}

/// The size in whole points of the parallelogram an image is drawn into, or `None` when it is
/// too large to scale an image up to, which no image on a page of sensible size is.
fn point_size(placement: Affine) -> Option<[usize; 2]> {
    const MOST_PIXELS: f64 = 16_777_216.0;
    let [a, b, c, d, _, _] = placement.0;
    let [width, height] =
        [a.hypot(b), c.hypot(d)].map(|inches| (pt(inches) + 0.5).floor().max(1.0));
    (width * height <= MOST_PIXELS).then_some([width as usize, height as usize])
}

/// Scales an image of `from` columns and rows to `to` by bilinear interpolation, edge pixels
/// extending outwards, sampling where GNU R's engine samples and with its weights in
/// sixteenths, so the scaled pixels match GNU R's.
fn interpolate_raster(pixels: &[Colour], from: [usize; 2], to: [usize; 2]) -> Vec<Colour> {
    let [columns, rows] = from;
    // Where destination pixel `index` samples the source along one axis: the source pixel and
    // the next one's weight, in sixteenths. The sample point is half a source pixel back from
    // the destination pixel's start.
    let sample = |index: usize, source: usize, destination: usize| -> (usize, u32) {
        let position = (index as f64 * 16.0 * source as f64 / destination as f64 - 8.0).max(0.0);
        let fixed = position as usize;
        let (whole, fraction) = (fixed / 16, (fixed % 16) as u32);
        if whole >= source - 1 {
            (source - 1, 0)
        } else {
            (whole, fraction)
        }
    };
    let pixel = |column: usize, row: usize| {
        pixels
            .get(row * columns + column)
            .copied()
            .unwrap_or_default()
    };
    let mut scaled = Vec::with_capacity(to[0] * to[1]);
    for row in 0..to[1] {
        let (top, down) = sample(row, rows, to[1]);
        let bottom = (top + 1).min(rows - 1);
        for column in 0..to[0] {
            let (left, right_weight) = sample(column, columns, to[0]);
            let right = (left + 1).min(columns - 1);
            let weights = [
                (16 - right_weight) * (16 - down),
                right_weight * (16 - down),
                (16 - right_weight) * down,
                right_weight * down,
            ];
            let corners = [
                pixel(left, top),
                pixel(right, top),
                pixel(left, bottom),
                pixel(right, bottom),
            ];
            let mut colour: Colour = 0;
            for channel in 0..4 {
                let shift = 8 * channel;
                let total: u32 = corners
                    .iter()
                    .zip(weights)
                    .map(|(corner, weight)| ((*corner >> shift) & 0xff) * weight)
                    .sum();
                colour |= ((total + 128) >> 8).min(255) << shift;
            }
            scaled.push(colour);
        }
    }
    scaled
}

/// A PostScript document of `pages`, each `size` inches, placed on `paper` as
/// [`Format::Postscript`] describes. An `encapsulated` document is EPS, which leaves out the
/// media and orientation comments. Text uses the standard Helvetica fonts in the ISO Latin-1
/// encoding, which matches the escapes `pdf_string` writes for it. The PostScript device makes
/// no definitions, so the pages need none.
fn postscript_document(
    pages: &[Page],
    size: [f64; 2],
    paper: Option<Paper>,
    landscape: bool,
    encapsulated: bool,
    title: &str,
) -> Vec<u8> {
    let [width, height] = size.map(|inches| inches * POINTS_PER_INCH);
    let (paper, landscape) = match paper {
        Some(paper) => (paper, landscape),
        None => (
            Paper {
                name: "special",
                size,
            },
            false,
        ),
    };
    let [paper_width, paper_height] = paper.size.map(|inches| inches * POINTS_PER_INCH);
    let (bounding_box, placement) = if landscape {
        let x = (paper_width - height) / 2.0;
        let y = (paper_height - width) / 2.0;
        (
            [x, y, x + height, y + width],
            format!("{paper_width:.2} 0 translate 90 rotate {y:.2} {x:.2} translate\n"),
        )
    } else {
        let x = (paper_width - width) / 2.0;
        let y = (paper_height - height) / 2.0;
        (
            [x, y, x + width, y + height],
            format!("{x:.2} {y:.2} translate\n"),
        )
    };
    let mut comments = String::new();
    if !encapsulated {
        // The media's size is whole points, cut down.
        comments.push_str(&format!(
            "%%DocumentMedia: {} {} {} 0 () ()\n",
            paper.name,
            paper_width.trunc(),
            paper_height.trunc()
        ));
        if paper.name != "special" {
            let orientation = if landscape { "Landscape" } else { "Portrait" };
            comments.push_str(&format!("%%Orientation: {orientation}\n"));
        }
    }
    let [left, bottom, right, top] = bounding_box.map(f64::round);
    let mut out = format!(
        "%!PS-Adobe-3.0{}\n%%DocumentNeededResources: font Helvetica Helvetica-Bold \
         Helvetica-Oblique Helvetica-BoldOblique Symbol\n{comments}%%Title: {}\n\
         %%Creator: R (ry)\n%%Pages: {}\n%%BoundingBox: {left} {bottom} {right} {top}\n\
         %%EndComments\n%%BeginProlog\n{POSTSCRIPT_PROLOG}%%EndProlog\n",
        if encapsulated { " EPSF-3.0" } else { "" },
        title.replace(['\n', '\r'], " "),
        pages.len(),
    );
    let mut writer = PdfWriter::new(&[], size, 0, true);
    for (index, page) in pages.iter().enumerate() {
        out.push_str(&format!("%%Page: {0} {0}\ngsave\n{placement}", index + 1));
        out.push_str(&writer.page(page));
        out.push_str("grestore\nshowpage\n");
    }
    out.push_str("%%EOF\n");
    out.into_bytes()
}

// SVG.

/// An SVG document of one page, in points with y growing downwards. The definitions the page
/// uses are written once each into the document's `<defs>`.
fn svg_document(page: &Page, definitions: &[Definition], size: [f64; 2]) -> String {
    let [width, height] = size.map(|inches| inches * POINTS_PER_INCH);
    let mut writer = SvgWriter {
        definitions,
        width,
        height,
        defs: String::new(),
        written: BTreeSet::new(),
        clips: Vec::new(),
    };
    let mut body = String::new();
    if alpha(page.background) > 0 {
        body.push_str(&format!(
            "<rect width=\"100%\" height=\"100%\" {}/>\n",
            svg_colour("fill", page.background)
        ));
    }
    writer.items(&mut body, &page.items, 0);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{width:.2}pt\" \
         height=\"{height:.2}pt\" viewBox=\"0 0 {width:.2} {height:.2}\">\n<defs>\n{}</defs>\n\
         {body}</svg>\n",
        writer.defs
    )
}

struct SvgWriter<'a> {
    definitions: &'a [Definition],
    /// The page width in points.
    width: f64,
    /// The page height in points, which turns inches from the bottom into points from the top.
    height: f64,
    defs: String,
    /// The definitions written into `defs`, by the prefix of their element id and their index.
    written: BTreeSet<(char, usize)>,
    /// The clipping rectangles written so far, the id of each its position.
    clips: Vec<Rect>,
}

impl SvgWriter<'_> {
    fn x(&self, inches: f64) -> f64 {
        inches * POINTS_PER_INCH
    }

    fn y(&self, inches: f64) -> f64 {
        self.height - inches * POINTS_PER_INCH
    }

    /// The map from inches to the document's points.
    fn to_points(&self) -> Affine {
        Affine([
            POINTS_PER_INCH,
            0.0,
            0.0,
            -POINTS_PER_INCH,
            0.0,
            self.height,
        ])
    }

    /// Writes `items`, each run of items with the same clipping region and mask in its own
    /// `<g>`.
    fn items(&mut self, out: &mut String, items: &[Item], depth: usize) {
        if depth > MAX_NESTING {
            return;
        }
        let mut current: Option<(Clip, Option<usize>)> = None;
        for item in items {
            if current != Some((item.clip, item.mask)) {
                if current.is_some() {
                    out.push_str("</g>\n");
                }
                let clip = self.clip(item.clip);
                out.push_str(&format!("<g clip-path=\"url(#{clip})\""));
                if let Some(mask) = item.mask
                    && self.define('m', mask, depth)
                {
                    out.push_str(&format!(" mask=\"url(#m{mask})\""));
                }
                out.push_str(">\n");
                current = Some((item.clip, item.mask));
            }
            self.mark(out, &item.mark, depth);
        }
        if current.is_some() {
            out.push_str("</g>\n");
        }
    }

    /// The id of the `<clipPath>` for a clipping region, written on first use.
    fn clip(&mut self, clip: Clip) -> String {
        match clip {
            Clip::Rect(rect) => {
                let index = match self.clips.iter().position(|known| *known == rect) {
                    Some(index) => index,
                    None => {
                        self.defs.push_str(&format!(
                            "<clipPath id=\"c{}\"><rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" \
                             height=\"{:.2}\"/></clipPath>\n",
                            self.clips.len(),
                            self.x(rect.x0),
                            self.y(rect.y1),
                            self.x(rect.x1 - rect.x0),
                            self.x(rect.y1 - rect.y0)
                        ));
                        self.clips.push(rect);
                        self.clips.len() - 1
                    }
                };
                format!("c{index}")
            }
            Clip::Path(index) => {
                if !self.written.contains(&('p', index))
                    && let Some(Definition::ClipPath { outline, rule }) =
                        self.definitions.get(index)
                {
                    self.written.insert(('p', index));
                    self.defs.push_str(&format!(
                        "<clipPath id=\"p{index}\"><path d=\"{}\" clip-rule=\"{}\"/></clipPath>\n",
                        self.outline(outline),
                        svg_rule(*rule)
                    ));
                }
                format!("p{index}")
            }
        }
    }

    fn mark(&mut self, out: &mut String, mark: &Mark, depth: usize) {
        match mark {
            Mark::Path {
                outline,
                fill,
                rule,
                stroke,
            } => {
                if outline.is_empty() {
                    return;
                }
                let fill = match painted(fill) {
                    Some(Fill::Colour(colour)) => svg_colour("fill", colour),
                    Some(Fill::Pattern(index)) if self.define('f', index, depth) => {
                        format!("fill=\"url(#f{index})\"")
                    }
                    _ => "fill=\"none\"".to_owned(),
                };
                out.push_str(&format!(
                    "<path d=\"{}\" {fill} fill-rule=\"{}\" {}/>\n",
                    self.outline(outline),
                    svg_rule(*rule),
                    svg_stroke(stroke)
                ));
            }
            Mark::Text {
                origin,
                text,
                points,
                font,
                rotation,
                hadj,
                colour,
            } => {
                if alpha(*colour) == 0 || text.is_empty() {
                    return;
                }
                let face = if (1..=5).contains(font) { *font } else { 1 };
                let width = text_width_ems(text, face) * points / POINTS_PER_INCH;
                let (sin, cos) = rotation.to_radians().sin_cos();
                let tx = self.x(origin[0] - hadj * width * cos);
                let ty = self.y(origin[1] - hadj * width * sin);
                let weight = if face == 2 || face == 4 {
                    " font-weight=\"bold\""
                } else {
                    ""
                };
                let style = if face == 3 || face == 4 {
                    " font-style=\"italic\""
                } else {
                    ""
                };
                let family = if face == 5 {
                    "Symbol"
                } else {
                    "Helvetica, Arial, sans-serif"
                };
                out.push_str(&format!(
                    "<text x=\"{tx:.2}\" y=\"{ty:.2}\" transform=\"rotate({:.2} {tx:.2} {ty:.2})\" \
                     font-family=\"{family}\" font-size=\"{points:.2}\"{weight}{style} {}>{}</text>\n",
                    -rotation,
                    svg_colour("fill", *colour),
                    xml_escape(text)
                ));
            }
            Mark::Image {
                pixels,
                columns,
                rows,
                placement,
                interpolate,
            } => {
                // Viewers scale a small image up in visible steps, so an image that asks for
                // interpolation is first scaled up smoothly to one pixel per point, as cairo
                // does.
                let Some(mut image) = image_pixmap(pixels, *columns, *rows) else {
                    return;
                };
                let (mut columns, mut rows) = (*columns, *rows);
                if *interpolate
                    && let Some(size) = point_size(*placement)
                    && (size[0] > columns || size[1] > rows)
                    && let Some(scaled) = smooth_scale(&image, size)
                {
                    (image, [columns, rows]) = (scaled, size);
                }
                let Ok(png) = image.encode_png() else {
                    return;
                };
                let [a, b, c, d, e, f] = image_space(columns, rows)
                    .then(*placement)
                    .then(self.to_points())
                    .0;
                // Viewers smooth a scaled image unless told otherwise.
                let rendering = if *interpolate {
                    ""
                } else {
                    " style=\"image-rendering:pixelated\""
                };
                out.push_str(&format!(
                    "<image width=\"{columns}\" height=\"{rows}\" preserveAspectRatio=\"none\" \
                     transform=\"matrix({a:.6} {b:.6} {c:.6} {d:.6} {e:.2} {f:.2})\"{rendering} \
                     xlink:href=\"data:image/png;base64,{}\"/>\n",
                    base64(&png)
                ));
            }
            Mark::Group { group, transform } => {
                if !self.define('g', *group, depth) {
                    return;
                }
                let to_points = self.to_points();
                let Some(from_points) = to_points.invert() else {
                    return;
                };
                let [a, b, c, d, e, f] = from_points.then(*transform).then(to_points).0;
                out.push_str(&format!(
                    "<use xlink:href=\"#g{group}\" transform=\"matrix({a:.4} {b:.4} {c:.4} {d:.4} \
                     {e:.2} {f:.2})\"/>\n"
                ));
            }
        }
    }

    /// Writes the element a definition stands for into `defs`, unless it has been written:
    /// `f` a pattern, `m` a mask, `g` a group. Whether the element exists, which it does not
    /// when `index` names another kind of definition.
    fn define(&mut self, kind: char, index: usize, depth: usize) -> bool {
        if self.written.contains(&(kind, index)) {
            return true;
        }
        let definitions = self.definitions;
        let Some(definition) = definitions.get(index) else {
            return false;
        };
        // Recorded before the content is written, so a definition that refers to itself
        // finds it instead of recursing.
        self.written.insert((kind, index));
        let mut element = String::new();
        match (kind, definition) {
            ('f', Definition::Pattern(pattern)) => {
                self.pattern(&mut element, index, pattern, depth)
            }
            ('m', Definition::Mask { luminance, items }) => {
                let height = self.height;
                element.push_str(&format!(
                    "<mask id=\"m{index}\" maskUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" \
                     width=\"100%\" height=\"{height:.2}\" style=\"mask-type:{}\">\n",
                    if *luminance { "luminance" } else { "alpha" }
                ));
                self.items(&mut element, items, depth + 1);
                element.push_str("</mask>\n");
            }
            (
                'g',
                Definition::Group {
                    destination,
                    operator,
                    source,
                },
            ) => {
                element.push_str(&format!(
                    "<g id=\"g{index}\" style=\"isolation:isolate\">\n"
                ));
                self.items(&mut element, destination, depth + 1);
                match svg_blend_mode(*operator) {
                    // Each source mark blends with what is under it, as on the other devices,
                    // rather than the source marks first combining among themselves.
                    Some(mode) => {
                        for item in source {
                            element.push_str(&format!("<g style=\"mix-blend-mode:{mode}\">\n"));
                            self.items(&mut element, std::slice::from_ref(item), depth + 1);
                            element.push_str("</g>\n");
                        }
                    }
                    None => self.items(&mut element, source, depth + 1),
                }
                element.push_str("</g>\n");
            }
            _ => {
                self.written.remove(&(kind, index));
                return false;
            }
        }
        self.defs.push_str(&element);
        true
    }

    fn pattern(&mut self, out: &mut String, index: usize, pattern: &Pattern, depth: usize) {
        let stop_elements = |stops: &[Stop], extend: Extend| -> String {
            gradient_stops(stops, extend)
                .iter()
                .map(|stop| {
                    let [red, green, blue, a] = channels(stop.colour);
                    format!(
                        "<stop offset=\"{:.4}\" stop-color=\"rgb({red},{green},{blue})\" \
                         stop-opacity=\"{:.3}\"/>\n",
                        stop.offset,
                        f64::from(a) / 255.0
                    )
                })
                .collect()
        };
        match pattern {
            Pattern::Linear {
                start,
                end,
                stops,
                extend,
            } => out.push_str(&format!(
                "<linearGradient id=\"f{index}\" gradientUnits=\"userSpaceOnUse\" x1=\"{:.2}\" \
                 y1=\"{:.2}\" x2=\"{:.2}\" y2=\"{:.2}\" spreadMethod=\"{}\">\n{}</linearGradient>\n",
                self.x(start[0]),
                self.y(start[1]),
                self.x(end[0]),
                self.y(end[1]),
                svg_spread(*extend),
                stop_elements(stops, *extend)
            )),
            Pattern::Radial {
                start,
                start_radius,
                end,
                end_radius,
                stops,
                extend,
            } => out.push_str(&format!(
                "<radialGradient id=\"f{index}\" gradientUnits=\"userSpaceOnUse\" fx=\"{:.2}\" \
                 fy=\"{:.2}\" fr=\"{:.2}\" cx=\"{:.2}\" cy=\"{:.2}\" r=\"{:.2}\" \
                 spreadMethod=\"{}\">\n{}</radialGradient>\n",
                self.x(start[0]),
                self.y(start[1]),
                self.x(*start_radius),
                self.x(end[0]),
                self.y(end[1]),
                self.x(*end_radius),
                svg_spread(*extend),
                stop_elements(stops, *extend)
            )),
            Pattern::Tiling {
                rect,
                items,
                extend,
            } => {
                let [x, y] = [self.x(rect.x0), self.y(rect.y1)];
                let [width, height] = [self.x(rect.x1 - rect.x0), self.x(rect.y1 - rect.y0)];
                // SVG patterns only repeat. A reflected tile is a repeated block of the tile and
                // its three mirror images. A tile that is not repeated sits `margin` points
                // inside a cell large enough that the next copy falls off the page; viewers
                // rasterize a pattern cell, so a cell much larger than the page would blur it.
                let margin = match extend {
                    Extend::Repeat | Extend::Reflect => 0.0,
                    Extend::Pad | Extend::None => {
                        self.width.max(self.height) + x.abs().max(y.abs())
                    }
                };
                let (cell_width, cell_height) = match extend {
                    Extend::Repeat => (width, height),
                    Extend::Reflect => (2.0 * width, 2.0 * height),
                    Extend::Pad | Extend::None => (width + 2.0 * margin, height + 2.0 * margin),
                };
                out.push_str(&format!(
                    "<pattern id=\"f{index}\" patternUnits=\"userSpaceOnUse\" x=\"{:.2}\" \
                     y=\"{:.2}\" width=\"{cell_width:.2}\" height=\"{cell_height:.2}\">\n\
                     <clipPath id=\"f{index}c\"><rect x=\"{margin:.2}\" y=\"{margin:.2}\" \
                     width=\"{width:.2}\" height=\"{height:.2}\"/></clipPath>\n",
                    x - margin,
                    y - margin,
                ));
                if *extend == Extend::Pad {
                    // cairo pads with the tile's edge pixels, which are one point wide at the
                    // SVG surface's 72 pixels per inch. Each edge strip and corner of the tile
                    // is mirrored outwards and stretched across the margin by the map
                    // `e (1 + k) - k v + s`, which sends the strip's inner side `k` points
                    // beyond the edge `e` and shifts it `s` points back over the tile. The
                    // pieces overlap, corners under edges under the tile, so viewers draw no
                    // anti-aliased seam where they meet.
                    let strip = [width, height].map(|extent| extent.min(1.0));
                    let stretch = margin.max(1.0);
                    let sides = |start: f64, extent: f64, strip: f64| {
                        [
                            Some((start, start, strip, strip)),
                            None,
                            Some((start + extent, start + extent - strip, strip, -strip)),
                        ]
                    };
                    let axis = |side: Option<(f64, f64, f64, f64)>, extent: f64| match side {
                        Some((edge, from, strip, shift)) => {
                            (-stretch, edge * (1.0 + stretch) + shift, from, strip)
                        }
                        None => (1.0, 0.0, margin, extent),
                    };
                    let mut pieces = Vec::new();
                    for vertical in sides(margin, height, strip[1]) {
                        for horizontal in sides(margin, width, strip[0]) {
                            match (horizontal.is_some(), vertical.is_some()) {
                                (true, true) => pieces.insert(0, (horizontal, vertical)),
                                (false, false) => {}
                                _ => pieces.push((horizontal, vertical)),
                            }
                        }
                    }
                    for (piece, (horizontal, vertical)) in pieces.into_iter().enumerate() {
                        let (scale_x, shift_x, clip_x, clip_width) = axis(horizontal, width);
                        let (scale_y, shift_y, clip_y, clip_height) = axis(vertical, height);
                        out.push_str(&format!(
                            "<clipPath id=\"f{index}s{piece}\"><rect x=\"{clip_x:.2}\" \
                             y=\"{clip_y:.2}\" width=\"{clip_width:.2}\" \
                             height=\"{clip_height:.2}\"/></clipPath>\n\
                             <g transform=\"matrix({scale_x:.2} 0 0 {scale_y:.2} {shift_x:.2} \
                             {shift_y:.2})\"><g clip-path=\"url(#f{index}s{piece})\">\
                             <use xlink:href=\"#f{index}t\"/></g></g>\n"
                        ));
                    }
                }
                out.push_str(&format!(
                    "<g id=\"f{index}t\" clip-path=\"url(#f{index}c)\">\n\
                     <g transform=\"translate({:.2} {:.2})\">\n",
                    margin - x,
                    margin - y
                ));
                self.items(out, items, depth + 1);
                out.push_str("</g>\n</g>\n");
                if *extend == Extend::Reflect {
                    for (scale_x, scale_y) in [(-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
                        out.push_str(&format!(
                            "<use xlink:href=\"#f{index}t\" transform=\"matrix({scale_x} 0 0 \
                             {scale_y} {:.2} {:.2})\"/>\n",
                            if scale_x < 0.0 { 2.0 * width } else { 0.0 },
                            if scale_y < 0.0 { 2.0 * height } else { 0.0 }
                        ));
                    }
                }
                out.push_str("</pattern>\n");
            }
        }
    }

    fn outline(&self, outline: &[Segment]) -> String {
        let mut data = String::new();
        for segment in outline {
            match segment {
                Segment::Move([x, y]) => {
                    data.push_str(&format!("M{:.2} {:.2} ", self.x(*x), self.y(*y)));
                }
                Segment::Line([x, y]) => {
                    data.push_str(&format!("L{:.2} {:.2} ", self.x(*x), self.y(*y)));
                }
                Segment::Cubic(first, second, end) => data.push_str(&format!(
                    "C{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} ",
                    self.x(first[0]),
                    self.y(first[1]),
                    self.x(second[0]),
                    self.y(second[1]),
                    self.x(end[0]),
                    self.y(end[1])
                )),
                Segment::Close => data.push_str("Z "),
            }
        }
        data.truncate(data.trim_end().len());
        data
    }
}

fn svg_rule(rule: FillRule) -> &'static str {
    match rule {
        FillRule::Winding => "nonzero",
        FillRule::EvenOdd => "evenodd",
    }
}

/// SVG has no gradient that stops at its ends, so a gradient that paints nothing beyond them
/// pads with the transparent stops [`gradient_stops`] adds.
fn svg_spread(extend: Extend) -> &'static str {
    match extend {
        Extend::Repeat => "repeat",
        Extend::Reflect => "reflect",
        Extend::Pad | Extend::None => "pad",
    }
}

fn svg_blend_mode(operator: Operator) -> Option<&'static str> {
    Some(match operator {
        Operator::Multiply => "multiply",
        Operator::Screen => "screen",
        Operator::Overlay => "overlay",
        Operator::Darken => "darken",
        Operator::Lighten => "lighten",
        Operator::ColorDodge => "color-dodge",
        Operator::ColorBurn => "color-burn",
        Operator::HardLight => "hard-light",
        Operator::SoftLight => "soft-light",
        Operator::Difference => "difference",
        Operator::Exclusion => "exclusion",
        _ => return None,
    })
}

fn svg_colour(attribute: &str, colour: Colour) -> String {
    let [red, green, blue, a] = channels(colour);
    if a == 0 {
        return format!("{attribute}=\"none\"");
    }
    let mut paint = format!("{attribute}=\"rgb({red},{green},{blue})\"");
    if a < 255 {
        paint.push_str(&format!(
            " {attribute}-opacity=\"{:.3}\"",
            f64::from(a) / 255.0
        ));
    }
    paint
}

fn svg_stroke(stroke: &Option<Stroke>) -> String {
    let Some(stroke) = visible(stroke) else {
        return "stroke=\"none\"".to_owned();
    };
    let caps = ["round", "butt", "square"];
    let joins = ["round", "miter", "bevel"];
    let pick = |names: [&'static str; 3], code: i32| {
        usize::try_from(code)
            .ok()
            .and_then(|index| names.get(index).copied())
            .unwrap_or("round")
    };
    let mut paint = format!(
        "{} stroke-width=\"{:.2}\" stroke-linecap=\"{}\" stroke-linejoin=\"{}\" \
         stroke-miterlimit=\"{:.2}\"",
        svg_colour("stroke", stroke.colour),
        stroke.lwd * 0.75,
        pick(caps, stroke.lend),
        pick(joins, stroke.ljoin),
        stroke.lmitre
    );
    let pattern = dashes(stroke);
    if !pattern.is_empty() {
        let lengths: Vec<String> = pattern
            .iter()
            .map(|length| format!("{:.2}", length * POINTS_PER_INCH))
            .collect();
        paint.push_str(&format!(" stroke-dasharray=\"{}\"", lengths.join(",")));
    }
    paint
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// Rasters.

/// Draws a page into pixels at `resolution` pixels per inch.
fn rasterize(
    page: &Page,
    definitions: &[Definition],
    size: [f64; 2],
    resolution: f64,
) -> Option<tiny_skia::Pixmap> {
    let [width, height] = size.map(|inches| (inches * resolution).round().max(1.0) as u32);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let [red, green, blue, a] = channels(page.background);
    pixmap.fill(tiny_skia::Color::from_rgba8(red, green, blue, a));
    let to_pixels = Affine([resolution, 0.0, 0.0, -resolution, 0.0, f64::from(height)]);
    draw_items(
        definitions,
        &mut pixmap,
        &page.items,
        to_pixels,
        Operator::Over,
        0,
    );
    Some(pixmap)
}

/// Draws `items` onto `canvas` through `to_pixels`, which maps inches to the canvas's pixels,
/// each mark composited with `operator`. A mark drawn with any operator but `over`, and every
/// group, is first drawn alone onto a transparent layer, and the layer is then composited onto
/// the canvas within the clipping region, as cairo composites a shape through an implicit
/// group.
fn draw_items(
    definitions: &[Definition],
    canvas: &mut tiny_skia::Pixmap,
    items: &[Item],
    to_pixels: Affine,
    operator: Operator,
    depth: usize,
) {
    if depth > MAX_NESTING {
        return;
    }
    let mut current: Option<((Clip, Option<usize>), Option<tiny_skia::Mask>)> = None;
    for item in items {
        let key = (item.clip, item.mask);
        if current.as_ref().is_none_or(|(known, _)| *known != key) {
            let coverage = coverage(
                definitions,
                canvas.width(),
                canvas.height(),
                item,
                to_pixels,
                depth,
            );
            current = Some((key, coverage));
        }
        let Some((_, Some(coverage))) = current.as_ref() else {
            continue;
        };
        if operator == Operator::Over && !matches!(item.mark, Mark::Group { .. }) {
            draw_mark(
                definitions,
                canvas,
                &item.mark,
                to_pixels,
                Some(coverage),
                depth,
            );
            continue;
        }
        let Some(mut layer) = tiny_skia::Pixmap::new(canvas.width(), canvas.height()) else {
            continue;
        };
        draw_mark(definitions, &mut layer, &item.mark, to_pixels, None, depth);
        if operator == Operator::Saturate {
            saturate(canvas, &layer, coverage);
            continue;
        }
        canvas.draw_pixmap(
            0,
            0,
            layer.as_ref(),
            &tiny_skia::PixmapPaint {
                blend_mode: operator.skia(),
                ..tiny_skia::PixmapPaint::default()
            },
            tiny_skia::Transform::identity(),
            Some(coverage),
        );
    }
}

/// Composites `layer` onto `canvas` within `coverage` by cairo's `saturate` operator, which
/// tiny-skia lacks: the source, scaled down to fit the room the destination's alpha leaves,
/// is added to the destination. Both are premultiplied and the same size.
fn saturate(canvas: &mut tiny_skia::Pixmap, layer: &tiny_skia::Pixmap, coverage: &tiny_skia::Mask) {
    let pixels = canvas.data_mut().chunks_exact_mut(4);
    for ((destination, source), &cover) in pixels
        .zip(layer.data().chunks_exact(4))
        .zip(coverage.data())
    {
        let cover = f32::from(cover) / 255.0;
        let source_alpha = f32::from(source[3]) * cover;
        if source_alpha <= 0.0 {
            continue;
        }
        let room = 255.0 - f32::from(destination[3]);
        let scale = cover * (room / source_alpha).min(1.0);
        for (value, added) in destination.iter_mut().zip(source) {
            *value = (f32::from(*value) + f32::from(*added) * scale)
                .round()
                .min(255.0) as u8;
        }
    }
}

/// Where an item may draw: its clipping region, scaled by its mask.
fn coverage(
    definitions: &[Definition],
    width: u32,
    height: u32,
    item: &Item,
    to_pixels: Affine,
    depth: usize,
) -> Option<tiny_skia::Mask> {
    let mut coverage = tiny_skia::Mask::new(width, height)?;
    match item.clip {
        Clip::Rect(rect) => {
            let path = skia_path(&rect_outline(rect), to_pixels)?;
            coverage.fill_path(&path, tiny_skia::FillRule::Winding, false, to_pixels.skia());
        }
        Clip::Path(index) => {
            if let Some(Definition::ClipPath { outline, rule }) = definitions.get(index)
                && let Some(path) = skia_path(outline, to_pixels)
            {
                coverage.fill_path(&path, skia_rule(*rule), true, to_pixels.skia());
            }
        }
    }
    if let Some(index) = item.mask
        && let Some(Definition::Mask { luminance, items }) = definitions.get(index)
    {
        let mut layer = tiny_skia::Pixmap::new(width, height)?;
        draw_items(
            definitions,
            &mut layer,
            items,
            to_pixels,
            Operator::Over,
            depth + 1,
        );
        let kind = if *luminance {
            tiny_skia::MaskType::Luminance
        } else {
            tiny_skia::MaskType::Alpha
        };
        let mask = tiny_skia::Mask::from_pixmap(layer.as_ref(), kind);
        for (value, scale) in coverage.data_mut().iter_mut().zip(mask.data()) {
            *value = ((u16::from(*value) * u16::from(*scale) + 127) / 255) as u8;
        }
    }
    Some(coverage)
}

fn draw_mark(
    definitions: &[Definition],
    canvas: &mut tiny_skia::Pixmap,
    mark: &Mark,
    to_pixels: Affine,
    coverage: Option<&tiny_skia::Mask>,
    depth: usize,
) {
    match mark {
        Mark::Path {
            outline,
            fill,
            rule,
            stroke,
        } => {
            let Some(path) = skia_path(outline, to_pixels) else {
                return;
            };
            let source = match painted(fill) {
                Some(Fill::Colour(colour)) => Some(Source::Solid(colour)),
                Some(Fill::Pattern(index)) => pattern_source(
                    definitions,
                    index,
                    to_pixels,
                    [canvas.width(), canvas.height()],
                    depth,
                ),
                None => None,
            };
            if let Some(source) = source {
                let tile;
                let shader = match source {
                    Source::Solid(colour) => {
                        let [red, green, blue, a] = channels(colour);
                        tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(
                            red, green, blue, a,
                        ))
                    }
                    Source::Gradient(shader) => shader,
                    Source::Tile {
                        pixmap,
                        spread,
                        transform,
                    } => {
                        tile = pixmap;
                        tiny_skia::Pattern::new(
                            tile.as_ref(),
                            spread,
                            tiny_skia::FilterQuality::Bilinear,
                            1.0,
                            transform,
                        )
                    }
                };
                let paint = tiny_skia::Paint {
                    shader,
                    anti_alias: true,
                    ..tiny_skia::Paint::default()
                };
                canvas.fill_path(&path, &paint, skia_rule(*rule), to_pixels.skia(), coverage);
            }
            if let Some(stroke) = visible(stroke) {
                canvas.stroke_path(
                    &path,
                    &solid_paint(stroke.colour),
                    &skia_stroke(stroke),
                    to_pixels.skia(),
                    coverage,
                );
            }
        }
        Mark::Text {
            origin,
            text,
            points,
            font,
            rotation,
            hadj,
            colour,
        } => {
            if alpha(*colour) == 0 {
                return;
            }
            let Some(face) = raster_font(*font) else {
                return;
            };
            let outline = text_outline(face, text, *points, *origin, *rotation, *hadj);
            if let Some(path) = skia_path(&outline, to_pixels) {
                canvas.fill_path(
                    &path,
                    &solid_paint(*colour),
                    tiny_skia::FillRule::Winding,
                    to_pixels.skia(),
                    coverage,
                );
            }
        }
        Mark::Image {
            pixels,
            columns,
            rows,
            placement,
            interpolate,
        } => {
            let Some(image) = image_pixmap(pixels, *columns, *rows) else {
                return;
            };
            let Some(path) = skia_path(
                &polygon_outline(
                    &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
                        .map(|corner| placement.apply(corner)),
                    true,
                ),
                to_pixels,
            ) else {
                return;
            };
            let quality = if *interpolate {
                tiny_skia::FilterQuality::Bilinear
            } else {
                tiny_skia::FilterQuality::Nearest
            };
            let paint = tiny_skia::Paint {
                shader: tiny_skia::Pattern::new(
                    image.as_ref(),
                    tiny_skia::SpreadMode::Pad,
                    quality,
                    1.0,
                    image_space(*columns, *rows).then(*placement).skia(),
                ),
                anti_alias: true,
                ..tiny_skia::Paint::default()
            };
            // cairo samples an image drawn without interpolation at pixel centres and finds
            // nothing beyond its edge, so an edge pixel whose centre lies outside the image
            // stays empty however much of it the image covers.
            let centres;
            let coverage = if *interpolate {
                coverage
            } else {
                let Some(mut inside) = tiny_skia::Mask::new(canvas.width(), canvas.height()) else {
                    return;
                };
                inside.fill_path(&path, tiny_skia::FillRule::Winding, false, to_pixels.skia());
                if let Some(coverage) = coverage {
                    for (value, scale) in inside.data_mut().iter_mut().zip(coverage.data()) {
                        *value = ((u16::from(*value) * u16::from(*scale) + 127) / 255) as u8;
                    }
                }
                centres = inside;
                Some(&centres)
            };
            canvas.fill_path(
                &path,
                &paint,
                tiny_skia::FillRule::Winding,
                to_pixels.skia(),
                coverage,
            );
        }
        Mark::Group { group, transform } => {
            let Some(Definition::Group {
                destination,
                operator,
                source,
            }) = definitions.get(*group)
            else {
                return;
            };
            let Some(mut layer) = tiny_skia::Pixmap::new(canvas.width(), canvas.height()) else {
                return;
            };
            let to_pixels = transform.then(to_pixels);
            draw_items(
                definitions,
                &mut layer,
                destination,
                to_pixels,
                Operator::Over,
                depth + 1,
            );
            draw_items(
                definitions,
                &mut layer,
                source,
                to_pixels,
                *operator,
                depth + 1,
            );
            canvas.draw_pixmap(
                0,
                0,
                layer.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                tiny_skia::Transform::identity(),
                coverage,
            );
        }
    }
}

/// What fills a path in a raster: a colour, a gradient's shader, or a tile repeated by a
/// pattern shader, which borrows the tile and so is made where it is drawn.
enum Source {
    Solid(Colour),
    Gradient(tiny_skia::Shader<'static>),
    Tile {
        pixmap: tiny_skia::Pixmap,
        spread: tiny_skia::SpreadMode,
        transform: tiny_skia::Transform,
    },
}

/// The source a pattern definition fills with. Gradients are defined in inches, which tiny-skia
/// maps through the path's own transform.
/// What a pattern paints on a canvas `canvas` pixels across, as a shader in path space.
fn pattern_source(
    definitions: &[Definition],
    index: usize,
    to_pixels: Affine,
    canvas: [u32; 2],
    depth: usize,
) -> Option<Source> {
    use tiny_skia::{GradientStop, LinearGradient, Point, SpreadMode};
    let Some(Definition::Pattern(pattern)) = definitions.get(index) else {
        return None;
    };
    let spread = |extend: Extend| match extend {
        Extend::Repeat => SpreadMode::Repeat,
        Extend::Reflect => SpreadMode::Reflect,
        Extend::Pad | Extend::None => SpreadMode::Pad,
    };
    let skia_stops = |stops: &[Stop], extend: Extend, remap: &dyn Fn(f64) -> f64| {
        gradient_stops(stops, extend)
            .iter()
            .map(|stop| {
                let [red, green, blue, a] = channels(stop.colour);
                GradientStop::new(
                    remap(stop.offset) as f32,
                    tiny_skia::Color::from_rgba8(red, green, blue, a),
                )
            })
            .collect::<Vec<_>>()
    };
    let point = |[x, y]: [f64; 2]| Point::from_xy(x as f32, y as f32);
    match pattern {
        Pattern::Linear {
            start,
            end,
            stops,
            extend,
        } => LinearGradient::new(
            point(*start),
            point(*end),
            skia_stops(stops, *extend, &|offset| offset),
            spread(*extend),
            tiny_skia::Transform::identity(),
        )
        .map(Source::Gradient),
        Pattern::Radial {
            start,
            start_radius,
            end,
            end_radius,
            stops,
            extend,
        } => {
            // tiny-skia's radial gradient only starts from a point, so the gradient between two
            // circles is evaluated per pixel into an image the size of the canvas.
            let from_pixels = to_pixels.invert()?;
            let pixmap = radial_gradient_image(
                [*start, *end],
                [*start_radius, *end_radius],
                &gradient_stops(stops, Extend::Pad),
                *extend,
                from_pixels,
                canvas,
            )?;
            Some(Source::Tile {
                pixmap,
                spread: SpreadMode::Pad,
                transform: from_pixels.skia(),
            })
        }
        Pattern::Tiling {
            rect,
            items,
            extend,
        } => {
            let scale = to_pixels.scale();
            // A tile that paints nothing beyond itself keeps a transparent pixel round its
            // edge for the pad to repeat.
            let border = if *extend == Extend::None { 1.0 } else { 0.0 };
            let width = ((rect.x1 - rect.x0) * scale).round().max(1.0) + 2.0 * border;
            let height = ((rect.y1 - rect.y0) * scale).round().max(1.0) + 2.0 * border;
            if width * height > 1.0e8 {
                return None;
            }
            let mut tile = tiny_skia::Pixmap::new(width as u32, height as u32)?;
            let to_tile = Affine([
                scale,
                0.0,
                0.0,
                -scale,
                border - rect.x0 * scale,
                border + rect.y1 * scale,
            ]);
            draw_items(
                definitions,
                &mut tile,
                items,
                to_tile,
                Operator::Over,
                depth + 1,
            );
            let from_tile = Affine([
                1.0 / scale,
                0.0,
                0.0,
                -1.0 / scale,
                rect.x0 - border / scale,
                rect.y1 + border / scale,
            ]);
            Some(Source::Tile {
                pixmap: tile,
                spread: match extend {
                    Extend::None => SpreadMode::Pad,
                    other => spread(*other),
                },
                transform: from_tile.skia(),
            })
        }
    }
}

/// The gradient between two circles as cairo and PDF define it, rendered at each pixel centre
/// of a canvas `canvas` pixels across. A point takes the colour at the largest `t` whose circle,
/// interpolated from the start circle to the end circle with a radius of at least zero, passes
/// through it; `extend` decides what `t` outside `[0, 1]` paints.
fn radial_gradient_image(
    [start, end]: [[f64; 2]; 2],
    [start_radius, end_radius]: [f64; 2],
    stops: &[Stop],
    extend: Extend,
    from_pixels: Affine,
    [width, height]: [u32; 2],
) -> Option<tiny_skia::Pixmap> {
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let centre = [end[0] - start[0], end[1] - start[1]];
    let growth = end_radius - start_radius;
    let a = centre[0] * centre[0] + centre[1] * centre[1] - growth * growth;
    let colour_at = |t: f64| -> Option<[f64; 4]> {
        let t = match extend {
            Extend::Pad => t.clamp(0.0, 1.0),
            Extend::None if !(0.0..=1.0).contains(&t) => return None,
            Extend::None => t,
            Extend::Repeat => t - t.floor(),
            Extend::Reflect => {
                let period = t.rem_euclid(2.0);
                if period > 1.0 { 2.0 - period } else { period }
            }
        };
        let after = stops
            .iter()
            .position(|stop| stop.offset >= t)
            .unwrap_or(stops.len().saturating_sub(1));
        let before = after.saturating_sub(1);
        let (low, high) = (stops.get(before)?, stops.get(after)?);
        let span = high.offset - low.offset;
        let weight = if span > 0.0 {
            (t - low.offset) / span
        } else {
            1.0
        };
        let [low, high] = [channels(low.colour), channels(high.colour)];
        Some(std::array::from_fn(|channel| {
            f64::from(low[channel]) * (1.0 - weight) + f64::from(high[channel]) * weight
        }))
    };
    let pixels = pixmap.pixels_mut();
    for row in 0..height {
        for column in 0..width {
            let [x, y] = from_pixels.apply([f64::from(column) + 0.5, f64::from(row) + 0.5]);
            let offset = [x - start[0], y - start[1]];
            let b = offset[0] * centre[0] + offset[1] * centre[1] + start_radius * growth;
            let c = offset[0] * offset[0] + offset[1] * offset[1] - start_radius * start_radius;
            let radius_ok = |t: f64| t.is_finite() && start_radius + t * growth >= 0.0;
            let t = if a.abs() < 1e-12 {
                Some(c / (2.0 * b)).filter(|t| radius_ok(*t))
            } else {
                let discriminant = b * b - a * c;
                if discriminant < 0.0 {
                    None
                } else {
                    let root = discriminant.sqrt();
                    let (larger, smaller) = if a > 0.0 {
                        ((b + root) / a, (b - root) / a)
                    } else {
                        ((b - root) / a, (b + root) / a)
                    };
                    [larger, smaller].into_iter().find(|t| radius_ok(*t))
                }
            };
            let Some([red, green, blue, alpha]) = t.and_then(colour_at) else {
                continue;
            };
            let premultiply = |value: f64| (value * alpha / 255.0).round() as u8;
            if let Some(pixel) = tiny_skia::PremultipliedColorU8::from_rgba(
                premultiply(red),
                premultiply(green),
                premultiply(blue),
                alpha.round() as u8,
            ) && let Some(slot) = pixels.get_mut((row * width + column) as usize)
            {
                *slot = pixel;
            }
        }
    }
    Some(pixmap)
}

/// The map from an image's pixel space, `columns` by `rows` with y growing downwards, to the
/// unit square a [`Mark::Image`]'s placement starts from.
fn image_space(columns: usize, rows: usize) -> Affine {
    Affine([1.0 / columns as f64, 0.0, 0.0, -1.0 / rows as f64, 0.0, 1.0])
}

/// An image's pixels as a premultiplied pixmap.
fn image_pixmap(pixels: &[Colour], columns: usize, rows: usize) -> Option<tiny_skia::Pixmap> {
    let size =
        tiny_skia::IntSize::from_wh(u32::try_from(columns).ok()?, u32::try_from(rows).ok()?)?;
    let data = pixels
        .iter()
        .flat_map(|pixel| {
            let [red, green, blue, a] = channels(*pixel);
            let premultiply = |value: u8| ((u16::from(value) * u16::from(a) + 127) / 255) as u8;
            [premultiply(red), premultiply(green), premultiply(blue), a]
        })
        .collect();
    tiny_skia::Pixmap::from_vec(data, size)
}

/// `image` scaled to `size` pixels by bilinear filtering, its edge pixels extending outwards.
fn smooth_scale(image: &tiny_skia::Pixmap, size: [usize; 2]) -> Option<tiny_skia::Pixmap> {
    let [width, height] = size.map(|extent| u32::try_from(extent).ok());
    let mut scaled = tiny_skia::Pixmap::new(width?, height?)?;
    let paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..tiny_skia::PixmapPaint::default()
    };
    let transform = tiny_skia::Transform::from_scale(
        size[0] as f32 / image.width() as f32,
        size[1] as f32 / image.height() as f32,
    );
    scaled.draw_pixmap(0, 0, image.as_ref(), &paint, transform, None);
    Some(scaled)
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let [first, second, third] = [0, 1, 2].map(|index| chunk.get(index).copied().unwrap_or(0));
        let group = (u32::from(first) << 16) | (u32::from(second) << 8) | u32::from(third);
        for position in 0..4 {
            if position <= chunk.len() {
                encoded.push(char::from(
                    ALPHABET[(group >> (18 - 6 * position)) as usize & 63],
                ));
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

fn solid_paint(colour: Colour) -> tiny_skia::Paint<'static> {
    let [red, green, blue, a] = channels(colour);
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(red, green, blue, a);
    paint.anti_alias = true;
    paint
}

fn skia_rule(rule: FillRule) -> tiny_skia::FillRule {
    match rule {
        FillRule::Winding => tiny_skia::FillRule::Winding,
        FillRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
    }
}

/// The outline as a tiny-skia path, or `None` when it is empty or any of its points lands
/// outside the finite range of `f32` once mapped `to_pixels`, which tiny-skia's rasterizer
/// assumes never happens.
fn skia_path(outline: &[Segment], to_pixels: Affine) -> Option<tiny_skia::Path> {
    let finite = |point: &[f64; 2]| {
        to_pixels
            .apply(*point)
            .iter()
            .all(|value| (*value as f32).is_finite())
    };
    let all_finite = outline.iter().all(|segment| match segment {
        Segment::Move(to) | Segment::Line(to) => finite(to),
        Segment::Cubic(first, second, end) => [first, second, end].into_iter().all(finite),
        Segment::Close => true,
    });
    if !all_finite {
        return None;
    }
    let mut builder = tiny_skia::PathBuilder::new();
    let point = |[x, y]: [f64; 2]| (x as f32, y as f32);
    for segment in outline {
        match *segment {
            Segment::Move(to) => {
                let (x, y) = point(to);
                builder.move_to(x, y);
            }
            Segment::Line(to) => {
                let (x, y) = point(to);
                builder.line_to(x, y);
            }
            Segment::Cubic(first, second, end) => {
                let (ax, ay) = point(first);
                let (bx, by) = point(second);
                let (x, y) = point(end);
                builder.cubic_to(ax, ay, bx, by, x, y);
            }
            Segment::Close => builder.close(),
        }
    }
    builder.finish()
}

fn skia_stroke(stroke: &Stroke) -> tiny_skia::Stroke {
    use tiny_skia::{LineCap, LineJoin, StrokeDash};
    let inches_per_lwd = 1.0 / 96.0;
    let pattern: Vec<f32> = dashes(stroke).iter().map(|length| *length as f32).collect();
    tiny_skia::Stroke {
        width: (stroke.lwd * inches_per_lwd) as f32,
        miter_limit: stroke.lmitre as f32,
        line_cap: match stroke.lend {
            1 => LineCap::Butt,
            2 => LineCap::Square,
            _ => LineCap::Round,
        },
        line_join: match stroke.ljoin {
            1 => LineJoin::Miter,
            2 => LineJoin::Bevel,
            _ => LineJoin::Round,
        },
        dash: if pattern.len() >= 2 && pattern.len() % 2 == 0 {
            StrokeDash::new(pattern, 0.0)
        } else {
            None
        },
    }
}

/// The outline of a line of text in inches, laid out with the font's own advances.
fn text_outline(
    face: &ab_glyph::FontVec,
    text: &str,
    points: f64,
    origin: [f64; 2],
    rotation: f64,
    hadj: f64,
) -> Outline {
    use ab_glyph::Font;
    let Some(units) = face.units_per_em() else {
        return Vec::new();
    };
    let scale = points / POINTS_PER_INCH / f64::from(units);
    let glyphs: Vec<_> = text
        .chars()
        .map(|character| face.glyph_id(character))
        .collect();
    let width: f64 = glyphs
        .iter()
        .map(|glyph| f64::from(face.h_advance_unscaled(*glyph)))
        .sum::<f64>()
        * scale;
    let (sin, cos) = rotation.to_radians().sin_cos();
    let mut outline = Vec::new();
    let mut pen = 0.0;
    for glyph in glyphs {
        let place = Affine([
            scale * cos,
            scale * sin,
            -scale * sin,
            scale * cos,
            origin[0] + (pen - hadj * width) * cos,
            origin[1] + (pen - hadj * width) * sin,
        ]);
        glyph_outline(face, glyph, place, &mut outline);
        pen += f64::from(face.h_advance_unscaled(glyph)) * scale;
    }
    outline
}

/// Appends a glyph's outline, in font units mapped through `place`, with its quadratic curves
/// raised to cubics.
fn glyph_outline(
    face: &ab_glyph::FontVec,
    glyph: ab_glyph::GlyphId,
    place: Affine,
    outline: &mut Outline,
) {
    use ab_glyph::{Font, OutlineCurve};
    let Some(curves) = face.outline(glyph) else {
        return;
    };
    let at = |point: ab_glyph::Point| place.apply([f64::from(point.x), f64::from(point.y)]);
    let mut last: Option<ab_glyph::Point> = None;
    for curve in &curves.curves {
        let start = match curve {
            OutlineCurve::Line(start, _)
            | OutlineCurve::Quad(start, _, _)
            | OutlineCurve::Cubic(start, _, _, _) => *start,
        };
        if last != Some(start) {
            if last.is_some() {
                outline.push(Segment::Close);
            }
            outline.push(Segment::Move(at(start)));
        }
        match curve {
            OutlineCurve::Line(_, end) => {
                outline.push(Segment::Line(at(*end)));
                last = Some(*end);
            }
            OutlineCurve::Quad(from, control, end) => {
                let [from, control, to] = [at(*from), at(*control), at(*end)];
                let lerp = |a: [f64; 2], b: [f64; 2]| {
                    [
                        a[0] + (b[0] - a[0]) * 2.0 / 3.0,
                        a[1] + (b[1] - a[1]) * 2.0 / 3.0,
                    ]
                };
                outline.push(Segment::Cubic(lerp(from, control), lerp(to, control), to));
                last = Some(*end);
            }
            OutlineCurve::Cubic(_, first, second, end) => {
                outline.push(Segment::Cubic(at(*first), at(*second), at(*end)));
                last = Some(*end);
            }
        }
    }
    if last.is_some() {
        outline.push(Segment::Close);
    }
}

/// The system fonts rasters draw text with, faces 1 to 4: Liberation Sans, which has
/// Helvetica's metrics, or else DejaVu Sans or FreeSans. Without any of them, raster text is
/// not drawn.
fn raster_font(font: i32) -> Option<&'static ab_glyph::FontVec> {
    static FONTS: OnceLock<[Option<ab_glyph::FontVec>; 4]> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let candidates: [[&str; 3]; 4] = [
            [
                "liberation/LiberationSans-Regular.ttf",
                "dejavu/DejaVuSans.ttf",
                "freefont/FreeSans.ttf",
            ],
            [
                "liberation/LiberationSans-Bold.ttf",
                "dejavu/DejaVuSans-Bold.ttf",
                "freefont/FreeSansBold.ttf",
            ],
            [
                "liberation/LiberationSans-Italic.ttf",
                "dejavu/DejaVuSans-Oblique.ttf",
                "freefont/FreeSansOblique.ttf",
            ],
            [
                "liberation/LiberationSans-BoldItalic.ttf",
                "dejavu/DejaVuSans-BoldOblique.ttf",
                "freefont/FreeSansBoldOblique.ttf",
            ],
        ];
        candidates.map(|names| {
            names.iter().find_map(|name| {
                [
                    "/usr/share/fonts/truetype",
                    "/usr/share/fonts/TTF",
                    "/usr/share/fonts",
                ]
                .iter()
                .find_map(|directory| std::fs::read(format!("{directory}/{name}")).ok())
                .and_then(|bytes| ab_glyph::FontVec::try_from_vec(bytes).ok())
            })
        })
    });
    let index = match font {
        2 => 1,
        3 => 2,
        4 => 3,
        _ => 0,
    };
    fonts[index].as_ref().or(fonts[0].as_ref())
}

/// The pixels of a raster, without premultiplied alpha, row by row from the top.
fn rgba_rows(pixmap: &tiny_skia::Pixmap) -> Vec<[u8; 4]> {
    pixmap
        .pixels()
        .iter()
        .map(|pixel| {
            let colour = pixel.demultiply();
            [colour.red(), colour.green(), colour.blue(), colour.alpha()]
        })
        .collect()
}

/// An uncompressed 24-bit BMP, its rows bottom-up and padded to four bytes. Transparent
/// pixels are composited on white, since the format has no alpha.
fn bmp(pixmap: &tiny_skia::Pixmap) -> Vec<u8> {
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    let row_bytes = (3 * width).div_ceil(4) * 4;
    let image_bytes = row_bytes * height;
    let mut out = Vec::with_capacity(54 + image_bytes);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((54 + image_bytes) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(image_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    let pixels = rgba_rows(pixmap);
    for row in (0..height).rev() {
        let start = out.len();
        for pixel in &pixels[row * width..(row + 1) * width] {
            let [red, green, blue] = on_white(*pixel);
            out.extend_from_slice(&[blue, green, red]);
        }
        out.resize(start + row_bytes, 0);
    }
    out
}

fn on_white([red, green, blue, a]: [u8; 4]) -> [u8; 3] {
    let blend = |channel: u8| {
        let a = u32::from(a);
        ((u32::from(channel) * a + 255 * (255 - a) + 127) / 255) as u8
    };
    [blend(red), blend(green), blend(blue)]
}

/// An uncompressed baseline RGB TIFF, transparent pixels composited on white.
fn tiff(pixmap: &tiny_skia::Pixmap, resolution: f64) -> Vec<u8> {
    let (width, height) = (pixmap.width(), pixmap.height());
    let pixels = rgba_rows(pixmap);
    let image: Vec<u8> = pixels.iter().flat_map(|pixel| on_white(*pixel)).collect();
    // Header, then the image, then the bits-per-sample and resolution values, then the
    // directory, so every offset is known when it is written.
    let image_offset = 8u32;
    let bits_offset = image_offset + image.len() as u32;
    let resolution_offset = bits_offset + 6;
    let directory_offset = resolution_offset + 8;
    let mut out = Vec::with_capacity(directory_offset as usize + 200);
    out.extend_from_slice(b"II*\0");
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&image);
    for _ in 0..3 {
        out.extend_from_slice(&8u16.to_le_bytes());
    }
    out.extend_from_slice(&(resolution.round().max(1.0) as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    // Each entry: tag, type (3 short, 4 long, 5 rational), count, value or offset.
    let entries: [(u16, u16, u32, u32); 12] = [
        (256, 4, 1, width),
        (257, 4, 1, height),
        (258, 3, 3, bits_offset),
        (259, 3, 1, 1),
        (262, 3, 1, 2),
        (273, 4, 1, image_offset),
        (277, 3, 1, 3),
        (278, 4, 1, height),
        (279, 4, 1, image.len() as u32),
        (282, 5, 1, resolution_offset),
        (283, 5, 1, resolution_offset),
        (296, 3, 1, 2),
    ];
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, kind, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

/// A baseline JPEG at `quality`, transparent pixels composited on white.
fn jpeg(pixmap: &tiny_skia::Pixmap, resolution: f64, quality: u8) -> std::io::Result<Vec<u8>> {
    let (Ok(width), Ok(height)) = (
        u16::try_from(pixmap.width()),
        u16::try_from(pixmap.height()),
    ) else {
        return Err(std::io::Error::other("the page is too large for JPEG"));
    };
    let image: Vec<u8> = rgba_rows(pixmap)
        .iter()
        .flat_map(|pixel| on_white(*pixel))
        .collect();
    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, quality.clamp(1, 100));
    let density = resolution.round().clamp(1.0, f64::from(u16::MAX)) as u16;
    encoder.set_density(jpeg_encoder::PixelDensity::dpi(density));
    encoder
        .encode(&image, width, height, jpeg_encoder::ColorType::Rgb)
        .map_err(|failure| std::io::Error::other(failure.to_string()))?;
    Ok(out)
}

// The built-in devices.

/// What a built-in device is opened with.
pub struct DeviceSpec {
    pub name: &'static str,
    pub file: Option<String>,
    /// What closing the device writes; a device with none (`xfig`, `pictex`) writes nothing.
    pub format: Option<Format>,
    /// Width and height in inches.
    pub size: [f64; 2],
    /// The size of a device unit in inches: a big point for vector devices, a pixel for
    /// bitmap ones.
    pub unit_inches: f64,
    /// Whether device y coordinates grow downwards, as in a bitmap.
    pub y_down: bool,
    pub pointsize: f64,
    pub bg: Colour,
    pub fg: Colour,
    pub title: String,
    pub quality: u8,
}

/// A built-in device's own state, which its `DevDesc` points to: the pages and definitions
/// drawn so far, which closing the device renders, in inches from the bottom-left corner.
struct RenderDevice {
    format: Option<Format>,
    file: Option<String>,
    size: [f64; 2],
    unit_inches: f64,
    y_down: bool,
    drawing: Drawing,
    clip: Clip,
    mask: Option<usize>,
    /// Where marks go while a pattern, mask, group or path is being defined, innermost last;
    /// with none, marks go onto the current page.
    targets: Vec<Target>,
    /// Whether the PostScript device has warned on this page that it dropped a
    /// semi-transparent colour.
    warned_transparency: bool,
    title: String,
    quality: u8,
}

enum Target {
    Items(Vec<Item>),
    Path(PathCapture),
}

/// The outline a path-drawing function builds for a clipping path, a stroke or a fill. Text
/// joins it as glyph outlines.
struct PathCapture {
    outline: Outline,
    text: bool,
    drawing: bool,
}

/// Opens a built-in device as an engine device and makes it current. A vector device creates
/// its file at once; a bitmap device writes one file per page and so none until it draws.
pub unsafe fn open_device(spec: DeviceSpec) -> R<()> {
    if !(spec.size[0] > 0.0 && spec.size[1] > 0.0)
        || !spec.pointsize.is_finite()
        || spec.pointsize <= 0.0
    {
        return Err(error(format!("unable to start {} device", spec.name)));
    }
    check_device_available()?;
    let bitmap = spec.format.is_some_and(Format::is_raster) || spec.y_down;
    if !bitmap
        && let Some(path) = spec
            .file
            .as_deref()
            .filter(|file| !file.starts_with('|'))
            .map(|file| file_for_page(file, 1))
    {
        std::fs::File::create(&path).map_err(|_| error(format!("cannot open file '{path}'")))?;
    }
    let dev = libc::calloc(1, std::mem::size_of::<DevDesc>()).cast::<DevDesc>();
    if dev.is_null() {
        return Err(error(format!("unable to start {} device", spec.name)));
    }
    let unit = spec.unit_inches;
    let [width, height] = spec.size.map(|inches| inches / unit);
    let (bottom, top) = if spec.y_down {
        (height, 0.0)
    } else {
        (0.0, height)
    };
    let d = &mut *dev;
    d.left = 0.0;
    d.right = width;
    d.bottom = bottom;
    d.top = top;
    d.clip_left = 0.0;
    d.clip_right = width;
    d.clip_bottom = bottom;
    d.clip_top = top;
    d.x_char_offset = 0.4900;
    d.y_char_offset = 0.3333;
    d.y_line_bias = 0.2;
    d.ipr = [unit, unit];
    d.cra = [
        0.9 * spec.pointsize / 72.0 / unit,
        1.2 * spec.pointsize / 72.0 / unit,
    ];
    d.gamma = 1.0;
    d.can_clip = 1;
    d.can_change_gamma = 0;
    d.can_hadj = 2;
    d.startps = spec.pointsize;
    d.startcol = spec.fg as c_int;
    d.startfill = spec.bg as c_int;
    d.startlty = 0;
    d.startfont = 1;
    d.startgamma = 1.0;
    d.display_list_on = 0;
    d.circle = Some(device_circle);
    d.clip = Some(device_clip);
    d.close = Some(device_close);
    d.line = Some(device_line);
    d.metric_info = Some(device_metric_info);
    d.new_page = Some(device_new_page);
    d.polygon = Some(device_polygon);
    d.polyline = Some(device_polyline);
    d.rect = Some(device_rect);
    d.path = Some(device_path);
    d.raster = Some(device_raster);
    d.size = Some(device_size);
    d.str_width = Some(device_str_width);
    d.text = Some(device_text);
    d.has_text_utf8 = 0;
    d.want_symbol_utf8 = 0;
    d.use_rotated_text_in_contour = 0;
    let postscript = matches!(spec.format, Some(Format::Postscript { .. }));
    d.have_transparency = if postscript { 1 } else { 2 };
    d.have_transparent_bg = if bitmap && spec.format != Some(Format::Png) {
        1
    } else {
        2
    };
    d.have_raster = 1;
    d.have_capture = 1;
    d.have_locator = 1;
    d.device_clip = 0;
    match spec.format {
        // GNU R's PostScript device declares groups' version and then defines nothing: every
        // definition is refused, and paths draw nothing.
        Some(Format::Postscript { .. }) => {
            d.device_version = VERSION_GROUP;
            d.set_pattern = Some(refuse_pattern);
            d.set_clip_path = Some(refuse_definition);
            d.set_mask = Some(refuse_definition);
            d.define_group = Some(refuse_group);
            d.use_group = Some(ignore_group);
            d.stroke = Some(ignore_stroke);
            d.fill = Some(ignore_fill);
            d.fill_stroke = Some(ignore_fill);
            d.capabilities = Some(unchanged_capabilities);
        }
        Some(format) => {
            d.device_version = VERSION_GLYPHS;
            d.set_pattern = Some(device_set_pattern);
            d.set_clip_path = Some(device_set_clip_path);
            d.set_mask = Some(device_set_mask);
            d.define_group = Some(device_define_group);
            d.use_group = Some(device_use_group);
            d.stroke = Some(device_stroke);
            d.fill = Some(device_fill);
            d.fill_stroke = Some(device_fill_stroke);
            d.glyph = Some(device_glyph);
            d.capabilities = Some(if format.is_raster() {
                raster_capabilities
            } else {
                vector_capabilities
            });
        }
        // A device that writes nothing predates definitions, which graphics systems still
        // release at each new page.
        None => {
            d.device_version = 0;
            d.set_pattern = Some(refuse_pattern);
            d.set_clip_path = Some(refuse_definition);
            d.set_mask = Some(refuse_definition);
        }
    }
    // Definitions are only ever appended, so releasing one has nothing to free.
    d.release_pattern = Some(release_nothing);
    d.release_clip_path = Some(release_nothing);
    d.release_mask = Some(release_nothing);
    d.release_group = Some(release_nothing);
    d.device_specific = Box::into_raw(Box::new(RenderDevice {
        format: spec.format,
        file: spec.file.clone(),
        size: spec.size,
        unit_inches: unit,
        y_down: spec.y_down,
        drawing: Drawing {
            pages: Vec::new(),
            definitions: Vec::new(),
        },
        clip: Clip::Rect(Rect {
            x0: 0.0,
            x1: spec.size[0],
            y0: 0.0,
            y1: spec.size[1],
        }),
        mask: None,
        targets: Vec::new(),
        warned_transparency: false,
        title: spec.title,
        quality: spec.quality,
    }))
    .cast();
    let dd = create_device(dev)?;
    add_device_named(dd, spec.name, spec.file.as_deref())
}

unsafe fn render_device<'a>(dd: *mut DevDesc) -> &'a mut RenderDevice {
    &mut *(*dd).device_specific.cast::<RenderDevice>()
}

impl RenderDevice {
    fn point(&self, x: f64, y: f64) -> [f64; 2] {
        let y = y * self.unit_inches;
        [
            x * self.unit_inches,
            if self.y_down { self.size[1] - y } else { y },
        ]
    }

    /// The map from device units to inches.
    fn from_device(&self) -> Affine {
        let unit = self.unit_inches;
        if self.y_down {
            Affine([unit, 0.0, 0.0, -unit, 0.0, self.size[1]])
        } else {
            Affine([unit, 0.0, 0.0, unit, 0.0, 0.0])
        }
    }

    fn whole_page(&self) -> Clip {
        Clip::Rect(Rect {
            x0: 0.0,
            x1: self.size[0],
            y0: 0.0,
            y1: self.size[1],
        })
    }

    /// The index of the definition `reference` names, when it names one `accept` accepts.
    unsafe fn definition(
        &self,
        reference: Sexp,
        accept: impl Fn(&Definition) -> bool,
    ) -> Option<usize> {
        if reference == nil() || length(reference) == 0 {
            return None;
        }
        let index = usize::try_from(values::as_integer_scalar(reference)).ok()?;
        self.drawing
            .definitions
            .get(index)
            .filter(|definition| accept(definition))
            .map(|_| index)
    }

    /// Checks a colour as GNU R's PostScript device does: it draws only opaque colours, and
    /// warns once per page when it drops a semi-transparent one. Other devices draw any.
    fn checked(&mut self, colour: c_int) -> (Colour, Option<&'static str>) {
        let colour = colour as Colour;
        let level = alpha(colour);
        if !matches!(self.format, Some(Format::Postscript { .. })) || level == 0 || level == 255 {
            return (colour, None);
        }
        let warning = (!self.warned_transparency).then_some(
            "semi-transparency is not supported on this device: reported only once per page",
        );
        self.warned_transparency = true;
        (TRANSPARENT_WHITE as Colour, warning)
    }
}

/// Raises a warning from inside a device callback, passing an error the warning turns into
/// back to the engine.
unsafe fn warn(message: Option<&str>) {
    if let Some(message) = message
        && let Err(unwind) = crate::conditions::warning(message)
    {
        fail_with(unwind);
    }
}

/// Adds a mark where marks currently go. The device's state is no longer borrowed when the
/// warning is raised, since a warning handler may draw on the device itself.
unsafe fn draw(dd: *mut DevDesc, mark: Mark) {
    let device = render_device(dd);
    let (clip, mask) = (device.clip, device.mask);
    let pdf = device.format == Some(Format::Pdf);
    let warning = match device.targets.last_mut() {
        Some(Target::Items(items)) => {
            items.push(Item { clip, mask, mark });
            None
        }
        Some(Target::Path(capture)) => append_to_path(capture, mark, pdf),
        None => {
            if device.drawing.pages.is_empty() {
                device.drawing.pages.push(Page {
                    background: TRANSPARENT_WHITE as Colour,
                    items: Vec::new(),
                });
            }
            if let Some(page) = device.drawing.pages.last_mut() {
                page.items.push(Item { clip, mask, mark });
            }
            None
        }
    };
    warn(warning);
}

/// Adds a mark's outline to a path being built. GNU R's PDF device builds a path either from
/// text or from other drawing, never both, and warns about the part it drops; groups and
/// raster images never join a path.
fn append_to_path(capture: &mut PathCapture, mark: Mark, pdf: bool) -> Option<&'static str> {
    match mark {
        Mark::Path { outline, .. } => {
            if pdf && capture.text {
                return Some("Drawing not appended to path (contains text)");
            }
            capture.outline.extend(outline);
            capture.drawing = true;
        }
        Mark::Text {
            origin,
            text,
            points,
            font,
            rotation,
            hadj,
            ..
        } => {
            if pdf && capture.drawing {
                return Some("Text not added to path containing other drawing");
            }
            if let Some(face) = raster_font(font) {
                capture
                    .outline
                    .extend(text_outline(face, &text, points, origin, rotation, hadj));
            }
            capture.text = true;
        }
        Mark::Group { .. } | Mark::Image { .. } => {}
    }
    None
}

/// Calls each of `functions` with marks going into a list of their own, which it returns. The
/// clipping region in effect is restored afterwards, as the definition of a pattern, mask or
/// group is drawn inside a save and restore of the graphics state, and no mask applies inside
/// one. A function that fails still restores the state.
unsafe fn record<const N: usize>(dd: *mut DevDesc, functions: [Sexp; N]) -> R<[Vec<Item>; N]> {
    let saved = {
        let device = render_device(dd);
        let saved = (device.clip, device.mask);
        device.mask = None;
        saved
    };
    let mut recorded: [Vec<Item>; N] = std::array::from_fn(|_| Vec::new());
    let mut outcome = Ok(());
    for (function, items) in functions.into_iter().zip(&mut recorded) {
        if function == nil() {
            continue;
        }
        render_device(dd).targets.push(Target::Items(Vec::new()));
        outcome = eval::apply(nil(), function, &[], values::global_env()).map(|_| ());
        if let Some(Target::Items(drawn)) = render_device(dd).targets.pop() {
            *items = drawn;
        }
        if outcome.is_err() {
            break;
        }
    }
    let device = render_device(dd);
    (device.clip, device.mask) = saved;
    outcome.map(|()| recorded)
}

/// Calls a path-drawing function and returns the outline it builds.
unsafe fn capture_path(dd: *mut DevDesc, function: Sexp) -> R<Outline> {
    render_device(dd).targets.push(Target::Path(PathCapture {
        outline: Vec::new(),
        text: false,
        drawing: false,
    }));
    let outcome = eval::apply(nil(), function, &[], values::global_env());
    let outline = match render_device(dd).targets.pop() {
        Some(Target::Path(capture)) => capture.outline,
        _ => Vec::new(),
    };
    outcome.map(|_| outline)
}

/// Element `index` of a numeric vector, `NaN` for anything else.
unsafe fn number_at(vector: Sexp, index: usize) -> f64 {
    match vector.kind() {
        SexpType::Real => values::reals(vector)
            .get(index)
            .copied()
            .unwrap_or(f64::NAN),
        SexpType::Integer | SexpType::Logical => values::ints(vector)
            .get(index)
            .filter(|value| **value != i32::MIN)
            .map_or(f64::NAN, |value| f64::from(*value)),
        _ => f64::NAN,
    }
}

/// A definition's reference, the integer vector R holds on to.
fn reference(index: usize) -> Option<Sexp> {
    let index = i32::try_from(index).ok()?;
    // SAFETY: the engine roots the value a callback returns.
    match unsafe { values::integer_vector(&[index]) } {
        Ok(reference) => Some(reference),
        Err(unwind) => {
            fail_with(unwind);
            None
        }
    }
}

unsafe fn define(dd: *mut DevDesc, definition: Definition) -> Option<Sexp> {
    let definitions = &mut render_device(dd).drawing.definitions;
    definitions.push(definition);
    reference(definitions.len() - 1)
}

unsafe extern "C" fn device_set_pattern(pattern: Sexp, dd: *mut DevDesc) -> Option<Sexp> {
    match read_pattern(dd, pattern) {
        Ok(pattern) => {
            // GNU R's pdf device warns once for a gradient's colours, and once more for its
            // opacities when it has any.
            if let Pattern::Linear { stops, extend, .. } | Pattern::Radial { stops, extend, .. } =
                &pattern
                && matches!(extend, Extend::Repeat | Extend::Reflect)
                && render_device(dd).format == Some(Format::Pdf)
            {
                let translucent = stops.iter().any(|stop| alpha(stop.colour) < 255);
                for _ in 0..1 + usize::from(translucent) {
                    warn(Some(
                        "Repeat or reflect pattern not supported on PDF device",
                    ));
                }
            }
            define(dd, Definition::Pattern(pattern))
        }
        Err(unwind) => {
            fail_with(unwind);
            None
        }
    }
}

/// A pattern as grDevices describes it in a list whose first element is its type.
unsafe fn read_pattern(dd: *mut DevDesc, pattern: Sexp) -> R<Pattern> {
    let element = |index: usize| values::list_elt(pattern, index);
    let number = |index: usize| values::as_real_scalar(element(index));
    let extend = |index: usize| match values::as_integer_scalar(element(index)) {
        2 => Extend::Repeat,
        3 => Extend::Reflect,
        4 => Extend::None,
        _ => Extend::Pad,
    };
    let stops = |offsets: usize, colours: usize| -> Vec<Stop> {
        let (offsets, colours) = (element(offsets), element(colours));
        (0..length(offsets).min(length(colours)))
            .map(|index| Stop {
                offset: number_at(offsets, index),
                colour: values::string_at(colours, index)
                    .and_then(|spec| parse_colour_spec(&spec).ok())
                    .unwrap_or(TRANSPARENT_WHITE as Colour),
            })
            .collect()
    };
    if pattern.kind() != SexpType::List || length(pattern) < 7 {
        return Err(error("invalid pattern"));
    }
    let device = render_device(dd);
    let unit = device.unit_inches;
    Ok(match values::as_integer_scalar(element(0)) {
        1 => Pattern::Linear {
            start: device.point(number(1), number(2)),
            end: device.point(number(3), number(4)),
            stops: stops(5, 6),
            extend: extend(7),
        },
        2 => Pattern::Radial {
            start: device.point(number(1), number(2)),
            start_radius: number(3) * unit,
            end: device.point(number(4), number(5)),
            end_radius: number(6) * unit,
            stops: stops(7, 8),
            extend: extend(9),
        },
        3 => {
            let [x, y, width, height] = [number(2), number(3), number(4), number(5)];
            let [a0, b0] = device.point(x, y);
            let [a1, b1] = device.point(x + width, y + height);
            let rect = Rect {
                x0: a0.min(a1),
                x1: a0.max(a1),
                y0: b0.min(b1),
                y1: b0.max(b1),
            };
            let extend = extend(6);
            let [items] = record(dd, [element(1)])?;
            Pattern::Tiling {
                rect,
                items,
                extend,
            }
        }
        _ => return Err(error("invalid pattern type")),
    })
}

unsafe extern "C" fn device_set_clip_path(
    path: Sexp,
    reference: Sexp,
    dd: *mut DevDesc,
) -> Option<Sexp> {
    let device = render_device(dd);
    if let Some(index) = device.definition(reference, |definition| {
        matches!(definition, Definition::ClipPath { .. })
    }) {
        device.clip = Clip::Path(index);
        return Some(reference);
    }
    let rule = match values::get_attr(path, values::install("rule").ok()?) {
        rule if rule != nil() && values::as_integer_scalar(rule) == 2 => FillRule::EvenOdd,
        _ => FillRule::Winding,
    };
    let outline = match capture_path(dd, path) {
        Ok(outline) => outline,
        Err(unwind) => {
            fail_with(unwind);
            return None;
        }
    };
    let reference = define(dd, Definition::ClipPath { outline, rule })?;
    let device = render_device(dd);
    device.clip = Clip::Path(device.drawing.definitions.len() - 1);
    Some(reference)
}

unsafe extern "C" fn device_set_mask(
    mask: Sexp,
    reference: Sexp,
    dd: *mut DevDesc,
) -> Option<Sexp> {
    let device = render_device(dd);
    if mask == nil() {
        device.mask = None;
        return None;
    }
    if let Some(index) = device.definition(reference, |definition| {
        matches!(definition, Definition::Mask { .. })
    }) {
        device.mask = Some(index);
        return Some(reference);
    }
    let luminance = match values::get_attr(mask, values::install("type").ok()?) {
        kind if kind != nil() => values::as_integer_scalar(kind) == 2,
        _ => false,
    };
    let items = match record(dd, [mask]) {
        Ok([items]) => items,
        Err(unwind) => {
            fail_with(unwind);
            return None;
        }
    };
    let reference = define(dd, Definition::Mask { luminance, items })?;
    let device = render_device(dd);
    device.mask = Some(device.drawing.definitions.len() - 1);
    Some(reference)
}

unsafe extern "C" fn device_define_group(
    source: Sexp,
    operator: c_int,
    destination: Sexp,
    dd: *mut DevDesc,
) -> Option<Sexp> {
    let Some(mut operator) = Operator::from_code(operator) else {
        fail("invalid compositing operator");
        return None;
    };
    // PDF and SVG only have the blend modes, so as GNU R's PDF device does, any other
    // operator composites as `over`.
    if !render_device(dd).format.is_some_and(Format::is_raster)
        && operator != Operator::Over
        && operator.blend_name().is_none()
    {
        warn(Some(
            "Compositing operator has no corresponding blend mode; defaulting to Normal",
        ));
        operator = Operator::Over;
    }
    let [destination, source] = match record(dd, [destination, source]) {
        Ok(recorded) => recorded,
        Err(unwind) => {
            fail_with(unwind);
            return None;
        }
    };
    define(
        dd,
        Definition::Group {
            destination,
            operator,
            source,
        },
    )
}

/// Draws a group through a transformation of device coordinates, a 3 by 3 matrix whose rows
/// are filled in R's column order, so its elements `t` map `(x, y)` to
/// `(t0 x + t1 y + t2, t3 x + t4 y + t5)`.
unsafe extern "C" fn device_use_group(reference: Sexp, transform: Sexp, dd: *mut DevDesc) {
    let device = render_device(dd);
    let Some(group) = device.definition(reference, |definition| {
        matches!(definition, Definition::Group { .. })
    }) else {
        return;
    };
    let transform = if transform != nil() && length(transform) >= 6 {
        let t: Vec<f64> = (0..6).map(|index| number_at(transform, index)).collect();
        let in_device = Affine([t[0], t[3], t[1], t[4], t[2], t[5]]);
        let from_device = device.from_device();
        match from_device.invert() {
            Some(to_device) => to_device.then(in_device).then(from_device),
            None => Affine::IDENTITY,
        }
    } else {
        Affine::IDENTITY
    };
    draw(dd, Mark::Group { group, transform });
}

unsafe extern "C" fn device_stroke(path: Sexp, gc: *const GEContext, dd: *mut DevDesc) {
    draw_captured(dd, path, None, &*gc);
}

unsafe extern "C" fn device_fill(path: Sexp, rule: c_int, gc: *const GEContext, dd: *mut DevDesc) {
    let gc = GEContext {
        col: TRANSPARENT_WHITE,
        ..*gc
    };
    draw_captured(dd, path, Some(rule), &gc);
}

unsafe extern "C" fn device_fill_stroke(
    path: Sexp,
    rule: c_int,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    draw_captured(dd, path, Some(rule), &*gc);
}

/// Draws the outline a path-drawing function builds, filled by `rule` (1 nonzero winding, 2
/// even-odd) unless `rule` is `None`, and stroked as `gc` says.
unsafe fn draw_captured(dd: *mut DevDesc, path: Sexp, rule: Option<c_int>, gc: &GEContext) {
    let outline = match capture_path(dd, path) {
        Ok(outline) => outline,
        Err(unwind) => return fail_with(unwind),
    };
    let fill = match rule {
        Some(_) => fill(dd, gc),
        None => None,
    };
    let rule = if rule == Some(2) {
        FillRule::EvenOdd
    } else {
        FillRule::Winding
    };
    let stroke = stroke(dd, gc);
    draw(
        dd,
        Mark::Path {
            outline,
            fill,
            rule,
            stroke,
        },
    );
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn device_glyph(
    count: c_int,
    glyphs: *mut c_int,
    x: *mut f64,
    y: *mut f64,
    font: Sexp,
    size: f64,
    colour: c_int,
    rotation: f64,
    dd: *mut DevDesc,
) {
    let count = usize::try_from(count).unwrap_or(0);
    if count == 0 || glyphs.is_null() || x.is_null() || y.is_null() {
        return;
    }
    let file = values::string_at(values::list_elt(font, 0), 0).map(|file| file.into_owned());
    let index = u32::try_from(values::as_integer_scalar(values::list_elt(font, 1))).unwrap_or(0);
    // GNU R's PDF device draws nothing for a font file it cannot read, without a warning.
    let Some(face) = file.and_then(|file| font_file(&file, index)) else {
        return;
    };
    let Some(units) = ab_glyph::Font::units_per_em(&*face) else {
        return;
    };
    let device = render_device(dd);
    let scale = size / POINTS_PER_INCH / f64::from(units);
    let (sin, cos) = rotation.to_radians().sin_cos();
    let glyphs = std::slice::from_raw_parts(glyphs, count);
    let x = std::slice::from_raw_parts(x, count);
    let y = std::slice::from_raw_parts(y, count);
    let mut outline = Vec::new();
    for ((glyph, x), y) in glyphs.iter().zip(x).zip(y) {
        let Ok(id) = u16::try_from(*glyph) else {
            continue;
        };
        let [ox, oy] = device.point(*x, *y);
        let place = Affine([scale * cos, scale * sin, -scale * sin, scale * cos, ox, oy]);
        glyph_outline(&face, ab_glyph::GlyphId(id), place, &mut outline);
    }
    let (colour, warning) = device.checked(colour);
    warn(warning);
    draw(
        dd,
        Mark::Path {
            outline,
            fill: Some(Fill::Colour(colour)),
            rule: FillRule::Winding,
            stroke: None,
        },
    );
}

/// A font file's face `index`, loaded once per session.
fn font_file(file: &str, index: u32) -> Option<Rc<ab_glyph::FontVec>> {
    thread_local! {
        static FONTS: RefCell<BTreeMap<(String, u32), Option<Rc<ab_glyph::FontVec>>>> =
            const { RefCell::new(BTreeMap::new()) };
    }
    FONTS.with(|fonts| {
        fonts
            .borrow_mut()
            .entry((file.to_owned(), index))
            .or_insert_with(|| {
                let bytes = std::fs::read(file).ok()?;
                ab_glyph::FontVec::try_from_vec_and_index(bytes, index)
                    .ok()
                    .map(Rc::new)
            })
            .clone()
    })
}

/// Fills in what a PDF or SVG device supports, in `R_GE_capability_*` order from index 6:
/// patterns, clipping paths, masks, compositing operators (the blend modes only), affine
/// transformations, paths and glyphs.
unsafe extern "C" fn vector_capabilities(capabilities: Sexp) -> Option<Sexp> {
    let compositing: Vec<i32> = (15..=25).collect();
    refine_capabilities(capabilities, &compositing)
}

/// Like [`vector_capabilities`], for the raster devices, which composite with every operator,
/// listed in the order GNU R's cairo devices list them. Unlike cairo, they draw luminance masks.
unsafe extern "C" fn raster_capabilities(capabilities: Sexp) -> Option<Sexp> {
    let compositing: Vec<i32> = (15..=25).chain(1..=14).collect();
    refine_capabilities(capabilities, &compositing)
}

unsafe fn refine_capabilities(capabilities: Sexp, compositing: &[i32]) -> Option<Sexp> {
    if capabilities.kind() != SexpType::List || length(capabilities) < 13 {
        return Some(capabilities);
    }
    let entries: [(usize, &[i32]); 7] = [
        (6, &[1, 2, 3]),
        (7, &[1]),
        (8, &[1, 2]),
        (9, compositing),
        (10, &[1]),
        (11, &[1]),
        (12, &[1]),
    ];
    for (index, codes) in entries {
        match values::integer_vector(codes) {
            Ok(value) => values::set_elt(capabilities, index, value),
            Err(unwind) => {
                fail_with(unwind);
                return None;
            }
        }
    }
    Some(capabilities)
}

unsafe extern "C" fn unchanged_capabilities(capabilities: Sexp) -> Option<Sexp> {
    Some(capabilities)
}

unsafe extern "C" fn refuse_pattern(_pattern: Sexp, _dd: *mut DevDesc) -> Option<Sexp> {
    None
}

unsafe extern "C" fn refuse_definition(
    _definition: Sexp,
    _reference: Sexp,
    _dd: *mut DevDesc,
) -> Option<Sexp> {
    None
}

unsafe extern "C" fn refuse_group(
    _source: Sexp,
    _operator: c_int,
    _destination: Sexp,
    _dd: *mut DevDesc,
) -> Option<Sexp> {
    None
}

unsafe extern "C" fn ignore_group(_reference: Sexp, _transform: Sexp, _dd: *mut DevDesc) {}

unsafe extern "C" fn ignore_stroke(_path: Sexp, _gc: *const GEContext, _dd: *mut DevDesc) {}

unsafe extern "C" fn ignore_fill(
    _path: Sexp,
    _rule: c_int,
    _gc: *const GEContext,
    _dd: *mut DevDesc,
) {
}

unsafe extern "C" fn release_nothing(_reference: Sexp, _dd: *mut DevDesc) {}

/// The stroke a context describes, converting the engine's line end and join codes, which
/// count from 1, to `par`'s, which count from 0.
unsafe fn stroke(dd: *mut DevDesc, gc: &GEContext) -> Option<Stroke> {
    let (colour, warning) = render_device(dd).checked(gc.col);
    warn(warning);
    Some(Stroke {
        colour,
        lwd: gc.lwd,
        lty: gc.lty as u32,
        lend: gc.lend - 1,
        ljoin: gc.ljoin - 1,
        lmitre: gc.lmitre,
    })
}

/// The fill a context describes: its pattern when it has one, else its colour.
unsafe fn fill(dd: *mut DevDesc, gc: &GEContext) -> Option<Fill> {
    let device = render_device(dd);
    if let Some(index) = device.definition(gc.pattern_fill, |definition| {
        matches!(definition, Definition::Pattern(_))
    }) {
        return Some(Fill::Pattern(index));
    }
    let (colour, warning) = device.checked(gc.fill);
    warn(warning);
    Some(Fill::Colour(colour))
}

/// The text size a context asks for, in points. GNU R's PDF and PostScript devices round it
/// to a whole point, for metrics as well as glyphs; its Cairo devices do not.
fn text_points(device: &RenderDevice, gc: &GEContext) -> f64 {
    let points = gc.ps * gc.cex;
    match device.format {
        Some(Format::Pdf | Format::Postscript { .. }) => (points + 0.5).floor().max(1.0),
        _ => points,
    }
}

unsafe fn draw_outline(dd: *mut DevDesc, outline: Outline, rule: FillRule, gc: &GEContext) {
    let fill = fill(dd, gc);
    let stroke = stroke(dd, gc);
    draw(
        dd,
        Mark::Path {
            outline,
            fill,
            rule,
            stroke,
        },
    );
}

unsafe fn draw_line(dd: *mut DevDesc, outline: Outline, gc: &GEContext) {
    let stroke = stroke(dd, gc);
    draw(
        dd,
        Mark::Path {
            outline,
            fill: None,
            rule: FillRule::Winding,
            stroke,
        },
    );
}

unsafe extern "C" fn device_circle(x: f64, y: f64, r: f64, gc: *const GEContext, dd: *mut DevDesc) {
    let device = render_device(dd);
    let outline = circle_outline(device.point(x, y), r * device.unit_inches);
    draw_outline(dd, outline, FillRule::Winding, &*gc);
}

/// Sets the clipping rectangle, which replaces a clipping path.
unsafe extern "C" fn device_clip(x0: f64, x1: f64, y0: f64, y1: f64, dd: *mut DevDesc) {
    let device = render_device(dd);
    let [a0, b0] = device.point(x0, y0);
    let [a1, b1] = device.point(x1, y1);
    device.clip = Clip::Rect(Rect {
        x0: a0.min(a1),
        x1: a0.max(a1),
        y0: b0.min(b1),
        y1: b0.max(b1),
    });
}

/// Renders the pages into the device's file and frees the device's own state; the engine
/// frees the `DevDesc`.
unsafe extern "C" fn device_close(dd: *mut DevDesc) {
    let device = Box::from_raw((*dd).device_specific.cast::<RenderDevice>());
    (*dd).device_specific = std::ptr::null_mut();
    let (Some(format), Some(file)) = (device.format, device.file.as_deref()) else {
        return;
    };
    if file.starts_with('|') {
        return;
    }
    if let Err(failure) = write_pages(
        format,
        file,
        &device.drawing,
        device.size,
        1.0 / device.unit_inches,
        &device.title,
        device.quality,
    ) {
        fail(format!(
            "cannot write file '{}': {failure}",
            file_for_page(file, 1)
        ));
    }
}

unsafe extern "C" fn device_line(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let device = render_device(dd);
    let outline = polygon_outline(&[device.point(x1, y1), device.point(x2, y2)], false);
    draw_line(dd, outline, &*gc);
}

/// The ascent, descent and width of character `c` (or of the font when `c` is 0) from the
/// Helvetica metrics; a negative `c` is a Unicode code point.
unsafe extern "C" fn device_metric_info(
    c: c_int,
    gc: *const GEContext,
    ascent: *mut f64,
    descent: *mut f64,
    width: *mut f64,
    dd: *mut DevDesc,
) {
    let device = render_device(dd);
    let gc = &*gc;
    let bold = gc.fontface == 2 || gc.fontface == 4;
    let scale = text_points(device, gc) / 72.0 / device.unit_inches / 1000.0;
    let code = c.unsigned_abs();
    let (up, down, across) = if code == 0 {
        if bold {
            (962.0, 228.0, 0.0)
        } else {
            (931.0, 225.0, 0.0)
        }
    } else if (32..=126).contains(&code) {
        let index = (code - 32) as usize;
        let (ascents, descents) = if bold {
            (&HELVETICA_BOLD_ASCENTS, &HELVETICA_BOLD_DESCENTS)
        } else {
            (&HELVETICA_ASCENTS, &HELVETICA_DESCENTS)
        };
        let character = char::from_u32(code).map_or_else(String::new, String::from);
        (
            f64::from(ascents[index]),
            f64::from(descents[index]),
            text_width_ems(&character, gc.fontface) * 1000.0,
        )
    } else {
        let character = char::from_u32(code).map_or_else(String::new, String::from);
        (718.0, 0.0, text_width_ems(&character, gc.fontface) * 1000.0)
    };
    *ascent = up * scale;
    *descent = down * scale;
    *width = across * scale;
}

/// Starts a page filled with the context's fill, or with the device's background when the
/// fill is transparent. The clipping region and mask reset to none.
unsafe extern "C" fn device_new_page(gc: *const GEContext, dd: *mut DevDesc) {
    let requested = if alpha((*gc).fill as Colour) > 0 {
        (*gc).fill
    } else {
        (*dd).startfill
    };
    let device = render_device(dd);
    let (background, warning) = device.checked(requested);
    // GNU R's PostScript device checks the background against the page it ends, then starts
    // counting afresh.
    device.warned_transparency = false;
    device.drawing.pages.push(Page {
        background,
        items: Vec::new(),
    });
    device.clip = device.whole_page();
    device.mask = None;
    warn(warning);
}

unsafe fn device_points(
    device: &RenderDevice,
    n: c_int,
    x: *mut f64,
    y: *mut f64,
) -> Vec<[f64; 2]> {
    let count = usize::try_from(n).unwrap_or(0);
    if count == 0 || x.is_null() || y.is_null() {
        return Vec::new();
    }
    let x = std::slice::from_raw_parts(x, count);
    let y = std::slice::from_raw_parts(y, count);
    x.iter().zip(y).map(|(x, y)| device.point(*x, *y)).collect()
}

unsafe extern "C" fn device_polygon(
    n: c_int,
    x: *mut f64,
    y: *mut f64,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let outline = polygon_outline(&device_points(render_device(dd), n, x, y), true);
    draw_outline(dd, outline, FillRule::Winding, &*gc);
}

unsafe extern "C" fn device_polyline(
    n: c_int,
    x: *mut f64,
    y: *mut f64,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let outline = polygon_outline(&device_points(render_device(dd), n, x, y), false);
    draw_line(dd, outline, &*gc);
}

unsafe extern "C" fn device_rect(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let device = render_device(dd);
    let corners = [
        device.point(x0, y0),
        device.point(x1, y0),
        device.point(x1, y1),
        device.point(x0, y1),
    ];
    draw_outline(dd, polygon_outline(&corners, true), FillRule::Winding, &*gc);
}

/// Draws a raster image, its first row at the top, with its bottom-left corner at `x`, `y` and
/// rotated `angle` degrees anticlockwise about it.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn device_raster(
    pixels: *mut c_uint,
    columns: c_int,
    rows: c_int,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    angle: f64,
    interpolate: Rboolean,
    _gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let (Ok(columns), Ok(rows)) = (usize::try_from(columns), usize::try_from(rows)) else {
        return;
    };
    let Some(count) = columns.checked_mul(rows) else {
        return;
    };
    if pixels.is_null() || count == 0 {
        return;
    }
    let device = render_device(dd);
    let pixels = std::slice::from_raw_parts(pixels, count).to_vec();
    let origin = device.point(x, y);
    let across = device.point(x + width, y);
    let up = device.point(x, y + height);
    let (sin, cos) = angle.to_radians().sin_cos();
    let rotate = |[dx, dy]: [f64; 2]| [dx * cos - dy * sin, dx * sin + dy * cos];
    let across = rotate([across[0] - origin[0], across[1] - origin[1]]);
    let up = rotate([up[0] - origin[0], up[1] - origin[1]]);
    draw(
        dd,
        Mark::Image {
            pixels,
            columns,
            rows,
            placement: Affine([across[0], across[1], up[0], up[1], origin[0], origin[1]]),
            interpolate: interpolate != 0,
        },
    );
}

/// Draws subpaths as one compound path, filled by the nonzero winding rule when `winding` is
/// true and by the even-odd rule otherwise.
unsafe extern "C" fn device_path(
    x: *mut f64,
    y: *mut f64,
    npoly: c_int,
    nper: *mut c_int,
    winding: Rboolean,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let count = usize::try_from(npoly).unwrap_or(0);
    if count == 0 || nper.is_null() || x.is_null() || y.is_null() {
        return;
    }
    let device = render_device(dd);
    let mut outline = Vec::new();
    let mut offset = 0usize;
    for &points in std::slice::from_raw_parts(nper, count) {
        let Ok(points) = usize::try_from(points) else {
            continue;
        };
        let subpath = device_points(device, points as c_int, x.add(offset), y.add(offset));
        outline.extend(polygon_outline(&subpath, true));
        offset += points;
    }
    let rule = if winding != 0 {
        FillRule::Winding
    } else {
        FillRule::EvenOdd
    };
    draw_outline(dd, outline, rule, &*gc);
}

unsafe extern "C" fn device_size(
    left: *mut f64,
    right: *mut f64,
    bottom: *mut f64,
    top: *mut f64,
    dd: *mut DevDesc,
) {
    *left = (*dd).left;
    *right = (*dd).right;
    *bottom = (*dd).bottom;
    *top = (*dd).top;
}

unsafe extern "C" fn device_str_width(
    text: *const c_char,
    gc: *const GEContext,
    dd: *mut DevDesc,
) -> f64 {
    let device = render_device(dd);
    let text = CStr::from_ptr(text).to_string_lossy();
    text_width_ems(&text, (*gc).fontface) * text_points(device, &*gc) / 72.0 / device.unit_inches
}

unsafe extern "C" fn device_text(
    x: f64,
    y: f64,
    text: *const c_char,
    rot: f64,
    hadj: f64,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let device = render_device(dd);
    let origin = device.point(x, y);
    let text = CStr::from_ptr(text).to_string_lossy().into_owned();
    let gc = &*gc;
    let (colour, warning) = device.checked(gc.col);
    warn(warning);
    draw(
        dd,
        Mark::Text {
            origin,
            text,
            points: text_points(device, gc),
            font: gc.fontface,
            rotation: rot,
            hadj,
            colour,
        },
    );
}

/// The top of each character from space to `~` in Helvetica, in thousandths of an em above the
/// baseline (the AFM bounding box's top).
const HELVETICA_ASCENTS: [i16; 95] = [
    0, 718, 718, 688, 775, 703, 718, 718, 733, 733, 718, 505, 106, 322, 106, 737, 703, 703, 703,
    703, 703, 688, 703, 688, 703, 703, 516, 516, 495, 390, 495, 727, 737, 718, 718, 737, 718, 718,
    718, 737, 718, 718, 718, 718, 718, 718, 718, 737, 718, 737, 718, 737, 718, 718, 718, 718, 718,
    718, 718, 722, 737, 722, 688, -75, 725, 538, 718, 538, 718, 538, 728, 538, 718, 718, 718, 718,
    718, 538, 538, 538, 538, 538, 538, 538, 669, 523, 523, 523, 523, 523, 523, 722, 775, 722, 326,
];

/// How far each character reaches below the baseline in Helvetica, in thousandths of an em
/// (the AFM bounding box's bottom, negated).
const HELVETICA_DESCENTS: [i16; 95] = [
    0, 0, -463, 0, 115, 19, 15, -463, 207, 207, -431, 0, 147, -232, 0, 19, 19, 0, 0, 19, 0, 19, 19,
    0, 19, 19, 0, 147, -11, -115, -11, 0, 19, 0, 0, 19, 0, 0, 0, 19, 0, 0, 19, 0, 0, 0, 0, 19, 0,
    56, 0, 19, 0, 19, 0, 0, 0, 0, 0, 196, 19, 196, -264, 125, -470, 15, 15, 15, 15, 15, 0, 220, 0,
    0, 210, 0, 0, 0, 0, 14, 207, 207, 0, 15, 7, 15, 0, 0, 0, 214, 0, 196, 225, 196, -180,
];

const HELVETICA_BOLD_ASCENTS: [i16; 95] = [
    0, 718, 718, 698, 775, 710, 718, 718, 734, 734, 718, 506, 146, 345, 146, 737, 710, 710, 710,
    710, 710, 698, 710, 698, 710, 710, 512, 512, 514, 419, 514, 727, 737, 718, 718, 737, 718, 718,
    718, 737, 718, 718, 718, 718, 718, 718, 718, 737, 718, 737, 718, 737, 718, 718, 718, 718, 718,
    718, 718, 722, 737, 722, 698, -75, 727, 546, 718, 546, 718, 546, 727, 546, 718, 725, 725, 718,
    718, 546, 546, 546, 546, 546, 546, 546, 676, 532, 532, 532, 532, 532, 532, 722, 775, 722, 343,
];

const HELVETICA_BOLD_DESCENTS: [i16; 95] = [
    0, 0, -447, 0, 115, 19, 19, -445, 208, 208, -387, 0, 168, -215, 0, 19, 19, 0, 0, 19, 0, 19, 19,
    0, 19, 19, 0, 168, 8, -87, 8, 0, 19, 0, 0, 19, 0, 0, 0, 19, 0, 0, 18, 0, 0, 0, 0, 19, 0, 52, 0,
    19, 0, 19, 0, 0, 0, 0, 0, 196, 19, 196, -323, 125, -454, 14, 14, 14, 14, 14, 0, 217, 0, 0, 214,
    0, 0, 0, 0, 14, 207, 207, 0, 14, 6, 14, 0, 0, 0, 214, 0, 196, 225, 196, -163,
];
