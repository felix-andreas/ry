//! The native routines of grDevices and graphics: colours, palettes, axis ticks, convex hulls
//! and contour lines, which need no device, and the devices and the base graphics engine.
//!
//! Every device ry opens draws nothing. It keeps what R code can observe instead: the device
//! list, the device size and text metrics, and the full set of base graphics parameters,
//! which `plot.new`, `plot.window`, `layout` and the rest update as GNU R's base graphics
//! does, so scripts that plot as a side effect run to completion and read back the same
//! `par()` values.

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
use crate::graphics_render::{Format, Item, Page, Rect, Shape, Stroke, file_for_page, write_pages};
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
    ("devcur", do_dev_cur),
    ("devnext", do_dev_next),
    ("devprev", do_dev_prev),
    ("devset", do_dev_set),
    ("devoff", do_dev_off),
    ("devholdflush", do_hold_flush),
    ("devAskNewPage", do_ask_new_page),
    ("getGraphicsEventEnv", |_| Ok(nil())),
    ("PicTeX", do_pictex),
    ("PostScript", do_postscript),
    ("XFig", do_xfig),
    ("PDF", do_pdf),
    ("devCairo", do_cairo),
    ("Quartz", |_| {
        Err(error(
            "unable to create quartz() device target, given type may not be supported",
        ))
    }),
    ("X11", do_x11),
    ("savePlot", do_device_noop),
    ("devcap", do_dev_capabilities),
    ("devcapture", |_| Ok(nil())),
    ("devcontrol", do_dev_control),
    ("devcopy", do_device_noop),
    ("devdisplaylist", do_dev_display_list),
    ("devsize", do_dev_size),
    ("devUp", do_dev_up),
    ("getSnapshot", do_get_snapshot),
    ("playSnapshot", do_play_snapshot),
    ("getGraphicsEvent", no_events),
    ("setGraphicsEventEnv", no_events),
    ("setPattern", do_device_noop),
    ("setClipPath", do_device_noop),
    ("setMask", do_device_noop),
    ("defineGroup", do_device_noop),
    ("useGroup", do_device_noop),
];

pub const GRAPHICS_ROUTINES: &[(&str, NativeFn)] = &[
    ("C_BinCount", do_bin_count),
    ("C_par", do_par),
    ("C_StemLeaf", do_stem_leaf),
    // Whether contour labels follow the contour, which every device ry opens does.
    ("C_contourDef", |_| unsafe { scalar_bool(true) }),
    ("RunregisterBase", |_| Ok(nil())),
    ("C_plot_new", do_plot_new),
    ("C_plot_window", do_plot_window),
    ("C_layout", do_layout),
    ("C_axis", do_axis),
    ("C_strWidth", do_str_width),
    ("C_strHeight", do_str_height),
    ("C_convertX", do_convert_x),
    ("C_convertY", do_convert_y),
    ("C_persp", do_persp),
    ("C_dendwindow", do_dend_window),
    ("C_xspline", do_xspline),
    ("C_plotXY", do_plot_xy),
    ("C_locator", do_locator),
    ("C_identify", do_identify),
    ("C_abline", do_abline),
    ("C_arrows", do_arrows),
    ("C_box", do_box),
    ("C_clip", do_clip),
    ("C_contour", do_contour),
    ("C_dend", do_draw),
    ("C_erase", do_erase),
    ("C_filledcontour", do_draw),
    ("C_image", do_image),
    ("C_mtext", do_mtext),
    ("C_path", do_path),
    ("C_polygon", do_polygon),
    ("C_raster", do_raster),
    ("C_rect", do_rect),
    ("C_segments", do_segments),
    ("C_symbols", do_symbols),
    ("C_text", do_text),
    ("C_title", do_title),
];

/// Binds `.Device` and `.Devices` in base as GNU R does at startup, for a session whose only
/// device is the null device.
pub unsafe fn define_device_variables() -> R<()> {
    publish_devices()
}

/// How many devices can be open at once, counting the null device, as in GNU R.
const MAX_DEVICES: usize = 64;

const NULL_DEVICE_NAME: &str = "null device";

thread_local! {
    static DEVICES: RefCell<DeviceTable> = RefCell::new(DeviceTable::new());
}

/// The open devices. Slot 0 is the null device, which always exists and is never stored, so
/// a device's number is its slot plus one.
struct DeviceTable {
    slots: Vec<Option<Device>>,
    current: usize,
    /// The length of `.Devices`, which GNU R never shrinks: a closed device leaves `""`.
    listed: usize,
}

impl DeviceTable {
    fn new() -> Self {
        DeviceTable {
            slots: (0..MAX_DEVICES).map(|_| None).collect(),
            current: 0,
            listed: 1,
        }
    }
}

/// A device: the state R code can observe (its size, its text metrics and the base graphics
/// parameters) and the pages drawn on it.
struct Device {
    name: &'static str,
    /// The file the device was opened on, as given, which `.Devices` reports as the entry's
    /// `filepath` attribute. `pdf(NULL)` has none.
    file: Option<String>,
    /// Width and height in inches.
    size: [f64; 2],
    /// The size of a device unit in inches: a big point for vector devices, a pixel for
    /// bitmap ones.
    unit_inches: f64,
    /// Whether device y coordinates grow downwards, as in a bitmap.
    y_down: bool,
    pointsize: f64,
    display_list: bool,
    /// Whether `plot.new` has run on this device, without which nothing can be drawn.
    started: bool,
    pars: Pars,
    /// What closing the device writes to `file`; a device ry cannot render yet has none and
    /// writes nothing.
    format: Option<Format>,
    /// The pages drawn so far, which `dev.off` renders.
    pages: Vec<Page>,
    /// The region `clip()` set, until the next `plot.new`.
    clip: Option<Rect>,
    /// The document title a PDF records.
    title: String,
}

/// The base graphics parameters of a device. GNU R keeps two copies, the defaults that
/// `par` sets and the current values that a high-level call's inline parameters change for
/// its duration; ry reads inline parameters straight from the call, so one copy is enough.
#[derive(Clone)]
struct Pars {
    adj: f64,
    ann: bool,
    ask: bool,
    bg: Colour,
    bty: char,
    cex: f64,
    cex_axis: f64,
    cex_lab: f64,
    cex_main: f64,
    cex_sub: f64,
    col: Colour,
    col_axis: Colour,
    col_lab: Colour,
    col_main: Colour,
    col_sub: Colour,
    crt: f64,
    err: i32,
    family: String,
    fg: Colour,
    font: i32,
    font_axis: i32,
    font_lab: i32,
    font_main: i32,
    font_sub: i32,
    lab: [i32; 3],
    las: i32,
    lend: i32,
    lheight: f64,
    ljoin: i32,
    lmitre: f64,
    lty: u32,
    lwd: f64,
    mex: f64,
    mgp: [f64; 3],
    mkh: f64,
    new: bool,
    pch: Pch,
    ps: i32,
    pty: char,
    smo: f64,
    srt: f64,
    tck: f64,
    tcl: f64,
    usr: [f64; 4],
    xaxp: [f64; 3],
    xaxs: char,
    xaxt: char,
    xlog: bool,
    xpd: i32,
    yaxp: [f64; 3],
    yaxs: char,
    yaxt: char,
    ylbias: f64,
    ylog: bool,
    outer: OuterMargins,
    margins: Margins,
    plot_region: PlotRegion,
    figure_region: FigureRegion,
    layout: Layout,
    /// The character expansion that turns margin lines into inches (`par("csi")`). `plot.new`
    /// and the layout functions set it from `cex`; setting `cex` alone leaves it.
    margin_cex: f64,
    /// Whether the plot region is square, which `pty = "s"` asks for from the next
    /// `plot.new` or region change on.
    square: bool,
}

#[derive(Clone, PartialEq)]
enum Pch {
    Code(i32),
    Text(String),
}

/// The outer margins in the unit they were last set in: lines (`oma`), inches (`omi`) or the
/// inner region as a fraction of the device (`omd`).
#[derive(Clone, Copy)]
enum OuterMargins {
    Lines([f64; 4]),
    Inches([f64; 4]),
    Inner([f64; 4]),
}

/// The figure margins in the unit they were last set in: lines (`mar`) or inches (`mai`).
#[derive(Clone, Copy)]
enum Margins {
    Lines([f64; 4]),
    Inches([f64; 4]),
}

/// How the plot region is placed in the figure: inside the margins, at a fraction of the
/// figure (`plt`), or centred with a size in inches (`pin`).
#[derive(Clone, Copy)]
enum PlotRegion {
    Margins,
    Fraction([f64; 4]),
    Size([f64; 2]),
}

/// How the figure region is placed in the inner region: the current cell of the layout, a
/// fraction of the inner region (`fig`), or centred in the layout cell with a size in inches
/// (`fin`).
#[derive(Clone, Copy)]
enum FigureRegion {
    Layout,
    Fraction([f64; 4]),
    Size([f64; 2]),
}

/// An array of figures from `mfrow`, `mfcol` or `layout`. `cells` holds the figure number of
/// each cell, row by row.
#[derive(Clone)]
struct Layout {
    rows: usize,
    columns: usize,
    cells: Vec<usize>,
    figures: usize,
    widths: Vec<f64>,
    heights: Vec<f64>,
    cm_widths: Vec<bool>,
    cm_heights: Vec<bool>,
    respect: bool,
    /// The figure being drawn, from 1; the last one before the first plot, so that the
    /// next `plot.new` starts a page.
    current: usize,
}

impl Layout {
    fn grid(rows: usize, columns: usize, by_column: bool) -> Self {
        let cells = (0..rows * columns)
            .map(|cell| {
                let (row, column) = (cell / columns, cell % columns);
                if by_column {
                    column * rows + row + 1
                } else {
                    cell + 1
                }
            })
            .collect();
        Layout {
            rows,
            columns,
            cells,
            figures: rows * columns,
            widths: vec![1.0; columns],
            heights: vec![1.0; rows],
            cm_widths: vec![false; columns],
            cm_heights: vec![false; rows],
            respect: false,
            current: rows * columns,
        }
    }
}

/// The text size reduction GNU R applies to an array of figures.
fn layout_cex(rows: usize, columns: usize) -> f64 {
    if rows >= 3 || columns >= 3 {
        0.66
    } else if rows == 2 && columns == 2 {
        0.83
    } else {
        1.0
    }
}

const BLACK: Colour = 0xFF00_0000;
const WHITE: Colour = 0xFFFF_FFFF;

/// The line type codes: four bits per dash length, the first dash in the low bits.
const LTY_BLANK: u32 = u32::MAX;
const LTY_SOLID: u32 = 0;
const LINE_TYPES: [(&str, u32); 7] = [
    ("blank", LTY_BLANK),
    ("solid", LTY_SOLID),
    ("dashed", 0x44),
    ("dotted", 0x31),
    ("dotdash", 0x3431),
    ("longdash", 0x37),
    ("twodash", 0x2262),
];

impl Pars {
    fn new(pointsize: f64, bg: Colour, fg: Colour) -> Self {
        Pars {
            adj: 0.5,
            ann: true,
            ask: false,
            bg,
            bty: 'o',
            cex: 1.0,
            cex_axis: 1.0,
            cex_lab: 1.0,
            cex_main: 1.2,
            cex_sub: 1.0,
            col: fg,
            col_axis: BLACK,
            col_lab: BLACK,
            col_main: BLACK,
            col_sub: BLACK,
            crt: 0.0,
            err: 0,
            family: String::new(),
            fg,
            font: 1,
            font_axis: 1,
            font_lab: 1,
            font_main: 2,
            font_sub: 1,
            lab: [5, 5, 7],
            las: 0,
            lend: 0,
            lheight: 1.0,
            ljoin: 0,
            lmitre: 10.0,
            lty: LTY_SOLID,
            lwd: 1.0,
            mex: 1.0,
            mgp: [3.0, 1.0, 0.0],
            mkh: 0.001,
            new: false,
            pch: Pch::Code(1),
            ps: pointsize.round() as i32,
            pty: 'm',
            smo: 1.0,
            srt: 0.0,
            tck: values::na_real(),
            tcl: -0.5,
            usr: [0.0, 1.0, 0.0, 1.0],
            xaxp: [0.0, 1.0, 5.0],
            xaxs: 'r',
            xaxt: 's',
            xlog: false,
            xpd: 0,
            yaxp: [0.0, 1.0, 5.0],
            yaxs: 'r',
            yaxt: 's',
            ylbias: 0.2,
            ylog: false,
            outer: OuterMargins::Lines([0.0; 4]),
            margins: Margins::Lines([5.1, 4.1, 4.1, 2.1]),
            plot_region: PlotRegion::Margins,
            figure_region: FigureRegion::Layout,
            layout: Layout::grid(1, 1, false),
            margin_cex: 1.0,
            square: false,
        }
    }
}

/// The parameters `par` reports but refuses to set.
const READ_ONLY_PARS: [&str; 6] = ["cin", "cra", "csi", "cxy", "din", "page"];

// Device geometry. Regions are `[x0, x1, y0, y1]` and margins `[bottom, left, top, right]`,
// as in `par`.

impl Device {
    /// `par("cin")`: the default character cell in inches.
    fn char_inches(&self) -> [f64; 2] {
        [0.9 * self.pointsize / 72.0, 1.2 * self.pointsize / 72.0]
    }

    /// `par("csi")`: the height of a margin line in inches, before `mex`.
    fn line_inches(&self) -> f64 {
        self.char_inches()[1] * self.pars.margin_cex
    }

    fn outer_inches(&self) -> [f64; 4] {
        let [width, height] = self.size;
        match self.pars.outer {
            OuterMargins::Lines(lines) => {
                lines.map(|line| line * self.pars.mex * self.line_inches())
            }
            OuterMargins::Inches(inches) => inches,
            OuterMargins::Inner([x0, x1, y0, y1]) => [
                y0 * height,
                x0 * width,
                (1.0 - y1) * height,
                (1.0 - x1) * width,
            ],
        }
    }

    /// `par("omd")`: the inner region as a fraction of the device.
    fn inner_region(&self) -> [f64; 4] {
        let [width, height] = self.size;
        let [bottom, left, top, right] = self.outer_inches();
        [
            left / width,
            1.0 - right / width,
            bottom / height,
            1.0 - top / height,
        ]
    }

    fn inner_size(&self) -> [f64; 2] {
        let [x0, x1, y0, y1] = self.inner_region();
        [(x1 - x0) * self.size[0], (y1 - y0) * self.size[1]]
    }

    /// `par("fig")`: the figure region as a fraction of the inner region.
    fn figure_region(&self) -> [f64; 4] {
        let inner = self.inner_size();
        let cell = layout_region(&self.pars.layout, inner);
        match self.pars.figure_region {
            FigureRegion::Layout => cell,
            FigureRegion::Fraction(region) => region,
            FigureRegion::Size([width, height]) => {
                let centre = [(cell[0] + cell[1]) / 2.0, (cell[2] + cell[3]) / 2.0];
                let half = [width / inner[0] / 2.0, height / inner[1] / 2.0];
                [
                    centre[0] - half[0],
                    centre[0] + half[0],
                    centre[1] - half[1],
                    centre[1] + half[1],
                ]
            }
        }
    }

    /// `par("fin")`.
    fn figure_size(&self) -> [f64; 2] {
        let [x0, x1, y0, y1] = self.figure_region();
        let inner = self.inner_size();
        [(x1 - x0) * inner[0], (y1 - y0) * inner[1]]
    }

    /// `par("mai")`.
    fn margin_inches(&self) -> [f64; 4] {
        match self.pars.margins {
            Margins::Lines(lines) => lines.map(|line| line * self.pars.mex * self.line_inches()),
            Margins::Inches(inches) => inches,
        }
    }

    /// `par("plt")`: the plot region as a fraction of the figure region.
    fn plot_region(&self) -> [f64; 4] {
        let [width, height] = self.figure_size();
        let region = match self.pars.plot_region {
            PlotRegion::Fraction(region) => region,
            PlotRegion::Size(size) => {
                let half = [size[0] / width / 2.0, size[1] / height / 2.0];
                [0.5 - half[0], 0.5 + half[0], 0.5 - half[1], 0.5 + half[1]]
            }
            PlotRegion::Margins => {
                let [bottom, left, top, right] = self.margin_inches();
                [
                    left / width,
                    1.0 - right / width,
                    bottom / height,
                    1.0 - top / height,
                ]
            }
        };
        if !self.pars.square {
            return region;
        }
        let plot = [
            (region[1] - region[0]) * width,
            (region[3] - region[2]) * height,
        ];
        let side = plot[0].min(plot[1]);
        let centre = [(region[0] + region[1]) / 2.0, (region[2] + region[3]) / 2.0];
        let half = [side / width / 2.0, side / height / 2.0];
        [
            centre[0] - half[0],
            centre[0] + half[0],
            centre[1] - half[1],
            centre[1] + half[1],
        ]
    }

    /// `par("pin")`.
    fn plot_size(&self) -> [f64; 2] {
        let [x0, x1, y0, y1] = self.plot_region();
        let figure = self.figure_size();
        [(x1 - x0) * figure[0], (y1 - y0) * figure[1]]
    }

    /// The width of the user coordinate range of each axis, in data units rather than
    /// decades on a log axis, as `par("cxy")` measures it.
    fn user_extent(&self) -> [f64; 2] {
        let usr = self.pars.usr;
        let extent = |low: f64, high: f64, log: bool| {
            if log {
                10f64.powf(high) - 10f64.powf(low)
            } else {
                high - low
            }
        };
        [
            extent(usr[0], usr[1], self.pars.xlog),
            extent(usr[2], usr[3], self.pars.ylog),
        ]
    }

    /// Whether the next `plot.new` starts a new page (`par("page")`).
    fn page(&self) -> bool {
        !self.pars.new && self.pars.layout.current == self.pars.layout.figures
    }

    /// The device coordinates of the corners of the device.
    fn device_extent(&self) -> [f64; 4] {
        let [width, height] = self.size.map(|inches| inches / self.unit_inches);
        if self.y_down {
            [0.0, width, height, 0.0]
        } else {
            [0.0, width, 0.0, height]
        }
    }
}

/// The region of the layout's current figure as a fraction of an inner region of `inner`
/// inches. Centimetre widths and heights are absolute; the others share the rest in
/// proportion, at the same scale in both directions when the layout respects them.
fn layout_region(layout: &Layout, inner: [f64; 2]) -> [f64; 4] {
    let absolute = |sizes: &[f64], cm: &[bool]| -> f64 {
        sizes
            .iter()
            .zip(cm)
            .filter(|(_, cm)| **cm)
            .map(|(size, _)| size / 2.54)
            .sum()
    };
    let relative = |sizes: &[f64], cm: &[bool]| -> f64 {
        sizes
            .iter()
            .zip(cm)
            .filter(|(_, cm)| !**cm)
            .map(|(size, _)| *size)
            .sum()
    };
    let fixed = [
        absolute(&layout.widths, &layout.cm_widths),
        absolute(&layout.heights, &layout.cm_heights),
    ];
    let shares = [
        relative(&layout.widths, &layout.cm_widths),
        relative(&layout.heights, &layout.cm_heights),
    ];
    let mut scale = [0, 1].map(|axis| {
        if shares[axis] > 0.0 {
            (inner[axis] - fixed[axis]).max(0.0) / shares[axis]
        } else {
            0.0
        }
    });
    if layout.respect {
        let common = scale[0].min(scale[1]);
        scale = [common, common];
    }
    let inches = |sizes: &[f64], cm: &[bool], scale: f64| -> Vec<f64> {
        sizes
            .iter()
            .zip(cm)
            .map(|(size, cm)| if *cm { size / 2.54 } else { size * scale })
            .collect()
    };
    let widths = inches(&layout.widths, &layout.cm_widths, scale[0]);
    let heights = inches(&layout.heights, &layout.cm_heights, scale[1]);
    let offset = [
        (inner[0] - widths.iter().sum::<f64>()) / 2.0,
        (inner[1] - heights.iter().sum::<f64>()) / 2.0,
    ];
    let mut rows = (usize::MAX, 0);
    let mut columns = (usize::MAX, 0);
    for (cell, figure) in layout.cells.iter().enumerate() {
        if *figure == layout.current {
            let (row, column) = (cell / layout.columns, cell % layout.columns);
            rows = (rows.0.min(row), rows.1.max(row));
            columns = (columns.0.min(column), columns.1.max(column));
        }
    }
    if rows.0 == usize::MAX {
        return [0.0, 1.0, 0.0, 1.0];
    }
    let left = offset[0] + widths[..columns.0].iter().sum::<f64>();
    let right = left + widths[columns.0..=columns.1].iter().sum::<f64>();
    let top = offset[1] + heights[..rows.0].iter().sum::<f64>();
    let bottom = top + heights[rows.0..=rows.1].iter().sum::<f64>();
    [
        left / inner[0],
        right / inner[0],
        1.0 - bottom / inner[1],
        1.0 - top / inner[1],
    ]
}

/// The row and column of the top-left cell of the layout's current figure, from 1.
fn current_cell(layout: &Layout) -> [usize; 2] {
    layout
        .cells
        .iter()
        .position(|figure| *figure == layout.current)
        .map_or([1, 1], |cell| {
            [cell / layout.columns + 1, cell % layout.columns + 1]
        })
}

// The device table and `.Devices`.

/// Runs `action` on the current device, opening the default device first when only the null
/// device exists, as GNU R does for every routine that needs a device.
unsafe fn with_device<T>(action: impl FnOnce(&mut Device) -> R<T>) -> R<T> {
    if DEVICES.with(|devices| devices.borrow().current) == 0 {
        open_default_device()?;
    }
    DEVICES.with(|devices| {
        let mut devices = devices.borrow_mut();
        let current = devices.current;
        match devices.slots.get_mut(current).and_then(Option::as_mut) {
            Some(device) => action(device),
            None => Err(error("no active or default device")),
        }
    })
}

/// Like [`with_device`], for a routine that draws: drawing makes the next `plot.new` move to
/// a fresh figure even after `par(new = TRUE)`.
unsafe fn with_plot<T>(action: impl FnOnce(&mut Device) -> R<T>) -> R<T> {
    with_started_plot(|device| {
        device.pars.new = false;
        action(device)
    })
}

/// Like [`with_device`], for a routine that needs the coordinates of a plot started with
/// `plot.new`.
unsafe fn with_started_plot<T>(action: impl FnOnce(&mut Device) -> R<T>) -> R<T> {
    with_device(|device| {
        if !device.started {
            return Err(error("plot.new has not been called yet"));
        }
        action(device)
    })
}

/// Calls `getOption("device")`, which is a device function or the name of one.
unsafe fn open_default_device() -> R<()> {
    let option = crate::envir::get_option("device");
    let function = if values::is_function(option) {
        option
    } else if let Some(name) =
        values::first_string(option).filter(|_| option.kind() == SexpType::String)
    {
        let lookup = values::call(values::install("match.fun")?, &[scalar_string(&name)?])?;
        crate::heap().root(lookup);
        crate::eval::eval(lookup, values::global_env())?
    } else {
        return Err(error("no active or default device"));
    };
    crate::heap().root(function);
    crate::eval::apply(nil(), function, &[], values::global_env())?;
    Ok(())
}

/// Adds `device` in the first free slot and makes it current.
unsafe fn add_device(device: Device) -> R<()> {
    DEVICES.with(|devices| {
        let mut devices = devices.borrow_mut();
        let Some(slot) = (1..MAX_DEVICES).find(|slot| devices.slots[*slot].is_none()) else {
            return Err(error("too many open devices"));
        };
        devices.slots[slot] = Some(device);
        devices.current = slot;
        devices.listed = devices.listed.max(slot + 1);
        Ok(())
    })?;
    publish_devices()
}

/// The next open device after slot `from`, wrapping around, or the null device.
fn next_device(devices: &DeviceTable, from: usize) -> usize {
    // The wrap includes `from` itself, so a lone open device is its own successor.
    (from + 1..MAX_DEVICES)
        .chain(1..(from + 1).min(MAX_DEVICES))
        .find(|slot| devices.slots[*slot].is_some())
        .unwrap_or(0)
}

fn previous_device(devices: &DeviceTable, from: usize) -> usize {
    (1..from.min(MAX_DEVICES))
        .rev()
        .chain((from.max(1)..MAX_DEVICES).rev())
        .find(|slot| devices.slots[*slot].is_some())
        .unwrap_or(0)
}

/// Rebinds `.Devices` and `.Device` in base to the device table. Like GNU R's, `.Devices` is a
/// pairlist of device names, and the name of a device opened on a file carries the file as
/// its `filepath` attribute.
unsafe fn publish_devices() -> R<()> {
    let (entries, current) = DEVICES.with(|devices| {
        let devices = devices.borrow();
        let entries: Vec<(&'static str, Option<String>)> = (0..devices.listed)
            .map(|slot| match devices.slots[slot].as_ref() {
                _ if slot == 0 => (NULL_DEVICE_NAME, None),
                Some(device) => (device.name, device.file.clone()),
                None => ("", None),
            })
            .collect();
        let current = entries
            .get(devices.current)
            .map_or(NULL_DEVICE_NAME, |(name, _)| name);
        (entries, current)
    });
    let mut list = nil();
    for (name, file) in entries.iter().rev() {
        let entry = scalar_string(name)?;
        if let Some(file) = file {
            values::set_attr(entry, values::install("filepath")?, scalar_string(file)?)?;
        }
        list = values::cons(entry, list)?;
    }
    let base = values::base_env();
    crate::eval::define(values::install(".Devices")?, list, base)?;
    crate::eval::define(symbols().dot_device, scalar_string(current)?, base)?;
    Ok(())
}

/// A device argument as a device number from 1, the null device for anything else.
unsafe fn device_number(value: Sexp) -> usize {
    let number = values::as_integer_scalar(value);
    if number == NA_INTEGER || number < 1 {
        0
    } else {
        (number as usize - 1).min(MAX_DEVICES - 1)
    }
}

unsafe fn device_result(slot: usize) -> R<Sexp> {
    scalar_integer(slot as i32 + 1)
}

// grDevices: opening devices.

/// The paper sizes `postscript`, `xfig` and `pdf` know, in inches.
fn paper_size(paper: &str) -> Option<[f64; 2]> {
    let a4 = [21.0 / 2.54, 29.7 / 2.54];
    match paper.to_ascii_lowercase().as_str() {
        "a4" => Some(a4),
        "a4r" => Some([a4[1], a4[0]]),
        "letter" | "us" => Some([8.5, 11.0]),
        "usr" => Some([11.0, 8.5]),
        "legal" => Some([8.5, 14.0]),
        "executive" => Some([7.25, 10.5]),
        _ => None,
    }
}

/// The size of a paper device: the given width and height, or the paper less a quarter-inch
/// margin all round when they are zero, turned sideways when `horizontal`.
unsafe fn paper_device_size(paper: Sexp, width: Sexp, height: Sexp, horizontal: bool) -> [f64; 2] {
    let given = [
        values::as_real_scalar(width),
        values::as_real_scalar(height),
    ];
    if given.iter().all(|size| size.is_finite() && *size > 0.0) {
        return given;
    }
    let [paper_width, paper_height] = named_paper(paper).unwrap_or([21.0 / 2.54, 29.7 / 2.54]);
    let size = [paper_width - 0.5, paper_height - 0.5];
    if horizontal { [size[1], size[0]] } else { size }
}

/// The size of the paper a `paper` argument names, with `"default"` taken from
/// `getOption("papersize")`, or `None` for `"special"`.
unsafe fn named_paper(paper: Sexp) -> Option<[f64; 2]> {
    let mut paper =
        values::first_string(paper).map_or_else(String::new, |paper| paper.into_owned());
    if paper == "default" {
        paper = values::first_string(crate::envir::get_option("papersize"))
            .map_or_else(|| "a4".to_owned(), |paper| paper.into_owned());
    }
    paper_size(&paper)
}

unsafe fn colour_arg(value: Sexp, default: Colour) -> R<Colour> {
    if value == nil() || length(value) == 0 {
        return Ok(default);
    }
    colour_element(value, 0)
}

/// The file a device's `file` argument names: `NULL` and `""` name none.
unsafe fn device_file(file: Sexp) -> Option<String> {
    values::first_string(file)
        .filter(|name| file.kind() == SexpType::String && !name.is_empty())
        .map(|name| name.into_owned())
}

/// Where a device opened on `file` writes page `page`. A pipe (`|command`) writes no file.
fn output_path(file: &str, page: usize) -> Option<String> {
    (!file.starts_with('|')).then(|| file_for_page(file, page))
}

struct DeviceSpec {
    name: &'static str,
    file: Option<String>,
    /// A bitmap device writes its file when a page is finished, so one that never draws
    /// leaves no file; a vector device opens its file at once.
    bitmap: bool,
    format: Option<Format>,
    size: [f64; 2],
    unit_inches: f64,
    y_down: bool,
    pointsize: f64,
    bg: Colour,
    fg: Colour,
    title: String,
}

unsafe fn open_device(spec: DeviceSpec) -> R<Sexp> {
    if !(spec.size[0] > 0.0 && spec.size[1] > 0.0)
        || !spec.pointsize.is_finite()
        || spec.pointsize <= 0.0
    {
        return Err(error(format!("unable to start {} device", spec.name)));
    }
    if !spec.bitmap
        && let Some(path) = spec.file.as_deref().and_then(|file| output_path(file, 1))
    {
        std::fs::File::create(&path)
            .map_err(|_| error(format!("cannot open file '{path}'")))?;
    }
    add_device(Device {
        name: spec.name,
        file: spec.file,
        size: spec.size,
        unit_inches: spec.unit_inches,
        y_down: spec.y_down,
        pointsize: spec.pointsize,
        display_list: false,
        started: false,
        pars: Pars::new(spec.pointsize, spec.bg, spec.fg),
        format: spec.format,
        pages: Vec::new(),
        clip: None,
        title: spec.title,
    })?;
    Ok(nil())
}

/// `pdf(file, paper, family, encoding, bg, fg, width, height, pointsize, ...)`.
unsafe fn do_pdf(call: &NativeCall<'_>) -> R<Sexp> {
    let size = paper_device_size(call.arg(1), call.arg(6), call.arg(7), false);
    let spec = DeviceSpec {
        name: "pdf",
        file: device_file(call.arg(0)),
        bitmap: false,
        format: Some(Format::Pdf),
        size,
        unit_inches: 1.0 / 72.0,
        y_down: false,
        pointsize: values::as_real_scalar(call.arg(8)),
        bg: colour_arg(call.arg(4), TRANSPARENT_WHITE)?,
        fg: colour_arg(call.arg(5), BLACK)?,
        title: values::first_string(call.arg(11)).map_or_else(String::new, |title| title.into_owned()),
    };
    open_device(spec)
}

/// `postscript(file, paper, family, encoding, bg, fg, width, height, horizontal, pointsize,
/// ...)`.
unsafe fn do_postscript(call: &NativeCall<'_>) -> R<Sexp> {
    let horizontal = values::as_logical_scalar(call.arg(8)) == 1;
    let onefile = values::as_logical_scalar(call.arg(10)) != 0;
    let spec = DeviceSpec {
        name: "postscript",
        file: device_file(call.arg(0)),
        bitmap: false,
        format: Some(Format::Postscript {
            onefile,
            paper: named_paper(call.arg(1)),
            landscape: horizontal,
        }),
        size: paper_device_size(call.arg(1), call.arg(6), call.arg(7), horizontal),
        unit_inches: 1.0 / 72.0,
        y_down: false,
        pointsize: values::as_real_scalar(call.arg(9)),
        bg: colour_arg(call.arg(4), TRANSPARENT_WHITE)?,
        fg: colour_arg(call.arg(5), BLACK)?,
        title: values::first_string(call.arg(14)).map_or_else(String::new, |title| title.into_owned()),
    };
    open_device(spec)
}

/// `xfig(file, paper, family, bg, fg, width, height, horizontal, pointsize, ...)`.
unsafe fn do_xfig(call: &NativeCall<'_>) -> R<Sexp> {
    let horizontal = values::as_logical_scalar(call.arg(7)) == 1;
    let spec = DeviceSpec {
        name: "xfig",
        file: device_file(call.arg(0)),
        bitmap: false,
        format: None,
        size: paper_device_size(call.arg(1), call.arg(5), call.arg(6), horizontal),
        unit_inches: 1.0 / 72.0,
        y_down: false,
        pointsize: values::as_real_scalar(call.arg(8)),
        bg: colour_arg(call.arg(3), TRANSPARENT_WHITE)?,
        fg: colour_arg(call.arg(4), BLACK)?,
        title: String::new(),
    };
    open_device(spec)
}

/// `pictex(file, bg, fg, width, height, debug)`, whose text is always ten points.
unsafe fn do_pictex(call: &NativeCall<'_>) -> R<Sexp> {
    let spec = DeviceSpec {
        name: "pictex",
        file: device_file(call.arg(0)),
        bitmap: false,
        format: None,
        size: [
            values::as_real_scalar(call.arg(3)),
            values::as_real_scalar(call.arg(4)),
        ],
        unit_inches: 1.0 / 72.0,
        y_down: false,
        pointsize: 10.0,
        bg: colour_arg(call.arg(1), WHITE)?,
        fg: colour_arg(call.arg(2), BLACK)?,
        title: String::new(),
    };
    open_device(spec)
}

/// `devCairo(file, type, width, height, pointsize, bg, res, ...)`: the bitmap devices take
/// pixels at `res` pixels per inch, the vector ones (svg, cairo_pdf, cairo_ps) big points.
unsafe fn do_cairo(call: &NativeCall<'_>) -> R<Sexp> {
    let (name, bitmap, format) = match values::as_integer_scalar(call.arg(1)) {
        2 | 5 => ("png", true, Some(Format::Png)),
        3 => ("jpeg", true, Some(Format::Jpeg { quality: jpeg_quality(values::as_integer_scalar(call.arg(8))) })),
        4 => ("svg", false, Some(Format::Svg)),
        6 => ("cairo_pdf", false, Some(Format::Pdf)),
        7 => (
            "cairo_ps",
            false,
            Some(Format::Postscript { onefile: true, paper: None, landscape: false }),
        ),
        8 => ("tiff", true, Some(Format::Tiff)),
        9 => ("bmp", true, Some(Format::Bmp)),
        _ => return Err(error("invalid type for cairo device")),
    };
    let resolution = values::as_real_scalar(call.arg(6));
    let resolution = if bitmap && resolution.is_finite() && resolution > 0.0 {
        resolution
    } else {
        72.0
    };
    let pixels = [
        values::as_real_scalar(call.arg(2)),
        values::as_real_scalar(call.arg(3)),
    ];
    let spec = DeviceSpec {
        name,
        file: device_file(call.arg(0)),
        bitmap,
        format,
        size: pixels.map(|count| count / resolution),
        unit_inches: 1.0 / resolution,
        y_down: true,
        pointsize: values::as_real_scalar(call.arg(4)),
        bg: colour_arg(call.arg(5), WHITE)?,
        fg: BLACK,
        title: String::new(),
    };
    open_device(spec)
}

/// `X11(display, width, height, pointsize, ..., bg, ..., res, ...)`: only the bitmap files
/// that `png(type = "Xlib")` and friends name as `png::file` can open, since ry has no
/// display.
unsafe fn do_x11(call: &NativeCall<'_>) -> R<Sexp> {
    let display =
        values::first_string(call.arg(0)).map_or_else(String::new, |text| text.into_owned());
    let Some((kind, rest)) = display.split_once("::") else {
        return Err(error("unable to start device X11"));
    };
    let jpeg = rest
        .split_once(':')
        .and_then(|(quality, _)| quality.parse::<i32>().ok())
        .map_or(75, jpeg_quality);
    let (name, format) = match kind {
        "png" => ("png", Some(Format::Png)),
        "jpeg" => ("jpeg", Some(Format::Jpeg { quality: jpeg })),
        "bmp" => ("bmp", Some(Format::Bmp)),
        "tiff" => ("tiff", Some(Format::Tiff)),
        _ => return Err(error("unable to start device X11")),
    };
    let file = rest
        .rsplit_once(':')
        .filter(|_| kind != "png" && kind != "bmp")
        .map_or(rest, |(_, file)| file);
    let resolution = values::as_real_scalar(call.arg(10));
    let resolution = if resolution.is_finite() && resolution > 0.0 {
        resolution
    } else {
        72.0
    };
    let pixels = [
        values::as_real_scalar(call.arg(1)),
        values::as_real_scalar(call.arg(2)),
    ];
    let spec = DeviceSpec {
        name,
        file: Some(file.to_owned()).filter(|file| !file.is_empty()),
        bitmap: true,
        format,
        size: pixels.map(|count| count / resolution),
        unit_inches: 1.0 / resolution,
        y_down: true,
        pointsize: values::as_real_scalar(call.arg(3)),
        bg: colour_arg(call.arg(7), WHITE)?,
        fg: BLACK,
        title: String::new(),
    };
    open_device(spec)
}

/// `jpeg(quality = )` as the encoder takes it, from 1 to 100.
fn jpeg_quality(quality: i32) -> u8 {
    if quality == NA_INTEGER { 75 } else { quality.clamp(1, 100) as u8 }
}

// grDevices: the device list.

unsafe fn do_dev_cur(_: &NativeCall<'_>) -> R<Sexp> {
    device_result(DEVICES.with(|devices| devices.borrow().current))
}

unsafe fn do_dev_next(call: &NativeCall<'_>) -> R<Sexp> {
    let from = device_number(call.arg(0));
    device_result(DEVICES.with(|devices| next_device(&devices.borrow(), from)))
}

unsafe fn do_dev_prev(call: &NativeCall<'_>) -> R<Sexp> {
    let from = device_number(call.arg(0));
    device_result(DEVICES.with(|devices| previous_device(&devices.borrow(), from)))
}

/// `dev.set(which)`: makes `which` current, or the next open device after it when it is not
/// open.
unsafe fn do_dev_set(call: &NativeCall<'_>) -> R<Sexp> {
    let wanted = device_number(call.arg(0));
    let slot = DEVICES.with(|devices| {
        let mut devices = devices.borrow_mut();
        let slot = if wanted > 0 && devices.slots[wanted].is_some() {
            wanted
        } else {
            next_device(&devices, wanted)
        };
        devices.current = slot;
        slot
    });
    publish_devices()?;
    if slot == 0 {
        // GNU R selects the null device and then asks for the current device, which starts
        // the default one, yet still reports the null device as the one selected.
        open_default_device()?;
    }
    device_result(slot)
}

/// `dev.off(which)`: closes `which` and writes what was drawn on it; when it was current,
/// the next open device becomes current.
unsafe fn do_dev_off(call: &NativeCall<'_>) -> R<Sexp> {
    let which = device_number(call.arg(0));
    let closed = DEVICES.with(|devices| {
        let mut devices = devices.borrow_mut();
        let closed = if which > 0 {
            devices.slots[which].take()
        } else {
            None
        };
        if closed.is_some() && devices.current == which {
            devices.current = next_device(&devices, which);
        }
        closed
    });
    publish_devices()?;
    if let Some(device) = closed {
        write_device(&device)?;
    }
    Ok(nil())
}

/// Closes every open device, writing its pages, as GNU R does when a session ends.
pub unsafe fn close_all_devices() -> R<()> {
    let devices = DEVICES.with(|devices| {
        let mut devices = devices.borrow_mut();
        devices.current = 0;
        devices.slots.iter_mut().filter_map(Option::take).collect::<Vec<_>>()
    });
    publish_devices()?;
    for device in &devices {
        write_device(device)?;
    }
    Ok(())
}

/// Renders a closed device's pages into its file. A PDF is written even without pages, as
/// GNU R writes one for a device nothing was drawn on, but a bitmap device writes one file per
/// page and so nothing when it has none.
unsafe fn write_device(device: &Device) -> R<()> {
    let (Some(format), Some(file)) = (device.format, device.file.as_deref()) else {
        return Ok(());
    };
    if file.starts_with('|') {
        return Ok(());
    }
    write_pages(
        format,
        file,
        &device.pages,
        device.size,
        1.0 / device.unit_inches,
        &device.title,
    )
    .map_err(|failure| error(format!("cannot write file '{}': {failure}", file_for_page(file, 1))))
}

/// `dev.size()`: the device's width and height in device units.
unsafe fn do_dev_size(_: &NativeCall<'_>) -> R<Sexp> {
    let size = with_device(|device| Ok(device.size.map(|inches| inches / device.unit_inches)))?;
    values::real_vector(&size)
}

unsafe fn do_dev_up(_: &NativeCall<'_>) -> R<Sexp> {
    let up = with_device(|device| Ok(!device.y_down))?;
    scalar_bool(up)
}

/// `dev.control(enable)`: turns the display list on or off.
unsafe fn do_dev_control(call: &NativeCall<'_>) -> R<Sexp> {
    let enable = values::as_logical_scalar(call.arg(0)) == 1;
    let old = with_device(|device| Ok(std::mem::replace(&mut device.display_list, enable)))?;
    scalar_bool(old)
}

unsafe fn do_dev_display_list(_: &NativeCall<'_>) -> R<Sexp> {
    let enabled = with_device(|device| Ok(device.display_list))?;
    scalar_bool(enabled)
}

/// `dev.capabilities()`: the codes `dev.capabilities` decodes, for a device that supports
/// semi-transparency and rasters but none of the newer features, since ry never draws them.
unsafe fn do_dev_capabilities(_: &NativeCall<'_>) -> R<Sexp> {
    with_device(|_| Ok(()))?;
    let codes: [&[i32]; 13] = [
        &[2],
        &[3],
        &[2],
        &[1],
        &[1],
        &[0, 0, 0, 0],
        &[0],
        &[0],
        &[0],
        &[0],
        &[0],
        &[0],
        &[0],
    ];
    let result = alloc(SexpType::List, codes.len())?;
    crate::heap().root(result);
    for (index, code) in codes.iter().enumerate() {
        values::set_elt(result, index, values::integer_vector(code)?);
    }
    Ok(result)
}

/// GNU R 4.3's `R_GE_version`, which a recorded plot carries as `engineVersion`.
const GRAPHICS_ENGINE_VERSION: i32 = 16;

/// `recordPlot()`'s snapshot, shaped like GNU R's: the display list, then one raw vector of
/// state per graphics system (only base graphics here), with the engine version attached.
/// Nothing is ever drawn, so the display list is always `NULL`, which is how code such as
/// evaluate's plot capture tells that no plot was made. The state is empty because replaying
/// an empty display list restores nothing.
unsafe fn do_get_snapshot(_: &NativeCall<'_>) -> R<Sexp> {
    with_device(|_| Ok(()))?;
    let state = alloc(SexpType::Raw, 0)?;
    crate::heap().root(state);
    values::set_attr(
        state,
        values::install("pkgName")?,
        scalar_string("graphics")?,
    )?;
    let snapshot = values::list_vector(&[nil(), state])?;
    crate::heap().root(snapshot);
    values::set_attr(
        snapshot,
        values::install("engineVersion")?,
        scalar_integer(GRAPHICS_ENGINE_VERSION)?,
    )?;
    Ok(snapshot)
}

/// `replayPlot(snapshot)`: replaying an empty display list draws nothing. A snapshot GNU R
/// recorded with drawing on its display list cannot be replayed, since its state is GNU R's
/// in-memory layout.
unsafe fn do_play_snapshot(call: &NativeCall<'_>) -> R<Sexp> {
    with_device(|_| Ok(()))?;
    let snapshot = call.arg(0);
    if snapshot.kind() == SexpType::List && length(snapshot) > 0 && snapshot.element(0) == nil() {
        return Ok(nil());
    }
    Err(error(
        "cannot replay a plot whose display list is not empty: ry does not draw yet",
    ))
}

unsafe fn do_hold_flush(_: &NativeCall<'_>) -> R<Sexp> {
    scalar_integer(0)
}

unsafe fn no_events(_: &NativeCall<'_>) -> R<Sexp> {
    Err(error("graphics device does not support graphics events"))
}

/// The routines that only draw: they need a started plot and return `NULL`.
unsafe fn do_draw(_: &NativeCall<'_>) -> R<Sexp> {
    with_plot(|_| Ok(()))?;
    Ok(nil())
}

/// The routines that change a device's state without drawing: they need a device.
unsafe fn do_device_noop(_: &NativeCall<'_>) -> R<Sexp> {
    with_device(|_| Ok(()))?;
    Ok(nil())
}

// graphics: par.

/// `par(args)`: each named element sets that parameter and reports its old value, and each
/// unnamed string queries one.
unsafe fn do_par(call: &NativeCall<'_>) -> R<Sexp> {
    let args = call.arg(0);
    let count = if args.kind() == SexpType::List {
        length(args)
    } else {
        0
    };
    let names = values::names(args);
    let mut warnings = Vec::new();
    let mut entries: Vec<(String, ParValue)> = Vec::with_capacity(count);
    with_device(|device| {
        for index in 0..count {
            let value = values::list_elt(args, index);
            let tag = if names == nil() {
                None
            } else {
                values::string_at(names, index).filter(|name| !name.is_empty())
            };
            match tag {
                Some(name) => {
                    let old = get_par(device, &name);
                    if old.is_none() {
                        warnings.push(format!("\"{name}\" is not a graphical parameter"));
                    } else if READ_ONLY_PARS.contains(&&*name) {
                        warnings.push(format!("graphical parameter \"{name}\" cannot be set"));
                    } else {
                        set_par(device, &name, value, &mut warnings)?;
                    }
                    entries.push((name.into_owned(), old.unwrap_or(ParValue::Null)));
                }
                None => {
                    let query = if value.kind() == SexpType::String {
                        values::string_at(value, 0).map(|name| name.into_owned())
                    } else {
                        None
                    };
                    match query {
                        Some(name) => {
                            let found = get_par(device, &name);
                            if found.is_none() {
                                warnings.push(format!("\"{name}\" is not a graphical parameter"));
                            }
                            entries.push((name, found.unwrap_or(ParValue::Null)));
                        }
                        None => entries.push((String::new(), ParValue::Null)),
                    }
                }
            }
        }
        Ok(())
    })?;
    let result = alloc(SexpType::List, entries.len())?;
    crate::heap().root(result);
    for (index, (_, value)) in entries.iter().enumerate() {
        values::set_elt(result, index, par_value_sexp(value)?);
    }
    let labels: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    values::set_attr(result, symbols().names, string_vector(&labels)?)?;
    for warning in warnings {
        call.warning(warning)?;
    }
    Ok(result)
}

/// A parameter value, read out of the device before it becomes an R object.
enum ParValue {
    Null,
    Logical(Vec<i32>),
    Integer(Vec<i32>),
    Real(Vec<f64>),
    Text(String),
}

unsafe fn par_value_sexp(value: &ParValue) -> R<Sexp> {
    match value {
        ParValue::Null => Ok(nil()),
        ParValue::Logical(items) => values::logical_vector(items),
        ParValue::Integer(items) => values::integer_vector(items),
        ParValue::Real(items) => values::real_vector(items),
        ParValue::Text(text) => scalar_string(text),
    }
}

fn text(value: impl Into<String>) -> ParValue {
    ParValue::Text(value.into())
}

fn real(items: &[f64]) -> ParValue {
    ParValue::Real(items.to_vec())
}

fn logical(value: bool) -> ParValue {
    ParValue::Logical(vec![i32::from(value)])
}

fn line_type_name(code: u32) -> String {
    if let Some((name, _)) = LINE_TYPES.iter().find(|(_, value)| *value == code) {
        return (*name).to_owned();
    }
    let mut digits = String::new();
    let mut rest = code;
    while rest != 0 {
        digits
            .push(char::from_digit(rest & 0xF, 16).map_or('0', |digit| digit.to_ascii_uppercase()));
        rest >>= 4;
    }
    digits
}

/// The value of parameter `name`, or `None` when there is no such parameter.
fn get_par(device: &Device, name: &str) -> Option<ParValue> {
    let pars = &device.pars;
    let value = match name {
        "xlog" => logical(pars.xlog),
        "ylog" => logical(pars.ylog),
        "adj" => real(&[pars.adj]),
        "ann" => logical(pars.ann),
        "ask" => logical(pars.ask),
        "bg" => text(colour_name(pars.bg)),
        "bty" => text(pars.bty.to_string()),
        "cex" => real(&[pars.cex]),
        "cex.axis" => real(&[pars.cex_axis]),
        "cex.lab" => real(&[pars.cex_lab]),
        "cex.main" => real(&[pars.cex_main]),
        "cex.sub" => real(&[pars.cex_sub]),
        "cin" => real(&device.char_inches()),
        "col" => text(colour_name(pars.col)),
        "col.axis" => text(colour_name(pars.col_axis)),
        "col.lab" => text(colour_name(pars.col_lab)),
        "col.main" => text(colour_name(pars.col_main)),
        "col.sub" => text(colour_name(pars.col_sub)),
        "cra" => real(
            &device
                .char_inches()
                .map(|inches| inches / device.unit_inches),
        ),
        "crt" => real(&[pars.crt]),
        "csi" => real(&[device.line_inches()]),
        "cxy" => {
            let cin = device.char_inches();
            let pin = device.plot_size();
            let extent = device.user_extent();
            real(&[cin[0] / pin[0] * extent[0], cin[1] / pin[1] * extent[1]])
        }
        "din" => real(&device.size),
        "err" => ParValue::Integer(vec![pars.err]),
        "family" => text(pars.family.clone()),
        "fg" => text(colour_name(pars.fg)),
        "fig" => real(&device.figure_region()),
        "fin" => real(&device.figure_size()),
        "font" => ParValue::Integer(vec![pars.font]),
        "font.axis" => ParValue::Integer(vec![pars.font_axis]),
        "font.lab" => ParValue::Integer(vec![pars.font_lab]),
        "font.main" => ParValue::Integer(vec![pars.font_main]),
        "font.sub" => ParValue::Integer(vec![pars.font_sub]),
        "lab" => ParValue::Integer(pars.lab.to_vec()),
        "las" => ParValue::Integer(vec![pars.las]),
        "lend" => text(["round", "butt", "square"][pars.lend.clamp(0, 2) as usize]),
        "lheight" => real(&[pars.lheight]),
        "ljoin" => text(["round", "mitre", "bevel"][pars.ljoin.clamp(0, 2) as usize]),
        "lmitre" => real(&[pars.lmitre]),
        "lty" => text(line_type_name(pars.lty)),
        "lwd" => real(&[pars.lwd]),
        "mai" => real(&device.margin_inches()),
        "mar" => {
            let lines = match pars.margins {
                Margins::Lines(lines) => lines,
                Margins::Inches(inches) => {
                    inches.map(|inches| inches / (pars.mex * device.line_inches()))
                }
            };
            real(&lines)
        }
        "mex" => real(&[pars.mex]),
        "mfrow" | "mfcol" => {
            ParValue::Integer(vec![pars.layout.rows as i32, pars.layout.columns as i32])
        }
        "mfg" => {
            let [row, column] = current_cell(&pars.layout);
            ParValue::Integer(vec![
                row as i32,
                column as i32,
                pars.layout.rows as i32,
                pars.layout.columns as i32,
            ])
        }
        "mgp" => real(&pars.mgp),
        "mkh" => real(&[pars.mkh]),
        "new" => logical(pars.new),
        "oma" => {
            let lines = match pars.outer {
                OuterMargins::Lines(lines) => lines,
                _ => device
                    .outer_inches()
                    .map(|inches| inches / (pars.mex * device.line_inches())),
            };
            real(&lines)
        }
        "omd" => real(&device.inner_region()),
        "omi" => real(&device.outer_inches()),
        "page" => logical(device.page()),
        "pch" => match &pars.pch {
            Pch::Code(code) => ParValue::Integer(vec![*code]),
            Pch::Text(symbol) => text(symbol.clone()),
        },
        "pin" => real(&device.plot_size()),
        "plt" => real(&device.plot_region()),
        "ps" => ParValue::Integer(vec![pars.ps]),
        "pty" => text(pars.pty.to_string()),
        "smo" => real(&[pars.smo]),
        "srt" => real(&[pars.srt]),
        "tck" => real(&[pars.tck]),
        "tcl" => real(&[pars.tcl]),
        "usr" => real(&pars.usr),
        "xaxp" => real(&pars.xaxp),
        "xaxs" => text(pars.xaxs.to_string()),
        "xaxt" => text(pars.xaxt.to_string()),
        "xpd" => ParValue::Logical(vec![pars.xpd]),
        "yaxp" => real(&pars.yaxp),
        "yaxs" => text(pars.yaxs.to_string()),
        "yaxt" => text(pars.yaxt.to_string()),
        "ylbias" => real(&[pars.ylbias]),
        _ => return None,
    };
    Some(value)
}

fn invalid_par(name: &str) -> crate::unwind::Unwind {
    error(format!(
        "invalid value specified for graphical parameter \"{name}\""
    ))
}

/// The real values of a parameter that must have exactly `count` of them.
unsafe fn par_reals(value: Sexp, name: &str, count: usize) -> R<Vec<f64>> {
    if !values::is_numeric_type(value.kind()) {
        return Err(invalid_par(name));
    }
    if length(value) != count {
        return Err(error(format!(
            "graphical parameter \"{name}\" has the wrong length"
        )));
    }
    let reals = coerce_vector(value, SexpType::Real)?;
    Ok(values::reals(reals).to_vec())
}

unsafe fn par_real(value: Sexp, name: &str) -> R<f64> {
    if length(value) == 0
        || !(values::is_numeric_type(value.kind()) || value.kind() == SexpType::String)
    {
        return Err(invalid_par(name));
    }
    let number = values::as_real_scalar(value);
    if number.is_finite() {
        Ok(number)
    } else {
        Err(invalid_par(name))
    }
}

unsafe fn par_positive(value: Sexp, name: &str) -> R<f64> {
    let number = par_real(value, name)?;
    if number > 0.0 {
        Ok(number)
    } else {
        Err(invalid_par(name))
    }
}

unsafe fn par_integer(value: Sexp, name: &str) -> R<i32> {
    let number = par_real(value, name)?;
    Ok(number as i32)
}

unsafe fn par_flag(value: Sexp, name: &str) -> R<bool> {
    match values::as_logical_scalar(value) {
        flag if length(value) == 0 || flag == NA_INTEGER => Err(invalid_par(name)),
        flag => Ok(flag == 1),
    }
}

/// A one-character style parameter that must be one of `allowed`.
unsafe fn par_style(value: Sexp, name: &str, allowed: &str) -> R<char> {
    let text = values::first_string(value).filter(|_| value.kind() == SexpType::String);
    match text.as_deref().and_then(|text| text.chars().next()) {
        Some(style) if allowed.contains(style) => Ok(style),
        _ => Err(invalid_par(name)),
    }
}

unsafe fn par_colour(value: Sexp, name: &str, background: Colour) -> R<Colour> {
    if length(value) == 0 {
        return Err(invalid_par(name));
    }
    let colour = colour_element(value, 0)?;
    // Colour 0 is the device background.
    let zero = match value.kind() {
        SexpType::String => values::string_at(value, 0).as_deref() == Some("0"),
        _ => crate::subset::numeric_element(value, 0) == 0.0,
    };
    Ok(if zero { background } else { colour })
}

unsafe fn par_line_type(value: Sexp) -> R<u32> {
    if length(value) == 0 {
        return Err(invalid_par("lty"));
    }
    if value.kind() == SexpType::String {
        let Some(text) = values::string_at(value, 0) else {
            return Err(invalid_par("lty"));
        };
        if let Some((_, code)) = LINE_TYPES.iter().find(|(name, _)| *name == &*text) {
            return Ok(*code);
        }
        return parse_dash_pattern(&text).map_err(error);
    }
    let number = values::as_real_scalar(value);
    if !number.is_finite() || number < 0.0 {
        return Err(error("invalid line type"));
    }
    let index = number as usize;
    Ok(if index == 0 {
        LTY_BLANK
    } else {
        LINE_TYPES[(index - 1) % 6 + 1].1
    })
}

/// A line type given as hexadecimal dash and gap lengths, such as `"44"`.
fn parse_dash_pattern(text: &str) -> Result<u32, String> {
    if !matches!(text.len(), 2 | 4 | 6 | 8) {
        return Err("invalid line type: must be length 2, 4, 6 or 8".to_owned());
    }
    let mut code = 0u32;
    for (index, character) in text.chars().enumerate() {
        let digit = character
            .to_digit(16)
            .ok_or_else(|| "invalid hex digit in 'color' or 'lty'".to_owned())?;
        if digit == 0 {
            return Err("invalid line type: zeroes are not allowed".to_owned());
        }
        code |= digit << (4 * index);
    }
    Ok(code)
}

unsafe fn par_line_style(value: Sexp, name: &str, names: [&str; 3]) -> R<i32> {
    if value.kind() == SexpType::String {
        let text = values::first_string(value);
        return names
            .iter()
            .position(|candidate| text.as_deref() == Some(*candidate))
            .map(|index| index as i32)
            .ok_or_else(|| invalid_par(name));
    }
    let number = par_integer(value, name)?;
    if (0..=2).contains(&number) {
        Ok(number)
    } else {
        Err(invalid_par(name))
    }
}

unsafe fn par_pch(value: Sexp) -> R<Pch> {
    if length(value) == 0 {
        return Err(invalid_par("pch"));
    }
    if value.kind() == SexpType::String {
        let text = values::string_at(value, 0).map_or_else(String::new, |text| text.into_owned());
        return Ok(Pch::Text(
            text.chars().next().map_or_else(String::new, String::from),
        ));
    }
    let number = values::as_real_scalar(value);
    Ok(Pch::Code(if number.is_nan() {
        NA_INTEGER
    } else {
        number as i32
    }))
}

fn four(items: &[f64]) -> [f64; 4] {
    [items[0], items[1], items[2], items[3]]
}

/// Sets parameter `name`, which [`get_par`] knows and is not read-only.
unsafe fn set_par(
    device: &mut Device,
    name: &str,
    value: Sexp,
    warnings: &mut Vec<String>,
) -> R<()> {
    let background = device.pars.bg;
    let pars = &mut device.pars;
    match name {
        "xlog" => pars.xlog = par_flag(value, name)?,
        "ylog" => pars.ylog = par_flag(value, name)?,
        "adj" => pars.adj = par_real(value, name)?,
        "ann" => pars.ann = par_flag(value, name)?,
        "ask" => pars.ask = par_flag(value, name)?,
        "bg" => {
            pars.bg = par_colour(value, name, background)?;
            pars.new = false;
        }
        "bty" => pars.bty = par_style(value, name, "ol7cu]n")?,
        "cex" => pars.cex = par_positive(value, name)?,
        "cex.axis" => pars.cex_axis = par_positive(value, name)?,
        "cex.lab" => pars.cex_lab = par_positive(value, name)?,
        "cex.main" => pars.cex_main = par_positive(value, name)?,
        "cex.sub" => pars.cex_sub = par_positive(value, name)?,
        "col" => pars.col = par_colour(value, name, background)?,
        "col.axis" => pars.col_axis = par_colour(value, name, background)?,
        "col.lab" => pars.col_lab = par_colour(value, name, background)?,
        "col.main" => pars.col_main = par_colour(value, name, background)?,
        "col.sub" => pars.col_sub = par_colour(value, name, background)?,
        "crt" => pars.crt = par_real(value, name)?,
        "err" => pars.err = par_integer(value, name)?,
        "family" => {
            let family = values::first_string(value).filter(|_| value.kind() == SexpType::String);
            pars.family = family.ok_or_else(|| invalid_par(name))?.into_owned();
        }
        // The foreground colour is also the drawing colour.
        "fg" => {
            pars.fg = par_colour(value, name, background)?;
            pars.col = pars.fg;
        }
        "fig" => {
            let region = four(&par_reals(value, name, 4)?);
            if !(region[0] < region[1] && region[2] < region[3]) {
                return Err(invalid_par(name));
            }
            pars.figure_region = FigureRegion::Fraction(region);
            pars.layout = Layout::grid(1, 1, false);
        }
        "fin" => {
            let size = par_reals(value, name, 2)?;
            if size.iter().any(|size| !(*size > 0.0)) {
                return Err(invalid_par(name));
            }
            pars.figure_region = FigureRegion::Size([size[0], size[1]]);
        }
        "font" => pars.font = par_font(value, name)?,
        "font.axis" => pars.font_axis = par_font(value, name)?,
        "font.lab" => pars.font_lab = par_font(value, name)?,
        "font.main" => pars.font_main = par_font(value, name)?,
        "font.sub" => pars.font_sub = par_font(value, name)?,
        "lab" => {
            let lab = par_reals(value, name, 3)?;
            if lab.iter().any(|count| !(*count >= 0.0)) {
                return Err(invalid_par(name));
            }
            pars.lab = [lab[0] as i32, lab[1] as i32, lab[2] as i32];
        }
        "las" => {
            let las = par_integer(value, name)?;
            if !(0..=3).contains(&las) {
                return Err(invalid_par(name));
            }
            pars.las = las;
        }
        "lend" => pars.lend = par_line_style(value, name, ["round", "butt", "square"])?,
        "lheight" => pars.lheight = par_positive(value, name)?,
        "ljoin" => pars.ljoin = par_line_style(value, name, ["round", "mitre", "bevel"])?,
        "lmitre" => {
            let mitre = par_real(value, name)?;
            if mitre < 1.0 {
                return Err(invalid_par(name));
            }
            pars.lmitre = mitre;
        }
        "lty" => pars.lty = par_line_type(value)?,
        "lwd" => {
            let width = par_real(value, name)?;
            if width < 0.0 {
                return Err(invalid_par(name));
            }
            pars.lwd = width;
        }
        "mai" => {
            pars.margins = Margins::Inches(four(&non_negative(par_reals(value, name, 4)?, name)?));
            pars.plot_region = PlotRegion::Margins;
        }
        "mar" => {
            pars.margins = Margins::Lines(four(&non_negative(par_reals(value, name, 4)?, name)?));
            pars.plot_region = PlotRegion::Margins;
        }
        "mex" => pars.mex = par_positive(value, name)?,
        "mfrow" | "mfcol" => {
            let shape = par_reals(value, name, 2)?;
            if shape.iter().any(|count| !(*count >= 1.0)) {
                return Err(invalid_par(name));
            }
            let (rows, columns) = (shape[0] as usize, shape[1] as usize);
            pars.layout = Layout::grid(rows, columns, name == "mfcol");
            pars.figure_region = FigureRegion::Layout;
            pars.cex = layout_cex(rows, columns);
        }
        "mfg" => {
            let count = length(value);
            if count != 2 && count != 4 {
                return Err(error("parameter \"mfg\" has the wrong length"));
            }
            let cell = par_reals(value, name, count)?;
            let layout = &mut pars.layout;
            let (row, column) = (cell[0] as usize, cell[1] as usize);
            if count == 4 && (cell[2] as usize != layout.rows || cell[3] as usize != layout.columns)
            {
                return Err(error(
                    "value of 'nr' in \"mfg\" is wrong and will be ignored",
                ));
            }
            if row < 1 || row > layout.rows || column < 1 || column > layout.columns {
                return Err(invalid_par(name));
            }
            layout.current = layout.cells[(row - 1) * layout.columns + column - 1];
            pars.new = true;
        }
        "mgp" => {
            pars.mgp = {
                let mgp = par_reals(value, name, 3)?;
                [mgp[0], mgp[1], mgp[2]]
            }
        }
        "mkh" => pars.mkh = par_positive(value, name)?,
        "new" => {
            let new = par_flag(value, name)?;
            if new && !device.started {
                warnings.push("calling par(new=TRUE) with no plot".to_owned());
            } else {
                pars.new = new;
            }
        }
        "oma" => {
            pars.outer = OuterMargins::Lines(four(&non_negative(par_reals(value, name, 4)?, name)?))
        }
        "omi" => {
            pars.outer =
                OuterMargins::Inches(four(&non_negative(par_reals(value, name, 4)?, name)?))
        }
        "omd" => {
            let region = four(&par_reals(value, name, 4)?);
            if !(region[0] < region[1] && region[2] < region[3]) {
                return Err(invalid_par(name));
            }
            pars.outer = OuterMargins::Inner(region);
        }
        "pch" => pars.pch = par_pch(value)?,
        "pin" => {
            let size = par_reals(value, name, 2)?;
            if size.iter().any(|size| !(*size > 0.0)) {
                return Err(invalid_par(name));
            }
            pars.plot_region = PlotRegion::Size([size[0], size[1]]);
        }
        "plt" => {
            let region = four(&par_reals(value, name, 4)?);
            if !(region[0] < region[1] && region[2] < region[3]) {
                return Err(invalid_par(name));
            }
            pars.plot_region = PlotRegion::Fraction(region);
        }
        "ps" => pars.ps = par_positive(value, name)? as i32,
        "pty" => pars.pty = par_style(value, name, "ms")?,
        "smo" => pars.smo = par_real(value, name)?,
        "srt" => pars.srt = par_real(value, name)?,
        "tck" => {
            pars.tck = if length(value) == 1 && values::as_real_scalar(value).is_nan() {
                values::na_real()
            } else {
                par_real(value, name)?
            };
        }
        "tcl" => pars.tcl = par_real(value, name)?,
        "usr" => {
            let usr = four(&par_reals(value, name, 4)?);
            if usr.iter().any(|limit| !limit.is_finite()) {
                return Err(invalid_par(name));
            }
            pars.usr = usr;
            // The ticks follow the new limits, measured in data units on a log axis.
            let data = |low: f64, high: f64, log: bool| {
                if log {
                    [10f64.powf(low), 10f64.powf(high)]
                } else {
                    [low, high]
                }
            };
            pars.xaxp = axis_parameters(data(usr[0], usr[1], pars.xlog), pars.lab[0], false);
            pars.yaxp = axis_parameters(data(usr[2], usr[3], pars.ylog), pars.lab[1], false);
        }
        "xaxp" => pars.xaxp = three(&par_reals(value, name, 3)?),
        "yaxp" => pars.yaxp = three(&par_reals(value, name, 3)?),
        "xaxs" => pars.xaxs = par_style(value, name, "rised")?,
        "yaxs" => pars.yaxs = par_style(value, name, "rised")?,
        "xaxt" => pars.xaxt = par_style(value, name, "sltn")?,
        "yaxt" => pars.yaxt = par_style(value, name, "sltn")?,
        "xpd" => {
            if length(value) == 0 {
                return Err(invalid_par(name));
            }
            let flag = values::as_logical_scalar(value);
            pars.xpd = if flag == NA_INTEGER {
                NA_INTEGER
            } else {
                i32::from(flag != 0)
            };
        }
        "ylbias" => pars.ylbias = par_real(value, name)?,
        _ => {}
    }
    // Setting a region lays the page out again, which picks up the current `cex` and `pty`
    // as `plot.new` does.
    if REGION_PARS.contains(&name) {
        pars.margin_cex = pars.cex;
        pars.square = pars.pty == 's';
    }
    Ok(())
}

const REGION_PARS: [&str; 13] = [
    "fig", "fin", "mai", "mar", "mex", "mfcol", "mfg", "mfrow", "oma", "omd", "omi", "pin", "plt",
];

fn three(items: &[f64]) -> [f64; 3] {
    [items[0], items[1], items[2]]
}

fn non_negative(items: Vec<f64>, name: &str) -> R<Vec<f64>> {
    if items.iter().all(|item| *item >= 0.0) {
        Ok(items)
    } else {
        Err(invalid_par(name))
    }
}

unsafe fn par_font(value: Sexp, name: &str) -> R<i32> {
    let font = par_integer(value, name)?;
    if font >= 1 {
        Ok(font)
    } else {
        Err(invalid_par(name))
    }
}

// graphics: the plot window.

/// `plot.new()`: moves to the next figure unless `par(new = TRUE)`, and resets the user
/// coordinates to the unit square.
unsafe fn do_plot_new(_: &NativeCall<'_>) -> R<Sexp> {
    with_device(|device| {
        // Over a plot kept with `par(new = TRUE)`, the regions stay as they were laid out,
        // even when `cex` or `pty` changed since.
        // A new page starts when the last figure of the layout is done, or on the first plot.
        let layout = &device.pars.layout;
        if !device.started || (!device.pars.new && layout.current == layout.figures) {
            device.pages.push(Page {
                background: device.pars.bg,
                items: Vec::new(),
            });
        }
        device.clip = None;
        if !device.pars.new || !device.started {
            let pars = &mut device.pars;
            pars.layout.current = pars.layout.current % pars.layout.figures + 1;
            pars.margin_cex = pars.cex;
            pars.square = pars.pty == 's';
        }
        device.started = true;
        let inner = device.inner_size();
        if !(inner[0] > 0.0 && inner[1] > 0.0) {
            return Err(error("outer margins too large (figure region too large)"));
        }
        let figure = device.figure_size();
        if !(figure[0] > 0.0 && figure[1] > 0.0) {
            return Err(error("figure region too large"));
        }
        let plot = device.plot_region();
        if !(plot[0] < plot[1] && plot[2] < plot[3]) {
            return Err(error("figure margins too large"));
        }
        let tolerance = 1e-6;
        if plot[0] < -tolerance
            || plot[1] > 1.0 + tolerance
            || plot[2] < -tolerance
            || plot[3] > 1.0 + tolerance
        {
            return Err(error("plot region too large"));
        }
        let pars = &mut device.pars;
        pars.xlog = false;
        pars.ylog = false;
        let lab = pars.lab;
        let (x, xaxp) = scale_axis([0.0, 1.0], pars.xaxs, false, lab[0]);
        let (y, yaxp) = scale_axis([0.0, 1.0], pars.yaxs, false, lab[1]);
        pars.usr = [x[0], x[1], y[0], y[1]];
        pars.xaxp = xaxp;
        pars.yaxp = yaxp;
        Ok(())
    })?;
    Ok(nil())
}

/// The value of inline parameter `name` among a call's tagged arguments. A `NULL` value, as
/// in `plot.window(yaxs = pars$yaxs)` with no such element, leaves the parameter alone.
unsafe fn inline_par(call: &NativeCall<'_>, name: &str) -> Option<Sexp> {
    call.args
        .iter()
        .find(|arg| arg.tag.kind() == SexpType::Symbol && values::symbol_name(arg.tag) == name)
        .map(|arg| arg.value)
        .filter(|value| *value != nil())
}

unsafe fn inline_style(call: &NativeCall<'_>, name: &str, default: char) -> R<char> {
    match inline_par(call, name) {
        Some(value) => par_style(value, name, "rised"),
        None => Ok(default),
    }
}

/// `plot.window(xlim, ylim, log, asp, ...)`: the user coordinates for the data limits, with
/// the axis styles and tick counts the call or `par` gives.
unsafe fn do_plot_window(call: &NativeCall<'_>) -> R<Sexp> {
    let limits = |index: usize, name: &str| -> R<[f64; 2]> {
        let value = call.arg(index);
        let reals = if values::is_numeric_type(value.kind()) && length(value) == 2 {
            let reals = coerce_vector(value, SexpType::Real)?;
            [values::reals(reals)[0], values::reals(reals)[1]]
        } else {
            [values::na_real(); 2]
        };
        if reals.iter().all(|limit| limit.is_finite()) {
            Ok(reals)
        } else {
            Err(error(format!("need finite '{name}' values")))
        }
    };
    let xlim = limits(0, "xlim")?;
    let ylim = limits(1, "ylim")?;
    let log = values::first_string(call.arg(2)).map_or_else(String::new, |log| log.into_owned());
    let (mut xlog, mut ylog) = (false, false);
    for character in log.chars() {
        match character {
            'x' => xlog = true,
            'y' => ylog = true,
            _ => return Err(error(format!("invalid \"log={log}\" specification"))),
        }
    }
    if (xlog && xlim.iter().any(|limit| *limit <= 0.0))
        || (ylog && ylim.iter().any(|limit| *limit <= 0.0))
    {
        return Err(error("Logarithmic axis must have positive limits"));
    }
    let asp = values::as_real_scalar(call.arg(3));
    let lab = match inline_par(call, "lab") {
        Some(value) => {
            let lab = par_reals(value, "lab", 3)?;
            Some([lab[0] as i32, lab[1] as i32])
        }
        None => None,
    };
    let xaxs_inline = inline_par(call, "xaxs").is_some();
    let yaxs_inline = inline_par(call, "yaxs").is_some();
    let (xaxs, yaxs) = with_device(|device| Ok((device.pars.xaxs, device.pars.yaxs)))?;
    let xaxs = if xaxs_inline {
        inline_style(call, "xaxs", xaxs)?
    } else {
        xaxs
    };
    let yaxs = if yaxs_inline {
        inline_style(call, "yaxs", yaxs)?
    } else {
        yaxs
    };
    with_device(|device| {
        let lab = lab.unwrap_or([device.pars.lab[0], device.pars.lab[1]]);
        let logged =
            |limits: [f64; 2], log: bool| if log { limits.map(f64::log10) } else { limits };
        let mut x = logged(xlim, xlog);
        let mut y = logged(ylim, ylog);
        if asp.is_finite() && asp > 0.0 {
            let pin = device.plot_size();
            let (x_range, y_range) = ((x[1] - x[0]).abs(), (y[1] - y[0]).abs());
            let x_needed = pin[0] * y_range / (asp * pin[1]);
            let (x_range, y_range) = if x_needed >= x_range {
                (x_needed, y_range)
            } else {
                (x_range, x_range * asp * pin[1] / pin[0])
            };
            x = centred(x, x_range);
            y = centred(y, y_range);
        }
        let (x, xaxp) = scale_axis(x, xaxs, xlog, lab[0]);
        let (y, yaxp) = scale_axis(y, yaxs, ylog, lab[1]);
        let pars = &mut device.pars;
        pars.xlog = xlog;
        pars.ylog = ylog;
        pars.usr = [x[0], x[1], y[0], y[1]];
        pars.xaxp = xaxp;
        pars.yaxp = yaxp;
        Ok(())
    })?;
    Ok(nil())
}

/// `limits` widened or narrowed to `range` about their centre, keeping their direction.
fn centred(limits: [f64; 2], range: f64) -> [f64; 2] {
    let centre = (limits[0] + limits[1]) / 2.0;
    let half = if limits[1] >= limits[0] {
        range / 2.0
    } else {
        -range / 2.0
    };
    [centre - half, centre + half]
}

/// The user limits of an axis spanning `limits` (in decades on a log axis) and its tick
/// parameters: equal limits are spread apart, and style `"r"` extends the range by 4% at
/// each end.
fn scale_axis(limits: [f64; 2], style: char, log: bool, intervals: i32) -> ([f64; 2], [f64; 3]) {
    let [mut low, mut high] = limits;
    if low == high {
        if low == 0.0 {
            low = -1.0;
            high = 1.0;
        } else {
            low -= 0.4 * low.abs();
            high += 0.4 * high.abs();
        }
    }
    if style == 'r' || style == 'e' || style == 's' {
        let extension = (high - low) * 0.04;
        low -= extension;
        high += extension;
    }
    let data = if log {
        [10f64.powf(low), 10f64.powf(high)]
    } else {
        [low, high]
    };
    ([low, high], axis_parameters(data, intervals, log))
}

/// `par("xaxp")` for an axis spanning `limits` in data units, in their direction.
fn axis_parameters(limits: [f64; 2], intervals: i32, log: bool) -> [f64; 3] {
    let reversed = limits[0] > limits[1];
    let (low, high) = if reversed {
        (limits[1], limits[0])
    } else {
        (limits[0], limits[1])
    };
    let (first, last, count) = if log {
        log_axis(low, high, intervals)
    } else {
        linear_axis(low, high, intervals)
    };
    if reversed {
        [last, first, f64::from(count)]
    } else {
        [first, last, f64::from(count)]
    }
}

/// `layout(rows, columns, matrix, figures, widths, heights, cm.widths, cm.heights, respect,
/// respect.matrix)`.
unsafe fn do_layout(call: &NativeCall<'_>) -> R<Sexp> {
    let rows = values::as_integer_scalar(call.arg(0));
    let columns = values::as_integer_scalar(call.arg(1));
    if rows < 1 || columns < 1 || rows == NA_INTEGER || columns == NA_INTEGER {
        return Err(error("invalid layout"));
    }
    let (rows, columns) = (rows as usize, columns as usize);
    let matrix = coerce_vector(call.arg(2), SexpType::Integer)?;
    let figures = values::as_integer_scalar(call.arg(3)).max(1) as usize;
    let cells_by_column = values::ints(matrix);
    if cells_by_column.len() != rows * columns {
        return Err(error("invalid layout"));
    }
    let cells: Vec<usize> = (0..rows * columns)
        .map(|cell| {
            let (row, column) = (cell / columns, cell % columns);
            cells_by_column[column * rows + row].max(0) as usize
        })
        .collect();
    let sizes = |index: usize, count: usize| -> R<Vec<f64>> {
        let reals = coerce_vector(call.arg(index), SexpType::Real)?;
        let reals = values::reals(reals);
        Ok((0..count)
            .map(|item| reals.get(item).copied().unwrap_or(1.0))
            .collect())
    };
    let flags = |index: usize, count: usize| -> R<Vec<bool>> {
        let mut flags = vec![false; count];
        let value = call.arg(index);
        if value != nil() {
            let indices = coerce_vector(value, SexpType::Integer)?;
            for &item in values::ints(indices) {
                if item >= 1 && (item as usize) <= count {
                    flags[item as usize - 1] = true;
                }
            }
        }
        Ok(flags)
    };
    let layout = Layout {
        rows,
        columns,
        cells,
        figures,
        widths: sizes(4, columns)?,
        heights: sizes(5, rows)?,
        cm_widths: flags(6, columns)?,
        cm_heights: flags(7, rows)?,
        respect: values::as_integer_scalar(call.arg(8)) != 0,
        current: figures,
    };
    with_device(|device| {
        let pars = &mut device.pars;
        pars.layout = layout;
        pars.figure_region = FigureRegion::Layout;
        pars.cex = layout_cex(rows, columns);
        pars.margin_cex = pars.cex;
        pars.square = pars.pty == 's';
        Ok(())
    })?;
    Ok(nil())
}

/// `axis(side, at, labels, ...)`: the tick positions, `at` or those `par("xaxp")` or
/// `par("yaxp")` gives.
unsafe fn do_axis(call: &NativeCall<'_>) -> R<Sexp> {
    let side = values::as_integer_scalar(call.arg(0));
    if !(1..=4).contains(&side) {
        return Err(error(format!("invalid axis number {side}")));
    }
    let at = call.arg(1);
    let labels = call.arg(2);
    let ticks = with_plot(|device| {
        let pars = &device.pars;
        let horizontal = side % 2 == 1;
        let (axp, usr, log, intervals) = if horizontal {
            (
                pars.xaxp,
                [pars.usr[0], pars.usr[1]],
                pars.xlog,
                pars.lab[0],
            )
        } else {
            (
                pars.yaxp,
                [pars.usr[2], pars.usr[3]],
                pars.ylog,
                pars.lab[1],
            )
        };
        let usr = if log {
            usr.map(|limit| 10f64.powf(limit))
        } else {
            usr
        };
        Ok(at_vector(axp, usr, intervals, log))
    })?;
    let result = if at == nil() {
        values::real_vector(&ticks)?
    } else {
        coerce_vector(at, SexpType::Real)?
    };
    let label_count = length(labels);
    let logical_labels = matches!(labels.kind(), SexpType::Logical | SexpType::Nil);
    if !logical_labels && label_count != length(result) {
        return Err(error(format!(
            "'at' and 'labels' lengths differ, {} != {label_count}",
            length(result)
        )));
    }
    draw_axis(call, side, values::reals(result))?;
    Ok(result)
}

// graphics: text metrics and coordinate conversion.

/// The widths of the printable ASCII characters in Helvetica, in thousandths of the font
/// size, from Adobe's published font metrics. Other characters get the width of a digit.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 222, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// The same widths in Helvetica-Bold. The oblique faces share the widths of their upright ones.
const HELVETICA_BOLD_WIDTHS: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 278, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 278, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// The height of a capital letter in Helvetica, in thousandths of the font size.
const HELVETICA_CAP_HEIGHT: f64 = 718.0;

/// The width of one line of text in ems, in the face `font` (`par("font")` numbering: 2 and 4
/// are bold).
pub(crate) fn text_width_ems(line: &str, font: i32) -> f64 {
    let widths = if font == 2 || font == 4 {
        &HELVETICA_BOLD_WIDTHS
    } else {
        &HELVETICA_WIDTHS
    };
    line.chars()
        .map(|character| {
            let code = character as usize;
            let width = if (32..127).contains(&code) {
                widths[code - 32]
            } else {
                556
            };
            f64::from(width) / 1000.0
        })
        .sum()
}

/// The lines of each label of a `strwidth` or `text` argument: strings line by line, other
/// atomic values as text, and plotmath expressions by their deparsed source.
unsafe fn label_lines(labels: Sexp) -> R<Vec<Vec<String>>> {
    let split = |text: &str| text.split('\n').map(str::to_owned).collect::<Vec<_>>();
    match labels.kind() {
        SexpType::Nil => Ok(Vec::new()),
        SexpType::Expression => {
            let mut result = Vec::with_capacity(length(labels));
            for index in 0..length(labels) {
                result.push(vec![crate::deparse::deparse_one_line(values::list_elt(
                    labels, index,
                ))]);
            }
            Ok(result)
        }
        SexpType::Language | SexpType::Symbol => {
            Ok(vec![vec![crate::deparse::deparse_one_line(labels)]])
        }
        SexpType::String => Ok((0..length(labels))
            .map(|index| match values::string_at(labels, index) {
                Some(text) => split(&text),
                None => Vec::new(),
            })
            .collect()),
        _ => {
            let strings = coerce_vector(labels, SexpType::String)?;
            Ok((0..length(strings))
                .map(|index| {
                    values::string_at(strings, index).map_or_else(Vec::new, |text| split(&text))
                })
                .collect())
        }
    }
}

/// `strwidth` and `strheight`: `(s, units, cex, font, vfont, ...)`, with units 1 for user
/// coordinates, 2 for the figure and 3 for inches.
unsafe fn string_metric(call: &NativeCall<'_>, height: bool) -> R<Sexp> {
    let labels = label_lines(call.arg(0))?;
    let units = values::as_integer_scalar(call.arg(1));
    if !(1..=3).contains(&units) {
        return Err(error("invalid units"));
    }
    let cex_value = call.arg(2);
    let cex = if cex_value == nil() {
        values::na_real()
    } else {
        values::as_real_scalar(cex_value)
    };
    let font_value = call.arg(3);
    let font = if font_value == nil() {
        0
    } else {
        values::as_integer_scalar(font_value)
    };
    let sizes = with_started_plot(|device| {
        let pars = &device.pars;
        let font = if font >= 1 { font } else { pars.font };
        // The `cex` argument scales `par("cex")` rather than replacing it.
        let cex = pars.cex
            * if cex.is_finite() && cex > 0.0 {
                cex
            } else {
                1.0
            };
        let unrounded = f64::from(pars.ps) * cex;
        // Fonts come in whole point sizes, so the glyphs are measured at the nearest one.
        let points = unrounded.round().max(1.0);
        let line_inches = 1.2 * unrounded / 72.0 * pars.lheight;
        let (plot_inches, figure_inches, user_range) = if height {
            (
                device.plot_size()[1],
                device.figure_size()[1],
                pars.usr[3] - pars.usr[2],
            )
        } else {
            (
                device.plot_size()[0],
                device.figure_size()[0],
                pars.usr[1] - pars.usr[0],
            )
        };
        let sizes: Vec<f64> = labels
            .iter()
            .map(|lines| {
                let inches = if height {
                    let extra = lines.len().saturating_sub(1) as f64 * line_inches;
                    extra + HELVETICA_CAP_HEIGHT / 1000.0 * points / 72.0
                } else {
                    lines
                        .iter()
                        .map(|line| text_width_ems(line, font))
                        .fold(0.0, f64::max)
                        * points
                        / 72.0
                };
                match units {
                    1 => inches / plot_inches * user_range,
                    2 => inches / figure_inches,
                    _ => inches,
                }
            })
            .collect();
        Ok(sizes)
    })?;
    values::real_vector(&sizes)
}

unsafe fn do_str_width(call: &NativeCall<'_>) -> R<Sexp> {
    string_metric(call, false)
}

unsafe fn do_str_height(call: &NativeCall<'_>) -> R<Sexp> {
    string_metric(call, true)
}

/// `grconvertX(x, from, to)` and `grconvertY`: the unit codes are positions in
/// `graphics:::.units`.
unsafe fn convert(call: &NativeCall<'_>, vertical: bool) -> R<Sexp> {
    let input = coerce_vector(call.arg(0), SexpType::Real)?;
    let from = values::as_integer_scalar(call.arg(1));
    let to = values::as_integer_scalar(call.arg(2));
    let known = [1, 2, 7, 8, 13, 14, 15, 16, 17];
    if !known.contains(&from) || !known.contains(&to) {
        return Err(error("invalid units"));
    }
    let points = values::reals(input).to_vec();
    let converted = with_device(|device| {
        Ok(points
            .iter()
            .map(|value| {
                let ndc = to_ndc(device, *value, from, vertical);
                from_ndc(device, ndc, to, vertical)
            })
            .collect::<Vec<f64>>())
    })?;
    values::real_vector(&converted)
}

unsafe fn do_convert_x(call: &NativeCall<'_>) -> R<Sexp> {
    convert(call, false)
}

unsafe fn do_convert_y(call: &NativeCall<'_>) -> R<Sexp> {
    convert(call, true)
}

/// The regions a coordinate passes through on its way from user coordinates to the device,
/// each `[low, high]` in the enclosing region's fractions, for one axis.
struct AxisFrames {
    inner: [f64; 2],
    figure: [f64; 2],
    plot: [f64; 2],
    usr: [f64; 2],
    log: bool,
    device: [f64; 2],
    size: f64,
    figure_inches: f64,
    line_inches: f64,
    char_inches: f64,
}

fn axis_frames(device: &Device, vertical: bool) -> AxisFrames {
    let axis = usize::from(vertical);
    let pick = |region: [f64; 4]| [region[2 * axis], region[2 * axis + 1]];
    let extent = device.device_extent();
    AxisFrames {
        inner: pick(device.inner_region()),
        figure: pick(device.figure_region()),
        plot: pick(device.plot_region()),
        usr: pick(device.pars.usr),
        log: if vertical {
            device.pars.ylog
        } else {
            device.pars.xlog
        },
        device: pick(extent),
        size: device.size[axis],
        figure_inches: device.figure_size()[axis],
        line_inches: device.pars.mex * device.line_inches(),
        char_inches: device.char_inches()[1] * device.pars.cex,
    }
}

fn lerp(range: [f64; 2], fraction: f64) -> f64 {
    range[0] + fraction * (range[1] - range[0])
}

fn unlerp(range: [f64; 2], value: f64) -> f64 {
    (value - range[0]) / (range[1] - range[0])
}

/// A coordinate in unit `unit` as a fraction of the device.
fn to_ndc(device: &Device, value: f64, unit: i32, vertical: bool) -> f64 {
    let frames = axis_frames(device, vertical);
    let figure_to_ndc = |fraction: f64| lerp(frames.inner, lerp(frames.figure, fraction));
    match unit {
        1 => unlerp(frames.device, value),
        2 => value,
        7 => lerp(frames.inner, value),
        8 => figure_to_ndc(value),
        13 => {
            let value = if frames.log { value.log10() } else { value };
            figure_to_ndc(lerp(frames.plot, unlerp(frames.usr, value)))
        }
        14 => value / frames.size,
        15 => figure_to_ndc(value * frames.line_inches / frames.figure_inches),
        16 => figure_to_ndc(value * frames.char_inches / frames.figure_inches),
        _ => figure_to_ndc(lerp(frames.plot, value)),
    }
}

fn from_ndc(device: &Device, ndc: f64, unit: i32, vertical: bool) -> f64 {
    let frames = axis_frames(device, vertical);
    let ndc_to_figure = |ndc: f64| unlerp(frames.figure, unlerp(frames.inner, ndc));
    match unit {
        1 => lerp(frames.device, ndc),
        2 => ndc,
        7 => unlerp(frames.inner, ndc),
        8 => ndc_to_figure(ndc),
        13 => {
            let value = lerp(frames.usr, unlerp(frames.plot, ndc_to_figure(ndc)));
            if frames.log { 10f64.powf(value) } else { value }
        }
        14 => ndc * frames.size,
        15 => ndc_to_figure(ndc) * frames.figure_inches / frames.line_inches,
        16 => ndc_to_figure(ndc) * frames.figure_inches / frames.char_inches,
        _ => unlerp(frames.plot, ndc_to_figure(ndc)),
    }
}

// graphics: perspective plots and dendrograms.

type Matrix4 = [[f64; 4]; 4];

fn multiply(left: &Matrix4, right: &Matrix4) -> Matrix4 {
    let mut product = [[0.0; 4]; 4];
    for (row, product_row) in product.iter_mut().enumerate() {
        for (column, cell) in product_row.iter_mut().enumerate() {
            *cell = (0..4)
                .map(|inner| left[row][inner] * right[inner][column])
                .sum();
        }
    }
    product
}

fn identity() -> Matrix4 {
    let mut matrix = [[0.0; 4]; 4];
    for (index, row) in matrix.iter_mut().enumerate() {
        row[index] = 1.0;
    }
    matrix
}

fn translation(offset: [f64; 3]) -> Matrix4 {
    let mut matrix = identity();
    matrix[3][..3].copy_from_slice(&offset);
    matrix
}

fn scaling(factors: [f64; 3]) -> Matrix4 {
    let mut matrix = identity();
    for (index, factor) in factors.iter().enumerate() {
        matrix[index][index] = *factor;
    }
    matrix
}

/// A rotation of row vectors by `degrees` about the x axis, or about the y axis.
fn rotation(degrees: f64, about_y: bool) -> Matrix4 {
    let (sine, cosine) = degrees.to_radians().sin_cos();
    let mut matrix = identity();
    if about_y {
        matrix[0][0] = cosine;
        matrix[0][2] = -sine;
        matrix[2][0] = sine;
        matrix[2][2] = cosine;
    } else {
        matrix[1][1] = cosine;
        matrix[1][2] = sine;
        matrix[2][1] = -sine;
        matrix[2][2] = cosine;
    }
    matrix
}

/// `persp(x, y, z, xlim, ylim, zlim, theta, phi, r, d, scale, expand, ...)`: the viewing
/// transformation `persp` documents, which maps the box of the limits, scaled to a cube about
/// the origin, through the rotations and the perspective projection. The plot window fits the
/// projected box at aspect ratio 1.
unsafe fn do_persp(call: &NativeCall<'_>) -> R<Sexp> {
    let limits = |index: usize| -> R<[f64; 2]> {
        let reals = coerce_vector(call.arg(index), SexpType::Real)?;
        let reals = values::reals(reals);
        if reals.len() != 2 || reals.iter().any(|limit| !limit.is_finite()) {
            return Err(error("invalid 'x', 'y' or 'z' limits"));
        }
        Ok([reals[0], reals[1]])
    };
    let boxes = [limits(3)?, limits(4)?, limits(5)?];
    let scalar = |index: usize| values::as_real_scalar(call.arg(index));
    let (theta, phi, distance, viewing) = (scalar(6), scalar(7), scalar(8), scalar(9));
    let scale = values::as_logical_scalar(call.arg(10)) == 1;
    let expand = scalar(11);
    if !(viewing > 0.0) || !(distance.is_finite()) || !expand.is_finite() {
        return Err(error("invalid viewing parameters"));
    }
    let centre = boxes.map(|limits| (limits[0] + limits[1]) / 2.0);
    let mut half = boxes.map(|limits| (limits[1] - limits[0]) / 2.0);
    if !scale {
        let largest = half.iter().copied().fold(0.0, f64::max);
        half = [largest; 3];
    }
    if half.iter().any(|half| *half == 0.0) {
        return Err(error("invalid 'x', 'y' or 'z' limits"));
    }
    let mut perspective = identity();
    perspective[2][3] = -1.0 / viewing;
    let steps = [
        translation(centre.map(|centre| -centre)),
        scaling([1.0 / half[0], 1.0 / half[1], expand / half[2]]),
        rotation(-90.0, false),
        rotation(-theta, true),
        rotation(phi, false),
        translation([0.0, 0.0, -distance - viewing]),
        perspective,
    ];
    let transform = steps
        .iter()
        .fold(identity(), |product, step| multiply(&product, step));
    let mut projected_x = [f64::INFINITY, f64::NEG_INFINITY];
    let mut projected_y = [f64::INFINITY, f64::NEG_INFINITY];
    for corner in 0..8 {
        let point = [
            boxes[0][corner & 1],
            boxes[1][(corner >> 1) & 1],
            boxes[2][(corner >> 2) & 1],
            1.0,
        ];
        let image: Vec<f64> = (0..4)
            .map(|column| (0..4).map(|row| point[row] * transform[row][column]).sum())
            .collect();
        let (x, y) = (image[0] / image[3], image[1] / image[3]);
        projected_x = [projected_x[0].min(x), projected_x[1].max(x)];
        projected_y = [projected_y[0].min(y), projected_y[1].max(y)];
    }
    with_device(|device| {
        if !device.started {
            return Err(error("plot.new has not been called yet"));
        }
        set_window(device, projected_x, projected_y, 1.0);
        device.pars.new = false;
        Ok(())
    })?;
    let result = alloc(SexpType::Real, 16)?;
    let cells = values::reals_mut(result);
    for column in 0..4 {
        for row in 0..4 {
            cells[column * 4 + row] = transform[row][column];
        }
    }
    let dim = values::integer_vector(&[4, 4])?;
    values::set_attr(result, symbols().dim, dim)?;
    Ok(result)
}

/// The plot window over linear limits with aspect ratio `asp` and the current axis styles.
fn set_window(device: &mut Device, xlim: [f64; 2], ylim: [f64; 2], asp: f64) {
    let pin = device.plot_size();
    let (x_range, y_range) = ((xlim[1] - xlim[0]).abs(), (ylim[1] - ylim[0]).abs());
    let x_needed = pin[0] * y_range / (asp * pin[1]);
    let (x_range, y_range) = if x_needed >= x_range {
        (x_needed, y_range)
    } else {
        (x_range, x_range * asp * pin[1] / pin[0])
    };
    let pars = &mut device.pars;
    let (x, xaxp) = scale_axis(centred(xlim, x_range), pars.xaxs, false, pars.lab[0]);
    let (y, yaxp) = scale_axis(centred(ylim, y_range), pars.yaxs, false, pars.lab[1]);
    pars.xlog = false;
    pars.ylog = false;
    pars.usr = [x[0], x[1], y[0], y[1]];
    pars.xaxp = xaxp;
    pars.yaxp = yaxp;
}

/// `C_dendwindow(n, merge, height, hang, labels, ...)`: the plot window of a dendrogram of
/// `n` merges, its leaves at 1 to `n + 1` and its heights from the lowest hanging leaf to the
/// root. GNU R also leaves room below for the leaf labels, which ry does not measure.
unsafe fn do_dend_window(call: &NativeCall<'_>) -> R<Sexp> {
    let leaves = values::as_integer_scalar(call.arg(0)).max(0) + 1;
    let heights = coerce_vector(call.arg(2), SexpType::Real)?;
    let heights: Vec<f64> = values::reals(heights)
        .iter()
        .copied()
        .filter(|height| height.is_finite())
        .collect();
    let hang = values::as_real_scalar(call.arg(3));
    let top = heights.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let bottom = heights.iter().copied().fold(f64::INFINITY, f64::min);
    let (bottom, top) = if top.is_finite() {
        (bottom, top)
    } else {
        (0.0, 1.0)
    };
    let bottom = if hang.is_finite() && hang >= 0.0 {
        bottom - hang * (top - bottom)
    } else {
        0.0_f64.min(bottom)
    };
    with_started_plot(|device| {
        let pars = &mut device.pars;
        let (x, xaxp) = scale_axis([1.0, f64::from(leaves)], pars.xaxs, false, pars.lab[0]);
        let (y, yaxp) = scale_axis([bottom, top], pars.yaxs, false, pars.lab[1]);
        pars.xlog = false;
        pars.ylog = false;
        pars.usr = [x[0], x[1], y[0], y[1]];
        pars.xaxp = xaxp;
        pars.yaxp = yaxp;
        Ok(())
    })?;
    Ok(nil())
}

/// `xspline(x, y, shape, open, repEnds, draw, ...)`: without drawing, the points of the
/// spline, which ry approximates by its control points.
unsafe fn do_xspline(call: &NativeCall<'_>) -> R<Sexp> {
    with_plot(|_| Ok(()))?;
    if values::as_logical_scalar(call.arg(5)) == 1 {
        return Ok(nil());
    }
    let x = coerce_vector(call.arg(0), SexpType::Real)?;
    crate::heap().root(x);
    let y = coerce_vector(call.arg(1), SexpType::Real)?;
    crate::heap().root(y);
    values::named_list(&[("x", x), ("y", y)])
}

/// `locator` and `identify` on a device without a pointer: nothing is located.
unsafe fn do_locator(_: &NativeCall<'_>) -> R<Sexp> {
    with_device(|_| Ok(()))?;
    Ok(nil())
}

unsafe fn do_identify(_: &NativeCall<'_>) -> R<Sexp> {
    with_device(|_| Ok(()))?;
    values::logical_vector(&[])
}

// graphics: drawing.
//
// Drawing routines record shapes on the device's current page in inches from its
// bottom-left corner. Positions follow GNU R's base graphics: a margin line is
// `mex * csi` inches, text sits on a margin line by its baseline, offset by `ylbias`, and
// symbols are sized from half the character height.

/// Runs a drawing routine with the inline graphical parameters among a call's arguments after
/// its first `fixed` ones in force, as GNU R's high-level and low-level plotting functions do,
/// and restores the device's parameters afterwards.
unsafe fn with_inline_pars<T>(
    call: &NativeCall<'_>,
    fixed: usize,
    action: impl FnOnce(&mut Device) -> R<T>,
) -> R<T> {
    let mut warnings = Vec::new();
    let result = with_plot(|device| {
        let saved = device.pars.clone();
        let mut outcome = Ok(());
        for arg in call.args.iter().skip(fixed) {
            if arg.tag.kind() != SexpType::Symbol || arg.value == nil() {
                continue;
            }
            let name = values::symbol_name(arg.tag);
            // High-level plotting functions pass their own arguments on through `...`, so
            // GNU R lets these names by without a warning.
            let plot_argument = matches!(
                name.as_ref(),
                "xlim" | "ylim" | "asp" | "main" | "sub" | "xlab" | "ylab"
            );
            if plot_argument
                || REGION_PARS.contains(&name.as_ref())
                || matches!(name.as_ref(), "new" | "usr" | "xlog" | "ylog" | "ask")
            {
                continue;
            }
            if get_par(device, &name).is_none() {
                warnings.push(format!("\"{name}\" is not a graphical parameter"));
                continue;
            }
            if READ_ONLY_PARS.contains(&name.as_ref()) {
                continue;
            }
            if let Err(failure) = set_par(device, &name, arg.value, &mut warnings) {
                outcome = Err(failure);
                break;
            }
        }
        let result = outcome.and_then(|()| action(device));
        device.pars = saved;
        result
    });
    for warning in warnings {
        call.warning(warning)?;
    }
    result
}

/// A region of the current figure in device inches: the inner region (unit 7), the figure
/// (8) or the plot region (17), as `grconvertX` numbers them.
fn region_rect(device: &Device, unit: i32) -> Rect {
    let [width, height] = device.size;
    Rect {
        x0: to_ndc(device, 0.0, unit, false) * width,
        x1: to_ndc(device, 1.0, unit, false) * width,
        y0: to_ndc(device, 0.0, unit, true) * height,
        y1: to_ndc(device, 1.0, unit, true) * height,
    }
}

fn device_rect(device: &Device) -> Rect {
    Rect {
        x0: 0.0,
        x1: device.size[0],
        y0: 0.0,
        y1: device.size[1],
    }
}

/// Where drawing is clipped: the region `clip()` set, or else the plot region, the figure or
/// the whole device as `xpd` is `FALSE`, `TRUE` or `NA`.
fn clip_rect(device: &Device) -> Rect {
    if let Some(clip) = device.clip {
        return clip;
    }
    match device.pars.xpd {
        0 => region_rect(device, 17),
        1 => region_rect(device, 8),
        _ => device_rect(device),
    }
}

/// User coordinates to device inches on one axis, which is linear in the coordinate, or in
/// its logarithm on a log axis.
#[derive(Clone, Copy)]
struct UserAxis {
    origin: f64,
    scale: f64,
    log: bool,
}

impl UserAxis {
    fn inches(self, value: f64) -> f64 {
        let value = if self.log { value.log10() } else { value };
        self.origin + self.scale * value
    }
}

fn user_axes(device: &Device) -> [UserAxis; 2] {
    [false, true].map(|vertical| {
        let frames = axis_frames(device, vertical);
        let ndc = |fraction: f64| lerp(frames.inner, lerp(frames.figure, lerp(frames.plot, fraction)));
        let size = device.size[usize::from(vertical)];
        let [low, high] = frames.usr;
        let (start, end) = (ndc(0.0) * size, ndc(1.0) * size);
        let scale = (end - start) / (high - low);
        UserAxis {
            origin: start - scale * low,
            scale,
            log: frames.log,
        }
    })
}

/// The coordinate in inches, across side `side`, of margin line `line`: lines count outwards
/// from the plot region, or from the inner region in the outer margins.
fn margin_line(device: &Device, side: i32, line: f64, outer: bool) -> f64 {
    let region = region_rect(device, if outer { 7 } else { 17 });
    let step = device.pars.mex * device.line_inches();
    match side {
        1 => region.y0 - line * step,
        2 => region.x0 - line * step,
        3 => region.y1 + line * step,
        _ => region.x1 + line * step,
    }
}

/// The coordinate in inches along side `side` of `at`, in user coordinates, or as a fraction
/// of the inner region in the outer margins.
fn along_side(device: &Device, side: i32, at: f64, outer: bool) -> f64 {
    let horizontal = side % 2 == 1;
    if outer {
        let inner = region_rect(device, 7);
        return if horizontal {
            lerp([inner.x0, inner.x1], at)
        } else {
            lerp([inner.y0, inner.y1], at)
        };
    }
    let [x, y] = user_axes(device);
    if horizontal { x.inches(at) } else { y.inches(at) }
}

fn push_shape(device: &mut Device, clip: Rect, shape: Shape) {
    if device.pages.is_empty() {
        device.pages.push(Page {
            background: device.pars.bg,
            items: Vec::new(),
        });
    }
    if let Some(page) = device.pages.last_mut() {
        page.items.push(Item { clip, shape });
    }
}

fn stroke_with(pars: &Pars, colour: Colour, lty: u32, lwd: f64) -> Stroke {
    Stroke {
        colour,
        lwd,
        lty,
        lend: pars.lend,
        ljoin: pars.ljoin,
        lmitre: pars.lmitre,
    }
}

fn par_stroke(pars: &Pars, colour: Colour) -> Stroke {
    stroke_with(pars, colour, pars.lty, pars.lwd)
}

fn push_line(device: &mut Device, clip: Rect, points: Vec<[f64; 2]>, stroke: Stroke) {
    if points.len() >= 2 {
        push_shape(
            device,
            clip,
            Shape::Path {
                points,
                closed: false,
                fill: TRANSPARENT_WHITE,
                stroke: Some(stroke),
            },
        );
    }
}

/// The size of text at character expansion `cex` in points, rounded to a whole point as the
/// fonts are.
fn text_points(pars: &Pars, cex: f64) -> f64 {
    (f64::from(pars.ps) * cex).round().max(1.0)
}

/// Records the lines of a label anchored at `anchor`. `hadj` places the anchor along each
/// line, 0 at its start and 1 at its end; `yadj` places it across the block of lines, 0 on
/// the last line's baseline and 1 at the top of the first line's capitals.
#[allow(clippy::too_many_arguments)]
fn push_text(
    device: &mut Device,
    clip: Rect,
    anchor: [f64; 2],
    lines: &[String],
    rotation: f64,
    hadj: f64,
    yadj: f64,
    cex: f64,
    font: i32,
    colour: Colour,
) {
    let points = text_points(&device.pars, cex);
    let line_height = 1.2 * f64::from(device.pars.ps) * cex / 72.0 * device.pars.lheight;
    let cap = HELVETICA_CAP_HEIGHT / 1000.0 * points / 72.0;
    let (sin, cos) = rotation.to_radians().sin_cos();
    let count = lines.len();
    for (index, line) in lines.iter().enumerate() {
        let up = ((count - 1) as f64 * (1.0 - yadj) - index as f64) * line_height - yadj * cap;
        push_shape(
            device,
            clip,
            Shape::Text {
                origin: [anchor[0] - up * sin, anchor[1] + up * cos],
                text: line.clone(),
                points,
                font,
                rotation,
                hadj,
                colour,
            },
        );
    }
}

/// The width of a line of text in inches.
fn text_inches(pars: &Pars, line: &str, cex: f64, font: i32) -> f64 {
    text_width_ems(line, font) * text_points(pars, cex) / 72.0
}

/// Places a label in a margin as `mtext` and `axis` do: on line `line` of side `side` at
/// `along` inches, reading parallel to the side unless `las` turns it, with the baseline
/// offset by `ylbias` so the text sits within the line.
#[allow(clippy::too_many_arguments)]
fn push_margin_text(
    device: &mut Device,
    clip: Rect,
    side: i32,
    line: f64,
    outer: bool,
    along: f64,
    lines: &[String],
    las: i32,
    hadj: f64,
    padj: f64,
    cex: f64,
    font: i32,
    colour: Colour,
) {
    let bias = device.pars.ylbias;
    let perpendicular = match side {
        1 | 3 => las == 2 || las == 3,
        _ => las == 1 || las == 2,
    };
    let (line, rotation) = match side {
        1 if perpendicular => (line, 90.0),
        1 => (line + 1.0 - bias, 0.0),
        2 if perpendicular => (line, 0.0),
        2 => (line + bias, 90.0),
        3 if perpendicular => (line, 90.0),
        3 => (line + bias, 0.0),
        _ if perpendicular => (line, 0.0),
        _ => (line + 1.0 - bias, 90.0),
    };
    let across = margin_line(device, side, line, outer);
    let anchor = if side % 2 == 1 {
        [along, across]
    } else {
        [across, along]
    };
    push_text(
        device, clip, anchor, lines, rotation, hadj, padj, cex, font, colour,
    );
}

/// The elements of a vector argument, recycled: `default` when it is `NULL` or empty.
unsafe fn real_at(value: Sexp, index: usize, default: f64) -> f64 {
    let count = length(value);
    if value == nil() || count == 0 || !values::is_numeric_type(value.kind()) {
        return default;
    }
    crate::subset::numeric_element(value, index % count)
}

unsafe fn colour_at(value: Sexp, index: usize, default: Colour, background: Colour) -> R<Colour> {
    let count = length(value);
    if value == nil() || count == 0 {
        return Ok(default);
    }
    let index = index % count;
    let zero = match value.kind() {
        SexpType::String => values::string_at(value, index).as_deref() == Some("0"),
        SexpType::Integer | SexpType::Logical | SexpType::Real => {
            crate::subset::numeric_element(value, index) == 0.0
        }
        _ => false,
    };
    if zero {
        return Ok(background);
    }
    colour_element(value, index)
}

/// A colour for text and axes, where an `NA` element means `default` rather than transparent.
unsafe fn text_colour_at(
    value: Sexp,
    index: usize,
    default: Colour,
    background: Colour,
) -> R<Colour> {
    let count = length(value);
    let missing = count > 0
        && match value.kind() {
            SexpType::Logical | SexpType::Integer => values::ints(value)[index % count] == NA_INTEGER,
            SexpType::Real => values::reals(value)[index % count].is_nan(),
            SexpType::String => values::string_at(value, index % count).is_none(),
            _ => false,
        };
    if missing {
        return Ok(default);
    }
    colour_at(value, index, default, background)
}

unsafe fn line_type_at(value: Sexp, index: usize, default: u32) -> R<u32> {
    let count = length(value);
    if value == nil() || count == 0 {
        return Ok(default);
    }
    let index = index % count;
    if value.kind() == SexpType::String {
        let Some(text) = values::string_at(value, index) else {
            return Ok(default);
        };
        if let Some((_, code)) = LINE_TYPES.iter().find(|(name, _)| *name == &*text) {
            return Ok(*code);
        }
        return parse_dash_pattern(&text).map_err(error);
    }
    let number = crate::subset::numeric_element(value, index);
    if number.is_nan() {
        return Ok(default);
    }
    if number < 0.0 {
        return Err(error("invalid line type"));
    }
    let code = number as usize;
    Ok(if code == 0 {
        LTY_BLANK
    } else {
        LINE_TYPES[(code - 1) % 6 + 1].1
    })
}

/// The plotting symbol at `index` of a `pch` argument.
unsafe fn pch_at(value: Sexp, index: usize, default: &Pch) -> Pch {
    let count = length(value);
    if value == nil() || count == 0 {
        return default.clone();
    }
    let index = index % count;
    if value.kind() == SexpType::String {
        return match values::string_at(value, index) {
            Some(text) => Pch::Text(text.chars().next().map_or_else(String::new, String::from)),
            None => Pch::Code(NA_INTEGER),
        };
    }
    let number = crate::subset::numeric_element(value, index);
    Pch::Code(if number.is_nan() {
        NA_INTEGER
    } else {
        number as i32
    })
}

unsafe fn reals_of(value: Sexp) -> R<Vec<f64>> {
    if value == nil() {
        return Ok(Vec::new());
    }
    let reals = coerce_vector(value, SexpType::Real)?;
    Ok(values::reals(reals).to_vec())
}

/// The `x` and `y` components of an `xy.coords` list.
unsafe fn xy_of(xy: Sexp) -> R<(Vec<f64>, Vec<f64>)> {
    if xy.kind() != SexpType::List || length(xy) < 2 {
        return Err(error("invalid plotting structure"));
    }
    let x = reals_of(values::list_elt(xy, 0))?;
    let y = reals_of(values::list_elt(xy, 1))?;
    if x.len() != y.len() {
        return Err(error("'x' and 'y' lengths differ in plot.xy()"));
    }
    Ok((x, y))
}

/// Records plotting symbol `pch` centred at `centre`. `size` is half the character height
/// in inches at the symbol's `cex`, from which every symbol is scaled: the circle's radius is
/// 0.375 of it, and the square, triangle and diamond have the circle's area.
#[allow(clippy::too_many_arguments)]
fn push_symbol(
    device: &mut Device,
    clip: Rect,
    centre: [f64; 2],
    pch: &Pch,
    size: f64,
    cex: f64,
    colour: Colour,
    fill: Colour,
    lwd: f64,
) {
    let code = match pch {
        Pch::Text(text) if text == "." => {
            let half = 0.005 * cex;
            let square = vec![
                [centre[0] - half, centre[1] - half],
                [centre[0] + half, centre[1] - half],
                [centre[0] + half, centre[1] + half],
                [centre[0] - half, centre[1] + half],
            ];
            push_shape(
                device,
                clip,
                Shape::Path {
                    points: square,
                    closed: true,
                    fill: colour,
                    stroke: None,
                },
            );
            return;
        }
        Pch::Text(text) => {
            if text.is_empty() {
                return;
            }
            let font = device.pars.font;
            push_text(
                device,
                clip,
                centre,
                std::slice::from_ref(text),
                0.0,
                0.5,
                0.5,
                cex * device.pars.cex,
                font,
                colour,
            );
            return;
        }
        Pch::Code(code) => *code,
    };
    if code == NA_INTEGER {
        return;
    }
    if (32..=127).contains(&code)
        && let Some(character) = char::from_u32(code as u32)
    {
        let pch = Pch::Text(character.to_string());
        push_symbol(device, clip, centre, &pch, size, cex, colour, fill, lwd);
        return;
    }
    let stroke = stroke_with(&device.pars, colour, LTY_SOLID, lwd);
    let radius = 0.375 * size;
    let [x, y] = centre;
    let square_half = radius * std::f64::consts::PI.sqrt() / 2.0;
    let triangle_radius = radius * (4.0 * std::f64::consts::PI / (3.0 * 3f64.sqrt())).sqrt();
    let diamond_half = radius * (std::f64::consts::PI / 2.0).sqrt();
    let square = |half: f64| {
        vec![
            [x - half, y - half],
            [x + half, y - half],
            [x + half, y + half],
            [x - half, y + half],
        ]
    };
    // An equilateral triangle about the centre, its apex up or down.
    let triangle = |up: bool| {
        let r = if up { triangle_radius } else { -triangle_radius };
        let half_base = triangle_radius * 3f64.sqrt() / 2.0;
        vec![[x, y + r], [x + half_base, y - r / 2.0], [x - half_base, y - r / 2.0]]
    };
    let diamond = |half: f64| vec![[x, y - half], [x + half, y], [x, y + half], [x - half, y]];
    let polygon = |points: Vec<[f64; 2]>, fill: Colour, stroke: Option<Stroke>| Shape::Path {
        points,
        closed: true,
        fill,
        stroke,
    };
    let circle = |radius: f64, fill: Colour, stroke: Option<Stroke>| Shape::Circle {
        centre,
        radius,
        fill,
        stroke,
    };
    let plus = |half: f64| [vec![[x - half, y], [x + half, y]], vec![[x, y - half], [x, y + half]]];
    let cross = |half: f64| {
        [
            vec![[x - half, y - half], [x + half, y + half]],
            vec![[x - half, y + half], [x + half, y - half]],
        ]
    };
    let none = TRANSPARENT_WHITE;
    let mut shapes: Vec<Shape> = Vec::new();
    let mut lines: Vec<Vec<[f64; 2]>> = Vec::new();
    match code {
        0 => shapes.push(polygon(square(square_half), none, Some(stroke))),
        1 => shapes.push(circle(radius, none, Some(stroke))),
        2 => shapes.push(polygon(triangle(true), none, Some(stroke))),
        3 => lines.extend(plus(radius * std::f64::consts::SQRT_2)),
        4 => lines.extend(cross(radius)),
        5 => shapes.push(polygon(diamond(diamond_half), none, Some(stroke))),
        6 => shapes.push(polygon(triangle(false), none, Some(stroke))),
        7 => {
            shapes.push(polygon(square(radius), none, Some(stroke)));
            lines.extend(cross(radius));
        }
        8 => {
            lines.extend(plus(radius * std::f64::consts::SQRT_2));
            lines.extend(cross(radius));
        }
        9 => {
            shapes.push(polygon(diamond(radius * std::f64::consts::SQRT_2), none, Some(stroke)));
            lines.extend(plus(radius * std::f64::consts::SQRT_2));
        }
        10 => {
            shapes.push(circle(radius, none, Some(stroke)));
            lines.extend(plus(radius));
        }
        11 => {
            let r = triangle_radius;
            let half_base = r * 3f64.sqrt() / 2.0;
            let shift = r / 4.0;
            shapes.push(polygon(
                vec![[x, y + r - shift], [x + half_base, y - r / 2.0 - shift], [x - half_base, y - r / 2.0 - shift]],
                none,
                Some(stroke),
            ));
            shapes.push(polygon(
                vec![[x, y - r + shift], [x + half_base, y + r / 2.0 + shift], [x - half_base, y + r / 2.0 + shift]],
                none,
                Some(stroke),
            ));
        }
        12 => {
            shapes.push(polygon(square(radius), none, Some(stroke)));
            lines.extend(plus(radius));
        }
        13 => {
            shapes.push(circle(radius, none, Some(stroke)));
            lines.extend(cross(radius));
        }
        14 => {
            shapes.push(polygon(square(radius), none, Some(stroke)));
            lines.push(vec![[x - radius, y - radius], [x, y + radius], [x + radius, y - radius]]);
        }
        15 => shapes.push(polygon(square(square_half), colour, None)),
        16 => shapes.push(circle(radius, colour, None)),
        17 => shapes.push(polygon(triangle(true), colour, None)),
        18 => shapes.push(polygon(diamond(radius), colour, None)),
        19 => shapes.push(circle(radius, colour, Some(stroke))),
        20 => shapes.push(circle(radius * 2.0 / 3.0, colour, Some(stroke))),
        21 => shapes.push(circle(radius, fill, Some(stroke))),
        22 => shapes.push(polygon(square(square_half), fill, Some(stroke))),
        23 => shapes.push(polygon(diamond(diamond_half), fill, Some(stroke))),
        24 => shapes.push(polygon(triangle(true), fill, Some(stroke))),
        25 => shapes.push(polygon(triangle(false), fill, Some(stroke))),
        _ => {}
    }
    for shape in shapes {
        push_shape(device, clip, shape);
    }
    for line in lines {
        push_line(device, clip, line, stroke);
    }
}

/// Half the character height in inches at the parameters' `cex`, the unit symbols scale by.
fn symbol_size(device: &Device) -> f64 {
    device.char_inches()[1] / 2.0 * device.pars.cex
}

/// `plot.xy(xy, type, pch, lty, col, bg, cex, lwd, ...)`.
unsafe fn do_plot_xy(call: &NativeCall<'_>) -> R<Sexp> {
    let pch = call.arg(2);
    if pch.kind() == SexpType::Logical && values::ints(pch).iter().any(|flag| *flag != NA_INTEGER) {
        return Err(error("only NA allowed in logical plotting symbol"));
    }
    let (x, y) = xy_of(call.arg(0))?;
    let kind = values::first_string(call.arg(1))
        .and_then(|kind| kind.chars().next())
        .unwrap_or('p');
    let (lty, col, bg, cex, lwd) = (call.arg(3), call.arg(4), call.arg(5), call.arg(6), call.arg(7));
    with_inline_pars(call, 8, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let points: Vec<Option<[f64; 2]>> = x
            .iter()
            .zip(&y)
            .map(|(x, y)| {
                let point = [x_axis.inches(*x), y_axis.inches(*y)];
                point.iter().all(|value| value.is_finite()).then_some(point)
            })
            .collect();
        let line_colour = colour_at(col, 0, pars.col, pars.bg)?;
        let line_stroke = stroke_with(
            &pars,
            line_colour,
            line_type_at(lty, 0, pars.lty)?,
            real_at(lwd, 0, pars.lwd),
        );
        // Lines break at missing points.
        let runs = || {
            points
                .split(Option::is_none)
                .filter(|run| run.len() >= 2)
                .map(|run| run.iter().flatten().copied().collect::<Vec<_>>())
        };
        match kind {
            'l' | 'o' => {
                for run in runs() {
                    push_line(device, clip, run, line_stroke);
                }
            }
            'b' | 'c' => {
                // Each segment stops short of its points by half a character width, leaving
                // room for the symbols.
                let gap = device.char_inches()[0] / 2.0 * pars.cex;
                for run in runs() {
                    for pair in run.windows(2) {
                        let [from, to] = [pair[0], pair[1]];
                        let length = ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2)).sqrt();
                        if length > 2.0 * gap {
                            let unit = [(to[0] - from[0]) / length, (to[1] - from[1]) / length];
                            push_line(
                                device,
                                clip,
                                vec![
                                    [from[0] + gap * unit[0], from[1] + gap * unit[1]],
                                    [to[0] - gap * unit[0], to[1] - gap * unit[1]],
                                ],
                                line_stroke,
                            );
                        }
                    }
                }
            }
            's' | 'S' => {
                for run in runs() {
                    let mut steps = Vec::with_capacity(2 * run.len());
                    for pair in run.windows(2) {
                        let [from, to] = [pair[0], pair[1]];
                        steps.push(from);
                        steps.push(if kind == 's' { [to[0], from[1]] } else { [from[0], to[1]] });
                    }
                    if let Some(last) = run.last() {
                        steps.push(*last);
                    }
                    push_line(device, clip, steps, line_stroke);
                }
            }
            'h' => {
                let base = y_axis.inches(if pars.ylog { 10f64.powf(pars.usr[2]) } else { 0.0 });
                for (index, point) in points.iter().enumerate() {
                    if let Some(point) = point {
                        let stroke = stroke_with(
                            &pars,
                            colour_at(col, index, pars.col, pars.bg)?,
                            line_type_at(lty, index, pars.lty)?,
                            real_at(lwd, index, pars.lwd),
                        );
                        push_line(device, clip, vec![[point[0], base], *point], stroke);
                    }
                }
            }
            _ => {}
        }
        if matches!(kind, 'p' | 'b' | 'o') {
            let size = symbol_size(device);
            for (index, point) in points.iter().enumerate() {
                let Some(point) = point else { continue };
                let colour = colour_at(col, index, pars.col, pars.bg)?;
                let fill = colour_at(bg, index, TRANSPARENT_WHITE, pars.bg)?;
                let symbol_cex = real_at(cex, index, 1.0);
                if !symbol_cex.is_finite() {
                    continue;
                }
                push_symbol(
                    device,
                    clip,
                    *point,
                    &pch_at(pch, index, &pars.pch),
                    size * symbol_cex,
                    symbol_cex,
                    colour,
                    fill,
                    real_at(lwd, index, pars.lwd),
                );
            }
        }
        Ok(())
    })?;
    Ok(nil())
}

/// The labels of an axis: `at` formatted together as GNU R prints numbers, or the given labels
/// as text.
unsafe fn axis_labels(labels: Sexp, at: &[f64]) -> R<Option<Vec<Vec<String>>>> {
    match labels.kind() {
        SexpType::Logical | SexpType::Nil => {
            if labels != nil() && values::as_logical_scalar(labels) == 0 {
                return Ok(None);
            }
            let settings = crate::format::Settings::current();
            let finite: Vec<f64> = at.iter().copied().filter(|value| value.is_finite()).collect();
            let format = crate::format::format_real_common(&finite, settings);
            Ok(Some(
                at.iter()
                    .map(|value| vec![crate::format::format_real_with(*value, format, "NA").trim().to_owned()])
                    .collect(),
            ))
        }
        _ => Ok(Some(label_lines(labels)?)),
    }
}

/// Draws `axis(side, at, labels, tick, line, pos, outer, font, lty, lwd, lwd.ticks, col,
/// col.ticks, hadj, padj, gap.axis, ...)` for ticks `at` in user coordinates.
unsafe fn draw_axis(call: &NativeCall<'_>, side: i32, at: &[f64]) -> R<()> {
    let labels = axis_labels(call.arg(2), at)?;
    let tick = values::as_logical_scalar(call.arg(3)) != 0;
    let line = values::as_real_scalar(call.arg(4));
    let pos = values::as_real_scalar(call.arg(5));
    let outer = values::as_logical_scalar(call.arg(6)) == 1;
    let (font, lty, lwd, lwd_ticks, col, col_ticks) = (
        call.arg(7),
        call.arg(8),
        call.arg(9),
        call.arg(10),
        call.arg(11),
        call.arg(12),
    );
    let hadj = values::as_real_scalar(call.arg(13));
    let padj = values::as_real_scalar(call.arg(14));
    let gap_axis = values::as_real_scalar(call.arg(15));
    with_inline_pars(call, 16, |device| {
        let pars = device.pars.clone();
        let horizontal = side % 2 == 1;
        let [x_axis, y_axis] = user_axes(device);
        let axis = if horizontal { x_axis } else { y_axis };
        let usr = if horizontal {
            [pars.usr[0], pars.usr[1]]
        } else {
            [pars.usr[2], pars.usr[3]]
        };
        let (low, high) = (usr[0].min(usr[1]), usr[0].max(usr[1]));
        let fuzz = 1e-7 * (high - low).abs();
        let logged = |value: f64| if axis.log { value.log10() } else { value };
        let shown: Vec<(usize, f64)> = at
            .iter()
            .enumerate()
            .filter(|(_, value)| {
                let value = logged(**value);
                value.is_finite() && value >= low - fuzz && value <= high + fuzz
            })
            .map(|(index, value)| (index, axis.inches(*value)))
            .collect();
        let clip = device_rect(device);
        let line = if line.is_finite() { line } else { 0.0 };
        // The axis line sits at `pos` in the other axis's user coordinates, or on margin line
        // `line`; labels sit `mgp[2]` lines further out.
        let (axis_across, label_line) = if pos.is_finite() {
            let across = if horizontal { y_axis.inches(pos) } else { x_axis.inches(pos) };
            let edge = margin_line(device, side, 0.0, outer);
            let step = pars.mex * device.line_inches();
            let offset = match side {
                1 | 2 => (edge - across) / step,
                _ => (across - edge) / step,
            };
            (across, offset + pars.mgp[1])
        } else {
            (margin_line(device, side, line + pars.mgp[2], outer), line + pars.mgp[1])
        };
        let axis_colour = text_colour_at(col, 0, pars.fg, pars.bg)?;
        let ticks_colour = text_colour_at(col_ticks, 0, axis_colour, pars.bg)?;
        let axis_lty = line_type_at(lty, 0, pars.lty)?;
        let axis_lwd = real_at(lwd, 0, 1.0);
        let ticks_lwd = real_at(lwd_ticks, 0, axis_lwd);
        let point = |along: f64, across: f64| if horizontal { [along, across] } else { [across, along] };
        if tick && let (Some(first), Some(last)) = (shown.first(), shown.last()) {
            if axis_lwd > 0.0 {
                push_line(
                    device,
                    clip,
                    vec![point(first.1, axis_across), point(last.1, axis_across)],
                    stroke_with(&pars, axis_colour, axis_lty, axis_lwd),
                );
            }
            // Ticks point outwards for negative `tcl`; `tck`, when set, is a fraction of the
            // plot region's height or width instead.
            let plot = region_rect(device, 17);
            let length = if pars.tck.is_finite() {
                let extent = if horizontal { plot.y1 - plot.y0 } else { plot.x1 - plot.x0 };
                pars.tck * extent
            } else {
                pars.tcl * pars.mex * device.line_inches()
            };
            let inwards = match side {
                1 | 2 => length,
                _ => -length,
            };
            if ticks_lwd > 0.0 {
                for (_, along) in &shown {
                    push_line(
                        device,
                        clip,
                        vec![point(*along, axis_across), point(*along, axis_across + inwards)],
                        stroke_with(&pars, ticks_colour, axis_lty, ticks_lwd),
                    );
                }
            }
        }
        let Some(labels) = labels else {
            return Ok(());
        };
        let perpendicular = match side {
            1 | 3 => pars.las == 2 || pars.las == 3,
            _ => pars.las == 1 || pars.las == 2,
        };
        let hadj = if hadj.is_finite() {
            hadj
        } else if perpendicular {
            if side <= 2 { 1.0 } else { 0.0 }
        } else {
            0.5
        };
        let padj = if padj.is_finite() {
            padj
        } else if perpendicular {
            0.5
        } else {
            0.0
        };
        let label_cex = pars.cex_axis * pars.cex;
        let label_font = values::as_integer_scalar(font);
        let label_font = if label_font == NA_INTEGER || label_font < 1 {
            pars.font_axis
        } else {
            label_font
        };
        let label_colour = text_colour_at(col, 0, pars.col_axis, pars.bg)?;
        let label_colour = if col == nil() || length(col) == 0 { pars.col_axis } else { label_colour };
        // A label that would come within `gap.axis` widths of "m" of the last one drawn is
        // left out.
        let gap = if gap_axis.is_finite() {
            gap_axis
        } else if perpendicular {
            0.25
        } else {
            1.0
        } * text_inches(&pars, "m", label_cex, label_font);
        let mut last_end = f64::NEG_INFINITY;
        let ascending = shown.first().zip(shown.last()).is_none_or(|(first, last)| first.1 <= last.1);
        let mut ordered = shown.clone();
        if !ascending {
            ordered.reverse();
        }
        for (index, along) in ordered {
            let Some(lines) = labels.get(index % labels.len().max(1)) else {
                continue;
            };
            if lines.is_empty() {
                continue;
            }
            let extent = if perpendicular {
                let line_height = 1.2 * f64::from(pars.ps) * label_cex / 72.0;
                line_height * lines.len() as f64
            } else {
                lines
                    .iter()
                    .map(|line| text_inches(&pars, line, label_cex, label_font))
                    .fold(0.0, f64::max)
            };
            let start = along - extent * if perpendicular { 0.5 } else { hadj };
            if start < last_end + gap {
                continue;
            }
            last_end = start + extent;
            push_margin_text(
                device,
                clip,
                side,
                label_line,
                outer,
                along,
                lines,
                pars.las,
                hadj,
                padj,
                label_cex,
                label_font,
                label_colour,
            );
        }
        Ok(())
    })
}

/// `box(which, lty, ...)`: the outline of the plot region, figure, inner region or device,
/// with the sides `bty` selects for the plot region.
unsafe fn do_box(call: &NativeCall<'_>) -> R<Sexp> {
    let which = values::as_integer_scalar(call.arg(0));
    let lty = call.arg(1);
    with_inline_pars(call, 2, |device| {
        let pars = device.pars.clone();
        let colour = inline_par(call, "col")
            .map_or(Ok(pars.fg), |value| colour_at(value, 0, pars.fg, pars.bg))?;
        let stroke = stroke_with(&pars, colour, line_type_at(lty, 0, pars.lty)?, pars.lwd);
        let region = match which {
            1 => region_rect(device, 17),
            2 => region_rect(device, 8),
            3 => region_rect(device, 7),
            4 => device_rect(device),
            _ => return Err(error("invalid 'which' argument")),
        };
        let clip = device_rect(device);
        let corners = [
            [region.x0, region.y0],
            [region.x1, region.y0],
            [region.x1, region.y1],
            [region.x0, region.y1],
        ];
        let [bottom_left, bottom_right, top_right, top_left] = corners;
        let bty = if which == 1 { pars.bty } else { 'o' };
        let path: Option<(Vec<[f64; 2]>, bool)> = match bty {
            'o' => Some((corners.to_vec(), true)),
            'l' => Some((vec![top_left, bottom_left, bottom_right], false)),
            '7' => Some((vec![top_left, top_right, bottom_right], false)),
            'c' => Some((vec![top_right, top_left, bottom_left, bottom_right], false)),
            'u' => Some((vec![top_left, bottom_left, bottom_right, top_right], false)),
            ']' => Some((vec![top_left, top_right, bottom_right, bottom_left], false)),
            _ => None,
        };
        if let Some((points, closed)) = path {
            push_shape(
                device,
                clip,
                Shape::Path {
                    points,
                    closed,
                    fill: TRANSPARENT_WHITE,
                    stroke: Some(stroke),
                },
            );
        }
        Ok(())
    })?;
    Ok(nil())
}

/// The lines of the first label of an annotation argument, or `None` when there is none.
unsafe fn annotation(value: Sexp) -> R<Option<Vec<String>>> {
    if value == nil() || length(value) == 0 {
        return Ok(None);
    }
    let lines: Vec<String> = label_lines(value)?.into_iter().flatten().collect();
    Ok((!lines.is_empty()).then_some(lines))
}

/// `title(main, sub, xlab, ylab, line, outer, ...)`.
unsafe fn do_title(call: &NativeCall<'_>) -> R<Sexp> {
    let main = annotation(call.arg(0))?;
    let sub = annotation(call.arg(1))?;
    let xlab = annotation(call.arg(2))?;
    let ylab = annotation(call.arg(3))?;
    let line = values::as_real_scalar(call.arg(4));
    let outer = values::as_logical_scalar(call.arg(5)) == 1;
    with_inline_pars(call, 6, |device| {
        let pars = device.pars.clone();
        let clip = device_rect(device);
        let region = region_rect(device, if outer { 7 } else { 17 });
        let along_x = lerp([region.x0, region.x1], pars.adj);
        let along_y = lerp([region.y0, region.y1], pars.adj);
        if let Some(lines) = main {
            let cex = pars.cex_main * pars.cex;
            if line.is_finite() {
                push_margin_text(
                    device, clip, 3, line, outer, along_x, &lines, 0, pars.adj, 0.0, cex,
                    pars.font_main, pars.col_main,
                );
            } else {
                // The main title is centred on the middle line of the top margin.
                let margin = if outer {
                    device.outer_inches()[2]
                } else {
                    device.margin_inches()[2]
                };
                let anchor = [along_x, region.y1 + margin / 2.0];
                push_text(
                    device, clip, anchor, &lines, 0.0, pars.adj, 0.5, cex, pars.font_main,
                    pars.col_main,
                );
            }
        }
        if let Some(lines) = sub {
            let at = if line.is_finite() { line } else { pars.mgp[0] + 1.0 };
            push_margin_text(
                device, clip, 1, at, outer, along_x, &lines, 0, pars.adj, 0.0,
                pars.cex_sub * pars.cex, pars.font_sub, pars.col_sub,
            );
        }
        if let Some(lines) = xlab {
            let at = if line.is_finite() { line } else { pars.mgp[0] };
            push_margin_text(
                device, clip, 1, at, outer, along_x, &lines, 0, pars.adj, 0.0,
                pars.cex_lab * pars.cex, pars.font_lab, pars.col_lab,
            );
        }
        if let Some(lines) = ylab {
            let at = if line.is_finite() { line } else { pars.mgp[0] };
            push_margin_text(
                device, clip, 2, at, outer, along_y, &lines, 0, pars.adj, 0.0,
                pars.cex_lab * pars.cex, pars.font_lab, pars.col_lab,
            );
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `text(xy, labels, adj, pos, offset, vfont, cex, col, font, ...)`.
unsafe fn do_text(call: &NativeCall<'_>) -> R<Sexp> {
    let (x, y) = xy_of(call.arg(0))?;
    let labels = label_lines(call.arg(1))?;
    let adj = reals_of(call.arg(2))?;
    let pos = call.arg(3);
    let offset = values::as_real_scalar(call.arg(4));
    let (cex, col, font) = (call.arg(6), call.arg(7), call.arg(8));
    with_inline_pars(call, 9, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let count = if labels.is_empty() { 0 } else { x.len().max(labels.len()) };
        let char_width = device.char_inches()[0] * pars.cex;
        let offset = if offset.is_finite() { offset } else { 0.5 } * char_width;
        for index in 0..count {
            if x.is_empty() {
                break;
            }
            let point = [
                x_axis.inches(x[index % x.len()]),
                y_axis.inches(y[index % y.len()]),
            ];
            let Some(lines) = labels.get(index % labels.len()) else {
                continue;
            };
            if point.iter().any(|value| !value.is_finite()) || lines.is_empty() {
                continue;
            }
            let label_cex = real_at(cex, index, 1.0);
            let label_cex = if label_cex.is_finite() { label_cex } else { 1.0 } * pars.cex;
            let colour = text_colour_at(col, index, pars.col, pars.bg)?;
            let label_font = real_at(font, index, f64::from(pars.font));
            let label_font = if label_font.is_finite() { label_font as i32 } else { pars.font };
            let mut hadj = adj.first().copied().filter(|value| value.is_finite()).unwrap_or(pars.adj);
            let mut yadj = adj.get(1).copied().filter(|value| value.is_finite()).unwrap_or(0.5);
            let mut anchor = point;
            let position = real_at(pos, index, f64::NAN);
            if position.is_finite() {
                match position as i32 {
                    1 => {
                        hadj = 0.5;
                        yadj = 1.0;
                        anchor[1] -= offset;
                    }
                    2 => {
                        hadj = 1.0;
                        yadj = 0.5;
                        anchor[0] -= offset;
                    }
                    3 => {
                        hadj = 0.5;
                        yadj = 0.0;
                        anchor[1] += offset;
                    }
                    4 => {
                        hadj = 0.0;
                        yadj = 0.5;
                        anchor[0] += offset;
                    }
                    _ => return Err(error("invalid 'pos' value")),
                }
            }
            push_text(
                device, clip, anchor, lines, pars.srt, hadj, yadj, label_cex, label_font, colour,
            );
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `mtext(text, side, line, outer, at, adj, padj, cex, col, font, ...)`, every argument
/// recycled to the longest.
unsafe fn do_mtext(call: &NativeCall<'_>) -> R<Sexp> {
    let labels = label_lines(call.arg(0))?;
    let arguments: Vec<Sexp> = (1..10).map(|index| call.arg(index)).collect();
    let count = arguments
        .iter()
        .map(|value| length(*value))
        .chain([labels.len()])
        .max()
        .unwrap_or(0);
    if labels.is_empty() {
        return Ok(nil());
    }
    with_inline_pars(call, 10, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let clip = if pars.xpd == 0 { region_rect(device, 8) } else { clip };
        for index in 0..count {
            let Some(lines) = labels.get(index % labels.len()) else {
                continue;
            };
            let side = real_at(arguments[0], index, 3.0) as i32;
            if !(1..=4).contains(&side) {
                return Err(error("invalid 'side' value"));
            }
            let line = real_at(arguments[1], index, 0.0);
            let outer = real_at(arguments[2], index, 0.0) == 1.0;
            let adj = real_at(arguments[4], index, pars.adj);
            let adj = if adj.is_finite() { adj } else { pars.adj };
            let at = real_at(arguments[3], index, f64::NAN);
            let along = if at.is_finite() {
                along_side(device, side, at, outer)
            } else {
                let region = region_rect(device, if outer { 7 } else { 17 });
                if side % 2 == 1 {
                    lerp([region.x0, region.x1], adj)
                } else {
                    lerp([region.y0, region.y1], adj)
                }
            };
            let perpendicular = match side {
                1 | 3 => pars.las == 2 || pars.las == 3,
                _ => pars.las == 1 || pars.las == 2,
            };
            let padj = real_at(arguments[5], index, f64::NAN);
            let padj = if padj.is_finite() {
                padj
            } else if perpendicular {
                0.5
            } else {
                0.0
            };
            let cex = real_at(arguments[6], index, 1.0);
            let cex = if cex.is_finite() { cex } else { 1.0 } * pars.cex;
            let colour = text_colour_at(arguments[7], index, pars.col, pars.bg)?;
            let font = real_at(arguments[8], index, f64::from(pars.font));
            let font = if font.is_finite() { font as i32 } else { pars.font };
            let clip = if outer { device_rect(device) } else { clip };
            push_margin_text(
                device, clip, side, line, outer, along, lines, pars.las, adj, padj, cex, font,
                colour,
            );
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `rect(xleft, ybottom, xright, ytop, col, border, lty, lwd, ...)`.
unsafe fn do_rect(call: &NativeCall<'_>) -> R<Sexp> {
    let corners: Vec<Vec<f64>> = (0..4).map(|index| reals_of(call.arg(index))).collect::<R<_>>()?;
    let (col, border, lty, lwd) = (call.arg(4), call.arg(5), call.arg(6), call.arg(7));
    let count = if corners.iter().any(Vec::is_empty) {
        0
    } else {
        corners.iter().map(Vec::len).max().unwrap_or(0)
    };
    with_inline_pars(call, 8, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        for index in 0..count {
            let value = |which: usize| corners[which][index % corners[which].len()];
            let (x0, x1) = (x_axis.inches(value(0)), x_axis.inches(value(2)));
            let (y0, y1) = (y_axis.inches(value(1)), y_axis.inches(value(3)));
            if [x0, x1, y0, y1].iter().any(|value| !value.is_finite()) {
                continue;
            }
            let fill = colour_at(col, index, TRANSPARENT_WHITE, pars.bg)?;
            let outline = colour_at(border, index, pars.fg, pars.bg)?;
            let stroke = stroke_with(
                &pars,
                outline,
                line_type_at(lty, index, pars.lty)?,
                real_at(lwd, index, pars.lwd),
            );
            push_shape(
                device,
                clip,
                Shape::Path {
                    points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
                    closed: true,
                    fill,
                    stroke: Some(stroke),
                },
            );
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `polygon(x, y, col, border, lty, ...)`: missing points separate polygons, which take the
/// colours in turn.
unsafe fn do_polygon(call: &NativeCall<'_>) -> R<Sexp> {
    let x = reals_of(call.arg(0))?;
    let y = reals_of(call.arg(1))?;
    let (col, border, lty) = (call.arg(2), call.arg(3), call.arg(4));
    with_inline_pars(call, 5, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let points: Vec<Option<[f64; 2]>> = x
            .iter()
            .zip(&y)
            .map(|(x, y)| {
                let point = [x_axis.inches(*x), y_axis.inches(*y)];
                point.iter().all(|value| value.is_finite()).then_some(point)
            })
            .collect();
        for (index, run) in points
            .split(Option::is_none)
            .filter(|run| !run.is_empty())
            .enumerate()
        {
            let fill = colour_at(col, index, TRANSPARENT_WHITE, pars.bg)?;
            let outline = colour_at(border, index, pars.fg, pars.bg)?;
            let stroke = stroke_with(&pars, outline, line_type_at(lty, index, pars.lty)?, pars.lwd);
            push_shape(
                device,
                clip,
                Shape::Path {
                    points: run.iter().flatten().copied().collect(),
                    closed: true,
                    fill,
                    stroke: Some(stroke),
                },
            );
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `path(x, y, lengths, rule, col, border, lty, ...)`: the subpaths drawn as polygons.
unsafe fn do_path(call: &NativeCall<'_>) -> R<Sexp> {
    let x = reals_of(call.arg(0))?;
    let y = reals_of(call.arg(1))?;
    let lengths = reals_of(call.arg(2))?;
    let (col, border, lty) = (call.arg(4), call.arg(5), call.arg(6));
    with_inline_pars(call, 7, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let fill = colour_at(col, 0, TRANSPARENT_WHITE, pars.bg)?;
        let outline = colour_at(border, 0, pars.fg, pars.bg)?;
        let stroke = stroke_with(&pars, outline, line_type_at(lty, 0, pars.lty)?, pars.lwd);
        let mut start = 0usize;
        for length in lengths {
            let end = (start + length.max(0.0) as usize).min(x.len()).min(y.len());
            let points: Vec<[f64; 2]> = (start..end)
                .map(|index| [x_axis.inches(x[index]), y_axis.inches(y[index])])
                .filter(|point| point.iter().all(|value| value.is_finite()))
                .collect();
            start = end;
            if points.is_empty() {
                continue;
            }
            push_shape(
                device,
                clip,
                Shape::Path {
                    points,
                    closed: true,
                    fill,
                    stroke: Some(stroke),
                },
            );
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `segments(x0, y0, x1, y1, col, lty, lwd, ...)` and `arrows(x0, y0, x1, y1, length, angle,
/// code, col, lty, lwd, ...)`, whose heads are `length` inches long at `angle` degrees to the
/// shaft, at the start (code 1), the end (2) or both (3).
unsafe fn draw_segments(call: &NativeCall<'_>, arrows: bool) -> R<Sexp> {
    let ends: Vec<Vec<f64>> = (0..4).map(|index| reals_of(call.arg(index))).collect::<R<_>>()?;
    let (head_length, angle, code, style) = if arrows {
        (
            values::as_real_scalar(call.arg(4)),
            values::as_real_scalar(call.arg(5)),
            values::as_integer_scalar(call.arg(6)),
            7,
        )
    } else {
        (0.0, 0.0, 0, 4)
    };
    let (col, lty, lwd) = (call.arg(style), call.arg(style + 1), call.arg(style + 2));
    let count = if ends.iter().any(Vec::is_empty) {
        0
    } else {
        ends.iter().map(Vec::len).max().unwrap_or(0)
    };
    with_inline_pars(call, style + 3, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        for index in 0..count {
            let value = |which: usize| ends[which][index % ends[which].len()];
            let from = [x_axis.inches(value(0)), y_axis.inches(value(1))];
            let to = [x_axis.inches(value(2)), y_axis.inches(value(3))];
            if from.iter().chain(&to).any(|value| !value.is_finite()) {
                continue;
            }
            let stroke = stroke_with(
                &pars,
                colour_at(col, index, pars.col, pars.bg)?,
                line_type_at(lty, index, pars.lty)?,
                real_at(lwd, index, pars.lwd),
            );
            push_line(device, clip, vec![from, to], stroke);
            if !arrows {
                continue;
            }
            let shaft = [to[0] - from[0], to[1] - from[1]];
            if shaft[0].hypot(shaft[1]) < 1e-3 {
                continue;
            }
            let direction = shaft[1].atan2(shaft[0]);
            let spread = angle.to_radians();
            let head = |tip: [f64; 2], pointing: f64| {
                let back = pointing + std::f64::consts::PI;
                vec![
                    [
                        tip[0] + head_length * (back + spread).cos(),
                        tip[1] + head_length * (back + spread).sin(),
                    ],
                    tip,
                    [
                        tip[0] + head_length * (back - spread).cos(),
                        tip[1] + head_length * (back - spread).sin(),
                    ],
                ]
            };
            if code & 1 != 0 {
                push_line(device, clip, head(from, direction + std::f64::consts::PI), stroke);
            }
            if code & 2 != 0 {
                push_line(device, clip, head(to, direction), stroke);
            }
        }
        Ok(())
    })?;
    Ok(nil())
}

unsafe fn do_segments(call: &NativeCall<'_>) -> R<Sexp> {
    draw_segments(call, false)
}

unsafe fn do_arrows(call: &NativeCall<'_>) -> R<Sexp> {
    draw_segments(call, true)
}

/// `abline(a, b, h, v, untf, col, lty, lwd, ...)`: lines across the plot region, styled in
/// turn from the colours, line types and widths.
unsafe fn do_abline(call: &NativeCall<'_>) -> R<Sexp> {
    let a = reals_of(call.arg(0))?;
    let b = reals_of(call.arg(1))?;
    let h = reals_of(call.arg(2))?;
    let v = reals_of(call.arg(3))?;
    let untf = values::as_logical_scalar(call.arg(4)) == 1;
    let (col, lty, lwd) = (call.arg(5), call.arg(6), call.arg(7));
    let line = match (a.as_slice(), b.as_slice()) {
        ([], _) => None,
        ([intercept, slope], []) => Some((*intercept, *slope)),
        ([_], []) | ([_, _, _, ..], []) => {
            return Err(error("invalid a=, b= specification"));
        }
        ([intercept, ..], [slope, ..]) => Some((*intercept, *slope)),
    };
    with_inline_pars(call, 8, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let mut index = 0usize;
        let mut next_stroke = |pars: &Pars| -> R<Stroke> {
            let stroke = stroke_with(
                pars,
                colour_at(col, index, pars.col, pars.bg)?,
                line_type_at(lty, index, pars.lty)?,
                real_at(lwd, index, pars.lwd),
            );
            index += 1;
            Ok(stroke)
        };
        let x_inches = |value: f64| x_axis.origin + x_axis.scale * value;
        let y_inches = |value: f64| y_axis.origin + y_axis.scale * value;
        if let Some((intercept, slope)) = line {
            let stroke = next_stroke(&pars)?;
            let [x0, x1] = [pars.usr[0], pars.usr[1]];
            // Without `untf`, the line is straight in the plot's (possibly logged)
            // coordinates; with it, straight in the data's, so drawn as a curve.
            let points: Vec<[f64; 2]> = if !untf || (!pars.xlog && !pars.ylog) {
                [x0, x1]
                    .iter()
                    .map(|x| [x_inches(*x), y_inches(intercept + slope * x)])
                    .collect()
            } else {
                (0..=100)
                    .map(|step| {
                        let plot_x = lerp([x0, x1], f64::from(step) / 100.0);
                        let data_x = if pars.xlog { 10f64.powf(plot_x) } else { plot_x };
                        let data_y = intercept + slope * data_x;
                        let plot_y = if pars.ylog { data_y.log10() } else { data_y };
                        [x_inches(plot_x), y_inches(plot_y)]
                    })
                    .filter(|point| point.iter().all(|value| value.is_finite()))
                    .collect()
            };
            push_line(device, clip, points, stroke);
        }
        for value in h {
            let stroke = next_stroke(&pars)?;
            let y = y_axis.inches(value);
            if y.is_finite() {
                let points = vec![[x_inches(pars.usr[0]), y], [x_inches(pars.usr[1]), y]];
                push_line(device, clip, points, stroke);
            }
        }
        for value in v {
            let stroke = next_stroke(&pars)?;
            let x = x_axis.inches(value);
            if x.is_finite() {
                let points = vec![[x, y_inches(pars.usr[2])], [x, y_inches(pars.usr[3])]];
                push_line(device, clip, points, stroke);
            }
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `image(x, y, zi, col)`: the cells between the `x` and `y` breaks filled with the colours
/// `zi` indexes from 0; missing indices leave a cell empty.
unsafe fn do_image(call: &NativeCall<'_>) -> R<Sexp> {
    let x = reals_of(call.arg(0))?;
    let y = reals_of(call.arg(1))?;
    let indices = coerce_vector(call.arg(2), SexpType::Integer)?;
    crate::heap().root(indices);
    let col = call.arg(3);
    let columns = x.len().saturating_sub(1);
    let rows = y.len().saturating_sub(1);
    let colours: Vec<Colour> = (0..length(col))
        .map(|index| colour_element(col, index))
        .collect::<R<_>>()?;
    with_inline_pars(call, 4, |device| {
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let cells = values::ints(indices);
        for row in 0..rows {
            for column in 0..columns {
                let Some(&index) = cells.get(column + row * columns) else {
                    continue;
                };
                if index == NA_INTEGER || index < 0 {
                    continue;
                }
                let Some(&fill) = colours.get(index as usize) else {
                    continue;
                };
                let (x0, x1) = (x_axis.inches(x[column]), x_axis.inches(x[column + 1]));
                let (y0, y1) = (y_axis.inches(y[row]), y_axis.inches(y[row + 1]));
                if [x0, x1, y0, y1].iter().any(|value| !value.is_finite()) {
                    continue;
                }
                push_shape(
                    device,
                    clip,
                    Shape::Path {
                        points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
                        closed: true,
                        fill,
                        stroke: None,
                    },
                );
            }
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `rasterImage(image, xleft, ybottom, xright, ytop, angle, interpolate, ...)`: the raster's
/// cells as filled rectangles, its first row at the top.
unsafe fn do_raster(call: &NativeCall<'_>) -> R<Sexp> {
    let image = call.arg(0);
    let dim = values::get_attr(image, symbols().dim);
    let (rows, columns) = if dim != nil() && length(dim) == 2 {
        let dim = coerce_vector(dim, SexpType::Integer)?;
        (values::ints(dim)[0].max(0) as usize, values::ints(dim)[1].max(0) as usize)
    } else {
        return Err(error("invalid raster object"));
    };
    let native = image.kind() == SexpType::Integer;
    let colours: Vec<Colour> = (0..rows * columns)
        .map(|index| {
            if native {
                Ok(values::ints(image)[index] as u32)
            } else {
                colour_element(image, index)
            }
        })
        .collect::<R<_>>()?;
    let corners: Vec<Vec<f64>> = (1..5).map(|index| reals_of(call.arg(index))).collect::<R<_>>()?;
    with_inline_pars(call, 7, |device| {
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let count = corners.iter().map(Vec::len).min().unwrap_or(0);
        for index in 0..count {
            let (left, right) = (x_axis.inches(corners[0][index]), x_axis.inches(corners[2][index]));
            let (bottom, top) = (y_axis.inches(corners[1][index]), y_axis.inches(corners[3][index]));
            if [left, right, bottom, top].iter().any(|value| !value.is_finite()) {
                continue;
            }
            for row in 0..rows {
                for column in 0..columns {
                    // Raster objects hold their cells row by row.
                    let Some(&fill) = colours.get(row * columns + column) else {
                        continue;
                    };
                    let x0 = lerp([left, right], column as f64 / columns as f64);
                    let x1 = lerp([left, right], (column + 1) as f64 / columns as f64);
                    let y1 = lerp([top, bottom], row as f64 / rows as f64);
                    let y0 = lerp([top, bottom], (row + 1) as f64 / rows as f64);
                    push_shape(
                        device,
                        clip,
                        Shape::Path {
                            points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
                            closed: true,
                            fill,
                            stroke: None,
                        },
                    );
                }
            }
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `contour(x, y, z, levels, labels, labcex, drawlabels, method, vfont, col, lty, lwd, ...)`:
/// the contour lines of each level, each labelled where it is flattest over the label's width
/// unless `drawlabels` is false.
unsafe fn do_contour(call: &NativeCall<'_>) -> R<Sexp> {
    let x = reals_of(call.arg(0))?;
    let y = reals_of(call.arg(1))?;
    let z = reals_of(call.arg(2))?;
    let levels = reals_of(call.arg(3))?;
    let labcex = real_at(call.arg(5), 0, 1.0);
    let draw_labels = values::as_logical_scalar(call.arg(6)) != 0;
    let labels = if !draw_labels {
        Vec::new()
    } else if call.arg(4) == nil() {
        let settings = crate::format::Settings::current();
        levels
            .iter()
            .map(|level| {
                let format = crate::format::format_real_common(&[*level], settings);
                crate::format::format_real_with(*level, format, "NA").trim().to_owned()
            })
            .collect()
    } else {
        label_lines(call.arg(4))?
            .into_iter()
            .map(|lines| lines.join(" "))
            .collect()
    };
    let (col, lty, lwd) = (call.arg(9), call.arg(10), call.arg(11));
    if x.len() < 2 || y.len() < 2 || z.len() != x.len() * y.len() {
        return Err(error("invalid x / y values or limits"));
    }
    with_inline_pars(call, 12, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let grid = Grid { x: &x, y: &y, z: &z };
        let mut placed: Vec<Rect> = Vec::new();
        for (index, level) in levels.iter().enumerate() {
            let colour = colour_at(col, index, pars.col, pars.bg)?;
            let stroke = stroke_with(
                &pars,
                colour,
                line_type_at(lty, index, pars.lty)?,
                real_at(lwd, index, pars.lwd),
            );
            let label = (!labels.is_empty())
                .then(|| format!(" {} ", labels[index % labels.len()]));
            for line in contour_level(&grid, *level) {
                let points: Vec<[f64; 2]> = line
                    .iter()
                    .map(|(x, y)| [x_axis.inches(*x), y_axis.inches(*y)])
                    .collect();
                let Some(label) = label.as_deref() else {
                    push_line(device, clip, points, stroke);
                    continue;
                };
                let width = text_inches(&pars, label, labcex, 1);
                let height = HELVETICA_CAP_HEIGHT / 1000.0 * text_points(&pars, labcex)
                    / 72.0;
                match flattest_window(&points, width, height, &placed) {
                    Some(window) => {
                        placed.push(window.bounds);
                        push_line(device, clip, window.before, stroke);
                        push_line(device, clip, window.after, stroke);
                        push_text(
                            device,
                            clip,
                            window.centre,
                            &[label.to_owned()],
                            window.angle,
                            0.5,
                            0.5,
                            labcex,
                            1,
                            colour,
                        );
                    }
                    None => push_line(device, clip, points, stroke),
                }
            }
        }
        Ok(())
    })?;
    Ok(nil())
}

/// Where a contour label goes: the line split around it, its centre and angle, and the box
/// it covers.
struct LabelWindow {
    before: Vec<[f64; 2]>,
    after: Vec<[f64; 2]>,
    centre: [f64; 2],
    angle: f64,
    bounds: Rect,
}

/// The stretch of `points`, `width` inches long along the line, that strays least from its
/// chord, as long as the label it would carry keeps clear of the labels already `placed`.
fn flattest_window(
    points: &[[f64; 2]],
    width: f64,
    height: f64,
    placed: &[Rect],
) -> Option<LabelWindow> {
    let mut arc = Vec::with_capacity(points.len());
    let mut total = 0.0;
    for (index, point) in points.iter().enumerate() {
        if index > 0 {
            let previous = points[index - 1];
            total += (point[0] - previous[0]).hypot(point[1] - previous[1]);
        }
        arc.push(total);
    }
    if total <= width || points.len() < 2 {
        return None;
    }
    let point_at = |distance: f64| -> ([f64; 2], usize) {
        let segment = arc
            .iter()
            .position(|length| *length >= distance)
            .unwrap_or(points.len() - 1)
            .max(1);
        let (start, end) = (points[segment - 1], points[segment]);
        let span = arc[segment] - arc[segment - 1];
        let fraction = if span > 0.0 {
            (distance - arc[segment - 1]) / span
        } else {
            0.0
        };
        (
            [
                start[0] + fraction * (end[0] - start[0]),
                start[1] + fraction * (end[1] - start[1]),
            ],
            segment,
        )
    };
    let mut best: Option<(f64, LabelWindow)> = None;
    for (index, start_distance) in arc.iter().enumerate() {
        if start_distance + width > total {
            break;
        }
        let (start, _) = point_at(*start_distance);
        let (end, end_segment) = point_at(start_distance + width);
        let chord = [end[0] - start[0], end[1] - start[1]];
        let length = chord[0].hypot(chord[1]);
        if length <= 0.0 {
            continue;
        }
        let straying = points[index..end_segment]
            .iter()
            .map(|point| {
                ((point[0] - start[0]) * chord[1] - (point[1] - start[1]) * chord[0]).abs()
                    / length
            })
            .fold(0.0, f64::max);
        if best.as_ref().is_some_and(|(score, _)| *score <= straying) {
            continue;
        }
        let centre = [(start[0] + end[0]) / 2.0, (start[1] + end[1]) / 2.0];
        let mut angle = chord[1].atan2(chord[0]).to_degrees();
        if angle > 90.0 {
            angle -= 180.0;
        } else if angle <= -90.0 {
            angle += 180.0;
        }
        let (sin, cos) = angle.to_radians().sin_cos();
        let half = [
            (width * cos.abs() + height * sin.abs()) / 2.0,
            (width * sin.abs() + height * cos.abs()) / 2.0,
        ];
        let bounds = Rect {
            x0: centre[0] - half[0],
            x1: centre[0] + half[0],
            y0: centre[1] - half[1],
            y1: centre[1] + half[1],
        };
        let overlaps = placed.iter().any(|other| {
            bounds.x0 < other.x1 && other.x0 < bounds.x1 && bounds.y0 < other.y1 && other.y0 < bounds.y1
        });
        if overlaps {
            continue;
        }
        let mut before = points[..index].to_vec();
        before.push(start);
        let mut after = vec![end];
        after.extend_from_slice(&points[end_segment..]);
        best = Some((
            straying,
            LabelWindow {
                before,
                after,
                centre,
                angle,
                bounds,
            },
        ));
    }
    best.map(|(_, window)| window)
}

/// `symbols(x, y, type, data, inches, bg, fg, ...)` for circles (type 1), squares (2) and
/// rectangles (3), sized in user x units, or scaled so the largest is `inches` across.
unsafe fn do_symbols(call: &NativeCall<'_>) -> R<Sexp> {
    let x = reals_of(call.arg(0))?;
    let y = reals_of(call.arg(1))?;
    let kind = values::as_integer_scalar(call.arg(2));
    let data = reals_of(call.arg(3))?;
    let inches = call.arg(4);
    let (bg, fg) = (call.arg(5), call.arg(6));
    with_inline_pars(call, 7, |device| {
        let pars = device.pars.clone();
        let clip = clip_rect(device);
        let [x_axis, y_axis] = user_axes(device);
        let count = x.len().min(y.len());
        let columns = if count == 0 { 0 } else { data.len() / count };
        let largest = data.iter().copied().filter(|value| value.is_finite()).fold(0.0, f64::max);
        let scale_to = values::as_real_scalar(inches);
        let size_inches = |value: f64| {
            if inches.kind() == SexpType::Logical && values::as_logical_scalar(inches) == 0 {
                value * x_axis.scale.abs()
            } else {
                let target = if scale_to.is_finite() && scale_to > 0.0 { scale_to } else { 1.0 };
                if largest > 0.0 { value / largest * target } else { 0.0 }
            }
        };
        for index in 0..count {
            let centre = [x_axis.inches(x[index]), y_axis.inches(y[index])];
            if centre.iter().any(|value| !value.is_finite()) {
                continue;
            }
            let fill = colour_at(bg, index, TRANSPARENT_WHITE, pars.bg)?;
            let outline = colour_at(fg, index, pars.col, pars.bg)?;
            let stroke = Some(par_stroke(&pars, outline));
            let datum = |column: usize| data.get(index + column * count).copied().unwrap_or(f64::NAN);
            match kind {
                1 if columns >= 1 => push_shape(
                    device,
                    clip,
                    Shape::Circle {
                        centre,
                        radius: size_inches(datum(0)),
                        fill,
                        stroke,
                    },
                ),
                2 | 3 if columns >= 1 => {
                    let half_width = size_inches(datum(0)) / 2.0;
                    let half_height = if kind == 3 && columns >= 2 {
                        size_inches(datum(1)) / 2.0
                    } else {
                        half_width
                    };
                    push_shape(
                        device,
                        clip,
                        Shape::Path {
                            points: vec![
                                [centre[0] - half_width, centre[1] - half_height],
                                [centre[0] + half_width, centre[1] - half_height],
                                [centre[0] + half_width, centre[1] + half_height],
                                [centre[0] - half_width, centre[1] + half_height],
                            ],
                            closed: true,
                            fill,
                            stroke,
                        },
                    );
                }
                _ => {}
            }
        }
        Ok(())
    })?;
    Ok(nil())
}

/// `clip(x1, x2, y1, y2)`: clips later drawing to a rectangle in user coordinates until the
/// next plot.
unsafe fn do_clip(call: &NativeCall<'_>) -> R<Sexp> {
    let bounds: Vec<f64> = (0..4).map(|index| values::as_real_scalar(call.arg(index))).collect();
    with_plot(|device| {
        let [x_axis, y_axis] = user_axes(device);
        let (x0, x1) = (x_axis.inches(bounds[0]), x_axis.inches(bounds[1]));
        let (y0, y1) = (y_axis.inches(bounds[2]), y_axis.inches(bounds[3]));
        device.clip = Some(Rect {
            x0: x0.min(x1),
            x1: x0.max(x1),
            y0: y0.min(y1),
            y1: y0.max(y1),
        });
        Ok(())
    })?;
    Ok(nil())
}

/// `C_erase(col)`: fills the whole device.
unsafe fn do_erase(call: &NativeCall<'_>) -> R<Sexp> {
    let col = call.arg(0);
    with_plot(|device| {
        let fill = colour_at(col, 0, device.pars.bg, device.pars.bg)?;
        let all = device_rect(device);
        push_shape(
            device,
            all,
            Shape::Path {
                points: vec![[all.x0, all.y0], [all.x1, all.y0], [all.x1, all.y1], [all.x0, all.y1]],
                closed: true,
                fill,
                stroke: None,
            },
        );
        Ok(())
    })?;
    Ok(nil())
}

/// A colour as GNU R packs it: red in the low byte, then green, blue and alpha.
pub(crate) type Colour = u32;

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
        // Cairo with FreeType and with Pango, so `bitmapType` is "cairo" as in a desktop build.
        1 | 2 => scalar_bool(true),
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
    // Subnormal numbers are too coarse for decades or pretty steps, so the axis keeps its
    // limits and ticks once per decade.
    if 10f64.powf(first.min(last)) < f64::MIN_POSITIVE {
        return (low, high, 1);
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
    if !(first > 0.0 && last.is_finite() && last >= first) {
        return Vec::new();
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
        // Doubles span fewer than 700 decades, so the clamp only guards against an infinite ratio.
        let decades = (last / first).log10().round().clamp(0.0, 700.0) as i32;
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
    let mut decade = 10f64
        .powf(first.log10().floor() - 1.0)
        .max(f64::MIN_POSITIVE);
    while decade.is_finite() && decade <= high * 10.0 {
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

unsafe fn do_stem_leaf(call: &NativeCall<'_>) -> R<Sexp> {
    // A single value has no spread to split into stems, so GNU R prints nothing for it.
    if length(call.arg(0)) <= 1 {
        return Ok(nil());
    }
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
