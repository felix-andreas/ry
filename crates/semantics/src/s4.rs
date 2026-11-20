//! The S4 classes a project defines with `setClass`, read from the call as written, so that
//! `new("Class", ...)` can be held to the class's real slots.

use {
    crate::{
        Db, DocumentKind, ProjectFiles, SourceFile,
        hir::{Argument, ExprId, ExpressionKind, LiteralKind, Module, Symbol},
        item_hir, item_tree,
        types::QueryName,
    },
    std::collections::BTreeMap,
};

/// The S4 facts a file's top-level statements declare.
#[derive(Debug, Clone, Default, PartialEq, Eq, salsa::SalsaValue)]
pub struct FileClasses {
    pub definitions: Vec<ClassDefinition>,
    /// Classes given their own `initialize` method, whose `new()` may take any argument.
    pub initialized: Vec<Symbol>,
}

/// One `setClass("Name", ...)` call.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ClassDefinition {
    pub class: Symbol,
    /// `None` when a slot list or a parent is computed rather than written out, so the class
    /// may have any slot.
    pub shape: Option<ClassShape>,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct ClassShape {
    pub slots: Vec<Symbol>,
    pub parents: Vec<Symbol>,
}

/// Every slot `new(class, ...)` may name as an argument, gathered through `contains`, or
/// `None` when any class on the way is unknown, computed, defined twice, a basic type, or
/// has its own `initialize` method. Sorted by spelling.
#[salsa::tracked(returns(ref))]
pub fn class_slots<'db>(
    db: &'db dyn Db,
    file: SourceFile,
    class: QueryName<'db>,
) -> Option<Vec<Symbol>> {
    let project = ProjectFiles::try_get(db).map(|files| project_classes(db, files));
    let own = (*file.kind(db) != DocumentKind::Package).then(|| file_classes(db, file));
    let definition = |class: Symbol| -> Option<&ClassShape> {
        let mut found = own
            .into_iter()
            .flat_map(|classes| &classes.definitions)
            .filter(|definition| definition.class == class)
            .map(|definition| definition.shape.as_ref())
            .chain(
                project
                    .and_then(|project| project.definitions.get(&class))
                    .into_iter()
                    .flatten()
                    .map(Option::as_ref),
            );
        let shape = found.next()?;
        // Two definitions of one class leave which one runs to the order of evaluation.
        if found.next().is_some() {
            return None;
        }
        shape
    };
    let initialized = |class: Symbol| {
        own.is_some_and(|classes| classes.initialized.contains(&class))
            || project.is_some_and(|project| project.initialized.contains(&class))
    };
    let mut slots = Vec::new();
    let mut pending = vec![class.name(db).symbol()];
    let mut seen = Vec::new();
    while let Some(class) = pending.pop() {
        if seen.contains(&class) {
            continue;
        }
        seen.push(class);
        if initialized(class) {
            return None;
        }
        let shape = definition(class)?;
        slots.extend(shape.slots.iter().copied());
        pending.extend(
            shape
                .parents
                .iter()
                .copied()
                .filter(|parent| parent.as_str() != "VIRTUAL"),
        );
    }
    slots.sort_unstable();
    slots.dedup();
    Some(slots)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, salsa::SalsaValue)]
struct ProjectClasses {
    /// Each class's definitions across the package files, in file order.
    definitions: BTreeMap<Symbol, Vec<Option<ClassShape>>>,
    initialized: Vec<Symbol>,
}

#[salsa::tracked(returns(ref))]
fn project_classes(db: &dyn Db, files: ProjectFiles) -> ProjectClasses {
    let mut classes = ProjectClasses::default();
    for &file in files.files(db) {
        if *file.kind(db) != DocumentKind::Package {
            continue;
        }
        let own = file_classes(db, file);
        for definition in &own.definitions {
            classes
                .definitions
                .entry(definition.class)
                .or_default()
                .push(definition.shape.clone());
        }
        classes.initialized.extend(own.initialized.iter().copied());
    }
    classes
}

#[salsa::tracked(returns(ref))]
pub fn file_classes(db: &dyn Db, file: SourceFile) -> FileClasses {
    let mut classes = FileClasses::default();
    for &item in item_tree(db, file).iter() {
        let Some(module) = item_hir(db, item) else {
            continue;
        };
        let Some(mut statement) = module.root else {
            continue;
        };
        // `Gen <- setClass(...)` defines the class and binds its generator.
        if let ExpressionKind::Assign { value, .. } = module.kind(statement) {
            statement = *value;
        }
        let ExpressionKind::Call { callee, arguments } = module.kind(statement) else {
            continue;
        };
        let arguments = &module[arguments];
        match methods_function(&module, *callee) {
            Some("setClass") => {
                let matched = match_arguments(&module, arguments, &SET_CLASS_FORMALS);
                let Some(class) = matched[0].and_then(|value| string(&module, value)) else {
                    continue;
                };
                classes.definitions.push(ClassDefinition {
                    class,
                    shape: class_shape(&module, &matched),
                });
            }
            Some("setMethod") => {
                let matched = match_arguments(&module, arguments, &["f", "signature"]);
                if matched[0]
                    .and_then(|value| string(&module, value))
                    .map(Symbol::as_str)
                    != Some("initialize")
                {
                    continue;
                }
                let first_class = matched[1].and_then(|signature| match module.kind(signature) {
                    ExpressionKind::Call { arguments, .. } => module[arguments]
                        .first()
                        .and_then(|argument| argument.value)
                        .and_then(|value| string(&module, value)),
                    _ => string(&module, signature),
                });
                if let Some(class) = first_class {
                    classes.initialized.push(class);
                }
            }
            _ => {}
        }
    }
    classes
}

/// `setClass`'s formals in R's order, which positional arguments fill.
const SET_CLASS_FORMALS: [&str; 12] = [
    "Class",
    "representation",
    "prototype",
    "contains",
    "validity",
    "access",
    "where",
    "version",
    "sealed",
    "package",
    "S3methods",
    "slots",
];

fn class_shape(module: &Module, matched: &[Option<ExprId>]) -> Option<ClassShape> {
    let mut shape = ClassShape {
        slots: Vec::new(),
        parents: Vec::new(),
    };
    // `representation(x = "numeric", "Parent")`: a named entry is a slot, an unnamed one a
    // parent.
    if let Some(representation) = matched[1] {
        for (name, value) in listed(module, representation)? {
            match name {
                Some(name) => shape.slots.push(name),
                None => shape.parents.push(string(module, value?)?),
            }
        }
    }
    if let Some(contains) = matched[3] {
        for (name, value) in listed(module, contains)? {
            if name.is_some() {
                return None;
            }
            shape.parents.push(string(module, value?)?);
        }
    }
    // `slots = c(x = "numeric")` names a slot with its class, `slots = c("x")` names it bare.
    if let Some(slots) = matched[11] {
        for (name, value) in listed(module, slots)? {
            shape.slots.push(match name {
                Some(name) => name,
                None => string(module, value?)?,
            });
        }
    }
    Some(shape)
}

/// The entries of a `c(...)`, `list(...)` or `representation(...)` call, or of a lone string,
/// as `(name, value)`; `None` for anything computed.
fn listed(module: &Module, value: ExprId) -> Option<Vec<(Option<Symbol>, Option<ExprId>)>> {
    match module.kind(value) {
        ExpressionKind::Literal(LiteralKind::String(_)) => Some(vec![(None, Some(value))]),
        ExpressionKind::Call { callee, arguments }
            if matches!(module.kind(*callee),
                ExpressionKind::NameRef(name)
                    if matches!(name.as_str(), "c" | "list" | "representation")) =>
        {
            Some(
                module[arguments]
                    .iter()
                    .map(|argument| (module.argument_name(argument), argument.value))
                    .collect(),
            )
        }
        _ => None,
    }
}

/// The methods-package function `callee` names, bare or as `methods::name`.
pub(crate) fn methods_function(module: &Module, callee: ExprId) -> Option<&'static str> {
    match module.kind(callee) {
        ExpressionKind::NameRef(name) => Some(name.as_str()),
        ExpressionKind::Namespace {
            package: Some(package),
            name: Some(name),
            ..
        } if package.as_str() == "methods" => Some(name.as_str()),
        _ => None,
    }
}

/// R's matching of `arguments` to `formals` by exact name, then by position for the rest.
pub(crate) fn match_arguments(
    module: &Module,
    arguments: &[Argument],
    formals: &[&str],
) -> Vec<Option<ExprId>> {
    let mut matched = vec![None; formals.len()];
    let mut positional = Vec::new();
    for argument in arguments {
        match module.argument_name(argument) {
            Some(name) => {
                if let Some(index) = formals.iter().position(|formal| name == *formal) {
                    matched[index] = argument.value;
                }
            }
            None => positional.push(argument.value),
        }
    }
    let mut positional = positional.into_iter();
    for slot in &mut matched {
        if slot.is_none() {
            match positional.next() {
                Some(value) => *slot = value,
                None => break,
            }
        }
    }
    matched
}

pub(crate) fn string(module: &Module, value: ExprId) -> Option<Symbol> {
    match module.kind(value) {
        ExpressionKind::Literal(LiteralKind::String(text)) => Some(Symbol::new(&module[*text])),
        _ => None,
    }
}
