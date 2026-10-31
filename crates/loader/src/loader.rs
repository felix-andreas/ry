//! The package-development loader: reads a package's sources into the analysis, derives its
//! load index, and diffs that index against what is loaded to produce a [`Plan`]. A plan says
//! what to bind, run, and remove, and nothing about how; a backend executes it. The GNU R
//! backend ([`gnur`]) hands it to R code running in the session, and ry's own runtime will take
//! the same plan.

pub mod gnur;

use rustc_hash::{FxHashMap, FxHashSet};
use salsa::{Durability, Setter};
use semantics::load_index::{
    IndexedFile, IndexedItem, PackageFile, PackageIndex, PackageSources, Registration, file_items,
    file_load_index, fnv1a, package_index,
};
use semantics::metadata::{
    DynamicLibraryDirective, NamespaceImportDirective, NamespaceSpec, S3MethodDirective,
    parse_description_collate,
};
use semantics::{ANALYSIS_STACK_SIZE, DocumentKind, RootDatabase, SourceFile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::SystemTime;

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
    /// Size and modification time per file as last read.
    stamps: FxHashMap<String, (u64, Option<SystemTime>)>,
    /// What the session has loaded, as confirmed by [`Loader::commit`].
    committed: Option<LoadState>,
    /// The state a plan was made for, until the backend confirms it applied.
    pending: Option<LoadState>,
}

/// What is loaded for one package: the load index plus the inputs the analysis does not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadState {
    pub root: PathBuf,
    pub index: Arc<PackageIndex>,
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
        let (position, changed) = self.refresh(&root)?;
        self.warm(&changed);
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
        let db = &self.db;
        let plan = diff(previous, &next, &|path| {
            package
                .files
                .get(path)
                .map_or("", |&file| file_load_index(db, file).text.as_str())
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
        let (position, _) = self.refresh(&state.root)?;
        self.packages[position].committed = Some(state);
        Ok(())
    }

    /// The roots of every package this loader has planned.
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        self.packages.iter().map(|package| package.root.as_path())
    }

    /// Derives the changed files' per-item facts across cores, so `package_index` only joins
    /// memoized results. Salsa storage handles share memos, so the workers fill one cache.
    fn warm(&self, files: &[SourceFile]) {
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
                thread::Builder::new()
                    .name("ry-loader-index".to_owned())
                    .stack_size(ANALYSIS_STACK_SIZE)
                    .spawn_scoped(scope, move || {
                        for &file in files.iter().skip(worker).step_by(workers) {
                            file_items(&db, file);
                        }
                    })
                    .expect("a loader worker thread should spawn");
            }
        });
    }

    /// Syncs the analysis inputs with the disk, setting only what changed, and returns the
    /// package's position. A file whose size and modification time are unchanged is not read.
    ///
    /// Files load at high durability and drop to low on their first edit. Salsa skips
    /// verifying a memo that depends only on inputs of a durability nothing changed at, so a
    /// reload re-derives the package without revisiting the files nobody touched.
    fn refresh(&mut self, root: &Path) -> Result<(usize, Vec<SourceFile>), LoadError> {
        let description = read(&root.join("DESCRIPTION"))?;
        let namespace = match fs::read_to_string(root.join("NAMESPACE")) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(LoadError::Io(root.join("NAMESPACE"), error)),
        };
        let paths = collated_r_files(root, &parse_description_collate(&description))?;
        let known = self
            .packages
            .iter()
            .find(|package| package.root == root)
            .map(|package| &package.stamps);
        let mut texts = Vec::with_capacity(paths.len());
        let mut stamps = FxHashMap::default();
        for path in &paths {
            let absolute = root.join(path);
            let stamp = fs::metadata(&absolute)
                .map(|metadata| (metadata.len(), metadata.modified().ok()))
                .map_err(|error| LoadError::Io(absolute.clone(), error))?;
            let unchanged =
                stamp.1.is_some() && known.is_some_and(|known| known.get(path) == Some(&stamp));
            texts.push(if unchanged {
                None
            } else {
                Some(read(&absolute)?.replace("\r\n", "\n").replace('\r', "\n"))
            });
            stamps.insert(path.clone(), stamp);
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
                    stamps: FxHashMap::default(),
                    committed: None,
                    pending: None,
                });
                self.packages.len() - 1
            }
        };
        let db = &mut self.db;
        let package = &mut self.packages[position];
        let mut files = Vec::with_capacity(paths.len());
        let mut changed = Vec::new();
        for (path, text) in paths.into_iter().zip(texts) {
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
                        None => read(&root.join(&path))?,
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
        package.stamps = stamps;
        package
            .files
            .retain(|path, _| files.iter().any(|file| &file.path == path));
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
        if sources.description(db) != &description {
            sources
                .set_description(db)
                .with_durability(Durability::LOW)
                .to(description);
        }
        Ok((position, changed))
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
        && previous.compiled == next.compiled
    {
        return Plan::unchanged(next);
    }
    let index = &*next.index;
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
    let dirty_items = |files: &[&'_ IndexedFile]| -> Vec<(String, IndexedItem)> {
        files
            .iter()
            .flat_map(|file| {
                file.items
                    .items
                    .iter()
                    .map(|item| (file.path.clone(), item.clone()))
            })
            .collect()
    };
    let old_items = dirty_items(&old_dirty);
    let new_dirty_files: Vec<&IndexedFile> = index
        .files
        .iter()
        .zip(&new_dirty)
        .filter(|(_, dirty)| **dirty)
        .map(|(file, _)| file)
        .collect();
    let new_items = dirty_items(&new_dirty_files);

    // Names whose laziness flipped, because a second writer appeared or went away. Only a
    // dirty item can change a writer count, so only their names are candidates.
    let flipped: FxHashSet<&str> = old_items
        .iter()
        .chain(&new_items)
        .flat_map(|(_, item)| item.binds.iter().map(String::as_str))
        .filter(|name| old.is_lazy(name) != index.is_lazy(name))
        .collect();

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
        let span = item.function.as_ref().map_or(item.span, |(_, span)| *span);
        Code {
            name: Some(name.to_owned()),
            path: path.to_owned(),
            line: span.line,
            column: span.column,
            start: span.char_start,
            end: span.char_end,
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
        registers_s4: registers_s4(item),
    };

    // Lazy definitions are keyed by name, which only one item writes.
    let old_lazy: FxHashMap<&str, (&str, &IndexedItem)> = old_items
        .iter()
        .filter_map(|(path, item)| Some((old.lazy_name(item)?, (path.as_str(), item))))
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
                    item.function
                        .as_ref()
                        .map(|(_, span)| (span.line, span.column))
                };
                *old_path != path.as_str() || position(previous) != position(item)
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

    // Eager items in dirty files are matched by content within their file, so inserting a
    // statement does not rerun the statements after it.
    let mut unmatched_old: FxHashMap<(&str, u64), usize> = FxHashMap::default();
    for (path, item) in &old_items {
        if old.lazy_name(item).is_none() {
            *unmatched_old.entry((path.as_str(), item.hash)).or_default() += 1;
        }
    }
    let mut run = Vec::new();
    let mut run_registrations: BTreeSet<&Registration> = BTreeSet::new();
    for (file, dirty) in index.files.iter().zip(&new_dirty) {
        for item in &file.items.items {
            if index.lazy_name(item).is_some() {
                continue;
            }
            let stale = !changed.is_empty() && item.reads.iter().any(|name| changed.contains(name));
            let fresh = if *dirty {
                match unmatched_old.get_mut(&(file.path.as_str(), item.hash)) {
                    Some(count) if *count > 0 => {
                        *count -= 1;
                        false
                    }
                    _ => true,
                }
            } else {
                // A definition in a clean file that stopped being lazy now runs in order.
                item.function
                    .as_ref()
                    .is_some_and(|(name, _)| flipped.contains(name.as_str()))
            };
            if full || fresh || stale {
                changed.extend(item.binds.iter().cloned());
                run_registrations.extend(&item.registrations);
                run.push(eager_code(&file.path, item));
            }
        }
    }

    let mut remove = BTreeSet::new();
    let mut unregister = BTreeSet::new();
    for (path, item) in &old_items {
        if old.lazy_name(item).is_some() {
            remove.extend(
                item.binds
                    .iter()
                    .filter(|name| !index.names.contains_key(*name))
                    .cloned(),
            );
            continue;
        }
        let Some(count) = unmatched_old.get_mut(&(path.as_str(), item.hash)) else {
            continue;
        };
        if *count == 0 {
            continue;
        }
        *count -= 1;
        remove.extend(
            item.binds
                .iter()
                .filter(|name| !index.names.contains_key(*name))
                .cloned(),
        );
        unregister.extend(
            item.registrations
                .iter()
                .filter(|registration| !run_registrations.contains(registration))
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
    let s3_register = declared
        .iter()
        .filter(|method| {
            full || !old_declared.contains(method)
                || changed.contains(&method.function)
                || rebound.contains(method.function.as_str())
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
            s3_register: Vec::new(),
            s3_unregister: Vec::new(),
            s3_declared: None,
            dynamic_libraries: None,
            namespace: state.index.namespace.clone(),
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
