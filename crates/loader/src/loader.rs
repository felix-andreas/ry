//! The package-development loader: reads a package's sources into the analysis, derives its
//! load index, and diffs that index against what is loaded to produce a [`Plan`]. A plan says
//! what to bind, run, and remove, and nothing about how; a backend executes it. Both runtimes
//! apply it through the same R code ([`r`]): GNU R parses the code from source text, and ry's
//! interpreter ([`ry_runtime`]) lowers the analysis's syntax trees.

pub mod r;
#[cfg(feature = "runtime")]
pub mod ry_runtime;
pub mod testing;

use {
    rustc_hash::{FxHashMap, FxHashSet},
    salsa::{Durability, Setter},
    semantics::{
        ANALYSIS_STACK_SIZE, Db, DocumentKind, ProjectFiles, RootDatabase, SourceFile, TypingMode,
        file_conditional_writers, file_definitions, file_typing_mode,
        load_index::{
            IndexedFile, IndexedItem, PackageFile, PackageIndex, PackageSources, Registration,
            classify_constants, file_binds, file_items, file_load_index, fnv1a, package_index,
        },
        metadata::{
            DynamicLibraryDirective, NamespaceImportDirective, NamespaceSpec, OS_TYPE,
            S3MethodDirective, parse_description_collate,
        },
        stubs::install_shipped_stubs,
    },
    serde::{Deserialize, Serialize},
    std::{
        collections::BTreeSet,
        fmt, fs, io,
        path::{Path, PathBuf},
        sync::Arc,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    },
    syntax::Parse,
};

/// One process's loader. It keeps one analysis database for every package it loads, so a
/// reload re-derives only the items whose text changed.
#[derive(Default)]
pub struct Loader {
    db: RootDatabase,
    packages: Vec<LoadedPackage>,
    /// Size and modification time of each file as last read, per package root.
    stamps: FxHashMap<PathBuf, FxHashMap<String, (u64, Option<SystemTime>)>>,
    /// Whether the shipped stubs are in the database. A strict package needs them to tell
    /// which constants call only pure functions; any other load never reads them, so it skips
    /// their parse.
    stubs_installed: bool,
    /// Whether the project's `ry.toml` turns strict mode on, which the host reads and sets
    /// with [`Loader::set_strict`]: the loader reads package sources, not ry's configuration.
    /// Off, a file is strict only when its `# typing: strict` directive says so.
    strict: bool,
}

/// A package's sources as the loader reads them.
#[derive(Debug, Clone, Default)]
pub struct PackageInputs {
    pub description: String,
    pub namespace: String,
    /// `R/` files in collation order, by path relative to the root. `None` keeps the text the
    /// loader already has for that path.
    pub files: Vec<(String, Option<String>)>,
    /// Of `R/sysdata.rda`.
    pub sysdata: Option<u64>,
    /// Of `data/`.
    pub data: Option<Datasets>,
    /// Of the sources under `src/`.
    pub compiled: Option<u64>,
    /// `[check] strict`, the mode of each file without its own `# typing:` directive.
    pub strict: bool,
}

struct LoadedPackage {
    root: PathBuf,
    sources: PackageSources,
    files: FxHashMap<String, SourceFile>,
    /// What the session has loaded, as confirmed by [`Loader::commit`].
    committed: Option<LoadState>,
    /// The state a plan was made for, until the backend confirms it applied.
    pending: Option<LoadState>,
    /// [`source_fingerprint`] of what is loaded.
    committed_fingerprint: Option<u64>,
    /// [`source_fingerprint`] of what the pending plan read.
    pending_fingerprint: Option<u64>,
}

/// What is loaded for one package: the load index plus the inputs the analysis does not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadState {
    pub root: PathBuf,
    pub index: Arc<PackageIndex>,
    /// Of `R/sysdata.rda`.
    pub sysdata: Option<u64>,
    /// A state saved before datasets were loaded has none.
    #[serde(default)]
    pub data: Option<Datasets>,
    /// Of the names, sizes and modification times under `src/`.
    pub compiled: Option<u64>,
}

/// The dataset files in `data/`, which a load reads into the namespace's lazy data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Datasets {
    /// Paths relative to the package root, in the order R CMD INSTALL reads them.
    pub files: Vec<String>,
    /// Of the files' names, sizes and modification times, so a changed dataset is noticed
    /// without reading it.
    pub fingerprint: u64,
}

/// Backend-neutral instructions, applied in field order: imports, sources, removals,
/// unregistrations, lazy bindings, eager code, `sysdata.rda`, datasets, S3 registrations,
/// compiled code, exports, and finally the hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub package: String,
    pub version: String,
    pub root: PathBuf,
    /// Build a fresh namespace. Otherwise the plan patches the loaded one.
    pub full: bool,
    /// Attached before anything runs (full loads only).
    pub depends: Vec<String>,
    /// `Some` when the imports environment must be rebuilt.
    pub imports: Option<Vec<NamespaceImportDirective>>,
    /// Source files whose text is new to the session. Code below refers to them by path.
    pub sources: Vec<SourceText>,
    /// Source files that no longer exist.
    pub dropped_sources: Vec<String>,
    /// Bindings no item writes any more.
    pub remove: Vec<String>,
    /// Registrations by items that are gone.
    pub unregister: Vec<Registration>,
    /// Definitions to bind lazily: functions, and the pure constants of strict files.
    pub bind: Vec<Code>,
    /// Top-level code to run, in collation order.
    pub run: Vec<Code>,
    pub sysdata: SysdataChange,
    /// `Some` when the datasets must be read again: every file of `data/` to read, in order,
    /// into a cleared lazy data environment. Empty when the last dataset went away.
    pub data: Option<Vec<String>>,
    /// `S3method()` registrations to (re)make.
    pub s3_register: Vec<S3MethodDirective>,
    /// `S3method()` registrations that `NAMESPACE` no longer declares.
    pub s3_unregister: Vec<S3MethodDirective>,
    /// The declared S3 methods, sent whenever they changed.
    pub s3_declared: Option<Vec<S3MethodDirective>>,
    /// `Some` when compiled code must be (re)built and loaded.
    pub dynamic_libraries: Option<Vec<DynamicLibraryDirective>>,
    pub namespace: NamespaceSpec,
    /// The export directives changed, so the exports need a resync even when nothing else
    /// does.
    pub exports_changed: bool,
    /// Every binding the plan writes was already bound and none goes away, so the namespace
    /// keeps the same names and a backend need not rescan them to resync its exports.
    pub keeps_names: bool,
    pub run_on_load: bool,
    /// Things the backend should tell the user, such as a directive it cannot honor.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceText {
    pub path: String,
    /// What R reads: inline annotations erased, line breaks kept.
    pub text: String,
}

/// One item's code: a slice of a source file, positioned so R's source references point at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// The binding a lazy definition creates.
    pub name: Option<String>,
    pub path: String,
    pub line: u32,
    pub column: u32,
    /// Character offsets into the source file's text.
    pub start: u32,
    pub end: u32,
    /// Byte offsets of the code's syntax node in the file as written, which a backend that
    /// lowers the analysis's syntax tree ([`Loader::syntax`]) reads instead of the text.
    pub source_start: u32,
    pub source_end: u32,
    /// Whether the code registers S4 classes or methods, after which R refreshes its S4 caches.
    pub registers_s4: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysdataChange {
    Keep,
    Load,
    Remove,
}

#[derive(Debug)]
pub enum LoadError {
    NotAPackage(PathBuf),
    Io(PathBuf, io::Error),
    NoPackageName(PathBuf),
    Syntax(Vec<(String, u32, String)>),
}

impl fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::NotAPackage(path) => write!(
                formatter,
                "{} is not inside an R package: no DESCRIPTION file was found in it or above it",
                path.display()
            ),
            LoadError::Io(path, error) => {
                write!(formatter, "cannot read {}: {error}", path.display())
            }
            LoadError::NoPackageName(path) => write!(
                formatter,
                "{} has no `Package:` field",
                path.join("DESCRIPTION").display()
            ),
            LoadError::Syntax(errors) => {
                let shown: Vec<String> = errors
                    .iter()
                    .take(5)
                    .map(|(path, line, message)| format!("{path}:{line}: {message}"))
                    .collect();
                let more = errors.len().saturating_sub(shown.len());
                write!(
                    formatter,
                    "the package has syntax errors, so nothing was loaded:\n  {}{}",
                    shown.join("\n  "),
                    if more > 0 {
                        format!("\n  and {more} more")
                    } else {
                        String::new()
                    }
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// The directory holding `DESCRIPTION`, at or above `path`.
pub fn find_package_root(path: &Path) -> Result<PathBuf, LoadError> {
    let absolute = fs::canonicalize(path).map_err(|error| LoadError::Io(path.into(), error))?;
    let mut current = Some(absolute.as_path());
    while let Some(directory) = current {
        if directory.join("DESCRIPTION").is_file() {
            return Ok(directory.to_path_buf());
        }
        current = directory.parent();
    }
    Err(LoadError::NotAPackage(absolute))
}

impl Loader {
    /// Sets whether the project's configuration turns strict mode on for every package this
    /// loader plans from then on.
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Reads the package at or above `path` and plans loading it. Without a committed state, or
    /// with `full`, the plan builds the namespace from scratch; otherwise it patches the loaded
    /// one. The plan stays pending until [`Loader::commit`].
    pub fn plan(&mut self, path: &Path, full: bool) -> Result<Plan, LoadError> {
        let root = find_package_root(path)?;
        // Taken before reading, so an edit made while the plan is read shows as a change later.
        let fingerprint = source_fingerprint(&root);
        let inputs = self.read(&root)?;
        let plan = self.plan_inputs(&root, inputs, full)?;

        if let Some(package) = self
            .packages
            .iter_mut()
            .find(|package| package.root == root)
        {
            package.pending_fingerprint = fingerprint;
        }
        Ok(plan)
    }

    /// Derives the analysis a plan of the package at or above `path` reads, without planning,
    /// so a plan made later, typically while the caller starts R, finds it memoized. A package
    /// that cannot be found or read is left for that plan to report.
    pub fn prepare(&mut self, path: &Path) {
        let Ok(root) = find_package_root(path) else {
            return;
        };
        let Ok(inputs) = self.read(&root) else {
            return;
        };
        // A failure is the plan's to report: it reads the same sources.
        if let Ok(position) = self.analyze(&root, inputs) {
            package_index(&self.db, self.packages[position].sources);
        }
    }

    /// Plans a load of the package at `root` from sources the caller read.
    pub fn plan_inputs(
        &mut self,
        root: &Path,
        inputs: PackageInputs,
        full: bool,
    ) -> Result<Plan, LoadError> {
        let root = root.to_path_buf();
        let (sysdata, compiled) = (inputs.sysdata, inputs.compiled);
        let data = inputs.data.clone();
        let position = self.analyze(&root, inputs)?;
        let package = &self.packages[position];
        let index = package_index(&self.db, package.sources);
        if index.package.is_empty() {
            return Err(LoadError::NoPackageName(root));
        }
        let syntax_errors: Vec<(String, u32, String)> = index
            .syntax_errors()
            .map(|(path, line, message)| (path.to_owned(), line, message.to_owned()))
            .collect();
        if !syntax_errors.is_empty() {
            return Err(LoadError::Syntax(syntax_errors));
        }
        let next = LoadState {
            root: root.clone(),
            index: index.clone(),
            sysdata,
            data,
            compiled,
        };
        let previous = if full {
            None
        } else {
            package
                .committed
                .as_ref()
                .filter(|previous| previous.index.package == next.index.package)
        };
        let db = &self.db;
        let plan = diff(previous, &next, &|path| {
            package
                .files
                .get(path)
                .map_or("", |&file| file_load_index(db, file).text.as_str())
        });
        let package = &mut self.packages[position];
        package.pending = Some(next);
        // Only `plan` knows what the caller's sources were read from, and it sets this after.
        package.pending_fingerprint = None;
        Ok(plan)
    }

    /// Syncs the package's sources into the analysis and derives every changed file's facts
    /// across cores, returning the package's position.
    fn analyze(&mut self, root: &Path, inputs: PackageInputs) -> Result<usize, LoadError> {
        let (position, changed) = self.sync(
            root,
            inputs.description,
            inputs.namespace,
            inputs.files,
            inputs.strict,
        )?;
        self.warm(&changed, |db, file| {
            file_items(db, file);
            file_typing_mode(db, file);
        });
        if self.prepare_purity(position) {
            // Classifying a constant resolves its callees through the project-wide definition
            // maps, which join per-file facts that are cheaper to derive across cores first.
            self.warm(&changed, |db, file| {
                file_definitions(db, file);
                file_conditional_writers(db, file);
            });
            let sources = self.packages[position].sources;
            self.warm(&changed, |db, file| classify_constants(db, sources, file));
        }
        let sources = self.packages[position].sources;
        self.warm(&changed, |db, file| {
            file_binds(db, sources, file);
        });
        Ok(position)
    }

    /// Records that the backend applied the pending plan for the package at `root`.
    pub fn commit(&mut self, root: &Path) -> bool {
        let Some(package) = self
            .packages
            .iter_mut()
            .find(|package| package.root == root)
        else {
            return false;
        };
        match package.pending.take() {
            Some(state) => {
                package.committed = Some(state);
                package.committed_fingerprint = package.pending_fingerprint.take();
                true
            }
            None => false,
        }
    }

    /// The roots of the loaded packages whose sources changed on disk since they were loaded,
    /// judged from file names, sizes and modification times without reading a file. A root is
    /// reported once per change: a reload that fails is not retried until the next change.
    pub fn changed_roots(&mut self) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        for package in &mut self.packages {
            if package.committed.is_none() {
                continue;
            }
            let current = source_fingerprint(&package.root);
            if current != package.committed_fingerprint {
                package.committed_fingerprint = current;
                changed.push(package.root.clone());
            }
        }
        changed
    }

    /// The pending state for `root`, which a backend that persists state saves.
    pub fn pending(&self, root: &Path) -> Option<&LoadState> {
        self.packages
            .iter()
            .find(|package| package.root == root)
            .and_then(|package| package.pending.as_ref())
    }

    /// Adopts a state saved by an earlier process as what is loaded.
    pub fn restore(&mut self, state: LoadState) -> Result<(), LoadError> {
        let inputs = self.read(&state.root)?;
        let (position, _) = self.sync(
            &state.root,
            inputs.description,
            inputs.namespace,
            inputs.files,
            inputs.strict,
        )?;
        self.packages[position].committed = Some(state);
        Ok(())
    }

    /// The analysis's syntax tree of the source file at `path` (relative to `root`), as the
    /// last plan read it. A plan's [`Code::source_start`] and [`Code::source_end`] index it.
    pub fn syntax(&self, root: &Path, path: &str) -> Option<Parse> {
        let package = self.packages.iter().find(|package| package.root == root)?;
        let &file = package.files.get(path)?;
        Some(semantics::parse(&self.db, file))
    }

    /// The roots of every package this loader has planned.
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        self.packages.iter().map(|package| package.root.as_path())
    }

    /// A strict file binds its pure constants lazily, and telling which constants are pure
    /// reads the stubs and the package's own `@pure` definitions, as `ry check` does.
    fn prepare_purity(&mut self, position: usize) -> bool {
        let sources = self.packages[position].sources;
        let strict = *sources.strict(&self.db)
            || sources
                .files(&self.db)
                .iter()
                .any(|file| *file_typing_mode(&self.db, file.file) == Some(TypingMode::Strict));
        if !strict {
            return false;
        }
        if !self.stubs_installed {
            install_shipped_stubs(&self.db);
            self.stubs_installed = true;
        }
        let files: Vec<SourceFile> = sources
            .files(&self.db)
            .iter()
            .map(|file| file.file)
            .collect();
        match ProjectFiles::try_get(&self.db) {
            Some(project) if project.files(&self.db) == &files => {}
            Some(project) => {
                project.set_files(&mut self.db).to(files);
            }
            None => {
                ProjectFiles::new(&self.db, files);
            }
        }
        true
    }

    /// Derives the changed files' per-item facts across cores, so `package_index` only joins
    /// memoized results. Salsa storage handles share memos, so the workers fill one cache.
    fn warm(&self, files: &[SourceFile], derive: impl Fn(&dyn Db, SourceFile) + Sync) {
        if files.is_empty() {
            return;
        }
        let workers = thread::available_parallelism()
            .map_or(1, |count| count.get())
            .min(files.len());
        thread::scope(|scope| {
            for worker in 0..workers {
                let db = self.db.clone();
                let files = &files;
                let derive = &derive;
                thread::Builder::new()
                    .name("ry-loader-index".to_owned())
                    .stack_size(ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        for &file in files.iter().skip(worker).step_by(workers) {
                            derive(&db, file);
                        }
                    })
                    .expect("a loader worker thread should spawn");
            }
        });
    }

    /// Reads the package's sources. A file whose size and modification time are unchanged
    /// since the last read is not read again.
    fn read(&mut self, root: &Path) -> Result<PackageInputs, LoadError> {
        let description = read(&root.join("DESCRIPTION"))?;
        let namespace = match fs::read_to_string(root.join("NAMESPACE")) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(LoadError::Io(root.join("NAMESPACE"), error)),
        };
        let paths = collated_r_files(root, &parse_description_collate(&description))?;
        let known = self.stamps.entry(root.to_path_buf()).or_default();
        let mut files = Vec::with_capacity(paths.len());
        let mut stamps = FxHashMap::default();
        for path in paths {
            let absolute = root.join(&path);
            let stamp = fs::metadata(&absolute)
                .map(|metadata| (metadata.len(), metadata.modified().ok()))
                .map_err(|error| LoadError::Io(absolute.clone(), error))?;
            let unchanged = stamp.1.is_some() && known.get(&path) == Some(&stamp);
            let text = if unchanged {
                None
            } else {
                Some(read(&absolute)?.replace("\r\n", "\n").replace('\r', "\n"))
            };
            stamps.insert(path.clone(), stamp);
            files.push((path, text));
        }
        *known = stamps;
        Ok(PackageInputs {
            description,
            namespace,
            files,
            sysdata: file_hash(&root.join("R").join("sysdata.rda"))?,
            data: datasets(root)?,
            compiled: compiled_fingerprint(&root.join("src"))?,
            strict: self.strict,
        })
    }

    /// Sets the analysis inputs to `files`, writing only what changed, and returns the
    /// package's position and the files whose text is new.
    ///
    /// Files load at high durability and drop to low on their first edit. Salsa skips
    /// verifying a memo that depends only on inputs of a durability nothing changed at, so a
    /// reload re-derives the package without revisiting the files nobody touched.
    fn sync(
        &mut self,
        root: &Path,
        description: String,
        namespace: String,
        texts: Vec<(String, Option<String>)>,
        strict: bool,
    ) -> Result<(usize, Vec<SourceFile>), LoadError> {
        let position = match self
            .packages
            .iter()
            .position(|package| package.root == root)
        {
            Some(position) => position,
            None => {
                let sources = PackageSources::builder(
                    Vec::new(),
                    namespace.clone(),
                    description.clone(),
                    strict,
                )
                .namespace_durability(Durability::HIGH)
                .description_durability(Durability::HIGH)
                .strict_durability(Durability::HIGH)
                .new(&self.db);
                self.packages.push(LoadedPackage {
                    root: root.to_path_buf(),
                    sources,
                    files: FxHashMap::default(),
                    committed: None,
                    committed_fingerprint: None,
                    pending_fingerprint: None,
                    pending: None,
                });
                self.packages.len() - 1
            }
        };
        let db = &mut self.db;
        let package = &mut self.packages[position];
        let mut files = Vec::with_capacity(texts.len());
        let mut changed = Vec::new();
        for (path, text) in texts {
            let file = match (package.files.get(&path), text) {
                (Some(&file), None) => file,
                (Some(&file), Some(text)) => {
                    if file.text(db) != text.as_str() {
                        file.set_text(db).with_durability(Durability::LOW).to(text);
                        changed.push(file);
                    }
                    file
                }
                (None, text) => {
                    let text = match text {
                        Some(text) => text,
                        None => read(&root.join(&path))?
                            .replace("\r\n", "\n")
                            .replace('\r', "\n"),
                    };
                    let file = SourceFile::builder(text, DocumentKind::Package)
                        .durability(Durability::HIGH)
                        .new(db);
                    package.files.insert(path.clone(), file);
                    changed.push(file);
                    file
                }
            };
            files.push(PackageFile { path, file });
        }
        if package.files.len() != files.len() {
            let kept: FxHashSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
            package.files.retain(|path, _| kept.contains(path.as_str()));
        }
        let sources = package.sources;
        if sources.files(db) != &files {
            sources.set_files(db).to(files);
        }
        if sources.namespace(db) != &namespace {
            sources
                .set_namespace(db)
                .with_durability(Durability::LOW)
                .to(namespace);
        }
        if *sources.strict(db) != strict {
            sources.set_strict(db).to(strict);
        }
        if sources.description(db) != &description {
            sources
                .set_description(db)
                .with_durability(Durability::LOW)
                .to(description);
        }
        Ok((position, changed))
    }
}

/// `R/` code files in R's order, by path relative to the root: those of `R/` and then those of
/// its operating-system subdirectory (`R/unix/` or `R/windows/`), ordered by
/// [`collate_code_files`].
pub fn collated_r_files(root: &Path, collate: &[String]) -> Result<Vec<String>, LoadError> {
    let directory = root.join("R");
    let mut paths = Vec::new();
    for (subdirectory, prefix) in [(directory.clone(), ""), (directory.join(OS_TYPE), OS_TYPE)] {
        let entries = match fs::read_dir(&subdirectory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(LoadError::Io(subdirectory, error)),
        };
        for entry in entries {
            let entry = entry.map_err(|error| LoadError::Io(subdirectory.clone(), error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_r_code(&name) && entry.path().is_file() {
                paths.push(if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                });
            }
        }
    }
    Ok(collate_code_files(paths, collate)
        .into_iter()
        .map(|path| format!("R/{path}"))
        .collect())
}

/// Orders code files given by path relative to `R/` (`a.R`, `unix/b.R`) as a load runs them:
/// the files the collation field lists first, in its order, then the rest as R CMD INSTALL
/// lists them, the files of `R/` before those of a subdirectory and each group in C-locale
/// order. R CMD INSTALL refuses a package whose collation field leaves a file out, while
/// `devtools::load_all` appends it, and so does this.
pub fn collate_code_files(mut paths: Vec<String>, collate: &[String]) -> Vec<String> {
    paths.sort_by(|left, right| (left.contains('/'), left).cmp(&(right.contains('/'), right)));
    let mut ordered: Vec<String> = collate
        .iter()
        .filter(|path| paths.contains(path))
        .cloned()
        .collect();
    let mut seen: FxHashSet<String> = ordered.iter().cloned().collect();
    for path in paths {
        if seen.insert(path.clone()) {
            ordered.push(path);
        }
    }
    ordered
}

/// Whether a file in `R/` is code a load sources, as R CMD INSTALL decides: its name starts
/// with a letter or digit, which leaves out an editor's `.a.R.swp`, and its extension is one
/// of R's code extensions, which leaves out `a.R~`.
fn is_r_code(name: &str) -> bool {
    name.starts_with(|character: char| character.is_ascii_alphanumeric())
        && Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "R" | "r" | "S" | "s" | "q"))
}

/// The dataset files of `data/`, as `data()` and R CMD INSTALL find them: `.rda` and `.RData`
/// files, which `load()` reads, `.R` files, which `sys.source()` runs, and `.csv`, `.tab` and
/// `.txt` tables, which `read.table()` reads.
fn datasets(root: &Path) -> Result<Option<Datasets>, LoadError> {
    let directory = root.join("data");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(LoadError::Io(directory, error)),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| LoadError::Io(directory.clone(), error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_dataset(&name) {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| LoadError::Io(entry.path(), error))?;
        if metadata.is_file() {
            records.push((name, stamp(&metadata)));
        }
    }
    if records.is_empty() {
        return Ok(None);
    }
    records.sort();
    let fingerprint = fnv1a(
        records
            .iter()
            .map(|(name, stamp)| format!("{name}\0{stamp}"))
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
    );
    Ok(Some(Datasets {
        files: records
            .into_iter()
            .map(|(name, _)| format!("data/{name}"))
            .collect(),
        fingerprint,
    }))
}

/// Whether a file in `data/` is a dataset a load reads. The R side picks the reader by the
/// same extensions.
pub fn is_dataset(name: &str) -> bool {
    !name.starts_with('.')
        && Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension,
                    "R" | "r" | "rda" | "RData" | "rdata" | "csv" | "CSV" | "tab" | "txt" | "TXT"
                )
            })
}

/// The plan that turns `previous` (nothing, for a full load) into `next`. It looks inside only
/// the files whose text changed, plus the eager code of every file, which is all that a change
/// can reach. `text_of` returns a source file's R text by path.
pub fn diff<'text>(
    previous: Option<&LoadState>,
    next: &LoadState,
    text_of: &dyn Fn(&str) -> &'text str,
) -> Plan {
    if let Some(previous) = previous
        && Arc::ptr_eq(&previous.index, &next.index)
        && previous.sysdata == next.sysdata
        && previous.data == next.data
        && previous.compiled == next.compiled
    {
        return Plan::unchanged(next);
    }
    let index = &*next.index;
    // When `Collate` reorders files, statements in different files swap their order, which
    // only a fresh load replays faithfully.
    let previous = previous.filter(|previous| same_relative_order(&previous.index, index));
    let full = previous.is_none();
    let empty = PackageIndex::default();
    let old = previous.map_or(&empty, |previous| &*previous.index);

    let by_path = |files: &'_ [IndexedFile]| -> FxHashMap<String, u64> {
        files
            .iter()
            .map(|file| (file.path.clone(), file.items.hash))
            .collect()
    };
    let old_hashes = by_path(&old.files);
    let new_hashes = by_path(&index.files);
    // A clean file's text, and so every item in it, is unchanged.
    let is_dirty = |file: &IndexedFile, other: &FxHashMap<String, u64>| {
        full || other.get(&file.path) != Some(&file.items.hash)
    };
    let new_dirty: Vec<bool> = index
        .files
        .iter()
        .map(|file| is_dirty(file, &old_hashes))
        .collect();
    let old_dirty: Vec<&IndexedFile> = old
        .files
        .iter()
        .filter(|file| is_dirty(file, &new_hashes))
        .collect();
    fn dirty_items<'a>(files: &[&'a IndexedFile]) -> Vec<(&'a str, &'a IndexedItem)> {
        files
            .iter()
            .flat_map(|file| {
                file.items
                    .items
                    .iter()
                    .map(|item| (file.path.as_str(), item))
            })
            .collect()
    }
    let old_items = dirty_items(&old_dirty);
    let new_dirty_files: Vec<&IndexedFile> = index
        .files
        .iter()
        .zip(&new_dirty)
        .filter(|(_, dirty)| **dirty)
        .map(|(file, _)| file)
        .collect();
    let new_items = dirty_items(&new_dirty_files);

    // Names whose laziness flipped: a second writer appeared or went away, or, for a constant
    // of a strict file, a name it mentions or a function it calls changed in another file.
    // The latter can flip a name no dirty item writes, so every name is a candidate, unless
    // the analysis reused the whole map.
    let flipped: FxHashSet<&str> = if Arc::ptr_eq(&old.bindings, &index.bindings) {
        FxHashSet::default()
    } else {
        old.bindings
            .names
            .keys()
            .chain(index.bindings.names.keys())
            .map(String::as_str)
            .filter(|name| old.is_lazy(name) != index.is_lazy(name))
            .collect()
    };

    let registers_s4 = |item: &IndexedItem| {
        item.registrations.iter().any(|registration| {
            matches!(
                registration,
                Registration::S4Class { .. }
                    | Registration::S4Generic { .. }
                    | Registration::S4Method { .. }
            )
        })
    };
    let lazy_code = |path: &str, name: &str, item: &IndexedItem| {
        let span = item
            .binding
            .as_ref()
            .map_or(item.span, |binding| binding.value);
        Code {
            name: Some(name.to_owned()),
            path: path.to_owned(),
            line: span.line,
            column: span.column,
            start: span.char_start,
            end: span.char_end,
            source_start: span.source_start,
            source_end: span.source_end,
            registers_s4: false,
        }
    };
    let eager_code = |path: &str, item: &IndexedItem| Code {
        name: None,
        path: path.to_owned(),
        line: item.span.line,
        column: item.span.column,
        start: item.span.char_start,
        end: item.span.char_end,
        source_start: item.span.source_start,
        source_end: item.span.source_end,
        registers_s4: registers_s4(item),
    };

    // Lazy definitions are keyed by name, which only one item writes.
    let old_lazy: FxHashMap<&str, (&str, &IndexedItem)> = old_items
        .iter()
        .filter_map(|&(path, item)| Some((old.lazy_name(item)?, (path, item))))
        .collect();
    let mut changed: FxHashSet<String> = FxHashSet::default();
    let mut bind = Vec::new();
    for (path, item) in &new_items {
        let Some(name) = index.lazy_name(item) else {
            continue;
        };
        let rebind = match old_lazy.get(name) {
            Some((old_path, previous)) if previous.hash == item.hash => {
                // The same code at a new position: rebinding keeps source references right,
                // and nothing that read it needs to rerun.
                let position = |item: &IndexedItem| {
                    item.binding
                        .as_ref()
                        .map(|binding| (binding.value.line, binding.value.column))
                };
                old_path != path || position(previous) != position(item)
            }
            _ => {
                changed.insert(name.to_owned());
                true
            }
        };
        if rebind {
            bind.push(lazy_code(path, name, item));
        }
    }
    for name in old_lazy.keys() {
        if !index.is_lazy(name) {
            changed.insert((*name).to_owned());
        }
    }
    // A definition in a clean file whose name became lazy is bound afresh.
    let flipped_to_lazy: FxHashSet<&str> = flipped
        .iter()
        .copied()
        .filter(|name| index.is_lazy(name))
        .collect();
    if !flipped_to_lazy.is_empty() {
        for (file, dirty) in index.files.iter().zip(&new_dirty) {
            if *dirty {
                continue;
            }
            for item in &file.items.items {
                if let Some(name) = index.lazy_name(item)
                    && flipped_to_lazy.contains(name)
                {
                    changed.insert(name.to_owned());
                    bind.push(lazy_code(&file.path, name, item));
                }
            }
        }
    }

    // Eager items in a dirty file are matched to the file's old eager items by content, in
    // order (a longest common subsequence), so inserting a statement does not rerun the ones
    // after it, while swapping two statements reruns one of them and so restores which
    // writer comes last.
    let mut old_eager: FxHashMap<&str, Vec<&IndexedItem>> = FxHashMap::default();
    for (path, item) in &old_items {
        if old.lazy_name(item).is_none() {
            old_eager.entry(path).or_default().push(item);
        }
    }
    let mut fresh: FxHashSet<(usize, usize)> = FxHashSet::default();
    let mut vanished: Vec<&IndexedItem> = Vec::new();
    for (file_position, (file, dirty)) in index.files.iter().zip(&new_dirty).enumerate() {
        if !*dirty && flipped.is_empty() {
            continue;
        }
        let eager_positions: Vec<usize> = file
            .items
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| index.lazy_name(item).is_none())
            .map(|(position, _)| position)
            .collect();
        if !*dirty {
            // A definition in a clean file that stopped being lazy now runs in order.
            for &position in &eager_positions {
                let item = &file.items.items[position];
                if item
                    .binding
                    .as_ref()
                    .is_some_and(|binding| flipped.contains(binding.name.as_str()))
                {
                    fresh.insert((file_position, position));
                }
            }
            continue;
        }
        let previous = old_eager.remove(file.path.as_str()).unwrap_or_default();
        let current: Vec<u64> = eager_positions
            .iter()
            .map(|&position| file.items.items[position].hash)
            .collect();
        let earlier: Vec<u64> = previous.iter().map(|item| item.hash).collect();
        let (kept_new, kept_old) = common_subsequence(&earlier, &current);
        for (index_in_file, &position) in eager_positions.iter().enumerate() {
            if !kept_new.contains(&index_in_file) {
                fresh.insert((file_position, position));
            }
        }
        vanished.extend(
            previous
                .iter()
                .enumerate()
                .filter(|(position, _)| !kept_old.contains(position))
                .map(|(_, item)| *item),
        );
    }
    // Old eager items of files that are gone, or are no longer package code.
    vanished.extend(old_eager.into_values().flatten());
    // What a vanished statement wrote must be written again by whoever else writes it, or
    // its value would survive; the same goes for the old version of an edited statement.
    for item in &vanished {
        changed.extend(item.binds.iter().cloned());
    }
    // A statement that called a function holds what the call returned, so a change anywhere
    // below that call counts too: a lazy function whose body mentions a changed name counts as
    // changed, transitively, and so does any function a statement reads.
    // A full load reruns everything, so it needs no call graph.
    let functions = (!full).then_some(index);
    // A statement reruns when it is new, when it read a binding that changed (it captured the
    // old value), or when it writes one (the last writer in collation order must win again).
    // A rerun statement must also see what it saw at load: when it reads a name that it or a
    // later statement rewrites, as `f <- memoise(f)` does, the earlier writers of that name
    // rerun first. Otherwise it would read the final value and, here, memoise twice.
    let eager: Vec<(&str, &IndexedItem, bool)> = index
        .bindings
        .eager
        .iter()
        .filter_map(|&at| {
            let (path, item) = index.item_at(at)?;
            Some((path, item, fresh.contains(&(at.0 as usize, at.1 as usize))))
        })
        .collect();
    let mut writers: FxHashMap<&str, Vec<usize>> = FxHashMap::default();
    for (position, (_, item, _)) in eager.iter().enumerate() {
        for name in &item.binds {
            writers.entry(name).or_default().push(position);
        }
    }
    let mut rerun: Vec<bool> = eager.iter().map(|(_, _, fresh)| full || *fresh).collect();
    // A lazy constant holds the value its initializer computed when it was forced, so like an
    // eager statement it is bound afresh when anything its initializer reaches changed.
    let constants: Vec<(&str, &str, &IndexedItem)> = if full {
        Vec::new()
    } else {
        index
            .bindings
            .constants
            .iter()
            .filter_map(|&at| {
                let (path, item) = index.item_at(at)?;
                Some((path, index.lazy_name(item)?, item))
            })
            .collect()
    };
    let mut bound: FxHashSet<String> = bind.iter().filter_map(|code| code.name.clone()).collect();
    loop {
        if !full && !changed.is_empty() {
            let roots: Vec<&str> = eager
                .iter()
                .flat_map(|(_, item, _)| item.reads.iter().map(String::as_str))
                .collect();
            let reaching: Vec<String> = reaching_changed(&roots, functions, &changed)
                .into_iter()
                .filter(|name| !changed.contains(*name))
                .map(str::to_owned)
                .collect();
            changed.extend(reaching);
        }
        let changed_before = changed.len();
        let mut grew = false;
        for (path, name, item) in &constants {
            if bound.contains(*name) {
                continue;
            }
            if reached_through_calls(&item.mentions, functions)
                .iter()
                .any(|reached| changed.contains(*reached))
            {
                bind.push(lazy_code(path, name, item));
                bound.insert((*name).to_owned());
                changed.insert((*name).to_owned());
                grew = true;
            }
        }
        for (position, (_, item, _)) in eager.iter().enumerate() {
            if !rerun[position]
                && !changed.is_empty()
                && item
                    .reads
                    .iter()
                    .chain(&item.binds)
                    .any(|name| changed.contains(name))
            {
                rerun[position] = true;
                grew = true;
            }
            if !rerun[position] {
                continue;
            }
            changed.extend(item.binds.iter().cloned());
            for name in reached_through_calls(&item.reads, functions) {
                let Some(positions) = writers.get(name) else {
                    continue;
                };
                if positions.last().is_some_and(|last| *last >= position) {
                    for &earlier in positions.iter().take_while(|earlier| **earlier < position) {
                        grew |= !rerun[earlier];
                        rerun[earlier] = true;
                    }
                }
            }
        }
        // A statement earlier in the order may read what a later one just changed.
        if !grew && changed.len() == changed_before {
            break;
        }
    }
    let run: Vec<Code> = eager
        .iter()
        .zip(&rerun)
        .filter(|(_, rerun)| **rerun)
        .map(|((path, item, _), _)| eager_code(path, item))
        .collect();
    // Another statement may make the same registration, which then stays.
    let registered: BTreeSet<&Registration> = eager
        .iter()
        .flat_map(|(_, item, _)| &item.registrations)
        .collect();

    let mut remove = BTreeSet::new();
    for (_, item) in &old_items {
        if old.lazy_name(item).is_some() {
            remove.extend(item.binds.iter().filter(|name| !index.binds(name)).cloned());
        }
    }
    let mut unregister = BTreeSet::new();
    for item in &vanished {
        remove.extend(item.binds.iter().filter(|name| !index.binds(name)).cloned());
        unregister.extend(
            item.registrations
                .iter()
                .filter(|registration| !registered.contains(registration))
                .cloned(),
        );
    }

    let sources = new_dirty_files
        .iter()
        .map(|file| SourceText {
            path: file.path.clone(),
            text: text_of(&file.path).to_owned(),
        })
        .collect();
    let dropped_sources = old
        .files
        .iter()
        .filter(|file| !new_hashes.contains_key(&file.path))
        .map(|file| file.path.clone())
        .collect();

    let declared = &index.namespace.s3_methods;
    let old_declared = &old.namespace.s3_methods;
    let s3_changed = full || declared != old_declared;
    let rebound: FxHashSet<&str> = bind
        .iter()
        .filter_map(|code| code.name.as_deref())
        .collect();
    let (old_set, new_set): (FxHashSet<&S3MethodDirective>, FxHashSet<&S3MethodDirective>) =
        if s3_changed {
            (old_declared.iter().collect(), declared.iter().collect())
        } else {
            Default::default()
        };
    let s3_register = declared
        .iter()
        .filter(|method| {
            full || (s3_changed && !old_set.contains(method))
                || changed.contains(&method.function)
                || rebound.contains(method.function.as_str())
        })
        .cloned()
        .collect();
    let s3_unregister = old_declared
        .iter()
        .filter(|method| s3_changed && !new_set.contains(method))
        .cloned()
        .collect();

    let previous_sysdata = previous.and_then(|previous| previous.sysdata);
    let sysdata = match (previous_sysdata, next.sysdata) {
        (_, Some(_)) if full => SysdataChange::Load,
        (old, Some(new)) if old != Some(new) => SysdataChange::Load,
        (Some(_), None) => SysdataChange::Remove,
        _ => SysdataChange::Keep,
    };
    let data = match (
        previous.and_then(|previous| previous.data.as_ref()),
        &next.data,
    ) {
        (_, Some(new)) if full => Some(new.files.clone()),
        (old, Some(new)) if old != Some(new) => Some(new.files.clone()),
        (Some(_), None) => Some(Vec::new()),
        _ => None,
    };
    let compiled_changed = previous.map(|previous| previous.compiled) != Some(next.compiled)
        || index.namespace.dynamic_libraries != old.namespace.dynamic_libraries;

    let warnings = index
        .namespace
        .unsupported
        .iter()
        .map(|directive| {
            format!("NAMESPACE directive ignored, because ry cannot evaluate it: {directive}")
        })
        .collect();

    let exports_changed = full || exports_differ(&index.namespace, &old.namespace);
    let keeps_names = !full
        && !exports_changed
        && remove.is_empty()
        && unregister.is_empty()
        && bind
            .iter()
            .all(|code| code.name.as_ref().is_some_and(|name| old.binds(name)));

    Plan {
        package: index.package.clone(),
        version: index.version.clone(),
        root: next.root.clone(),
        full,
        depends: if full {
            index.depends.clone()
        } else {
            Vec::new()
        },
        imports: (full || index.namespace.imports != old.namespace.imports)
            .then(|| index.namespace.imports.clone()),
        sources,
        dropped_sources,
        remove: remove.into_iter().collect(),
        unregister: unregister.into_iter().collect(),
        bind,
        run_on_load: full || changed.contains(".onLoad"),
        run,
        sysdata,
        data,
        s3_register,
        s3_unregister,
        s3_declared: s3_changed.then(|| declared.clone()),
        dynamic_libraries: (full || compiled_changed)
            .then(|| index.namespace.dynamic_libraries.clone())
            .filter(|libraries| !libraries.is_empty() || !full),
        exports_changed,
        keeps_names,
        namespace: index.namespace.clone(),
        warnings,
    }
}

/// The names among `roots` whose call may read a changed binding, through any chain of lazy
/// functions. A forward walk collects the call graph the roots reach, then a backward walk from
/// the changed names marks everything that reaches one; both visit each function once.
fn reaching_changed<'a>(
    roots: &[&'a str],
    functions: Option<&'a PackageIndex>,
    changed: &FxHashSet<String>,
) -> FxHashSet<&'a str> {
    let mut callers: FxHashMap<&'a str, Vec<&'a str>> = FxHashMap::default();
    let mut seen: FxHashSet<&'a str> = roots.iter().copied().collect();
    let mut queue: Vec<&'a str> = roots.to_vec();
    while let Some(name) = queue.pop() {
        let Some(item) = functions.and_then(|index| index.lazy_item(name)) else {
            continue;
        };
        for mention in &item.mentions {
            let mention = mention.as_str();
            callers.entry(mention).or_default().push(name);
            if seen.insert(mention) {
                queue.push(mention);
            }
        }
    }
    let mut reaching: FxHashSet<&'a str> = seen
        .iter()
        .copied()
        .filter(|name| changed.contains(*name))
        .collect();
    let mut queue: Vec<&'a str> = reaching.iter().copied().collect();
    while let Some(name) = queue.pop() {
        for &caller in callers.get(name).into_iter().flatten() {
            if reaching.insert(caller) {
                queue.push(caller);
            }
        }
    }
    reaching
}

/// `reads` plus every name the lazy functions among them mention, transitively: what running
/// a statement that calls them may read.
fn reached_through_calls<'a>(
    reads: &'a [String],
    functions: Option<&'a PackageIndex>,
) -> FxHashSet<&'a str> {
    let mut seen: FxHashSet<&'a str> = reads.iter().map(String::as_str).collect();
    let mut queue: Vec<&'a str> = seen.iter().copied().collect();
    while let Some(name) = queue.pop() {
        for mention in functions
            .and_then(|index| index.lazy_item(name))
            .map_or(&[][..], |item| item.mentions.as_slice())
        {
            if seen.insert(mention) {
                queue.push(mention);
            }
        }
    }
    seen
}

/// Whether the files both indexes hold appear in the same order in each.
fn same_relative_order(old: &PackageIndex, new: &PackageIndex) -> bool {
    let old_paths: FxHashSet<&str> = old.files.iter().map(|file| file.path.as_str()).collect();
    let new_paths: FxHashSet<&str> = new.files.iter().map(|file| file.path.as_str()).collect();
    old.files
        .iter()
        .map(|file| file.path.as_str())
        .filter(|path| new_paths.contains(path))
        .eq(new
            .files
            .iter()
            .map(|file| file.path.as_str())
            .filter(|path| old_paths.contains(path)))
}

/// The positions in `new` and in `old` of a longest common subsequence of the two.
fn common_subsequence(old: &[u64], new: &[u64]) -> (FxHashSet<usize>, FxHashSet<usize>) {
    // Equal sequences, the common case for a file whose functions alone changed, need no table.
    if old == new {
        return ((0..new.len()).collect(), (0..old.len()).collect());
    }
    let width = new.len() + 1;
    let mut lengths = vec![0u32; (old.len() + 1) * width];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lengths[i * width + j] = if old[i] == new[j] {
                lengths[(i + 1) * width + j + 1] + 1
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }
    let (mut kept_new, mut kept_old) = (FxHashSet::default(), FxHashSet::default());
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            kept_old.insert(i);
            kept_new.insert(j);
            i += 1;
            j += 1;
        } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (kept_new, kept_old)
}

fn exports_differ(new: &NamespaceSpec, old: &NamespaceSpec) -> bool {
    new.exports != old.exports
        || new.export_patterns != old.export_patterns
        || new.export_classes != old.export_classes
        || new.export_class_patterns != old.export_class_patterns
        || new.export_methods != old.export_methods
}

impl Plan {
    fn unchanged(state: &LoadState) -> Plan {
        Plan {
            package: state.index.package.clone(),
            version: state.index.version.clone(),
            root: state.root.clone(),
            full: false,
            depends: Vec::new(),
            imports: None,
            sources: Vec::new(),
            dropped_sources: Vec::new(),
            remove: Vec::new(),
            unregister: Vec::new(),
            bind: Vec::new(),
            run: Vec::new(),
            sysdata: SysdataChange::Keep,
            data: None,
            s3_register: Vec::new(),
            s3_unregister: Vec::new(),
            s3_declared: None,
            dynamic_libraries: None,
            namespace: state.index.namespace.clone(),
            exports_changed: false,
            keeps_names: true,
            run_on_load: false,
            warnings: Vec::new(),
        }
    }

    /// The bindings this plan writes or deletes, which a backend mirrors onto the attached
    /// package environment. Bindings that eager code makes dynamically (`assign()`) are not
    /// known statically, so a plan that runs code calls for a full resync instead.
    pub fn touched_names(&self) -> Option<Vec<String>> {
        if self.full
            || !self.run.is_empty()
            || self.imports.is_some()
            || self.sysdata != SysdataChange::Keep
            || self.data.is_some()
            || self.dynamic_libraries.is_some()
            || self.run_on_load
        {
            return None;
        }
        let mut names: Vec<String> = self
            .bind
            .iter()
            .filter_map(|code| code.name.clone())
            .chain(self.remove.iter().cloned())
            .collect();
        names.sort();
        names.dedup();
        Some(names)
    }

    /// Nothing to do but resync exports, which costs nothing.
    pub fn is_noop(&self) -> bool {
        !self.full
            && self.imports.is_none()
            && self.remove.is_empty()
            && self.unregister.is_empty()
            && self.bind.is_empty()
            && self.run.is_empty()
            && self.sysdata == SysdataChange::Keep
            && self.data.is_none()
            && self.s3_register.is_empty()
            && self.s3_unregister.is_empty()
            && self.dynamic_libraries.is_none()
            && !self.exports_changed
    }
}

/// A hash of the names, sizes and modification times of `DESCRIPTION`, `NAMESPACE`, the code
/// files in `R/` and its operating-system subdirectory, `R/sysdata.rda` and the datasets in
/// `data/`, so a change is noticed without reading a file. `src/` is left out: hashing it would
/// cost a walk of the compiled sources before every command, and a rebuild is slow enough that
/// it is asked for with `load_all()`.
fn source_fingerprint(root: &Path) -> Option<u64> {
    let mut records = Vec::new();
    let mut record = |path: &Path| {
        if let Ok(metadata) = fs::metadata(path) {
            records.push(format!("{}\0{}", path.display(), stamp(&metadata)));
        }
    };
    record(&root.join("DESCRIPTION"));
    record(&root.join("NAMESPACE"));
    record(&root.join("R").join("sysdata.rda"));
    let code = fs::read_dir(root.join("R")).ok()?;
    let os_code = fs::read_dir(root.join("R").join(OS_TYPE))
        .into_iter()
        .flatten();
    // An editor's swap and backup files (`.a.R.swp`, `a.R~`) are not code, so saving one
    // reloads nothing.
    for entry in code.chain(os_code).flatten() {
        if is_r_code(&entry.file_name().to_string_lossy()) {
            record(&entry.path());
        }
    }
    for entry in fs::read_dir(root.join("data"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if is_dataset(&entry.file_name().to_string_lossy()) {
            record(&entry.path());
        }
    }
    records.sort();
    Some(fnv1a(records.join("\n").as_bytes()))
}

/// A file's size and modification time, as one record of a fingerprint.
fn stamp(metadata: &fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    format!("{}\0{modified}", metadata.len())
}

fn read(path: &Path) -> Result<String, LoadError> {
    fs::read_to_string(path).map_err(|error| LoadError::Io(path.to_path_buf(), error))
}

fn file_hash(path: &Path) -> Result<Option<u64>, LoadError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(fnv1a(&bytes))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LoadError::Io(path.to_path_buf(), error)),
    }
}

/// Names, sizes and modification times of the sources under `src/`, its subdirectories
/// included, which change whenever a rebuild is due without reading every file.
fn compiled_fingerprint(directory: &Path) -> Result<Option<u64>, LoadError> {
    if !directory.is_dir() {
        return Ok(None);
    }
    let mut records = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries =
            fs::read_dir(&current).map_err(|error| LoadError::Io(current.clone(), error))?;
        for entry in entries {
            let entry = entry.map_err(|error| LoadError::Io(current.clone(), error))?;
            let path = entry.path();
            let metadata = entry
                .metadata()
                .map_err(|error| LoadError::Io(path.clone(), error))?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if is_build_product(&path) {
                continue;
            }
            let relative = path
                .strip_prefix(directory)
                .unwrap_or(&path)
                .display()
                .to_string();
            records.push(format!("{relative}\0{}", stamp(&metadata)));
        }
    }
    records.sort();
    Ok(Some(fnv1a(records.join("\n").as_bytes())))
}

/// Build products change on every build, so only sources decide whether one is due.
pub fn is_build_product(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "o" | "so" | "dll" | "dylib"))
}
