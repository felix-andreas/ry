//! The package loader's fuzz contracts, shared by the in-tree harness
//! (`tests/test_reload_fuzz.rs`) and the coverage-guided `fuzz/` targets.
//!
//! The reload contract: applying the reload plan to a loaded package gives the same bindings and
//! registrations as loading the edited package from scratch. A model executes both: a lazy
//! binding's value is its definition's text, and a statement stores its text plus the values it
//! read, so a statement that should have rerun shows up as a stale value. A strict file binds its
//! pure constants lazily, and the model forces every constant as soon as its binding lands, so a
//! constant that should have been bound afresh keeps a stale forced value.
//!
//! The package's `NAMESPACE` is random too, and edits change it. The model then also tracks what
//! a backend does with the namespace half of a plan: the imports it rebuilds, the exports it
//! resyncs, and each `S3method()` registration together with the value of the method's binding
//! when it was registered, so a method whose binding changed but was not registered again shows
//! up as a stale value.

use {
    crate::{Loader, PackageInputs, Plan},
    rustc_hash::{FxHashMap, FxHashSet},
    semantics::{
        load_index::{IndexedItem, PackageIndex, Registration},
        metadata::S3MethodDirective,
    },
    std::{
        collections::{BTreeMap, BTreeSet},
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

/// A package as statements per file, `FILES` files in collation order, and its `NAMESPACE`
/// as one directive per line.
#[derive(Debug, Clone)]
pub struct Package {
    pub files: Vec<Vec<String>>,
    pub namespace: Vec<String>,
}

const FILES: usize = 3;

fn random_statement(entropy: &mut impl Entropy) -> String {
    let number = entropy.below(100);
    match entropy.below(14) {
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
        11 => format!(
            "{} <- function(x, ...) {}(x) + {number}",
            method_binding(entropy),
            pick(entropy, "f", 5)
        ),
        12 => format!(
            "{} <- function(x) {}(x)",
            pick(entropy, "f", 5),
            ["print", "format", "summary"][entropy.below(3)]
        ),
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

/// One of the bindings the generated `S3method()` directives name.
fn method_binding(entropy: &mut impl Entropy) -> String {
    match entropy.below(3) {
        0 => "print.widget".to_owned(),
        1 => "format.widget".to_owned(),
        _ => pick(entropy, "f", 5),
    }
}

fn random_directive(entropy: &mut impl Entropy) -> String {
    match entropy.below(8) {
        0 => "S3method(print, widget)".to_owned(),
        1 => "S3method(format, widget)".to_owned(),
        2 => format!("S3method(summary, widget, {})", pick(entropy, "f", 5)),
        3 => format!("export({})", pick(entropy, "f", 5)),
        4 => format!(
            "export({}, {})",
            pick(entropy, "v", 4),
            pick(entropy, "c", 3)
        ),
        5 => "exportPattern(\"^c\")".to_owned(),
        6 => "importFrom(stats, setNames)".to_owned(),
        _ => "import(utils, except = c(\"head\"))".to_owned(),
    }
}

pub fn random_package(entropy: &mut impl Entropy) -> Package {
    let files = (0..FILES)
        .map(|_| {
            let header = (entropy.below(2) == 0).then(|| "# typing: strict".to_owned());
            header
                .into_iter()
                .chain((0..entropy.below(6)).map(|_| random_statement(entropy)))
                .collect()
        })
        .collect();
    let namespace = (0..entropy.below(5))
        .map(|_| random_directive(entropy))
        .collect();
    Package { files, namespace }
}

pub fn edit(entropy: &mut impl Entropy, package: &mut Package) {
    if entropy.below(4) == 0 {
        let namespace = &mut package.namespace;
        match entropy.below(3) {
            0 if !namespace.is_empty() => {
                let at = entropy.below(namespace.len());
                namespace.remove(at);
            }
            1 if !namespace.is_empty() => {
                let at = entropy.below(namespace.len());
                namespace[at] = random_directive(entropy);
            }
            _ => {
                let at = entropy.below(namespace.len() + 1);
                namespace.insert(at, random_directive(entropy));
            }
        }
        return;
    }
    let file = &mut package.files[entropy.below(FILES)];
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
        namespace: package
            .namespace
            .iter()
            .map(|directive| format!("{directive}\n"))
            .collect(),
        files: package
            .files
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
    /// Each registered S3 method by generic and class: the binding it names and that
    /// binding's value when it was registered.
    s3: BTreeMap<(String, String), (String, String)>,
    /// What the backend last rebuilt the imports environment from.
    imports: String,
    /// What the backend last resynced the exports from.
    exports: String,
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
    let first = loader
        .plan_inputs(root, inputs(before), false)
        .expect("the package plans");
    let old_index = loader.pending(root).expect("pending").index.clone();
    loader.commit(root);
    let old_texts = texts(before);
    let mut reloaded = fresh_load(&old_index, &old_texts);
    apply_namespace(&mut reloaded, &first);

    let plan = loader
        .plan_inputs(root, inputs(after), false)
        .expect("the edited package plans");
    let new_index = loader.pending(root).expect("pending").index.clone();
    let new_texts = texts(after);
    let scratch_plan = scratch
        .plan_inputs(root, inputs(after), false)
        .expect("the edited package plans from scratch");
    assert_eq!(
        *new_index,
        *scratch.pending(root).expect("pending").index,
        "the incrementally derived index differs from a fresh one"
    );
    let names_before: BTreeSet<String> = reloaded.bindings.keys().cloned().collect();
    let model_before = reloaded.clone();
    apply(&mut reloaded, &plan, &new_index, &new_texts);
    apply_namespace(&mut reloaded, &plan);
    let mut fresh = fresh_load(&new_index, &new_texts);
    apply_namespace(&mut fresh, &scratch_plan);

    if plan.keeps_names {
        let names_after: BTreeSet<String> = reloaded.bindings.keys().cloned().collect();
        assert_eq!(
            names_before, names_after,
            "the plan claims to keep the namespace's names\n---- plan:\n{plan:#?}"
        );
    }
    if let Some(writes) = &plan.run_writes {
        for code in &plan.run {
            let item = new_index
                .items()
                .find(|(path, item)| *path == code.path && item.span.char_start == code.start)
                .map(|(_, item)| item)
                .expect("a planned statement is in the index");
            for name in &item.binds {
                assert!(
                    writes.contains(name),
                    "`run_writes` leaves out `{name}`, which a planned statement writes\n---- plan:\n{plan:#?}"
                );
            }
        }
    }

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

    // `ry test --watch` runs first the tests that mention a caller of a touched name, so a
    // name whose reachable code changed, by name or through S3 dispatch, must be a caller.
    if let Some(touched) = plan.changed_names() {
        loader.commit(root);
        let callers = loader.callers(root, &touched);
        for name in model_before.bindings.keys().chain(fresh.bindings.keys()) {
            if reachable(&model_before, name) != reachable(&fresh, name) {
                assert!(
                    callers.contains(name),
                    "`{name}` reaches changed code, but the callers of {touched:?} are {callers:?}\n---- before:\n{before:#?}\n---- after:\n{after:#?}"
                );
            }
        }
    }
}

/// The bindings a call of `name` can reach through the names their values mention, each with
/// its value, and every mentioned name that is not bound. A mention of a generic reaches its
/// methods as S3 dispatch does: each method registered for it, and each binding named after it
/// and a class.
fn reachable<'a>(model: &'a Model, name: &'a str) -> BTreeMap<&'a str, Option<&'a str>> {
    let bindings = &model.bindings;
    let mut reached = BTreeMap::new();
    let mut queue = vec![name];
    while let Some(name) = queue.pop() {
        if reached.contains_key(name) {
            continue;
        }
        let value = bindings.get(name).map(String::as_str);
        reached.insert(name, value);
        queue.extend(
            model
                .s3
                .iter()
                .filter(|((generic, _), _)| generic == name)
                .map(|(_, (function, _))| function.as_str()),
        );
        queue.extend(bindings.keys().map(String::as_str).filter(|binding| {
            binding
                .strip_prefix(name)
                .is_some_and(|class| class.len() > 1 && class.starts_with('.'))
        }));
        for mention in value
            .unwrap_or("")
            .split(|character: char| {
                !(character.is_alphanumeric() || character == '.' || character == '_')
            })
            .filter(|mention| !mention.is_empty())
        {
            queue.push(mention);
        }
    }
    reached
}

/// What a backend does with the namespace half of a plan, once its bindings are in place.
fn apply_namespace(model: &mut Model, plan: &Plan) {
    if let Some(imports) = &plan.imports {
        model.imports = format!("{imports:?}");
    }
    if plan.exports_changed {
        model.exports = format!(
            "{:?} {:?}",
            plan.namespace.exports, plan.namespace.export_patterns
        );
    }
    for S3MethodDirective { generic, class, .. } in &plan.s3_unregister {
        model.s3.remove(&(generic.clone(), class.clone()));
    }
    for S3MethodDirective {
        generic,
        class,
        function,
    } in &plan.s3_register
    {
        let value = model
            .bindings
            .get(function)
            .cloned()
            .unwrap_or_else(|| "missing".to_owned());
        model
            .s3
            .insert((generic.clone(), class.clone()), (function.clone(), value));
    }
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
