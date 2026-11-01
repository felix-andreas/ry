//! What a graphics device draws and the files it writes. Base graphics records each page as a
//! list of shapes in inches from the device's bottom-left corner, and closing the device
//! renders the pages into its file format: PDF and SVG as vector graphics, PNG, BMP and TIFF
//! as rasterized pixels.

use {
    crate::{
        graphics::{Colour, text_width_ems},
        graphics_engine::{
            DevDesc, GEContext, Rboolean, TRANSPARENT_WHITE, add_device_named,
            check_device_available, create_device, fail,
        },
        unwind::{R, error},
    },
    heap::object::Sexp,
    std::{
        collections::BTreeMap,
        ffi::{CStr, c_char, c_int, c_uint},
        io::Write,
        sync::OnceLock,
    },
};

/// One page of a device: its background and the shapes drawn on it, in drawing order.
pub struct Page {
    pub background: Colour,
    pub items: Vec<Item>,
}

/// A shape and the region it is clipped to.
pub struct Item {
    pub clip: Rect,
    pub shape: Shape,
}

/// A rectangle in inches, `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
}

pub enum Shape {
    /// A polyline, or a polygon when `closed`. An open path is never filled.
    Path {
        points: Vec<[f64; 2]>,
        closed: bool,
        fill: Colour,
        stroke: Option<Stroke>,
    },
    Circle {
        centre: [f64; 2],
        radius: f64,
        fill: Colour,
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
}

/// How a line is drawn. `lwd` 1 is 1/96 inch, and each hex digit of `lty` is a dash or gap
/// length in units of the line width, the first dash in the low bits.
#[derive(Clone, Copy)]
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
    /// `onefile = FALSE` writes each page to its own file. On `paper` (inches), the page is
    /// centred, and turned a quarter anticlockwise when `landscape`; without paper, as for
    /// `paper = "special"`, the document is an EPS file whose bounding box is the page itself.
    Postscript {
        onefile: bool,
        paper: Option<[f64; 2]>,
        landscape: bool,
    },
}

/// Renders `pages` of a device `size` inches across into one file for PDF and PostScript (unless
/// it was opened with `onefile = FALSE`), or into one file per page for the others, naming page `n` by filling `file`'s `%d` conversion with `n`.
/// Raster formats use `resolution` pixels per inch, and JPEG compresses at `quality`.
pub fn write_pages(
    format: Format,
    file: &str,
    pages: &[Page],
    size: [f64; 2],
    resolution: f64,
    title: &str,
    quality: u8,
) -> std::io::Result<()> {
    match format {
        Format::Pdf => {
            return std::fs::write(file_for_page(file, 1), pdf_document(pages, size, title));
        }
        Format::Postscript {
            onefile: true,
            paper,
            landscape,
        } => {
            let document = postscript_document(pages, size, paper, landscape, title);
            return std::fs::write(file_for_page(file, 1), document);
        }
        _ => {}
    }
    for (index, page) in pages.iter().enumerate() {
        let bytes = match format {
            Format::Svg => svg_document(page, size).into_bytes(),
            Format::Png | Format::Bmp | Format::Tiff | Format::Jpeg => {
                let pixels = rasterize(page, size, resolution)
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
                paper, landscape, ..
            } => postscript_document(std::slice::from_ref(page), size, paper, landscape, title),
            Format::Pdf => Vec::new(),
        };
        std::fs::write(file_for_page(file, index + 1), bytes)?;
    }
    Ok(())
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
                let width: usize = spec.parse().unwrap_or(0);
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

/// The control points of four cubic Bézier arcs that approximate a circle, each `[c1, c2,
/// end]`, starting from the circle's rightmost point.
fn circle_arcs(centre: [f64; 2], radius: f64) -> ([f64; 2], [[[f64; 2]; 3]; 4]) {
    let handle = radius * 0.552_284_749_830_793_4;
    let [x, y] = centre;
    (
        [x + radius, y],
        [
            [
                [x + radius, y + handle],
                [x + handle, y + radius],
                [x, y + radius],
            ],
            [
                [x - handle, y + radius],
                [x - radius, y + handle],
                [x - radius, y],
            ],
            [
                [x - radius, y - handle],
                [x - handle, y - radius],
                [x, y - radius],
            ],
            [
                [x + handle, y - radius],
                [x + radius, y - handle],
                [x + radius, y],
            ],
        ],
    )
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

/// A PDF document of `pages`, each `size` inches, with the standard Helvetica fonts. A
/// document without pages is still valid, which is what a device nothing was drawn on writes.
fn pdf_document(pages: &[Page], size: [f64; 2], title: &str) -> Vec<u8> {
    let mut opacities = BTreeMap::new();
    let contents: Vec<Vec<u8>> = pages
        .iter()
        .map(|page| pdf_content(page, &mut opacities).into_bytes())
        .collect();
    // Objects 1 to 4 are the catalog, the page tree, the shared resources and the document
    // information; each page then takes a page object and a content stream.
    let page_count = pages.len();
    let first_page = 5;
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
    let states: Vec<String> = opacities
        .iter()
        .map(|((stroke, fill), index)| {
            format!(
                "/GS{index} << /Type /ExtGState /CA {:.3} /ca {:.3} >>",
                f64::from(*stroke) / 255.0,
                f64::from(*fill) / 255.0
            )
        })
        .collect();
    objects.push(
        format!(
            "<< /ProcSet [/PDF /Text] /Font << {} >> /ExtGState << {} >> >>",
            fonts.join(" "),
            states.join(" ")
        )
        .into_bytes(),
    );
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
        let compressed = deflate(&content);
        let mut stream = format!(
            "<< /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed.len()
        )
        .into_bytes();
        stream.extend_from_slice(&compressed);
        stream.extend_from_slice(b"\nendstream");
        objects.push(stream);
    }
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

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(
        Vec::with_capacity(bytes.len() / 4),
        flate2::Compression::default(),
    );
    // Writing into a vector cannot fail.
    match encoder.write_all(bytes).and_then(|()| encoder.finish()) {
        Ok(compressed) => compressed,
        Err(_) => Vec::new(),
    }
}

/// The content stream of one page. Each run of items with the same clip region sits in its
/// own graphics state, so the clip is set once per run.
fn pdf_content(page: &Page, opacities: &mut BTreeMap<(u8, u8), usize>) -> String {
    let mut out = String::new();
    let mut opacity = |stroke: u8, fill: u8| {
        let next = opacities.len();
        *opacities.entry((stroke, fill)).or_insert(next)
    };
    let pt = |value: f64| value * POINTS_PER_INCH;
    if alpha(page.background) > 0 {
        let [red, green, blue, a] = channels(page.background);
        if a < 255 {
            out.push_str(&format!("/GS{} gs\n", opacity(255, a)));
        }
        out.push_str(&format!(
            "{} rg 0 0 10000 10000 re f\n",
            pdf_rgb(red, green, blue)
        ));
    }
    let mut current_clip: Option<Rect> = None;
    for item in &page.items {
        if current_clip != Some(item.clip) {
            if current_clip.is_some() {
                out.push_str("Q\n");
            }
            let clip = item.clip;
            out.push_str(&format!(
                "q {:.2} {:.2} {:.2} {:.2} re W n\n",
                pt(clip.x0),
                pt(clip.y0),
                pt(clip.x1 - clip.x0),
                pt(clip.y1 - clip.y0)
            ));
            current_clip = Some(clip);
        }
        match &item.shape {
            Shape::Path {
                points,
                closed,
                fill,
                stroke,
            } => {
                let fill = if *closed && alpha(*fill) > 0 {
                    Some(*fill)
                } else {
                    None
                };
                let stroke = visible(stroke);
                if (fill.is_none() && stroke.is_none()) || points.is_empty() {
                    continue;
                }
                pdf_paint(&mut out, fill, stroke, &mut opacity);
                for (index, [x, y]) in points.iter().enumerate() {
                    let operator = if index == 0 { "m" } else { "l" };
                    out.push_str(&format!("{:.2} {:.2} {operator}\n", pt(*x), pt(*y)));
                }
                if *closed {
                    out.push_str("h ");
                }
                out.push_str(pdf_paint_operator(fill.is_some(), stroke.is_some()));
                out.push('\n');
            }
            Shape::Circle {
                centre,
                radius,
                fill,
                stroke,
            } => {
                let fill = (alpha(*fill) > 0).then_some(*fill);
                let stroke = visible(stroke);
                if fill.is_none() && stroke.is_none() {
                    continue;
                }
                pdf_paint(&mut out, fill, stroke, &mut opacity);
                let (start, arcs) = circle_arcs(*centre, *radius);
                out.push_str(&format!("{:.2} {:.2} m\n", pt(start[0]), pt(start[1])));
                for [first, second, end] in arcs {
                    out.push_str(&format!(
                        "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c\n",
                        pt(first[0]),
                        pt(first[1]),
                        pt(second[0]),
                        pt(second[1]),
                        pt(end[0]),
                        pt(end[1])
                    ));
                }
                out.push_str("h ");
                out.push_str(pdf_paint_operator(fill.is_some(), stroke.is_some()));
                out.push('\n');
            }
            Shape::Text {
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
                    continue;
                }
                if a < 255 {
                    out.push_str(&format!("/GS{} gs\n", opacity(255, a)));
                }
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
                if a < 255 {
                    out.push_str(&format!("/GS{} gs\n", opacity(255, 255)));
                }
            }
        }
    }
    if current_clip.is_some() {
        out.push_str("Q\n");
    }
    out
}

fn pdf_rgb(red: u8, green: u8, blue: u8) -> String {
    format!(
        "{:.3} {:.3} {:.3}",
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0
    )
}

/// Sets the fill and stroke colours, opacity, width, caps, joins and dashes for a shape.
fn pdf_paint(
    out: &mut String,
    fill: Option<Colour>,
    stroke: Option<&Stroke>,
    opacity: &mut impl FnMut(u8, u8) -> usize,
) {
    let fill_alpha = fill.map_or(255, alpha);
    let stroke_alpha = stroke.map_or(255, |stroke| alpha(stroke.colour));
    out.push_str(&format!("/GS{} gs\n", opacity(stroke_alpha, fill_alpha)));
    if let Some(fill) = fill {
        let [red, green, blue, _] = channels(fill);
        out.push_str(&format!("{} rg\n", pdf_rgb(red, green, blue)));
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
}

fn pdf_paint_operator(fill: bool, stroke: bool) -> &'static str {
    match (fill, stroke) {
        (true, true) => "B",
        (true, false) => "f",
        _ => "S",
    }
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

/// Procedures named after the PDF content operators that `pdf_content` writes, so that a page's
/// PDF content stream is also a PostScript program. PDF keeps separate stroke and fill colours
/// where PostScript has one current colour, so `RG` and `rg` store theirs and each painting
/// operator sets the one it needs. Opacity has no PostScript equivalent, so `gs` drops it, as
/// GNU R's `postscript` device does.
const POSTSCRIPT_PROLOG: &str = "\
/FillColor [0 0 0] def /StrokeColor [0 0 0] def
/rg { 3 array astore /FillColor exch def } bind def
/RG { 3 array astore /StrokeColor exch def } bind def
/m { moveto } bind def /l { lineto } bind def /c { curveto } bind def /h { closepath } bind def
/re { 4 2 roll moveto 1 index 0 rlineto 0 exch rlineto neg 0 rlineto closepath } bind def
/f { FillColor aload pop setrgbcolor fill } bind def
/S { StrokeColor aload pop setrgbcolor stroke } bind def
/B { gsave f grestore S } bind def
/q { gsave } bind def /Q { grestore } bind def /W { clip } bind def /n { newpath } bind def
/w { setlinewidth } bind def /J { setlinecap } bind def /j { setlinejoin } bind def
/M { setmiterlimit } bind def /d { setdash } bind def /gs { pop } bind def
/BT { gsave } bind def /ET { grestore } bind def
/Tf { exch findfont exch scalefont setfont } bind def
/Tm { 6 array astore concat 0 0 moveto } bind def
/Tj { FillColor aload pop setrgbcolor show } bind def
/reencode { findfont dup length dict begin { 1 index /FID ne { def } { pop pop } ifelse } forall
  /Encoding ISOLatin1Encoding def currentdict end definefont pop } bind def
/F1 /Helvetica reencode /F2 /Helvetica-Bold reencode /F3 /Helvetica-Oblique reencode
/F4 /Helvetica-BoldOblique reencode /F5 /Symbol findfont definefont pop
";

/// A PostScript document of `pages`, each `size` inches, placed on `paper` as
/// [`Format::Postscript`] describes. Text uses the standard Helvetica fonts in the ISO Latin-1
/// encoding, which matches the escapes `pdf_string` writes for it.
fn postscript_document(
    pages: &[Page],
    size: [f64; 2],
    paper: Option<[f64; 2]>,
    landscape: bool,
    title: &str,
) -> Vec<u8> {
    let [width, height] = size.map(|inches| inches * POINTS_PER_INCH);
    // The paper's media and orientation comments, the bounding box in paper coordinates, and the
    // transformation from page to paper.
    let (media, bounding_box, placement) = match paper {
        None => (String::new(), [0.0, 0.0, width, height], String::new()),
        Some(paper) => {
            let [paper_width, paper_height] = paper.map(|inches| inches * POINTS_PER_INCH);
            let media = format!(
                "%%DocumentMedia: custom {paper_width:.0} {paper_height:.0} 0 () ()\n\
                 %%Orientation: {}\n",
                if landscape { "Landscape" } else { "Portrait" }
            );
            if landscape {
                let x = (paper_width - height) / 2.0;
                let y = (paper_height - width) / 2.0;
                (
                    media,
                    [x, y, x + height, y + width],
                    format!("{paper_width:.2} 0 translate 90 rotate {y:.2} {x:.2} translate\n"),
                )
            } else {
                let x = (paper_width - width) / 2.0;
                let y = (paper_height - height) / 2.0;
                (
                    media,
                    [x, y, x + width, y + height],
                    format!("{x:.2} {y:.2} translate\n"),
                )
            }
        }
    };
    let [left, bottom, right, top] = bounding_box;
    let mut out = format!(
        "%!PS-Adobe-3.0{}\n%%DocumentNeededResources: font Helvetica Helvetica-Bold \
         Helvetica-Oblique Helvetica-BoldOblique Symbol\n{media}%%Title: {}\n\
         %%Creator: R (ry)\n%%Pages: {}\n%%BoundingBox: {:.0} {:.0} {:.0} {:.0}\n\
         %%EndComments\n%%BeginProlog\n{POSTSCRIPT_PROLOG}%%EndProlog\n",
        if paper.is_none() { " EPSF-3.0" } else { "" },
        title.replace(['\n', '\r'], " "),
        pages.len(),
        left.floor(),
        bottom.floor(),
        right.ceil(),
        top.ceil()
    );
    let mut opacities = BTreeMap::new();
    for (index, page) in pages.iter().enumerate() {
        out.push_str(&format!("%%Page: {0} {0}\ngsave\n{placement}", index + 1));
        out.push_str(&pdf_content(page, &mut opacities));
        out.push_str("grestore\nshowpage\n");
    }
    out.push_str("%%EOF\n");
    out.into_bytes()
}

// SVG.

/// An SVG document of one page, in points with y growing downwards.
fn svg_document(page: &Page, size: [f64; 2]) -> String {
    let [width, height] = size.map(|inches| inches * POINTS_PER_INCH);
    let x = |value: f64| value * POINTS_PER_INCH;
    let y = |value: f64| height - value * POINTS_PER_INCH;
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" \
         width=\"{width:.2}pt\" height=\"{height:.2}pt\" viewBox=\"0 0 {width:.2} {height:.2}\">\n"
    );
    if alpha(page.background) > 0 {
        out.push_str(&format!(
            "<rect width=\"100%\" height=\"100%\" {}/>\n",
            svg_paint("fill", page.background)
        ));
    }
    let mut clips: Vec<Rect> = Vec::new();
    let mut open_group = None;
    for item in &page.items {
        let clip_index = match clips.iter().position(|clip| *clip == item.clip) {
            Some(index) => index,
            None => {
                let clip = item.clip;
                out.push_str(&format!(
                    "<clipPath id=\"clip{}\"><rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\"/></clipPath>\n",
                    clips.len(),
                    x(clip.x0),
                    y(clip.y1),
                    x(clip.x1 - clip.x0),
                    x(clip.y1 - clip.y0)
                ));
                clips.push(clip);
                clips.len() - 1
            }
        };
        if open_group != Some(clip_index) {
            if open_group.is_some() {
                out.push_str("</g>\n");
            }
            out.push_str(&format!("<g clip-path=\"url(#clip{clip_index})\">\n"));
            open_group = Some(clip_index);
        }
        match &item.shape {
            Shape::Path {
                points,
                closed,
                fill,
                stroke,
            } => {
                let mut data = String::new();
                for (index, [px, py]) in points.iter().enumerate() {
                    let command = if index == 0 { 'M' } else { 'L' };
                    data.push_str(&format!("{command}{:.2} {:.2} ", x(*px), y(*py)));
                }
                if *closed {
                    data.push('Z');
                }
                let fill = if *closed { *fill } else { 0 };
                out.push_str(&format!(
                    "<path d=\"{}\" {} {}/>\n",
                    data.trim_end(),
                    svg_paint("fill", fill),
                    svg_stroke(stroke)
                ));
            }
            Shape::Circle {
                centre,
                radius,
                fill,
                stroke,
            } => out.push_str(&format!(
                "<circle cx=\"{:.2}\" cy=\"{:.2}\" r=\"{:.2}\" {} {}/>\n",
                x(centre[0]),
                y(centre[1]),
                x(*radius),
                svg_paint("fill", *fill),
                svg_stroke(stroke)
            )),
            Shape::Text {
                origin,
                text,
                points,
                font,
                rotation,
                hadj,
                colour,
            } => {
                let face = if (1..=5).contains(font) { *font } else { 1 };
                let width = text_width_ems(text, face) * points / POINTS_PER_INCH;
                let (sin, cos) = rotation.to_radians().sin_cos();
                let tx = x(origin[0] - hadj * width * cos);
                let ty = y(origin[1] - hadj * width * sin);
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
                    svg_paint("fill", *colour),
                    xml_escape(text)
                ));
            }
        }
    }
    if open_group.is_some() {
        out.push_str("</g>\n");
    }
    out.push_str("</svg>\n");
    out
}

fn svg_paint(attribute: &str, colour: Colour) -> String {
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
    let mut paint = format!(
        "{} stroke-width=\"{:.2}\" stroke-linecap=\"{}\" stroke-linejoin=\"{}\" stroke-miterlimit=\"{:.2}\"",
        svg_paint("stroke", stroke.colour),
        stroke.lwd * 0.75,
        caps.get(stroke.lend as usize).unwrap_or(&"round"),
        joins.get(stroke.ljoin as usize).unwrap_or(&"round"),
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
fn rasterize(page: &Page, size: [f64; 2], resolution: f64) -> Option<tiny_skia::Pixmap> {
    use tiny_skia::{FillRule, Mask, Paint, PathBuilder, Pixmap, Transform};
    let [width, height] = size.map(|inches| (inches * resolution).round().max(1.0) as u32);
    let mut pixmap = Pixmap::new(width, height)?;
    let [red, green, blue, a] = channels(page.background);
    pixmap.fill(tiny_skia::Color::from_rgba8(red, green, blue, a));
    let to_pixels = Transform::from_row(
        resolution as f32,
        0.0,
        0.0,
        -resolution as f32,
        0.0,
        f64::from(height) as f32,
    );
    let mut mask_for: Option<(Rect, Mask)> = None;
    for item in &page.items {
        if mask_for.as_ref().is_none_or(|(clip, _)| *clip != item.clip) {
            let clip = item.clip;
            let mask = Mask::new(width, height).and_then(|mut mask| {
                let rect = tiny_skia::Rect::from_ltrb(
                    clip.x0 as f32,
                    clip.y0 as f32,
                    clip.x1 as f32,
                    clip.y1 as f32,
                )?;
                mask.fill_path(
                    &PathBuilder::from_rect(rect),
                    FillRule::Winding,
                    false,
                    to_pixels,
                );
                Some(mask)
            });
            match mask {
                Some(mask) => mask_for = Some((clip, mask)),
                None => continue,
            }
        }
        let Some((_, mask)) = mask_for.as_ref() else {
            continue;
        };
        let paint_of = |colour: Colour| {
            let [red, green, blue, a] = channels(colour);
            let mut paint = Paint::default();
            paint.set_color_rgba8(red, green, blue, a);
            paint.anti_alias = true;
            paint
        };
        let draw = |pixmap: &mut Pixmap,
                    path: tiny_skia::Path,
                    fill: Option<Colour>,
                    stroke: Option<&Stroke>| {
            if let Some(fill) = fill.filter(|fill| alpha(*fill) > 0) {
                pixmap.fill_path(
                    &path,
                    &paint_of(fill),
                    FillRule::Winding,
                    to_pixels,
                    Some(mask),
                );
            }
            if let Some(stroke) = stroke {
                pixmap.stroke_path(
                    &path,
                    &paint_of(stroke.colour),
                    &skia_stroke(stroke),
                    to_pixels,
                    Some(mask),
                );
            }
        };
        match &item.shape {
            Shape::Path {
                points,
                closed,
                fill,
                stroke,
            } => {
                let mut builder = PathBuilder::new();
                for (index, [x, y]) in points.iter().enumerate() {
                    if index == 0 {
                        builder.move_to(*x as f32, *y as f32);
                    } else {
                        builder.line_to(*x as f32, *y as f32);
                    }
                }
                if *closed {
                    builder.close();
                }
                if let Some(path) = builder.finish() {
                    draw(&mut pixmap, path, closed.then_some(*fill), visible(stroke));
                }
            }
            Shape::Circle {
                centre,
                radius,
                fill,
                stroke,
            } => {
                if let Some(path) =
                    PathBuilder::from_circle(centre[0] as f32, centre[1] as f32, *radius as f32)
                {
                    draw(&mut pixmap, path, Some(*fill), visible(stroke));
                }
            }
            Shape::Text {
                origin,
                text,
                points,
                font,
                rotation,
                hadj,
                colour,
            } => {
                if let Some(path) = text_path(text, *points, *font, *origin, *rotation, *hadj) {
                    draw(&mut pixmap, path, Some(*colour), None);
                }
            }
        }
    }
    Some(pixmap)
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
fn text_path(
    text: &str,
    points: f64,
    font: i32,
    origin: [f64; 2],
    rotation: f64,
    hadj: f64,
) -> Option<tiny_skia::Path> {
    use ab_glyph::{Font, OutlineCurve};
    let face = raster_font(font)?;
    let units = f64::from(face.units_per_em()?);
    let scale = points / POINTS_PER_INCH / units;
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
    let place = |pen: f64, x: f32, y: f32| -> (f32, f32) {
        let local_x = pen + f64::from(x) * scale - hadj * width;
        let local_y = f64::from(y) * scale;
        (
            (origin[0] + local_x * cos - local_y * sin) as f32,
            (origin[1] + local_x * sin + local_y * cos) as f32,
        )
    };
    let mut builder = tiny_skia::PathBuilder::new();
    let mut pen = 0.0;
    for glyph in glyphs {
        if let Some(outline) = face.outline(glyph) {
            let mut last: Option<ab_glyph::Point> = None;
            for curve in &outline.curves {
                let start = match curve {
                    OutlineCurve::Line(start, _)
                    | OutlineCurve::Quad(start, _, _)
                    | OutlineCurve::Cubic(start, _, _, _) => *start,
                };
                if last != Some(start) {
                    if last.is_some() {
                        builder.close();
                    }
                    let (x, y) = place(pen, start.x, start.y);
                    builder.move_to(x, y);
                }
                match curve {
                    OutlineCurve::Line(_, end) => {
                        let (x, y) = place(pen, end.x, end.y);
                        builder.line_to(x, y);
                        last = Some(*end);
                    }
                    OutlineCurve::Quad(_, control, end) => {
                        let (cx, cy) = place(pen, control.x, control.y);
                        let (x, y) = place(pen, end.x, end.y);
                        builder.quad_to(cx, cy, x, y);
                        last = Some(*end);
                    }
                    OutlineCurve::Cubic(_, first, second, end) => {
                        let (ax, ay) = place(pen, first.x, first.y);
                        let (bx, by) = place(pen, second.x, second.y);
                        let (x, y) = place(pen, end.x, end.y);
                        builder.cubic_to(ax, ay, bx, by, x, y);
                        last = Some(*end);
                    }
                }
            }
            if last.is_some() {
                builder.close();
            }
        }
        pen += f64::from(face.h_advance_unscaled(glyph)) * scale;
    }
    builder.finish()
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

/// A built-in device's own state, which its `DevDesc` points to: the pages drawn so far,
/// which closing the device renders, in inches from the bottom-left corner.
struct RenderDevice {
    format: Option<Format>,
    file: Option<String>,
    size: [f64; 2],
    unit_inches: f64,
    y_down: bool,
    pages: Vec<Page>,
    clip: Rect,
    title: String,
    quality: u8,
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
    let bitmap = matches!(
        spec.format,
        Some(Format::Png | Format::Bmp | Format::Tiff | Format::Jpeg)
    ) || spec.y_down;
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
    d.have_transparency = if matches!(spec.format, Some(Format::Postscript { .. })) {
        1
    } else {
        2
    };
    d.have_transparent_bg = if bitmap && spec.format != Some(Format::Png) {
        1
    } else {
        2
    };
    d.have_raster = 1;
    d.have_capture = 1;
    d.have_locator = 1;
    d.device_version = 0;
    d.device_clip = 0;
    // Graphics systems call these whatever the device version, as grid's new page releases
    // every pattern, clipping path and mask, so a device without them would crash.
    d.set_pattern = Some(device_set_pattern);
    d.release_pattern = Some(device_release);
    d.set_clip_path = Some(device_set_definition);
    d.release_clip_path = Some(device_release);
    d.set_mask = Some(device_set_definition);
    d.release_mask = Some(device_release);
    d.device_specific = Box::into_raw(Box::new(RenderDevice {
        format: spec.format,
        file: spec.file.clone(),
        size: spec.size,
        unit_inches: unit,
        y_down: spec.y_down,
        pages: Vec::new(),
        clip: Rect {
            x0: 0.0,
            x1: spec.size[0],
            y0: 0.0,
            y1: spec.size[1],
        },
        title: spec.title,
        quality: spec.quality,
    }))
    .cast();
    let dd = create_device(dev)?;
    add_device_named(dd, spec.name, spec.file.as_deref())
}

unsafe extern "C" fn device_set_pattern(_pattern: Sexp, _dd: *mut DevDesc) -> Option<Sexp> {
    None
}

unsafe extern "C" fn device_set_definition(
    _definition: Sexp,
    _reference: Sexp,
    _dd: *mut DevDesc,
) -> Option<Sexp> {
    None
}

unsafe extern "C" fn device_release(_reference: Sexp, _dd: *mut DevDesc) {}

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

    fn push(&mut self, shape: Shape) {
        if self.pages.is_empty() {
            self.pages.push(Page {
                background: TRANSPARENT_WHITE as Colour,
                items: Vec::new(),
            });
        }
        let clip = self.clip;
        if let Some(page) = self.pages.last_mut() {
            page.items.push(Item { clip, shape });
        }
    }
}

/// The stroke a context describes, converting the engine's line end and join codes, which
/// count from 1, to `par`'s, which count from 0.
fn stroke(gc: &GEContext) -> Option<Stroke> {
    Some(Stroke {
        colour: gc.col as Colour,
        lwd: gc.lwd,
        lty: gc.lty as u32,
        lend: gc.lend - 1,
        ljoin: gc.ljoin - 1,
        lmitre: gc.lmitre,
    })
}

/// The text size a context asks for, in points.
fn text_points(gc: &GEContext) -> f64 {
    gc.ps * gc.cex
}

unsafe extern "C" fn device_circle(x: f64, y: f64, r: f64, gc: *const GEContext, dd: *mut DevDesc) {
    let device = render_device(dd);
    let gc = &*gc;
    let centre = device.point(x, y);
    let radius = r * device.unit_inches;
    device.push(Shape::Circle {
        centre,
        radius,
        fill: gc.fill as Colour,
        stroke: stroke(gc),
    });
}

unsafe extern "C" fn device_clip(x0: f64, x1: f64, y0: f64, y1: f64, dd: *mut DevDesc) {
    let device = render_device(dd);
    let [a0, b0] = device.point(x0, y0);
    let [a1, b1] = device.point(x1, y1);
    device.clip = Rect {
        x0: a0.min(a1),
        x1: a0.max(a1),
        y0: b0.min(b1),
        y1: b0.max(b1),
    };
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
        &device.pages,
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
    let points = vec![device.point(x1, y1), device.point(x2, y2)];
    device.push(Shape::Path {
        points,
        closed: false,
        fill: TRANSPARENT_WHITE as Colour,
        stroke: stroke(&*gc),
    });
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
    let scale = text_points(gc) / 72.0 / device.unit_inches / 1000.0;
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
/// fill is transparent.
unsafe extern "C" fn device_new_page(gc: *const GEContext, dd: *mut DevDesc) {
    let background = if alpha((*gc).fill as Colour) > 0 {
        (*gc).fill
    } else {
        (*dd).startfill
    };
    let device = render_device(dd);
    device.pages.push(Page {
        background: background as Colour,
        items: Vec::new(),
    });
    device.clip = Rect {
        x0: 0.0,
        x1: device.size[0],
        y0: 0.0,
        y1: device.size[1],
    };
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
    let device = render_device(dd);
    let points = device_points(device, n, x, y);
    device.push(Shape::Path {
        points,
        closed: true,
        fill: (*gc).fill as Colour,
        stroke: stroke(&*gc),
    });
}

unsafe extern "C" fn device_polyline(
    n: c_int,
    x: *mut f64,
    y: *mut f64,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let device = render_device(dd);
    let points = device_points(device, n, x, y);
    device.push(Shape::Path {
        points,
        closed: false,
        fill: TRANSPARENT_WHITE as Colour,
        stroke: stroke(&*gc),
    });
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
    let points = vec![
        device.point(x0, y0),
        device.point(x1, y0),
        device.point(x1, y1),
        device.point(x0, y1),
    ];
    device.push(Shape::Path {
        points,
        closed: true,
        fill: (*gc).fill as Colour,
        stroke: stroke(&*gc),
    });
}

/// Draws a raster image as one filled quadrilateral per cell, its first row at the top, with
/// its bottom-left corner at `x`, `y` and rotated `angle` degrees anticlockwise about it. The
/// renderer has no images, so `interpolate` is ignored and cells keep hard edges.
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
    _interpolate: Rboolean,
    _gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let (Ok(columns), Ok(rows)) = (usize::try_from(columns), usize::try_from(rows)) else {
        return;
    };
    if pixels.is_null() || columns == 0 || rows == 0 {
        return;
    }
    let device = render_device(dd);
    let pixels = std::slice::from_raw_parts(pixels, columns * rows);
    let origin = device.point(x, y);
    let across = device.point(x + width, y);
    let up = device.point(x, y + height);
    let (sin, cos) = angle.to_radians().sin_cos();
    let rotate = |[dx, dy]: [f64; 2]| [dx * cos - dy * sin, dx * sin + dy * cos];
    let across = rotate([across[0] - origin[0], across[1] - origin[1]]);
    let up = rotate([up[0] - origin[0], up[1] - origin[1]]);
    let at = |column: usize, row: usize| {
        let (u, v) = (
            column as f64 / columns as f64,
            1.0 - row as f64 / rows as f64,
        );
        [
            origin[0] + u * across[0] + v * up[0],
            origin[1] + u * across[1] + v * up[1],
        ]
    };
    for (index, &pixel) in pixels.iter().enumerate() {
        if pixel >> 24 == 0 {
            continue;
        }
        let (row, column) = (index / columns, index % columns);
        device.push(Shape::Path {
            points: vec![
                at(column, row + 1),
                at(column + 1, row + 1),
                at(column + 1, row),
                at(column, row),
            ],
            closed: true,
            fill: pixel as Colour,
            stroke: None,
        });
    }
}

/// Draws each subpath as its own polygon. The renderer has no compound paths, so a subpath
/// that should cut a hole in another is filled instead.
unsafe extern "C" fn device_path(
    x: *mut f64,
    y: *mut f64,
    npoly: c_int,
    nper: *mut c_int,
    _winding: c_int,
    gc: *const GEContext,
    dd: *mut DevDesc,
) {
    let count = usize::try_from(npoly).unwrap_or(0);
    if count == 0 || nper.is_null() {
        return;
    }
    let mut offset = 0usize;
    for &points in std::slice::from_raw_parts(nper, count) {
        device_polygon(points, x.add(offset), y.add(offset), gc, dd);
        offset += usize::try_from(points).unwrap_or(0);
    }
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
    text_width_ems(&text, (*gc).fontface) * text_points(&*gc) / 72.0 / device.unit_inches
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
    device.push(Shape::Text {
        origin,
        text,
        points: text_points(gc),
        font: gc.fontface,
        rotation: rot,
        hadj,
        colour: gc.col as Colour,
    });
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
