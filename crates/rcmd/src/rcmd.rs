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

/// The toolchain an R home is configured with.
#[derive(Clone, Debug)]
pub struct Toolchain {
    pub cc: String,
    pub cxx: String,
    pub fc: String,
    pub blas_libs: String,
    pub lapack_libs: String,
    pub flibs: String,
}

impl Default for Toolchain {
    /// The GNU toolchain with the system's reference BLAS and LAPACK, which is what GNU R's
    /// Linux builds and the package binaries of the corpus link against.
    fn default() -> Toolchain {
        Toolchain {
            cc: "gcc".into(),
            cxx: "g++".into(),
            fc: "gfortran".into(),
            blas_libs: "-lblas".into(),
            lapack_libs: "-llapack".into(),
            flibs: "-lgfortran -lm -lquadmath".into(),
        }
    }
}

/// Where the pieces of an R home come from.
pub struct HomeSources<'a> {
    /// ry's headers (`crates/libr/include`).
    pub include: &'a Path,
    /// ry's built `libR.so`, which holds the interpreter.
    pub libr: &'a Path,
    /// The `ry-rcmd` binary that `bin/R` and `bin/Rscript` run.
    pub rcmd: &'a Path,
    /// The GNU R installation (`R RHOME`) whose base packages, `share/` and `etc/` files the
    /// home links to.
    pub gnu_r_home: &'a Path,
    pub toolchain: Toolchain,
}

/// The front ends ry answers itself; every other `bin/` script of GNU R (`build`, `check`,
/// `Rdconv`, `REMOVE` and the rest) is linked, and runs R code through ry's `bin/R`.
const OWN_SCRIPTS: &[&str] = &["R", "Rscript", "Rcmd", "INSTALL", "SHLIB", "config", "exec"];

/// Creates ry's R home at `root`: `include/`, `lib/libR.so`, `etc/Makeconf`,
/// `share/make/shlib.mk` and the `bin/` front ends, with base R's packages in `library/` and
/// the rest of `share/`, `etc/` and `bin/` linked from GNU R.
pub fn assemble(root: &Path, sources: &HomeSources<'_>) -> Result<()> {
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
    let libr = root.join("lib/libR.so");
    // Replacing rather than overwriting keeps a running session's mapping of the old file valid.
    remove_file_if_present(&libr)?;
    fs::copy(sources.libr, &libr).map_err(io_error(&libr))?;
    write(
        &root.join("etc/Makeconf"),
        &makeconf(root, &sources.toolchain),
    )?;
    write(&root.join("share/make/shlib.mk"), SHLIB_MK)?;

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
    for entry in read_directory(&gnu.join("bin"))? {
        let name = entry.file_name().unwrap_or_default();
        if !OWN_SCRIPTS.iter().any(|own| name == *own) && entry.is_file() {
            link(&entry, &root.join("bin").join(name))?;
        }
    }

    let home = root.display();
    let rcmd = sources.rcmd.display();
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

/// `etc/Makeconf`: the variables Writing R Extensions documents for `Makevars`, and the
/// suffix rules that compile a package's sources.
fn makeconf(root: &Path, toolchain: &Toolchain) -> String {
    let Toolchain {
        cc,
        cxx,
        fc,
        blas_libs,
        lapack_libs,
        flibs,
    } = toolchain;
    format!(
        r#"# ry's build configuration for packages, read by R CMD INSTALL and R CMD SHLIB.

R_HOME = {home}
R_INCLUDE_DIR = $(R_HOME)/include
R_SHARE_DIR = $(R_HOME)/share
R_ARCH =
SHELL = /bin/sh

CC = {cc}
CFLAGS = -g -O2
CPICFLAGS = -fpic
C99 = $(CC) -std=gnu99
C11 = $(CC) -std=gnu11
C17 = $(CC) -std=gnu17
C23 = $(CC) -std=gnu2x
CXX = {cxx} -std=gnu++17
CXXFLAGS = -g -O2
CXXPICFLAGS = -fpic
CXX11 = {cxx}
CXX11STD = -std=gnu++11
CXX11FLAGS = -g -O2
CXX11PICFLAGS = -fpic
CXX14 = {cxx}
CXX14STD = -std=gnu++14
CXX14FLAGS = -g -O2
CXX14PICFLAGS = -fpic
CXX17 = {cxx}
CXX17STD = -std=gnu++17
CXX17FLAGS = -g -O2
CXX17PICFLAGS = -fpic
CXX20 = {cxx}
CXX20STD = -std=gnu++20
CXX20FLAGS = -g -O2
CXX20PICFLAGS = -fpic
CXX23 = {cxx}
CXX23STD = -std=gnu++23
CXX23FLAGS = -g -O2
CXX23PICFLAGS = -fpic
CXXCPP = $(CXX) -E
CPP = $(CC) -E
FC = {fc}
F77 = {fc}
FFLAGS = -g -O2
FCFLAGS = -g -O2
FPICFLAGS = -fpic
SAFE_FFLAGS = -g -O2 -ffloat-store
FLIBS = {flibs}
FCLIBS_XTRA =
OBJC = $(CC)
OBJCFLAGS = -g -O2
AR = ar
RANLIB = ranlib
LDFLAGS =
CPPFLAGS =
DYLIB_EXT = .so
SHLIB_EXT = .so
SHLIB_LD = $(CC)
SHLIB_LDFLAGS = -shared
SHLIB_CXXLD = $(CXX)
SHLIB_CXXLDFLAGS = -shared
SHLIB_FCLD = $(FC)
SHLIB_FCLDFLAGS = -shared
SHLIB_OPENMP_CFLAGS = -fopenmp
SHLIB_OPENMP_CXXFLAGS = -fopenmp
SHLIB_OPENMP_FFLAGS = -fopenmp
SHLIB_OPENMP_FCFLAGS = -fopenmp
BLAS_LIBS = {blas_libs}
LAPACK_LIBS = {lapack_libs}
LIBM = -lm
LIBR = -L"$(R_HOME)/lib" -lR
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
        home = root.display()
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
                let mut options = InstallOptions::default();
                let mut packages = Vec::new();
                let mut rest = rest.iter();
                while let Some(argument) = rest.next() {
                    match argument.as_str() {
                        "-l" | "--library" => library = rest.next().map(PathBuf::from),
                        "--install-tests" => options.tests = true,
                        "--no-test-load" => options.test_load = false,
                        "--no-docs" | "--no-help" => options.driver_flags.push("--no-docs".into()),
                        "--no-data" | "--no-inst" | "--no-demo" | "--no-exec" | "--no-R" => {
                            options.driver_flags.push(argument.clone());
                        }
                        other => {
                            if let Some(path) = other.strip_prefix("--library=") {
                                library = Some(PathBuf::from(path));
                            } else if !other.starts_with('-') {
                                packages.push(PathBuf::from(other));
                            }
                        }
                    }
                }
                let library = library.unwrap_or_else(|| home.join("library"));
                let mut output = String::new();
                for package in packages {
                    let installed = install(home, &package, &library, &options)?;
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
            install_binary(&directory, &name, built, library)
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
fn install_binary(directory: &Path, name: &str, built: &str, library: &Path) -> Result<Installed> {
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
    let shared = target
        .join("libs")
        .join(format!("{}.so", name.replace('.', "_")));
    let library_path = [target.join("libs").join(format!("{name}.so")), shared]
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
    let paths = library_paths(home, library);
    let environment = build_environment(home, name, &target_staging, &paths);
    let configure = build.join("configure");
    if configure.exists() {
        run_command(
            Command::new("sh")
                .arg("./configure")
                .current_dir(&build)
                .envs(
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
    let mut shared_object = None;
    let mut install_libs = false;
    if src.is_dir() {
        let include_flags: Vec<String> = description
            .packages("LinkingTo")
            .iter()
            .filter_map(|dependency| find_package(&paths, dependency))
            .map(|directory| format!("-I\"{}\"", directory.join("include").display()))
            .collect();
        let library_name = format!("{}.so", name.replace('.', "_"));
        let built = if src.join("Makefile").exists() {
            run_command(
                Command::new("make").current_dir(&src).envs(
                    environment
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str())),
                ),
            )
            .map_err(|error| {
                Error::Package(format!("compilation failed for package '{name}'\n{error}"))
            })?;
            src.join(&library_name)
        } else {
            shlib(home, &src, &[], Some(&library_name), &include_flags, &[]).map_err(|error| {
                Error::Package(format!("compilation failed for package '{name}'\n{error}"))
            })?
        };
        // A package's `src/install.libs.R` installs its own libraries, and runs as R code in
        // the driver; otherwise every shared object `src` holds is installed, as GNU R does.
        if src.join("install.libs.R").exists() {
            install_libs = true;
        } else if built.exists() {
            let libs = target_staging.join("libs");
            fs::create_dir_all(&libs).map_err(io_error(&libs))?;
            for entry in fs::read_dir(&src).map_err(io_error(&src))? {
                let path = entry.map_err(io_error(&src))?.path();
                let Some(file) = path
                    .file_name()
                    .filter(|_| path.extension().is_some_and(|extension| extension == "so"))
                else {
                    continue;
                };
                let installed = libs.join(file);
                fs::copy(&path, &installed).map_err(io_error(&installed))?;
            }
            let symbols = src.join("symbols.rds");
            if symbols.exists() {
                fs::copy(&symbols, libs.join("symbols.rds")).map_err(io_error(&symbols))?;
            }
        }
        shared_object = Some(library_name);
    }
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
        library: shared_object
            .map(|file| target.join("libs").join(file))
            .filter(|path| path.exists()),
        directory: target,
    })
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
    run_command(
        Command::new(home.join("bin/R"))
            .args(["--vanilla", "--no-echo", "-f"])
            .arg(file)
            .arg("--args")
            .args(arguments)
            .current_dir(directory)
            .env("R_LIBS", r_libs)
            .env("R_TESTS", "")
            .env("LC_COLLATE", "C"),
    )
}

/// The environment `configure` and `make` run in, as GNU R's installer sets it.
fn build_environment(
    home: &Path,
    name: &str,
    target: &Path,
    paths: &[PathBuf],
) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    let path = std::env::var("PATH").unwrap_or_default();
    environment.insert(
        "PATH".to_owned(),
        format!("{}:{path}", home.join("bin").display()),
    );
    environment.insert("R_HOME".to_owned(), home.display().to_string());
    environment.insert("R_ARCH".to_owned(), String::new());
    environment.insert("R_PACKAGE_NAME".to_owned(), name.to_owned());
    environment.insert("R_PACKAGE_DIR".to_owned(), target.display().to_string());
    environment.insert(
        "R_INCLUDE_DIR".to_owned(),
        home.join("include").display().to_string(),
    );
    environment.insert(
        "R_LIBS".to_owned(),
        std::env::join_paths(paths)
            .map(|joined| joined.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    environment
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
    let makevars = ["Makevars", "Makevars.in"]
        .iter()
        .map(|name| directory.join(name))
        .find(|path| path.exists() && path.file_name().is_some_and(|name| name == "Makevars"));
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
            format!("{stem}.so")
        },
        str::to_owned,
    );
    let mut command = Command::new("make");
    command.current_dir(directory);
    if let Some(makevars) = &makevars {
        command.arg("-f").arg(makevars);
    }
    command
        .arg("-f")
        .arg(home.join("etc/Makeconf"))
        .arg("-f")
        .arg(home.join("share/make/shlib.mk"));
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
    command.env(
        "PATH",
        format!(
            "{}:{}",
            home.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    run_command(&mut command)?;
    Ok(directory.join(output))
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
}
