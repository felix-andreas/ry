//! The package-development loader: reads a package's sources into the analysis, derives its
//! load index, and diffs that index against what is loaded to produce a [`Plan`]. A plan says
//! what to bind, run, and remove, and nothing about how; a backend executes it. The GNU R
//! backend ([`gnur`]) hands it to R code running in the session, and ry's own runtime will take
//! the same plan.

pub mod gnur;

use rustc_hash::{FxHashMap, FxHashSet};
use salsa::Setter;
use semantics::load_index::{
    Binding, PackageFile, PackageIndex, PackageSources, Registration, fnv1a, package_index,
};
use semantics::metadata::{
    DynamicLibraryDirective, NamespaceImportDirective, NamespaceSpec, S3MethodDirective,
    parse_description_collate,
};
use semantics::{DocumentKind, RootDatabase, SourceFile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One process's loader. It keeps one analysis database for every package it loads, so a
/// reload re-derives only the items whose text changed.
#[derive(Default)]
pub struct Loader {
    db: RootDatabase,
    packages: Vec<LoadedPackage>,
}

struct LoadedPackage {
    root: PathBuf,
    sources: PackageSources,
    files: FxHashMap<String, SourceFile>,
    /// What the session has loaded, as confirmed by [`Loader::commit`].
    committed: Option<LoadState>,
    /// The state a plan was made for, until the backend confirms it applied.
    pending: Option<LoadState>,
}

/// What is loaded for one package: the load index plus the inputs the analysis does not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadState {
    pub root: PathBuf,
    pub index: PackageIndex,
    /// Of `R/sysdata.rda`.
    pub sysdata: Option<u64>,
    /// Of the names, sizes and modification times under `src/`.
    pub compiled: Option<u64>,
}

/// Backend-neutral instructions, applied in field order: imports, sources, removals,
/// unregistrations, lazy bindings, eager code, data, S3 registrations, compiled code,
/// exports, and finally the hooks.
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
    /// Function definitions to bind lazily.
    pub bind: Vec<Code>,
    /// Top-level code to run, in collation order.
    pub run: Vec<Code>,
    pub sysdata: SysdataChange,
    /// `S3method()` registrations to (re)make.
    pub s3_register: Vec<S3MethodDirective>,
    /// `S3method()` registrations that `NAMESPACE` no longer declares.
    pub s3_unregister: Vec<S3MethodDirective>,
    /// The declared S3 methods, sent whenever they changed.
    pub s3_declared: Option<Vec<S3MethodDirective>>,
    /// `Some` when compiled code must be (re)built and loaded.
    pub dynamic_libraries: Option<Vec<DynamicLibraryDirective>>,
    pub namespace: NamespaceSpec,
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

/// One item's code, positioned in its source file so R's source references point at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// The binding a lazy definition creates.
    pub name: Option<String>,
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub text: String,
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
    /// Reads the package at or above `path` and plans loading it. Without a committed state, or
    /// with `full`, the plan builds the namespace from scratch; otherwise it patches the loaded
    /// one. The plan stays pending until [`Loader::commit`].
    pub fn plan(&mut self, path: &Path, full: bool) -> Result<Plan, LoadError> {
        let root = find_package_root(path)?;
        let position = self.refresh(&root)?;
        let package = &self.packages[position];
        let index = package_index(&self.db, package.sources);
        if index.package.is_empty() {
            return Err(LoadError::NoPackageName(root));
        }
        if !index.syntax_errors.is_empty() {
            return Err(LoadError::Syntax(index.syntax_errors.clone()));
        }
        let next = LoadState {
            root: root.clone(),
            index: index.clone(),
            sysdata: file_hash(&root.join("R").join("sysdata.rda"))?,
            compiled: compiled_fingerprint(&root.join("src"))?,
        };
        let previous = if full {
            None
        } else {
            package
                .committed
                .as_ref()
                .filter(|previous| previous.index.package == next.index.package)
        };
        let texts: FxHashMap<&str, &str> = package
            .sources
            .files(&self.db)
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    semantics::load_index::file_load_index(&self.db, file.file)
                        .text
                        .as_str(),
                )
            })
            .collect();
        let plan = diff(previous, &next, &|path| {
            texts.get(path).copied().unwrap_or_default()
        });
        self.packages[position].pending = Some(next);
        Ok(plan)
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
                true
            }
            None => false,
        }
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
        let position = self.refresh(&state.root)?;
        self.packages[position].committed = Some(state);
        Ok(())
    }

    /// The roots of every package this loader has planned.
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        self.packages.iter().map(|package| package.root.as_path())
    }

    /// Syncs the analysis inputs with the disk, setting only what changed, and returns the
    /// package's position.
    fn refresh(&mut self, root: &Path) -> Result<usize, LoadError> {
        let description = read(&root.join("DESCRIPTION"))?;
        let namespace = match fs::read_to_string(root.join("NAMESPACE")) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(LoadError::Io(root.join("NAMESPACE"), error)),
        };
        let paths = collated_r_files(root, &parse_description_collate(&description))?;
        let mut texts = Vec::with_capacity(paths.len());
        for path in &paths {
            let text = read(&root.join(path))?;
            texts.push(text.replace("\r\n", "\n").replace('\r', "\n"));
        }

        let position = match self
            .packages
            .iter()
            .position(|package| package.root == root)
        {
            Some(position) => position,
            None => {
                let sources =
                    PackageSources::new(&self.db, Vec::new(), String::new(), String::new());
                self.packages.push(LoadedPackage {
                    root: root.to_path_buf(),
                    sources,
                    files: FxHashMap::default(),
                    committed: None,
                    pending: None,
                });
                self.packages.len() - 1
            }
        };
        let db = &mut self.db;
        let package = &mut self.packages[position];
        let mut files = Vec::with_capacity(paths.len());
        for (path, text) in paths.into_iter().zip(texts) {
            let file = match package.files.get(&path) {
                Some(&file) => {
                    if file.text(db) != text.as_str() {
                        file.set_text(db).to(text);
                    }
                    file
                }
                None => {
                    let file = SourceFile::new(db, text, DocumentKind::Package);
                    package.files.insert(path.clone(), file);
                    file
                }
            };
            files.push(PackageFile { path, file });
        }
        package
            .files
            .retain(|path, _| files.iter().any(|file| &file.path == path));
        let sources = package.sources;
        if sources.files(db) != &files {
            sources.set_files(db).to(files);
        }
        if sources.namespace(db) != &namespace {
            sources.set_namespace(db).to(namespace);
        }
        if sources.description(db) != &description {
            sources.set_description(db).to(description);
        }
        Ok(position)
    }
}

/// `R/` code files in R's order: the `Collate` field's files first, as listed, then any
/// file it leaves out in C-locale order, as `devtools::load_all` does.
pub fn collated_r_files(root: &Path, collate: &[String]) -> Result<Vec<String>, LoadError> {
    let directory = root.join("R");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(LoadError::Io(directory, error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| LoadError::Io(directory.clone(), error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_code = Path::new(&name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "R" | "r" | "S" | "s" | "q"));
        if is_code && entry.path().is_file() {
            names.push(name);
        }
    }
    names.sort();
    let mut ordered: Vec<String> = collate
        .iter()
        .filter(|name| names.contains(name))
        .cloned()
        .collect();
    let mut seen: FxHashSet<String> = ordered.iter().cloned().collect();
    for name in names {
        if seen.insert(name.clone()) {
            ordered.push(name);
        }
    }
    Ok(ordered
        .into_iter()
        .map(|name| format!("R/{name}"))
        .collect())
}

/// The plan that turns `previous` (nothing, for a full load) into `next`. `text_of` returns a
/// source file's R text by path.
pub fn diff<'text>(
    previous: Option<&LoadState>,
    next: &LoadState,
    text_of: &dyn Fn(&str) -> &'text str,
) -> Plan {
    let index = &next.index;
    let full = previous.is_none();
    let empty = PackageIndex::default();
    let old = previous.map_or(&empty, |previous| &previous.index);

    let code = |item: &semantics::load_index::IndexedItem, name: Option<String>| {
        let span = match &item.binding {
            Binding::Lazy { function, .. } => *function,
            Binding::Eager { .. } => item.span,
        };
        Code {
            name,
            path: item.path.clone(),
            line: span.line,
            column: span.column,
            text: span.slice(text_of(&item.path)).to_owned(),
            registers_s4: item.registrations.iter().any(|registration| {
                matches!(
                    registration,
                    Registration::S4Class { .. }
                        | Registration::S4Generic { .. }
                        | Registration::S4Method { .. }
                )
            }),
        }
    };

    // Lazy functions are keyed by name, which only one item writes.
    let old_lazy: FxHashMap<&str, &semantics::load_index::IndexedItem> = old
        .items
        .iter()
        .filter_map(|item| match &item.binding {
            Binding::Lazy { name, .. } => Some((name.as_str(), item)),
            Binding::Eager { .. } => None,
        })
        .collect();
    let mut changed: FxHashSet<String> = FxHashSet::default();
    let mut bind = Vec::new();
    let mut new_names: FxHashSet<&str> = FxHashSet::default();
    for item in &index.items {
        match &item.binding {
            Binding::Lazy { name, function } => {
                new_names.insert(name);
                let rebind = match old_lazy.get(name.as_str()) {
                    Some(previous) if previous.hash == item.hash => {
                        // Same code at a new position: rebinding keeps source references
                        // right, and nothing that read it needs to rerun.
                        let Binding::Lazy {
                            function: previous_function,
                            ..
                        } = &previous.binding
                        else {
                            continue;
                        };
                        previous.path != item.path
                            || previous_function.line != function.line
                            || previous_function.column != function.column
                    }
                    _ => {
                        changed.insert(name.clone());
                        true
                    }
                };
                if rebind || full {
                    bind.push(code(item, Some(name.clone())));
                }
            }
            Binding::Eager { binds, .. } => new_names.extend(binds.iter().map(String::as_str)),
        }
    }
    for name in old_lazy.keys() {
        if !new_names.contains(name) {
            changed.insert((*name).to_owned());
        }
    }

    // Eager items are matched by content within their file, so inserting a statement does
    // not rerun the statements after it.
    let mut unmatched_old: FxHashMap<(&str, u64), usize> = FxHashMap::default();
    for item in &old.items {
        if matches!(item.binding, Binding::Eager { .. }) {
            *unmatched_old
                .entry((item.path.as_str(), item.hash))
                .or_default() += 1;
        }
    }
    let mut run = Vec::new();
    let mut run_registrations: BTreeSet<&Registration> = BTreeSet::new();
    for item in &index.items {
        let Binding::Eager { binds, reads } = &item.binding else {
            continue;
        };
        let unchanged = match unmatched_old.get_mut(&(item.path.as_str(), item.hash)) {
            Some(count) if *count > 0 => {
                *count -= 1;
                true
            }
            _ => false,
        };
        let stale = reads.iter().any(|name| changed.contains(name));
        if full || !unchanged || stale {
            changed.extend(binds.iter().cloned());
            run_registrations.extend(&item.registrations);
            run.push(code(item, None));
        }
    }

    let mut remove = BTreeSet::new();
    let mut unregister = BTreeSet::new();
    let mut leftover = unmatched_old;
    for item in &old.items {
        match &item.binding {
            Binding::Lazy { name, .. } => {
                if !new_names.contains(name.as_str()) {
                    remove.insert(name.clone());
                }
            }
            Binding::Eager { binds, .. } => {
                let Some(count) = leftover.get_mut(&(item.path.as_str(), item.hash)) else {
                    continue;
                };
                if *count == 0 {
                    continue;
                }
                *count -= 1;
                remove.extend(
                    binds
                        .iter()
                        .filter(|name| !new_names.contains(name.as_str()))
                        .cloned(),
                );
                unregister.extend(
                    item.registrations
                        .iter()
                        .filter(|registration| !run_registrations.contains(registration))
                        .cloned(),
                );
            }
        }
    }

    let old_files: FxHashMap<&str, u64> = old
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.hash))
        .collect();
    let sources = index
        .files
        .iter()
        .filter(|file| full || old_files.get(file.path.as_str()) != Some(&file.hash))
        .map(|file| SourceText {
            path: file.path.clone(),
            text: text_of(&file.path).to_owned(),
        })
        .collect();
    let dropped_sources = old
        .files
        .iter()
        .filter(|file| !index.files.iter().any(|new| new.path == file.path))
        .map(|file| file.path.clone())
        .collect();

    let declared = &index.namespace.s3_methods;
    let old_declared = &old.namespace.s3_methods;
    let s3_changed = full || declared != old_declared;
    let s3_register = declared
        .iter()
        .filter(|method| {
            full || !old_declared.contains(method)
                || changed.contains(&method.function)
                || bind
                    .iter()
                    .any(|code: &Code| code.name.as_deref() == Some(method.function.as_str()))
        })
        .cloned()
        .collect();
    let s3_unregister = old_declared
        .iter()
        .filter(|method| !declared.contains(method))
        .cloned()
        .collect();

    let previous_sysdata = previous.and_then(|previous| previous.sysdata);
    let sysdata = match (previous_sysdata, next.sysdata) {
        (_, Some(_)) if full => SysdataChange::Load,
        (old, Some(new)) if old != Some(new) => SysdataChange::Load,
        (Some(_), None) => SysdataChange::Remove,
        _ => SysdataChange::Keep,
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
        s3_register,
        s3_unregister,
        s3_declared: s3_changed.then(|| declared.clone()),
        dynamic_libraries: (full || compiled_changed)
            .then(|| index.namespace.dynamic_libraries.clone())
            .filter(|libraries| !libraries.is_empty() || !full),
        namespace: index.namespace.clone(),
        warnings,
    }
}

impl Plan {
    /// Nothing to do but resync exports, which costs nothing.
    pub fn is_noop(&self) -> bool {
        !self.full
            && self.imports.is_none()
            && self.remove.is_empty()
            && self.unregister.is_empty()
            && self.bind.is_empty()
            && self.run.is_empty()
            && self.sysdata == SysdataChange::Keep
            && self.s3_register.is_empty()
            && self.s3_unregister.is_empty()
            && self.dynamic_libraries.is_none()
    }
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

/// Names, sizes and modification times, which change whenever a rebuild is due without
/// reading every object file.
fn compiled_fingerprint(directory: &Path) -> Result<Option<u64>, LoadError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(LoadError::Io(directory.to_path_buf(), error)),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| LoadError::Io(directory.to_path_buf(), error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Build products change on every build; only sources decide whether one is due.
        if name.ends_with(".o")
            || name.ends_with(".so")
            || name.ends_with(".dll")
            || name.ends_with(".dylib")
        {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| LoadError::Io(entry.path(), error))?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        records.push(format!("{name}\0{}\0{modified}", metadata.len()));
    }
    records.sort();
    Ok(Some(fnv1a(records.join("\n").as_bytes())))
}
