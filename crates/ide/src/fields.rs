//! `$` fields. R has no field declarations, so a field's definition is where the value it is read
//! from gets that element: a named element of the `list(...)` (or record, data frame, R6 class)
//! the receiver was built from, or a `receiver$name <- value` write. A read is resolved by following
//! its receiver back to those constructions; a receiver that cannot be followed (a parameter, an
//! element of an indexed list) falls back to every place in the project that declares a field of
//! that name, which is a guess, so such a read never takes part in a rename. A receiver a library
//! function returns (`optim(...)$par`) has its fields declared by that function, so the read
//! navigates to the function, whose documentation lists what it returns.

use {
    crate::{NavigationTarget, Occurrence, declaring_item, position_in_item},
    semantics::{
        Db, Item, ItemKind, ProjectFiles, SourceFile,
        hir::{AssignSpelling, ExprId, ExpressionKind, Module, Symbol},
        item_hir, item_naming, item_node, item_tree,
        naming::BindingKind,
    },
    std::{
        collections::{HashMap, HashSet},
        mem,
    },
    syntax::{TextRange, TextSize},
};

/// Where a field at the cursor is declared. `precise` is false for the by-name fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldTarget {
    pub name: Symbol,
    pub sites: Vec<NavigationTarget>,
    pub precise: bool,
    /// The heads of the library calls the receiver comes from, for a receiver no project
    /// construction builds.
    pub producers: Vec<NavigationTarget>,
}

/// The field at `relative` in `item`: the name of a `$` read or write, or a named element of a
/// constructor call (`list(total = 1)`) or record, which declares itself.
pub(crate) fn field_at(
    db: &dyn Db,
    files: ProjectFiles,
    item: Item<'_>,
    relative: TextSize,
) -> Option<FieldTarget> {
    let hir = item_hir(db, item).as_ref()?;
    let file = *item.file(db);
    let item_offset = item_node(db, item)?.text_range().start();
    for (index, expression) in hir.expressions().enumerate() {
        if !expression.range.contains_inclusive(relative) {
            continue;
        }
        match expression.kind {
            ExpressionKind::Field {
                at: false,
                name: Some(name),
                ..
            } if hir.name_range(*name).contains_inclusive(relative) => {
                return Some(resolve_field(db, files, item, ExprId::new(index)));
            }
            ExpressionKind::Call { callee, arguments } if is_constructor(hir, *callee) => {
                for argument in &hir[arguments] {
                    let (Some(name), Some(range)) = (
                        hir.argument_name(argument),
                        hir.argument_name_range(argument),
                    ) else {
                        continue;
                    };
                    if range.contains_inclusive(relative) {
                        return Some(declared_here(file, name, range + item_offset));
                    }
                }
            }
            // A tag's payload names are matched by patterns, not read with `$`.
            ExpressionKind::Record(fields)
                if !hir.expressions().any(|expression| {
                    matches!(expression.kind, ExpressionKind::Tag { payload: Some(payload), .. }
                        if *payload == ExprId::new(index))
                }) =>
            {
                if let Some(field) = hir[fields]
                    .iter()
                    .find(|field| field.name_range.contains_inclusive(relative))
                {
                    return Some(declared_here(
                        file,
                        field.name,
                        field.name_range + item_offset,
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

fn declared_here(file: SourceFile, name: Symbol, range: TextRange) -> FieldTarget {
    FieldTarget {
        name,
        sites: vec![NavigationTarget { file, range }],
        precise: true,
        producers: Vec::new(),
    }
}

/// The library calls the receiver of the `$` field at `relative` comes from, without the by-name
/// fallback, which hover has no use for and which costs a project-wide index on first use.
pub(crate) fn field_producers(
    db: &dyn Db,
    files: ProjectFiles,
    item: Item<'_>,
    relative: TextSize,
) -> Option<Vec<NavigationTarget>> {
    let hir = item_hir(db, item).as_ref()?;
    let field = hir.expressions().position(|expression| {
        matches!(expression.kind, ExpressionKind::Field { at: false, name: Some(name), .. }
            if hir.name_range(*name).contains_inclusive(relative))
    })?;
    let resolved = precise_sites(db, files, item, ExprId::new(field))?;
    Some(match resolved.sites.is_empty() {
        true => resolved.producers,
        false => Vec::new(),
    })
}

/// Where the field read or written by `field` (a `Field` expression of `item`) is declared.
pub(crate) fn resolve_field<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    item: Item<'db>,
    field: ExprId,
) -> FieldTarget {
    let Some(Resolved {
        name,
        sites,
        producers,
    }) = precise_sites(db, files, item, field)
    else {
        return FieldTarget {
            name: Symbol::new(""),
            sites: Vec::new(),
            precise: false,
            producers: Vec::new(),
        };
    };
    if !sites.is_empty() {
        return FieldTarget {
            name,
            sites,
            precise: true,
            producers: Vec::new(),
        };
    }
    if !producers.is_empty() {
        return FieldTarget {
            name,
            sites: Vec::new(),
            precise: false,
            producers,
        };
    }
    FieldTarget {
        name,
        sites: project_declarations(db, files, name),
        precise: false,
        producers: Vec::new(),
    }
}

struct Resolved {
    name: Symbol,
    sites: Vec<NavigationTarget>,
    producers: Vec<NavigationTarget>,
}

/// The sites the receiver's constructions declare, empty when the receiver cannot be followed.
fn precise_sites<'db>(
    db: &'db dyn Db,
    files: ProjectFiles,
    item: Item<'db>,
    field: ExprId,
) -> Option<Resolved> {
    resolve_with(&mut Resolver::new(db, files), item, field)
}

/// [`precise_sites`] through a resolver that may already hold the sources of earlier accesses.
fn resolve_with<'db>(
    resolver: &mut Resolver<'db>,
    item: Item<'db>,
    field: ExprId,
) -> Option<Resolved> {
    let db = resolver.db;
    resolver.visited.clear();
    resolver.producers.clear();
    let hir = item_hir(db, item).as_ref()?;
    let ExpressionKind::Field {
        target,
        name: Some(name),
        ..
    } = hir.kind(field)
    else {
        return None;
    };
    let name = hir[*name].name;
    let absolute = |item: Item<'db>, range: TextRange| {
        Some(NavigationTarget {
            file: *item.file(db),
            range: range + item_node(db, item)?.text_range().start(),
        })
    };
    let mut sites: Vec<NavigationTarget> = Vec::new();
    for site in resolver.sites(item, *target, name, 0) {
        if let Some(target) = absolute(site.item, site.range)
            && !sites.contains(&target)
        {
            sites.push(target);
        }
    }
    let mut producers: Vec<NavigationTarget> = Vec::new();
    for (producer_item, callee) in mem::take(&mut resolver.producers) {
        let Some(range) = item_hir(db, producer_item)
            .as_ref()
            .map(|hir| hir.range(callee))
        else {
            continue;
        };
        if let Some(target) = absolute(producer_item, range)
            && !producers.contains(&target)
        {
            producers.push(target);
        }
    }
    Some(Resolved {
        name,
        sites,
        producers,
    })
}

/// Every read, write and declaration of the field `target` names. A guessed target is the
/// by-name set of declarations, which every same-named access reaches, so all of them belong to
/// it. A precise
/// target takes the precise reads that reach one of its sites, so that a rename never edits a
/// read it merely guessed at.
pub(crate) fn field_occurrences(
    db: &dyn Db,
    files: ProjectFiles,
    target: &FieldTarget,
) -> Vec<Occurrence> {
    // A field nothing in the project declares (a column of a package's data frame, say) is no
    // symbol the project owns, so same-named reads are not known to be the same field.
    if target.sites.is_empty() {
        return Vec::new();
    }
    let declaration = |file, range| Occurrence {
        file,
        range,
        is_declaration: true,
    };
    let mut result: Vec<Occurrence> = target
        .sites
        .iter()
        .map(|site| declaration(site.file, site.range))
        .collect();
    let Some(spellings) = field_index(db, files).by_name.get(target.name.as_str()) else {
        return result;
    };
    let accesses = spellings
        .iter()
        .filter(|spelled| spelled.kind == SpellingKind::Access);
    if !target.precise {
        result.extend(accesses.map(|spelled| Occurrence {
            file: spelled.file,
            range: spelled.range,
            is_declaration: false,
        }));
        return result;
    }
    let target_sites: HashSet<NavigationTarget> = target.sites.iter().copied().collect();
    let mut resolver = Resolver::new(db, files);
    for spelled in accesses {
        let file = spelled.file;
        let Some(position) = position_in_item(db, file, spelled.range.start()) else {
            continue;
        };
        let Some(hir) = item_hir(db, position.item).as_ref() else {
            continue;
        };
        let relative = spelled.range - position.item_offset;
        let Some(index) = hir.expressions().position(|expression| {
            matches!(expression.kind, ExpressionKind::Field { name: Some(name), .. }
                if hir.name_range(*name) == relative)
        }) else {
            continue;
        };
        let Some(Resolved { sites, .. }) =
            resolve_with(&mut resolver, position.item, ExprId::new(index))
        else {
            continue;
        };
        if sites.iter().any(|site| target_sites.contains(site)) {
            let here = NavigationTarget {
                file,
                range: spelled.range,
            };
            result.push(Occurrence {
                file,
                range: spelled.range,
                is_declaration: sites.contains(&here),
            });
        }
    }
    result
}

/// Every declaration of a field named `name` in the project, the fallback for a read whose
/// receiver cannot be followed.
fn project_declarations(db: &dyn Db, files: ProjectFiles, name: Symbol) -> Vec<NavigationTarget> {
    field_index(db, files)
        .by_name
        .get(name.as_str())
        .into_iter()
        .flatten()
        .filter(|spelled| spelled.kind != SpellingKind::Access)
        .map(|spelled| NavigationTarget {
            file: spelled.file,
            range: spelled.range,
        })
        .collect()
}

/// Every field spelling in the project by name. Any edit rebuilds it, but only from the per-file
/// memos, and it spares each request a scan of every file, which a fallback read or a references
/// request over a common name like `value` would otherwise repeat thousands of times.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct FieldIndex {
    by_name: HashMap<String, Vec<IndexedSpelling>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
struct IndexedSpelling {
    file: SourceFile,
    kind: SpellingKind,
    range: TextRange,
}

#[salsa::tracked(returns(ref))]
fn field_index(db: &dyn Db, files: ProjectFiles) -> FieldIndex {
    let mut by_name: HashMap<String, Vec<IndexedSpelling>> = HashMap::new();
    let mut forwarders: HashMap<&str, &[String]> = HashMap::new();
    for &file in files.files(db) {
        let fields = file_fields(db, file);
        for forwarder in &fields.forwarders {
            forwarders
                .entry(forwarder.name.as_str())
                .or_insert(&forwarder.formals);
        }
        for spelled in &fields.spellings {
            by_name
                .entry(spelled.name.clone())
                .or_default()
                .push(IndexedSpelling {
                    file,
                    kind: spelled.kind,
                    range: spelled.range,
                });
        }
    }
    // `new_rate(pause = 1)` declares `pause` when `new_rate` puts its `...` into `list(...)`.
    for &file in files.files(db) {
        for argument in &file_fields(db, file).call_arguments {
            if forwarders
                .get(argument.callee.as_str())
                .is_some_and(|formals| !formals.contains(&argument.name))
            {
                by_name
                    .entry(argument.name.clone())
                    .or_default()
                    .push(IndexedSpelling {
                        file,
                        kind: SpellingKind::Element,
                        range: argument.range,
                    });
            }
        }
    }
    FieldIndex { by_name }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpellingKind {
    /// A named element of a constructor call or record.
    Element,
    /// The field of `receiver$name <- value`, which is also an access.
    Write,
    Access,
}

/// A file's field spellings, and what the project index needs to find fields passed through
/// `...` into a constructor.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct FileFields {
    spellings: Vec<FieldSpelling>,
    forwarders: Vec<Forwarder>,
    /// Named arguments of calls to plain names, which become fields when the callee is a
    /// forwarder and the name is none of its parameters.
    call_arguments: Vec<CallArgument>,
}

/// A top-level function that passes its `...` to a constructor, as `list(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Forwarder {
    name: String,
    formals: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CallArgument {
    callee: String,
    name: String,
    range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
struct FieldSpelling {
    name: String,
    kind: SpellingKind,
    range: TextRange,
}

/// Memoized per file, so the by-name fallback and references do not lower every item of a large
/// project on each request. A write is listed twice, as a write and as an access.
#[salsa::tracked(returns(ref))]
fn file_fields(db: &dyn Db, file: SourceFile) -> FileFields {
    let mut spellings = Vec::new();
    let mut forwarders = Vec::new();
    let mut call_arguments = Vec::new();
    for &item in item_tree(db, file) {
        let (Some(hir), Some(node)) = (item_hir(db, item).as_ref(), item_node(db, item)) else {
            continue;
        };
        let item_offset = node.text_range().start();
        let is_dots = |value: Option<ExprId>| {
            value.is_some_and(
                |value| matches!(hir.kind(value), ExpressionKind::NameRef(name) if name.as_str() == "..."),
            )
        };
        if *item.kind(db) == ItemKind::Function
            && let Some(name) = item.name(db)
            && let Some(parameters) = hir.expressions().find_map(|expression| match &expression.kind {
                ExpressionKind::Function { parameters, .. } => Some(*parameters),
                _ => None,
            })
            && hir[parameters].iter().any(|parameter| parameter.name.as_str() == "...")
            && hir.expressions().any(|expression| {
                matches!(&expression.kind, ExpressionKind::Call { callee, arguments }
                    if is_constructor(hir, *callee)
                        && hir[arguments].iter().any(|argument| argument.name.is_none() && is_dots(argument.value)))
            })
        {
            forwarders.push(Forwarder {
                name: name.clone(),
                formals: hir[parameters]
                    .iter()
                    .map(|parameter| parameter.name.to_string())
                    .collect(),
            });
        }
        let mut push = |name: Symbol, kind, range: TextRange| {
            spellings.push(FieldSpelling {
                name: name.to_string(),
                kind,
                range: range + item_offset,
            });
        };
        for expression in hir.expressions() {
            match expression.kind {
                ExpressionKind::Field {
                    at: false,
                    name: Some(name),
                    ..
                } => push(hir[*name].name, SpellingKind::Access, hir.name_range(*name)),
                ExpressionKind::Assign { target, .. } => {
                    if let ExpressionKind::Field {
                        at: false,
                        name: Some(name),
                        ..
                    } = hir.kind(*target)
                    {
                        push(hir[*name].name, SpellingKind::Write, hir.name_range(*name));
                    }
                }
                ExpressionKind::Call { callee, arguments } if is_constructor(hir, *callee) => {
                    for argument in &hir[arguments] {
                        if let (Some(name), Some(range)) = (
                            hir.argument_name(argument),
                            hir.argument_name_range(argument),
                        ) {
                            push(name, SpellingKind::Element, range);
                        }
                    }
                }
                ExpressionKind::Record(fields) => {
                    for field in &hir[fields] {
                        push(field.name, SpellingKind::Element, field.name_range);
                    }
                }
                ExpressionKind::Call { callee, arguments } => {
                    if let ExpressionKind::NameRef(callee) = hir.kind(*callee) {
                        for argument in &hir[arguments] {
                            if let (Some(name), Some(range)) = (
                                hir.argument_name(argument),
                                hir.argument_name_range(argument),
                            ) {
                                call_arguments.push(CallArgument {
                                    callee: callee.to_string(),
                                    name: name.to_string(),
                                    range: range + item_offset,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    spellings.sort_by_key(|spelled| spelled.range.start());
    FileFields {
        spellings,
        forwarders,
        call_arguments,
    }
}

/// Calls whose named arguments become the `$` fields of the value they build.
const CONSTRUCTORS: &[&str] = &[
    "list",
    "list2",
    "lst",
    "data.frame",
    "data.table",
    "DataFrame",
    "tibble",
    "env",
    // data.table's alias for `list` inside `[`.
    ".",
];

/// Calls that return their first argument with attributes or visibility changed.
const PASS_THROUGH: &[&str] = &["structure", "invisible", "new_data_frame", "as.environment"];

const R6_CLASS: &str = "R6Class";

/// Following a receiver through a chain of assignments and calls stops here, which bounds the
/// cost of a request on code that builds values through many helpers.
const MAX_DEPTH: usize = 8;

fn is_constructor(hir: &Module, callee: ExprId) -> bool {
    callee_name(hir, callee).is_some_and(|name| CONSTRUCTORS.contains(&name.as_str()))
}

fn callee_name(hir: &Module, callee: ExprId) -> Option<Symbol> {
    match hir.kind(callee) {
        ExpressionKind::NameRef(name) => Some(*name),
        ExpressionKind::Namespace { name, .. } => *name,
        _ => None,
    }
}

/// A declaration of a field, item-relative, with the value it holds when there is one.
#[derive(Clone, Copy)]
struct Site<'db> {
    item: Item<'db>,
    range: TextRange,
    value: Option<ExprId>,
}

/// A value whose fields can be listed: a constructor call or record, or an R6 class seen from
/// outside (`public` and `active` members) or from its own methods through `private`.
#[derive(Clone, Copy)]
enum Source<'db> {
    Value(Item<'db>, ExprId),
    R6 {
        item: Item<'db>,
        class: ExprId,
        private: bool,
    },
}

struct Resolver<'db> {
    db: &'db dyn Db,
    files: ProjectFiles,
    /// Recursive helpers and self-referential assignments would otherwise loop.
    visited: HashSet<(Item<'db>, ExprId, Symbol)>,
    /// The heads of library calls a followed value came from, as item and callee.
    producers: Vec<(Item<'db>, ExprId)>,
    /// Per field name, the items that write it; per item and name, those writes. A global
    /// receiver's writes are searched across the project once, not once per access.
    written_items: HashMap<Symbol, Vec<Item<'db>>>,
    field_writes: HashMap<(Item<'db>, Symbol), Vec<(ExprId, Site<'db>)>>,
}

impl<'db> Resolver<'db> {
    fn new(db: &'db dyn Db, files: ProjectFiles) -> Self {
        Resolver {
            db,
            files,
            visited: HashSet::new(),
            producers: Vec::new(),
            written_items: HashMap::new(),
            field_writes: HashMap::new(),
        }
    }

    /// The declarations of `name` on the value of `receiver`.
    fn sites(
        &mut self,
        item: Item<'db>,
        receiver: ExprId,
        name: Symbol,
        depth: usize,
    ) -> Vec<Site<'db>> {
        if depth > MAX_DEPTH || !self.visited.insert((item, receiver, name)) {
            return Vec::new();
        }
        let mut sources = Vec::new();
        self.sources(item, receiver, depth, &mut sources);
        let mut sites = Vec::new();
        for source in sources {
            self.declarations(source, name, depth, &mut sites);
        }
        self.writes(item, receiver, name, &mut sites);
        sites
    }

    /// The constructions the value of `expression` comes from.
    fn sources(
        &mut self,
        item: Item<'db>,
        expression: ExprId,
        depth: usize,
        sources: &mut Vec<Source<'db>>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let Some(hir) = item_hir(self.db, item).as_ref() else {
            return;
        };
        match hir.kind(expression) {
            ExpressionKind::Paren(inner) => self.sources(item, *inner, depth + 1, sources),
            ExpressionKind::Assign { value, .. } => self.sources(item, *value, depth + 1, sources),
            ExpressionKind::Block { statements } => {
                if let Some(last) = hir[statements].last() {
                    self.sources(item, *last, depth + 1, sources);
                }
            }
            ExpressionKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.sources(item, *then_branch, depth + 1, sources);
                if let Some(else_branch) = else_branch {
                    self.sources(item, *else_branch, depth + 1, sources);
                }
            }
            ExpressionKind::Record(_) => sources.push(Source::Value(item, expression)),
            ExpressionKind::Call { callee, arguments } => {
                let name = callee_name(hir, *callee);
                let spelled = name.map(Symbol::as_str);
                if spelled.is_some_and(|name| CONSTRUCTORS.contains(&name) || name == R6_CLASS) {
                    sources.push(Source::Value(item, expression));
                } else if spelled.is_some_and(|name| PASS_THROUGH.contains(&name)) {
                    if let Some(first) = hir[arguments].iter().find_map(|argument| argument.value) {
                        self.sources(item, first, depth + 1, sources);
                    }
                } else if let ExpressionKind::Field {
                    target,
                    name: Some(method),
                    ..
                } = hir.kind(*callee)
                    && hir[*method].name.as_str() == "new"
                {
                    // `Class$new(...)` builds an object of the R6 class `Class` names.
                    let mut generators = Vec::new();
                    self.sources(item, *target, depth + 1, &mut generators);
                    sources.extend(generators.into_iter().filter_map(
                        |generator| match generator {
                            Source::Value(item, class) if self.is_r6_class(item, class) => {
                                Some(Source::R6 {
                                    item,
                                    class,
                                    private: false,
                                })
                            }
                            _ => None,
                        },
                    ));
                } else {
                    let functions = self.called_functions(item, *callee);
                    if functions.is_empty() && self.is_library_function(item, *callee) {
                        self.producers.push((item, *callee));
                    }
                    for (function_item, function) in functions {
                        self.returned(function_item, function, depth + 1, sources);
                    }
                }
            }
            ExpressionKind::NameRef(name) => {
                let Some(naming) = item_naming(self.db, item).as_ref() else {
                    return;
                };
                match naming.resolutions.get(&expression) {
                    Some(binding)
                        if naming
                            .bindings
                            .get(binding)
                            .is_some_and(|info| info.kind != BindingKind::TopLevel) =>
                    {
                        let values: Vec<ExprId> = assigned_values(hir, naming, *binding);
                        for value in values {
                            self.sources(item, value, depth + 1, sources);
                        }
                    }
                    Some(_) | None => {
                        if matches!(name.as_str(), "self" | "private")
                            && let Some(class) = enclosing_r6_class(hir, expression)
                        {
                            sources.push(Source::R6 {
                                item,
                                class,
                                private: name.as_str() == "private",
                            });
                            return;
                        }
                        self.global_sources(*name, depth, sources);
                    }
                }
            }
            ExpressionKind::Field {
                target,
                name: Some(name),
                ..
            } => {
                let name = hir[*name].name;
                for site in self.sites(item, *target, name, depth + 1) {
                    if let Some(value) = site.value {
                        self.sources(site.item, value, depth + 1, sources);
                    }
                }
            }
            _ => {}
        }
    }

    /// The values a project top-level name is assigned.
    fn global_sources(&mut self, name: Symbol, depth: usize, sources: &mut Vec<Source<'db>>) {
        let Some(definition) = declaring_item(self.db, self.files, name.as_str()) else {
            return;
        };
        let (Some(hir), Some(naming)) = (
            item_hir(self.db, definition).as_ref(),
            item_naming(self.db, definition).as_ref(),
        ) else {
            return;
        };
        let Some(binding) = naming
            .bindings
            .iter()
            .find(|(_, info)| info.kind == BindingKind::TopLevel && info.name == name)
            .map(|(binding, _)| *binding)
        else {
            return;
        };
        for value in assigned_values(hir, naming, binding) {
            self.sources(definition, value, depth + 1, sources);
        }
    }

    /// The project functions a call head reaches, as the function expression and its item.
    fn called_functions(&mut self, item: Item<'db>, callee: ExprId) -> Vec<(Item<'db>, ExprId)> {
        let (Some(hir), Some(naming)) = (
            item_hir(self.db, item).as_ref(),
            item_naming(self.db, item).as_ref(),
        ) else {
            return Vec::new();
        };
        let ExpressionKind::NameRef(name) = hir.kind(callee) else {
            return Vec::new();
        };
        let functions = |item: Item<'db>, hir: &Module, values: Vec<ExprId>| {
            values
                .into_iter()
                .filter(|value| matches!(hir.kind(*value), ExpressionKind::Function { .. }))
                .map(|value| (item, value))
                .collect::<Vec<_>>()
        };
        if let Some(binding) = naming.resolutions.get(&callee)
            && naming
                .bindings
                .get(binding)
                .is_some_and(|info| info.kind != BindingKind::TopLevel)
        {
            return functions(item, hir, assigned_values(hir, naming, *binding));
        }
        let Some(definition) = declaring_item(self.db, self.files, name.as_str()) else {
            return Vec::new();
        };
        let (Some(hir), Some(naming)) = (
            item_hir(self.db, definition).as_ref(),
            item_naming(self.db, definition).as_ref(),
        ) else {
            return Vec::new();
        };
        let Some(binding) = naming
            .bindings
            .iter()
            .find(|(_, info)| info.kind == BindingKind::TopLevel && info.name == *name)
            .map(|(binding, _)| *binding)
        else {
            return Vec::new();
        };
        functions(definition, hir, assigned_values(hir, naming, binding))
    }

    /// The sources of what `function` returns: its body's value and every `return(value)`.
    fn returned(
        &mut self,
        item: Item<'db>,
        function: ExprId,
        depth: usize,
        sources: &mut Vec<Source<'db>>,
    ) {
        let Some(hir) = item_hir(self.db, item).as_ref() else {
            return;
        };
        let ExpressionKind::Function { body, .. } = hir.kind(function) else {
            return;
        };
        let body = *body;
        let mut returns = Vec::new();
        collect_returns(hir, body, &mut returns);
        self.sources(item, body, depth, sources);
        for value in returns {
            self.sources(item, value, depth, sources);
        }
    }

    /// The declarations of `name` that `source` lists.
    fn declarations(
        &mut self,
        source: Source<'db>,
        name: Symbol,
        depth: usize,
        sites: &mut Vec<Site<'db>>,
    ) {
        match source {
            Source::Value(item, expression) => {
                let Some(hir) = item_hir(self.db, item).as_ref() else {
                    return;
                };
                match hir.kind(expression) {
                    ExpressionKind::Record(fields) => {
                        sites.extend(hir[fields].iter().filter(|field| field.name == name).map(
                            |field| Site {
                                item,
                                range: field.name_range,
                                value: Some(field.value),
                            },
                        ));
                    }
                    ExpressionKind::Call { arguments, .. }
                        if !self.is_r6_class(item, expression) =>
                    {
                        sites.extend(hir[arguments].iter().filter_map(|argument| {
                            (hir.argument_name(argument)? == name).then_some(())?;
                            Some(Site {
                                item,
                                range: hir.argument_name_range(argument)?,
                                value: argument.value,
                            })
                        }));
                    }
                    _ => {}
                }
            }
            Source::R6 {
                item,
                class,
                private,
            } => self.r6_members(item, class, private, name, depth, sites),
        }
    }

    /// A member of an R6 class or of the classes it inherits from. The class's own declaration
    /// shadows an inherited one, as R6 does.
    fn r6_members(
        &mut self,
        item: Item<'db>,
        class: ExprId,
        private: bool,
        name: Symbol,
        depth: usize,
        sites: &mut Vec<Site<'db>>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let Some(hir) = item_hir(self.db, item).as_ref() else {
            return;
        };
        let ExpressionKind::Call { arguments, .. } = hir.kind(class) else {
            return;
        };
        let groups: &[&str] = if private {
            &["private"]
        } else {
            &["public", "active"]
        };
        let mut own = Vec::new();
        let mut parent = None;
        for argument in &hir[arguments] {
            let (Some(group), Some(value)) = (hir.argument_name(argument), argument.value) else {
                continue;
            };
            if group.as_str() == "inherit" {
                parent = Some(value);
            } else if groups.contains(&group.as_str()) {
                let mut lists = Vec::new();
                self.sources(item, value, depth + 1, &mut lists);
                for list in lists {
                    self.declarations(list, name, depth + 1, &mut own);
                }
            }
        }
        if !own.is_empty() {
            sites.extend(own);
            return;
        }
        let Some(parent) = parent else {
            return;
        };
        let mut classes = Vec::new();
        self.sources(item, parent, depth + 1, &mut classes);
        for parent_class in classes {
            if let Source::Value(parent_item, parent_class) = parent_class
                && self.is_r6_class(parent_item, parent_class)
            {
                self.r6_members(parent_item, parent_class, private, name, depth + 1, sites);
            }
        }
    }

    /// A call head naming a function the project does not define: `pkg::name`, or a free name
    /// with no local binding and no project declaration.
    fn is_library_function(&self, item: Item<'db>, callee: ExprId) -> bool {
        let (Some(hir), Some(naming)) = (
            item_hir(self.db, item).as_ref(),
            item_naming(self.db, item).as_ref(),
        ) else {
            return false;
        };
        match hir.kind(callee) {
            ExpressionKind::Namespace { name: Some(_), .. } => true,
            ExpressionKind::NameRef(name) => {
                naming.non_locals.contains_key(&callee)
                    && declaring_item(self.db, self.files, name.as_str()).is_none()
            }
            _ => false,
        }
    }

    fn is_r6_class(&self, item: Item<'db>, expression: ExprId) -> bool {
        item_hir(self.db, item).as_ref().is_some_and(|hir| {
            matches!(hir.kind(expression), ExpressionKind::Call { callee, .. }
                if callee_name(hir, *callee).is_some_and(|name| name.as_str() == R6_CLASS))
        })
    }

    /// `receiver$name <- value` writes to the variable `receiver` names, in the item that binds
    /// it, or in any item for a global. Inside an R6 class, writes through `self` or `private`
    /// in the class's methods.
    fn writes(
        &mut self,
        item: Item<'db>,
        receiver: ExprId,
        name: Symbol,
        sites: &mut Vec<Site<'db>>,
    ) {
        let db = self.db;
        let (Some(hir), Some(naming)) =
            (item_hir(db, item).as_ref(), item_naming(db, item).as_ref())
        else {
            return;
        };
        let ExpressionKind::NameRef(variable) = hir.kind(receiver) else {
            return;
        };
        let local = naming
            .resolutions
            .get(&receiver)
            .copied()
            .filter(|binding| {
                naming
                    .bindings
                    .get(binding)
                    .is_some_and(|info| info.kind != BindingKind::TopLevel)
            });
        if let Some(binding) = local {
            sites.extend(
                self.field_writes(item, name)
                    .iter()
                    .filter(|(written, _)| naming.resolutions.get(written) == Some(&binding))
                    .map(|(_, site)| *site),
            );
            return;
        }
        if matches!(variable.as_str(), "self" | "private")
            && let Some(class) = enclosing_r6_class(hir, receiver)
        {
            let class_range = hir.range(class);
            sites.extend(
                self.field_writes(item, name)
                    .iter()
                    .filter(|(written, _)| {
                        matches!(hir.kind(*written), ExpressionKind::NameRef(spelled) if spelled == variable)
                            && class_range.contains_range(hir.range(*written))
                    })
                    .map(|(_, site)| *site),
            );
            return;
        }
        let variable = *variable;
        let written_items = match self.written_items.get(&name) {
            Some(items) => items.clone(),
            None => {
                let mut items: Vec<Item<'db>> = Vec::new();
                let writes = field_index(db, self.files)
                    .by_name
                    .get(name.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|spelled| spelled.kind == SpellingKind::Write);
                for spelled in writes {
                    if let Some(position) =
                        position_in_item(db, spelled.file, spelled.range.start())
                        && !items.contains(&position.item)
                    {
                        items.push(position.item);
                    }
                }
                self.written_items.insert(name, items.clone());
                items
            }
        };
        for other in written_items {
            let (Some(hir), Some(naming)) = (
                item_hir(db, other).as_ref(),
                item_naming(db, other).as_ref(),
            ) else {
                continue;
            };
            sites.extend(
                self.field_writes(other, name)
                    .iter()
                    .filter(|(written, _)| {
                        matches!(hir.kind(*written), ExpressionKind::NameRef(spelled) if *spelled == variable)
                            && naming.resolutions.get(written).is_none_or(|binding| {
                                naming
                                    .bindings
                                    .get(binding)
                                    .is_some_and(|info| info.kind == BindingKind::TopLevel)
                            })
                    })
                    .map(|(_, site)| *site),
            );
        }
    }

    /// The writes `receiver$name <- value` in `item`, as the written receiver and the site.
    fn field_writes(&mut self, item: Item<'db>, name: Symbol) -> &[(ExprId, Site<'db>)] {
        let db = self.db;
        self.field_writes.entry((item, name)).or_insert_with(|| {
            let Some(hir) = item_hir(db, item).as_ref() else {
                return Vec::new();
            };
            hir.expressions()
                .filter_map(|expression| match expression.kind {
                    ExpressionKind::Assign {
                        spelling:
                            AssignSpelling::Local | AssignSpelling::Equals | AssignSpelling::Super,
                        target,
                        value,
                    } => match hir.kind(*target) {
                        ExpressionKind::Field {
                            target: written,
                            name: Some(field),
                            at: false,
                        } if hir[*field].name == name => Some((
                            *written,
                            Site {
                                item,
                                range: hir.name_range(*field),
                                value: Some(*value),
                            },
                        )),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        })
    }
}

/// The values `binding` is assigned in its item, in source order.
fn assigned_values(
    hir: &Module,
    naming: &semantics::naming::ItemNaming,
    binding: semantics::naming::BindingId,
) -> Vec<ExprId> {
    hir.expressions()
        .filter_map(|expression| match expression.kind {
            ExpressionKind::Assign { target, value, .. }
                if naming.resolutions.get(target) == Some(&binding) =>
            {
                Some(*value)
            }
            _ => None,
        })
        .collect()
}

/// The smallest `R6Class(...)` call around `expression`, whose methods `self` and `private`
/// belong to.
fn enclosing_r6_class(hir: &Module, expression: ExprId) -> Option<ExprId> {
    let range = hir.range(expression);
    hir.expressions()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.range.contains_range(range)
                && matches!(candidate.kind, ExpressionKind::Call { callee, .. }
                    if callee_name(hir, *callee).is_some_and(|name| name.as_str() == R6_CLASS))
        })
        .min_by_key(|(_, candidate)| candidate.range.len())
        .map(|(index, _)| ExprId::new(index))
}

/// The values of the `return(value)` calls in `body`, not counting nested functions, which
/// return from themselves.
fn collect_returns(hir: &Module, expression: ExprId, returns: &mut Vec<ExprId>) {
    match hir.kind(expression) {
        ExpressionKind::Function { .. } => return,
        ExpressionKind::Call { callee, arguments }
            if callee_name(hir, *callee).is_some_and(|name| name.as_str() == "return") =>
        {
            if let Some(value) = hir[arguments].iter().find_map(|argument| argument.value) {
                returns.push(value);
            }
        }
        _ => {}
    }
    hir.for_each_child(expression, |child| collect_returns(hir, child, returns));
}
