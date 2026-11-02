//! The installed packages behind names that neither the project nor a stub declares. The
//! analysis only says which packages could define a name ([`PackageTarget`]); this reads them
//! from the R library, lazily and once per package, so a session that never navigates into a
//! package never touches the library.

use {
    ide::{Formal, PackageTarget, function_formals},
    rdb::{PackageSource, find_package, library_paths, package_source, topic_documentation},
    std::{
        collections::HashMap,
        fs,
        ops::Range,
        path::{Path, PathBuf},
        sync::Arc,
    },
};

/// Bytes of a generated definition read to find its parameter list.
const FORMALS_WINDOW: usize = 4096;

pub struct InstalledPackages {
    library: Vec<PathBuf>,
    directories: HashMap<String, Option<PathBuf>>,
    sources: HashMap<String, Option<Arc<GeneratedSource>>>,
    /// Where generated sources are written so an editor can open them; `None` keeps them in
    /// memory only, which is all the coverage probe needs.
    cache: Option<PathBuf>,
}

/// An installed package's namespace rendered as R source.
pub struct GeneratedSource {
    pub package: String,
    pub text: String,
    /// The file holding `text`, when it could be written.
    pub path: Option<PathBuf>,
    definitions: HashMap<String, Range<usize>>,
}

impl InstalledPackages {
    pub fn new(project_root: &Path, cache: Option<PathBuf>) -> Self {
        InstalledPackages {
            library: library_paths(project_root),
            directories: HashMap::new(),
            sources: HashMap::new(),
            cache,
        }
    }

    /// The first candidate package whose namespace binds the name, and the name's range in that
    /// package's generated source.
    pub fn definition(
        &mut self,
        target: &PackageTarget,
    ) -> Option<(Arc<GeneratedSource>, Range<usize>)> {
        target.packages.iter().find_map(|package| {
            let source = self.source(package)?;
            let range = source.definitions.get(&target.name)?.clone();
            Some((source, range))
        })
    }

    /// The parameters of the function the name is bound to, read from its generated source.
    pub fn formals(&mut self, target: &PackageTarget) -> Option<Vec<Formal>> {
        let (source, range) = self.definition(target)?;
        // The definition's `function(...)` header comes first, so a bounded window parses it
        // without reading a long body; a body cut mid-expression only adds parse errors after it.
        let mut end = (range.start + FORMALS_WINDOW).min(source.text.len());
        while !source.text.is_char_boundary(end) {
            end -= 1;
        }
        function_formals(source.text.get(range.start..end)?)
    }

    /// The help page of the package that defines the name, as Markdown, with the package named.
    pub fn documentation(&mut self, target: &PackageTarget) -> Option<String> {
        let (source, _) = self.definition(target)?;
        let directory = self.directory(&source.package)?;
        let page = topic_documentation(&directory, &target.name);
        let origin = format!("From the installed `{}` package.", source.package);
        Some(match page {
            Some(page) => format!("{page}\n\n{origin}"),
            None => origin,
        })
    }

    /// The directory under the cache that generated sources go to.
    pub fn is_generated(&self, path: &Path) -> bool {
        self.cache
            .as_deref()
            .is_some_and(|cache| path.starts_with(cache))
    }

    fn directory(&mut self, package: &str) -> Option<PathBuf> {
        if let Some(found) = self.directories.get(package) {
            return found.clone();
        }
        let found = find_package(&self.library, package);
        self.directories.insert(package.to_owned(), found.clone());
        found
    }

    fn source(&mut self, package: &str) -> Option<Arc<GeneratedSource>> {
        if let Some(loaded) = self.sources.get(package) {
            return loaded.clone();
        }
        let loaded = self.load(package).map(Arc::new);
        self.sources.insert(package.to_owned(), loaded.clone());
        loaded
    }

    fn load(&mut self, package: &str) -> Option<GeneratedSource> {
        let directory = self.directory(package)?;
        let PackageSource { text, definitions } = match package_source(&directory) {
            Ok(source) => source,
            Err(error) => {
                tracing::warn!("cannot read installed package {package}: {error}");
                return None;
            }
        };
        let path = self
            .cache
            .as_ref()
            .and_then(|cache| write_source(cache, package, &directory, &text));
        Some(GeneratedSource {
            package: package.to_owned(),
            text,
            path,
            definitions: definitions.into_iter().collect(),
        })
    }
}

/// Keyed by the package's installed version, so an upgrade regenerates the file instead of
/// showing stale code.
fn write_source(cache: &Path, package: &str, directory: &Path, text: &str) -> Option<PathBuf> {
    let version = fs::read_to_string(directory.join("DESCRIPTION"))
        .ok()
        .and_then(|description| {
            description.lines().find_map(|line| {
                line.strip_prefix("Version:")
                    .map(|version| version.trim().to_owned())
            })
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let folder = cache.join(package).join(version);
    let path = folder.join(format!("{package}.R"));
    if fs::read_to_string(&path).ok().as_deref() == Some(text) {
        return Some(path);
    }
    if let Err(error) = fs::create_dir_all(&folder).and_then(|()| fs::write(&path, text)) {
        tracing::warn!(
            "installed package source unavailable: {}: {error}",
            path.display()
        );
        return None;
    }
    Some(path)
}

/// Where the server writes generated package sources.
pub fn source_cache() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("ry").join("packages"))
}
