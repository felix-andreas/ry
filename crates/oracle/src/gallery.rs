//! Renders a gallery of real plotting code under GNU R and ry's runtime and compares the images.
//!
//! A gallery file in `gallery/` registers each figure with `figure(name, code)`. The harness
//! picks a seeded sample of the figures, has `gallery.R` render each one on every requested
//! device under both engines, and compares the outputs:
//!
//! - `agg` (ragg) is the same rasterizer on both engines, so its pixels must agree exactly.
//! - `png` is Cairo under GNU R and ry's own renderer under ry, so it is compared pixel by pixel
//!   with the antialiasing difference tolerated (see [`Comparison::differing`]).
//! - `pdf` is rasterized with `pdftoppm` when it is installed and compared like `png`.
//! - `svg` from Cairo spells every glyph as a path while ry's keeps text, so the files are not
//!   compared; the figure only has to render.
//!
//! Each render is timed by `gallery.R` itself, best of `repeats`, so the timings exclude
//! starting the engine and loading packages.

use {
    crate::{gnu_r_home, gnu_r_program, installed_packages, ry_program},
    std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        process::Command,
    },
    syntax::testing::SplitMix64,
};

const GALLERY_DRIVER: &str = include_str!("gallery.R");

/// The devices the driver can open, in the order the report lists them.
pub const DEVICES: [&str; 4] = ["agg", "png", "pdf", "svg"];

/// A figure in a gallery file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Figure {
    /// The gallery file's stem, such as `base`.
    pub file: String,
    pub name: String,
}

impl Figure {
    pub fn label(&self) -> String {
        format!("{}:{}", self.file, self.name)
    }
}

pub struct Options {
    pub seed: u64,
    /// How many figures to sample, or every figure for `None`.
    pub count: Option<usize>,
    /// Figures named as `file:figure`; when given, they replace the sample.
    pub figures: Vec<String>,
    pub devices: Vec<String>,
    pub repeats: u32,
    /// Where both engines write their images (`<output>/gnu-r`, `<output>/ry`).
    pub output: PathBuf,
}

/// One figure on one device under both engines.
#[derive(Debug, Clone)]
pub struct Render {
    pub figure: Figure,
    pub device: String,
    pub gnu_r_seconds: Option<f64>,
    pub ry_seconds: Option<f64>,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Both images decoded and compared.
    Compared(Comparison),
    /// Both engines rendered, and the device's output is not compared (`svg`, or `pdf`
    /// without `pdftoppm`).
    Rendered,
    /// GNU R failed too, so the figure says nothing about ry.
    GnuRFailed(String),
    /// ry failed where GNU R did not.
    RyFailed(String),
    /// The images could not be read or have different sizes.
    Unreadable(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comparison {
    /// The share of pixels where some channel differs by more than 64 of 255: a mark one
    /// engine drew and the other did not, rather than antialiasing.
    pub differing: f64,
    /// The share of pixels that differ at all.
    pub inexact: f64,
}

impl Render {
    /// Whether this render counts as a parity failure under the device's tolerance.
    pub fn fails(&self) -> bool {
        match &self.outcome {
            Outcome::Compared(comparison) => comparison.differing > tolerance(&self.device),
            Outcome::RyFailed(_) | Outcome::Unreadable(_) => true,
            Outcome::Rendered | Outcome::GnuRFailed(_) => false,
        }
    }
}

/// The share of strongly differing pixels a device tolerates. ragg rasterizes identically on
/// both engines, so any difference there is a real one; the vector devices go through
/// different rasterizers, whose glyph shapes alone differ in a few percent of the pixels.
pub fn tolerance(device: &str) -> f64 {
    match device {
        "agg" => 0.002,
        _ => 0.05,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GalleryError {
    #[error("{0}")]
    Setup(String),
}

/// The gallery directory in the oracle crate.
pub fn gallery_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("gallery")
}

/// Every figure in the gallery files, with the files whose `# oracle: requires` packages GNU R
/// lacks left out, sorted by file and name.
pub fn figures() -> Result<Vec<Figure>, GalleryError> {
    let installed = installed_packages().unwrap_or_default();
    let mut figures = Vec::new();
    for path in gallery_files()? {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| GalleryError::Setup(format!("{}: {error}", path.display())))?;
        let required = text
            .lines()
            .take_while(|line| line.starts_with('#'))
            .filter_map(|line| line.strip_prefix("# oracle: requires "))
            .flat_map(|list| list.split(','))
            .map(str::trim);
        if required
            .into_iter()
            .any(|package| !installed.iter().any(|name| name == package))
        {
            continue;
        }
        let file = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("figure(\"") else {
                continue;
            };
            if let Some((name, _)) = rest.split_once('"') {
                figures.push(Figure {
                    file: file.clone(),
                    name: name.to_string(),
                });
            }
        }
    }
    figures.sort();
    Ok(figures)
}

/// A seeded sample of `count` figures, in the order the seed draws them.
pub fn sample(mut figures: Vec<Figure>, seed: u64, count: usize) -> Vec<Figure> {
    let mut rng = SplitMix64(seed);
    let mut chosen = Vec::new();
    for _ in 0..count.min(figures.len()) {
        chosen.push(figures.swap_remove(rng.below(figures.len())));
    }
    chosen
}

/// The devices that can run here: `agg` needs ragg.
pub fn available_devices(devices: &[String]) -> Vec<String> {
    let has_ragg =
        installed_packages().is_some_and(|names| names.iter().any(|name| name == "ragg"));
    devices
        .iter()
        .filter(|device| device.as_str() != "agg" || has_ragg)
        .cloned()
        .collect()
}

/// Renders the chosen figures under both engines and compares them.
pub fn run(options: &Options) -> Result<Vec<Render>, GalleryError> {
    let all = figures()?;
    let chosen = if options.figures.is_empty() {
        match options.count {
            Some(count) => sample(all, options.seed, count),
            None => all,
        }
    } else {
        let mut chosen = Vec::new();
        for label in &options.figures {
            match all.iter().find(|figure| figure.label() == *label) {
                Some(figure) => chosen.push(figure.clone()),
                None => {
                    return Err(GalleryError::Setup(format!(
                        "no gallery figure named {label}"
                    )));
                }
            }
        }
        chosen
    };
    let devices = available_devices(&options.devices);
    if chosen.is_empty() || devices.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(&options.output).map_err(setup_error)?;
    let driver = options.output.join("gallery.R");
    std::fs::write(&driver, GALLERY_DRIVER).map_err(setup_error)?;
    let selection = options.output.join("selection.tsv");
    let lines: String = chosen
        .iter()
        .map(|figure| format!("{}\t{}\n", figure.file, figure.name))
        .collect();
    std::fs::write(&selection, lines).map_err(setup_error)?;

    let gnu_r_dir = options.output.join("gnu-r");
    let ry_dir = options.output.join("ry");
    let mut gnu_r = Command::new(gnu_r_program());
    gnu_r.args(["--vanilla", "--slave", "--no-restore", "-f"]);
    gnu_r.arg(&driver).arg("--args");
    let mut ry = Command::new(ry_program());
    ry.args(["run", "--runtime", "ry"]).arg(&driver);
    if let Some(home) = gnu_r_home().or_else(r_home) {
        ry.env("R_HOME", home);
    }
    let gnu_r_renders = render_all(gnu_r, &gnu_r_dir, &selection, &devices, options)?;
    let ry_renders = render_all(ry, &ry_dir, &selection, &devices, options)?;

    let rasterize_pdf = devices.iter().any(|device| device == "pdf") && pdftoppm_available();
    let mut renders = Vec::new();
    for figure in &chosen {
        for device in &devices {
            let key = (figure.file.clone(), figure.name.clone(), device.clone());
            let gnu = gnu_r_renders.get(&key);
            let ours = ry_renders.get(&key);
            let gnu_r_seconds = gnu.and_then(|entry| entry.ok().then_some(entry.seconds));
            let ry_seconds = ours.and_then(|entry| entry.ok().then_some(entry.seconds));
            let path = |dir: &Path| {
                dir.join(format!(
                    "{}__{}.{}",
                    figure.file,
                    figure.name,
                    extension(device)
                ))
            };
            let outcome = match (gnu, ours) {
                (None, _) => Outcome::GnuRFailed("not rendered".to_string()),
                (Some(entry), _) if !entry.ok() => Outcome::GnuRFailed(entry.status.clone()),
                (_, None) => Outcome::RyFailed("not rendered".to_string()),
                (_, Some(entry)) if !entry.ok() => Outcome::RyFailed(entry.status.clone()),
                _ => match device.as_str() {
                    "agg" => compare_png_files(&path(&gnu_r_dir), &path(&ry_dir), 0),
                    "png" => compare_png_files(&path(&gnu_r_dir), &path(&ry_dir), 1),
                    "pdf" if rasterize_pdf => compare_pdf_files(&path(&gnu_r_dir), &path(&ry_dir)),
                    _ => Outcome::Rendered,
                },
            };
            renders.push(Render {
                figure: figure.clone(),
                device: device.clone(),
                gnu_r_seconds,
                ry_seconds,
                outcome,
            });
        }
    }
    Ok(renders)
}

/// A report with one line per render: the verdict, the timings and the pixel differences.
pub fn render_report(renders: &[Render]) -> String {
    let mut report = String::new();
    let mut gnu_r_total = 0.0;
    let mut ry_total = 0.0;
    let mut failures = 0;
    for render in renders {
        let verdict = if render.fails() { "DIFFERS" } else { "ok" };
        let detail = match &render.outcome {
            Outcome::Compared(comparison) => format!(
                "{:.4} differing, {:.4} inexact",
                comparison.differing, comparison.inexact
            ),
            Outcome::Rendered => "rendered".to_string(),
            Outcome::GnuRFailed(message) => format!("GNU R failed: {message}"),
            Outcome::RyFailed(message) => format!("ry failed: {message}"),
            Outcome::Unreadable(message) => message.clone(),
        };
        let time = |seconds: Option<f64>| seconds.map_or("-".to_string(), |s| format!("{s:.3}s"));
        if let (Some(gnu), Some(ours)) = (render.gnu_r_seconds, render.ry_seconds) {
            gnu_r_total += gnu;
            ry_total += ours;
        }
        failures += usize::from(render.fails());
        report.push_str(&format!(
            "{verdict:<8}{:<40}{:<5}GNU R {:>8}  ry {:>8}  {detail}\n",
            render.figure.label(),
            render.device,
            time(render.gnu_r_seconds),
            time(render.ry_seconds),
        ));
    }
    report.push_str(&format!(
        "{} renders, {failures} differing; rendering took {gnu_r_total:.2}s under GNU R and \
         {ry_total:.2}s under ry\n",
        renders.len()
    ));
    report
}

/// Compares two PNG files pixel by pixel, with `slack` as in [`compare_images`].
pub fn compare_png_files(expected: &Path, actual: &Path, slack: u32) -> Outcome {
    match (read_png(expected), read_png(actual)) {
        (Ok(expected), Ok(actual)) => compare_images(&expected, &actual, slack),
        (Err(error), _) | (_, Err(error)) => Outcome::Unreadable(error),
    }
}

/// An image as 8-bit RGB, composited over white.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Compares two images of the same size. With a `slack` of 0 each pixel is compared with the
/// pixel at the same place. With a larger slack a pixel counts as differing only when no pixel
/// within `slack` pixels in the other image comes close to it, in both directions: two
/// rasterizers antialias a dense tangle of thin lines differently, which moves coverage by a
/// pixel without changing what is drawn.
pub fn compare_images(expected: &Image, actual: &Image, slack: u32) -> Outcome {
    if (expected.width, expected.height) != (actual.width, actual.height) {
        return Outcome::Unreadable(format!(
            "sizes differ: {}x{} under GNU R, {}x{} under ry",
            expected.width, expected.height, actual.width, actual.height
        ));
    }
    let pixels = expected.rgb.len() / 3;
    if pixels == 0 {
        return Outcome::Unreadable("empty image".to_string());
    }
    let distance = |left: &[u8], right: &[u8]| {
        left.iter()
            .zip(right)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0)
    };
    let (width, height) = (expected.width as usize, expected.height as usize);
    let slack = slack as usize;
    fn pixel(image: &Image, x: usize, y: usize) -> &[u8] {
        let start = (y * image.width as usize + x) * 3;
        image.rgb.get(start..start + 3).unwrap_or(&[0, 0, 0])
    }
    let matched_nearby = |from: &Image, to: &Image, x: usize, y: usize| {
        let wanted = pixel(from, x, y);
        (y.saturating_sub(slack)..=(y + slack).min(height - 1)).any(|near_y| {
            (x.saturating_sub(slack)..=(x + slack).min(width - 1))
                .any(|near_x| distance(wanted, pixel(to, near_x, near_y)) <= 64)
        })
    };
    let mut differing = 0usize;
    let mut inexact = 0usize;
    for y in 0..height {
        for x in 0..width {
            let largest = distance(pixel(expected, x, y), pixel(actual, x, y));
            inexact += usize::from(largest > 0);
            differing += usize::from(
                largest > 64
                    && (slack == 0
                        || !matched_nearby(expected, actual, x, y)
                        || !matched_nearby(actual, expected, x, y)),
            );
        }
    }
    Outcome::Compared(Comparison {
        differing: differing as f64 / pixels as f64,
        inexact: inexact as f64 / pixels as f64,
    })
}

pub fn read_png(path: &Path) -> Result<Image, String> {
    let describe = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
    let file = std::fs::File::open(path).map_err(|error| describe(&error))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|error| describe(&error))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| describe(&error))?;
    let bytes = buffer.get(..info.buffer_size()).unwrap_or(&[]);
    let channels = info.color_type.samples();
    let mut rgb = Vec::with_capacity(info.width as usize * info.height as usize * 3);
    for pixel in bytes.chunks_exact(channels) {
        let (colour, alpha) = match *pixel {
            [grey] => ([grey; 3], 255),
            [grey, alpha] => ([grey; 3], alpha),
            [red, green, blue] => ([red, green, blue], 255),
            [red, green, blue, alpha] => ([red, green, blue], alpha),
            _ => return Err(describe(&"unsupported pixel layout")),
        };
        let alpha = u16::from(alpha);
        for value in colour {
            let blended = (u16::from(value) * alpha + 255 * (255 - alpha) + 127) / 255;
            rgb.push(blended as u8);
        }
    }
    Ok(Image {
        width: info.width,
        height: info.height,
        rgb,
    })
}

fn compare_pdf_files(expected: &Path, actual: &Path) -> Outcome {
    let rasterize = |pdf: &Path| -> Result<PathBuf, String> {
        let stem = pdf.with_extension("");
        let status = Command::new("pdftoppm")
            .args(["-png", "-r", "96", "-singlefile"])
            .arg(pdf)
            .arg(&stem)
            .status()
            .map_err(|error| format!("pdftoppm: {error}"))?;
        match status.success() {
            true => Ok(stem.with_extension("png")),
            false => Err(format!("pdftoppm could not read {}", pdf.display())),
        }
    };
    match (rasterize(expected), rasterize(actual)) {
        (Ok(expected), Ok(actual)) => compare_png_files(&expected, &actual, 1),
        (Err(error), _) | (_, Err(error)) => Outcome::Unreadable(error),
    }
}

fn pdftoppm_available() -> bool {
    Command::new("pdftoppm")
        .arg("-v")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn extension(device: &str) -> &'static str {
    match device {
        "agg" => "agg.png",
        "png" => "png",
        "pdf" => "pdf",
        _ => "svg",
    }
}

fn gallery_files() -> Result<Vec<PathBuf>, GalleryError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(gallery_dir())
        .map_err(setup_error)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "R"))
        .collect();
    files.sort();
    Ok(files)
}

/// One line of the driver's `renders.tsv`.
struct DriverRender {
    seconds: f64,
    status: String,
}

impl DriverRender {
    fn ok(&self) -> bool {
        self.status == "ok"
    }
}

type RenderKey = (String, String, String);

fn render_all(
    mut command: Command,
    output: &Path,
    selection: &Path,
    devices: &[String],
    options: &Options,
) -> Result<BTreeMap<RenderKey, DriverRender>, GalleryError> {
    if output.exists() {
        std::fs::remove_dir_all(output).map_err(setup_error)?;
    }
    std::fs::create_dir_all(output).map_err(setup_error)?;
    let files = gallery_files()?;
    command
        .arg(output)
        .arg(devices.join(","))
        .arg(options.seed.to_string())
        .arg(options.repeats.max(1).to_string())
        .arg(selection)
        .args(&files)
        .env("LC_ALL", "C.UTF-8")
        .env("TZ", "UTC");
    let finished = command
        .output()
        .map_err(|error| GalleryError::Setup(format!("could not start the renderer: {error}")))?;
    let renders = std::fs::read_to_string(output.join("renders.tsv")).unwrap_or_default();
    let mut parsed = BTreeMap::new();
    for line in renders.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if let [file, figure, device, seconds, status] = fields.as_slice() {
            parsed.insert(
                (file.to_string(), figure.to_string(), device.to_string()),
                DriverRender {
                    seconds: seconds.parse().unwrap_or(f64::NAN),
                    status: status.to_string(),
                },
            );
        }
    }
    if parsed.is_empty() && !finished.status.success() {
        let stderr = String::from_utf8_lossy(&finished.stderr);
        let tail: String = stderr
            .lines()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(GalleryError::Setup(format!(
            "the renderer failed before rendering anything ({}):\n{tail}",
            finished.status
        )));
    }
    Ok(parsed)
}

fn r_home() -> Option<String> {
    let output = Command::new(gnu_r_program()).arg("RHOME").output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|home| !home.is_empty())
}

fn setup_error(error: std::io::Error) -> GalleryError {
    GalleryError::Setup(error.to_string())
}
