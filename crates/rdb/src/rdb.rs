//! Reads installed R packages for editor navigation into packages that have no ry type stubs:
//! where the library directories are, the R code of a package as browsable source, and the
//! help page behind a topic.
//!
//! Installed packages keep their R objects in a lazy-load database: `R/<package>.rdx` is an
//! index from each name to a compressed record in `R/<package>.rdb`, and each record is one
//! object in R's serialization format. Help pages are stored the same way under `help/`. This
//! crate reads both without an R runtime, so the analysis side of the workspace stays free of
//! one.

mod deparse;
mod rd;
mod unserialize;

pub use {
    deparse::{deparse, format_double, is_syntactic_name, quote_string},
    unserialize::{
        Argument, Attributes, Closure, Environment, Value, Vector, decompress_file,
        decompress_record, is_na_real, unserialize,
    },
};
use {
    std::{
        env, fs,
        io::{self, Read, Seek, SeekFrom},
        ops::Range,
        path::{Path, PathBuf},
    },
    unserialize::invalid,
};

/// Browsable R source for an installed package's namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageSource {
    pub text: String,
    /// Each top-level name and the byte range of its name token in `text`, sorted by name.
    pub definitions: Vec<(String, Range<usize>)>,
}

/// The library directories to search for installed packages, in priority order and without
/// duplicates: the project's renv library, the `R_LIBS`, `R_LIBS_USER` and `R_LIBS_SITE`
/// variables, the default user library, a `.corpus/library` above `project_root` (the
/// repository's own package corpus), R's own library under `R_HOME`, and the usual install
/// locations.
pub fn library_paths(project_root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    // renv puts the project library first on `.libPaths()`, so it shadows every other one.
    for outer in sorted_subdirectories(&project_root.join("renv/library")) {
        for inner in sorted_subdirectories(&outer).into_iter().rev() {
            if holds_packages(&inner) {
                candidates.push(inner);
            }
        }
    }
    let separator = if cfg!(windows) { ';' } else { ':' };
    for variable in ["R_LIBS", "R_LIBS_USER", "R_LIBS_SITE"] {
        let Some(value) = env::var_os(variable) else {
            continue;
        };
        for entry in value.to_string_lossy().split(separator) {
            let entry = entry.trim();
            // `%v`-style patterns are expanded by R's startup code, not by the shell.
            if !entry.is_empty() && !entry.contains('%') {
                candidates.push(expand_home(entry));
            }
        }
    }
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        // R's default `R_LIBS_USER`: `~/R/<platform>-library/<version>` on Linux and
        // `~/Library/R/<arch>/<version>/library` on macOS, newest version first.
        for platform in sorted_subdirectories(&home.join("R"))
            .into_iter()
            .filter(|platform| platform.to_string_lossy().ends_with("-library"))
        {
            candidates.extend(sorted_subdirectories(&platform).into_iter().rev());
        }
        for arch in sorted_subdirectories(&home.join("Library/R")) {
            for version in sorted_subdirectories(&arch).into_iter().rev() {
                candidates.push(version.join("library"));
            }
        }
    }
    if let Some(corpus) = project_root
        .ancestors()
        .map(|ancestor| ancestor.join(".corpus/library"))
        .find(|corpus| corpus.is_dir())
    {
        candidates.push(corpus);
    }
    if let Some(r_home) = env::var_os("R_HOME") {
        candidates.push(PathBuf::from(r_home).join("library"));
    }
    candidates.extend(
        [
            "/usr/local/lib/R/site-library",
            "/usr/lib/R/site-library",
            "/usr/lib/R/library",
            "/usr/local/lib/R/library",
            "/usr/lib64/R/library",
            "/opt/homebrew/lib/R/library",
            "/Library/Frameworks/R.framework/Resources/library",
        ]
        .map(PathBuf::from),
    );

    let mut paths: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if !candidate.is_dir() {
            continue;
        }
        let identity = fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
        if !seen.contains(&identity) {
            seen.push(identity);
            paths.push(candidate);
        }
    }
    paths
}

/// The directory of the first installed `package` in `paths`.
pub fn find_package(paths: &[PathBuf], package: &str) -> Option<PathBuf> {
    if package.is_empty() || package.contains(['/', '\\']) || package.starts_with('.') {
        return None;
    }
    paths
        .iter()
        .map(|path| path.join(package))
        .find(|directory| directory.join("DESCRIPTION").is_file())
}

/// The namespace of the package installed at `package_dir` as R source: one
/// `name <- value` per object, sorted by name. A function shows its original source when the
/// package was installed with source references kept, and deparsed code otherwise; a large or
/// unprintable value shows as `NULL` with a comment describing it.
pub fn package_source(package_dir: &Path) -> io::Result<PackageSource> {
    let description = Description::read(package_dir)?;
    let name = &description.package;
    let index = read_index(&package_dir.join("R").join(format!("{name}.rdx")))?;
    let data = fs::read(package_dir.join("R").join(format!("{name}.rdb")))?;

    let mut variables: Vec<&(String, Record)> = index
        .variables
        .iter()
        .filter(|(name, _)| !name.starts_with(".__"))
        .collect();
    variables.sort_by(|left, right| left.0.cmp(&right.0));

    let mut text = format!(
        "# R code of the installed package {name}{}, generated from its lazy-load database.\n\
         # Read-only: changes here have no effect.\n",
        description
            .version
            .as_deref()
            .map(|version| format!(" {version}"))
            .unwrap_or_default(),
    );
    let mut definitions = Vec::with_capacity(variables.len());
    for (variable, record) in variables {
        text.push('\n');
        let start = text.len();
        if is_syntactic_name(variable) {
            text.push_str(variable);
        } else {
            text.push_str(&deparse(&Value::Symbol(variable.as_str().into())));
        }
        definitions.push((variable.clone(), start..text.len()));
        text.push_str(" <- ");
        match fetch(&data, record, index.compression) {
            Ok(value) => text.push_str(&definition_source(&value)),
            Err(failure) => {
                text.push_str("NULL # <could not be read: ");
                text.push_str(&failure.to_string().replace('\n', " "));
                text.push('>');
            }
        }
        text.push('\n');
    }

    let primitives = primitive_signatures(&index, &data);
    if !primitives.is_empty() {
        text.push_str("\n# Primitive functions have no R code, so these show only the arguments args() reports.\n");
    }
    for (primitive, formals) in primitives {
        if definitions.iter().any(|(name, _)| *name == primitive) {
            continue;
        }
        text.push('\n');
        let start = text.len();
        text.push_str(&deparse(&Value::Symbol(primitive.as_str().into())));
        definitions.push((primitive.clone(), start..text.len()));
        let body = Value::Language(
            Box::new(Value::Symbol(".Primitive".into())),
            vec![Argument {
                name: None,
                value: Value::Character(Vector {
                    elements: vec![Some(primitive)],
                    attributes: Vec::new(),
                }),
            }],
        );
        text.push_str(" <- ");
        text.push_str(&deparse(&Value::Closure(Box::new(Closure {
            formals,
            body,
            attributes: Vec::new(),
        }))));
        text.push('\n');
    }
    definitions.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(PackageSource { text, definitions })
}

/// The argument lists of base's primitives, sorted by name. Base records them as closures with
/// empty bodies in the `.ArgsEnv` and `.GenericArgsEnv` environments (what `args(sum)`
/// reads), and the lazy-load database stores each environment as a record of its own: a list
/// whose `bindings` element holds the closures.
fn primitive_signatures(index: &Index, data: &[u8]) -> Vec<(String, Vec<Argument>)> {
    let mut signatures = Vec::new();
    for environment in [".ArgsEnv", ".GenericArgsEnv"] {
        let Some((_, record)) = index.variables.iter().find(|(name, _)| name == environment) else {
            continue;
        };
        let Ok(Value::Environment(found)) = fetch(data, record, index.compression) else {
            continue;
        };
        let Environment::Persistent(keys) = found.as_ref() else {
            continue;
        };
        let Some((_, record)) = keys
            .first()
            .and_then(|key| index.references.iter().find(|(name, _)| name == key))
        else {
            continue;
        };
        let Ok(stored) = fetch(data, record, index.compression) else {
            continue;
        };
        let Value::List(fields) = &stored else {
            continue;
        };
        let Some(bindings) = stored
            .names()
            .and_then(|names| {
                names
                    .iter()
                    .position(|name| name.as_deref() == Some("bindings"))
            })
            .and_then(|position| fields.elements.get(position))
        else {
            continue;
        };
        let Value::List(closures) = bindings else {
            continue;
        };
        for (name, closure) in bindings
            .names()
            .unwrap_or(&[])
            .iter()
            .zip(&closures.elements)
        {
            if let (Some(name), Value::Closure(closure)) = (name, closure) {
                signatures.push((name.clone(), closure.formals.clone()));
            }
        }
    }
    signatures.sort_by(|left, right| left.0.cmp(&right.0));
    signatures.dedup_by(|left, right| left.0 == right.0);
    signatures
}

/// The help topic `topic` of the package at `package_dir` as Markdown, found through the
/// package's alias index.
pub fn topic_documentation(package_dir: &Path, topic: &str) -> Option<String> {
    let description = Description::read(package_dir).ok()?;
    let help = package_dir.join("help");
    let aliases =
        unserialize(&decompress_file(&fs::read(help.join("aliases.rds")).ok()?).ok()?).ok()?;
    let Value::Character(pages) = &aliases else {
        return None;
    };
    let position = aliases
        .names()?
        .iter()
        .position(|alias| alias.as_deref() == Some(topic))?;
    let page = pages.elements.get(position)?.as_deref()?;

    let index = read_index(&help.join(format!("{}.rdx", description.package))).ok()?;
    let (_, record) = index.variables.iter().find(|(name, _)| name == page)?;
    let mut file = fs::File::open(help.join(format!("{}.rdb", description.package))).ok()?;
    file.seek(SeekFrom::Start(record.offset)).ok()?;
    let mut bytes = vec![0; usize::try_from(record.length).ok()?];
    file.read_exact(&mut bytes).ok()?;
    let rd = fetch(
        &bytes,
        &Record {
            offset: 0,
            length: record.length,
        },
        index.compression,
    )
    .ok()?;
    let markdown = rd::render(&rd);
    (!markdown.is_empty()).then_some(markdown)
}

/// Whether the namespace of the package at `package_dir` has an object called `name`,
/// exported or not.
pub fn defines(package_dir: &Path, name: &str) -> bool {
    namespace_names(package_dir).is_ok_and(|names| names.iter().any(|variable| variable == name))
}

/// Every object in the namespace of the package at `package_dir`, exported or not, in the
/// lazy-load index's order. Reads only the index, not the objects.
pub fn namespace_names(package_dir: &Path) -> io::Result<Vec<String>> {
    let description = Description::read(package_dir)?;
    let path = package_dir
        .join("R")
        .join(format!("{}.rdx", description.package));
    Ok(read_index(&path)?
        .variables
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// A lazy-load database's index: where each object's record is, and how records are
/// compressed.
struct Index {
    variables: Vec<(String, Record)>,
    /// Environments stored as records of their own, by the key a `Persistent` value names.
    references: Vec<(String, Record)>,
    /// `lazyLoadDBfetch`'s `compressed`: 0 none, 1 zlib, 2 and 3 a per-record method byte.
    compression: i32,
}

struct Record {
    offset: u64,
    length: u64,
}

/// The fields of a package's `DESCRIPTION` file that name its files and its header.
struct Description {
    package: String,
    version: Option<String>,
}

impl Description {
    fn read(package_dir: &Path) -> io::Result<Description> {
        let text = fs::read_to_string(package_dir.join("DESCRIPTION"))?;
        let field = |name: &str| {
            text.lines().find_map(|line| {
                let value = line.strip_prefix(name)?.strip_prefix(':')?;
                Some(value.trim().to_string())
            })
        };
        let package = field("Package")
            .or_else(|| {
                package_dir
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .ok_or_else(|| invalid("DESCRIPTION has no Package field"))?;
        Ok(Description {
            package,
            version: field("Version"),
        })
    }
}

fn read_index(path: &Path) -> io::Result<Index> {
    let index = unserialize(&decompress_file(&fs::read(path)?)?)?;
    let Value::List(fields) = &index else {
        return Err(invalid("lazy-load index is not a list"));
    };
    let names = index
        .names()
        .ok_or_else(|| invalid("lazy-load index has no names"))?;
    let field = |wanted: &str| {
        names
            .iter()
            .position(|name| name.as_deref() == Some(wanted))
            .and_then(|position| fields.elements.get(position))
    };
    let compression = match field("compressed") {
        Some(Value::Logical(vector)) => i32::from(vector.elements.first() == Some(&Some(true))),
        Some(Value::Integer(vector)) => vector.elements.first().copied().flatten().unwrap_or(1),
        Some(Value::Double(vector)) => vector.elements.first().map_or(1, |&value| value as i32),
        _ => 1,
    };
    let Some(variables) = field("variables") else {
        return Err(invalid("lazy-load index has no variables"));
    };
    Ok(Index {
        variables: records(variables)?,
        references: field("references").map_or(Ok(Vec::new()), records)?,
        compression,
    })
}

fn records(table: &Value) -> io::Result<Vec<(String, Record)>> {
    let Value::List(entries) = table else {
        return Err(invalid("lazy-load index table is not a list"));
    };
    let names = table.names().unwrap_or(&[]);
    let mut records = Vec::with_capacity(entries.elements.len());
    for (name, entry) in names.iter().zip(&entries.elements) {
        let Some(name) = name else { continue };
        let bounds: Option<[f64; 2]> = match entry {
            Value::Integer(vector) => match vector.elements.as_slice() {
                [Some(offset), Some(length)] => Some([f64::from(*offset), f64::from(*length)]),
                _ => None,
            },
            Value::Double(vector) => vector.elements.as_slice().try_into().ok(),
            _ => None,
        };
        let Some([offset, length]) = bounds.filter(|bounds| bounds.iter().all(|&b| b >= 0.0))
        else {
            return Err(invalid(format!(
                "lazy-load index entry for {name} is malformed"
            )));
        };
        records.push((
            name.clone(),
            Record {
                offset: offset as u64,
                length: length as u64,
            },
        ));
    }
    Ok(records)
}

fn fetch(data: &[u8], record: &Record, compression: i32) -> io::Result<Value> {
    let bytes = usize::try_from(record.offset)
        .ok()
        .zip(usize::try_from(record.length).ok())
        .and_then(|(offset, length)| data.get(offset..offset.checked_add(length)?))
        .ok_or_else(|| invalid("lazy-load record lies outside the database"))?;
    let value = unserialize(&decompress_record(bytes, compression)?)?;
    Ok(match value {
        Value::Promise(inner) => *inner,
        value => value,
    })
}

/// The right-hand side of one definition in a package's generated source.
fn definition_source(value: &Value) -> String {
    match value {
        Value::Closure(_) => original_source(value).unwrap_or_else(|| deparse(value)),
        Value::Symbol(_) | Value::Language(..) => format!("quote({})", deparse(value)),
        Value::Missing => "quote(expr = )".to_string(),
        value => match placeholder(value, 0) {
            Some(description) => format!("NULL # <{description}>"),
            None => deparse(value),
        },
    }
}

/// The text a function's `srcref` attribute points at, when the source file's lines were
/// kept with it.
fn original_source(function: &Value) -> Option<String> {
    let srcref = function.attribute("srcref")?;
    let Value::Integer(positions) = srcref else {
        return None;
    };
    let position = |index: usize| -> Option<usize> {
        usize::try_from(positions.elements.get(index).copied().flatten()?).ok()
    };
    let (first_line, first_byte, last_line, last_byte) =
        (position(0)?, position(1)?, position(2)?, position(3)?);
    let Some(Value::Environment(source_file)) = srcref.attribute("srcfile") else {
        return None;
    };
    let lines = source_lines(source_file)?;
    let lines: Vec<&str> = lines.iter().flat_map(|line| line.split('\n')).collect();
    let selected = lines.get(first_line.checked_sub(1)?..last_line)?;
    let mut text = String::new();
    for (offset, line) in selected.iter().enumerate() {
        let start = if offset == 0 {
            first_byte.checked_sub(1)?
        } else {
            0
        };
        let end = if offset + 1 == selected.len() {
            last_byte
        } else {
            line.len()
        };
        text.push_str(line.get(start.min(end)..end)?);
        if offset + 1 < selected.len() {
            text.push('\n');
        }
    }
    text.starts_with("function").then_some(text)
}

/// The lines a `srcfile` environment holds, either itself or through the original file a
/// `srcfilealias` points at.
fn source_lines(source_file: &Environment) -> Option<Vec<String>> {
    let Environment::Frame(bindings) = source_file else {
        return None;
    };
    let binding = |name: &str| {
        bindings
            .iter()
            .find(|(binding, _)| &**binding == name)
            .map(|(_, value)| value)
    };
    match (binding("lines"), binding("original")) {
        (Some(Value::Character(lines)), _) => {
            Some(lines.elements.iter().flatten().cloned().collect())
        }
        (_, Some(Value::Environment(original))) => source_lines(original),
        _ => None,
    }
}

/// Why `value` is shown as a placeholder instead of its deparsed form, if it is: it is too
/// large to read at a glance, or it has no source form.
fn placeholder(value: &Value, depth: usize) -> Option<String> {
    const LONGEST_VECTOR: usize = 20;
    const LONGEST_LIST: usize = 10;
    const LONGEST_STRING: usize = 200;
    const DEEPEST: usize = 3;
    let plural = |count: usize| if count == 1 { "" } else { "s" };
    let attributes = value
        .attributes()
        .iter()
        .find_map(|(_, attribute)| placeholder(attribute, depth + 1));
    let own = match value {
        Value::Environment(_) => Some("environment".to_string()),
        Value::Opaque(description) => Some(description.to_string()),
        Value::Closure(_) if depth > 0 => Some("function".to_string()),
        Value::Logical(vector) if vector.elements.len() > LONGEST_VECTOR => Some(format!(
            "logical vector of {} elements",
            vector.elements.len()
        )),
        Value::Integer(vector) if vector.elements.len() > LONGEST_VECTOR => Some(format!(
            "integer vector of {} elements",
            vector.elements.len()
        )),
        Value::Double(vector) if vector.elements.len() > LONGEST_VECTOR => Some(format!(
            "numeric vector of {} elements",
            vector.elements.len()
        )),
        Value::Complex(vector) if vector.elements.len() > LONGEST_VECTOR => Some(format!(
            "complex vector of {} elements",
            vector.elements.len()
        )),
        Value::Raw(vector) if vector.elements.len() > LONGEST_VECTOR => {
            Some(format!("raw vector of {} bytes", vector.elements.len()))
        }
        Value::Character(vector)
            if vector.elements.len() > LONGEST_VECTOR
                || vector
                    .elements
                    .iter()
                    .flatten()
                    .any(|string| string.len() > LONGEST_STRING) =>
        {
            let count = vector.elements.len();
            Some(format!(
                "character vector of {count} element{}",
                plural(count)
            ))
        }
        Value::List(vector) | Value::Expression(vector) => {
            let count = vector.elements.len();
            let too_large = count > LONGEST_LIST
                || depth >= DEEPEST
                || vector
                    .elements
                    .iter()
                    .any(|element| placeholder(element, depth + 1).is_some());
            too_large.then(|| format!("list of {count} element{}", plural(count)))
        }
        Value::Language(..) | Value::Symbol(_) | Value::Pairlist(_) if depth > 0 => {
            Some("language object".to_string())
        }
        _ => None,
    };
    own.or(attributes)
}

fn expand_home(entry: &str) -> PathBuf {
    match (entry.strip_prefix("~/"), env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(entry),
    }
}

/// The subdirectories of `directory`, sorted by name so the search order is reproducible.
fn sorted_subdirectories(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut directories: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    directories.sort();
    directories
}

fn holds_packages(directory: &Path) -> bool {
    fs::read_dir(directory).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.path().join("DESCRIPTION").is_file())
    })
}

#[cfg(test)]
mod tests {
    use {super::*, std::sync::Arc};

    fn strings(values: &[&str]) -> Value {
        Value::Character(Vector::new(
            values.iter().map(|value| Some(value.to_string())).collect(),
        ))
    }

    #[test]
    fn functions_with_kept_source_show_it() {
        let source_file = Environment::Frame(vec![(
            Arc::from("lines"),
            strings(&["x <- 1", "foo <- function(a)", "  a + 1 # add one", ""]),
        )]);
        let srcref = Value::Integer(Vector {
            elements: [2, 8, 3, 17, 8, 17, 2, 3].map(Some).to_vec(),
            attributes: vec![(
                Arc::from("srcfile"),
                Value::Environment(Arc::new(source_file)),
            )],
        });
        let function = Value::Closure(Box::new(Closure {
            formals: vec![Argument {
                name: Some(Arc::from("a")),
                value: Value::Missing,
            }],
            body: Value::Null,
            attributes: vec![(Arc::from("srcref"), srcref)],
        }));
        assert_eq!(
            definition_source(&function),
            "function(a)\n  a + 1 # add one"
        );
    }

    #[test]
    fn large_and_unprintable_values_are_placeholders() {
        let letters: Vec<String> = ('a'..='z').map(String::from).collect();
        let letters: Vec<&str> = letters.iter().map(String::as_str).collect();
        assert_eq!(
            definition_source(&strings(&letters)),
            "NULL # <character vector of 26 elements>"
        );
        assert_eq!(
            definition_source(&Value::Environment(Arc::new(Environment::Global))),
            "NULL # <environment>"
        );
        assert_eq!(definition_source(&strings(&["a", "b"])), "c(\"a\", \"b\")");
        assert_eq!(
            definition_source(&Value::Symbol(Arc::from("x"))),
            "quote(x)"
        );
    }
}
