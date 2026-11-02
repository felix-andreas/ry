//! ry's R home and `R CMD`: package installation, `R CMD SHLIB` and `R CMD config` in Rust,
//! and the `R` and `Rscript` command lines, which run R code on ry's interpreter.
//!
//! A package is installed the way GNU R's `R CMD INSTALL` installs one, in two halves. The
//! build half is Rust: the package's `configure` script runs with `R_HOME` pointing at ry's R
//! home, and its `src/` directory is compiled by `make` with the package's `Makevars`, the R
//! home's `etc/Makeconf` and `share/make/shlib.mk`, against ry's headers and ry's `libR`. The
//! R-level half (`install.R`) runs on ry's interpreter and calls the `tools` functions GNU R's
//! installer calls, so the installation has GNU R's layout: `Meta/`, a lazy-load database of
//! the package's R code, lazy data, and the help database and indices. Nothing at install time
//! runs a GNU R process.
//!
//! The R home is assembled by [`assemble`]. Base R's packages, `share/` and `etc/` files are
//! linked from a GNU R installation, because ry reuses base R's R code rather than vendoring it
//! (`decisions.md`, "runtime licensing"), while `include/`, `lib/libR.so`, `etc/Makeconf` and
//! the `bin/` front ends are ry's own.

use std::{
    collections::BTreeMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

/// ry implements the API that data.table guards on its revision (`R_mapAttrib`, the resizable
/// vectors) whatever base R it runs, so it reports at least this revision, as the interpreter
/// does (`interp::system::MINIMUM_SVN_REVISION`).
pub const MINIMUM_SVN_REVISION: u32 = 89194;

/// The R-level half of an installation, run on ry's interpreter.
const INSTALL_DRIVER: &str = include_str!("install.R");

/// ry's C headers, `crates/libr/include` as built, by path relative to `include/`.
mod headers {
    include!(concat!(env!("OUT_DIR"), "/headers.rs"));
}

pub mod front_end;
mod makeconf;

use makeconf::{expand_make, make_assignments};

#[derive(Debug)]
pub enum Error {
    Io(PathBuf, io::Error),
    Command(String, String),
    Package(String),
    Usage(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(path, error) => write!(formatter, "{}: {error}", path.display()),
            Error::Command(command, output) => write!(formatter, "{command} failed:\n{output}"),
            Error::Package(message) => write!(formatter, "ERROR: {message}"),
            Error::Usage(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |error| Error::Io(path.to_path_buf(), error)
}

/// The file name of ry's libR in an R home's `lib/`: the name compiled packages link against.
pub const LIBR_FILE: &str = if cfg!(target_os = "macos") {
    "libR.dylib"
} else if cfg!(windows) {
    "R.dll"
} else {
    "libR.so"
};

/// Where ry's libR sits in an R home: `lib/` on Unix, and `bin/x64/` on Windows, where GNU R
/// keeps `R.dll` beside the import library packages link against.
pub fn libr_path(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("bin").join("x64").join(LIBR_FILE)
    } else {
        home.join("lib").join(LIBR_FILE)
    }
}

/// The operating system an R home builds packages for. It fixes the conventions GNU R follows
/// there: the shared-object suffix, the architecture directory under `libs/`, and which
/// platform-specific `Makevars`, `Makefile` and `configure` files take precedence over a
/// package's portable ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
}

impl Platform {
    pub const HOST: Platform = if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Linux
    };

    /// The platform an R home was assembled for, read back from its `etc/Makeconf`, so the
    /// home stays the one source of truth for how its packages are built.
    pub fn of_home(home: &Path) -> Result<Platform> {
        let path = home.join("etc/Makeconf");
        let variables = make_assignments(
            &fs::read_to_string(&path).map_err(io_error(&path))?,
            &home.display().to_string(),
        );
        let get = |name: &str| {
            expand_make(variables.get(name).map_or("", String::as_str), &variables)
                .trim()
                .to_owned()
        };
        Ok(
            match (get("SHLIB_EXT").as_str(), get("DYLIB_EXT").as_str()) {
                (".dll", _) => Platform::Windows,
                (_, ".dylib") => Platform::MacOs,
                _ => Platform::Linux,
            },
        )
    }

    /// The suffix of a package's shared object. macOS uses `.so` too, as GNU R does there.
    pub fn shlib_ext(self) -> &'static str {
        match self {
            Platform::Windows => ".dll",
            Platform::Linux | Platform::MacOs => ".so",
        }
    }

    /// `R_ARCH`: GNU R's Windows build installs a package's libraries under `libs/x64`.
    pub fn arch(self) -> &'static str {
        match self {
            Platform::Windows => "/x64",
            Platform::Linux | Platform::MacOs => "",
        }
    }

    /// The candidates for a build file, most specific first: on Windows `Makevars.ucrt`, then
    /// `Makevars.win`, then `Makevars`. `configure` and `Makefile` have no portable fallback
    /// there, because GNU R does not expect a Unix configure script or Makefile to run on
    /// Windows.
    fn build_files(self, base: &str) -> Vec<String> {
        match self {
            Platform::Windows if matches!(base, "configure" | "Makefile") => {
                vec![format!("{base}.ucrt"), format!("{base}.win")]
            }
            Platform::Windows => vec![
                format!("{base}.ucrt"),
                format!("{base}.win"),
                base.to_owned(),
            ],
            Platform::Linux | Platform::MacOs => vec![base.to_owned()],
        }
    }

    /// The first of a package's build files `directory` holds.
    fn build_file(self, directory: &Path, base: &str) -> Option<PathBuf> {
        self.build_files(base)
            .into_iter()
            .map(|name| directory.join(name))
            .find(|path| path.is_file())
    }
}

/// The toolchain an R home is configured with.
#[derive(Clone, Debug)]
pub struct Toolchain {
    pub cc: String,
    pub cxx: String,
    pub fc: String,
    pub blas_libs: String,
    pub lapack_libs: String,
    pub flibs: String,
    pub cppflags: String,
    pub ldflags: String,
    /// The compiler flag that enables OpenMP, or empty where the compiler has none (Apple's
    /// clang), in which case packages build single-threaded as they do under CRAN's R.
    pub openmp: String,
}

impl Default for Toolchain {
    /// The GNU toolchain with the system's reference BLAS and LAPACK, which is what GNU R's
    /// Linux builds and the package binaries of the corpus link against. On Windows it is
    /// [`Toolchain::rtools`].
    fn default() -> Toolchain {
        if cfg!(windows) {
            return Toolchain::rtools();
        }
        Toolchain {
            cc: "gcc".into(),
            cxx: "g++".into(),
            fc: "gfortran".into(),
            blas_libs: "-lblas".into(),
            lapack_libs: "-llapack".into(),
            flibs: "-lgfortran -lm -lquadmath".into(),
            cppflags: String::new(),
            ldflags: String::new(),
            openmp: "-fopenmp".into(),
        }
    }
}

impl Toolchain {
    /// Rtools, the toolchain CRAN builds Windows packages with and GNU R's Windows installer
    /// expects: its UCRT GCC on `PATH`, and the BLAS and LAPACK DLLs of the R installation.
    pub fn rtools() -> Toolchain {
        Toolchain {
            cc: "gcc".into(),
            cxx: "g++".into(),
            fc: "gfortran".into(),
            blas_libs: r#"-L"$(R_HOME)/bin/x64" -lRblas"#.into(),
            lapack_libs: r#"-L"$(R_HOME)/bin/x64" -lRlapack"#.into(),
            flibs: "-lgfortran -lquadmath".into(),
            cppflags: String::new(),
            ldflags: String::new(),
            openmp: "-fopenmp".into(),
        }
    }

    /// The toolchain for building packages on this platform next to the GNU R at `gnu_r_home`.
    /// On macOS it is the one GNU R's own `etc/Makeconf` names, because the compilers and
    /// libraries vary by installer: CRAN's R uses Apple's clang, the gfortran in `/opt/gfortran`
    /// and its own `libRblas`, Homebrew's uses Homebrew's gcc and OpenBLAS, and neither has a
    /// usable default. On Linux it is [`Toolchain::default`] linking the BLAS and LAPACK that
    /// `Makeconf` names (see [`Toolchain::with_makeconf_libraries`]). On Windows it is Rtools.
    pub fn for_gnu_r_home(gnu_r_home: &Path) -> Result<Toolchain> {
        if cfg!(windows) {
            return Ok(Toolchain::default());
        }
        let path = gnu_r_home.join("etc").join("Makeconf");
        let text = fs::read_to_string(&path).map_err(io_error(&path))?;
        Ok(if cfg!(target_os = "macos") {
            Toolchain::from_makeconf(&text, gnu_r_home)
        } else {
            Toolchain::default().with_makeconf_libraries(&text, gnu_r_home)
        })
    }

    /// This toolchain linking the BLAS and LAPACK that GNU R's `Makeconf` names, where it names
    /// one. A distribution's R names the bare `-lblas` and `-llapack` the default has, but an R
    /// on a system without a global library path records where they are (nixpkgs writes
    /// `-L/nix/store/...-blas-3/lib -lblas`), and the linker finds them nowhere else. An empty
    /// `LAPACK_LIBS` beside a set `BLAS_LIBS` is kept: GNU R then takes LAPACK from the BLAS,
    /// which packages link after `$(LAPACK_LIBS)` anyway.
    pub fn with_makeconf_libraries(self, text: &str, gnu_r_home: &Path) -> Toolchain {
        let variables = make_assignments(text, &gnu_r_home.display().to_string());
        let get = |name: &str| {
            expand_make(variables.get(name).map_or("", String::as_str), &variables)
                .trim()
                .to_owned()
        };
        let blas_libs = get("BLAS_LIBS");
        if blas_libs.is_empty() {
            return self;
        }
        Toolchain {
            blas_libs,
            lapack_libs: get("LAPACK_LIBS"),
            ..self
        }
    }

    /// Reads a toolchain from the text of GNU R's `Makeconf`, expanding `$(VAR)` references
    /// against the file's own assignments and `R_HOME` against `gnu_r_home`, since ry's home
    /// does not hold GNU R's BLAS or LAPACK.
    pub fn from_makeconf(text: &str, gnu_r_home: &Path) -> Toolchain {
        let variables = make_assignments(text, &gnu_r_home.display().to_string());
        let get = |name: &str| {
            expand_make(variables.get(name).map_or("", String::as_str), &variables)
                .trim()
                .to_owned()
        };
        let defaults = Toolchain::default();
        let or = |value: String, fallback: &str| match value.is_empty() {
            true => fallback.to_owned(),
            false => value,
        };
        // `CXX17` is the bare compiler; `CXX` may already carry a `-std` flag.
        let cxx = match get("CXX17") {
            value if value.is_empty() => get("CXX"),
            value => value,
        };
        Toolchain {
            cc: or(get("CC"), "clang"),
            cxx: or(cxx, "clang++"),
            fc: or(get("FC"), &defaults.fc),
            blas_libs: or(get("BLAS_LIBS"), "-framework Accelerate"),
            lapack_libs: or(get("LAPACK_LIBS"), "-framework Accelerate"),
            flibs: get("FLIBS"),
            cppflags: get("CPPFLAGS"),
            ldflags: get("LDFLAGS"),
            openmp: get("SHLIB_OPENMP_CFLAGS"),
        }
    }
}

/// Where the pieces of an R home come from.
pub struct HomeSources<'a> {
    /// ry's headers (`crates/libr/include`).
    pub include: &'a Path,
    /// ry's built `libR.so`, which holds the interpreter.
    pub libr: &'a Path,
    /// The front end that `bin/R` and `bin/Rscript` run: `ry-rcmd`, or `ry` itself.
    pub rcmd: &'a Path,
    /// The GNU R installation (`R RHOME`) whose base packages, `share/` and `etc/` files the
    /// home links to.
    pub gnu_r_home: &'a Path,
    pub toolchain: Toolchain,
}

/// The front ends ry answers itself; every other `bin/` script of GNU R (`build`, `check`,
/// `Rdconv`, `REMOVE` and the rest) is linked, and runs R code through ry's `bin/R`.
const OWN_SCRIPTS: &[&str] = &["R", "Rscript", "Rcmd", "INSTALL", "SHLIB", "config", "exec"];

/// Creates ry's R home at `root`: `include/`, `lib/libR.so` (`libR.dylib`), `etc/Makeconf`,
/// `share/make/shlib.mk` and the `bin/` front ends, with base R's packages in `library/` and
/// the rest of `share/`, `etc/` and `bin/` linked from GNU R.
pub fn assemble(root: &Path, sources: &HomeSources<'_>) -> Result<()> {
    assemble_as(root, root, sources)
}

/// [`assemble`] into `root` a home whose scripts and `Makeconf` name it `home`, where it will be
/// moved.
fn assemble_as(root: &Path, home: &Path, sources: &HomeSources<'_>) -> Result<()> {
    for directory in ["bin", "etc", "lib", "share/make", "library"] {
        let path = root.join(directory);
        fs::create_dir_all(&path).map_err(io_error(&path))?;
    }
    copy_tree(sources.include, &root.join("include"))?;
    let base_description = sources.gnu_r_home.join("library/base/DESCRIPTION");
    let version = BaseVersion::read(&base_description)?;
    // The headers present the version of the base R the interpreter runs, as `R.version` does,
    // so a package's C code and its R code take the same version branches.
    write(&root.join("include/Rversion.h"), &version.header())?;
    let libr = libr_path(root);
    if let Some(parent) = libr.parent() {
        fs::create_dir_all(parent).map_err(io_error(parent))?;
    }
    // Replacing rather than overwriting keeps a running session's mapping of the old file valid.
    // A hard link costs no space, and a rebuild writes a new file rather than into this one.
    remove_file_if_present(&libr)?;
    if fs::hard_link(sources.libr, &libr).is_err() {
        fs::copy(sources.libr, &libr).map_err(io_error(&libr))?;
    }
    // A Windows linker resolves `-lR` through the import library the build wrote next to R.dll.
    let import_library = sources.libr.with_file_name("libR.dll.a");
    if cfg!(windows) && import_library.exists() {
        let installed = root.join("bin/x64/libR.dll.a");
        fs::copy(&import_library, &installed).map_err(io_error(&installed))?;
    }
    write(
        &root.join("etc/Makeconf"),
        &makeconf(home, &sources.toolchain, Platform::HOST),
    )?;
    write(&root.join("share/make/shlib.mk"), SHLIB_MK)?;
    // The interpreter registers an `ry` namespace with no path, so `packageDescription("ry")`
    // searches the library, and `sessionInfo()` fails on a loaded namespace it cannot describe.
    // The version is the one the namespace's `spec` gives (`crates/interp/src/frame.rs`).
    let ry_package = root.join("library/ry");
    fs::create_dir_all(&ry_package).map_err(io_error(&ry_package))?;
    write(
        &ry_package.join("DESCRIPTION"),
        "Package: ry\nVersion: 0.0.0\nPriority: base\nTitle: The ry Runtime and Standard Library\n\
         Description: Built into ry's interpreter.\nLicense: UPL-1.0\n",
    )?;

    let gnu = sources.gnu_r_home;
    let gnu_share = wrapper_directory(gnu, "R_SHARE_DIR").unwrap_or_else(|| gnu.join("share"));
    let gnu_doc = wrapper_directory(gnu, "R_DOC_DIR").unwrap_or_else(|| gnu.join("doc"));
    let base_library = gnu.join("library");
    for entry in read_directory(&base_library)? {
        // A package ry installed into its own library wins over GNU R's copy.
        let target = root
            .join("library")
            .join(entry.file_name().unwrap_or_default());
        if !target.exists() || target.is_symlink() {
            link(&entry, &target)?;
        }
    }
    for entry in read_directory(&gnu_share)? {
        let name = entry.file_name().unwrap_or_default();
        if name == "make" {
            for file in read_directory(&entry)? {
                if file.file_name().is_some_and(|file| file != "shlib.mk") {
                    link(
                        &file,
                        &root
                            .join("share/make")
                            .join(file.file_name().unwrap_or_default()),
                    )?;
                }
            }
        } else {
            link(&entry, &root.join("share").join(name))?;
        }
    }
    if gnu_doc.exists() {
        link(&gnu_doc, &root.join("doc"))?;
    }
    for entry in read_directory(&gnu.join("etc"))? {
        let name = entry.file_name().unwrap_or_default();
        if name != "Makeconf" && name != "ldpaths" {
            link(&entry, &root.join("etc").join(name))?;
        }
    }
    if cfg!(windows) {
        return assemble_windows_front_ends(root, sources, gnu);
    }
    for entry in read_directory(&gnu.join("bin"))? {
        let name = entry.file_name().unwrap_or_default();
        if !OWN_SCRIPTS.iter().any(|own| name == *own) && entry.is_file() {
            link(&entry, &root.join("bin").join(name))?;
        }
    }

    // The scripts run the front end through a link named `ry-rcmd`, which is how a multi-call
    // `ry` knows to act as the front end (`front_end::invoked_as_front_end`).
    let front_end = root.join("bin/exec/ry-rcmd");
    fs::create_dir_all(root.join("bin/exec")).map_err(io_error(&root.join("bin/exec")))?;
    link(sources.rcmd, &front_end)?;
    let rcmd = format!("{}/bin/exec/ry-rcmd", home.display());
    let home = home.display();
    let r_version = &version.version;
    let gnu_doc = gnu_doc.display();
    // `R_SHARE_DIR`, `R_INCLUDE_DIR` and `R_DOC_DIR` stay plain `NAME='value'` lines: the
    // interpreter reads them back from this script, as it does from GNU R's.
    write_script(
        &root.join("bin/R"),
        &format!(
            r#"#!/bin/sh
# ry's R front end. R CMD INSTALL, SHLIB and config are ry's; other R CMD commands are GNU R's
# scripts, which run R code through this front end on ry's interpreter.
R_HOME='{home}'
export R_HOME
R_SHARE_DIR='{home}/share'
export R_SHARE_DIR
R_INCLUDE_DIR='{home}/include'
export R_INCLUDE_DIR
R_DOC_DIR='{gnu_doc}'
export R_DOC_DIR
case "$1" in
RHOME)
    echo "$R_HOME"
    exit 0
    ;;
CMD)
    shift
    exec "$R_HOME/bin/Rcmd" "$@"
    ;;
esac
exec '{rcmd}' R "$@"
"#
        ),
    )?;
    write_script(
        &root.join("bin/Rcmd"),
        &format!(
            r#"#!/bin/sh
# R CMD: ry answers INSTALL, SHLIB and config; any other command is the script of that name in
# bin/, or a program on PATH, run with R's environment.
test $# -eq 0 && exec "$R_HOME/bin/R" --help
R_CMD="$R_HOME/bin/Rcmd"
R_VERSION={r_version}
R_OSTYPE=unix
TEXINPUTS=".:${{TEXINPUTS:+$TEXINPUTS:}}$R_SHARE_DIR/texmf/tex/latex:"
BIBINPUTS=".:${{BIBINPUTS:+$BIBINPUTS:}}$R_SHARE_DIR/texmf/bibtex/bib:"
BSTINPUTS=".:${{BSTINPUTS:+$BSTINPUTS:}}$R_SHARE_DIR/texmf/bibtex/bst:"
export R_CMD R_VERSION R_OSTYPE TEXINPUTS BIBINPUTS BSTINPUTS
if test -f "$R_HOME/etc/Renviron"; then
    . "$R_HOME/etc/Renviron"
    export `sed 's/^ *#.*//; s/^\([^=]*\)=.*/\1/' "$R_HOME/etc/Renviron"`
fi
extra=
case "$1" in
INSTALL|SHLIB|config)
    exec '{rcmd}' CMD "$@"
    ;;
Rd2txt)
    cmd="$R_HOME/bin/Rdconv"
    extra="-t txt"
    ;;
*)
    if test -x "$R_HOME/bin/$1"; then
        cmd="$R_HOME/bin/$1"
    else
        cmd="$1"
    fi
    ;;
esac
shift
exec "$cmd" $extra "$@"
"#
        ),
    )?;
    // `install.packages` refuses source packages unless `bin/INSTALL` exists, and scripts call
    // `bin/SHLIB` and `bin/config` directly, so these forward to ry's `R CMD`.
    for command in ["INSTALL", "SHLIB", "config"] {
        write_script(
            &root.join("bin").join(command),
            &format!("#!/bin/sh\nexec '{home}/bin/R' CMD {command} \"$@\"\n"),
        )?;
    }
    write_script(
        &root.join("bin/Rscript"),
        &format!(
            "#!/bin/sh\n# ry's Rscript: runs an R file or -e expressions on ry's interpreter.\n\
             R_HOME='{home}'\nexport R_HOME\nR_SHARE_DIR='{home}/share'\nexport R_SHARE_DIR\n\
             R_INCLUDE_DIR='{home}/include'\nexport R_INCLUDE_DIR\nR_DOC_DIR='{gnu_doc}'\nexport R_DOC_DIR\n\
             exec '{rcmd}' Rscript \"$@\"\n"
        ),
    )
}

/// ry's R home for the runtime library `libr`, assembled in the user's cache on first use
/// ([`cached_home`]) from `libr`, a front end, and the base R of `gnu_r_home`. The front end is
/// the `ry-rcmd` beside `libr` when there is one (a development build), and otherwise the
/// running executable, which is `ry` (a release, whose runtime is extracted from `ry` itself) or
/// a front end already.
pub fn runtime_home(libr: &Path, gnu_r_home: &Path) -> Result<PathBuf> {
    let beside = libr.with_file_name(if cfg!(windows) {
        "ry-rcmd.exe"
    } else {
        "ry-rcmd"
    });
    let rcmd = if beside.is_file() {
        beside
    } else {
        std::env::current_exe().map_err(io_error(Path::new("the running executable")))?
    };
    let cache = cache_directory()
        .ok_or_else(|| {
            Error::Package(
                "ry's R home lives in a cache directory, and none of XDG_CACHE_HOME, LOCALAPPDATA \
                 (on Windows) or HOME is set"
                    .into(),
            )
        })?
        .join("ry")
        .join("home");
    cached_home(&cache, libr, &rcmd, gnu_r_home)
}

/// The user's cache directory: `XDG_CACHE_HOME`, then `%LOCALAPPDATA%` on Windows, then
/// `~/.cache`. ry's extracted runtime and its R homes both live here, so they must agree.
pub fn cache_directory() -> Option<PathBuf> {
    let variable = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    variable("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            cfg!(windows)
                .then(|| variable("LOCALAPPDATA"))
                .flatten()
                .map(PathBuf::from)
        })
        .or_else(|| variable("HOME").map(|home| Path::new(&home).join(".cache")))
}

/// ry's R home for the runtime `libr` and the front end `rcmd`, assembled under `cache` the
/// first time it is asked for, with ry's embedded headers and the base R of `gnu_r_home`.
/// The home is named by everything it is built from, so a rebuilt `libR` or another GNU R gets
/// a home of its own, and a home in use is never rewritten.
pub fn cached_home(cache: &Path, libr: &Path, rcmd: &Path, gnu_r_home: &Path) -> Result<PathBuf> {
    let stamp = |path: &Path| -> Result<String> {
        let metadata = fs::metadata(path).map_err(io_error(path))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_nanos());
        Ok(format!(
            "{}\0{}\0{modified}",
            path.display(),
            metadata.len()
        ))
    };
    let key = [
        env!("CARGO_PKG_VERSION").to_owned(),
        stamp(libr)?,
        stamp(rcmd)?,
        gnu_r_home.display().to_string(),
    ]
    .join("\n");
    let home = cache.join(format!("{:016x}", fnv1a(key.as_bytes())));
    if home.join("etc").join("Makeconf").exists() {
        return Ok(home);
    }
    fs::create_dir_all(cache).map_err(io_error(cache))?;
    // Assembled beside its final place and renamed into it, so a process that finds the home
    // finds it whole, and two processes assembling at once both succeed.
    let staging = cache.join(format!(
        ".{}-{}",
        home.file_name().unwrap_or_default().to_string_lossy(),
        staging_suffix()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io_error(&staging))?;
    }
    let include = staging.join(".headers");
    for (relative, contents) in headers::HEADERS {
        let path = include.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
        fs::write(&path, contents).map_err(io_error(&path))?;
    }
    let assembled = assemble_as(
        &staging,
        &home,
        &HomeSources {
            include: &include,
            libr,
            rcmd,
            gnu_r_home,
            toolchain: Toolchain::for_gnu_r_home(gnu_r_home)?,
        },
    )
    .and_then(|()| fs::remove_dir_all(&include).map_err(io_error(&include)));
    if let Err(error) = assembled {
        // The assembly error is the one to report; a staging directory left behind wastes disk
        // but is never mistaken for a home.
        drop(fs::remove_dir_all(&staging));
        return Err(error);
    }
    // The runtime and front end a home was built for, so a home for a rebuilt `libR` replaces
    // the old one. Two copies of `ry` that share one extracted runtime keep a home each, since a
    // home in use by the other copy must not be pruned.
    let source = format!("{}\n{}", libr.display(), rcmd.display());
    write(&staging.join(".libr"), &source)?;
    match fs::rename(&staging, &home) {
        Ok(()) => {
            for entry in read_directory(cache)? {
                let stale = entry != home
                    && fs::read_to_string(entry.join(".libr")).is_ok_and(|other| other == source);
                // A session still running from a stale home keeps its mapping of that `libR`.
                // Pruning only saves disk once the new home is in place, so a home that cannot
                // be removed now is tried again when the next home is built.
                if stale {
                    drop(fs::remove_dir_all(&entry));
                }
            }
            Ok(home)
        }
        // Another process renamed its home into place first.
        Err(_) if home.join("etc").join("Makeconf").exists() => fs::remove_dir_all(&staging)
            .map(|()| home)
            .map_err(io_error(&staging)),
        Err(error) => {
            // The rename error is the one to report, as for a failed assembly above.
            drop(fs::remove_dir_all(&staging));
            Err(Error::Io(home, error))
        }
    }
}

/// The Windows front ends: `bin/R.exe`, `bin/Rscript.exe` and `bin/x64/Rcmd.exe` are copies of
/// `ry-rcmd`, which reads which one it is from its own file name and finds the home from its
/// location, because Windows runs no shell scripts. GNU R's own executables are never linked:
/// ry answers every front end, and the BLAS and LAPACK DLLs are the only binaries taken from
/// GNU R, for the packages that link them.
fn assemble_windows_front_ends(root: &Path, sources: &HomeSources<'_>, gnu: &Path) -> Result<()> {
    for front_end in [
        "bin/R.exe",
        "bin/Rscript.exe",
        "bin/x64/R.exe",
        "bin/x64/Rscript.exe",
        "bin/x64/Rcmd.exe",
        "bin/x64/Rterm.exe",
    ] {
        let path = root.join(front_end);
        remove_file_if_present(&path)?;
        // `ry.exe` carries the runtime when it is the front end, so a link saves its size six
        // times over.
        if fs::hard_link(sources.rcmd, &path).is_err() {
            fs::copy(sources.rcmd, &path).map_err(io_error(&path))?;
        }
    }
    for library in ["Rblas.dll", "Rlapack.dll", "Riconv.dll"] {
        let from = gnu.join("bin/x64").join(library);
        if from.exists() {
            let to = root.join("bin/x64").join(library);
            remove_file_if_present(&to)?;
            fs::copy(&from, &to).map_err(io_error(&to))?;
        }
    }
    Ok(())
}

/// A directory GNU R's `bin/R` wrapper exports as a plain `NAME=value` line. Debian keeps
/// `share/` and `doc/` under `/usr/share/R` rather than in the R home.
fn wrapper_directory(gnu_home: &Path, name: &str) -> Option<PathBuf> {
    let wrapper = fs::read_to_string(gnu_home.join("bin/R")).ok()?;
    wrapper.lines().find_map(|line| {
        let (key, value) = line.trim().split_once('=')?;
        let value = value.trim_matches(|character| character == '\'' || character == '"');
        (key == name && !value.is_empty() && !value.contains('$')).then(|| PathBuf::from(value))
    })
}

fn read_directory(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(io_error(directory))? {
        entries.push(entry.map_err(io_error(directory))?.path());
    }
    entries.sort();
    Ok(entries)
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(path.to_path_buf(), error)),
    }
}

/// Points `link_path` at `target`, replacing an older link or file.
fn link(target: &Path, link_path: &Path) -> Result<()> {
    if link_path.is_dir() && !link_path.is_symlink() {
        fs::remove_dir_all(link_path).map_err(io_error(link_path))?;
    } else {
        remove_file_if_present(link_path)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link_path).map_err(io_error(link_path))?;
    // Windows creates symbolic links only with Developer Mode or administrator rights, so
    // without them the home holds a copy. Wine reports success without creating the link,
    // which the existence check catches.
    #[cfg(windows)]
    {
        let linked = if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, link_path)
        } else {
            std::os::windows::fs::symlink_file(target, link_path)
        };
        if linked.is_err() || !link_path.exists() {
            if target.is_dir() {
                copy_tree(target, link_path)?;
            } else {
                fs::copy(target, link_path).map_err(io_error(link_path))?;
            }
        }
    }
    Ok(())
}

/// What an `R` or `Rscript` command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Session {
    /// `--help`: text to print, with no session.
    Print(String),
    /// `--version`, which prints [`version_text`].
    Version,
    /// A session that runs `file`, or `expressions` (`-e`), or else R code read from stdin.
    Run {
        file: Option<String>,
        expressions: Option<String>,
        /// What follows `--args`, which `commandArgs(TRUE)` returns.
        arguments: Vec<String>,
        /// `Rscript --default-packages=`, which becomes `R_DEFAULT_PACKAGES`.
        default_packages: Option<String>,
        /// Off with `--vanilla` or `--no-site-file`.
        site_profile: bool,
        /// Off with `--vanilla` or `--no-init-file`.
        user_profile: bool,
    },
}

/// Which startup profiles a command line turns off, from GNU R's options for them.
struct Profiles {
    site: bool,
    user: bool,
}

impl Profiles {
    /// Reads one option, returning whether it was a profile option.
    fn read(&mut self, option: &str) -> bool {
        match option {
            "--vanilla" => {
                self.site = false;
                self.user = false;
            }
            "--no-site-file" => self.site = false,
            "--no-init-file" => self.user = false,
            _ => return false,
        }
        true
    }
}

/// What `R --version` prints for the R home at `home`.
pub fn version_text(home: &Path) -> Result<String> {
    let version = BaseVersion::read(&home.join("library/base/DESCRIPTION"))?;
    Ok(format!(
        "R version {} ({}-{}-{}) (ry {})\nry's runtime: R code runs on ry's interpreter.\n",
        version.version,
        version.year,
        version.month,
        version.day,
        env!("CARGO_PKG_VERSION")
    ))
}

/// The version of a base package, from its `DESCRIPTION`: the R version ry presents.
struct BaseVersion {
    version: String,
    year: String,
    month: String,
    day: String,
    revision: u32,
}

impl BaseVersion {
    fn read(description: &Path) -> Result<Self> {
        let text = fs::read_to_string(description).map_err(io_error(description))?;
        let field = |name: &str| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key == name).then(|| value.trim().to_owned())
            })
        };
        let version = field("Version").ok_or_else(|| {
            Error::Package(format!("{} has no Version field", description.display()))
        })?;
        // The revision and release date live in `SVN-REVISION` of the GNU R home the base
        // package really belongs to; ry's own home links `library/base` there.
        let svn = description
            .parent()
            .and_then(|base| fs::canonicalize(base).ok())
            .and_then(|base| Some(base.parent()?.parent()?.join("SVN-REVISION")))
            .and_then(|path| fs::read_to_string(path).ok())
            .unwrap_or_default();
        let svn_field = |name: &str| {
            svn.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key == name).then(|| value.trim().to_owned())
            })
        };
        let date = svn_field("Last Changed Date").unwrap_or_default();
        let mut date = date.splitn(3, '-').map(str::to_owned);
        let revision = svn_field("Revision")
            .and_then(|revision| revision.parse().ok())
            .unwrap_or(0);
        Ok(BaseVersion {
            version,
            year: date.next().unwrap_or_default(),
            month: date.next().unwrap_or_default(),
            day: date.next().unwrap_or_default(),
            revision: revision.max(MINIMUM_SVN_REVISION),
        })
    }

    fn header(&self) -> String {
        let parts: Vec<&str> = self.version.split('.').collect();
        let part = |index: usize| parts.get(index).copied().unwrap_or("0");
        let (major, minor, patch) = (part(0), part(1), part(2));
        let revision = self.revision;
        format!(
            "/* Generated by ry's R home assembly from the base package it runs, as R.version reports it. */\n\
             #ifndef R_VERSION_H\n#define R_VERSION_H\n\n\
             #define R_Version(v, p, s) (((v) * 65536) + ((p) * 256) + (s))\n\
             #define R_VERSION R_Version({major}, {minor}, {patch})\n\
             #define R_NICK \"ry\"\n\
             #define R_MAJOR \"{major}\"\n\
             #define R_MINOR \"{minor}.{patch}\"\n\
             #define R_STATUS \"\"\n\
             #define R_YEAR \"{year}\"\n#define R_MONTH \"{month}\"\n#define R_DAY \"{day}\"\n\
             #define R_SVN_REVISION {revision}\n\
             #define R_FILEVERSION {major}, {minor}{patch}, {revision}, 0\n\n#endif\n",
            year = self.year,
            month = self.month,
            day = self.day,
        )
    }
}

/// Reads GNU R's `R [options] [< infile]` command line. `--vanilla`, `--no-site-file` and
/// `--no-init-file` turn the startup profiles off; other session options that change nothing in
/// a non-interactive run on ry (`--no-save`, `--no-echo`, memory sizes) are accepted and
/// ignored.
pub fn r_command_line(arguments: &[String]) -> Result<Session> {
    let mut file = None;
    let mut expressions: Vec<String> = Vec::new();
    let mut rest = arguments.iter();
    let mut trailing = Vec::new();
    let mut profiles = Profiles {
        site: true,
        user: true,
    };
    while let Some(argument) = rest.next() {
        if profiles.read(argument) {
            continue;
        }
        match argument.as_str() {
            "--args" => {
                trailing.extend(rest.by_ref().cloned());
            }
            "--version" => return Ok(Session::Version),
            "-h" | "--help" => {
                return Ok(Session::Print(
                    "Usage: R [options] [< infile] [> outfile]\n   or: R CMD command [arguments]\n\n\
                     Options: --version, --vanilla, --no-echo, -f FILE, -e EXPR, --args\n"
                        .into(),
                ));
            }
            "-f" => {
                file = Some(rest.next().cloned().ok_or_else(|| {
                    Error::Usage("option '-f' requires a filename argument".into())
                })?);
            }
            "-e" => {
                expressions.push(rest.next().cloned().ok_or_else(|| {
                    Error::Usage("option '-e' requires a non-empty argument".into())
                })?);
            }
            "-d" | "-g" | "--encoding" | "--debugger-args" => {
                rest.next();
            }
            other => {
                if let Some(path) = other.strip_prefix("--file=") {
                    file = Some(path.to_owned());
                } else if !other.starts_with('-') {
                    return Err(Error::Usage(format!("ARGUMENT '{other}' __ignored__")));
                }
            }
        }
    }
    Ok(Session::Run {
        // `-f -` and `--file=-` read stdin, as no file does.
        file: file.filter(|path| path != "-"),
        expressions: (!expressions.is_empty()).then(|| expressions.join("\n")),
        arguments: trailing,
        default_packages: None,
        site_profile: profiles.site,
        user_profile: profiles.user,
    })
}

/// Reads `Rscript [options] (FILE | -e EXPR...) [args]`: options come first, then the file or
/// the expressions, and everything after is for `commandArgs(TRUE)`.
pub fn rscript_command_line(arguments: &[String]) -> Result<Session> {
    let mut expressions: Vec<String> = Vec::new();
    let mut default_packages = None;
    let mut profiles = Profiles {
        site: true,
        user: true,
    };
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if profiles.read(argument) {
            index += 1;
        } else if argument == "-e" {
            let expression = arguments
                .get(index + 1)
                .ok_or_else(|| Error::Usage("-e not followed by an expression".into()))?;
            expressions.push(expression.clone());
            index += 2;
        } else if argument == "--version" {
            return Ok(Session::Version);
        } else if let Some(packages) = argument.strip_prefix("--default-packages=") {
            default_packages = Some(packages.to_owned());
            index += 1;
        } else if argument.starts_with("--") {
            index += 1;
        } else {
            break;
        }
    }
    let (file, rest) = if expressions.is_empty() {
        let file = arguments
            .get(index)
            .ok_or_else(|| Error::Usage("Usage: Rscript [options] file [args]\n   or: Rscript [options] -e expr [-e expr2 ...] [args]".into()))?;
        (
            Some(file.clone()),
            arguments.get(index + 1..).unwrap_or(&[]),
        )
    } else {
        (None, arguments.get(index..).unwrap_or(&[]))
    };
    Ok(Session::Run {
        file: file.filter(|path| path != "-"),
        expressions: (!expressions.is_empty()).then(|| expressions.join("\n")),
        arguments: rest.to_vec(),
        default_packages,
        site_profile: profiles.site,
        user_profile: profiles.user,
    })
}

fn write(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).map_err(io_error(path))
}

fn write_script(path: &Path, text: &str) -> Result<()> {
    write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(io_error(path))?;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(io_error(to))?;
    for entry in fs::read_dir(from).map_err(io_error(from))? {
        let entry = entry.map_err(io_error(from))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            fs::copy(&source, &target).map_err(io_error(&target))?;
        }
    }
    Ok(())
}

/// The `etc/Makeconf` of an R home at `root` that builds packages for `platform`: the variables
/// Writing R Extensions documents for `Makevars`, and the suffix rules that compile a package's
/// sources.
pub fn makeconf(root: &Path, toolchain: &Toolchain, platform: Platform) -> String {
    let Toolchain {
        cc,
        cxx,
        fc,
        blas_libs,
        lapack_libs,
        flibs,
        cppflags,
        ldflags,
        openmp,
    } = toolchain;
    // On macOS a package records no libR at all and resolves R's API with a flat lookup when
    // it loads, from the libR already in the process, so the same build loads under ry's
    // runtime and under GNU R (`decisions.md`, "macOS packages bind to ry's libR"). On Linux the
    // `libR.so` dependency is satisfied by soname from whichever libR is loaded.
    // On Windows every DLL names the DLLs it imports from, so packages link R.dll's import
    // library, and position-independent code is the default there.
    let (dylib_ext, shlib_ldflags, libr, pic) = match platform {
        Platform::MacOs => (
            ".dylib",
            "-dynamiclib -Wl,-headerpad_max_install_names -undefined dynamic_lookup",
            "",
            "-fpic",
        ),
        Platform::Linux => (".so", "-shared", r#"-L"$(R_HOME)/lib" -lR"#, "-fpic"),
        Platform::Windows => (
            ".dll",
            "-shared -static-libgcc",
            r#"-L"$(R_HOME)/bin/x64" -lR"#,
            "",
        ),
    };
    let shlib_ext = platform.shlib_ext();
    let r_arch = platform.arch();
    format!(
        r#"# ry's build configuration for packages, read by R CMD INSTALL and R CMD SHLIB.

R_HOME = {home}
R_INCLUDE_DIR = $(R_HOME)/include
R_SHARE_DIR = $(R_HOME)/share
R_ARCH = {r_arch}
SHELL = /bin/sh

CC = {cc}
CFLAGS = -g -O2
CPICFLAGS = {pic}
C99 = $(CC) -std=gnu99
C11 = $(CC) -std=gnu11
C17 = $(CC) -std=gnu17
C23 = $(CC) -std=gnu2x
CXX = {cxx} -std=gnu++17
CXXFLAGS = -g -O2
CXXPICFLAGS = {pic}
CXX11 = {cxx}
CXX11STD = -std=gnu++11
CXX11FLAGS = -g -O2
CXX11PICFLAGS = {pic}
CXX14 = {cxx}
CXX14STD = -std=gnu++14
CXX14FLAGS = -g -O2
CXX14PICFLAGS = {pic}
CXX17 = {cxx}
CXX17STD = -std=gnu++17
CXX17FLAGS = -g -O2
CXX17PICFLAGS = {pic}
CXX20 = {cxx}
CXX20STD = -std=gnu++20
CXX20FLAGS = -g -O2
CXX20PICFLAGS = {pic}
CXX23 = {cxx}
CXX23STD = -std=gnu++23
CXX23FLAGS = -g -O2
CXX23PICFLAGS = {pic}
CXXCPP = $(CXX) -E
CPP = $(CC) -E
FC = {fc}
F77 = {fc}
FFLAGS = -g -O2
FCFLAGS = -g -O2
FPICFLAGS = {pic}
SAFE_FFLAGS = -g -O2 -ffloat-store
FLIBS = {flibs}
FCLIBS_XTRA =
OBJC = $(CC)
OBJCFLAGS = -g -O2
AR = ar
RANLIB = ranlib
LDFLAGS = {ldflags}
CPPFLAGS = {cppflags}
DYLIB_EXT = {dylib_ext}
SHLIB_EXT = {shlib_ext}
SHLIB_LD = $(CC)
SHLIB_LDFLAGS = {shlib_ldflags}
SHLIB_CXXLD = $(CXX)
SHLIB_CXXLDFLAGS = {shlib_ldflags}
SHLIB_FCLD = $(FC)
SHLIB_FCLDFLAGS = {shlib_ldflags}
SHLIB_OPENMP_CFLAGS = {openmp}
SHLIB_OPENMP_CXXFLAGS = {openmp}
SHLIB_OPENMP_FFLAGS = {openmp}
SHLIB_OPENMP_FCFLAGS = {openmp}
BLAS_LIBS = {blas_libs}
LAPACK_LIBS = {lapack_libs}
LIBM = -lm
LIBR = {libr}
LIBINTL =
TCLTK_CPPFLAGS =
TCLTK_LIBS =
JAVA_HOME =
R_XTRA_CPPFLAGS = -I"$(R_INCLUDE_DIR)" -DNDEBUG

ALL_CPPFLAGS = $(R_XTRA_CPPFLAGS) $(PKG_CPPFLAGS) $(CLINK_CPPFLAGS) $(CPPFLAGS)
ALL_CFLAGS = $(PKG_CFLAGS) $(CPICFLAGS) $(SHLIB_CFLAGS) $(CFLAGS)
ALL_CXXFLAGS = $(PKG_CXXFLAGS) $(CXXPICFLAGS) $(SHLIB_CXXFLAGS) $(CXXFLAGS)
ALL_FFLAGS = $(PKG_FFLAGS) $(FPICFLAGS) $(SHLIB_FFLAGS) $(FFLAGS)
ALL_FCFLAGS = $(PKG_FCFLAGS) $(FPICFLAGS) $(SHLIB_FFLAGS) $(FCFLAGS)
ALL_OBJCFLAGS = $(PKG_OBJCFLAGS) $(CPICFLAGS) $(SHLIB_CFLAGS) $(OBJCFLAGS)
ALL_LIBS = $(PKG_LIBS) $(SHLIB_LIBADD) $(LIBR) $(LIBINTL)

.SUFFIXES:
.SUFFIXES: .c .cc .cpp .cxx .f .f90 .f95 .m .o

.c.o:
	$(CC) $(ALL_CPPFLAGS) $(ALL_CFLAGS) -c $< -o $@
.cc.o:
	$(CXX) $(ALL_CPPFLAGS) $(ALL_CXXFLAGS) -c $< -o $@
.cpp.o:
	$(CXX) $(ALL_CPPFLAGS) $(ALL_CXXFLAGS) -c $< -o $@
.cxx.o:
	$(CXX) $(ALL_CPPFLAGS) $(ALL_CXXFLAGS) -c $< -o $@
.f.o:
	$(FC) $(ALL_FFLAGS) -c $< -o $@
.f90.o:
	$(FC) $(ALL_FCFLAGS) -c $< -o $@
.f95.o:
	$(FC) $(ALL_FCFLAGS) -c $< -o $@
.m.o:
	$(OBJC) $(ALL_CPPFLAGS) $(ALL_OBJCFLAGS) -c $< -o $@
"#,
        // GNU make and the MSYS2 shell read a backslash as an escape, so Windows paths use
        // forward slashes, which Windows accepts too.
        home = root.display().to_string().replace('\\', "/")
    )
}

/// `share/make/shlib.mk`: links the package's shared object from its objects.
const SHLIB_MK: &str = "# Links a package's shared object, as R CMD SHLIB and R CMD INSTALL do.

all: $(SHLIB)

$(SHLIB): $(OBJECTS)
\t@if test \"z$(OBJECTS)\" != \"z\"; then \\
\t  echo $(SHLIB_LINK) -o $@ $(OBJECTS) $(ALL_LIBS); \\
\t  $(SHLIB_LINK) -o $@ $(OBJECTS) $(ALL_LIBS); \\
\tfi

shlib-clean:
\t@rm -f $(OBJECTS) $(SHLIB)
";

/// Answers the `R` command line: `CMD INSTALL`, `CMD SHLIB`, `CMD config` and `RHOME`.
pub fn run(home: &Path, arguments: &[String]) -> Result<String> {
    match arguments {
        [rhome] if rhome == "RHOME" => Ok(format!("{}\n", home.display())),
        [cmd, command, rest @ ..] if cmd == "CMD" => match command.as_str() {
            "config" => {
                let [variable] = rest else {
                    return Err(Error::Usage("usage: R CMD config VAR".into()));
                };
                Ok(format!("{}\n", config(home, variable)?))
            }
            "SHLIB" => {
                let directory = std::env::current_dir().map_err(io_error(Path::new(".")))?;
                let mut output = None;
                let mut files = Vec::new();
                let mut link_flags = Vec::new();
                let mut rest = rest.iter();
                while let Some(argument) = rest.next() {
                    if argument == "-o" {
                        output = rest.next().cloned();
                    } else if let Some(name) = argument.strip_prefix("--output=") {
                        output = Some(name.to_owned());
                    } else if argument.starts_with("-l")
                        || argument.starts_with("-L")
                        || argument.ends_with(".a")
                    {
                        // Configure scripts probe for a system library with
                        // `R CMD SHLIB conftest.c -lfoo`, which links it in.
                        link_flags.push(argument.clone());
                    } else if !argument.starts_with('-') {
                        files.push(argument.clone());
                    }
                }
                let library = shlib(
                    home,
                    &directory,
                    &files,
                    output.as_deref(),
                    &[],
                    &link_flags,
                )?;
                Ok(format!("{}\n", library.display()))
            }
            "INSTALL" => {
                let mut library = None;
                let mut build = false;
                let mut options = InstallOptions::default();
                let mut packages = Vec::new();
                let mut rest = rest.iter();
                while let Some(argument) = rest.next() {
                    match argument.as_str() {
                        "-l" | "--library" => library = rest.next().map(PathBuf::from),
                        "--install-tests" => options.tests = true,
                        "--no-test-load" => options.test_load = false,
                        "--build" => build = true,
                        "--no-docs" | "--no-help" => options.driver_flags.push("--no-docs".into()),
                        // A single-architecture build has nothing to skip, and every build
                        // starts from a fresh copy, so there is nothing to clean beforehand.
                        "--no-multiarch" | "--preclean" => {}
                        "--no-data" | "--no-inst" | "--no-demo" | "--no-exec" | "--no-R" => {
                            options.driver_flags.push(argument.clone());
                        }
                        other => {
                            if let Some(path) = other.strip_prefix("--library=") {
                                library = Some(PathBuf::from(path));
                            } else if !other.starts_with('-') {
                                packages.push(PathBuf::from(other));
                            } else {
                                eprintln!("Warning: unknown option '{other}'");
                            }
                        }
                    }
                }
                let library = match library {
                    Some(library) => library,
                    None => default_library(home)?,
                };
                let mut output = String::new();
                for package in packages {
                    let installed = install(home, &package, &library, &options)?;
                    if build {
                        let tarball = package_binary(&installed)?;
                        output.push_str(&format!(
                            "packaged installation of '{}' as {}\n",
                            installed.name,
                            tarball.display()
                        ));
                    }
                    output.push_str(&format!("* DONE ({})\n", installed.name));
                }
                Ok(output)
            }
            other => Err(Error::Usage(format!(
                "ry-rcmd answers R CMD INSTALL, SHLIB and config, not R CMD {other}; run it through bin/R"
            ))),
        },
        _ => Err(Error::Usage(
            "usage: R CMD INSTALL|SHLIB|config ... or R RHOME".into(),
        )),
    }
}

/// The library `R CMD INSTALL` installs into without `-l`: the first of `.libPaths()` in a
/// session of the home's R, as for GNU R, so `R_LIBS`, `R_LIBS_USER` and the site library
/// decide it in the same order.
pub fn default_library(home: &Path) -> Result<PathBuf> {
    let mut command = Command::new(home.join("bin").join("Rscript"));
    command.args(["--no-echo", "--no-restore", "-e", "cat(.libPaths()[1L])"]);
    let output = command
        .output()
        .map_err(|error| Error::Command(format!("{command:?}"), error.to_string()))?;
    if !output.status.success() {
        return Err(Error::Command(
            format!("{command:?}"),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let library = String::from_utf8_lossy(&output.stdout);
    let library = library.trim();
    if library.is_empty() {
        return Err(Error::Package(
            "the R session reported no library to install into".into(),
        ));
    }
    Ok(PathBuf::from(library))
}

/// `R CMD config VAR`: the value `etc/Makeconf` gives the variable.
pub fn config(home: &Path, variable: &str) -> Result<String> {
    if !variable
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(Error::Usage(format!(
            "R CMD config: unknown variable '{variable}'"
        )));
    }
    let makeconf = home.join("etc/Makeconf");
    let printer = format!("print-variable:\n\t@echo $({variable})\n");
    let output = Command::new("make")
        .args(["-s", "--no-print-directory", "-f"])
        .arg(&makeconf)
        .args(["-f", "-", "print-variable"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(printer.as_bytes())?;
            }
            child.wait_with_output()
        })
        .map_err(io_error(&makeconf))?;
    if !output.status.success() {
        return Err(Error::Command(
            format!("make -f {}", makeconf.display()),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A package's `DESCRIPTION`, as ordered fields.
#[derive(Clone, Debug, Default)]
pub struct Description {
    pub fields: Vec<(String, String)>,
}

impl Description {
    pub fn parse(text: &str) -> Description {
        let mut fields: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            if line.starts_with([' ', '\t']) {
                if let Some((_, value)) = fields.last_mut() {
                    value.push('\n');
                    value.push_str(line);
                }
            } else if let Some((name, value)) = line.split_once(':') {
                fields.push((name.trim().to_owned(), value.trim().to_owned()));
            }
        }
        Description { fields }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn set(&mut self, name: &str, value: String) {
        match self.fields.iter_mut().find(|(field, _)| field == name) {
            Some((_, existing)) => *existing = value,
            None => self.fields.push((name.to_owned(), value)),
        }
    }

    /// The package names a dependency field lists, without version constraints.
    pub fn packages(&self, name: &str) -> Vec<String> {
        self.get(name)
            .map(|value| {
                value
                    .split(',')
                    .filter_map(|entry| {
                        let entry = entry.trim();
                        let name = entry
                            .split(|character: char| character.is_whitespace() || character == '(')
                            .next()?;
                        (!name.is_empty() && name != "R").then(|| name.to_owned())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn render(&self) -> String {
        let mut text = String::new();
        for (name, value) in &self.fields {
            text.push_str(&format!("{name}: {value}\n"));
        }
        text
    }
}

/// An installed package.
#[derive(Debug)]
pub struct Installed {
    pub name: String,
    pub directory: PathBuf,
    /// The package's shared object, when it has compiled code.
    pub library: Option<PathBuf>,
}

/// How `R CMD INSTALL` installs a package.
#[derive(Clone, Debug)]
pub struct InstallOptions {
    /// Installs `tests/`, as `--install-tests` does.
    pub tests: bool,
    /// Loads the installed package in a fresh session before it replaces an older copy.
    pub test_load: bool,
    /// `--no-docs`, `--no-data`, `--no-inst`, `--no-demo`, `--no-exec` and `--no-R`, which the
    /// R-level half reads.
    pub driver_flags: Vec<String>,
}

impl Default for InstallOptions {
    fn default() -> InstallOptions {
        InstallOptions {
            tests: false,
            test_load: true,
            driver_flags: Vec::new(),
        }
    }
}

/// Installs a package into `library`: a source package (a directory or a `.tar.gz`), or a binary
/// package that GNU R or ry installed before (it has a `Built:` field), which is copied as it
/// is.
pub fn install(
    home: &Path,
    package: &Path,
    library: &Path,
    options: &InstallOptions,
) -> Result<Installed> {
    fs::create_dir_all(library).map_err(io_error(library))?;
    // Building happens outside the library, as in GNU R: a build tree inside a Cargo workspace
    // (such as a library in ry's own checkout) would make Cargo build the package as a member.
    let staging = tempdir(&std::env::temp_dir())?;
    let result = (|| {
        let directory = if package.is_dir() {
            package.to_path_buf()
        } else {
            unpack(package, &staging)?
        };
        let description_path = directory.join("DESCRIPTION");
        let description = Description::parse(
            &fs::read_to_string(&description_path).map_err(io_error(&description_path))?,
        );
        let name = description
            .get("Package")
            .ok_or_else(|| {
                Error::Package(format!(
                    "{} has no Package field",
                    description_path.display()
                ))
            })?
            .to_owned();
        if let Some(built) = description.get("Built") {
            install_binary(home, &directory, &name, built, library)
        } else {
            install_source(
                home,
                &directory,
                &name,
                description,
                library,
                &staging,
                options,
            )
        }
    })();
    drop(fs::remove_dir_all(&staging));
    result
}

/// The FNV-1a hash of `bytes`, which names a cached home the same way in every build of ry.
/// A name part unique to this process and moment, for staging files and directories that are
/// renamed into place. The process id alone is not enough: containers sharing a cache volume
/// often run with the same process id.
pub fn staging_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.subsec_nanos());
    format!("{}-{nanos:09}", std::process::id())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn tempdir(near: &Path) -> Result<PathBuf> {
    for attempt in 0..1000u32 {
        let path = near.join(format!("ry-install-{}-{attempt}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(Error::Io(path, error)),
        }
    }
    Err(Error::Package("cannot create a staging directory".into()))
}

/// `R CMD INSTALL --build`: packs the installed package into
/// `<name>_<version>_R_<platform>.tar.gz` in the working directory, the binary package
/// `install.packages` and `R CMD INSTALL` take back.
fn package_binary(installed: &Installed) -> Result<PathBuf> {
    let description_path = installed.directory.join("DESCRIPTION");
    let description = Description::parse(
        &fs::read_to_string(&description_path).map_err(io_error(&description_path))?,
    );
    let field = |name: &str| {
        description.get(name).map(str::to_owned).ok_or_else(|| {
            Error::Package(format!(
                "{} has no {name} field",
                description_path.display()
            ))
        })
    };
    let version = field("Version")?;
    let built = field("Built")?;
    let platform = built.split(';').nth(1).map(str::trim).unwrap_or_default();
    let tarball = PathBuf::from(format!("{}_{version}_R_{platform}.tar.gz", installed.name));
    let parent = installed.directory.parent().ok_or_else(|| {
        Error::Package(format!("{} has no parent", installed.directory.display()))
    })?;
    let absolute = std::env::current_dir()
        .map_err(io_error(Path::new(".")))?
        .join(&tarball);
    run_command(
        Command::new("tar")
            .arg("-czf")
            .arg(&absolute)
            .arg("-C")
            .arg(parent)
            .arg(&installed.name),
    )?;
    Ok(tarball)
}

/// Unpacks a tarball into `staging` and returns the package directory inside it.
fn unpack(tarball: &Path, staging: &Path) -> Result<PathBuf> {
    let unpacked = staging.join("unpacked");
    fs::create_dir_all(&unpacked).map_err(io_error(&unpacked))?;
    run_command(
        Command::new("tar")
            .arg("-xf")
            .arg(tarball)
            .arg("-C")
            .arg(&unpacked),
    )?;
    let mut entries = fs::read_dir(&unpacked)
        .map_err(io_error(&unpacked))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("DESCRIPTION").exists());
    entries
        .next()
        .ok_or_else(|| Error::Package(format!("{} holds no package", tarball.display())))
}

fn run_command(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .map_err(|error| Error::Command(format!("{command:?}"), error.to_string()))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(text)
    } else {
        Err(Error::Command(format!("{command:?}"), text))
    }
}

/// A binary package was built by an R that presents a version in `Built:`. GNU R refuses only
/// builds before R 3.0.0, and loads, for example, a 4.5 build under 4.6; ry does the same.
fn install_binary(
    home: &Path,
    directory: &Path,
    name: &str,
    built: &str,
    library: &Path,
) -> Result<Installed> {
    let version = built
        .trim_start_matches("R ")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let major: u32 = version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .ok_or_else(|| {
            Error::Package(format!(
                "package '{name}' has an invalid Built field: {built}"
            ))
        })?;
    if major < 3 {
        return Err(Error::Package(format!(
            "package '{name}' was built before R 3.0.0: please re-install it"
        )));
    }
    if !built.contains(std::env::consts::ARCH) && built.contains("-") {
        let platform = built.split(';').nth(1).unwrap_or("").trim();
        if !platform.is_empty() && !platform.starts_with(std::env::consts::ARCH) {
            return Err(Error::Package(format!(
                "package '{name}' was built for {platform}"
            )));
        }
    }
    let target = library.join(name);
    replace_directory(directory, &target)?;
    let platform = Platform::of_home(home)?;
    let libs = libs_directory(&target, platform);
    let extension = platform.shlib_ext();
    let shared = libs.join(format!("{}{extension}", name.replace('.', "_")));
    let library_path = [libs.join(format!("{name}{extension}")), shared]
        .into_iter()
        .find(|path| path.exists());
    Ok(Installed {
        name: name.to_owned(),
        directory: target,
        library: library_path,
    })
}

fn replace_directory(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        fs::remove_dir_all(to).map_err(io_error(to))?;
    }
    copy_tree(from, to)
}

/// The library directories packages are found in: `library` itself, then `R_LIBS`, then the
/// R home's own library.
fn library_paths(home: &Path, library: &Path) -> Vec<PathBuf> {
    let mut paths = vec![library.to_path_buf()];
    if let Some(value) = std::env::var_os("R_LIBS") {
        paths.extend(std::env::split_paths(&value));
    }
    paths.push(home.join("library"));
    paths
}

fn find_package(paths: &[PathBuf], name: &str) -> Option<PathBuf> {
    paths
        .iter()
        .map(|path| path.join(name))
        .find(|path| path.join("DESCRIPTION").exists())
}

fn install_source(
    home: &Path,
    source: &Path,
    name: &str,
    mut description: Description,
    library: &Path,
    staging: &Path,
    options: &InstallOptions,
) -> Result<Installed> {
    // Building happens in a copy, because configure and make write into the source tree.
    let build = staging.join("build").join(name);
    copy_tree(source, &build)?;
    let staged_library = staging.join("installed");
    let target_staging = staged_library.join(name);
    fs::create_dir_all(&target_staging).map_err(io_error(&target_staging))?;
    let platform = Platform::of_home(home)?;
    let paths = library_paths(home, library);
    let src = build.join("src");
    let mut shared_object = None;
    let mut install_libs = false;
    if let Some(library_name) =
        compile_in(home, &build, name, &description, &paths, &target_staging)?
    {
        // GNU R builds a source directory in place, so its shared objects stay in `src`, where
        // `pkgbuild::compile_dll` and `pkgload::load_all` look for them after a libs-only
        // install. The build here runs in a copy, so the objects go back to the source tree.
        let source_src = source.join("src");
        if !source.starts_with(staging) && source_src.is_dir() {
            for entry in fs::read_dir(&src).map_err(io_error(&src))? {
                let path = entry.map_err(io_error(&src))?.path();
                let Some(file) = path.file_name().filter(|_| {
                    path.extension()
                        .is_some_and(|extension| Some(extension) == extension_of(platform))
                }) else {
                    continue;
                };
                let built = source_src.join(file);
                fs::copy(&path, &built).map_err(io_error(&built))?;
            }
        }
        // A package's `src/install.libs.R` installs its own libraries, and runs as R code in
        // the driver; otherwise every shared object `src` holds is installed, as GNU R does.
        if src.join("install.libs.R").exists() {
            install_libs = true;
        } else {
            let libs = libs_directory(&target_staging, platform);
            for entry in fs::read_dir(&src).map_err(io_error(&src))? {
                let path = entry.map_err(io_error(&src))?.path();
                let Some(file) = path.file_name().filter(|_| {
                    path.extension()
                        .is_some_and(|extension| Some(extension) == extension_of(platform))
                }) else {
                    continue;
                };
                fs::create_dir_all(&libs).map_err(io_error(&libs))?;
                let installed = libs.join(file);
                fs::copy(&path, &installed).map_err(io_error(&installed))?;
            }
            let symbols = src.join("symbols.rds");
            if symbols.exists() && libs.is_dir() {
                fs::copy(&symbols, libs.join("symbols.rds")).map_err(io_error(&symbols))?;
            }
        }
        shared_object = Some(library_name);
    }
    let renamed = format!("{}{}", name.replace('.', "_"), platform.shlib_ext());
    // `tools:::.install_package_description` copies the build's DESCRIPTION and adds `Built:`,
    // so the field that marks an installation as ry's goes in first.
    description.set("RyBuilt", env!("CARGO_PKG_VERSION").to_owned());
    write(&build.join("DESCRIPTION"), &description.render())?;
    let target = library.join(name);
    let driver = staging.join("install.R");
    write(&driver, INSTALL_DRIVER)?;
    let mut driver_arguments = vec![
        build.display().to_string(),
        staged_library.display().to_string(),
        target.display().to_string(),
    ];
    driver_arguments.extend(options.driver_flags.iter().cloned());
    if options.tests {
        driver_arguments.push("--install-tests".into());
    }
    if install_libs {
        driver_arguments.push("--install-libs".into());
    }
    let r_libs =
        std::env::join_paths(std::iter::once(staged_library.clone()).chain(paths.iter().cloned()))
            .map(|joined| joined.to_string_lossy().into_owned())
            .unwrap_or_default();
    run_r(home, &build, &driver, &driver_arguments, &r_libs).map_err(|error| {
        Error::Package(format!(
            "installing the R code of package '{name}' failed\n{error}"
        ))
    })?;
    if options.test_load {
        let check = staging.join("test-load.R");
        write(
            &check,
            "arguments <- commandArgs(TRUE)\ntools:::.test_load_package(arguments[[1L]], arguments[[2L]])\n",
        )?;
        run_r(
            home,
            staging,
            &check,
            &[name.to_owned(), staged_library.display().to_string()],
            &r_libs,
        )
        .map_err(|error| Error::Package(format!("loading failed for package '{name}'\n{error}")))?;
    }
    if target.exists() {
        fs::remove_dir_all(&target).map_err(io_error(&target))?;
    }
    fs::rename(&target_staging, &target)
        .or_else(|_| {
            copy_tree(&target_staging, &target).map_err(|error| io::Error::other(error.to_string()))
        })
        .map_err(io_error(&target))?;
    Ok(Installed {
        name: name.to_owned(),
        library: shared_object.and_then(|file| {
            [file, renamed]
                .into_iter()
                .map(|file| libs_directory(&target, platform).join(file))
                .find(|path| path.exists())
        }),
        directory: target,
    })
}

/// Builds the compiled code of the package source tree at `package` in place, against ry's R
/// home `home`, as `R CMD INSTALL --libs-only` builds it but without installing anything: its
/// `configure` script runs, then `src/` is compiled into `src/<package><ext>`, which `ry load`
/// then loads. The headers of the packages in `LinkingTo` are searched for in `libraries`, the
/// session's library paths, and then in the home's own library.
pub fn compile(home: &Path, package: &Path, libraries: &[PathBuf]) -> Result<()> {
    let description_path = package.join("DESCRIPTION");
    let description = Description::parse(
        &fs::read_to_string(&description_path).map_err(io_error(&description_path))?,
    );
    let name = description
        .get("Package")
        .ok_or_else(|| {
            Error::Package(format!(
                "{} has no Package field",
                description_path.display()
            ))
        })?
        .to_owned();
    // `configure` scripts may write into `R_PACKAGE_DIR`, which an installation keeps and a load
    // has no use for.
    let target = tempdir(&std::env::temp_dir())?;
    let mut paths = libraries.to_vec();
    paths.push(home.join("library"));
    let built = compile_in(home, package, &name, &description, &paths, &target);
    let removed = fs::remove_dir_all(&target).map_err(io_error(&target));
    built.and(removed)
}

/// Runs the `configure` script of the source tree `build` and compiles its `src/`, returning the
/// name of the shared object the package's name gives it, or `None` when it has no `src/`.
fn compile_in(
    home: &Path,
    build: &Path,
    name: &str,
    description: &Description,
    paths: &[PathBuf],
    target: &Path,
) -> Result<Option<String>> {
    let platform = Platform::of_home(home)?;
    let environment = build_environment(home, platform, name, target, paths)?;
    if let Some(configure) = platform.build_file(build, "configure") {
        run_command(
            Command::new("sh").arg(&configure).current_dir(build).envs(
                environment
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            ),
        )
        .map_err(|error| {
            Error::Package(format!(
                "configuration failed for package '{name}'\n{error}"
            ))
        })?;
    }
    let src = build.join("src");
    if !src.is_dir() {
        return Ok(None);
    }
    let include_flags: Vec<String> = description
        .packages("LinkingTo")
        .iter()
        .filter_map(|dependency| find_package(paths, dependency))
        .map(|directory| format!("-I\"{}\"", directory.join("include").display()))
        .collect();
    // GNU R links `<package><ext>` with the package name as it is: a package whose name has a
    // dot renames the result in its Makevars (data.table's `mv $(SHLIB) data_table.dll`).
    let library_name = format!("{name}{}", platform.shlib_ext());
    if let Some(makefile) = platform.build_file(&src, "Makefile") {
        run_command(
            Command::new("make")
                .arg("-f")
                .arg(&makefile)
                .current_dir(&src)
                .envs(
                    environment
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                ),
        )
        .map_err(|error| {
            Error::Package(format!("compilation failed for package '{name}'\n{error}"))
        })?;
    } else {
        shlib(home, &src, &[], Some(&library_name), &include_flags, &[]).map_err(|error| {
            Error::Package(format!("compilation failed for package '{name}'\n{error}"))
        })?;
    }
    Ok(Some(library_name))
}

/// Runs an R file on ry's interpreter through the home's `bin/R`, in `directory`, with
/// `R_LIBS` set to `r_libs`, and returns its output or the output of the failed run.
fn run_r(
    home: &Path,
    directory: &Path,
    file: &Path,
    arguments: &[String],
    r_libs: &str,
) -> Result<String> {
    let platform = Platform::of_home(home)?;
    let front_end = if platform == Platform::Windows {
        home.join("bin").join("R.exe")
    } else {
        home.join("bin").join("R")
    };
    run_command(
        Command::new(front_end)
            .args(["--vanilla", "--no-echo", "-f"])
            .arg(file)
            .arg("--args")
            .args(arguments)
            .current_dir(directory)
            .env("R_LIBS", r_libs)
            .env("R_TESTS", "")
            .env("R_ARCH", platform.arch())
            .env("SHLIB_EXT", platform.shlib_ext())
            .env("LC_COLLATE", "C"),
    )
}

/// Where a package's shared objects go: `libs/`, or `libs/x64/` on Windows.
fn libs_directory(package: &Path, platform: Platform) -> PathBuf {
    package.join(format!("libs{}", platform.arch()))
}

fn extension_of(platform: Platform) -> Option<&'static std::ffi::OsStr> {
    Some(std::ffi::OsStr::new(
        platform.shlib_ext().trim_start_matches('.'),
    ))
}

/// `directory` put in front of the `PATH` this process runs with.
fn path_with(directory: &Path) -> Result<String> {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(
        std::iter::once(directory.to_path_buf()).chain(std::env::split_paths(&inherited)),
    )
    .map(|joined| joined.to_string_lossy().into_owned())
    .map_err(|error| {
        Error::Package(format!(
            "cannot put {} on PATH: {error}",
            directory.display()
        ))
    })
}

/// A path as make and the shell read it. Both take a backslash for an escape, so Windows paths
/// use forward slashes, which Windows accepts too.
fn shell_path(path: &Path) -> String {
    let text = path.display().to_string();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    }
}

/// The environment `configure` and `make` run in, as GNU R's installer sets it.
fn build_environment(
    home: &Path,
    platform: Platform,
    name: &str,
    target: &Path,
    paths: &[PathBuf],
) -> Result<BTreeMap<String, String>> {
    let mut environment = BTreeMap::new();
    environment.insert("PATH".to_owned(), path_with(&home.join("bin"))?);
    environment.insert("R_HOME".to_owned(), shell_path(home));
    environment.insert("R_ARCH".to_owned(), platform.arch().to_owned());
    environment.insert("R_PACKAGE_NAME".to_owned(), name.to_owned());
    environment.insert("R_PACKAGE_DIR".to_owned(), shell_path(target));
    environment.insert(
        "R_INCLUDE_DIR".to_owned(),
        shell_path(&home.join("include")),
    );
    let libraries: Vec<String> = paths.iter().map(|path| shell_path(path)).collect();
    let libraries = std::env::join_paths(&libraries)
        .map_err(|error| Error::Package(format!("cannot join library paths: {error}")))?;
    environment.insert(
        "R_LIBS".to_owned(),
        libraries.to_string_lossy().into_owned(),
    );
    Ok(environment)
}

/// What the C++ standard in `Makevars` (`CXX_STD = CXX17`) selects, as `CXX` and its flags.
fn cxx_standard(makevars: &str) -> Option<String> {
    makevars.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == "CXX_STD")
            .then(|| value.trim().to_owned())
            .filter(|value| value.starts_with("CXX") && value.len() > 3)
    })
}

/// Compiles the sources in `directory` into a shared object, as `R CMD SHLIB` does: `make` with
/// the package's `Makevars`, the R home's `Makeconf` and `shlib.mk`. With no `files`, every
/// C, C++ and Fortran source in the directory is compiled, unless `Makevars` names its own
/// `OBJECTS`. `link_flags` (`-l`, `-L` and archives) are added to the link line.
pub fn shlib(
    home: &Path,
    directory: &Path,
    files: &[String],
    output: Option<&str>,
    include_flags: &[String],
    link_flags: &[String],
) -> Result<PathBuf> {
    let platform = Platform::of_home(home)?;
    let makevars = platform.build_file(directory, "Makevars");
    let makevars_text = makevars
        .as_ref()
        .map(|path| fs::read_to_string(path).unwrap_or_default())
        .unwrap_or_default();
    let sources: Vec<String> = if files.is_empty() {
        let mut found: Vec<String> = fs::read_dir(directory)
            .map_err(io_error(directory))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                Path::new(name).extension().is_some_and(|extension| {
                    matches!(
                        extension.to_str(),
                        Some("c" | "cc" | "cpp" | "cxx" | "f" | "f90" | "f95")
                    )
                })
            })
            .collect();
        found.sort();
        found
    } else {
        files.to_vec()
    };
    let objects: Vec<String> = sources
        .iter()
        .filter_map(|source| {
            let (stem, extension) = source.rsplit_once('.')?;
            matches!(
                extension,
                "c" | "cc" | "cpp" | "cxx" | "f" | "f90" | "f95" | "m" | "o"
            )
            .then(|| format!("{stem}.o"))
        })
        .collect();
    let has = |extensions: &[&str]| {
        sources.iter().any(|source| {
            source
                .rsplit_once('.')
                .is_some_and(|(_, extension)| extensions.contains(&extension))
        })
    };
    let cxx = has(&["cc", "cpp", "cxx"]) || makevars_text.contains("CXX_STD");
    let fortran = has(&["f", "f90", "f95"]);
    let output = output.map_or_else(
        || {
            let stem = sources
                .first()
                .and_then(|source| source.rsplit_once('.').map(|(stem, _)| stem.to_owned()))
                .unwrap_or_else(|| "shlib".to_owned());
            format!("{stem}{}", platform.shlib_ext())
        },
        str::to_owned,
    );
    let mut command = Command::new("make");
    command.current_dir(directory);
    if let Some(makevars) = &makevars {
        command.arg("-f").arg(makevars);
    }
    command.arg("-f").arg(home.join("etc/Makeconf"));
    if let Some(site) = site_makevars(home) {
        command.arg("-f").arg(site);
    }
    command.arg("-f").arg(home.join("share/make/shlib.mk"));
    // The user's Makevars comes last so it overrides the toolchain, as in GNU R; pkgbuild hides
    // the compilers by pointing `R_MAKEVARS_USER` at one that sets `CC = test`.
    if let Some(user) = user_makevars() {
        command.arg("-f").arg(user);
    }
    command.arg(format!("SHLIB={output}"));
    if !makevars_text.lines().any(|line| {
        line.split_once('=')
            .is_some_and(|(name, _)| name.trim().trim_end_matches(['+', ':']) == "OBJECTS")
    }) {
        command.arg(format!("OBJECTS={}", objects.join(" ")));
    }
    if let Some(standard) = cxx_standard(&makevars_text) {
        command.arg(format!("CXX=$({standard}) $({standard}STD)"));
        command.arg(format!("CXXFLAGS=$({standard}FLAGS)"));
        command.arg(format!("CXXPICFLAGS=$({standard}PICFLAGS)"));
        command.arg(format!("SHLIB_CXXLD=$({standard}) $({standard}STD)"));
    }
    let linker = if cxx {
        "$(SHLIB_CXXLD) $(SHLIB_CXXLDFLAGS) $(LDFLAGS)"
    } else if fortran {
        "$(SHLIB_FCLD) $(SHLIB_FCLDFLAGS) $(LDFLAGS)"
    } else {
        "$(SHLIB_LD) $(SHLIB_LDFLAGS) $(LDFLAGS)"
    };
    command.arg(format!("SHLIB_LINK={linker}"));
    let mut library_additions = Vec::new();
    if fortran {
        library_additions.push("$(FLIBS)".to_owned());
    }
    library_additions.extend(link_flags.iter().cloned());
    if !library_additions.is_empty() {
        command.arg(format!("SHLIB_LIBADD={}", library_additions.join(" ")));
    }
    if !include_flags.is_empty() {
        command.arg(format!("CLINK_CPPFLAGS={}", include_flags.join(" ")));
    }
    command.env("R_HOME", home);
    command.env("PATH", path_with(&home.join("bin"))?);
    run_command(&mut command)?;
    Ok(directory.join(output))
}

/// The site Makevars GNU R reads after `Makeconf`: `R_MAKEVARS_SITE`, else `etc/Makevars.site`.
fn site_makevars(home: &Path) -> Option<PathBuf> {
    match std::env::var_os("R_MAKEVARS_SITE") {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => Some(home.join("etc/Makevars.site")).filter(|path| path.exists()),
    }
}

/// The user Makevars GNU R reads last: `R_MAKEVARS_USER`, else `~/.R/Makevars-<platform>`, else
/// `~/.R/Makevars`.
fn user_makevars() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("R_MAKEVARS_USER").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let directory = PathBuf::from(std::env::var_os("HOME")?).join(".R");
    let platform = std::env::var("R_PLATFORM")
        .ok()
        .filter(|platform| !platform.is_empty());
    platform
        .map(|platform| directory.join(format!("Makevars-{platform}")))
        .into_iter()
        .chain([directory.join("Makevars")])
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_parse_continuations_and_dependencies() {
        let description = Description::parse(
            "Package: demo\nLinkingTo: Rcpp (>= 1.0),\n    cpp11\nImports: R (>= 4.0), utils\n",
        );
        assert_eq!(description.get("Package"), Some("demo"));
        assert_eq!(description.packages("LinkingTo"), ["Rcpp", "cpp11"]);
        assert_eq!(description.packages("Imports"), ["utils"]);
    }

    #[test]
    fn the_cxx_standard_comes_from_makevars() {
        assert_eq!(cxx_standard("CXX_STD = CXX17\n"), Some("CXX17".to_owned()));
        assert_eq!(cxx_standard("PKG_LIBS = -lz\n"), None);
    }

    /// An excerpt of the `etc/Makeconf` of CRAN's R 4.5 for Apple silicon.
    const CRAN_MACOS_MAKECONF: &str = "\
R_ARCH =
CC = clang -arch arm64
CXX = clang++ -arch arm64 -std=gnu++17
CXX17 = clang++ -arch arm64
CPPFLAGS = -I/opt/R/arm64/include
LDFLAGS = -L/opt/R/arm64/lib
FC = /opt/gfortran/bin/gfortran -mtune=native
FLIBS =  -L/opt/gfortran/lib/gcc/aarch64-apple-darwin20.0/14.2.0 \\
 -L/opt/gfortran/lib -lgfortran -lemutls_w -lheapt_w -lquadmath
BLAS_LIBS = -L\"$(R_HOME)/lib$(R_ARCH)\" -lRblas
LAPACK_LIBS = -L\"$(R_HOME)/lib$(R_ARCH)\" -lRlapack
SHLIB_OPENMP_CFLAGS =
\tthis line is a recipe = and is skipped
";

    #[test]
    fn a_macos_toolchain_comes_from_gnu_r_makeconf() {
        let home = Path::new("/Library/Frameworks/R.framework/Resources");
        let toolchain = Toolchain::from_makeconf(CRAN_MACOS_MAKECONF, home);
        assert_eq!(toolchain.cc, "clang -arch arm64");
        assert_eq!(toolchain.cxx, "clang++ -arch arm64");
        assert_eq!(toolchain.fc, "/opt/gfortran/bin/gfortran -mtune=native");
        assert_eq!(
            toolchain.flibs,
            "-L/opt/gfortran/lib/gcc/aarch64-apple-darwin20.0/14.2.0 -L/opt/gfortran/lib \
             -lgfortran -lemutls_w -lheapt_w -lquadmath"
        );
        assert_eq!(
            toolchain.blas_libs,
            "-L\"/Library/Frameworks/R.framework/Resources/lib\" -lRblas"
        );
        assert_eq!(toolchain.cppflags, "-I/opt/R/arm64/include");
        assert_eq!(toolchain.ldflags, "-L/opt/R/arm64/lib");
        assert_eq!(toolchain.openmp, "", "Apple's clang has no OpenMP");
    }

    #[test]
    fn a_linux_toolchain_links_the_blas_and_lapack_gnu_r_makeconf_names() {
        let home = Path::new("/nix/store/abc-R-4.5.2/lib/R");
        let nix = "BLAS_LIBS = -L/nix/store/def-blas-3/lib -lblas\n\
                   LAPACK_LIBS = -L/nix/store/ghi-lapack-3/lib -llapack\n";
        let toolchain = Toolchain::default().with_makeconf_libraries(nix, home);
        assert_eq!(toolchain.blas_libs, "-L/nix/store/def-blas-3/lib -lblas");
        assert_eq!(
            toolchain.lapack_libs,
            "-L/nix/store/ghi-lapack-3/lib -llapack"
        );
        assert_eq!(toolchain.cc, Toolchain::default().cc);
        let internal = "BLAS_LIBS = -L\"$(R_HOME)/lib$(R_ARCH)\" -lRblas\nLAPACK_LIBS =\n";
        let toolchain = Toolchain::default().with_makeconf_libraries(internal, home);
        assert_eq!(
            toolchain.blas_libs,
            "-L\"/nix/store/abc-R-4.5.2/lib/R/lib\" -lRblas"
        );
        assert_eq!(toolchain.lapack_libs, "");
        let toolchain = Toolchain::default().with_makeconf_libraries("CC = gcc\n", home);
        assert_eq!(toolchain.blas_libs, "-lblas");
        assert_eq!(toolchain.lapack_libs, "-llapack");
    }

    #[test]
    fn make_expansion_appends_and_stops_on_self_reference() {
        let variables = make_assignments(
            "A = x\nA += y\nB ?= $(A) ${C}\nB ?= no\nLOOP = $(LOOP)\n",
            "/r",
        );
        assert_eq!(expand_make("$(B)", &variables), "x y ");
        assert_eq!(expand_make("$(LOOP) $$ $(R_HOME)", &variables), " $ /r");
    }

    #[test]
    fn make_expansion_nests_keeps_quoted_spaces_and_trailing_blanks() {
        let variables = make_assignments(
            "STD = 17\nCXX17 = clang++\nPADDED = 17 # the blank before # stays\n",
            "/My R",
        );
        assert_eq!(expand_make("$(CXX$(STD))", &variables), "clang++");
        assert_eq!(expand_make("$(CXX$(PADDED))", &variables), "");
        assert_eq!(
            expand_make("-L\"$(R_HOME)/lib\"", &variables),
            "-L\"/My R/lib\""
        );
    }

    /// Pinned from the differential harness against GNU make: `:=` expands once, `+=` on it
    /// expands the appended text at once, `$$` survives a simple assignment, and `$A` names
    /// the one-letter variable `A`.
    #[test]
    fn make_expansion_follows_simple_assignments() {
        let variables = make_assignments(
            "A = x # c\nB = $(A)y\nS := $(B) $$Q\nB = changed\nT := $(S)\nT += $(B)\n\
             U += u\nU := $(U)v\nR_HOME = ignored\n",
            "/r",
        );
        assert_eq!(
            expand_make("[$(S)] [$(T)] [$(U)]", &variables),
            "[x y $Q] [x y $Q changed] [uv]"
        );
        assert_eq!(expand_make("[$Ax] [$(R_HOME)]", &variables), "[x x] [/r]");
    }

    /// Pinned from the differential harness against GNU make: an empty append adds no
    /// space, a run of continuation lines collapses to one space, and a trailing `$` stays.
    #[test]
    fn make_expansion_joins_like_gnu_make() {
        let variables = make_assignments(
            "A := x\nA += $(NONE)\nB = $$ \\\n    \\\n   \nB := $B17\nC := x$$$\n",
            "/r",
        );
        assert_eq!(
            expand_make("[$(A)] [$(B)] [$(C)]", &variables),
            "[x] [$ 17] [x$$]"
        );
    }

    /// Each self-reference expands to nothing once, so a value that repeats one is linear in
    /// its length rather than exponential.
    #[test]
    fn make_expansion_of_a_repeated_self_reference_ends() {
        let variables = make_assignments("A = $(A)$(A)$(A)$(A)$(A)$(A)$(A)$(A)\n", "/r");
        assert_eq!(expand_make("$(A)", &variables), "");
    }

    /// The variable names the generated makefiles assign. None is one of make's built-in
    /// variables, and `X` is one letter, so `$X` refers to it.
    const MAKE_NAMES: [&str; 5] = ["RA", "RB", "RC", "RAB", "X"];

    /// Generates makefiles from a seed, runs each through GNU make with `$(info ...)` lines
    /// that print every variable, and requires `make_assignments` and `expand_make` to
    /// print the same. A makefile make rejects (a recursive variable) is skipped, but ry
    /// must still expand it without hanging.
    #[test]
    fn make_expansion_agrees_with_gnu_make() {
        if Command::new("make").arg("--version").output().is_err() {
            eprintln!("skipping: GNU make is not installed");
            return;
        }
        let home = "/r h";
        let mut compared = 0usize;
        let mut total = 0usize;
        syntax::testing::for_each_seed(300, |seed| {
            let mut random = SplitMix64(seed);
            let makefile = random_makefile(&mut random);
            let variables = make_assignments(&makefile, home);
            let ours: String = MAKE_NAMES
                .iter()
                .chain(&["R_HOME", "UNSET"])
                .map(|name| {
                    let value = variables.get(*name).map_or("", String::as_str);
                    format!("{name}=[{}]\n", expand_make(value, &variables))
                })
                .collect();
            total += 1;
            let names = MAKE_NAMES.iter().chain(&["R_HOME", "UNSET"]).copied();
            let Some(theirs) = gnu_make_info(&makefile, home, names) else {
                return;
            };
            compared += 1;
            assert_eq!(ours, theirs, "makefile:\n{makefile}");
        });
        assert!(
            compared * 2 >= total,
            "GNU make rejected {} of {total} makefiles, so the harness compares too few",
            total - compared
        );
    }

    /// GNU R's own `Makeconf`, when GNU R is installed, expands as make expands it.
    #[test]
    fn gnu_r_makeconf_agrees_with_gnu_make() {
        let Some(path) = Command::new("R")
            .arg("RHOME")
            .output()
            .ok()
            .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
            .map(|home| home.join("etc/Makeconf"))
            .filter(|path| path.is_file())
        else {
            eprintln!("skipping: GNU R is not installed");
            return;
        };
        // R names the included `vars.mk` by `R_SHARE_DIR`, which only R CMD sets, and ry
        // does not follow includes.
        let text: String = fs::read_to_string(&path)
            .expect("Makeconf is readable")
            .lines()
            .filter(|line| !line.starts_with("include "))
            .map(|line| format!("{line}\n"))
            .collect();
        let home = "/r h";
        let variables = make_assignments(&text, home);
        let ours: String = variables
            .iter()
            .map(|(name, value)| format!("{name}=[{}]\n", expand_make(value, &variables)))
            .collect();
        let theirs = gnu_make_info(&text, home, variables.keys().map(String::as_str))
            .expect("make reads GNU R's Makeconf");
        assert_eq!(ours, theirs);
    }

    /// Arbitrary text never panics the parser or the expander, and expansion ends.
    #[test]
    fn arbitrary_makefiles_never_break_expansion() {
        syntax::testing::for_each_seed(500, |seed| {
            let mut random = SplitMix64(seed);
            let alphabet = [
                "$", "(", ")", "{", "}", "=", ":", "+", "?", "#", "\\", "\n", " ", "\t", "A", "RA",
                "é",
            ];
            let text: String = (0..random.below(80))
                .map(|_| alphabet[random.below(alphabet.len())])
                .collect();
            let variables = make_assignments(&text, "/r");
            for value in variables.values().chain([&text]) {
                expand_make(value, &variables);
            }
        });
    }

    use syntax::testing::SplitMix64;

    fn random_makefile(random: &mut SplitMix64) -> String {
        let operators = ["=", ":=", "::=", "+=", "?="];
        let mut makefile = String::new();
        for _ in 0..1 + random.below(8) {
            match random.below(12) {
                0 => makefile.push_str("# a comment \\\n  continued\n"),
                1 => makefile.push('\n'),
                _ => {
                    let name = MAKE_NAMES[random.below(MAKE_NAMES.len())];
                    let operator = operators[random.below(operators.len())];
                    let space = if random.chance(1, 2) { " " } else { "" };
                    makefile.push_str(&format!("{name}{space}{operator}{space}"));
                    makefile.push_str(&random_make_value(random, 2));
                    if random.chance(1, 4) {
                        makefile.push_str(" # trailing comment");
                    }
                    makefile.push('\n');
                }
            }
        }
        makefile
    }

    fn random_make_value(random: &mut SplitMix64, depth: usize) -> String {
        let reference_names = ["RA", "RB", "RC", "RAB", "X", "R_HOME", "UNSET"];
        let mut value = String::new();
        for _ in 0..random.below(5) {
            match random.below(if depth == 0 { 6 } else { 9 }) {
                0 => value.push_str(["x", "-I/p", "17", "\"a b\"", "A", "B"][random.below(6)]),
                1 => value.push_str(["  ", " ", "\t"][random.below(3)]),
                2 => value.push_str("$$"),
                3 => value.push_str("$X"),
                4 => value.push_str(" \\\n   "),
                5 => value.push_str(["$A", "$"][random.below(2)]),
                6 | 7 => {
                    let name = reference_names[random.below(reference_names.len())];
                    match random.chance(1, 2) {
                        true => value.push_str(&format!("$({name})")),
                        false => value.push_str(&format!("${{{name}}}")),
                    }
                }
                _ => {
                    let inner = random_make_value(random, depth - 1);
                    value.push_str(&format!(
                        "$(R{})",
                        inner.replace(['(', ')', '#', '\\', '\n'], "")
                    ));
                }
            }
        }
        // A trailing backslash would continue the line.
        while value.ends_with('\\') {
            value.pop();
        }
        value
    }

    /// What GNU make prints for each of `names` in `makefile`, or `None` when make rejects
    /// the makefile.
    fn gnu_make_info<'a>(
        makefile: &str,
        home: &str,
        names: impl Iterator<Item = &'a str>,
    ) -> Option<String> {
        use std::io::Write;
        let info: String = names
            .map(|name| format!("$(info {name}=[$({name})])\n"))
            .collect();
        let mut child = Command::new("make")
            .args(["-s", "--no-print-directory", "-f", "-"])
            .arg(format!("R_HOME={home}"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("make starts");
        child
            .stdin
            .take()
            .expect("make has a stdin")
            .write_all(format!("{makefile}{info}all: ;@:\n").as_bytes())
            .expect("make reads the makefile");
        let output = child.wait_with_output().expect("make finishes");
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[test]
    fn windows_homes_build_dlls_under_libs_x64() {
        let root = std::env::temp_dir().join(format!("ry-rcmd-platform-{}", std::process::id()));
        fs::create_dir_all(root.join("etc")).expect("create etc");
        let text = makeconf(Path::new("C:\\R"), &Toolchain::rtools(), Platform::Windows);
        for line in [
            "R_HOME = C:/R",
            "R_ARCH = /x64",
            "SHLIB_EXT = .dll",
            "LIBR = -L\"$(R_HOME)/bin/x64\" -lR",
            "CPICFLAGS = \n",
            "BLAS_LIBS = -L\"$(R_HOME)/bin/x64\" -lRblas",
        ] {
            assert!(text.contains(line), "missing {line:?} in\n{text}");
        }
        write(&root.join("etc/Makeconf"), &text).expect("write Makeconf");
        assert_eq!(
            Platform::of_home(&root).expect("platform"),
            Platform::Windows
        );
        let unix = makeconf(&root, &Toolchain::rtools(), Platform::Linux);
        write(&root.join("etc/Makeconf"), &unix).expect("write Makeconf");
        assert_eq!(Platform::of_home(&root).expect("platform"), Platform::Linux);
        assert_eq!(
            libs_directory(Path::new("pkg"), Platform::Windows),
            Path::new("pkg/libs/x64")
        );
        fs::remove_dir_all(&root).expect("remove home");
    }

    #[test]
    fn windows_build_files_prefer_ucrt_then_win() {
        let package =
            std::env::temp_dir().join(format!("ry-rcmd-build-files-{}", std::process::id()));
        fs::create_dir_all(&package).expect("create package");
        for file in ["Makevars", "Makevars.win", "configure", "Makefile"] {
            write(&package.join(file), "").expect("write build file");
        }
        let windows = Platform::Windows;
        assert_eq!(
            windows.build_file(&package, "Makevars"),
            Some(package.join("Makevars.win"))
        );
        write(&package.join("Makevars.ucrt"), "").expect("write Makevars.ucrt");
        assert_eq!(
            windows.build_file(&package, "Makevars"),
            Some(package.join("Makevars.ucrt"))
        );
        assert_eq!(windows.build_file(&package, "configure"), None);
        assert_eq!(windows.build_file(&package, "Makefile"), None);
        assert_eq!(
            Platform::Linux.build_file(&package, "Makevars"),
            Some(package.join("Makevars"))
        );
        assert_eq!(
            Platform::Linux.build_file(&package, "configure"),
            Some(package.join("configure"))
        );
        fs::remove_dir_all(&package).expect("remove package");
    }
}
