//! The native routines of grDevices and graphics: colours, palettes, axis ticks, convex hulls
//! and contour lines, which need no device, and the device and plotting routines.
//!
//! ry has no graphics devices yet, so a session behaves as one in which only the null device
//! exists: `dev.cur()` is device 1, and every routine that would open or draw on a device fails
//! with [`NO_DEVICES`]. The session reports neither cairo nor X11, as a headless build does,
//! which makes the default device `pdf`, so loading the packages succeeds.

use crate::{
    builtin_libraries::{NativeCall, NativeFn},
    coerce::coerce_vector,
    format::c_general,
    unwind::{R, error},
    values::{
        self, alloc, length, nil, scalar_bool, scalar_integer, scalar_string, string_vector,
        symbols,
    },
};
use heap::object::{NA_INTEGER, Sexp, SexpType};
use std::cell::{Cell, RefCell};

pub const GRDEVICES_ROUTINES: &[(&str, NativeFn)] = &[
    ("rgb", do_rgb),
    ("hsv", do_hsv),
    ("hcl", do_hcl),
    ("gray", do_gray),
    ("col2rgb", do_col2rgb),
    ("RGB2hsv", do_rgb2hsv),
    ("colors", do_colors),
    ("palette", do_palette),
    ("palette2", do_palette2),
    ("chull", do_chull),
    ("contourLines", do_contour_lines),
    ("R_CreateAtVector", do_create_at_vector),
    ("R_GAxisPars", do_axis_pars),
    ("cairoProps", do_cairo_props),
    ("cairoVersion", |_| unsafe { scalar_string("") }),
    ("pangoVersion", |_| unsafe { scalar_string("") }),
    ("cairoFT", |_| unsafe { scalar_string("") }),
    ("bmVersion", do_bitmap_versions),
    ("makeQuartzDefault", |_| unsafe { scalar_bool(false) }),
    ("Type1FontInUse", |_| unsafe { scalar_bool(false) }),
    ("CIDFontInUse", |_| unsafe { scalar_bool(false) }),
    ("devcur", |_| unsafe { scalar_integer(NULL_DEVICE) }),
    ("devnext", |_| unsafe { scalar_integer(NULL_DEVICE) }),
    ("devprev", |_| unsafe { scalar_integer(NULL_DEVICE) }),
    ("devset", |_| unsafe { scalar_integer(NULL_DEVICE) }),
    ("devoff", |_| Ok(nil())),
    ("devholdflush", |_| unsafe { scalar_integer(0) }),
    ("devAskNewPage", do_ask_new_page),
    ("getGraphicsEventEnv", |_| Ok(nil())),
    ("PicTeX", no_devices),
    ("PostScript", no_devices),
    ("XFig", no_devices),
    ("PDF", no_devices),
    ("devCairo", no_devices),
    ("Quartz", no_devices),
    ("X11", no_devices),
    ("savePlot", no_devices),
    ("devcap", no_devices),
    ("devcapture", no_devices),
    ("devcontrol", no_devices),
    ("devcopy", no_devices),
    ("devdisplaylist", no_devices),
    ("devsize", no_devices),
    ("devUp", no_devices),
    ("getSnapshot", no_devices),
    ("playSnapshot", no_devices),
    ("getGraphicsEvent", no_devices),
    ("setGraphicsEventEnv", no_devices),
    ("setPattern", no_devices),
    ("setClipPath", no_devices),
    ("setMask", no_devices),
    ("defineGroup", no_devices),
    ("useGroup", no_devices),
];

pub const GRAPHICS_ROUTINES: &[(&str, NativeFn)] = &[
    ("C_BinCount", do_bin_count),
    ("C_StemLeaf", do_stem_leaf),
    // Whether contour labels follow the contour, which every device ry could open does.
    ("C_contourDef", |_| unsafe { scalar_bool(true) }),
    ("RunregisterBase", |_| Ok(nil())),
];

/// The error of every routine that needs a graphics device.
pub const NO_DEVICES: &str = "graphics devices are not supported in ry yet";

const NULL_DEVICE: i32 = 1;

/// Binds `.Device` and `.Devices` in base as GNU R does at startup, for a session whose only
/// device is the null device.
pub unsafe fn define_device_variables() -> R<()> {
    let name = scalar_string("null device")?;
    crate::eval::define(symbols().dot_device, name, values::base_env())?;
    let devices = values::list_vector(&[name])?;
    crate::eval::define(values::install(".Devices")?, devices, values::base_env())?;
    Ok(())
}

/// The routines that draw on or query a device, which are every graphics routine the table
/// above leaves out.
pub unsafe fn no_devices(_: &NativeCall<'_>) -> R<Sexp> {
    Err(error(NO_DEVICES))
}

/// A colour as GNU R packs it: red in the low byte, then green, blue and alpha.
type Colour = u32;

const TRANSPARENT_WHITE: Colour = 0x00FF_FFFF;
const OPAQUE: u32 = 0xFF;
const MAX_PALETTE: usize = 1024;

const DEFAULT_PALETTE: [&str; 8] = [
    "black", "#DF536B", "#61D04F", "#2297E6", "#28E2E5", "#CD0BBC", "#F5C710", "gray62",
];

thread_local! {
    static PALETTE: RefCell<Vec<Colour>> = RefCell::new(default_palette());
    static ASK_NEW_PAGE: Cell<bool> = const { Cell::new(false) };
}

fn default_palette() -> Vec<Colour> {
    DEFAULT_PALETTE
        .iter()
        .filter_map(|name| parse_colour_spec(name).ok())
        .collect()
}

fn rgba(red: u32, green: u32, blue: u32, alpha: u32) -> Colour {
    red | (green << 8) | (blue << 16) | (alpha << 24)
}

fn channels(colour: Colour) -> [u32; 4] {
    [
        colour & 0xFF,
        (colour >> 8) & 0xFF,
        (colour >> 16) & 0xFF,
        colour >> 24,
    ]
}

/// A colour intensity in `[0, 1]` as a byte, rounding to nearest.
fn scale_unit(value: f64) -> u32 {
    (255.0 * value + 0.5) as u32
}

fn hex(colour: Colour, with_alpha: bool) -> String {
    let [red, green, blue, alpha] = channels(colour);
    if with_alpha {
        format!("#{red:02X}{green:02X}{blue:02X}{alpha:02X}")
    } else {
        format!("#{red:02X}{green:02X}{blue:02X}")
    }
}

/// How GNU R names a colour it reports: a colour name for an opaque colour that has one,
/// `transparent` for full transparency, and hex digits otherwise.
fn colour_name(colour: Colour) -> String {
    let [.., alpha] = channels(colour);
    match alpha {
        OPAQUE => COLOUR_NAMES
            .iter()
            .find(|(_, value)| *value == colour & 0x00FF_FFFF)
            .map_or_else(|| hex(colour, false), |(name, _)| (*name).to_owned()),
        0 => "transparent".to_owned(),
        _ => hex(colour, true),
    }
}

/// A colour given as hex digits or a name, without palette indices.
fn parse_colour_spec(text: &str) -> Result<Colour, String> {
    if let Some(digits) = text.strip_prefix('#') {
        let valid =
            matches!(digits.len(), 6 | 8) && digits.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !valid {
            return Err("invalid RGB specification".to_owned());
        }
        let byte = |index: usize| u32::from_str_radix(&digits[index..index + 2], 16).unwrap_or(0);
        let alpha = if digits.len() == 8 { byte(6) } else { OPAQUE };
        return Ok(rgba(byte(0), byte(2), byte(4), alpha));
    }
    if text == "NA" || text.eq_ignore_ascii_case("transparent") {
        return Ok(TRANSPARENT_WHITE);
    }
    // Names match ignoring case and spaces, so "Light Goldenrod 2" is lightgoldenrod2.
    let wanted: String = text
        .chars()
        .filter(|character| *character != ' ')
        .collect::<String>()
        .to_ascii_lowercase();
    COLOUR_NAMES
        .iter()
        .find(|(name, _)| *name == wanted)
        .map(|(_, value)| value | (OPAQUE << 24))
        .ok_or_else(|| format!("invalid color name '{text}'"))
}

/// A colour as `col2rgb` and the plotting functions accept it: a specification, or a palette
/// index written in digits, where 0 is the background.
fn parse_colour(text: &str) -> Result<Colour, String> {
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        let index: usize = text
            .parse()
            .map_err(|_| format!("invalid color specification \"{text}\""))?;
        return Ok(palette_colour(index));
    }
    parse_colour_spec(text)
}

fn palette_colour(index: usize) -> Colour {
    if index == 0 {
        return TRANSPARENT_WHITE;
    }
    PALETTE.with(|palette| {
        let palette = palette.borrow();
        palette
            .get((index - 1) % palette.len().max(1))
            .copied()
            .unwrap_or(TRANSPARENT_WHITE)
    })
}

/// `rgb(red, green, blue, alpha, maxColorValue, names)`.
unsafe fn do_rgb(call: &NativeCall<'_>) -> R<Sexp> {
    let max_value = values::as_real_scalar(call.arg(4));
    let alpha = call.arg(3);
    let has_alpha = alpha != nil();
    let components = if has_alpha { 4 } else { 3 };
    let integer = max_value == 255.0;
    let mut vectors = Vec::with_capacity(components);
    for index in 0..components {
        let kind = if integer {
            SexpType::Integer
        } else {
            SexpType::Real
        };
        vectors.push(coerce_vector(call.arg(index), kind)?);
    }
    let lengths: Vec<usize> = vectors.iter().map(|vector| length(*vector)).collect();
    let count = if lengths.contains(&0) {
        0
    } else {
        lengths.iter().copied().max().unwrap_or(0)
    };
    let mut colours = Vec::with_capacity(count);
    for index in 0..count {
        let mut bytes = [0u32, 0, 0, OPAQUE];
        for (component, vector) in vectors.iter().enumerate() {
            let what = if component == 3 {
                "alpha level"
            } else {
                "color intensity"
            };
            let position = index % lengths[component];
            bytes[component] = if integer {
                let value = values::ints(*vector)[position];
                if !(0..=255).contains(&value) {
                    let shown = if value == NA_INTEGER {
                        "NA".to_owned()
                    } else {
                        value.to_string()
                    };
                    return Err(error(format!("{what} {shown}, not in 0:255")));
                }
                value as u32
            } else {
                let mut value = values::reals(*vector)[position];
                if max_value != 1.0 {
                    value /= max_value;
                }
                if !(0.0..=1.0).contains(&value) {
                    return Err(error(format!("{what} {}, not in [0,1]", general(value))));
                }
                scale_unit(value)
            };
        }
        colours.push(hex(rgba(bytes[0], bytes[1], bytes[2], bytes[3]), has_alpha));
    }
    let result = string_vector(&colours)?;
    let names = call.arg(5);
    if names != nil() {
        let names = coerce_vector(names, SexpType::String)?;
        if length(names) != count {
            return Err(error("invalid 'names' vector"));
        }
        values::set_attr(result, symbols().names, names)?;
    }
    Ok(result)
}

/// C's `%g` of a value in an error message, with R's spelling of the special values.
fn general(value: f64) -> String {
    if values::is_na(value) {
        "NA".to_owned()
    } else if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 {
            "Inf".to_owned()
        } else {
            "-Inf".to_owned()
        }
    } else {
        c_general(value, 6, false)
    }
}

/// The real vectors of `arguments`, recycled to the longest, or `None` when one is empty.
unsafe fn real_arguments(call: &NativeCall<'_>, arguments: &[usize]) -> R<(Vec<Sexp>, usize)> {
    let mut vectors = Vec::with_capacity(arguments.len());
    for &index in arguments {
        vectors.push(coerce_vector(call.arg(index), SexpType::Real)?);
    }
    let lengths: Vec<usize> = vectors.iter().map(|vector| length(*vector)).collect();
    let count = if lengths.contains(&0) {
        0
    } else {
        lengths.iter().copied().max().unwrap_or(0)
    };
    Ok((vectors, count))
}

unsafe fn recycled(vector: Sexp, index: usize) -> f64 {
    let values = values::reals(vector);
    values[index % values.len()]
}

/// `hsv(h, s, v, alpha)`.
unsafe fn do_hsv(call: &NativeCall<'_>) -> R<Sexp> {
    let has_alpha = call.arg(3) != nil();
    let arguments: &[usize] = if has_alpha { &[0, 1, 2, 3] } else { &[0, 1, 2] };
    let (vectors, count) = real_arguments(call, arguments)?;
    let mut colours = Vec::with_capacity(count);
    for index in 0..count {
        let components: Vec<f64> = vectors
            .iter()
            .map(|vector| recycled(*vector, index))
            .collect();
        if components.iter().any(|value| !(0.0..=1.0).contains(value)) {
            return Err(error("invalid hsv color"));
        }
        let [red, green, blue] = hsv_to_rgb(components[0], components[1], components[2]);
        let alpha = if has_alpha {
            scale_unit(components[3])
        } else {
            OPAQUE
        };
        let colour = rgba(scale_unit(red), scale_unit(green), scale_unit(blue), alpha);
        colours.push(hex(colour, has_alpha));
    }
    string_vector(&colours)
}

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> [f64; 3] {
    if saturation == 0.0 {
        return [value; 3];
    }
    let sector = 6.0 * (hue % 1.0);
    let whole = sector.floor();
    let fraction = sector - whole;
    let low = value * (1.0 - saturation);
    let falling = value * (1.0 - saturation * fraction);
    let rising = value * (1.0 - saturation * (1.0 - fraction));
    match whole as i32 {
        0 => [value, rising, low],
        1 => [falling, value, low],
        2 => [low, value, rising],
        3 => [low, falling, value],
        4 => [rising, low, value],
        _ => [value, low, falling],
    }
}

/// `hcl(h, c, l, alpha, fixup)`: polar CIE-LUV under the D65 white point, converted to sRGB.
unsafe fn do_hcl(call: &NativeCall<'_>) -> R<Sexp> {
    let has_alpha = call.arg(3) != nil();
    let fixup = values::as_logical_scalar(call.arg(4)) == 1;
    let arguments: &[usize] = if has_alpha { &[0, 1, 2, 3] } else { &[0, 1, 2] };
    let (vectors, count) = real_arguments(call, arguments)?;
    let mut colours = Vec::with_capacity(count);
    for index in 0..count {
        let components: Vec<f64> = vectors
            .iter()
            .map(|vector| recycled(*vector, index))
            .collect();
        let (hue, chroma, luminance) = (components[0], components[1], components[2]);
        if !hue.is_finite() || !chroma.is_finite() || !luminance.is_finite() {
            colours.push(None);
            continue;
        }
        if !(0.0..=100.0).contains(&luminance) || chroma < 0.0 {
            return Err(error("invalid hcl color"));
        }
        let mut rgb = hcl_to_rgb(hue, chroma, luminance);
        if fixup {
            for channel in &mut rgb {
                *channel = channel.clamp(0.0, 1.0);
            }
        } else if rgb.iter().any(|channel| !(0.0..=1.0).contains(channel)) {
            colours.push(None);
            continue;
        }
        let alpha = if has_alpha {
            let alpha = components[3];
            if !(0.0..=1.0).contains(&alpha) {
                return Err(error(format!(
                    "alpha level {}, not in [0,1]",
                    general(alpha)
                )));
            }
            scale_unit(alpha)
        } else {
            OPAQUE
        };
        let colour = rgba(
            scale_unit(rgb[0]),
            scale_unit(rgb[1]),
            scale_unit(rgb[2]),
            alpha,
        );
        colours.push(Some(hex(colour, has_alpha)));
    }
    values::string_vector_na(&colours)
}

const WHITE_X: f64 = 95.047;
const WHITE_Y: f64 = 100.000;
const WHITE_Z: f64 = 108.883;

fn hcl_to_rgb(hue: f64, chroma: f64, luminance: f64) -> [f64; 3] {
    if luminance <= 0.0 && chroma == 0.0 {
        return [0.0; 3];
    }
    let radians = hue.to_radians();
    let u = chroma * radians.cos();
    let v = chroma * radians.sin();
    let denominator = WHITE_X + 15.0 * WHITE_Y + 3.0 * WHITE_Z;
    let white_u = 4.0 * WHITE_X / denominator;
    let white_v = 9.0 * WHITE_Y / denominator;
    // CIE's lightness inverse: cubic above the knee at L = 8, linear below it.
    let y = WHITE_Y
        * if luminance > 7.999592 {
            ((luminance + 16.0) / 116.0).powi(3)
        } else {
            luminance / 903.3
        };
    let u_prime = u / (13.0 * luminance) + white_u;
    let v_prime = v / (13.0 * luminance) + white_v;
    let x = 9.0 * y * u_prime / (4.0 * v_prime);
    let z = -x / 3.0 - 5.0 * y + 3.0 * y / v_prime;
    let gamma = |linear: f64| {
        if linear > 0.00304 {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        } else {
            12.92 * linear
        }
    };
    [
        gamma((3.240479 * x - 1.537150 * y - 0.498535 * z) / WHITE_Y),
        gamma((-0.969256 * x + 1.875992 * y + 0.041556 * z) / WHITE_Y),
        gamma((0.055648 * x - 0.204043 * y + 1.057311 * z) / WHITE_Y),
    ]
}

/// `gray(level, alpha)`.
unsafe fn do_gray(call: &NativeCall<'_>) -> R<Sexp> {
    let levels = coerce_vector(call.arg(0), SexpType::Real)?;
    let alpha = call.arg(1);
    let alpha = if alpha == nil() {
        None
    } else {
        Some(coerce_vector(alpha, SexpType::Real)?)
    };
    let count = length(levels);
    let mut colours = Vec::with_capacity(count);
    for index in 0..count {
        let level = values::reals(levels)[index];
        if !(0.0..=1.0).contains(&level) {
            return Err(error("invalid gray level, must be in [0,1]."));
        }
        let grey = scale_unit(level);
        let colour = match alpha {
            Some(alpha) if length(alpha) > 0 => {
                let alpha = recycled(alpha, index);
                if !(0.0..=1.0).contains(&alpha) {
                    return Err(error(format!(
                        "alpha level {}, not in [0,1]",
                        general(alpha)
                    )));
                }
                hex(rgba(grey, grey, grey, scale_unit(alpha)), true)
            }
            _ => hex(rgba(grey, grey, grey, OPAQUE), false),
        };
        colours.push(colour);
    }
    string_vector(&colours)
}

/// `col2rgb(col, alpha)`: an integer matrix with a column per colour.
unsafe fn do_col2rgb(call: &NativeCall<'_>) -> R<Sexp> {
    let colours = call.arg(0);
    let with_alpha = values::as_logical_scalar(call.arg(1)) == 1;
    let rows = if with_alpha { 4 } else { 3 };
    let count = length(colours);
    let mut parsed = Vec::with_capacity(count);
    for index in 0..count {
        parsed.push(colour_element(colours, index)?);
    }
    let result = alloc(SexpType::Integer, rows * count)?;
    let cells = values::ints_mut(result);
    for (column, colour) in parsed.iter().enumerate() {
        for (row, channel) in channels(*colour).iter().take(rows).enumerate() {
            cells[column * rows + row] = *channel as i32;
        }
    }
    let dim = values::integer_vector(&[rows as i32, count as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    let row_names = string_vector(&["red", "green", "blue", "alpha"][..rows])?;
    let column_names = values::names(colours);
    let dimnames = values::list_vector(&[row_names, column_names])?;
    values::set_attr(result, symbols().dimnames, dimnames)?;
    Ok(result)
}

/// Element `index` of a colour vector: strings as specifications or palette indices, numbers
/// as palette indices.
unsafe fn colour_element(colours: Sexp, index: usize) -> R<Colour> {
    match colours.kind() {
        SexpType::String => match values::string_at(colours, index) {
            None => Ok(TRANSPARENT_WHITE),
            Some(text) => parse_colour(&text).map_err(error),
        },
        SexpType::Integer | SexpType::Logical | SexpType::Real => {
            let value = crate::subset::numeric_element(colours, index);
            if value.is_nan() {
                return Ok(TRANSPARENT_WHITE);
            }
            if value < 0.0 {
                return Err(error(format!(
                    "numerical color values must be >= 0, found {}",
                    value as i64
                )));
            }
            Ok(palette_colour(value as usize))
        }
        _ => Err(error("invalid color specification")),
    }
}

/// `RGB2hsv(rgb)`: a 3-row matrix of intensities in `[0, 1]` to hue, saturation and value.
unsafe fn do_rgb2hsv(call: &NativeCall<'_>) -> R<Sexp> {
    let matrix = coerce_vector(call.arg(0), SexpType::Real)?;
    let count = length(matrix) / 3;
    let result = alloc(SexpType::Real, 3 * count)?;
    let input = values::reals(matrix);
    let output = values::reals_mut(result);
    for column in 0..count {
        let [red, green, blue] = [
            input[3 * column],
            input[3 * column + 1],
            input[3 * column + 2],
        ];
        let max = red.max(green).max(blue);
        let min = red.min(green).min(blue);
        let delta = max - min;
        let (mut hue, saturation) = if max == 0.0 || delta == 0.0 {
            (0.0, if max == 0.0 { 0.0 } else { delta / max })
        } else {
            let sector = if red == max {
                (green - blue) / delta
            } else if green == max {
                2.0 + (blue - red) / delta
            } else {
                4.0 + (red - green) / delta
            };
            (sector / 6.0, delta / max)
        };
        if hue < 0.0 {
            hue += 1.0;
        }
        output[3 * column..3 * column + 3].copy_from_slice(&[hue, saturation, max]);
    }
    let dim = values::integer_vector(&[3, count as i32])?;
    values::set_attr(result, symbols().dim, dim)?;
    let input_dimnames = values::get_attr(call.arg(0), symbols().dimnames);
    let column_names = if input_dimnames.kind() == SexpType::List && length(input_dimnames) == 2 {
        input_dimnames.element(1)
    } else {
        nil()
    };
    let row_names = string_vector(&["h", "s", "v"])?;
    let dimnames = values::list_vector(&[row_names, column_names])?;
    values::set_attr(result, symbols().dimnames, dimnames)?;
    Ok(result)
}

unsafe fn do_colors(_: &NativeCall<'_>) -> R<Sexp> {
    let names: Vec<&str> = COLOUR_NAMES.iter().map(|(name, _)| *name).collect();
    string_vector(&names)
}

/// `palette(value)`: the current palette by name, after replacing it with `value` unless that
/// is empty.
unsafe fn do_palette(call: &NativeCall<'_>) -> R<Sexp> {
    let value = call.arg(0);
    if value.kind() != SexpType::String {
        return Err(error("invalid argument type"));
    }
    let current: Vec<String> = PALETTE.with(|palette| {
        palette
            .borrow()
            .iter()
            .map(|colour| colour_name(*colour))
            .collect()
    });
    let old = string_vector(&current)?;
    let count = length(value);
    if count == 1 {
        if values::string_at(value, 0).as_deref() != Some("default") {
            return Err(error("unknown palette (need >= 2 colors)"));
        }
        PALETTE.with(|palette| *palette.borrow_mut() = default_palette());
    } else if count > 1 {
        if count > MAX_PALETTE {
            return Err(error(format!("maximum number of colors is {MAX_PALETTE}")));
        }
        let mut colours = Vec::with_capacity(count);
        for index in 0..count {
            let text = values::string_at(value, index)
                .map_or_else(|| "NA".to_owned(), |text| text.into_owned());
            colours.push(parse_colour_spec(&text).map_err(error)?);
        }
        PALETTE.with(|palette| *palette.borrow_mut() = colours);
    }
    Ok(old)
}

/// `palette2(value)`: the palette as packed integers, after replacing it with `value` unless
/// that is `NULL`; the display list saves and restores palettes this way.
unsafe fn do_palette2(call: &NativeCall<'_>) -> R<Sexp> {
    let current: Vec<i32> = PALETTE.with(|palette| {
        palette
            .borrow()
            .iter()
            .map(|colour| *colour as i32)
            .collect()
    });
    let old = values::integer_vector(&current)?;
    let value = call.arg(0);
    if value != nil() {
        if value.kind() != SexpType::Integer {
            return Err(error("requires INTSXP argument"));
        }
        if length(value) > MAX_PALETTE {
            return Err(error(format!("maximum number of colors is {MAX_PALETTE}")));
        }
        let colours: Vec<Colour> = values::ints(value)
            .iter()
            .map(|colour| *colour as u32)
            .collect();
        PALETTE.with(|palette| *palette.borrow_mut() = colours);
    }
    Ok(old)
}

unsafe fn do_cairo_props(call: &NativeCall<'_>) -> R<Sexp> {
    match values::as_integer_scalar(call.arg(0)) {
        1 | 2 => scalar_bool(false),
        _ => Ok(nil()),
    }
}

unsafe fn do_bitmap_versions(_: &NativeCall<'_>) -> R<Sexp> {
    let result = string_vector(&["", "", ""])?;
    let names = string_vector(&["libpng", "jpeg", "libtiff"])?;
    values::set_attr(result, symbols().names, names)?;
    Ok(result)
}

/// `devAskNewPage(ask)`: the setting before the call, which only matters once a device exists.
unsafe fn do_ask_new_page(call: &NativeCall<'_>) -> R<Sexp> {
    let old = ASK_NEW_PAGE.with(Cell::get);
    let ask = call.arg(0);
    if ask != nil() {
        let value = values::as_logical_scalar(ask);
        if value == NA_INTEGER {
            return Err(error("invalid 'ask' argument"));
        }
        ASK_NEW_PAGE.with(|setting| setting.set(value == 1));
        crate::interp().visible = false;
    }
    scalar_bool(old)
}

/// `chull(x)`: the indices of the points of an n-by-2 matrix on its convex hull, without points
/// that lie on an edge between two others and keeping the first of duplicated points.
unsafe fn do_chull(call: &NativeCall<'_>) -> R<Sexp> {
    let matrix = coerce_vector(call.arg(0), SexpType::Real)?;
    let count = length(matrix) / 2;
    let coordinates = values::reals(matrix);
    let point = |index: usize| (coordinates[index], coordinates[count + index]);
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|&left, &right| {
        let (left_point, right_point) = (point(left), point(right));
        left_point
            .0
            .total_cmp(&right_point.0)
            .then(left_point.1.total_cmp(&right_point.1))
            .then(left.cmp(&right))
    });
    order.dedup_by(|later, earlier| point(*later) == point(*earlier));
    if order.len() < 3 {
        let indices: Vec<i32> = order.iter().map(|index| *index as i32 + 1).collect();
        return values::integer_vector(&indices);
    }
    let cross = |origin: usize, first: usize, second: usize| {
        let (origin, first, second) = (point(origin), point(first), point(second));
        (first.0 - origin.0) * (second.1 - origin.1) - (first.1 - origin.1) * (second.0 - origin.0)
    };
    let mut hull: Vec<usize> = Vec::with_capacity(2 * order.len());
    for pass in 0..2 {
        let start = hull.len();
        let sequence: Box<dyn Iterator<Item = &usize>> = if pass == 0 {
            Box::new(order.iter())
        } else {
            Box::new(order.iter().rev())
        };
        for &index in sequence {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], index) <= 0.0
            {
                hull.pop();
            }
            hull.push(index);
        }
        hull.pop();
    }
    let indices: Vec<i32> = hull.iter().map(|index| *index as i32 + 1).collect();
    values::integer_vector(&indices)
}

/// `R_GAxisPars(usr, log, nintLog)`: `list(axp = c(first, last), n = intervals)` for the ticks
/// an axis spanning `usr` gets. On a log axis `usr` is in decades and `n` is the tick code of
/// `par("xaxp")`: 1 for powers of ten, 2 for 1 and 5 times them, 3 for 1, 2 and 5 times them.
unsafe fn do_axis_pars(call: &NativeCall<'_>) -> R<Sexp> {
    let usr = coerce_vector(call.arg(0), SexpType::Real)?;
    if length(usr) != 2 {
        return Err(error("'usr' must be numeric of length 2"));
    }
    let log = values::as_logical_scalar(call.arg(1)) == 1;
    let intervals = values::as_integer_scalar(call.arg(2));
    let [mut low, mut high] = [values::reals(usr)[0], values::reals(usr)[1]];
    if !low.is_finite() || !high.is_finite() {
        return Err(error("'usr' must be finite"));
    }
    if high < low {
        std::mem::swap(&mut low, &mut high);
    }
    let (first, last, count) = if log {
        log_axis(10f64.powf(low), 10f64.powf(high), intervals)
    } else {
        linear_axis(low, high, intervals)
    };
    let axp = values::real_vector(&[first, last])?;
    values::named_list(&[("axp", axp), ("n", scalar_integer(count)?)])
}

/// The ticks of a linear axis: the pretty values inside `[low, high]`.
fn linear_axis(low: f64, high: f64, intervals: i32) -> (f64, f64, i32) {
    let pretty = crate::arith::pretty(low, high, intervals, 1, 0.25, [0.8, 1.7], 2, false);
    let unit = pretty.unit;
    let mut first = pretty.low;
    let mut last = pretty.high;
    let mut count = pretty.count;
    if last >= first + 1.0 {
        let tolerance = 1e-10 * unit;
        if first * unit < low - tolerance {
            first += 1.0;
        }
        if last > first + 1.0 && last * unit > high + tolerance {
            last -= 1.0;
        }
        count = (last - first) as i32;
    }
    (first * unit, last * unit, count)
}

/// The ticks of a log axis spanning `[low, high]` (not logged): decades with a tick code, or a
/// linear axis with a negative count when the range is too short for decades.
fn log_axis(low: f64, high: f64, intervals: i32) -> (f64, f64, i32) {
    let (log_low, log_high) = (low.log10(), high.log10());
    let mut first = log_low.ceil();
    let mut last = log_high.floor();
    if last <= first && log_high - log_low > 1.0 {
        first = (log_low - 0.5).ceil();
        last = (log_high + 0.5).floor();
    }
    if last <= first {
        let (first, last, count) = linear_axis(low, high, intervals);
        return (first, last, -count);
    }
    let decades = last - first;
    let code = if decades <= 2.0 {
        3
    } else if decades <= 3.0 {
        2
    } else {
        1
    };
    (10f64.powf(first), 10f64.powf(last), code)
}

/// `R_CreateAtVector(axp, usr, nint, log)`: the tick positions for `par("xaxp")`-style `axp`
/// on an axis spanning `usr` (not logged).
unsafe fn do_create_at_vector(call: &NativeCall<'_>) -> R<Sexp> {
    let axp = coerce_vector(call.arg(0), SexpType::Real)?;
    let usr = coerce_vector(call.arg(1), SexpType::Real)?;
    if length(axp) != 3 {
        return Err(error("'axp' must be numeric of length 3"));
    }
    if length(usr) != 2 {
        return Err(error("'usr' must be numeric of length 2"));
    }
    let intervals = values::as_integer_scalar(call.arg(2));
    let log = values::as_logical_scalar(call.arg(3)) == 1;
    let axp = values::reals(axp);
    let usr = values::reals(usr);
    let ticks = at_vector([axp[0], axp[1], axp[2]], [usr[0], usr[1]], intervals, log);
    values::real_vector(&ticks)
}

fn at_vector(axp: [f64; 3], usr: [f64; 2], intervals: i32, log: bool) -> Vec<f64> {
    let [first, last, code] = axp;
    let count = code.round() as i32;
    if !log || count < 0 {
        let count = count.unsigned_abs().max(1) as usize;
        let step = (last - first) / count as f64;
        return (0..=count)
            .map(|index| {
                let tick = first + index as f64 * step;
                // A tick that should be zero but is off by rounding is shown as zero.
                if tick.abs() < 1e-10 * step.abs() {
                    0.0
                } else {
                    tick
                }
            })
            .collect();
    }
    let (low, high) = (usr[0].min(usr[1]), usr[0].max(usr[1]));
    let inside = |tick: f64| tick >= low * (1.0 - 1e-7) && tick <= high * (1.0 + 1e-7);
    let multiples: &[f64] = match count {
        2 => &[1.0, 5.0],
        3 => &[1.0, 2.0, 5.0],
        _ => &[1.0],
    };
    let mut ticks = Vec::new();
    if count == 1 {
        let decades = (last / first).log10().round().max(0.0) as i32;
        let intervals = intervals.max(1);
        // Too many decades for the wanted number of intervals: every few decades only. GNU R
        // also skips the first decades when the count is well past a multiple of the
        // intervals, which only happens for more than five intervals.
        let stride = (decades / intervals).max(1);
        let excess = decades % intervals;
        let mut exponent = if decades >= intervals && excess >= 5 {
            (excess - 2) / 2 + 1
        } else {
            0
        };
        while exponent <= decades {
            let tick = first * 10f64.powi(exponent);
            if inside(tick) {
                ticks.push(tick);
            }
            exponent += stride;
        }
        return ticks;
    }
    let mut decade = 10f64.powf(first.log10().floor() - 1.0);
    while decade <= high * 10.0 {
        for multiple in multiples {
            let tick = decade * multiple;
            if inside(tick) {
                ticks.push(tick);
            }
        }
        decade *= 10.0;
    }
    ticks
}

/// `contourLines(x, y, z, levels)`: the contour lines of `z` over the grid `x` by `y`, as a
/// list with `level`, `x` and `y` per line.
unsafe fn do_contour_lines(call: &NativeCall<'_>) -> R<Sexp> {
    let x = coerce_vector(call.arg(0), SexpType::Real)?;
    let y = coerce_vector(call.arg(1), SexpType::Real)?;
    let z = coerce_vector(call.arg(2), SexpType::Real)?;
    let levels = coerce_vector(call.arg(3), SexpType::Real)?;
    let (x, y, z) = (
        values::reals(x).to_vec(),
        values::reals(y).to_vec(),
        values::reals(z).to_vec(),
    );
    if x.len() < 2 || y.len() < 2 {
        return Err(error("insufficient 'x' or 'y' values"));
    }
    if z.len() != x.len() * y.len() {
        return Err(error("dimension mismatch"));
    }
    let grid = Grid {
        x: &x,
        y: &y,
        z: &z,
    };
    let mut lines = Vec::new();
    for &level in values::reals(levels) {
        for line in contour_level(&grid, level) {
            let (xs, ys): (Vec<f64>, Vec<f64>) = line.into_iter().unzip();
            let entry = values::named_list(&[
                ("level", values::scalar_real(level)?),
                ("x", values::real_vector(&xs)?),
                ("y", values::real_vector(&ys)?),
            ])?;
            lines.push(entry);
        }
    }
    values::list_vector(&lines)
}

struct Grid<'a> {
    x: &'a [f64],
    y: &'a [f64],
    z: &'a [f64],
}

impl Grid<'_> {
    fn value(&self, column: usize, row: usize) -> f64 {
        self.z[column + row * self.x.len()]
    }
}

/// How an edge runs from its grid point: along x, along y, or across its cell from the
/// lower-left corner (`Rising`) or from the lower-right corner (`Falling`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EdgeKind {
    AlongX,
    AlongY,
    Rising,
    Falling,
}

/// A grid edge: the grid point it is anchored at (the lower-left corner of its cell for a
/// diagonal) and how it runs.
type Edge = (usize, usize, EdgeKind);

/// The lines of one level, in the order and direction GNU R gives them.
///
/// A grid value equal to the level counts as a thousandth of the value range above it. Each
/// grid cell the level passes through holds one segment, from the first of its crossed edges
/// to the second, taking them as bottom, left, right, top. A saddle holds two: top to right
/// and left to bottom, or bottom to right and left to top when the saddle point of the
/// bilinear surface through the corners lies strictly on the same side of the level as the
/// lower-left corner. A cell with one missing corner is
/// the triangle of the other three, whose edges count in the order vertical, horizontal,
/// diagonal; a cell with more is skipped.
///
/// A line starts at the first unused segment, taking cells by x and then by y, and follows the
/// neighbouring cells' segments forwards and then backwards until it reaches the boundary or
/// closes, in which case its first point is repeated at the end.
fn contour_level(grid: &Grid<'_>, level: f64) -> Vec<Vec<(f64, f64)>> {
    let columns = grid.x.len();
    let rows = grid.y.len();
    let cells_x = columns - 1;
    let cells_y = rows - 1;
    let (low, high) = grid
        .z
        .iter()
        .filter(|value| value.is_finite())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), value| {
            (low.min(*value), high.max(*value))
        });
    let nudge = 1e-3 * (high - low);
    let value = |column: usize, row: usize| {
        let value = grid.value(column, row);
        if value == level { value + nudge } else { value }
    };
    let ends = |edge: Edge| -> [(usize, usize); 2] {
        let (column, row, kind) = edge;
        match kind {
            EdgeKind::AlongX => [(column, row), (column + 1, row)],
            EdgeKind::AlongY => [(column, row), (column, row + 1)],
            EdgeKind::Rising => [(column, row), (column + 1, row + 1)],
            EdgeKind::Falling => [(column + 1, row), (column, row + 1)],
        }
    };
    let crossing = |edge: Edge| -> Option<(f64, f64)> {
        let [(column, row), (column2, row2)] = ends(edge);
        let (first, second) = (value(column, row), value(column2, row2));
        if !first.is_finite() || !second.is_finite() || (first >= level) == (second >= level) {
            return None;
        }
        let fraction = (level - first) / (second - first);
        Some((
            grid.x[column] + fraction * (grid.x[column2] - grid.x[column]),
            grid.y[row] + fraction * (grid.y[row2] - grid.y[row]),
        ))
    };
    let mut segments: Vec<Vec<[Edge; 2]>> = vec![Vec::new(); cells_x * cells_y];
    for column in 0..cells_x {
        for row in 0..cells_y {
            let corners = [
                value(column, row),
                value(column + 1, row),
                value(column, row + 1),
                value(column + 1, row + 1),
            ];
            let bottom = (column, row, EdgeKind::AlongX);
            let left = (column, row, EdgeKind::AlongY);
            let right = (column + 1, row, EdgeKind::AlongY);
            let top = (column, row + 1, EdgeKind::AlongX);
            let rising = (column, row, EdgeKind::Rising);
            let falling = (column, row, EdgeKind::Falling);
            let missing: Vec<usize> = (0..4)
                .filter(|corner| !corners[*corner].is_finite())
                .collect();
            let candidates: Vec<Edge> = match missing.as_slice() {
                [] => vec![bottom, left, right, top],
                [0] => vec![right, top, falling],
                [1] => vec![left, top, rising],
                [2] => vec![right, bottom, rising],
                [3] => vec![left, bottom, falling],
                _ => continue,
            };
            let crossed: Vec<Edge> = candidates
                .into_iter()
                .filter(|edge| crossing(*edge).is_some())
                .collect();
            let cell = &mut segments[column * cells_y + row];
            match crossed.len() {
                2 => cell.push([crossed[0], crossed[1]]),
                4 => {
                    // The value at the saddle point of the bilinear surface through the corners,
                    // relative to the level.
                    let [lower_left, lower_right, upper_left, upper_right] =
                        corners.map(|corner| corner - level);
                    let saddle = (lower_left * upper_right - lower_right * upper_left)
                        / (lower_left + upper_right - lower_right - upper_left);
                    let saddle_with_lower_left = if lower_left > 0.0 {
                        saddle > 0.0
                    } else {
                        saddle < 0.0
                    };
                    if saddle_with_lower_left {
                        cell.push([bottom, right]);
                        cell.push([left, top]);
                    } else {
                        cell.push([top, right]);
                        cell.push([left, bottom]);
                    }
                }
                _ => {}
            }
        }
    }
    let cells_of = |edge: Edge| -> [Option<(usize, usize)>; 2] {
        let (column, row, kind) = edge;
        match kind {
            EdgeKind::AlongX => [
                (row > 0 && column < cells_x).then(|| (column, row - 1)),
                (row < cells_y && column < cells_x).then_some((column, row)),
            ],
            EdgeKind::AlongY => [
                (column > 0 && row < cells_y).then(|| (column - 1, row)),
                (column < cells_x && row < cells_y).then_some((column, row)),
            ],
            EdgeKind::Rising | EdgeKind::Falling => [Some((column, row)), None],
        }
    };
    // Takes the unused segment containing `edge` in the cell on the other side of it from
    // `cell`, returning that cell and the segment's other edge.
    let take_next = |segments: &mut Vec<Vec<[Edge; 2]>>,
                     cell: (usize, usize),
                     edge: Edge|
     -> Option<((usize, usize), Edge)> {
        let next_cell = cells_of(edge)
            .into_iter()
            .flatten()
            .find(|other| *other != cell)?;
        let list = &mut segments[next_cell.0 * cells_y + next_cell.1];
        let position = list.iter().position(|segment| segment.contains(&edge))?;
        let segment = list.remove(position);
        let other = if segment[0] == edge {
            segment[1]
        } else {
            segment[0]
        };
        Some((next_cell, other))
    };
    let mut lines = Vec::new();
    for column in 0..cells_x {
        for row in 0..cells_y {
            let cell = (column, row);
            while !segments[column * cells_y + row].is_empty() {
                let [start, end] = segments[column * cells_y + row].remove(0);
                let mut forward = vec![start, end];
                let mut current = (cell, end);
                let mut closed = false;
                while let Some((next_cell, next_edge)) =
                    take_next(&mut segments, current.0, current.1)
                {
                    forward.push(next_edge);
                    if next_edge == start {
                        closed = true;
                        break;
                    }
                    current = (next_cell, next_edge);
                }
                let mut edges = Vec::new();
                if !closed {
                    let mut current = (cell, start);
                    while let Some((next_cell, next_edge)) =
                        take_next(&mut segments, current.0, current.1)
                    {
                        edges.push(next_edge);
                        current = (next_cell, next_edge);
                    }
                    edges.reverse();
                }
                edges.extend(forward);
                lines.push(edges.into_iter().filter_map(crossing).collect());
            }
        }
    }
    lines
}

/// `C_BinCount(x, breaks, right, include.border)`: how many values fall in each interval of
/// `breaks`. A value on an inner break counts in the interval below it when `right` and in the
/// one above otherwise; a value on the first break always counts, and one on the last break
/// only with `include.border`. `hist` widens the breaks by a fuzz first, which is how its
/// `include.lowest` reaches the open end.
unsafe fn do_bin_count(call: &NativeCall<'_>) -> R<Sexp> {
    let x = coerce_vector(call.arg(0), SexpType::Real)?;
    let breaks = coerce_vector(call.arg(1), SexpType::Real)?;
    let right = values::as_logical_scalar(call.arg(2)) == 1;
    let include_border = values::as_logical_scalar(call.arg(3)) == 1;
    let breaks = values::reals(breaks);
    let bins = breaks.len().saturating_sub(1);
    let mut counts = vec![0i32; bins];
    for &value in values::reals(x) {
        if !value.is_finite() || bins == 0 || value < breaks[0] || value > breaks[bins] {
            continue;
        }
        if value == breaks[bins] && !include_border {
            continue;
        }
        // The number of inner breaks below the value, or at it unless intervals close on the
        // right.
        let inner = &breaks[1..bins];
        let bin = if right {
            inner.partition_point(|brk| *brk < value)
        } else {
            inner.partition_point(|brk| *brk <= value)
        };
        counts[bin] += 1;
    }
    values::integer_vector(&counts)
}

unsafe fn do_stem_leaf(_: &NativeCall<'_>) -> R<Sexp> {
    Err(error("stem() is not supported in ry yet"))
}

/// GNU R's colour names and their values (red in the low byte), in the order `colors()`
/// lists them.
const COLOUR_NAMES: &[(&str, u32)] = &[
    ("white", 0xFFFFFF),
    ("aliceblue", 0xFFF8F0),
    ("antiquewhite", 0xD7EBFA),
    ("antiquewhite1", 0xDBEFFF),
    ("antiquewhite2", 0xCCDFEE),
    ("antiquewhite3", 0xB0C0CD),
    ("antiquewhite4", 0x78838B),
    ("aquamarine", 0xD4FF7F),
    ("aquamarine1", 0xD4FF7F),
    ("aquamarine2", 0xC6EE76),
    ("aquamarine3", 0xAACD66),
    ("aquamarine4", 0x748B45),
    ("azure", 0xFFFFF0),
    ("azure1", 0xFFFFF0),
    ("azure2", 0xEEEEE0),
    ("azure3", 0xCDCDC1),
    ("azure4", 0x8B8B83),
    ("beige", 0xDCF5F5),
    ("bisque", 0xC4E4FF),
    ("bisque1", 0xC4E4FF),
    ("bisque2", 0xB7D5EE),
    ("bisque3", 0x9EB7CD),
    ("bisque4", 0x6B7D8B),
    ("black", 0x000000),
    ("blanchedalmond", 0xCDEBFF),
    ("blue", 0xFF0000),
    ("blue1", 0xFF0000),
    ("blue2", 0xEE0000),
    ("blue3", 0xCD0000),
    ("blue4", 0x8B0000),
    ("blueviolet", 0xE22B8A),
    ("brown", 0x2A2AA5),
    ("brown1", 0x4040FF),
    ("brown2", 0x3B3BEE),
    ("brown3", 0x3333CD),
    ("brown4", 0x23238B),
    ("burlywood", 0x87B8DE),
    ("burlywood1", 0x9BD3FF),
    ("burlywood2", 0x91C5EE),
    ("burlywood3", 0x7DAACD),
    ("burlywood4", 0x55738B),
    ("cadetblue", 0xA09E5F),
    ("cadetblue1", 0xFFF598),
    ("cadetblue2", 0xEEE58E),
    ("cadetblue3", 0xCDC57A),
    ("cadetblue4", 0x8B8653),
    ("chartreuse", 0x00FF7F),
    ("chartreuse1", 0x00FF7F),
    ("chartreuse2", 0x00EE76),
    ("chartreuse3", 0x00CD66),
    ("chartreuse4", 0x008B45),
    ("chocolate", 0x1E69D2),
    ("chocolate1", 0x247FFF),
    ("chocolate2", 0x2176EE),
    ("chocolate3", 0x1D66CD),
    ("chocolate4", 0x13458B),
    ("coral", 0x507FFF),
    ("coral1", 0x5672FF),
    ("coral2", 0x506AEE),
    ("coral3", 0x455BCD),
    ("coral4", 0x2F3E8B),
    ("cornflowerblue", 0xED9564),
    ("cornsilk", 0xDCF8FF),
    ("cornsilk1", 0xDCF8FF),
    ("cornsilk2", 0xCDE8EE),
    ("cornsilk3", 0xB1C8CD),
    ("cornsilk4", 0x78888B),
    ("cyan", 0xFFFF00),
    ("cyan1", 0xFFFF00),
    ("cyan2", 0xEEEE00),
    ("cyan3", 0xCDCD00),
    ("cyan4", 0x8B8B00),
    ("darkblue", 0x8B0000),
    ("darkcyan", 0x8B8B00),
    ("darkgoldenrod", 0x0B86B8),
    ("darkgoldenrod1", 0x0FB9FF),
    ("darkgoldenrod2", 0x0EADEE),
    ("darkgoldenrod3", 0x0C95CD),
    ("darkgoldenrod4", 0x08658B),
    ("darkgray", 0xA9A9A9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xA9A9A9),
    ("darkkhaki", 0x6BB7BD),
    ("darkmagenta", 0x8B008B),
    ("darkolivegreen", 0x2F6B55),
    ("darkolivegreen1", 0x70FFCA),
    ("darkolivegreen2", 0x68EEBC),
    ("darkolivegreen3", 0x5ACDA2),
    ("darkolivegreen4", 0x3D8B6E),
    ("darkorange", 0x008CFF),
    ("darkorange1", 0x007FFF),
    ("darkorange2", 0x0076EE),
    ("darkorange3", 0x0066CD),
    ("darkorange4", 0x00458B),
    ("darkorchid", 0xCC3299),
    ("darkorchid1", 0xFF3EBF),
    ("darkorchid2", 0xEE3AB2),
    ("darkorchid3", 0xCD329A),
    ("darkorchid4", 0x8B2268),
    ("darkred", 0x00008B),
    ("darksalmon", 0x7A96E9),
    ("darkseagreen", 0x8FBC8F),
    ("darkseagreen1", 0xC1FFC1),
    ("darkseagreen2", 0xB4EEB4),
    ("darkseagreen3", 0x9BCD9B),
    ("darkseagreen4", 0x698B69),
    ("darkslateblue", 0x8B3D48),
    ("darkslategray", 0x4F4F2F),
    ("darkslategray1", 0xFFFF97),
    ("darkslategray2", 0xEEEE8D),
    ("darkslategray3", 0xCDCD79),
    ("darkslategray4", 0x8B8B52),
    ("darkslategrey", 0x4F4F2F),
    ("darkturquoise", 0xD1CE00),
    ("darkviolet", 0xD30094),
    ("deeppink", 0x9314FF),
    ("deeppink1", 0x9314FF),
    ("deeppink2", 0x8912EE),
    ("deeppink3", 0x7610CD),
    ("deeppink4", 0x500A8B),
    ("deepskyblue", 0xFFBF00),
    ("deepskyblue1", 0xFFBF00),
    ("deepskyblue2", 0xEEB200),
    ("deepskyblue3", 0xCD9A00),
    ("deepskyblue4", 0x8B6800),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0xFF901E),
    ("dodgerblue1", 0xFF901E),
    ("dodgerblue2", 0xEE861C),
    ("dodgerblue3", 0xCD7418),
    ("dodgerblue4", 0x8B4E10),
    ("firebrick", 0x2222B2),
    ("firebrick1", 0x3030FF),
    ("firebrick2", 0x2C2CEE),
    ("firebrick3", 0x2626CD),
    ("firebrick4", 0x1A1A8B),
    ("floralwhite", 0xF0FAFF),
    ("forestgreen", 0x228B22),
    ("gainsboro", 0xDCDCDC),
    ("ghostwhite", 0xFFF8F8),
    ("gold", 0x00D7FF),
    ("gold1", 0x00D7FF),
    ("gold2", 0x00C9EE),
    ("gold3", 0x00ADCD),
    ("gold4", 0x00758B),
    ("goldenrod", 0x20A5DA),
    ("goldenrod1", 0x25C1FF),
    ("goldenrod2", 0x22B4EE),
    ("goldenrod3", 0x1D9BCD),
    ("goldenrod4", 0x14698B),
    ("gray", 0xBEBEBE),
    ("gray0", 0x000000),
    ("gray1", 0x030303),
    ("gray2", 0x050505),
    ("gray3", 0x080808),
    ("gray4", 0x0A0A0A),
    ("gray5", 0x0D0D0D),
    ("gray6", 0x0F0F0F),
    ("gray7", 0x121212),
    ("gray8", 0x141414),
    ("gray9", 0x171717),
    ("gray10", 0x1A1A1A),
    ("gray11", 0x1C1C1C),
    ("gray12", 0x1F1F1F),
    ("gray13", 0x212121),
    ("gray14", 0x242424),
    ("gray15", 0x262626),
    ("gray16", 0x292929),
    ("gray17", 0x2B2B2B),
    ("gray18", 0x2E2E2E),
    ("gray19", 0x303030),
    ("gray20", 0x333333),
    ("gray21", 0x363636),
    ("gray22", 0x383838),
    ("gray23", 0x3B3B3B),
    ("gray24", 0x3D3D3D),
    ("gray25", 0x404040),
    ("gray26", 0x424242),
    ("gray27", 0x454545),
    ("gray28", 0x474747),
    ("gray29", 0x4A4A4A),
    ("gray30", 0x4D4D4D),
    ("gray31", 0x4F4F4F),
    ("gray32", 0x525252),
    ("gray33", 0x545454),
    ("gray34", 0x575757),
    ("gray35", 0x595959),
    ("gray36", 0x5C5C5C),
    ("gray37", 0x5E5E5E),
    ("gray38", 0x616161),
    ("gray39", 0x636363),
    ("gray40", 0x666666),
    ("gray41", 0x696969),
    ("gray42", 0x6B6B6B),
    ("gray43", 0x6E6E6E),
    ("gray44", 0x707070),
    ("gray45", 0x737373),
    ("gray46", 0x757575),
    ("gray47", 0x787878),
    ("gray48", 0x7A7A7A),
    ("gray49", 0x7D7D7D),
    ("gray50", 0x7F7F7F),
    ("gray51", 0x828282),
    ("gray52", 0x858585),
    ("gray53", 0x878787),
    ("gray54", 0x8A8A8A),
    ("gray55", 0x8C8C8C),
    ("gray56", 0x8F8F8F),
    ("gray57", 0x919191),
    ("gray58", 0x949494),
    ("gray59", 0x969696),
    ("gray60", 0x999999),
    ("gray61", 0x9C9C9C),
    ("gray62", 0x9E9E9E),
    ("gray63", 0xA1A1A1),
    ("gray64", 0xA3A3A3),
    ("gray65", 0xA6A6A6),
    ("gray66", 0xA8A8A8),
    ("gray67", 0xABABAB),
    ("gray68", 0xADADAD),
    ("gray69", 0xB0B0B0),
    ("gray70", 0xB3B3B3),
    ("gray71", 0xB5B5B5),
    ("gray72", 0xB8B8B8),
    ("gray73", 0xBABABA),
    ("gray74", 0xBDBDBD),
    ("gray75", 0xBFBFBF),
    ("gray76", 0xC2C2C2),
    ("gray77", 0xC4C4C4),
    ("gray78", 0xC7C7C7),
    ("gray79", 0xC9C9C9),
    ("gray80", 0xCCCCCC),
    ("gray81", 0xCFCFCF),
    ("gray82", 0xD1D1D1),
    ("gray83", 0xD4D4D4),
    ("gray84", 0xD6D6D6),
    ("gray85", 0xD9D9D9),
    ("gray86", 0xDBDBDB),
    ("gray87", 0xDEDEDE),
    ("gray88", 0xE0E0E0),
    ("gray89", 0xE3E3E3),
    ("gray90", 0xE5E5E5),
    ("gray91", 0xE8E8E8),
    ("gray92", 0xEBEBEB),
    ("gray93", 0xEDEDED),
    ("gray94", 0xF0F0F0),
    ("gray95", 0xF2F2F2),
    ("gray96", 0xF5F5F5),
    ("gray97", 0xF7F7F7),
    ("gray98", 0xFAFAFA),
    ("gray99", 0xFCFCFC),
    ("gray100", 0xFFFFFF),
    ("green", 0x00FF00),
    ("green1", 0x00FF00),
    ("green2", 0x00EE00),
    ("green3", 0x00CD00),
    ("green4", 0x008B00),
    ("greenyellow", 0x2FFFAD),
    ("grey", 0xBEBEBE),
    ("grey0", 0x000000),
    ("grey1", 0x030303),
    ("grey2", 0x050505),
    ("grey3", 0x080808),
    ("grey4", 0x0A0A0A),
    ("grey5", 0x0D0D0D),
    ("grey6", 0x0F0F0F),
    ("grey7", 0x121212),
    ("grey8", 0x141414),
    ("grey9", 0x171717),
    ("grey10", 0x1A1A1A),
    ("grey11", 0x1C1C1C),
    ("grey12", 0x1F1F1F),
    ("grey13", 0x212121),
    ("grey14", 0x242424),
    ("grey15", 0x262626),
    ("grey16", 0x292929),
    ("grey17", 0x2B2B2B),
    ("grey18", 0x2E2E2E),
    ("grey19", 0x303030),
    ("grey20", 0x333333),
    ("grey21", 0x363636),
    ("grey22", 0x383838),
    ("grey23", 0x3B3B3B),
    ("grey24", 0x3D3D3D),
    ("grey25", 0x404040),
    ("grey26", 0x424242),
    ("grey27", 0x454545),
    ("grey28", 0x474747),
    ("grey29", 0x4A4A4A),
    ("grey30", 0x4D4D4D),
    ("grey31", 0x4F4F4F),
    ("grey32", 0x525252),
    ("grey33", 0x545454),
    ("grey34", 0x575757),
    ("grey35", 0x595959),
    ("grey36", 0x5C5C5C),
    ("grey37", 0x5E5E5E),
    ("grey38", 0x616161),
    ("grey39", 0x636363),
    ("grey40", 0x666666),
    ("grey41", 0x696969),
    ("grey42", 0x6B6B6B),
    ("grey43", 0x6E6E6E),
    ("grey44", 0x707070),
    ("grey45", 0x737373),
    ("grey46", 0x757575),
    ("grey47", 0x787878),
    ("grey48", 0x7A7A7A),
    ("grey49", 0x7D7D7D),
    ("grey50", 0x7F7F7F),
    ("grey51", 0x828282),
    ("grey52", 0x858585),
    ("grey53", 0x878787),
    ("grey54", 0x8A8A8A),
    ("grey55", 0x8C8C8C),
    ("grey56", 0x8F8F8F),
    ("grey57", 0x919191),
    ("grey58", 0x949494),
    ("grey59", 0x969696),
    ("grey60", 0x999999),
    ("grey61", 0x9C9C9C),
    ("grey62", 0x9E9E9E),
    ("grey63", 0xA1A1A1),
    ("grey64", 0xA3A3A3),
    ("grey65", 0xA6A6A6),
    ("grey66", 0xA8A8A8),
    ("grey67", 0xABABAB),
    ("grey68", 0xADADAD),
    ("grey69", 0xB0B0B0),
    ("grey70", 0xB3B3B3),
    ("grey71", 0xB5B5B5),
    ("grey72", 0xB8B8B8),
    ("grey73", 0xBABABA),
    ("grey74", 0xBDBDBD),
    ("grey75", 0xBFBFBF),
    ("grey76", 0xC2C2C2),
    ("grey77", 0xC4C4C4),
    ("grey78", 0xC7C7C7),
    ("grey79", 0xC9C9C9),
    ("grey80", 0xCCCCCC),
    ("grey81", 0xCFCFCF),
    ("grey82", 0xD1D1D1),
    ("grey83", 0xD4D4D4),
    ("grey84", 0xD6D6D6),
    ("grey85", 0xD9D9D9),
    ("grey86", 0xDBDBDB),
    ("grey87", 0xDEDEDE),
    ("grey88", 0xE0E0E0),
    ("grey89", 0xE3E3E3),
    ("grey90", 0xE5E5E5),
    ("grey91", 0xE8E8E8),
    ("grey92", 0xEBEBEB),
    ("grey93", 0xEDEDED),
    ("grey94", 0xF0F0F0),
    ("grey95", 0xF2F2F2),
    ("grey96", 0xF5F5F5),
    ("grey97", 0xF7F7F7),
    ("grey98", 0xFAFAFA),
    ("grey99", 0xFCFCFC),
    ("grey100", 0xFFFFFF),
    ("honeydew", 0xF0FFF0),
    ("honeydew1", 0xF0FFF0),
    ("honeydew2", 0xE0EEE0),
    ("honeydew3", 0xC1CDC1),
    ("honeydew4", 0x838B83),
    ("hotpink", 0xB469FF),
    ("hotpink1", 0xB46EFF),
    ("hotpink2", 0xA76AEE),
    ("hotpink3", 0x9060CD),
    ("hotpink4", 0x623A8B),
    ("indianred", 0x5C5CCD),
    ("indianred1", 0x6A6AFF),
    ("indianred2", 0x6363EE),
    ("indianred3", 0x5555CD),
    ("indianred4", 0x3A3A8B),
    ("ivory", 0xF0FFFF),
    ("ivory1", 0xF0FFFF),
    ("ivory2", 0xE0EEEE),
    ("ivory3", 0xC1CDCD),
    ("ivory4", 0x838B8B),
    ("khaki", 0x8CE6F0),
    ("khaki1", 0x8FF6FF),
    ("khaki2", 0x85E6EE),
    ("khaki3", 0x73C6CD),
    ("khaki4", 0x4E868B),
    ("lavender", 0xFAE6E6),
    ("lavenderblush", 0xF5F0FF),
    ("lavenderblush1", 0xF5F0FF),
    ("lavenderblush2", 0xE5E0EE),
    ("lavenderblush3", 0xC5C1CD),
    ("lavenderblush4", 0x86838B),
    ("lawngreen", 0x00FC7C),
    ("lemonchiffon", 0xCDFAFF),
    ("lemonchiffon1", 0xCDFAFF),
    ("lemonchiffon2", 0xBFE9EE),
    ("lemonchiffon3", 0xA5C9CD),
    ("lemonchiffon4", 0x70898B),
    ("lightblue", 0xE6D8AD),
    ("lightblue1", 0xFFEFBF),
    ("lightblue2", 0xEEDFB2),
    ("lightblue3", 0xCDC09A),
    ("lightblue4", 0x8B8368),
    ("lightcoral", 0x8080F0),
    ("lightcyan", 0xFFFFE0),
    ("lightcyan1", 0xFFFFE0),
    ("lightcyan2", 0xEEEED1),
    ("lightcyan3", 0xCDCDB4),
    ("lightcyan4", 0x8B8B7A),
    ("lightgoldenrod", 0x82DDEE),
    ("lightgoldenrod1", 0x8BECFF),
    ("lightgoldenrod2", 0x82DCEE),
    ("lightgoldenrod3", 0x70BECD),
    ("lightgoldenrod4", 0x4C818B),
    ("lightgoldenrodyellow", 0xD2FAFA),
    ("lightgray", 0xD3D3D3),
    ("lightgreen", 0x90EE90),
    ("lightgrey", 0xD3D3D3),
    ("lightpink", 0xC1B6FF),
    ("lightpink1", 0xB9AEFF),
    ("lightpink2", 0xADA2EE),
    ("lightpink3", 0x958CCD),
    ("lightpink4", 0x655F8B),
    ("lightsalmon", 0x7AA0FF),
    ("lightsalmon1", 0x7AA0FF),
    ("lightsalmon2", 0x7295EE),
    ("lightsalmon3", 0x6281CD),
    ("lightsalmon4", 0x42578B),
    ("lightseagreen", 0xAAB220),
    ("lightskyblue", 0xFACE87),
    ("lightskyblue1", 0xFFE2B0),
    ("lightskyblue2", 0xEED3A4),
    ("lightskyblue3", 0xCDB68D),
    ("lightskyblue4", 0x8B7B60),
    ("lightslateblue", 0xFF7084),
    ("lightslategray", 0x998877),
    ("lightslategrey", 0x998877),
    ("lightsteelblue", 0xDEC4B0),
    ("lightsteelblue1", 0xFFE1CA),
    ("lightsteelblue2", 0xEED2BC),
    ("lightsteelblue3", 0xCDB5A2),
    ("lightsteelblue4", 0x8B7B6E),
    ("lightyellow", 0xE0FFFF),
    ("lightyellow1", 0xE0FFFF),
    ("lightyellow2", 0xD1EEEE),
    ("lightyellow3", 0xB4CDCD),
    ("lightyellow4", 0x7A8B8B),
    ("limegreen", 0x32CD32),
    ("linen", 0xE6F0FA),
    ("magenta", 0xFF00FF),
    ("magenta1", 0xFF00FF),
    ("magenta2", 0xEE00EE),
    ("magenta3", 0xCD00CD),
    ("magenta4", 0x8B008B),
    ("maroon", 0x6030B0),
    ("maroon1", 0xB334FF),
    ("maroon2", 0xA730EE),
    ("maroon3", 0x9029CD),
    ("maroon4", 0x621C8B),
    ("mediumaquamarine", 0xAACD66),
    ("mediumblue", 0xCD0000),
    ("mediumorchid", 0xD355BA),
    ("mediumorchid1", 0xFF66E0),
    ("mediumorchid2", 0xEE5FD1),
    ("mediumorchid3", 0xCD52B4),
    ("mediumorchid4", 0x8B377A),
    ("mediumpurple", 0xDB7093),
    ("mediumpurple1", 0xFF82AB),
    ("mediumpurple2", 0xEE799F),
    ("mediumpurple3", 0xCD6889),
    ("mediumpurple4", 0x8B475D),
    ("mediumseagreen", 0x71B33C),
    ("mediumslateblue", 0xEE687B),
    ("mediumspringgreen", 0x9AFA00),
    ("mediumturquoise", 0xCCD148),
    ("mediumvioletred", 0x8515C7),
    ("midnightblue", 0x701919),
    ("mintcream", 0xFAFFF5),
    ("mistyrose", 0xE1E4FF),
    ("mistyrose1", 0xE1E4FF),
    ("mistyrose2", 0xD2D5EE),
    ("mistyrose3", 0xB5B7CD),
    ("mistyrose4", 0x7B7D8B),
    ("moccasin", 0xB5E4FF),
    ("navajowhite", 0xADDEFF),
    ("navajowhite1", 0xADDEFF),
    ("navajowhite2", 0xA1CFEE),
    ("navajowhite3", 0x8BB3CD),
    ("navajowhite4", 0x5E798B),
    ("navy", 0x800000),
    ("navyblue", 0x800000),
    ("oldlace", 0xE6F5FD),
    ("olivedrab", 0x238E6B),
    ("olivedrab1", 0x3EFFC0),
    ("olivedrab2", 0x3AEEB3),
    ("olivedrab3", 0x32CD9A),
    ("olivedrab4", 0x228B69),
    ("orange", 0x00A5FF),
    ("orange1", 0x00A5FF),
    ("orange2", 0x009AEE),
    ("orange3", 0x0085CD),
    ("orange4", 0x005A8B),
    ("orangered", 0x0045FF),
    ("orangered1", 0x0045FF),
    ("orangered2", 0x0040EE),
    ("orangered3", 0x0037CD),
    ("orangered4", 0x00258B),
    ("orchid", 0xD670DA),
    ("orchid1", 0xFA83FF),
    ("orchid2", 0xE97AEE),
    ("orchid3", 0xC969CD),
    ("orchid4", 0x89478B),
    ("palegoldenrod", 0xAAE8EE),
    ("palegreen", 0x98FB98),
    ("palegreen1", 0x9AFF9A),
    ("palegreen2", 0x90EE90),
    ("palegreen3", 0x7CCD7C),
    ("palegreen4", 0x548B54),
    ("paleturquoise", 0xEEEEAF),
    ("paleturquoise1", 0xFFFFBB),
    ("paleturquoise2", 0xEEEEAE),
    ("paleturquoise3", 0xCDCD96),
    ("paleturquoise4", 0x8B8B66),
    ("palevioletred", 0x9370DB),
    ("palevioletred1", 0xAB82FF),
    ("palevioletred2", 0x9F79EE),
    ("palevioletred3", 0x8968CD),
    ("palevioletred4", 0x5D478B),
    ("papayawhip", 0xD5EFFF),
    ("peachpuff", 0xB9DAFF),
    ("peachpuff1", 0xB9DAFF),
    ("peachpuff2", 0xADCBEE),
    ("peachpuff3", 0x95AFCD),
    ("peachpuff4", 0x65778B),
    ("peru", 0x3F85CD),
    ("pink", 0xCBC0FF),
    ("pink1", 0xC5B5FF),
    ("pink2", 0xB8A9EE),
    ("pink3", 0x9E91CD),
    ("pink4", 0x6C638B),
    ("plum", 0xDDA0DD),
    ("plum1", 0xFFBBFF),
    ("plum2", 0xEEAEEE),
    ("plum3", 0xCD96CD),
    ("plum4", 0x8B668B),
    ("powderblue", 0xE6E0B0),
    ("purple", 0xF020A0),
    ("purple1", 0xFF309B),
    ("purple2", 0xEE2C91),
    ("purple3", 0xCD267D),
    ("purple4", 0x8B1A55),
    ("red", 0x0000FF),
    ("red1", 0x0000FF),
    ("red2", 0x0000EE),
    ("red3", 0x0000CD),
    ("red4", 0x00008B),
    ("rosybrown", 0x8F8FBC),
    ("rosybrown1", 0xC1C1FF),
    ("rosybrown2", 0xB4B4EE),
    ("rosybrown3", 0x9B9BCD),
    ("rosybrown4", 0x69698B),
    ("royalblue", 0xE16941),
    ("royalblue1", 0xFF7648),
    ("royalblue2", 0xEE6E43),
    ("royalblue3", 0xCD5F3A),
    ("royalblue4", 0x8B4027),
    ("saddlebrown", 0x13458B),
    ("salmon", 0x7280FA),
    ("salmon1", 0x698CFF),
    ("salmon2", 0x6282EE),
    ("salmon3", 0x5470CD),
    ("salmon4", 0x394C8B),
    ("sandybrown", 0x60A4F4),
    ("seagreen", 0x578B2E),
    ("seagreen1", 0x9FFF54),
    ("seagreen2", 0x94EE4E),
    ("seagreen3", 0x80CD43),
    ("seagreen4", 0x578B2E),
    ("seashell", 0xEEF5FF),
    ("seashell1", 0xEEF5FF),
    ("seashell2", 0xDEE5EE),
    ("seashell3", 0xBFC5CD),
    ("seashell4", 0x82868B),
    ("sienna", 0x2D52A0),
    ("sienna1", 0x4782FF),
    ("sienna2", 0x4279EE),
    ("sienna3", 0x3968CD),
    ("sienna4", 0x26478B),
    ("skyblue", 0xEBCE87),
    ("skyblue1", 0xFFCE87),
    ("skyblue2", 0xEEC07E),
    ("skyblue3", 0xCDA66C),
    ("skyblue4", 0x8B704A),
    ("slateblue", 0xCD5A6A),
    ("slateblue1", 0xFF6F83),
    ("slateblue2", 0xEE677A),
    ("slateblue3", 0xCD5969),
    ("slateblue4", 0x8B3C47),
    ("slategray", 0x908070),
    ("slategray1", 0xFFE2C6),
    ("slategray2", 0xEED3B9),
    ("slategray3", 0xCDB69F),
    ("slategray4", 0x8B7B6C),
    ("slategrey", 0x908070),
    ("snow", 0xFAFAFF),
    ("snow1", 0xFAFAFF),
    ("snow2", 0xE9E9EE),
    ("snow3", 0xC9C9CD),
    ("snow4", 0x89898B),
    ("springgreen", 0x7FFF00),
    ("springgreen1", 0x7FFF00),
    ("springgreen2", 0x76EE00),
    ("springgreen3", 0x66CD00),
    ("springgreen4", 0x458B00),
    ("steelblue", 0xB48246),
    ("steelblue1", 0xFFB863),
    ("steelblue2", 0xEEAC5C),
    ("steelblue3", 0xCD944F),
    ("steelblue4", 0x8B6436),
    ("tan", 0x8CB4D2),
    ("tan1", 0x4FA5FF),
    ("tan2", 0x499AEE),
    ("tan3", 0x3F85CD),
    ("tan4", 0x2B5A8B),
    ("thistle", 0xD8BFD8),
    ("thistle1", 0xFFE1FF),
    ("thistle2", 0xEED2EE),
    ("thistle3", 0xCDB5CD),
    ("thistle4", 0x8B7B8B),
    ("tomato", 0x4763FF),
    ("tomato1", 0x4763FF),
    ("tomato2", 0x425CEE),
    ("tomato3", 0x394FCD),
    ("tomato4", 0x26368B),
    ("turquoise", 0xD0E040),
    ("turquoise1", 0xFFF500),
    ("turquoise2", 0xEEE500),
    ("turquoise3", 0xCDC500),
    ("turquoise4", 0x8B8600),
    ("violet", 0xEE82EE),
    ("violetred", 0x9020D0),
    ("violetred1", 0x963EFF),
    ("violetred2", 0x8C3AEE),
    ("violetred3", 0x7832CD),
    ("violetred4", 0x52228B),
    ("wheat", 0xB3DEF5),
    ("wheat1", 0xBAE7FF),
    ("wheat2", 0xAED8EE),
    ("wheat3", 0x96BACD),
    ("wheat4", 0x667E8B),
    ("whitesmoke", 0xF5F5F5),
    ("yellow", 0x00FFFF),
    ("yellow1", 0x00FFFF),
    ("yellow2", 0x00EEEE),
    ("yellow3", 0x00CDCD),
    ("yellow4", 0x008B8B),
    ("yellowgreen", 0x32CD9A),
];
