//! Builds ry's base R from GNU R's source tarball: the default packages' R code, data, help and
//! metadata, laid out as an installed GNU R home (`library/`, `share/`, `etc/`, `doc/`), so that
//! ry needs no GNU R installation to run R code (`decisions.md`, "ry ships its own base R, and
//! GNU R through `R_HOME` is the opt-in").
//!
//! The build follows `src/library/Makefile` of the R sources step by step, with ry's own
//! interpreter as the R that runs each step: base's R code is concatenated and sourced, then
//! turned into a lazy-load database by `makebasedb.R`; tools builds its own database with its
//! own `makeLazyLoading`; every other package is concatenated and lazy-loaded by tools; methods
//! builds its class and method tables by loading itself once, as GNU R's build does. Byte
//! compilation is skipped, because ry's interpreter compiles R code to its own bytecode.
//!
//! The compiled code of the base packages is ry's own (`interp::builtin_libraries`), so each of
//! those packages gets a placeholder `libs/<package>.so` that `library.dynam` finds and ry's
//! `dyn.load` answers without opening. grid and splines use only R's public C API, so their C
//! sources are compiled against ry's headers and `libR` with ry's `R CMD SHLIB`.
//!
//! Each step that runs R is its own process, because an R session cannot be restarted inside one.

use {
    crate::{
        Error, HomeSources, Platform, Result, Toolchain, assemble, io_error, libr_path,
        read_directory, shlib, staging_suffix, write, write_script,
    },
    std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
    },
};

/// The file at the root of a built base that records what it was built from. Its presence marks
/// a directory as a complete build: the staging directory is renamed into place only after the
/// file is written.
pub const MANIFEST: &str = "ry-base.txt";

/// ry's base R tree when one is installed outside the executable: `RY_BASE_R` when set, then a
/// `base-r` directory beside the executable, one or two levels above it (`target/base-r`, which
/// serves every profile's binaries and test executables of a development checkout), or in
/// `../lib/ry/` (a distribution's package). A release carries its tree inside `ry` instead,
/// which the host extracts (`repl::bundled_base`).
pub fn installed() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RY_BASE_R").filter(|path| !path.is_empty()) {
        // Canonical, because the path ends up in `R_HOME` and in ry's cached R home, which a
        // process started in another directory must still find.
        return Some(fs::canonicalize(&path).unwrap_or_else(|_| PathBuf::from(path)));
    }
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    [
        directory.join("base-r"),
        directory.join("../base-r"),
        directory.join("../../base-r"),
        directory.join("../lib/ry/base-r"),
    ]
    .into_iter()
    .find(|candidate| is_complete(candidate))
    .and_then(|candidate| fs::canonicalize(candidate).ok())
}

/// Whether `root` holds base R's R code, which a session sources first: the test every R home,
/// ry's own or a GNU R installation, has to pass.
pub fn holds_base_r(root: &Path) -> bool {
    root.join("library/base/R/base").is_file()
}

/// Whether `root` holds a complete build, which the manifest marks.
pub fn is_complete(root: &Path) -> bool {
    root.join(MANIFEST).is_file()
}

/// How a package's R code is assembled before it is lazy-loaded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Code {
    /// `R/*.R`, then `R/<ostype>/*.R` where the package has platform code (GNU R's `mkR1`).
    Plain { platform_code: bool },
    /// `.packageName <- "<package>"` followed by `R/*.R`, for the packages that define S4
    /// classes (`mkR2`).
    S4,
}

/// How a package's compiled code is provided.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Native {
    None,
    /// ry's own routines (`interp::builtin_libraries`); the library file is a placeholder.
    Builtin,
    /// C sources that use only the public API, compiled against ry's `libR`.
    Compiled,
}

/// One default package of `R_PKGS_BASE1`, the packages built after base and tools.
struct Package {
    name: &'static str,
    code: Code,
    native: Native,
    /// `inst/` subdirectories copied into the installed package (`INSTDIRS`).
    inst: &'static [&'static str],
    /// Files of the source directory copied into the installed package (`FRONTFILES`).
    front: &'static [&'static str],
    demos: bool,
    /// `man/figures`, installed as `help/figures`.
    figures: bool,
    /// `R_DEFAULT_PACKAGES` while the package is lazy-loaded (`DEFPKGS`).
    default_packages: &'static str,
}

const PACKAGES: &[Package] = &[
    Package {
        name: "utils",
        code: Code::Plain {
            platform_code: true,
        },
        native: Native::Builtin,
        inst: &["Sweave", "doc", "misc"],
        front: &[],
        demos: false,
        figures: false,
        default_packages: "NULL",
    },
    Package {
        name: "grDevices",
        code: Code::Plain {
            platform_code: true,
        },
        native: Native::Builtin,
        inst: &[
            "afm",
            "enc",
            "fonts/Roboto",
            "fonts/Montserrat/static",
            "icc",
        ],
        front: &[],
        demos: true,
        figures: false,
        default_packages: "NULL",
    },
    Package {
        name: "graphics",
        code: Code::Plain {
            platform_code: false,
        },
        native: Native::Builtin,
        inst: &[],
        front: &[],
        demos: true,
        figures: true,
        default_packages: "NULL",
    },
    Package {
        name: "stats",
        code: Code::Plain {
            platform_code: false,
        },
        native: Native::Builtin,
        inst: &["doc"],
        front: &["COPYRIGHTS.modreg", "SOURCES.ts"],
        demos: true,
        figures: false,
        default_packages: "NULL",
    },
    Package {
        name: "datasets",
        code: Code::Plain {
            platform_code: false,
        },
        native: Native::None,
        inst: &[],
        front: &[],
        demos: false,
        figures: false,
        default_packages: "NULL",
    },
    Package {
        name: "methods",
        code: Code::S4,
        native: Native::Builtin,
        inst: &[],
        front: &[],
        demos: false,
        figures: false,
        default_packages: "NULL",
    },
    Package {
        name: "grid",
        code: Code::Plain {
            platform_code: false,
        },
        native: Native::Compiled,
        inst: &["doc"],
        front: &[],
        demos: false,
        figures: false,
        default_packages: "NULL",
    },
    Package {
        name: "splines",
        code: Code::Plain {
            platform_code: false,
        },
        native: Native::Compiled,
        inst: &[],
        front: &[],
        demos: false,
        figures: false,
        default_packages: "NULL",
    },
    Package {
        name: "stats4",
        code: Code::S4,
        native: Native::None,
        inst: &[],
        front: &[],
        demos: false,
        figures: false,
        default_packages: "methods,graphics,stats",
    },
    Package {
        name: "tcltk",
        code: Code::Plain {
            platform_code: false,
        },
        native: Native::None,
        inst: &[],
        front: &[],
        demos: true,
        figures: false,
        default_packages: "NULL",
    },
    Package {
        name: "parallel",
        code: Code::Plain {
            platform_code: true,
        },
        native: Native::Builtin,
        inst: &["doc"],
        front: &[],
        demos: false,
        figures: false,
        default_packages: "NULL",
    },
];

/// `R_PKGS_BASE`, in GNU R's order.
const ALL_PACKAGES: &[&str] = &[
    "base",
    "tools",
    "utils",
    "grDevices",
    "graphics",
    "stats",
    "datasets",
    "methods",
    "grid",
    "splines",
    "stats4",
    "tcltk",
    "compiler",
    "parallel",
];

/// The scripts of GNU R's `bin/` that run R code through the home's own `bin/R`: ry's home
/// links them, so `R CMD check`, `R CMD build` and the rest run on ry's interpreter. `R`,
/// `Rscript`, `Rcmd`, `INSTALL`, `SHLIB` and `config` are ry's own (`OWN_SCRIPTS`).
const SCRIPTS: &[&str] = &[
    "BATCH", "COMPILE", "LINK", "REMOVE", "Rd2pdf", "Rdconv", "Rdiff", "Rprof", "Stangle",
    "Sweave", "build", "check",
];

/// What the build needs besides the R sources.
pub struct BuildSources<'a> {
    /// The unpacked R source tree, patched (`tools/base-r`).
    pub source: &'a Path,
    /// ry's built runtime library, which runs every step.
    pub libr: &'a Path,
    /// The front end (`ry-rcmd`, or `ry`).
    pub rcmd: &'a Path,
    /// ry's C headers, for compiling grid and splines.
    pub include: &'a Path,
    /// The SHA-256 of the source tarball, recorded in the manifest.
    pub checksum: &'a str,
}

/// Builds the base packages from `sources` into `output`, which must not exist yet or holds an
/// earlier build that is replaced. The build happens in a staging directory beside `output` and
/// is renamed into place once complete.
pub fn build(sources: &BuildSources<'_>, output: &Path) -> Result<()> {
    let parent = output.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(io_error(parent))?;
    let staging = parent.join(format!(
        ".{}-{}",
        output.file_name().unwrap_or_default().to_string_lossy(),
        staging_suffix()
    ));
    let builder = Builder::new(sources, &staging)?;
    let built = builder.run();
    if let Err(error) = built {
        eprintln!(
            "the partial build is kept in {} for inspection",
            staging.display()
        );
        return Err(error);
    }
    if output.exists() {
        fs::remove_dir_all(output).map_err(io_error(output))?;
    }
    fs::rename(&staging, output).map_err(io_error(output))
}

/// The start-up image of a built base tree (`interp::image`), whose sessions load it in place of
/// base R's loader and the default packages: [`build`] writes it as its last step, and this
/// writes it again over the tree at `root` with the runtime `libr` and the front end `rcmd`,
/// which a runtime of another build needs, because an image loads only into the build that wrote
/// it.
pub fn write_start_image(root: &Path, rcmd: &Path, libr: &Path) -> Result<()> {
    let libr_link = libr_path(root);
    let front_end = if Platform::HOST == Platform::Windows {
        root.join("bin").join("R.exe")
    } else {
        root.join("bin/exec/ry-rcmd")
    };
    let work = root.join(format!(".image-{}", staging_suffix()));
    for path in [&libr_link, &front_end, &work] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
    }
    fs::create_dir_all(&work).map_err(io_error(&work))?;
    remove_if_present(&libr_link)?;
    remove_if_present(&front_end)?;
    crate::link(libr, &libr_link)?;
    crate::link(rcmd, &front_end)?;
    let written = run_image_writer(&front_end, root, &work);
    fs::remove_dir_all(&work).map_err(io_error(&work))?;
    remove_if_present(&libr_link)?;
    remove_if_present(&front_end)?;
    for directory in ["lib", "bin/exec", "bin/x64"] {
        let path = root.join(directory);
        if path.is_dir() && read_directory(&path)?.is_empty() {
            fs::remove_dir(&path).map_err(io_error(&path))?;
        }
    }
    written
}

/// Runs `front_end write-start-image` in the home `root`. The environment is emptied down to
/// what finds programs and temporary files, because the image holds start-up as this one
/// environment ran it; the steps that read the environment run again on every start.
fn run_image_writer(front_end: &Path, root: &Path, work: &Path) -> Result<()> {
    let image = root.join("library/base/R/ry-start.image");
    let mut command = Command::new(front_end);
    command
        .arg("write-start-image")
        .arg(&image)
        .current_dir(work)
        .env_clear()
        .env("R_HOME", root)
        .env("HOME", work)
        .env("R_LIBS_USER", "NULL")
        .env("R_LIBS_SITE", "NULL");
    for name in [
        "PATH",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SYSTEMROOT",
        "RY_PRINT_FAST_PATH_HASHES",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let output = command
        .output()
        .map_err(|error| Error::Command("ry-rcmd write-start-image".into(), error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(Error::Command(
        "ry-rcmd write-start-image".into(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    ))
}

/// The version fields a build reads from the R sources.
struct Release {
    /// `4.6.2`.
    version: String,
    /// The release date from `SVN-REVISION`, `YYYY-MM-DD`, used as every package's build date so
    /// that two builds of one release are identical.
    date: String,
}

struct Builder<'a> {
    sources: &'a BuildSources<'a>,
    root: PathBuf,
    /// Scratch space for the generated `DESCRIPTION` files and step scripts.
    work: PathBuf,
    platform: Platform,
    release: Release,
}

impl<'a> Builder<'a> {
    fn new(sources: &'a BuildSources<'a>, root: &Path) -> Result<Builder<'a>> {
        let source = sources.source;
        let read = |name: &str| -> Result<String> {
            let path = source.join(name);
            Ok(fs::read_to_string(&path)
                .map_err(io_error(&path))?
                .trim()
                .to_owned())
        };
        let version = read("VERSION")?;
        let version = version
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        let revision = read("SVN-REVISION")?;
        let date = revision
            .lines()
            .find_map(|line| line.strip_prefix("Last Changed Date:"))
            .map(|date| date.trim().chars().take(10).collect())
            .ok_or_else(|| Error::Package("SVN-REVISION has no Last Changed Date".into()))?;
        if root.exists() {
            fs::remove_dir_all(root).map_err(io_error(root))?;
        }
        let work = root.join(".build");
        for directory in ["library", "etc", "bin/exec", "lib", "share", "doc"] {
            let path = root.join(directory);
            fs::create_dir_all(&path).map_err(io_error(&path))?;
        }
        fs::create_dir_all(&work).map_err(io_error(&work))?;
        Ok(Builder {
            sources,
            root: root.to_path_buf(),
            work,
            platform: Platform::HOST,
            release: Release { version, date },
        })
    }

    fn ostype(&self) -> &'static str {
        if self.platform == Platform::Windows {
            "windows"
        } else {
            "unix"
        }
    }

    fn library(&self, package: &str) -> PathBuf {
        self.root.join("library").join(package)
    }

    fn package_source(&self, package: &str) -> PathBuf {
        self.sources.source.join("src/library").join(package)
    }

    fn run(&self) -> Result<()> {
        self.install_runtime_links()?;
        self.copy_home_files()?;
        self.build_profile()?;
        self.build_base()?;
        self.build_tools()?;
        self.build_simple("compiler", "NULL")?;
        // GNU R's `Rlazy` for base and tools: the lazy-load databases replace the sources.
        self.lazy_load_base()?;
        self.lazy_load_tools()?;
        for package in PACKAGES {
            self.build_package(package)?;
        }
        // base's DESCRIPTION is rewritten by tools, as `mkdesc` in base's Makefile does.
        let base_description = self.description("base")?;
        self.run_r(
            &format!(
                "tools:::.install_package_description({}, {}, {})",
                quote(&base_description),
                quote(&self.library("base")),
                quote_text(&self.built_stamp())
            ),
            "NULL",
        )?;
        let library = quote(&self.root.join("library"));
        let names = quote_text(&ALL_PACKAGES.join(" "));
        self.run_r(
            &format!("tools:::.vinstall_package_descriptions_as_RDS({library}, {names})"),
            "tools",
        )?;
        self.run_r(
            &format!("tools:::.vinstall_package_namespaces_as_RDS({library}, {names})"),
            "tools",
        )?;
        self.run_r(
            &format!(
                "tools:::.install_R_bibliographies_as_RDS({})",
                quote(&self.root.join("share/bibliographies"))
            ),
            "tools,utils",
        )?;
        self.run_r(
            &format!(
                "tools:::.install_R_dictionaries_as_RDS({})",
                quote(&self.root.join("share/dictionaries"))
            ),
            "tools",
        )?;
        self.build_help()?;
        self.compile_native()?;
        run_image_writer(&self.front_end(), &self.root, &self.work)?;
        self.finish()
    }

    /// The runtime and the front end, linked where an R home keeps them so that every step can
    /// run R through this home: `ry-rcmd` finds `libR` at `lib/` of `R_HOME`. Both links are
    /// removed when the build finishes, because ry's assembled home supplies its own.
    fn install_runtime_links(&self) -> Result<()> {
        let libr = libr_path(&self.root);
        if let Some(parent) = libr.parent() {
            fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
        crate::link(self.sources.libr, &libr)?;
        crate::link(self.sources.rcmd, &self.front_end())?;
        Ok(())
    }

    fn front_end(&self) -> PathBuf {
        if self.platform == Platform::Windows {
            self.root.join("bin").join("R.exe")
        } else {
            self.root.join("bin/exec/ry-rcmd")
        }
    }

    /// The files of an installed GNU R home that are not packages: `share/` as the sources hold
    /// it, the documentation the help system and `RShowDoc` read, `etc/`, `bin/`'s R-level
    /// scripts, and the version files the interpreter reads its version from.
    fn copy_home_files(&self) -> Result<()> {
        let source = self.sources.source;
        for entry in read_directory(&source.join("share"))? {
            let name = entry.file_name().unwrap_or_default();
            if entry.is_dir() {
                copy_tree_except(
                    &entry,
                    &self.root.join("share").join(name),
                    &["Makefile.in"],
                )?;
            }
        }
        // `shlib.mk` is ry's own, written into each assembled home.
        remove_if_present(&self.root.join("share/make/shlib.mk"))?;
        for name in [
            "AUTHORS",
            "COPYING",
            "COPYRIGHTS",
            "CRAN_mirrors.csv",
            "BioC_mirrors.csv",
            "FAQ",
            "KEYWORDS",
            "KEYWORDS.db",
            "RESOURCES",
            "THANKS",
        ] {
            copy_file(
                &source.join("doc").join(name),
                &self.root.join("doc").join(name),
            )?;
        }
        copy_tree_except(
            &source.join("doc/html"),
            &self.root.join("doc/html"),
            &["Makefile.in", "Makefile.win"],
        )?;
        for name in ["COPYING", "VERSION", "VERSION-NICK", "SVN-REVISION"] {
            copy_file(&source.join(name), &self.root.join(name))?;
        }
        copy_file(
            &source.join("etc/repositories"),
            &self.root.join("etc/repositories"),
        )?;
        let renviron_path = source.join("etc/Renviron.in");
        let renviron = fs::read_to_string(&renviron_path).map_err(io_error(&renviron_path))?;
        write(
            &self.root.join("etc/Renviron"),
            &substitute(&renviron, &self.renviron_values()),
        )?;
        if self.platform != Platform::Windows {
            for name in SCRIPTS {
                let from = source.join("src/scripts").join(name);
                let text = fs::read_to_string(&from).map_err(io_error(&from))?;
                write_script(&self.root.join("bin").join(name), &text)?;
            }
            let pager = source.join("src/scripts/pager.in");
            let text = fs::read_to_string(&pager).map_err(io_error(&pager))?;
            write_script(
                &self.root.join("bin/pager"),
                &substitute(&text, &[("R_SHELL", "/bin/sh")]),
            )?;
        }
        Ok(())
    }

    /// The values `configure` substitutes into `etc/Renviron`, chosen for the platform rather
    /// than probed, since the home must work on machines other than the one that built it.
    fn renviron_values(&self) -> Vec<(&'static str, &'static str)> {
        let common = [
            ("R_PAPERSIZE", "a4"),
            ("R_PRINTCMD", ""),
            ("R_RD4PDF", "times,inconsolata,hyper"),
            ("TEXI2DVICMD", "texi2dvi"),
            ("R_GZIPCMD", "gzip -n"),
            ("R_UNZIPCMD", "unzip"),
            ("R_ZIPCMD", "zip"),
            ("R_BZIPCMD", "bzip2"),
            ("LN_S", "ln -s"),
            ("MAKE", "make"),
            ("SED", "sed"),
            ("TAR", "tar"),
            ("STRIP_SHARED_LIB", ""),
            ("STRIP_STATIC_LIB", ""),
            ("configure_input", "Generated by ry from etc/Renviron.in."),
        ];
        // The triplet GNU R's `configure` would report for the building machine's architecture,
        // which `R.version$platform` and package installers read.
        let aarch64 = std::env::consts::ARCH == "aarch64";
        let platform: [(&'static str, &'static str); 4] = match self.platform {
            Platform::Linux => [
                (
                    "R_PLATFORM",
                    if aarch64 {
                        "aarch64-unknown-linux-gnu"
                    } else {
                        "x86_64-pc-linux-gnu"
                    },
                ),
                ("R_BROWSER", "xdg-open"),
                ("PAGER", "less"),
                ("R_PDFVIEWER", "xdg-open"),
            ],
            Platform::MacOs => [
                (
                    "R_PLATFORM",
                    if aarch64 {
                        "aarch64-apple-darwin"
                    } else {
                        "x86_64-apple-darwin"
                    },
                ),
                ("R_BROWSER", "/usr/bin/open"),
                ("PAGER", "/usr/bin/less"),
                ("R_PDFVIEWER", "/usr/bin/open"),
            ],
            Platform::Windows => [
                (
                    "R_PLATFORM",
                    if aarch64 {
                        "aarch64-w64-mingw32"
                    } else {
                        "x86_64-w64-mingw32"
                    },
                ),
                ("R_BROWSER", ""),
                ("PAGER", "internal"),
                ("R_PDFVIEWER", ""),
            ],
        };
        common.into_iter().chain(platform).collect()
    }

    /// `library/base/R/Rprofile`, the system profile: `Common.R` then the platform's part.
    fn build_profile(&self) -> Result<()> {
        let profile = self.sources.source.join("src/library/profile");
        let mut text = read_text(&profile.join("Common.R"))?;
        text.push_str(&read_text(
            &profile.join(format!("Rprofile.{}", self.ostype())),
        )?);
        let directory = self.library("base").join("R");
        fs::create_dir_all(&directory).map_err(io_error(&directory))?;
        write(&directory.join("Rprofile"), &text)
    }

    /// base's R code concatenated into `R/base`, which the session sources directly until
    /// [`Builder::lazy_load_base`] replaces it with the loader.
    fn build_base(&self) -> Result<()> {
        let source = self.package_source("base");
        let library = self.library("base");
        let mut code = self.concatenate(&source, true)?;
        // `@WHICH@` is the `which` program configure found, used by `Sys.which`.
        code = code.replace("@WHICH@", if cfg!(windows) { "" } else { "which" });
        write(&library.join("R/base"), &code)?;
        let description = self.description("base")?;
        let mut text = read_text(&description.join("DESCRIPTION"))?;
        text.push_str(&format!("Built: {}\n", self.built_stamp_for()));
        write(&library.join("DESCRIPTION"), &text)?;
        copy_file(&source.join("inst/CITATION"), &library.join("CITATION"))?;
        let demo = library.join("demo");
        fs::create_dir_all(&demo).map_err(io_error(&demo))?;
        for entry in read_directory(&source.join("demo"))? {
            if entry.file_name().is_some_and(|name| name != "00Index") {
                copy_file(&entry, &demo.join(entry.file_name().unwrap_or_default()))?;
            }
        }
        Ok(())
    }

    /// tools' R code, `DESCRIPTION`, `sysdata` and the API lists `R CMD check` reads from
    /// Writing R Extensions. Its code stays plain source until [`Builder::lazy_load_tools`].
    fn build_tools(&self) -> Result<()> {
        let library = self.library("tools");
        let misc = library.join("misc");
        fs::create_dir_all(&misc).map_err(io_error(&misc))?;
        write(&misc.join("top.txt"), "\n")?;
        self.install_code(
            "tools",
            Code::Plain {
                platform_code: false,
            },
        )?;
        let description = self.description("tools")?;
        let mut text = read_text(&description.join("DESCRIPTION"))?;
        text.push_str(&format!("Built: {}\n", self.built_stamp_for()));
        write(&library.join("DESCRIPTION"), &text)?;
        let manual = self.sources.source.join("doc/manual/R-exts.texi");
        let api: String = read_text(&manual)?
            .lines()
            .filter(|line| {
                ["@api", "@eapi", "@emb", "@for"].iter().any(|prefix| {
                    line.strip_prefix(prefix).is_some_and(|rest| {
                        ["fun", "var", "hdr"]
                            .iter()
                            .any(|kind| rest.starts_with(kind))
                    })
                })
            })
            .map(|line| format!("{line}\n"))
            .collect();
        write(&misc.join("wre.txt"), &api)?;
        self.install_native("tools", Native::Builtin)?;
        self.install_sysdata("tools")?;
        self.run_r(
            &format!(
                "tools:::.install_package_description({}, {})",
                quote(&description),
                quote(&library)
            ),
            "NULL",
        )
    }

    /// A package with only R code, built as base and tools are (`compiler`).
    fn build_simple(&self, name: &str, default_packages: &str) -> Result<()> {
        self.install_code(
            name,
            Code::Plain {
                platform_code: false,
            },
        )?;
        self.install_description(name)?;
        self.make_lazy_loading(name, default_packages)
    }

    fn lazy_load_base(&self) -> Result<()> {
        let source = self.package_source("base");
        // The script refuses to run with any package attached, but every ry session attaches
        // ry's own environments (`tools:ry`, `package:std`, `package:ry`), so the check counts
        // the search path without them.
        let check = "length(search()[search()!=\"Autoloads\"]) != 2";
        let script = read_text(&source.join("makebasedb.R"))?;
        if !script.contains(check) {
            return Err(Error::Package(
                "makebasedb.R no longer checks the search path as ry expects".into(),
            ));
        }
        let script = script.replace(
            check,
            "length(setdiff(search(), c(\"Autoloads\", \"tools:ry\", \"package:std\", \"package:ry\"))) != 2",
        );
        self.run_r(&script, "NULL")?;
        copy_file(
            &source.join("baseloader.R"),
            &self.library("base").join("R/base"),
        )
    }

    fn lazy_load_tools(&self) -> Result<()> {
        let mut script = read_text(&self.package_source("tools").join("R/makeLazyLoad.R"))?;
        script.push_str("\nmakeLazyLoading(\"tools\")\n");
        self.run_r(&script, "NULL")
    }

    fn build_package(&self, package: &Package) -> Result<()> {
        let source = self.package_source(package.name);
        let library = self.library(package.name);
        if package.name == "tcltk" {
            // ry has no Tcl/Tk, so tcltk is installed as GNU R installs it when configure finds
            // none: a stub whose load fails with the reason.
            let directory = library.join("R");
            fs::create_dir_all(&directory).map_err(io_error(&directory))?;
            copy_file(&source.join("R/unix/zzzstub.R"), &directory.join("tcltk"))?;
            copy_file(&source.join("NAMESPACE"), &library.join("NAMESPACE"))?;
            self.install_description(package.name)?;
            copy_tree_except(&source.join("exec"), &library.join("exec"), &[])?;
            return self.install_demos(package.name);
        }
        if package.name != "datasets" {
            self.install_code(package.name, package.code)?;
        } else {
            copy_file(&source.join("NAMESPACE"), &library.join("NAMESPACE"))?;
        }
        for name in package.front {
            copy_file(&source.join(name), &library.join(name))?;
        }
        self.install_description(package.name)?;
        if package.demos {
            self.install_demos(package.name)?;
        }
        for directory in package.inst {
            let from = source.join("inst").join(directory);
            if from.is_dir() {
                copy_files(&from, &library.join(directory))?;
            }
        }
        if package.figures {
            copy_files(&source.join("man/figures"), &library.join("help/figures"))?;
        }
        if package.name == "utils" {
            // glibc cannot list its encodings through the iconv API, so `iconvlist()` reads
            // this file. GNU R's Makefile writes it from `iconv -l` and leaves it empty when
            // that command is missing, which makes `iconvlist()` say it is not available.
            let encodings = Command::new("iconv")
                .arg("-l")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map_or_else(String::new, |output| {
                    String::from_utf8_lossy(&output.stdout).into_owned()
                });
            write(&library.join("iconvlist"), &encodings)?;
        }
        self.install_native(package.name, package.native)?;
        if source.join("R/sysdata.rda").exists() {
            self.install_sysdata(package.name)?;
        }
        match package.name {
            "datasets" => {
                copy_files(&source.join("data"), &library.join("data"))?;
                self.run_r("tools:::data2LazyLoadDB(\"datasets\", compress=3)", "NULL")?;
                copy_file(
                    &source.join("data/morley.tab"),
                    &library.join("data/morley.tab"),
                )?;
                Ok(())
            }
            "methods" => {
                // Loading methods from its sources runs its initial `.onLoad`, which builds the
                // class and method tables and writes the namespace, tables included, to the
                // lazy-load database; the loader then replaces the sources.
                self.run_r("invisible(loadNamespace(\"methods\"))", "NULL")?;
                copy_file(
                    &self.sources.source.join("share/R/nspackloader.R"),
                    &library.join("R/methods"),
                )
            }
            name => self.make_lazy_loading(name, package.default_packages),
        }?;
        if package.name == "grDevices" {
            // GNU R compresses the font metrics after installing them; grDevices reads either.
            for entry in read_directory(&library.join("afm"))? {
                if entry
                    .extension()
                    .is_some_and(|extension| extension == "afm")
                {
                    gzip(&entry)?;
                }
            }
        }
        Ok(())
    }

    /// `R/<package>` holding the package's concatenated sources, and its `NAMESPACE`.
    fn install_code(&self, name: &str, code: Code) -> Result<()> {
        let source = self.package_source(name);
        let library = self.library(name);
        let text = match code {
            Code::Plain { platform_code } => self.concatenate(&source, platform_code)?,
            Code::S4 => format!(
                ".packageName <- \"{name}\"\n{}",
                self.concatenate(&source, false)?
            ),
        };
        let directory = library.join("R");
        fs::create_dir_all(&directory).map_err(io_error(&directory))?;
        write(&directory.join(name), &text)?;
        copy_file(&source.join("NAMESPACE"), &library.join("NAMESPACE"))
    }

    /// `R/*.R`, then `R/<ostype>/*.R` when `platform_code`, each set sorted in the C locale, as
    /// `LC_COLLATE=C ls` orders them for GNU R's build.
    fn concatenate(&self, source: &Path, platform_code: bool) -> Result<String> {
        let mut files = r_files(&source.join("R"))?;
        if platform_code {
            files.extend(r_files(&source.join("R").join(self.ostype()))?);
        }
        let mut text = String::new();
        for file in files {
            text.push_str(&read_text(&file)?);
        }
        Ok(text)
    }

    fn install_description(&self, name: &str) -> Result<()> {
        let description = self.description(name)?;
        self.run_r(
            &format!(
                "tools:::.install_package_description({}, {}, {})",
                quote(&description),
                quote(&self.library(name)),
                quote_text(&self.built_stamp())
            ),
            "NULL",
        )
    }

    /// The directory holding the package's `DESCRIPTION`, generated from `DESCRIPTION.in`.
    fn description(&self, name: &str) -> Result<PathBuf> {
        let template = self.package_source(name).join("DESCRIPTION.in");
        let text = substitute(
            &read_text(&template)?,
            &[("VERSION", self.release.version.as_str())],
        );
        let directory = self.work.join(name);
        fs::create_dir_all(&directory).map_err(io_error(&directory))?;
        write(&directory.join("DESCRIPTION"), &text)?;
        Ok(directory)
    }

    /// The date every `Built:` field carries (GNU R's `PKG_BUILT_STAMP`).
    fn built_stamp(&self) -> String {
        self.release.date.clone()
    }

    /// The `Built:` field that `mkdesc2` writes for base and tools, which tools cannot yet write.
    fn built_stamp_for(&self) -> String {
        format!(
            "R {}; ; {}; {}",
            self.release.version,
            self.release.date,
            self.ostype()
        )
    }

    fn install_demos(&self, name: &str) -> Result<()> {
        if !self.package_source(name).join("demo").is_dir() {
            return Ok(());
        }
        self.run_r(
            &format!(
                "tools:::.install_package_demos({}, {})",
                quote(&self.package_source(name)),
                quote(&self.library(name))
            ),
            "NULL",
        )
    }

    /// The package's shared library: a placeholder for a builtin library, which `dyn.load`
    /// recognises by its path; a compiled library is built at the end
    /// ([`Builder::compile_native`]), once the home can compile.
    fn install_native(&self, name: &str, native: Native) -> Result<()> {
        if native != Native::Builtin {
            return Ok(());
        }
        let libs = self
            .library(name)
            .join(format!("libs{}", self.platform.arch()));
        fs::create_dir_all(&libs).map_err(io_error(&libs))?;
        write(
            &libs.join(format!("{name}{}", self.platform.shlib_ext())),
            "ry builtin library: ry's runtime implements this package's routines itself.\n",
        )
    }

    fn install_sysdata(&self, name: &str) -> Result<()> {
        self.run_r(
            &format!(
                "tools:::sysdata2LazyLoadDB({}, {})",
                quote(&self.package_source(name).join("R/sysdata.rda")),
                quote(&self.library(name).join("R"))
            ),
            "NULL",
        )
    }

    fn make_lazy_loading(&self, name: &str, default_packages: &str) -> Result<()> {
        self.run_r(
            &format!("tools:::makeLazyLoading(\"{name}\")"),
            default_packages,
        )
    }

    /// The parsed help (`help/`), the package indices (`Meta/Rd.rds`, `Meta/hsearch.rds`, the
    /// links and the data index) and the `INDEX` and `html/00Index.html` pages, as the `docs`
    /// target of GNU R's library Makefile builds them.
    fn build_help(&self) -> Result<()> {
        for name in ALL_PACKAGES {
            self.run_r(
                &format!(
                    "options(warn=2); tools:::.install_package_Rd_objects({}, {})",
                    quote(&self.package_source(name)),
                    quote(&self.library(name))
                ),
                "NULL",
            )?;
        }
        self.run_r(
            &format!(
                "tools:::.vinstall_package_indices({}, {}, {})",
                quote(&self.sources.source.join("src/library")),
                quote(&self.root.join("library")),
                quote_text(&ALL_PACKAGES.join(" "))
            ),
            "tools",
        )?;
        for name in ALL_PACKAGES {
            self.run_r(
                &format!(
                    "tools:::.writePkgIndices({}, {})",
                    quote(&self.package_source(name)),
                    quote(&self.library(name))
                ),
                "utils",
            )?;
        }
        Ok(())
    }

    /// Compiles grid and splines against ry's headers and runtime, through an R home assembled
    /// over this build.
    fn compile_native(&self) -> Result<()> {
        let home = self.work.join("home");
        assemble(
            &home,
            &HomeSources {
                include: self.sources.include,
                libr: self.sources.libr,
                rcmd: self.sources.rcmd,
                base: &self.root,
                toolchain: Toolchain::default(),
            },
        )?;
        for package in PACKAGES
            .iter()
            .filter(|package| package.native == Native::Compiled)
        {
            let source = self.package_source(package.name).join("src");
            let build = self.work.join(format!("{}-src", package.name));
            copy_tree_except(&source, &build, &["Makefile.in", "Makefile.win"])?;
            let library_name = format!("{}{}", package.name, self.platform.shlib_ext());
            let built = shlib(&home, &build, &[], Some(&library_name), &[], &[])?;
            let libs = self
                .library(package.name)
                .join(format!("libs{}", self.platform.arch()));
            fs::create_dir_all(&libs).map_err(io_error(&libs))?;
            fs::copy(&built, libs.join(&library_name)).map_err(io_error(&built))?;
        }
        Ok(())
    }

    /// Removes the build's scaffolding and writes the manifest, which marks the build complete.
    fn finish(&self) -> Result<()> {
        fs::remove_dir_all(&self.work).map_err(io_error(&self.work))?;
        remove_if_present(&libr_path(&self.root))?;
        remove_if_present(&self.front_end())?;
        for directory in ["lib", "bin/exec", "bin/x64"] {
            let path = self.root.join(directory);
            if path.is_dir() && read_directory(&path)?.is_empty() {
                fs::remove_dir(&path).map_err(io_error(&path))?;
            }
        }
        write(
            &self.root.join(MANIFEST),
            &format!(
                "R {}\nsource-sha256 {}\nplatform {}\n",
                self.release.version,
                self.sources.checksum,
                self.ostype()
            ),
        )
    }

    /// Runs `code` on ry's interpreter in a session of this home, as GNU R's build runs each step:
    /// `R --vanilla --no-echo` with `R_DEFAULT_PACKAGES` and the C locale, and no library but
    /// the home's own.
    fn run_r(&self, code: &str, default_packages: &str) -> Result<()> {
        let script = self.work.join("step.R");
        write(&script, code)?;
        let mut command = Command::new(self.front_end());
        // `bin/R.exe` is R already, while the Unix front end is `ry-rcmd`, which takes the mode.
        if self.platform != Platform::Windows {
            command.arg("R");
        }
        let output = command
            .args(["--vanilla", "--no-echo", "-f"])
            .arg(&script)
            .current_dir(&self.work)
            .env("R_HOME", &self.root)
            .env("R_DEFAULT_PACKAGES", default_packages)
            .env("LC_ALL", "C")
            .env("R_LIBS", "")
            .env("R_LIBS_USER", "NULL")
            .env("R_LIBS_SITE", "NULL")
            .env("R_SYSTEM_ABI", self.system_abi())
            .env_remove("R_PROFILE_USER")
            .env_remove("R_ENVIRON_USER")
            .output()
            .map_err(|error| Error::Command(first_line(code).to_owned(), error.to_string()))?;
        if output.status.success() {
            return Ok(());
        }
        Err(Error::Command(
            format!("R step `{}`", first_line(code)),
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        ))
    }

    /// GNU R's `R_SYSTEM_ABI`, which tools records in a package's `Built:` field.
    fn system_abi(&self) -> &'static str {
        match self.platform {
            Platform::Linux => "linux,gcc,gxx,gfortran,gfortran",
            Platform::MacOs => "macos,clang,clang++,gfortran,gfortran",
            Platform::Windows => "windows,gcc,gxx,gfortran,gfortran",
        }
    }
}

fn first_line(code: &str) -> &str {
    code.lines()
        .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .unwrap_or(code)
}

/// The `.R` files of a directory (GNU R also takes `.r`, `.S`, `.s` and `.q`, which the base
/// packages do not use), sorted by byte value, as the C locale collates them.
fn r_files(directory: &Path) -> Result<Vec<PathBuf>> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = read_directory(directory)?
        .into_iter()
        .filter(|path| path.is_file() && path.extension().is_some_and(|extension| extension == "R"))
        .collect();
    files.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    Ok(files)
}

/// Replaces each `@NAME@` of `values` in `text`, as `configure` does.
fn substitute(text: &str, values: &[(&str, &str)]) -> String {
    let mut text = text.to_owned();
    for (name, value) in values {
        text = text.replace(&format!("@{name}@"), value);
    }
    text
}

/// An R string literal of a path, with forward slashes, which R accepts on every platform.
fn quote(path: &Path) -> String {
    quote_text(&path.display().to_string().replace('\\', "/"))
}

fn quote_text(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(io_error(path))
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(io_error(parent))?;
    }
    fs::copy(from, to).map_err(io_error(from))?;
    Ok(())
}

/// The files directly in `from`, copied into `to` (GNU R's `instdirs` copies one level).
fn copy_files(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(io_error(to))?;
    for entry in read_directory(from)? {
        if entry.is_file() {
            copy_file(&entry, &to.join(entry.file_name().unwrap_or_default()))?;
        } else if entry.is_dir() {
            copy_tree_except(&entry, &to.join(entry.file_name().unwrap_or_default()), &[])?;
        }
    }
    Ok(())
}

fn copy_tree_except(from: &Path, to: &Path, excluded: &[&str]) -> Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(to).map_err(io_error(to))?;
    for entry in read_directory(from)? {
        let name = entry.file_name().unwrap_or_default();
        if excluded.iter().any(|excluded| name == *excluded) {
            continue;
        }
        if entry.is_dir() {
            copy_tree_except(&entry, &to.join(name), excluded)?;
        } else {
            copy_file(&entry, &to.join(name))?;
        }
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(path.to_path_buf(), error)),
    }
}

/// Compresses `path` to `path.gz` with `gzip -9n` and removes it, as `R_GZIPCMD -9f` does.
fn gzip(path: &Path) -> Result<()> {
    let output = Command::new("gzip")
        .args(["-9nf"])
        .arg(path)
        .output()
        .map_err(|error| Error::Command(format!("gzip {}", path.display()), error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Command(
            format!("gzip {}", path.display()),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}
