//! The package loader's fuzz contracts, shared by the in-tree harness
//! (`tests/test_reload_fuzz.rs`) and the coverage-guided `fuzz/` targets.
//!
//! The reload contract: applying the reload plan to a loaded package gives the same bindings and
//! registrations as loading the edited package from scratch. A model executes both: a lazy
//! binding's value is its definition's text, and a statement stores its text plus the values it
//! read, so a statement that should have rerun shows up as a stale value. A strict file binds its
//! pure constants lazily, and the model forces every constant as soon as its binding lands, so a
//! constant that should have been bound afresh keeps a stale forced value.

use {
    crate::{Loader, PackageInputs, Plan},
    rustc_hash::{FxHashMap, FxHashSet},
    semantics::load_index::{IndexedItem, PackageIndex, Registration},
    std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
    },
    syntax::testing::Entropy,
};

/// Any text as a package's only R file either refuses the load with a syntax error or indexes,
/// and every span the index records lies inside that text.
pub fn check_load_index_input(text: &str) {
    let root = Path::new("/fuzz-bytes");
    let mut loader = Loader::default();
    let inputs = PackageInputs {
        description: "Package: demo\n".to_owned(),
        files: vec![("R/a.R".to_owned(), Some(text.to_owned()))],
        ..PackageInputs::default()
    };
    if loader.plan_inputs(root, inputs, false).is_err() {
        return;
    }
    let index = loader
        .pending(root)
        .expect("a planned load is pending")
        .index
        .clone();
    loader.commit(root);
    let characters = text.chars().count() as u32;
    for (_, item) in index.items() {
        let spans =
            std::iter::once(item.span).chain(item.binding.as_ref().map(|binding| binding.value));
        for span in spans {
            assert!(
                span.char_start <= span.char_end && span.char_end <= characters && span.line >= 1,
                "span {span:?} escapes {text:?}"
            );
        }
    }
}

/// Two loaders that serve every reload check, each case under its own root, so the shipped stubs
/// a strict package needs are parsed once rather than per case.
#[derive(Default)]
pub struct ReloadLoaders {
    incremental: Loader,
    scratch: Loader,
    cases: u64,
}

/// Generates a package and up to four edits of it, then checks the reload contract.
pub fn check_random_reload(loaders: &mut ReloadLoaders, entropy: &mut impl Entropy) {
    let before = random_package(entropy);
    let mut after = before.clone();
    for _ in 0..1 + entropy.below(4) {
        edit(entropy, &mut after);
    }
    check_reload(loaders, &before, &after);
}

/// A package as statements per file, `FILES` files in collation order.
pub type Package = Vec<Vec<String>>;

const FILES: usize = 3;

fn random_statement(entropy: &mut impl Entropy) -> String {
    let number = entropy.below(100);
    match entropy.below(12) {
        0..=2 => format!(
            "{} <- function(x) {}(x) + {number}",
            pick(entropy, "f", 5),
            pick(entropy, "f", 5)
        ),
        3 => {
            let name = pick(entropy, "f", 5);
            format!("{name} <- wrap({name})")
        }
        4 => format!("{} <- {number}", pick(entropy, "c", 3)),
        5 => format!(
            "{} <- {}({})",
            pick(entropy, "v", 4),
            pick(entropy, "f", 5),
            pick(entropy, "c", 3)
        ),
        6 => format!(
            "{} <- list(a = {}, b = function() {})",
            pick(entropy, "v", 4),
            pick(entropy, "c", 3),
            pick(entropy, "v", 4)
        ),
        7 => {
            let name = pick(entropy, "v", 4);
            format!("{name} <- {name} + {}", pick(entropy, "c", 3))
        }
        8 => format!("if (TRUE) {} <- function() {number}", pick(entropy, "f", 5)),
        9 => format!(
            "#: @pure\n{} <- function(x) {}(x) + {number}",
            pick(entropy, "f", 5),
            pick(entropy, "f", 5)
        ),
        10 => "# typing: strict".to_owned(),
        _ => format!(
            "setMethod(\"g{}\", \"C{}\", function(x) {number})",
            entropy.below(2),
            entropy.below(2)
        ),
    }
}

fn pick(entropy: &mut impl Entropy, prefix: &str, count: usize) -> String {
    format!("{prefix}{}", entropy.below(count))
}

pub fn random_package(entropy: &mut impl Entropy) -> Package {
    (0..FILES)
        .map(|_| {
            let header = (entropy.below(2) == 0).then(|| "# typing: strict".to_owned());
            header
                .into_iter()
                .chain((0..entropy.below(6)).map(|_| random_statement(entropy)))
                .collect()
        })
        .collect()
}

pub fn edit(entropy: &mut impl Entropy, package: &mut Package) {
    let file = &mut package[entropy.below(FILES)];
    match entropy.below(5) {
        0 => {
            let at = entropy.below(file.len() + 1);
            file.insert(at, random_statement(entropy));
        }
        1 if !file.is_empty() => {
            let at = entropy.below(file.len());
            file.remove(at);
        }
        2 if !file.is_empty() => {
            let at = entropy.below(file.len());
            file[at] = random_statement(entropy);
        }
        3 if file.len() > 1 => {
            let from = entropy.below(file.len());
            let statement = file.remove(from);
            let to = entropy.below(file.len() + 1);
            file.insert(to, statement);
        }
        _ => {
            let at = entropy.below(file.len() + 1);
            file.insert(at, String::new());
        }
    }
}

fn inputs(package: &Package) -> PackageInputs {
    PackageInputs {
        description: "Package: demo\nVersion: 1.0\n".to_owned(),
        files: package
            .iter()
            .enumerate()
            .map(|(position, statements)| {
                (
                    format!("R/file{position}.R"),
                    Some(
                        statements
                            .iter()
                            .map(|statement| format!("{statement}\n"))
                            .collect(),
                    ),
                )
            })
            .collect(),
        ..PackageInputs::default()
    }
}

/// A namespace as the model sees it.
#[derive(Debug, Default, Clone, PartialEq)]
struct Model {
    bindings: FxHashMap<String, String>,
    registrations: BTreeSet<Registration>,
    /// Each lazy constant's value as it was forced right after its binding landed.
    forced: FxHashMap<String, String>,
}

/// Reloading `before` into `after` gives the same bindings and registrations as loading `after`
/// from scratch, and the incrementally derived index equals a fresh one.
pub fn check_reload(loaders: &mut ReloadLoaders, before: &Package, after: &Package) {
    loaders.cases += 1;
    let root = PathBuf::from(format!("/fuzz/{}", loaders.cases));
    let root = root.as_path();
    let ReloadLoaders {
        incremental: loader,
        scratch,
        ..
    } = loaders;
    loader
        .plan_inputs(root, inputs(before), false)
        .expect("the package plans");
    let old_index = loader.pending(root).expect("pending").index.clone();
    loader.commit(root);
    let old_texts = texts(before);
    let mut reloaded = fresh_load(&old_index, &old_texts);

    let plan = loader
        .plan_inputs(root, inputs(after), false)
        .expect("the edited package plans");
    let new_index = loader.pending(root).expect("pending").index.clone();
    let new_texts = texts(after);
    scratch
        .plan_inputs(root, inputs(after), false)
        .expect("the edited package plans from scratch");
    assert_eq!(
        *new_index,
        *scratch.pending(root).expect("pending").index,
        "the incrementally derived index differs from a fresh one"
    );
    apply(&mut reloaded, &plan, &new_index, &new_texts);
    let fresh = fresh_load(&new_index, &new_texts);

    for name in &plan.remove {
        assert!(
            !new_index.binds(name),
            "the plan removes `{name}`, which the edited package still binds"
        );
    }
    assert_eq!(
        reloaded, fresh,
        "reloading differs from a fresh load\n---- before:\n{before:#?}\n---- after:\n{after:#?}\n---- plan:\n{plan:#?}"
    );
}

fn texts(package: &Package) -> FxHashMap<String, String> {
    inputs(package)
        .files
        .into_iter()
        .map(|(path, text)| (path, text.unwrap_or_default()))
        .collect()
}

fn slice(texts: &FxHashMap<String, String>, path: &str, start: u32, end: u32) -> String {
    texts[path]
        .chars()
        .skip(start as usize)
        .take((end - start) as usize)
        .collect()
}

/// Binds every lazy definition, then runs the statements in collation order, as a load does.
fn fresh_load(index: &PackageIndex, texts: &FxHashMap<String, String>) -> Model {
    let mut model = Model::default();
    for (path, item) in index.items() {
        if let (Some(name), Some(span)) = (
            index.lazy_name(item),
            item.binding.as_ref().map(|binding| binding.value),
        ) {
            model.bindings.insert(
                name.to_owned(),
                format!(
                    "lazy {}",
                    slice(texts, path, span.char_start, span.char_end)
                ),
            );
        }
    }
    let order = eager_order(index);
    for (path, item) in index.items() {
        if index.lazy_name(item).is_none() {
            run(&mut model, index, &order, texts, path, item);
        }
    }
    let constants: Vec<String> = index
        .items()
        .filter_map(|(_, item)| lazy_constant(index, item))
        .map(str::to_owned)
        .collect();
    force(&mut model, index, &order, &constants);
    model
}

fn lazy_constant<'a>(index: &PackageIndex, item: &'a IndexedItem) -> Option<&'a str> {
    let binding = item.binding.as_ref()?;
    (!binding.function && index.lazy_name(item).is_some()).then_some(binding.name.as_str())
}

/// Forces the named constants once the whole load or reload has run, which is when a session
/// first reads them at the latest.
fn force(model: &mut Model, index: &PackageIndex, order: &EagerOrder, names: &[String]) {
    model.forced.retain(|name, _| {
        index
            .items()
            .any(|(_, item)| lazy_constant(index, item) == Some(name.as_str()))
    });
    for name in names {
        let value = deep_value(
            model,
            index,
            order,
            usize::MAX,
            name,
            &mut FxHashSet::default(),
        );
        model.forced.insert(name.clone(), value);
    }
}

/// Each eager item's position, and per name the positions of its eager writers, in
/// collation order.
struct EagerOrder {
    positions: FxHashMap<(String, u32), usize>,
    writers: FxHashMap<String, Vec<usize>>,
}

fn eager_order(index: &PackageIndex) -> EagerOrder {
    let mut order = EagerOrder {
        positions: FxHashMap::default(),
        writers: FxHashMap::default(),
    };
    for (path, item) in index.items() {
        if index.lazy_name(item).is_some() {
            continue;
        }
        let position = order.positions.len();
        order
            .positions
            .insert((path.to_owned(), item.span.char_start), position);
        for name in &item.binds {
            order
                .writers
                .entry(name.clone())
                .or_default()
                .push(position);
        }
    }
    order
}

/// A statement sees a name only once something bound it: a lazy definition, or a statement
/// earlier in collation order. Reading anything else fails in R, whatever the session holds.
fn run(
    model: &mut Model,
    index: &PackageIndex,
    order: &EagerOrder,
    texts: &FxHashMap<String, String>,
    path: &str,
    item: &IndexedItem,
) {
    let position = order.positions[&(path.to_owned(), item.span.char_start)];
    let mut seen = Vec::new();
    for name in &item.reads {
        let value = if visible(index, order, position, name) {
            deep_value(
                model,
                index,
                order,
                position,
                name,
                &mut FxHashSet::default(),
            )
        } else {
            "missing".to_owned()
        };
        seen.push(format!("{name}={value}"));
    }
    let value = format!(
        "ran {} with [{}]",
        slice(texts, path, item.span.char_start, item.span.char_end),
        seen.join(", ")
    );
    for name in &item.binds {
        model.bindings.insert(name.clone(), value.clone());
    }
    model
        .registrations
        .extend(item.registrations.iter().cloned());
}

fn visible(index: &PackageIndex, order: &EagerOrder, position: usize, name: &str) -> bool {
    index.is_lazy(name)
        || order
            .writers
            .get(name)
            .is_some_and(|writers| writers.first().is_some_and(|first| *first < position))
}

/// A name's value, and for a lazy function also everything its body mentions, transitively:
/// calling it during a statement reads all of that.
fn deep_value(
    model: &Model,
    index: &PackageIndex,
    order: &EagerOrder,
    position: usize,
    name: &str,
    seen: &mut FxHashSet<String>,
) -> String {
    if !seen.insert(name.to_owned()) {
        return "cycle".to_owned();
    }
    let value = model
        .bindings
        .get(name)
        .cloned()
        .unwrap_or_else(|| "missing".to_owned());
    if !index.is_lazy(name) {
        return value;
    }
    let mentions: Vec<String> = index
        .items()
        .find(|(_, item)| index.lazy_name(item) == Some(name))
        .map(|(_, item)| item.mentions.clone())
        .unwrap_or_default();
    let inner: Vec<String> = mentions
        .iter()
        .filter(|mention| {
            model.bindings.contains_key(*mention) && visible(index, order, position, mention)
        })
        .map(|mention| {
            let value = deep_value(model, index, order, position, mention, seen);
            format!("{mention}={value}")
        })
        .collect();
    format!("{value} calling [{}]", inner.join(", "))
}

fn apply(model: &mut Model, plan: &Plan, index: &PackageIndex, texts: &FxHashMap<String, String>) {
    for name in &plan.remove {
        model.bindings.remove(name);
    }
    for registration in &plan.unregister {
        model.registrations.remove(registration);
    }
    for code in &plan.bind {
        let name = code.name.clone().expect("a lazy binding has a name");
        model.bindings.insert(
            name,
            format!("lazy {}", slice(texts, &code.path, code.start, code.end)),
        );
    }
    let order = eager_order(index);
    let items: FxHashMap<(&str, u32), &IndexedItem> = index
        .items()
        .map(|(path, item)| ((path, item.span.char_start), item))
        .collect();
    let mut ran = FxHashSet::default();
    for code in &plan.run {
        let item = items[&(code.path.as_str(), code.start)];
        assert!(
            ran.insert((code.path.clone(), code.start)),
            "a plan runs a statement twice"
        );
        run(model, index, &order, texts, &code.path, item);
    }
    let rebound: Vec<String> = plan
        .bind
        .iter()
        .filter_map(|code| code.name.clone())
        .filter(|name| {
            index
                .items()
                .any(|(_, item)| lazy_constant(index, item) == Some(name.as_str()))
        })
        .collect();
    force(model, index, &order, &rebound);
}
