//! Mathematical annotation (plotmath): the layout of R expressions such as
//! `hat(beta) == frac(1, n) * sum(x[i], i == 1, n)` as typeset mathematics.
//!
//! This is a port of GNU R's `plotmath.c`, which follows the TeXbook's Appendix G. Every
//! construct is laid out twice: once without drawing to measure its bounding box, and once to
//! draw it, so the measuring pass and the drawing pass must make the same calls in the same
//! order. Boxes are in inches, and every glyph is measured and drawn through the engine's
//! `metric_info`, `str_width`, `text` and `polyline`, so a device sees exactly the calls GNU R
//! makes. Quirks of the original that change the output are kept on purpose, because the
//! goal is the same pixels.

use {
    crate::{
        format::{
            Settings, format_complex_alone, format_integer, format_real_common, format_real_with,
        },
        graphics_engine::{
            GEContext, GEDevDesc, Unit, from_device_height, from_device_x, from_device_y,
            metric_info, polyline, str_width, text, to_device_height, to_device_width, to_device_x,
            to_device_y,
        },
        unwind::{R, error, error_in},
        values::{self, length, nil},
    },
    heap::object::{NA_INTEGER, Sexp, SexpType},
    std::ffi::c_int,
};

/// `GEExpressionWidth`: the width of `expression` laid out as mathematics, in device units.
pub unsafe fn expression_width(expression: Sexp, gc: &GEContext, dd: *mut GEDevDesc) -> R<f64> {
    let bbox = Layout::new(gc, dd).element(expression, false)?;
    Ok(to_device_width(bbox.width, Unit::Inches, dd).abs())
}

/// `GEExpressionHeight`: the height plus depth of `expression`, in device units.
pub unsafe fn expression_height(expression: Sexp, gc: &GEContext, dd: *mut GEDevDesc) -> R<f64> {
    let bbox = Layout::new(gc, dd).element(expression, false)?;
    Ok(to_device_height(bbox.height + bbox.depth, Unit::Inches, dd).abs())
}

/// `GEExpressionMetric`: the ascent, descent and width of `expression`, in device units.
pub unsafe fn expression_metric(
    expression: Sexp,
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<[f64; 3]> {
    let bbox = Layout::new(gc, dd).element(expression, false)?;
    Ok([
        to_device_height(bbox.height, Unit::Inches, dd).abs(),
        to_device_height(bbox.depth, Unit::Inches, dd).abs(),
        to_device_width(bbox.width, Unit::Inches, dd).abs(),
    ])
}

/// `GEMathText`: draws `expression` with `(x, y)` (device units) at `(xc, yc)` of its box,
/// rotated `rotation` degrees. A non-finite `xc` or `yc` centres the box on that axis.
#[allow(clippy::too_many_arguments)]
pub unsafe fn math_text(
    x: f64,
    y: f64,
    expression: Sexp,
    xc: f64,
    yc: f64,
    rotation: f64,
    gc: &GEContext,
    dd: *mut GEDevDesc,
) -> R<()> {
    if metric_info(c_int::from(b'M'), gc, dd)? == [0.0; 3] {
        return Err(error(
            "Metric information not available for this family/device",
        ));
    }
    let mut layout = Layout::new(gc, dd);
    let bbox = layout.element(expression, false)?;
    layout.reference_x = from_device_x(x, Unit::Inches, dd);
    layout.reference_y = from_device_y(y, Unit::Inches, dd);
    let xc = if xc.is_finite() { xc } else { 0.5 };
    let yc = if yc.is_finite() { yc } else { 0.5 };
    layout.current_x = layout.reference_x - xc * bbox.width;
    layout.current_y = layout.reference_y + bbox.depth - yc * (bbox.height + bbox.depth);
    layout.angle = rotation;
    let radians = rotation * std::f64::consts::FRAC_PI_2 / 90.0;
    layout.cos = radians.cos();
    layout.sin = radians.sin();
    layout.element(expression, true)?;
    Ok(())
}

/// TeX's math styles (The TeXbook, page 441), ordered so that a larger style is a larger size.
/// A cramped style is TeX's primed style, used under a radical and in denominators, which
/// raises superscripts less.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Style {
    ScriptScriptCramped = 1,
    ScriptScript,
    ScriptCramped,
    Script,
    TextCramped,
    Text,
    DisplayCramped,
    Display,
}

/// A box in inches, with the italic correction its last glyph wants and whether its nucleus
/// was a plain run of glyphs, which places scripts closer.
#[derive(Clone, Copy, Default)]
struct BBox {
    height: f64,
    depth: f64,
    width: f64,
    italic: f64,
    simple: bool,
}

/// How the bytes of a string reach the engine: in the native (UTF-8) encoding, or in the
/// Adobe Symbol encoding of the symbol font.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Native,
    Symbol,
}

/// The TeX font parameters plotmath uses (The TeXbook, page 447), with GNU R's values, which
/// are derived from the current font's glyphs.
#[derive(Clone, Copy)]
enum Parameter {
    XHeight,
    Num1,
    Num2,
    Denom1,
    Denom2,
    Sup1,
    Sup2,
    Sup3,
    Sub1,
    Sub2,
    SupDrop,
    SubDrop,
    AxisHeight,
    RuleThickness,
    BigOpSpacing,
}

const PLAIN: c_int = 1;
const BOLD: c_int = 2;
const ITALIC: c_int = 3;
const BOLD_ITALIC: c_int = 4;
const SYMBOL: c_int = 5;

const LTY_SOLID: c_int = 0;

/// The italic correction of a glyph, as a fraction of its height.
const ITALIC_FACTOR: f64 = 0.15;
/// How far script styles shrink the numerator and denominator spacing.
const SUBS: f64 = 0.7;
const ACCENT_GAP: f64 = 0.2;
const HAT_HEIGHT: f64 = 0.3;
const TILDE_SEGMENTS: usize = 8;
const TILDE_INSET: f64 = 0.05;
const DELIMITER_MAGNIFICATION: f64 = 1.25;
const OPERATOR_MAGNIFICATION: f64 = 1.25;
const RADICAL_GAP: f64 = 0.4;
const RADICAL_SPACE: f64 = 0.2;

const HAT: c_int = 94;
const TILDE: c_int = 126;
const SPACE: c_int = 32;
const PAREN_LEFT: c_int = 40;
const PAREN_RIGHT: c_int = 41;
const ASTERISK: c_int = 42;
const COMMA: c_int = 44;
const SLASH: c_int = 47;
const ELLIPSIS: c_int = 188;
const INTERSECTION: c_int = 199;
const UNION: c_int = 200;
const PRODUCT: c_int = 213;
const SUM: c_int = 229;
const ANGLE_LEFT: c_int = 225;
const BRACKET_LEFT_TOP: c_int = 233;
const BRACKET_LEFT_BOTTOM: c_int = 235;
const ANGLE_RIGHT: c_int = 241;
const BRACKET_RIGHT_TOP: c_int = 249;
const BRACKET_RIGHT_BOTTOM: c_int = 251;
const RING: c_int = 176;
const DOT_MATH: c_int = 215;

/// The names that draw a glyph of the symbol font: GNU R's table lists the whole Adobe Symbol
/// font, but only the Greek letters and these few others are translated.
const SYMBOL_NAMES: &[(&str, c_int)] = &[
    ("Alpha", 65),
    ("Beta", 66),
    ("Chi", 67),
    ("Delta", 68),
    ("Epsilon", 69),
    ("Phi", 70),
    ("Gamma", 71),
    ("Eta", 72),
    ("Iota", 73),
    ("theta1", 74),
    ("vartheta", 74),
    ("Kappa", 75),
    ("Lambda", 76),
    ("Mu", 77),
    ("Nu", 78),
    ("Omicron", 79),
    ("Pi", 80),
    ("Theta", 81),
    ("Rho", 82),
    ("Sigma", 83),
    ("Tau", 84),
    ("Upsilon", 85),
    ("sigma1", 86),
    ("varsigma", 86),
    ("stigma", 86),
    ("Omega", 87),
    ("Xi", 88),
    ("Psi", 89),
    ("Zeta", 90),
    ("alpha", 97),
    ("beta", 98),
    ("chi", 99),
    ("delta", 100),
    ("epsilon", 101),
    ("phi", 102),
    ("gamma", 103),
    ("eta", 104),
    ("iota", 105),
    ("phi1", 106),
    ("varphi", 106),
    ("kappa", 107),
    ("lambda", 108),
    ("mu", 109),
    ("nu", 110),
    ("omicron", 111),
    ("pi", 112),
    ("theta", 113),
    ("rho", 114),
    ("sigma", 115),
    ("tau", 116),
    ("upsilon", 117),
    ("omega1", 118),
    ("omega", 119),
    ("xi", 120),
    ("psi", 121),
    ("zeta", 122),
    ("Upsilon1", 161),
    ("minute", 162),
    ("infinity", 165),
    ("degree", 176),
    ("second", 178),
    ("partialdiff", 182),
    ("aleph", 192),
    ("nabla", 209),
];

const BINARY_OPERATORS: &[(&str, c_int)] = &[
    ("!", 0o41),
    ("*", 0o52),
    ("+", 0o53),
    ("-", 0o55),
    ("/", 0o57),
    (":", 0o72),
    ("%+-%", 0o261),
    ("%*%", 0o264),
    ("%/%", 0o270),
    ("%intersection%", 0o307),
    ("%union%", 0o310),
    ("%.%", 0o327),
];

const RELATIONS: &[(&str, c_int)] = &[
    ("<", 60),
    ("==", 61),
    (">", 62),
    ("%=~%", 64),
    ("!=", 185),
    ("<=", 163),
    (">=", 179),
    ("%==%", 186),
    ("%~~%", 187),
    ("%prop%", 181),
    ("%~%", 126),
    ("%<->%", 171),
    ("%<-%", 172),
    ("%up%", 173),
    ("%->%", 174),
    ("%down%", 175),
    ("%<=>%", 219),
    ("%<=%", 220),
    ("%dblup%", 221),
    ("%=>%", 222),
    ("%dbldown%", 223),
    ("%supset%", 201),
    ("%supseteq%", 202),
    ("%notsubset%", 203),
    ("%subset%", 204),
    ("%subseteq%", 205),
    ("%in%", 206),
    ("%notin%", 207),
];

const ACCENTS: &[(&str, c_int)] = &[("hat", 94), ("ring", 176), ("tilde", 126), ("dot", 215)];

/// The big operators. The named ones (`lim`, `max`, ...) have no glyph and draw their name;
/// GNU R maps `limsup` to `liminf`'s code, which only matters for that distinction.
const LIMIT_NAME: c_int = 1001;
const OPERATORS: &[(&str, c_int)] = &[
    ("prod", PRODUCT),
    ("sum", SUM),
    ("union", UNION),
    ("intersect", INTERSECTION),
    ("lim", LIMIT_NAME),
    ("liminf", LIMIT_NAME + 1),
    ("limsup", LIMIT_NAME + 1),
    ("inf", LIMIT_NAME + 3),
    ("sup", LIMIT_NAME + 4),
    ("min", LIMIT_NAME + 5),
    ("max", LIMIT_NAME + 6),
];

/// Adobe Symbol codes 32 to 255 as Unicode, as GNU R's `AdobeSymbol2utf8` maps them when the
/// private use area is off: bracket and brace pieces become the Unicode bracket-piece code
/// points, which UTF-8 devices such as ragg draw with ordinary text fonts.
const ADOBE_SYMBOL_UNICODE: [u16; 224] = [
    32, 33, 8704, 35, 8707, 37, 38, 8717, 40, 41, 8727, 43, 44, 8722, 46, 47, 48, 49, 50, 51, 52,
    53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 8773, 913, 914, 935, 916, 917, 934, 915, 919, 921,
    977, 922, 923, 924, 925, 927, 928, 920, 929, 931, 932, 933, 962, 937, 926, 936, 918, 91, 8756,
    93, 8869, 95, 9135, 945, 946, 967, 948, 949, 966, 947, 951, 953, 981, 954, 955, 956, 957, 959,
    960, 952, 961, 963, 964, 965, 982, 969, 958, 968, 950, 123, 124, 125, 8764, 32, 32, 32, 32, 32,
    32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32,
    32, 32, 32, 32, 8364, 978, 8242, 8804, 8260, 8734, 402, 9827, 9830, 9829, 9824, 8596, 8592,
    8593, 8594, 8595, 176, 177, 8243, 8805, 215, 8733, 8706, 8226, 247, 8800, 8801, 8776, 8230,
    9168, 9135, 8629, 8501, 8465, 8476, 8472, 8855, 8853, 8709, 8745, 8746, 8835, 8839, 8836, 8834,
    8838, 8712, 8713, 8736, 8711, 174, 169, 8482, 8719, 8730, 8901, 172, 8743, 8744, 8660, 8656,
    8657, 8658, 8659, 9674, 9001, 174, 169, 8482, 8721, 9115, 9116, 9117, 9121, 9122, 9123, 9127,
    9128, 9129, 9130, 32, 9002, 8747, 8992, 9134, 8993, 9118, 9119, 9120, 9124, 9125, 9126, 9131,
    9132, 9133, 32,
];

/// The layout state GNU R calls the math context: the graphics context whose face and size
/// the layout changes as it descends, and the pen position in inches, before rotation about
/// the reference point.
struct Layout {
    gc: GEContext,
    dd: *mut GEDevDesc,
    base_cex: f64,
    reference_x: f64,
    reference_y: f64,
    current_x: f64,
    current_y: f64,
    angle: f64,
    cos: f64,
    sin: f64,
    style: Style,
}

impl Layout {
    fn new(gc: &GEContext, dd: *mut GEDevDesc) -> Layout {
        let mut gc = *gc;
        let base_cex = gc.cex;
        gc.fontface = PLAIN;
        Layout {
            gc,
            dd,
            base_cex,
            reference_x: 0.0,
            reference_y: 0.0,
            current_x: 0.0,
            current_y: 0.0,
            angle: 0.0,
            cos: 0.0,
            sin: 0.0,
            style: Style::Display,
        }
    }

    // Dispatch.

    unsafe fn element(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        if expression.kind() == SexpType::Language {
            self.formula(expression, draw)
        } else {
            self.atom(expression, draw)
        }
    }

    unsafe fn offset_element(&mut self, expression: Sexp, x: f64, y: f64, draw: bool) -> R<BBox> {
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        if draw {
            self.current_x += x;
            self.current_y += y;
        }
        let mut bbox = self.element(expression, draw)?;
        bbox.width += x;
        bbox.height += y;
        bbox.depth -= y;
        self.current_x = saved_x;
        self.current_y = saved_y;
        Ok(bbox)
    }

    unsafe fn formula(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let head = expression.car();
        let named = |names: &[&str]| names.iter().any(|name| name_is(head, name));
        if named(&["~"]) {
            self.space(expression, draw)
        } else if table_code(head, BINARY_OPERATORS) != 0 {
            self.binary(expression, draw)
        } else if named(&["^"]) {
            self.superscript(expression, draw)
        } else if named(&["["]) {
            self.subscript(expression, draw)
        } else if named(&["widetilde"]) {
            self.wide_tilde(expression, draw)
        } else if named(&["widehat"]) {
            self.wide_hat(expression, draw)
        } else if named(&["bar"]) {
            self.bar(expression, draw)
        } else if table_code(head, ACCENTS) != 0 {
            self.accent(expression, draw)
        } else if named(&["over", "frac"]) {
            self.fraction(expression, true, draw)
        } else if named(&["underline"]) {
            self.underline(expression, draw)
        } else if named(&["atop"]) {
            self.fraction(expression, false, draw)
        } else if named(&["("]) {
            self.parenthesis(expression, draw)
        } else if named(&["bgroup"]) {
            self.big_group(expression, draw)
        } else if named(&["group"]) {
            self.group(expression, draw)
        } else if named(&["integral"]) {
            self.integral(expression, draw)
        } else if table_code(head, OPERATORS) != 0 {
            self.operator(expression, draw)
        } else if named(&["root", "sqrt"]) {
            self.radical(expression, draw)
        } else if named(&["abs"]) {
            self.absolute(expression, draw)
        } else if named(&["{"]) {
            self.element(argument(expression, 1), draw)
        } else if table_code(head, RELATIONS) != 0 {
            self.relation(expression, draw)
        } else if named(&["bold"]) {
            self.with_font(BOLD, expression, draw)
        } else if named(&["italic", "math"]) {
            self.with_font(ITALIC, expression, draw)
        } else if named(&["plain"]) {
            self.with_font(PLAIN, expression, draw)
        } else if named(&["symbol"]) {
            self.with_font(SYMBOL, expression, draw)
        } else if named(&["bolditalic", "boldmath"]) {
            self.with_font(BOLD_ITALIC, expression, draw)
        } else if named(&[
            "displaystyle",
            "textstyle",
            "scriptstyle",
            "scriptscriptstyle",
        ]) {
            self.with_style(expression, draw)
        } else if named(&["phantom", "vphantom"]) {
            self.phantom(expression, draw)
        } else if named(&["paste"]) {
            self.concatenate(expression, draw)
        } else if named(&["list"]) {
            self.comma_list(expression.cdr(), draw)
        } else {
            self.function_call(expression, draw)
        }
    }

    // Atoms.

    unsafe fn atom(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        match expression.kind() {
            SexpType::Symbol => {
                if name_is(expression, "cdots")
                    || name_is(expression, "...")
                    || name_is(expression, "ldots")
                {
                    self.dots(expression, draw)
                } else {
                    match translated_symbol(expression) {
                        0 => self.symbol_string(&values::symbol_name(expression), draw),
                        code => self.symbol_char(code, draw),
                    }
                }
            }
            SexpType::Real | SexpType::Integer | SexpType::Complex => {
                let previous = self.set_font(PLAIN);
                let bbox = self.string(number_text(expression).as_bytes(), draw);
                self.set_font(previous);
                bbox
            }
            SexpType::String => {
                let text = if length(expression) > 0 {
                    values::string_at(expression, 0)
                        .map_or_else(|| "NA".to_owned(), |text| text.into_owned())
                } else {
                    String::new()
                };
                self.string(text.as_bytes(), draw)
            }
            _ => Ok(BBox::default()),
        }
    }

    unsafe fn dots(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let bbox = self.symbol_char(ELLIPSIS, false)?;
        if name_is(expression, "cdots") || name_is(expression, "...") {
            let shift = self.axis_height()? - 0.5 * bbox.height;
            if draw {
                self.current_y += shift;
                self.symbol_char(ELLIPSIS, true)?;
                self.current_y -= shift;
            }
            Ok(bbox.shift(shift))
        } else {
            if draw {
                self.symbol_char(ELLIPSIS, true)?;
            }
            Ok(bbox)
        }
    }

    /// `RenderSymbolChar`: one glyph of the symbol font, or of the plain font for the hat and
    /// tilde, which the symbol font lacks.
    unsafe fn symbol_char(&mut self, code: c_int, draw: bool) -> R<BBox> {
        let previous = if code == HAT || code == TILDE {
            self.set_font(PLAIN)
        } else {
            self.set_font(SYMBOL)
        };
        let bbox = self.glyph_bbox(code)?;
        if draw {
            let byte = [u8::try_from(code).unwrap_or(b' ')];
            self.draw_text(&byte, Encoding::Symbol)?;
            self.current_x += bbox.width;
        }
        self.set_font(previous);
        Ok(bbox)
    }

    /// `RenderSymbolStr`: a name drawn glyph by glyph in the current font, with an italic
    /// correction after each glyph and digits always upright.
    unsafe fn symbol_string(&mut self, text: &str, draw: bool) -> R<BBox> {
        let mut result = BBox::default();
        let mut last_italic = 0.0;
        let previous = self.gc.fontface;
        let mut font = previous;
        let glyphs: Vec<(c_int, Vec<u8>, bool)> = if self.gc.fontface != SYMBOL {
            text.chars()
                .map(|character| {
                    let mut buffer = [0; 4];
                    (
                        character as c_int,
                        character.encode_utf8(&mut buffer).as_bytes().to_vec(),
                        character.is_ascii_digit(),
                    )
                })
                .collect()
        } else {
            text.bytes()
                .map(|byte| (c_int::from(byte), vec![byte], byte.is_ascii_digit()))
                .collect()
        };
        for (code, bytes, digit) in glyphs {
            if digit && font != PLAIN {
                font = PLAIN;
                self.set_font(PLAIN);
            } else if font != previous {
                font = previous;
                self.set_font(previous);
            }
            let mut glyph = self.glyph_bbox(code)?;
            glyph.italic = if self.using_italics() {
                ITALIC_FACTOR * glyph.height
            } else {
                0.0
            };
            if draw {
                self.current_x += last_italic;
                self.draw_text(&bytes, Encoding::Native)?;
                self.current_x += glyph.width;
            }
            result.width += last_italic;
            result = result.combine(glyph);
            last_italic = glyph.italic;
        }
        if font != previous {
            self.set_font(previous);
        }
        result.simple = true;
        Ok(result)
    }

    /// `RenderChar`: one glyph of the current font, for accents.
    unsafe fn char(&mut self, code: c_int, draw: bool) -> R<BBox> {
        let bbox = self.glyph_bbox(code)?;
        if draw {
            let character = char::from_u32(code as u32).unwrap_or(' ');
            let mut buffer = [0; 4];
            self.draw_text(
                character.encode_utf8(&mut buffer).as_bytes(),
                Encoding::Native,
            )?;
            self.current_x += bbox.width;
        }
        Ok(bbox)
    }

    /// `RenderStr`: a string drawn in one piece, measured glyph by glyph for its height and
    /// as a whole for its width, so kerning counts.
    unsafe fn string(&mut self, text: &[u8], draw: bool) -> R<BBox> {
        let mut glyph = BBox::default();
        let mut result = BBox::default();
        let encoding = if self.gc.fontface == SYMBOL {
            Encoding::Symbol
        } else {
            Encoding::Native
        };
        let codes: Vec<c_int> = if self.gc.fontface != SYMBOL {
            String::from_utf8_lossy(text)
                .chars()
                .map(|character| character as c_int)
                .collect()
        } else {
            text.iter().map(|&byte| c_int::from(byte)).collect()
        };
        for &code in &codes {
            glyph = self.glyph_bbox(code)?;
            result = result.combine(glyph);
        }
        if codes.len() > 1 {
            let device_text = self.device_string(text, encoding, false);
            let width = str_width(&device_text, &self.gc, self.dd)?;
            result.width = from_device_height(width, Unit::Inches, self.dd);
        }
        if draw {
            self.draw_text(text, encoding)?;
            self.current_x += result.width;
        }
        result.italic = if self.using_italics() {
            ITALIC_FACTOR * glyph.height
        } else {
            0.0
        };
        result.simple = true;
        Ok(result)
    }

    // Operators.

    unsafe fn space(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        match length(expression) {
            2 => {
                let operator = self.symbol_char(SPACE, draw)?;
                let operand = self.element(argument(expression, 1), draw)?;
                Ok(operator.combine(operand))
            }
            3 => {
                let left = self.element(argument(expression, 1), draw)?;
                let operator = self.symbol_char(SPACE, draw)?;
                let right = self.element(argument(expression, 2), draw)?;
                Ok(left.combine(operator).combine(right))
            }
            _ => Err(error("invalid mathematical annotation")),
        }
    }

    unsafe fn slash(&mut self, draw: bool) -> R<BBox> {
        let depth = 0.5 * self.tex(Parameter::AxisHeight)?;
        let height = self.cap_height()? + 0.5 * self.tex(Parameter::AxisHeight)?;
        let width = 0.5 * self.x_height()?;
        if draw {
            self.current_x += 0.5 * width;
            self.current_y -= depth;
            let start = self.converted();
            self.current_x += width;
            self.current_y += depth + height;
            let end = self.converted();
            self.current_y -= height;
            self.rule(&[start, end])?;
            self.current_x += 0.5 * width;
        }
        Ok(BBox::new(height, depth, 2.0 * width))
    }

    unsafe fn binary(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let operator = table_code(expression.car(), BINARY_OPERATORS);
        match length(expression) {
            3 => {
                let left = argument(expression, 1);
                let right = argument(expression, 2);
                if operator == ASTERISK {
                    let bbox = self.element(left, draw)?;
                    let bbox = self.italic_correction(bbox, draw);
                    Ok(bbox.combine(self.element(right, draw)?))
                } else if operator == SLASH {
                    let bbox = self.element(left, draw)?;
                    let bbox = self.italic_correction(bbox, draw);
                    let bbox = bbox.combine(self.gap(0.0, draw));
                    let bbox = bbox.combine(self.slash(draw)?);
                    let bbox = bbox.combine(self.gap(0.0, draw));
                    Ok(bbox.combine(self.element(right, draw)?))
                } else {
                    let gap = if self.style > Style::Script {
                        self.medium_space()?
                    } else {
                        0.0
                    };
                    let bbox = self.element(left, draw)?;
                    let bbox = self.italic_correction(bbox, draw);
                    let bbox = bbox.combine(self.gap(gap, draw));
                    let bbox = bbox.combine(self.symbol_char(operator, draw)?);
                    let bbox = bbox.combine(self.gap(gap, draw));
                    Ok(bbox.combine(self.element(right, draw)?))
                }
            }
            2 => {
                let gap = if self.style > Style::Script {
                    self.thin_space()?
                } else {
                    0.0
                };
                let bbox = self.symbol_char(operator, draw)?;
                let bbox = bbox.combine(self.gap(gap, draw));
                Ok(bbox.combine(self.element(argument(expression, 1), draw)?))
            }
            _ => Err(error("invalid mathematical annotation")),
        }
    }

    unsafe fn relation(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let operator = table_code(expression.car(), RELATIONS);
        if length(expression) != 3 {
            return Err(error("invalid mathematical annotation"));
        }
        let gap = if self.style > Style::Script {
            self.thick_space()?
        } else {
            0.0
        };
        let bbox = self.element(argument(expression, 1), draw)?;
        let bbox = self.italic_correction(bbox, draw);
        let bbox = bbox.combine(self.gap(gap, draw));
        let bbox = bbox.combine(self.symbol_char(operator, draw)?);
        let bbox = bbox.combine(self.gap(gap, draw));
        Ok(bbox.combine(self.element(argument(expression, 2), draw)?))
    }

    // Scripts: rules 18 to 18f of the TeXbook.

    unsafe fn subscript(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let body = argument(expression, 1);
        let sub = argument(expression, 2);
        let style = self.style;
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let body_bbox = self.element(body, draw)?;
        let body_bbox = self.italic_correction(body_bbox, draw);
        let mut shift = if body_bbox.simple {
            0.0
        } else {
            body_bbox.depth + self.tex(Parameter::SubDrop)?
        };
        let sub1 = self.tex(Parameter::Sub1)?;
        self.set_sub_style(style);
        let sub_bbox = self.element(sub, false)?;
        // GNU R subtracts 0.8 times the ordinal of its `sigma5` parameter, which is 1, not
        // 0.8 x-heights, so this term only wins for a subscript taller than 0.8 inches.
        shift = shift.max(sub1).max(sub_bbox.height - 0.8);
        let sub_bbox = self.offset_element(sub, 0.0, -shift, draw)?;
        let body_bbox = body_bbox.combine(sub_bbox);
        self.set_style(style);
        if draw {
            self.move_to(saved_x + body_bbox.width, saved_y);
        }
        Ok(body_bbox)
    }

    unsafe fn superscript(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let mut body = argument(expression, 1);
        let sup = argument(expression, 2);
        let style = self.style;
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let sub = if body.kind() == SexpType::Language && name_is(body.car(), "[") {
            let sub = argument(body, 2);
            body = argument(body, 1);
            Some(sub)
        } else {
            None
        };
        let body_bbox = self.element(body, draw)?;
        let delta = body_bbox.italic;
        let mut body_bbox = self.italic_correction(body_bbox, draw);
        let width = body_bbox.width;
        let (mut up, mut down) = if body_bbox.simple {
            (0.0, 0.0)
        } else {
            (
                body_bbox.height - self.tex(Parameter::SupDrop)?,
                body_bbox.depth + self.tex(Parameter::SubDrop)?,
            )
        };
        let theta = self.tex(Parameter::RuleThickness)?;
        let x_height = self.tex(Parameter::XHeight)?;
        let sub2 = self.tex(Parameter::Sub2)?;
        let raise = if style == Style::Display {
            self.tex(Parameter::Sup1)?
        } else if is_cramped(style) {
            self.tex(Parameter::Sup3)?
        } else {
            self.tex(Parameter::Sup2)?
        };
        self.set_sup_style(style);
        let sup_bbox = self.element(sup, false)?;
        up = up.max(raise).max(sup_bbox.depth + 0.25 * x_height);
        if let Some(sub) = sub {
            self.set_sub_style(style);
            let sub_bbox = self.element(sub, false)?;
            down = down.max(sub2);
            if (up - sup_bbox.depth) - (sub_bbox.height - down) < 4.0 * theta {
                let psi = 0.8 * x_height - (up - sup_bbox.depth);
                if psi > 0.0 {
                    up += psi;
                    down -= psi;
                }
            }
            if draw {
                self.move_to(saved_x, saved_y);
            }
            let sub_bbox = self.offset_element(sub, width, -down, draw)?;
            if draw {
                self.move_to(saved_x, saved_y);
            }
            self.set_sup_style(style);
            let sup_bbox = self.offset_element(sup, width + delta, up, draw)?;
            body_bbox = body_bbox
                .combine_aligned(sub_bbox)
                .combine_aligned(sup_bbox);
        } else {
            let sup_bbox = self.offset_element(sup, 0.0, up, draw)?;
            body_bbox = body_bbox.combine(sup_bbox);
        }
        if draw {
            self.move_to(saved_x + body_bbox.width, saved_y);
        }
        self.set_style(style);
        Ok(body_bbox)
    }

    // Accents.

    unsafe fn wide_tilde(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let bbox = self.element(argument(expression, 1), draw)?;
        let height = bbox.height;
        let total_width = bbox.width + bbox.italic;
        let delta = total_width * (1.0 - 2.0 * TILDE_INSET) / TILDE_SEGMENTS as f64;
        let start = TILDE_INSET * total_width;
        let accent_gap = ACCENT_GAP * self.cap_height()?;
        let hat_height = 0.5 * HAT_HEIGHT * self.cap_height()?;
        let step = std::f64::consts::TAU / TILDE_SEGMENTS as f64;
        if draw {
            let base_x = saved_x;
            let base_y = saved_y + height + accent_gap;
            let mut points = Vec::with_capacity(TILDE_SEGMENTS + 3);
            self.move_to(base_x, base_y);
            points.push(self.converted());
            for index in 0..=TILDE_SEGMENTS {
                let x = start + index as f64 * delta;
                let y = 0.5 * hat_height * ((step * index as f64).sin() + 1.0);
                self.move_to(base_x + x, base_y + y);
                points.push(self.converted());
            }
            self.move_to(base_x + total_width, base_y + hat_height);
            points.push(self.converted());
            self.rule(&points)?;
            self.move_to(saved_x + total_width, saved_y);
        }
        Ok(BBox::new(
            height + accent_gap + hat_height,
            bbox.depth,
            total_width,
        ))
    }

    unsafe fn wide_hat(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let bbox = self.element(argument(expression, 1), draw)?;
        let accent_gap = ACCENT_GAP * self.cap_height()?;
        let hat_height = HAT_HEIGHT * self.cap_height()?;
        let total_width = bbox.width + bbox.italic;
        if draw {
            self.move_to(saved_x, saved_y + bbox.height + accent_gap);
            let left = self.converted();
            self.current_x += 0.5 * total_width;
            self.current_y += hat_height;
            let peak = self.converted();
            self.current_x += 0.5 * total_width;
            self.current_y -= hat_height;
            let right = self.converted();
            self.rule(&[left, peak, right])?;
            self.move_to(saved_x + bbox.width, saved_y);
        }
        Ok(bbox.enlarge(accent_gap + hat_height, 0.0, 0.0))
    }

    unsafe fn bar(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let bbox = self.element(argument(expression, 1), draw)?;
        let accent_gap = ACCENT_GAP * self.cap_height()?;
        if draw {
            self.move_to(saved_x + bbox.italic, saved_y + bbox.height + accent_gap);
            let left = self.converted();
            self.current_x += bbox.width;
            let right = self.converted();
            self.rule(&[left, right])?;
            self.move_to(saved_x + bbox.width, saved_y);
        }
        Ok(bbox.enlarge(accent_gap, 0.0, 0.0))
    }

    unsafe fn accent(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        if length(expression) != 2 {
            return Err(error_in(expression, "invalid accent"));
        }
        let body = argument(expression, 1);
        let code = table_code(expression.car(), ACCENTS);
        if code == 0 {
            return Err(error_in(expression, "invalid accent"));
        }
        let body_bbox = self.element(body, false)?;
        let italic = body_bbox.italic;
        let symbol = code == RING || code == DOT_MATH;
        let accent_bbox = if symbol {
            self.symbol_char(code, false)?
        } else {
            self.char(code, false)?
        };
        let width = (body_bbox.width + body_bbox.italic).max(accent_bbox.width);
        let xoffset = 0.5 * (width - body_bbox.width);
        let body_bbox = self.gap(xoffset, draw);
        let body_bbox = body_bbox.combine(self.element(body, draw)?);
        let body_bbox = body_bbox.combine(self.gap(xoffset, draw));
        self.move_to(saved_x, saved_y);
        let xoffset = 0.5 * (width - accent_bbox.width) + 0.9 * italic;
        let yoffset = body_bbox.height + accent_bbox.depth + 0.1 * self.cap_height()?;
        if draw {
            self.move_to(saved_x + xoffset, saved_y + yoffset);
            if symbol {
                self.symbol_char(code, draw)?;
            } else {
                self.char(code, draw)?;
            }
        }
        let body_bbox = body_bbox.combine_offset(accent_bbox, xoffset, yoffset);
        if draw {
            self.move_to(saved_x + width, saved_y);
        }
        Ok(body_bbox)
    }

    // Fractions: rules 15 to 15e of the TeXbook.

    unsafe fn fraction(&mut self, expression: Sexp, rule: bool, draw: bool) -> R<BBox> {
        let numerator = argument(expression, 1);
        let denominator = argument(expression, 2);
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let style = self.style;
        self.set_numerator_style(style);
        let numerator_bbox = self.element(numerator, false)?;
        let numerator_bbox = self.italic_correction(numerator_bbox, false);
        self.set_denominator_style(style);
        let denominator_bbox = self.element(denominator, false)?;
        let denominator_bbox = self.italic_correction(denominator_bbox, false);
        self.set_style(style);

        let width = numerator_bbox.width.max(denominator_bbox.width);
        let (numerator_shift, denominator_shift) = if numerator_bbox.width > denominator_bbox.width
        {
            (0.0, (numerator_bbox.width - denominator_bbox.width) / 2.0)
        } else {
            ((denominator_bbox.width - numerator_bbox.width) / 2.0, 0.0)
        };
        let (up, down) = self.fraction_shifts(numerator_bbox, denominator_bbox)?;

        self.move_to(saved_x, saved_y);
        self.set_numerator_style(style);
        let numerator_bbox = self.offset_element(numerator, numerator_shift, up, draw)?;

        self.move_to(saved_x, saved_y);
        self.set_denominator_style(style);
        let denominator_bbox = self.offset_element(denominator, denominator_shift, -down, draw)?;

        self.set_style(style);
        if draw {
            if rule {
                self.move_to(saved_x, saved_y);
                let axis = self.axis_height()?;
                self.current_y += axis;
                let left = self.converted();
                self.current_x += width;
                let right = self.converted();
                self.rule(&[left, right])?;
                let axis = self.axis_height()?;
                self.current_y -= axis;
            }
            self.move_to(saved_x + width, saved_y);
        }
        Ok(numerator_bbox.combine_aligned(denominator_bbox))
    }

    /// `NumDenomVShift`: how far the numerator rises and the denominator drops.
    unsafe fn fraction_shifts(&mut self, numerator: BBox, denominator: BBox) -> R<(f64, f64)> {
        let axis = self.tex(Parameter::AxisHeight)?;
        let theta = self.tex(Parameter::RuleThickness)?;
        let (mut up, mut down, phi) = if self.style > Style::Text {
            (
                self.tex(Parameter::Num1)?,
                self.tex(Parameter::Denom1)?,
                3.0 * theta,
            )
        } else {
            (
                self.tex(Parameter::Num2)?,
                self.tex(Parameter::Denom2)?,
                theta,
            )
        };
        let delta = (up - numerator.depth) - (axis + 0.5 * theta);
        if delta < phi {
            up += phi - delta;
        }
        let delta = (axis + 0.5 * theta) - (denominator.height - down);
        if delta < phi {
            down += phi - delta;
        }
        Ok((up, down))
    }

    unsafe fn underline(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let body = argument(expression, 1);
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let measured = self.element(body, false)?;
        let width = self.italic_correction(measured, false).width;
        self.move_to(saved_x, saved_y);
        let bbox = self.element(body, draw)?;
        let gap = 0.1 * self.cap_height()?;
        let depth = bbox.depth + gap;
        if draw {
            self.move_to(saved_x, saved_y);
            self.current_y -= depth;
            let left = self.converted();
            self.current_x += width;
            let right = self.converted();
            self.rule(&[left, right])?;
            self.current_y += depth;
            self.move_to(saved_x + width, saved_y);
        }
        Ok(bbox.enlarge(0.0, gap, 0.0))
    }

    // Groups and delimiters.

    unsafe fn delimiter(&mut self, code: c_int, draw: bool) -> R<BBox> {
        let saved_cex = self.gc.cex;
        self.gc.cex *= DELIMITER_MAGNIFICATION;
        let bbox = self.symbol_char(code, draw);
        self.gc.cex = saved_cex;
        bbox
    }

    unsafe fn group(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let saved_cex = self.gc.cex;
        if length(expression) != 4 {
            return Err(error_in(expression, "invalid group specification"));
        }
        let mut bbox = BBox::default();
        let code = delimiter_code(expression, argument(expression, 1))?;
        self.gc.cex *= DELIMITER_MAGNIFICATION;
        if code != c_int::from(b'.') {
            bbox = self.symbol_char(code, draw)?;
        }
        self.gc.cex = saved_cex;
        bbox = bbox.combine(self.element(argument(expression, 2), draw)?);
        bbox = self.italic_correction(bbox, draw);
        let code = delimiter_code(expression, argument(expression, 3))?;
        self.gc.cex *= DELIMITER_MAGNIFICATION;
        if code != c_int::from(b'.') {
            bbox = bbox.combine(self.symbol_char(code, draw)?);
        }
        self.gc.cex = saved_cex;
        Ok(bbox)
    }

    /// `RenderDelim`: a delimiter built from the symbol font's top, middle, bottom and
    /// extension pieces, reaching `distance` above and below the axis.
    unsafe fn built_delimiter(&mut self, which: u8, distance: f64, draw: bool) -> R<BBox> {
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let previous = self.set_font(SYMBOL);
        let axis = self.tex(Parameter::AxisHeight)?;
        let (top, extension, bottom, middle) = match which {
            b'.' => {
                self.set_font(previous);
                return Ok(BBox::default());
            }
            b'|' => (239, 239, 239, 0),
            b'(' => (230, 231, 232, 0),
            b')' => (246, 247, 248, 0),
            b'[' => (233, 234, 235, 0),
            b']' => (249, 250, 251, 0),
            b'{' => (236, 239, 238, 237),
            b'}' => (252, 239, 254, 253),
            _ => {
                self.set_font(previous);
                return Err(error("group is incomplete"));
            }
        };
        let braces = which == b'{' || which == b'}';
        let top_bbox = self.glyph_bbox(top)?;
        let extension_bbox = self.glyph_bbox(extension)?;
        let bottom_bbox = self.glyph_bbox(bottom)?;
        let mut distance = distance;
        if braces {
            if 1.2 * (top_bbox.height + top_bbox.depth) > distance {
                distance = 1.2 * (top_bbox.height + bottom_bbox.depth);
            }
        } else if 0.8 * (top_bbox.height + top_bbox.depth) > distance {
            distance = 0.8 * (top_bbox.height + top_bbox.depth);
        }
        let extension_height = extension_bbox.height + extension_bbox.depth;
        let top_shift = distance - top_bbox.height + axis;
        let bottom_shift = distance - bottom_bbox.depth - axis;
        let extension_shift = 0.5 * (extension_bbox.height - extension_bbox.depth);
        let top_bbox = top_bbox.shift(top_shift);
        let bottom_bbox = bottom_bbox.shift(-bottom_shift);
        let mut result = top_bbox.combine_aligned(bottom_bbox);
        if braces {
            let middle_bbox = self.glyph_bbox(middle)?;
            let middle_shift = axis - 0.5 * (middle_bbox.height - middle_bbox.depth);
            result = result.combine_aligned(middle_bbox.shift(middle_shift));
            if draw {
                self.move_to(saved_x, saved_y + top_shift);
                self.symbol_char(top, draw)?;
                self.move_to(saved_x, saved_y + middle_shift);
                self.symbol_char(middle, draw)?;
                self.move_to(saved_x, saved_y - bottom_shift);
                self.symbol_char(bottom, draw)?;
                self.move_to(saved_x + result.width, saved_y);
            }
        } else if draw {
            self.move_to(saved_x, saved_y + top_shift);
            self.symbol_char(top, draw)?;
            self.move_to(saved_x, saved_y - bottom_shift);
            self.symbol_char(bottom, draw)?;
            let top_end = axis + distance - (top_bbox.height + top_bbox.depth);
            let bottom_end = axis - distance + (bottom_bbox.height + bottom_bbox.depth);
            let count = ((top_end - bottom_end) / (0.99 * extension_height)).ceil() as i32;
            if count > 0 {
                let delta = (top_end - bottom_end) / f64::from(count);
                for index in 0..count {
                    self.move_to(
                        saved_x,
                        saved_y + bottom_end + (f64::from(index) + 0.5) * delta - extension_shift,
                    );
                    self.symbol_char(extension, draw)?;
                }
            }
            self.move_to(saved_x + result.width, saved_y);
        }
        self.set_font(previous);
        Ok(result)
    }

    unsafe fn big_group(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let axis = self.tex(Parameter::AxisHeight)?;
        let extra = 0.2 * self.x_height()?;
        if length(expression) != 4 {
            return Err(error_in(expression, "invalid group specification"));
        }
        let left = delimiter_code(expression, argument(expression, 1))?;
        let right = delimiter_code(expression, argument(expression, 3))?;
        let body = argument(expression, 2);
        let measured = self.element(body, false)?;
        let distance = (measured.height - axis).max(measured.depth + axis);
        let bbox = self.built_delimiter(left as u8, distance + extra, draw)?;
        let bbox = bbox.combine(self.element(body, draw)?);
        let bbox = self.italic_correction(bbox, draw);
        Ok(bbox.combine(self.built_delimiter(right as u8, distance + extra, draw)?))
    }

    unsafe fn parenthesis(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let bbox = self.delimiter(PAREN_LEFT, draw)?;
        let bbox = bbox.combine(self.element(argument(expression, 1), draw)?);
        let bbox = self.italic_correction(bbox, draw);
        Ok(bbox.combine(self.delimiter(PAREN_RIGHT, draw)?))
    }

    // Big operators.

    unsafe fn integral_symbol(&mut self, draw: bool) -> R<BBox> {
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        if self.style > Style::Text {
            let top = self.symbol_char(243, false)?;
            let bottom = self.symbol_char(245, false)?;
            let shift = self.tex(Parameter::AxisHeight)? + 0.99 * top.depth;
            self.current_y += shift;
            let top = self.symbol_char(243, draw)?.shift(shift);
            self.move_to(saved_x, saved_y);
            let shift = self.tex(Parameter::AxisHeight)? - 0.99 * bottom.height;
            self.current_y += shift;
            let bottom = self.symbol_char(245, draw)?.shift(shift);
            if draw {
                self.move_to(saved_x + top.width.max(bottom.width), saved_y);
            } else {
                self.move_to(saved_x, saved_y);
            }
            Ok(top.combine_aligned(bottom))
        } else {
            self.symbol_char(0o362, draw)
        }
    }

    unsafe fn integral(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let count = length(expression);
        let style = self.style;
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let mut operator = self.integral_symbol(draw)?;
        let width = operator.width;
        self.move_to(saved_x, saved_y);
        if count > 2 {
            let lower = argument(expression, 2);
            let hshift = 0.5 * width + self.thin_space()?;
            self.set_sub_style(style);
            let measured = self.element(lower, false)?;
            let vshift = operator.depth + measured.center_shift();
            let lower_bbox = self.offset_element(lower, hshift, -vshift, draw)?;
            operator = operator.combine_aligned(lower_bbox);
            self.set_style(style);
            self.move_to(saved_x, saved_y);
        }
        if count > 3 {
            let upper = argument(expression, 3);
            let hshift = width + self.thin_space()?;
            self.set_sup_style(style);
            let measured = self.element(upper, false)?;
            let vshift = operator.height - measured.center_shift();
            let upper_bbox = self.offset_element(upper, hshift, vshift, draw)?;
            operator = operator.combine_aligned(upper_bbox);
            self.set_style(style);
            self.move_to(saved_x, saved_y);
        }
        self.current_x += operator.width;
        if count > 1 {
            let body = self.element(argument(expression, 1), draw)?;
            operator = operator.combine(body);
        }
        Ok(operator)
    }

    unsafe fn operator_symbol(&mut self, operator: Sexp, draw: bool) -> R<BBox> {
        let saved_cex = self.gc.cex;
        let display = self.style > Style::Text;
        let code = table_code(operator, OPERATORS);
        if code == SUM || code == PRODUCT || code == UNION || code == INTERSECTION {
            if display {
                self.gc.cex *= OPERATOR_MAGNIFICATION;
                let mut bbox = self.symbol_char(code, false)?;
                let shift = 0.5 * (bbox.height - bbox.depth) - self.tex(Parameter::AxisHeight)?;
                if draw {
                    self.current_y -= shift;
                    bbox = self.symbol_char(code, true)?;
                    self.current_y += shift;
                }
                self.gc.cex = saved_cex;
                Ok(bbox.shift(-shift))
            } else {
                self.symbol_char(code, draw)
            }
        } else {
            let previous = self.set_font(PLAIN);
            let bbox = self.string(values::symbol_name(operator).as_bytes(), draw);
            self.set_font(previous);
            bbox
        }
    }

    unsafe fn operator(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let count = length(expression);
        let style = self.style;
        let head = expression.car();
        let mut operator = self.operator_symbol(head, false)?;
        let mut width = operator.width;
        let mut lower_bbox = BBox::default();
        let mut upper_bbox = BBox::default();
        let mut lower_shift = 0.0;
        let mut upper_shift = 0.0;
        if count > 2 {
            self.set_sub_style(style);
            lower_bbox = self.element(argument(expression, 2), false)?;
            self.set_style(style);
            width = width.max(lower_bbox.width);
            lower_shift = self
                .tex(Parameter::BigOpSpacing)?
                .max(self.tex(Parameter::BigOpSpacing)? - lower_bbox.height);
            lower_shift += operator.depth + lower_bbox.height;
        }
        if count > 3 {
            self.set_sup_style(style);
            upper_bbox = self.element(argument(expression, 3), false)?;
            self.set_style(style);
            width = width.max(upper_bbox.width);
            upper_shift = self
                .tex(Parameter::BigOpSpacing)?
                .max(self.tex(Parameter::BigOpSpacing)? - upper_bbox.depth);
            upper_shift += operator.height + upper_bbox.depth;
        }
        let hshift = 0.5 * (width - operator.width);
        operator = self.gap(hshift, draw);
        operator = operator.combine(self.operator_symbol(head, draw)?);
        self.move_to(saved_x, saved_y);
        if count > 2 {
            self.set_sub_style(style);
            let hshift = 0.5 * (width - lower_bbox.width);
            let lower = self.offset_element(argument(expression, 2), hshift, -lower_shift, draw)?;
            self.set_style(style);
            operator = operator.combine_aligned(lower);
            self.move_to(saved_x, saved_y);
        }
        if count > 3 {
            self.set_sup_style(style);
            let hshift = 0.5 * (width - upper_bbox.width);
            let upper = self.offset_element(argument(expression, 3), hshift, upper_shift, draw)?;
            self.set_style(style);
            operator = operator.combine_aligned(upper);
            self.move_to(saved_x, saved_y);
        }
        let spacing = self.tex(Parameter::BigOpSpacing)?;
        operator = operator.enlarge(spacing, spacing, 0.0);
        if draw {
            self.current_x += width;
        }
        let thin = self.thin_space()?;
        operator = operator.combine(self.gap(thin, draw));
        let body = self.element(argument(expression, 1), draw)?;
        Ok(operator.combine(body))
    }

    // Radicals.

    unsafe fn script(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let style = self.style;
        self.set_sup_style(style);
        let bbox = self.element(expression, draw);
        self.set_style(style);
        bbox
    }

    unsafe fn radical(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let body = argument(expression, 1);
        let order = argument(expression, 2);
        let style = self.style;
        let (saved_x, saved_y) = (self.current_x, self.current_y);
        let radical_gap = RADICAL_GAP * self.x_height()?;
        let radical_space = RADICAL_SPACE * self.x_height()?;
        let radical_trail = self.mu_space()?;
        self.set_cramped_style(style);
        let body_bbox = self.element(body, false)?;
        let body_bbox = self.italic_correction(body_bbox, false);

        let radical_width = 0.6 * self.cap_height()?;
        let radical_height = body_bbox.height + radical_gap;
        let twiddle_height = body_bbox.center_shift();

        let mut lead_width = radical_width;
        let lead_height = radical_height;
        let mut order_bbox = if order != nil() {
            self.set_sup_style(style);
            let mut order_bbox = self.script(order, false)?;
            lead_width = lead_width.max(order_bbox.width + 0.4 * radical_width);
            let hshift = lead_width - order_bbox.width - 0.4 * radical_width;
            let mut vshift = lead_height - order_bbox.height;
            if vshift - order_bbox.depth < twiddle_height + radical_gap {
                vshift = twiddle_height + order_bbox.depth + radical_gap;
            }
            if draw {
                self.move_to(saved_x + hshift, saved_y + vshift);
                order_bbox = self.script(order, draw)?;
            }
            order_bbox.enlarge(vshift, 0.0, hshift)
        } else {
            BBox::default()
        };
        if draw {
            self.move_to(saved_x + lead_width - radical_width, saved_y);
            self.current_y += 0.8 * twiddle_height;
            let first = self.converted();
            self.current_y += 0.2 * twiddle_height;
            self.current_x += 0.3 * radical_width;
            let second = self.converted();
            self.current_y -= twiddle_height + body_bbox.depth;
            self.current_x += 0.3 * radical_width;
            let third = self.converted();
            self.current_y += body_bbox.depth + body_bbox.height + radical_gap;
            self.current_x += 0.4 * radical_width;
            let fourth = self.converted();
            self.current_x += radical_space + body_bbox.width + radical_trail;
            let fifth = self.converted();
            self.rule(&[first, second, third, fourth, fifth])?;
            self.move_to(saved_x, saved_y);
        }
        order_bbox = order_bbox.combine_aligned(self.gap(lead_width + radical_space, draw));
        self.set_cramped_style(style);
        order_bbox = order_bbox.combine(self.element(body, draw)?);
        order_bbox = order_bbox.combine(self.gap(2.0 * radical_trail, draw));
        order_bbox = order_bbox.enlarge(radical_gap, 0.0, 0.0);
        self.set_style(style);
        Ok(order_bbox)
    }

    unsafe fn absolute(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let body = argument(expression, 1);
        let measured = self.element(body, false)?;
        let (height, depth) = (measured.height, measured.depth);
        let mu = self.mu_space()?;
        let mut bbox = self.gap(mu, draw);
        if draw {
            self.vertical_rule(height, depth)?;
        }
        let mu = self.mu_space()?;
        bbox = bbox.combine(self.gap(mu, draw));
        bbox = bbox.combine(self.element(body, draw)?);
        bbox = self.italic_correction(bbox, draw);
        let mu = self.mu_space()?;
        bbox = bbox.combine(self.gap(mu, draw));
        if draw {
            self.vertical_rule(height, depth)?;
        }
        let mu = self.mu_space()?;
        Ok(bbox.combine(self.gap(mu, draw)))
    }

    unsafe fn vertical_rule(&mut self, height: f64, depth: f64) -> R<()> {
        self.current_y -= depth;
        let bottom = self.converted();
        self.current_y += depth + height;
        let top = self.converted();
        self.rule(&[bottom, top])?;
        self.current_y -= height;
        Ok(())
    }

    // Fonts, styles and phantoms.

    unsafe fn with_font(&mut self, font: c_int, expression: Sexp, draw: bool) -> R<BBox> {
        let previous = self.set_font(font);
        let bbox = self.element(argument(expression, 1), draw);
        self.set_font(previous);
        bbox
    }

    unsafe fn with_style(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let previous = self.style;
        let head = expression.car();
        let style = if name_is(head, "displaystyle") {
            Style::Display
        } else if name_is(head, "textstyle") {
            Style::Text
        } else if name_is(head, "scriptstyle") {
            Style::Script
        } else {
            Style::ScriptScript
        };
        self.set_style(style);
        let bbox = self.element(argument(expression, 1), draw);
        self.set_style(previous);
        bbox
    }

    unsafe fn phantom(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let mut bbox = self.element(argument(expression, 1), false)?;
        if name_is(expression.car(), "vphantom") {
            bbox.width = 0.0;
            bbox.italic = 0.0;
        } else {
            self.gap(bbox.width, draw);
        }
        Ok(bbox)
    }

    // Juxtaposition and lists.

    unsafe fn concatenate(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let mut bbox = BBox::default();
        let mut cell = expression.cdr();
        let count = length(cell);
        for index in 0..count {
            bbox = bbox.combine(self.element(cell.car(), draw)?);
            if index != count - 1 {
                bbox = self.italic_correction(bbox, draw);
            }
            cell = cell.cdr();
        }
        Ok(bbox)
    }

    unsafe fn comma_list(&mut self, list: Sexp, draw: bool) -> R<BBox> {
        let mut bbox = BBox::default();
        let small = 0.4 * self.thin_space()?;
        let mut cell = list;
        for index in 0..length(list) {
            let item = cell.car();
            if index > 0 {
                bbox = bbox.combine(self.symbol_char(COMMA, draw)?);
                bbox = bbox.combine(self.symbol_char(SPACE, draw)?);
            }
            if name_is(item, "...") {
                bbox = bbox.combine(self.symbol_char(ELLIPSIS, draw)?);
                bbox = bbox.combine(self.gap(small, draw));
            } else {
                bbox = bbox.combine(self.element(item, draw)?);
            }
            cell = cell.cdr();
        }
        Ok(bbox)
    }

    /// `RenderExpression`: any other call, drawn as a function call `f(x, y)`.
    unsafe fn function_call(&mut self, expression: Sexp, draw: bool) -> R<BBox> {
        let head = expression.car();
        let bbox = if head.kind() == SexpType::Symbol {
            match translated_symbol(head) {
                0 => self.string(values::symbol_name(head).as_bytes(), draw)?,
                code => self.symbol_char(code, draw)?,
            }
        } else {
            self.element(head, draw)?
        };
        let bbox = self.italic_correction(bbox, draw);
        let bbox = bbox.combine(self.delimiter(PAREN_LEFT, draw)?);
        let bbox = bbox.combine(self.comma_list(expression.cdr(), draw)?);
        let bbox = self.italic_correction(bbox, draw);
        Ok(bbox.combine(self.delimiter(PAREN_RIGHT, draw)?))
    }

    // Boxes and movement.

    fn italic_correction(&mut self, mut bbox: BBox, draw: bool) -> BBox {
        if bbox.italic > 0.0 {
            if draw {
                self.current_x += bbox.italic;
            }
            bbox.width += bbox.italic;
            bbox.italic = 0.0;
        }
        bbox
    }

    fn gap(&mut self, gap: f64, draw: bool) -> BBox {
        if draw {
            self.current_x += gap;
        }
        BBox::new(0.0, 0.0, gap)
    }

    fn move_to(&mut self, x: f64, y: f64) {
        self.current_x = x;
        self.current_y = y;
    }

    /// The pen position rotated about the reference point, in device units.
    unsafe fn converted(&self) -> (f64, f64) {
        let dx = self.current_x - self.reference_x;
        let dy = self.current_y - self.reference_y;
        let x = self.reference_x + dx * self.cos - dy * self.sin;
        let y = self.reference_y + dy * self.cos + dx * self.sin;
        (
            to_device_x(x, Unit::Inches, self.dd),
            to_device_y(y, Unit::Inches, self.dd),
        )
    }

    /// Draws a solid line through `points` no wider than one unit, as every rule, bar and
    /// radical sign is drawn.
    unsafe fn rule(&mut self, points: &[(f64, f64)]) -> R<()> {
        let (saved_lty, saved_lwd) = (self.gc.lty, self.gc.lwd);
        self.gc.lty = LTY_SOLID;
        if self.gc.lwd > 1.0 {
            self.gc.lwd = 1.0;
        }
        let x: Vec<f64> = points.iter().map(|point| point.0).collect();
        let y: Vec<f64> = points.iter().map(|point| point.1).collect();
        let outcome = polyline(&x, &y, &self.gc, self.dd);
        self.gc.lty = saved_lty;
        self.gc.lwd = saved_lwd;
        outcome
    }

    // Glyphs and text.

    /// `GlyphBBox`: a glyph's box. A device that wants the symbol font in UTF-8 measures the
    /// glyph by its Unicode code point.
    unsafe fn glyph_bbox(&self, code: c_int) -> R<BBox> {
        let want_utf8 = (*(*self.dd).dev).want_symbol_utf8 != 0;
        let code = if want_utf8 && self.gc.fontface == SYMBOL {
            -adobe_symbol_unicode(code)
        } else {
            code
        };
        let [ascent, descent, width] = metric_info(code, &self.gc, self.dd)?;
        Ok(BBox {
            height: from_device_height(ascent, Unit::Inches, self.dd),
            depth: from_device_height(descent, Unit::Inches, self.dd),
            width: from_device_height(width, Unit::Inches, self.dd),
            italic: 0.0,
            simple: true,
        })
    }

    unsafe fn draw_text(&self, bytes: &[u8], encoding: Encoding) -> R<()> {
        let (x, y) = self.converted();
        let string = self.device_string(bytes, encoding, true);
        text(x, y, &string, 0.0, 0.0, self.angle, &self.gc, self.dd)
    }

    /// The text the engine hands the device for `bytes`. The symbol font's text is
    /// re-encoded as UTF-8 for a device that wants that, and a device that wants Latin-1
    /// symbol text (`NA`) gets drawn text re-encoded from Latin-1. A device that takes the
    /// Adobe encoding itself also gets UTF-8, because the engine's text is a Rust string.
    unsafe fn device_string(&self, bytes: &[u8], encoding: Encoding, drawing: bool) -> String {
        let symbol_target = self.gc.fontface == SYMBOL || encoding == Encoding::Symbol;
        let want = (*(*self.dd).dev).want_symbol_utf8;
        if symbol_target && want == heap::object::NA_LOGICAL && drawing {
            return bytes.iter().map(|&byte| char::from(byte)).collect();
        }
        if symbol_target && encoding == Encoding::Symbol {
            return bytes
                .iter()
                .map(|&byte| {
                    char::from_u32(adobe_symbol_unicode(c_int::from(byte)) as u32).unwrap_or(' ')
                })
                .collect();
        }
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn set_font(&mut self, font: c_int) -> c_int {
        std::mem::replace(&mut self.gc.fontface, font)
    }

    fn using_italics(&self) -> bool {
        self.gc.fontface == ITALIC || self.gc.fontface == BOLD_ITALIC
    }

    // Styles.

    fn set_style(&mut self, style: Style) {
        self.gc.cex = match style {
            Style::Display | Style::Text | Style::DisplayCramped | Style::TextCramped => {
                self.base_cex
            }
            Style::Script | Style::ScriptCramped => 0.7 * self.base_cex,
            Style::ScriptScript | Style::ScriptScriptCramped => 0.5 * self.base_cex,
        };
        self.style = style;
    }

    fn set_cramped_style(&mut self, style: Style) {
        self.set_style(match style {
            Style::Display | Style::DisplayCramped => Style::DisplayCramped,
            Style::Text | Style::TextCramped => Style::TextCramped,
            Style::Script | Style::ScriptCramped => Style::ScriptCramped,
            Style::ScriptScript | Style::ScriptScriptCramped => Style::ScriptScriptCramped,
        });
    }

    fn set_sup_style(&mut self, style: Style) {
        self.set_style(match style {
            Style::Display | Style::Text => Style::Script,
            Style::DisplayCramped | Style::TextCramped => Style::ScriptCramped,
            Style::Script | Style::ScriptScript => Style::ScriptScript,
            Style::ScriptCramped | Style::ScriptScriptCramped => Style::ScriptScriptCramped,
        });
    }

    fn set_sub_style(&mut self, style: Style) {
        self.set_style(match style {
            Style::Display | Style::Text | Style::DisplayCramped | Style::TextCramped => {
                Style::ScriptCramped
            }
            _ => Style::ScriptScriptCramped,
        });
    }

    fn set_numerator_style(&mut self, style: Style) {
        match style {
            Style::Display => self.set_style(Style::Text),
            Style::DisplayCramped => self.set_style(Style::TextCramped),
            _ => self.set_sup_style(style),
        }
    }

    fn set_denominator_style(&mut self, style: Style) {
        if style > Style::Text {
            self.set_style(Style::TextCramped);
        } else {
            self.set_sub_style(style);
        }
    }

    // Font dimensions, in inches.

    unsafe fn glyph_metric(&self, code: u8) -> R<[f64; 3]> {
        let metrics = metric_info(c_int::from(code), &self.gc, self.dd)?;
        Ok(metrics.map(|value| from_device_height(value, Unit::Inches, self.dd)))
    }

    unsafe fn x_height(&self) -> R<f64> {
        Ok(self.glyph_metric(b'x')?[0])
    }

    unsafe fn cap_height(&self) -> R<f64> {
        Ok(self.glyph_metric(b'X')?[0])
    }

    unsafe fn axis_height(&self) -> R<f64> {
        let metrics = metric_info(c_int::from(b'+'), &self.gc, self.dd)?;
        Ok(from_device_height(0.5 * metrics[0], Unit::Inches, self.dd))
    }

    unsafe fn em_fraction(&self, fraction: f64) -> R<f64> {
        let metrics = metric_info(c_int::from(b'M'), &self.gc, self.dd)?;
        Ok(from_device_height(
            fraction * metrics[2],
            Unit::Inches,
            self.dd,
        ))
    }

    unsafe fn thin_space(&self) -> R<f64> {
        self.em_fraction(0.166_666_666_666_666_66)
    }

    unsafe fn medium_space(&self) -> R<f64> {
        self.em_fraction(0.222_222_222_222_222_22)
    }

    unsafe fn thick_space(&self) -> R<f64> {
        self.em_fraction(0.277_777_777_777_777_77)
    }

    unsafe fn mu_space(&self) -> R<f64> {
        self.em_fraction(0.055_555_555_555_555_55)
    }

    unsafe fn tex(&self, parameter: Parameter) -> R<f64> {
        let rule = 0.015;
        Ok(match parameter {
            Parameter::XHeight => self.x_height()?,
            Parameter::Num1 => {
                self.axis_height()?
                    + 3.51 * rule
                    + 0.15 * self.cap_height()?
                    + SUBS * self.glyph_metric(b'g')?[1]
            }
            Parameter::Num2 => {
                self.axis_height()? + 1.51 * rule + 0.083_333_33 * self.cap_height()?
            }
            Parameter::Denom1 => {
                -self.axis_height()?
                    + 3.51 * rule
                    + SUBS * self.glyph_metric(b'0')?[0]
                    + 0.344_444 * self.cap_height()?
            }
            Parameter::Denom2 => {
                -self.axis_height()?
                    + 1.51 * rule
                    + SUBS * self.glyph_metric(b'0')?[0]
                    + 0.083_333_33 * self.cap_height()?
            }
            Parameter::Sup1 => 0.95 * self.x_height()?,
            Parameter::Sup2 => 0.825 * self.x_height()?,
            Parameter::Sup3 => 0.7 * self.x_height()?,
            Parameter::Sub1 => 0.35 * self.x_height()?,
            Parameter::Sub2 => 0.45 * self.cap_height()?,
            Parameter::SupDrop => 0.386_111_1 * self.cap_height()?,
            Parameter::SubDrop => 0.05 * self.cap_height()?,
            Parameter::AxisHeight => self.axis_height()?,
            Parameter::RuleThickness => rule,
            Parameter::BigOpSpacing => 0.15 * self.cap_height()?,
        })
    }
}

impl BBox {
    fn new(height: f64, depth: f64, width: f64) -> BBox {
        BBox {
            height,
            depth,
            width,
            italic: 0.0,
            simple: false,
        }
    }

    fn shift(self, up: f64) -> BBox {
        BBox {
            height: self.height + up,
            depth: self.depth - up,
            ..self
        }
    }

    fn enlarge(self, height: f64, depth: f64, width: f64) -> BBox {
        BBox {
            height: self.height + height,
            depth: self.depth + depth,
            width: self.width + width,
            ..self
        }
    }

    /// Side by side: the second box's italic correction and simplicity carry over.
    fn combine(self, other: BBox) -> BBox {
        BBox {
            height: self.height.max(other.height),
            depth: self.depth.max(other.depth),
            width: self.width + other.width,
            italic: other.italic,
            simple: other.simple,
        }
    }

    /// Stacked at the same origin.
    fn combine_aligned(self, other: BBox) -> BBox {
        BBox {
            height: self.height.max(other.height),
            depth: self.depth.max(other.depth),
            width: self.width.max(other.width),
            italic: 0.0,
            simple: false,
        }
    }

    /// With `other` placed at `(x, y)` from this box's origin, without italic corrections.
    fn combine_offset(self, other: BBox, x: f64, y: f64) -> BBox {
        BBox {
            height: self.height.max(other.height + y),
            depth: self.depth.max(other.depth - y),
            width: self.width.max(other.width + x),
            italic: 0.0,
            simple: false,
        }
    }

    fn center_shift(self) -> f64 {
        0.5 * (self.height - self.depth)
    }
}

fn is_cramped(style: Style) -> bool {
    matches!(
        style,
        Style::DisplayCramped
            | Style::TextCramped
            | Style::ScriptCramped
            | Style::ScriptScriptCramped
    )
}

/// The `index`th element of a call, the function being element 0.
unsafe fn argument(expression: Sexp, index: usize) -> Sexp {
    let mut cell = expression;
    for _ in 0..index {
        cell = cell.cdr();
    }
    if cell == nil() { nil() } else { cell.car() }
}

unsafe fn name_is(expression: Sexp, name: &str) -> bool {
    expression.kind() == SexpType::Symbol && values::symbol_name(expression) == name
}

unsafe fn table_code(expression: Sexp, table: &[(&str, c_int)]) -> c_int {
    if expression.kind() != SexpType::Symbol {
        return 0;
    }
    let name = values::symbol_name(expression);
    table
        .iter()
        .find(|(entry, _)| *entry == name)
        .map_or(0, |&(_, code)| code)
}

unsafe fn translated_symbol(expression: Sexp) -> c_int {
    table_code(expression, SYMBOL_NAMES)
}

/// `Rf_AdobeSymbol2ucs2`: the Unicode code point of an Adobe Symbol code, or 0.
fn adobe_symbol_unicode(code: c_int) -> c_int {
    usize::try_from(code - 32)
        .ok()
        .and_then(|index| ADOBE_SYMBOL_UNICODE.get(index))
        .map_or(0, |&unicode| c_int::from(unicode))
}

/// `DelimCode`: the delimiter a `group` or `bgroup` argument names.
unsafe fn delimiter_code(expression: Sexp, head: Sexp) -> R<c_int> {
    let mut code = 0;
    if head.kind() == SexpType::Symbol {
        code = match values::symbol_name(head).as_ref() {
            "lfloor" => BRACKET_LEFT_BOTTOM,
            "rfloor" => BRACKET_RIGHT_BOTTOM,
            "lceil" => BRACKET_LEFT_TOP,
            "rceil" => BRACKET_RIGHT_TOP,
            "langle" => ANGLE_LEFT,
            "rangle" => ANGLE_RIGHT,
            _ => 0,
        };
    } else if head.kind() == SexpType::String && length(head) > 0 {
        let text =
            values::string_at(head, 0).map_or_else(|| "NA".to_owned(), |text| text.into_owned());
        code = match text.as_str() {
            "|" | "||" => c_int::from(b'|'),
            "(" => c_int::from(b'('),
            ")" => c_int::from(b')'),
            "[" => c_int::from(b'['),
            "]" => c_int::from(b']'),
            "{" => c_int::from(b'{'),
            "}" => c_int::from(b'}'),
            "" | "." => c_int::from(b'.'),
            _ => 0,
        };
    }
    if code == 0 {
        return Err(error_in(expression, "invalid group delimiter"));
    }
    Ok(code)
}

/// A number as `asChar` formats it, at the `digits` option's precision.
unsafe fn number_text(expression: Sexp) -> String {
    if length(expression) == 0 {
        return "NA".to_owned();
    }
    let settings = Settings::current();
    match expression.kind() {
        SexpType::Integer => match values::ints(expression)[0] {
            NA_INTEGER => "NA".to_owned(),
            value => format_integer(value),
        },
        SexpType::Complex => {
            format_complex_alone(values::complexes(expression)[0], settings.digits)
        }
        _ => {
            let value = values::reals(expression)[0];
            format_real_with(value, format_real_common(&[value], settings), "NA")
        }
    }
}
