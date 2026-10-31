//! ry's R home and the parts of `R CMD` that build and install packages.
//!
//! A package compiled for ry's runtime is built the way GNU R's `R CMD INSTALL` builds one: its
//! `configure` script runs with `R_HOME` pointing at ry's R home, and its `src/` directory is
//! compiled by `make` with the package's `Makevars`, the R home's `etc/Makeconf` and
//! `share/make/shlib.mk`, against ry's headers and ry's `libR`. The R home is assembled by
//! [`assemble`]; its `bin/R` runs the `ry-rcmd` binary, so `configure` scripts that ask
//! `R CMD config CC` get ry's answers.
//!
//! The R-level half of an installation (the lazy-load database, help indices, the `Meta/`
//! files) is R code that runs in the interpreter. Until one is installed, a source install keeps
//! the package's `R/` sources as they are and records `RyBuilt:` in the installed
//! `DESCRIPTION`, so the installation is recognizably ry's (the packaging design,
//! `contributing/design/packaging.md`).

use std::{
    collections::BTreeMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

/// The R version ry presents, as `R.version` and `Built:` report it.
pub const R_VERSION: &str = "4.6.2";

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
    /// ry's built `libR.so`.
    pub libr: &'a Path,
    /// The `ry-rcmd` binary that `bin/R` runs.
    pub rcmd: &'a Path,
    /// GNU R's `Rscript`, which `bin/Rscript` runs for the R code a `configure` script or
    /// `Makevars` evaluates (`Rcpp:::CxxFlags()`), until ry's interpreter can run it. `None`
    /// makes `bin/Rscript` fail with a message that says so.
    pub gnu_rscript: Option<&'a Path>,
    pub toolchain: Toolchain,
}

/// Creates ry's R home at `root`: `include/`, `lib/libR.so`, `etc/Makeconf`,
/// `share/make/shlib.mk`, and the `bin/R` and `bin/Rscript` scripts.
pub fn assemble(root: &Path, sources: &HomeSources<'_>) -> Result<()> {
    for directory in ["bin", "etc", "lib", "share/make", "library"] {
        let path = root.join(directory);
        fs::create_dir_all(&path).map_err(io_error(&path))?;
    }
    copy_tree(sources.include, &root.join("include"))?;
    let libr = root.join("lib/libR.so");
    fs::copy(sources.libr, &libr).map_err(io_error(&libr))?;
    write(&root.join("etc/Makeconf"), &makeconf(root, &sources.toolchain))?;
    write(&root.join("share/make/shlib.mk"), SHLIB_MK)?;
    let rcmd = sources.rcmd.display();
    write_script(
        &root.join("bin/R"),
        &format!(
            "#!/bin/sh\n# ry's R front end: R CMD INSTALL, SHLIB and config, and R RHOME.\n\
             R_HOME='{}'\nexport R_HOME\nexec '{rcmd}' \"$@\"\n",
            root.display()
        ),
    )?;
    let rscript = match sources.gnu_rscript {
        Some(rscript) => format!(
            "#!/bin/sh\n# R code that configure scripts and Makevars evaluate runs in GNU R until \
             ry's interpreter can run it.\nunset R_HOME\nexec '{}' \"$@\"\n",
            rscript.display()
        ),
        None => "#!/bin/sh\necho 'Rscript: ry cannot run R code without its interpreter' >&2\n\
                 exit 1\n"
            .to_owned(),
    };
    write_script(&root.join("bin/Rscript"), &rscript)
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
MAKE = make

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
                let mut rest = rest.iter();
                while let Some(argument) = rest.next() {
                    if argument == "-o" {
                        output = rest.next().cloned();
                    } else if let Some(name) = argument.strip_prefix("--output=") {
                        output = Some(name.to_owned());
                    } else {
                        files.push(argument.clone());
                    }
                }
                let library = shlib(home, &directory, &files, output.as_deref(), &[])?;
                Ok(format!("{}\n", library.display()))
            }
            "INSTALL" => {
                let mut library = None;
                let mut packages = Vec::new();
                let mut rest = rest.iter();
                while let Some(argument) = rest.next() {
                    if argument == "-l" || argument == "--library" {
                        library = rest.next().map(PathBuf::from);
                    } else if let Some(path) = argument.strip_prefix("--library=") {
                        library = Some(PathBuf::from(path));
                    } else if !argument.starts_with('-') {
                        packages.push(PathBuf::from(argument));
                    }
                }
                let library = library.unwrap_or_else(|| home.join("library"));
                let mut output = String::new();
                for package in packages {
                    let installed = install(home, &package, &library)?;
                    output.push_str(&format!("* DONE ({})\n", installed.name));
                }
                Ok(output)
            }
            other => Err(Error::Usage(format!(
                "R CMD {other} needs ry's interpreter, which is not available yet"
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
        return Err(Error::Usage(format!("R CMD config: unknown variable '{variable}'")));
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

/// Installs a package into `library`: a source package (a directory or a `.tar.gz` holding
/// `src/` or R code to install), or a binary package that GNU R or ry installed before (it has
/// a `Built:` field), which is copied as it is.
pub fn install(home: &Path, package: &Path, library: &Path) -> Result<Installed> {
    fs::create_dir_all(library).map_err(io_error(library))?;
    let staging = tempdir(library)?;
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
            .ok_or_else(|| Error::Package(format!("{} has no Package field", description_path.display())))?
            .to_owned();
        if let Some(built) = description.get("Built") {
            install_binary(&directory, &name, built, library)
        } else {
            install_source(home, &directory, &name, description, library, &staging)
        }
    })();
    drop(fs::remove_dir_all(&staging));
    result
}

fn tempdir(near: &Path) -> Result<PathBuf> {
    for attempt in 0..1000u32 {
        let path = near.join(format!(".ry-install-{}-{attempt}", std::process::id()));
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
        .ok_or_else(|| Error::Package(format!("package '{name}' has an invalid Built field: {built}")))?;
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
    let shared = target.join("libs").join(format!("{}.so", name.replace('.', "_")));
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
) -> Result<Installed> {
    // Building happens in a copy, because configure and make write into the source tree.
    let build = staging.join("build").join(name);
    copy_tree(source, &build)?;
    let target_staging = staging.join("installed").join(name);
    fs::create_dir_all(&target_staging).map_err(io_error(&target_staging))?;
    let paths = library_paths(home, library);
    let environment = build_environment(home, name, &target_staging, &paths);
    let configure = build.join("configure");
    if configure.exists() {
        run_command(
            Command::new("sh")
                .arg("./configure")
                .current_dir(&build)
                .envs(environment.iter().map(|(key, value)| (key.as_str(), value.as_str()))),
        )
        .map_err(|error| Error::Package(format!("configuration failed for package '{name}'\n{error}")))?;
    }
    let src = build.join("src");
    let mut shared_object = None;
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
                Command::new("make")
                    .current_dir(&src)
                    .envs(environment.iter().map(|(key, value)| (key.as_str(), value.as_str()))),
            )
            .map_err(|error| Error::Package(format!("compilation failed for package '{name}'\n{error}")))?;
            src.join(&library_name)
        } else {
            shlib(home, &src, &[], Some(&library_name), &include_flags)
                .map_err(|error| Error::Package(format!("compilation failed for package '{name}'\n{error}")))?
        };
        if built.exists() {
            let libs = target_staging.join("libs");
            fs::create_dir_all(&libs).map_err(io_error(&libs))?;
            let installed = libs.join(&library_name);
            fs::copy(&built, &installed).map_err(io_error(&installed))?;
            shared_object = Some(library_name);
        }
    }
    for directory in ["R", "data", "demo", "exec", "po", "tests"] {
        let from = build.join(directory);
        if from.is_dir() {
            copy_tree(&from, &target_staging.join(directory))?;
        }
    }
    let inst = build.join("inst");
    if inst.is_dir() {
        copy_tree(&inst, &target_staging)?;
    }
    for file in ["NAMESPACE", "LICENSE", "NEWS", "NEWS.md"] {
        let from = build.join(file);
        if from.exists() {
            let to = target_staging.join(file);
            fs::copy(&from, &to).map_err(io_error(&to))?;
        }
    }
    description.set(
        "Built",
        format!(
            "R {R_VERSION}; {}; {}; unix",
            if shared_object.is_some() {
                format!("{}-pc-linux-gnu", std::env::consts::ARCH)
            } else {
                String::new()
            },
            build_stamp()
        ),
    );
    description.set("RyBuilt", env!("CARGO_PKG_VERSION").to_owned());
    write(&target_staging.join("DESCRIPTION"), &description.render())?;
    let target = library.join(name);
    if target.exists() {
        fs::remove_dir_all(&target).map_err(io_error(&target))?;
    }
    fs::rename(&target_staging, &target)
        .or_else(|_| copy_tree(&target_staging, &target).map_err(|error| io::Error::other(error.to_string())))
        .map_err(io_error(&target))?;
    Ok(Installed {
        name: name.to_owned(),
        library: shared_object.map(|file| target.join("libs").join(file)),
        directory: target,
    })
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

/// The `Built:` timestamp: UTC, in R's `YYYY-MM-DD HH:MM:SS UTC` form.
fn build_stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = seconds / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
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
/// `OBJECTS`.
pub fn shlib(
    home: &Path,
    directory: &Path,
    files: &[String],
    output: Option<&str>,
    include_flags: &[String],
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
            matches!(extension, "c" | "cc" | "cpp" | "cxx" | "f" | "f90" | "f95" | "m" | "o")
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
    if fortran {
        command.arg("SHLIB_LIBADD=$(FLIBS)");
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
    fn dates_convert_from_day_counts() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_392), (2025, 10, 31));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

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
