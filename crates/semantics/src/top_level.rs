//! What a package file's top level runs when the package loads, and which functions are
//! declared pure.
//!
//! In strict mode a file in `R/` may hold only definitions at its top level, so loading the
//! package has no effect beyond binding names and registering classes. A constant counts as a
//! definition when its initializer calls only functions declared pure, which also lets the
//! loader bind it lazily. Purity is declared (`@pure`), never inferred, and each declaration is
//! a per-item `bool`, so editing a function's body never reaches the code that calls it.

use {
    crate::{
        Db, Item,
        check::{ArgumentShape, ArgumentTarget, argument_targets},
        hir::{
            Argument, AssignSpelling, BinaryOperator, ExprId, ExpressionKind, LiteralKind, Module,
            Span, Symbol, UnaryOperator,
        },
        inline_annotations_enabled, item_hir, item_syntax_flags,
        metadata::namespace_active,
        name_layer,
        naming::{base_masking_family, data_argument_flags},
        stubs::{QUALIFIED_ONLY_NAMESPACES, StubSources, stubs},
        types::TyKind,
    },
    bumpalo::Bump,
    rustc_hash::{FxHashMap, FxHashSet},
    std::{
        cell::{Cell, RefCell},
        rc::Rc,
    },
    syntax::{TextRange, native_values::object_system_function},
};

/// How one top-level statement of a package file behaves when the package loads.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum TopLevel {
    /// A function, a class definition, an import, a declaration, or a statement with no
    /// effect at all. It runs at load, and running it changes nothing but bindings and
    /// registrations.
    Definition,
    /// `name <- value` where evaluating `value` has no effect, so it may be bound lazily.
    Constant,
    /// `name <- R6Class(...)` built from literals, names, functions and `list()` or `c()` of
    /// them. The generator depends on nothing but those arguments and the namespace it is
    /// created in, so unlike a constant, whose pure functions may still read options or the
    /// locale, it may be bound lazily in any file.
    Generator,
    /// The statement has an effect, found at `range` (item-relative).
    Effect { range: TextRange, effect: Effect },
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum Effect {
    /// A call to a function that is not declared pure.
    Call {
        callee: String,
        origin: CalleeOrigin,
    },
    /// A call whose function is computed, so nothing can say whether it is pure.
    DynamicCall,
    /// A function passed to a parameter that `caller` calls (`lapply`'s `FUN`), when the
    /// function is not declared pure. `function` is `None` when the argument computes it.
    Callback {
        caller: String,
        function: Option<String>,
        origin: CalleeOrigin,
    },
    Loop,
    /// `<<-`, which assigns outside the package namespace.
    SuperAssignment,
    /// `names(x) <- value` or `x$field <- value`: the statement changes `x` after its
    /// definition.
    Replacement {
        target: Option<String>,
    },
    /// An assignment nested in an expression, which binds a name as a side effect.
    NestedAssignment {
        target: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub enum CalleeOrigin {
    /// Defined by the project, so marking it `@pure` would clear the finding.
    Project,
    /// Defined by the project only under a condition, so which definition runs is unknown.
    Conditional,
    /// Bound inside the enclosing `local()`.
    Local,
    /// Declared by a stub, or unknown.
    Other,
}

/// Item-relative and derived from the item's own syntax plus the per-item purity of the
/// functions it calls, so an edit inside another item's body never re-derives it.
#[salsa::tracked(returns(ref))]
pub fn item_top_level<'db>(db: &'db dyn Db, item: Item<'db>) -> TopLevel {
    let Some(module) = item_hir(db, item) else {
        return TopLevel::Definition;
    };
    let Some(root) = module.root else {
        return TopLevel::Definition;
    };
    Classifier::new(db, module).statement(root)
}

/// Whether running the item has no effect but bindings, registrations and the loading of other
/// packages, so a host may run it before the package's strict check finishes. A constant
/// or an R6 generator built from literals qualifies. A definition qualifies only when the arguments of its declaring calls have no
/// effect either: `setRefClass("A", methods = list(m = { cat("x"); function() 1 }))` is a
/// definition strict mode accepts, but its argument prints.
#[salsa::tracked]
pub fn item_effect_free<'db>(db: &'db dyn Db, item: Item<'db>) -> bool {
    match item_top_level(db, item) {
        TopLevel::Constant | TopLevel::Generator => true,
        TopLevel::Effect { .. } => false,
        TopLevel::Definition => {
            let Some(module) = item_hir(db, item) else {
                return true;
            };
            let Some(root) = module.root else {
                return true;
            };
            Classifier::new(db, module).inert(root)
        }
    }
}

/// Whether the item declares the function it binds pure, with `#: @pure` or an inline
/// `@pure function`. Trusted, not checked against the body, so a body edit leaves it unchanged.
pub fn item_declared_pure<'db>(db: &'db dyn Db, item: Item<'db>) -> bool {
    let flags = item_syntax_flags(db, item);
    flags.comment_pure || (flags.inline_pure && inline_annotations_enabled(db))
}

/// The names the item's `<<-` assignments write, wherever they sit. A package namespace is
/// locked once loading finishes, so a function body's `<<-` can reach a namespace binding
/// only while `.onLoad`, or something it calls, runs.
#[salsa::tracked(returns(ref))]
pub fn item_super_assigned_names<'db>(db: &'db dyn Db, item: Item<'db>) -> Vec<Symbol> {
    let Some(module) = item_hir(db, item) else {
        return Vec::new();
    };
    super_assigned_names(module)
}

/// The root names of every `<<-` target in `module`, sorted and without duplicates.
pub fn super_assigned_names(module: &Module) -> Vec<Symbol> {
    let mut names: Vec<Symbol> = module
        .kinds()
        .iter()
        .filter_map(|kind| match kind {
            ExpressionKind::Assign {
                spelling: AssignSpelling::Super,
                target,
                ..
            } => root_name(module, *target),
            _ => None,
        })
        .collect();
    names.sort_by_key(|name| name.as_str());
    names.dedup();
    names
}

/// The name a replacement target such as `x$a[[1]]` or `names(x)` changes.
fn root_name(module: &Module, target: ExprId) -> Option<Symbol> {
    match &module.expression(target).kind {
        ExpressionKind::NameRef(name) => Some(*name),
        ExpressionKind::Index { target, .. } | ExpressionKind::Field { target, .. } => {
            root_name(module, *target)
        }
        ExpressionKind::Call { arguments, .. } => module[arguments]
            .first()
            .and_then(|argument| argument.value)
            .and_then(|value| root_name(module, value)),
        ExpressionKind::Paren(inner) => root_name(module, *inner),
        _ => None,
    }
}

/// Whether the item is `name <- value` with `value` a literal, or `c()` or `list()` of such
/// values. Such a value is never an environment, so replacing in a copy of it leaves the
/// binding alone.
#[salsa::tracked]
pub fn item_binds_plain_value<'db>(db: &'db dyn Db, item: Item<'db>) -> bool {
    let Some(module) = item_hir(db, item) else {
        return false;
    };
    let Some(root) = module.root else {
        return false;
    };
    let ExpressionKind::Assign {
        spelling: AssignSpelling::Local | AssignSpelling::Equals,
        value,
        ..
    } = &module.expression(root).kind
    else {
        return false;
    };
    plain_value(module, *value)
}

fn plain_value(module: &Module, id: ExprId) -> bool {
    match &module.expression(id).kind {
        ExpressionKind::Literal(_) => true,
        ExpressionKind::Unary {
            operator: UnaryOperator::Minus,
            operand,
        }
        | ExpressionKind::Paren(operand) => plain_value(module, *operand),
        ExpressionKind::Call { callee, arguments } => {
            matches!(
                &module.expression(*callee).kind,
                ExpressionKind::NameRef(name) if matches!(name.as_str(), "c" | "list")
            ) && module[arguments].iter().all(|argument| {
                argument
                    .value
                    .is_some_and(|value| plain_value(module, value))
            })
        }
        _ => false,
    }
}

/// Whether `id` is what an R6 class definition is built from: literals, names, functions, and
/// `list()` or `c()` of them. The rule is syntactic, so a load that reads neither the stubs
/// nor the project's definitions classifies a generator the same way `ry check` does.
fn generator_part(module: &Module, id: ExprId) -> bool {
    match &module.expression(id).kind {
        ExpressionKind::Missing
        | ExpressionKind::Literal(_)
        | ExpressionKind::NameRef(_)
        | ExpressionKind::Namespace { .. }
        | ExpressionKind::Function { .. } => true,
        ExpressionKind::Unary { operand, .. } | ExpressionKind::Paren(operand) => {
            generator_part(module, *operand)
        }
        ExpressionKind::Call { callee, arguments } => {
            matches!(
                &module.expression(*callee).kind,
                ExpressionKind::NameRef(name) if matches!(name.as_str(), "c" | "list")
            ) && module[arguments].iter().all(|argument| {
                argument
                    .value
                    .is_none_or(|value| generator_part(module, value))
            })
        }
        _ => false,
    }
}

/// Whether calling `package::name`, or a bare `name`, is declared to have no effect, and
/// where the declaration would live. A project definition shadows a stub's. A pure function
/// comes with the parameters whose function arguments must be pure too (`@pure(FUN)`), empty
/// when it is pure whatever it is passed.
pub fn callee_purity<'db>(
    db: &'db dyn Db,
    package: Option<&str>,
    name: &str,
) -> (Option<&'db [String]>, CalleeOrigin) {
    if package.is_none() {
        let definitions = name_layer(db, Symbol::new(name), false);
        if !definitions.conditional_writers.is_empty() {
            return (None, CalleeOrigin::Conditional);
        }
        if let Some(winner) = definitions.winner {
            return (
                item_declared_pure(db, winner).then_some(&[]),
                CalleeOrigin::Project,
            );
        }
        if INTRINSIC_PURE.contains(&name) {
            return (Some(&[]), CalleeOrigin::Other);
        }
    }
    let pure = stubs(db)
        .zip(stub_namespace(db, package, name))
        .and_then(|(library, namespace)| {
            library
                .pure_by_namespace
                .get(namespace)
                .and_then(|names| names.get(name))
                .map(Vec::as_slice)
        });
    (pure, CalleeOrigin::Other)
}

/// The namespace whose stub declares what a call of `package::name`, or a bare `name`, calls.
/// A qualified call names it. A bare call resolves to the namespace whose declaration of the
/// name wins, which must be reachable without a qualifier, so a later package's undecorated
/// redeclaration of the name never clears another package's marker.
fn stub_namespace<'db>(db: &'db dyn Db, package: Option<&'db str>, name: &str) -> Option<&'db str> {
    if let Some(package) = package {
        return Some(package);
    }
    let (library, sources) = stubs(db).zip(StubSources::try_get(db))?;
    let declaration = library.declarations.get(name)?;
    let (namespace, _) = sources.sources(db).get(declaration.source_index)?;
    if QUALIFIED_ONLY_NAMESPACES.contains(&namespace.as_str()) && !namespace_active(db, namespace) {
        return None;
    }
    Some(namespace.as_str())
}

/// Recognized by the checker or the parser rather than declared by a stub, including the
/// operators a call names in backticks, such as `Reduce(\`+\`, values)`.
const INTRINSIC_PURE: &[&str] = &[
    "c",
    "list",
    "switch",
    "expression",
    "alist",
    "return",
    "+",
    "-",
    "*",
    "/",
    "^",
    "%%",
    "%/%",
    "==",
    "!=",
    "<",
    ">",
    "<=",
    ">=",
    "&",
    "|",
    "&&",
    "||",
    "!",
    "[",
    "[[",
    "$",
    "(",
    ":",
];

/// Calls that load or attach a package, which a package file may make at its top level.
const IMPORTS: &[&str] = &[
    "library",
    "require",
    "requireNamespace",
    "loadNamespace",
    "attachNamespace",
];

/// Calls that only declare something to R's tools or bind a name, without computing a value
/// at load.
const DECLARATIONS: &[&str] = &[
    "globalVariables",
    "suppressForeignCheck",
    "delayedAssign",
    "makeActiveBinding",
    ".S3method",
    "registerS3method",
];

/// Pure functions whose arguments R does not evaluate, so what they quote is never run.
const QUOTING: &[&str] = &["quote", "expression", "alist", "substitute"];

struct Classifier<'a> {
    db: &'a dyn Db,
    module: &'a Module,
    /// The local function literals whose call is being judged, so a recursive call is judged
    /// by the walk of its body already under way.
    active: RefCell<Vec<ExprId>>,
    /// How many more local calls may be judged by walking the callee's body. Each call walks
    /// it again, so a chain of functions calling each other twice would take exponential time;
    /// past the budget a call counts as an effect.
    call_budget: Cell<u32>,
}

/// What `local()` or a function body binds: assignments there touch only its environment.
#[derive(Clone, Default)]
struct LocalScope {
    /// Every name bound in this environment or an enclosing local one.
    names: FxHashSet<Symbol>,
    /// The visible names bound to a function literal, with the depth of the environment that
    /// bound it. Under lexical scoping a call runs the body against that environment as it
    /// stands at the call, so the call, not the binding, is judged.
    functions: FxHashMap<Symbol, (ExprId, u32)>,
    depth: u32,
    /// The enclosing environment as it stood when this one was entered. Code here cannot change
    /// it except through `<<-`, which is an effect of its own.
    outer: Option<Rc<LocalScope>>,
}

const LOCAL_CALL_BUDGET: u32 = 256;

/// A new environment enclosed by `scope`, or the outermost local one.
fn enter(scope: &Option<LocalScope>) -> LocalScope {
    match scope {
        Some(scope) => LocalScope {
            names: scope.names.clone(),
            functions: scope.functions.clone(),
            depth: scope.depth + 1,
            outer: Some(Rc::new(scope.clone())),
        },
        None => LocalScope::default(),
    }
}

/// Binds `name` in `scope` to something other than a function literal.
fn rebind(scope: &mut LocalScope, name: Symbol) {
    scope.names.insert(name);
    scope.functions.remove(&name);
}

/// The environment at `depth` that encloses `scope`, or `scope` itself.
fn ancestor(scope: &LocalScope, depth: u32) -> &LocalScope {
    let mut current = scope;
    while current.depth > depth
        && let Some(outer) = &current.outer
    {
        current = outer;
    }
    current
}

impl<'a> Classifier<'a> {
    fn new(db: &'a dyn Db, module: &'a Module) -> Self {
        Classifier {
            db,
            module,
            active: RefCell::new(Vec::new()),
            call_budget: Cell::new(LOCAL_CALL_BUDGET),
        }
    }

    fn statement(&self, id: ExprId) -> TopLevel {
        let expression = self.module.expression(id);
        match &expression.kind {
            ExpressionKind::Paren(inner) => self.statement(*inner),
            ExpressionKind::Assign {
                spelling: AssignSpelling::Local | AssignSpelling::Equals,
                target,
                value,
            } if self.binds_name(*target) => self.binding(*value),
            // zeallot's `c(a, b) %<-% value` binds names like `<-` does.
            ExpressionKind::Assign {
                spelling: AssignSpelling::Destructure,
                value,
                ..
            } => self.binding(*value),
            ExpressionKind::Call { callee, .. } if self.declares(*callee) => TopLevel::Definition,
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                // `if (requireNamespace("x", quietly = TRUE))` only loads a package, as a
                // top-level `requireNamespace()` may.
                if !self.imports(*condition)
                    && let Some(effect) = self.effect(*condition, &mut None)
                {
                    return effect;
                }
                for branch in std::iter::once(*then_branch).chain(*else_branch) {
                    if let effect @ TopLevel::Effect { .. } = self.statement(branch) {
                        return effect;
                    }
                }
                TopLevel::Definition
            }
            ExpressionKind::Block { statements } => {
                for &statement in &self.module[statements] {
                    if let effect @ TopLevel::Effect { .. } = self.statement(statement) {
                        return effect;
                    }
                }
                TopLevel::Definition
            }
            _ => self.effect(id, &mut None).unwrap_or(TopLevel::Definition),
        }
    }

    /// Whether a statement classified as a definition evaluates nothing with an effect, which
    /// `statement` leaves to R for the arguments of a declaring call.
    fn inert(&self, id: ExprId) -> bool {
        match &self.module.expression(id).kind {
            ExpressionKind::Paren(inner) => self.inert(*inner),
            ExpressionKind::Assign {
                spelling: AssignSpelling::Local | AssignSpelling::Equals,
                target,
                value,
            } if self.binds_name(*target) => self.inert(*value),
            ExpressionKind::Assign {
                spelling: AssignSpelling::Destructure,
                value,
                ..
            } => self.inert(*value),
            ExpressionKind::Call { callee, arguments } if self.declares(*callee) => self.module
                [arguments]
                .iter()
                .filter_map(|argument| argument.value)
                .all(|argument| self.effect(argument, &mut None).is_none()),
            ExpressionKind::If {
                then_branch,
                else_branch,
                ..
            } => std::iter::once(*then_branch)
                .chain(*else_branch)
                .all(|branch| self.inert(branch)),
            ExpressionKind::Block { statements } => self.module[statements]
                .iter()
                .all(|&statement| self.inert(statement)),
            _ => true,
        }
    }

    /// The value of `name <- value`, through a chain such as `a <- b <- value`.
    fn binding(&self, value: ExprId) -> TopLevel {
        match &self.module.expression(value).kind {
            ExpressionKind::Function { .. } => TopLevel::Definition,
            ExpressionKind::Assign {
                spelling: AssignSpelling::Local | AssignSpelling::Equals,
                target,
                value,
            } if self.binds_name(*target) => self.binding(*value),
            ExpressionKind::Call { callee, arguments }
                if self.callee_name(*callee).is_some_and(|(package, name)| {
                    name == "R6Class" && package.is_none_or(|package| package == "R6")
                }) =>
            {
                let built = self.module[arguments].iter().all(|argument| {
                    argument
                        .value
                        .is_none_or(|value| generator_part(self.module, value))
                });
                if built {
                    TopLevel::Generator
                } else {
                    TopLevel::Definition
                }
            }
            ExpressionKind::Call { callee, .. } if self.declares(*callee) => TopLevel::Definition,
            _ => self.effect(value, &mut None).unwrap_or(TopLevel::Constant),
        }
    }

    /// Whether `condition` is an import call, possibly negated.
    fn imports(&self, condition: ExprId) -> bool {
        match &self.module.expression(condition).kind {
            ExpressionKind::Call { callee, arguments } => {
                self.callee_name(*callee)
                    .is_some_and(|(_, name)| IMPORTS.contains(&name))
                    && self.module[arguments]
                        .iter()
                        .filter_map(|argument| argument.value)
                        .all(|argument| self.effect(argument, &mut None).is_none())
            }
            ExpressionKind::Unary {
                operator: UnaryOperator::Not,
                operand,
            }
            | ExpressionKind::Paren(operand) => self.imports(*operand),
            _ => false,
        }
    }

    fn binds_name(&self, target: ExprId) -> bool {
        matches!(
            &self.module.expression(target).kind,
            ExpressionKind::NameRef(_)
                | ExpressionKind::Literal(crate::hir::LiteralKind::String(_))
        )
    }

    /// A class definition, an import or a declaration, whose arguments are R's to judge.
    fn declares(&self, callee: ExprId) -> bool {
        let Some(name) = self.callee_name(callee).map(|(_, name)| name) else {
            return false;
        };
        object_system_function(name).is_some()
            || IMPORTS.contains(&name)
            || DECLARATIONS.contains(&name)
    }

    fn callee_name(&self, callee: ExprId) -> Option<(Option<&'static str>, &'static str)> {
        match &self.module.expression(callee).kind {
            ExpressionKind::NameRef(name) => Some((None, name.as_str())),
            ExpressionKind::Namespace {
                package: Some(package),
                name: Some(name),
                ..
            } => Some((Some(package.as_str()), name.as_str())),
            _ => None,
        }
    }

    /// The first effect evaluating `id` would have. `locals` is `Some` inside `local()`, where
    /// assignments and loops touch only the local environment; it holds the names bound there.
    fn effect(&self, id: ExprId, locals: &mut Option<LocalScope>) -> Option<TopLevel> {
        let expression = self.module.expression(id);
        let found = |effect: Effect| {
            Some(TopLevel::Effect {
                range: expression.range,
                effect,
            })
        };
        match &expression.kind {
            ExpressionKind::Missing
            | ExpressionKind::Literal(_)
            | ExpressionKind::NameRef(_)
            | ExpressionKind::Namespace { .. }
            | ExpressionKind::Function { .. }
            | ExpressionKind::Break
            | ExpressionKind::Next => None,
            ExpressionKind::Call { callee, arguments } => {
                let Some((package, name)) = self.callee_name(*callee) else {
                    return found(Effect::DynamicCall);
                };
                let symbol = Symbol::new(name);
                let local = locals
                    .as_ref()
                    .filter(|scope| package.is_none() && scope.names.contains(&symbol));
                let callbacks = match local {
                    Some(scope) if !self.local_call_has_effect(symbol, scope) => &[][..],
                    Some(_) => {
                        return found(Effect::Call {
                            callee: name.to_owned(),
                            origin: CalleeOrigin::Local,
                        });
                    }
                    None => match callee_purity(self.db, package, name) {
                        (Some(callbacks), _) => callbacks,
                        (None, origin) => {
                            return found(Effect::Call {
                                callee: name.to_owned(),
                                origin,
                            });
                        }
                    },
                };
                if QUOTING.contains(&name) {
                    return None;
                }
                if !callbacks.is_empty()
                    && let Some(effect) = self.callback_effect(
                        expression.range,
                        package,
                        name,
                        callbacks,
                        *arguments,
                        locals,
                    )
                {
                    return Some(effect);
                }
                // A masked argument runs in an environment made from the data, so its
                // assignments touch only that environment, as inside `local()`.
                if let Some(leading) = base_masking_family(name)
                    .filter(|_| package.is_none_or(|package| package == "base"))
                {
                    let flags = data_argument_flags(self.module, &self.module[arguments], &leading);
                    return self.module[arguments].iter().zip(flags).find_map(
                        |(argument, data_argument)| {
                            let value = argument.value?;
                            if data_argument {
                                self.effect(value, locals)
                            } else {
                                self.effect(value, &mut Some(enter(locals)))
                            }
                        },
                    );
                }
                self.module[arguments]
                    .iter()
                    .filter_map(|argument| argument.value)
                    .find_map(|argument| self.effect(argument, locals))
            }
            ExpressionKind::Binary {
                operator: BinaryOperator::Special,
                special_name: Some(name),
                lhs,
                rhs,
            } => {
                let (purity, origin) = callee_purity(self.db, None, name.as_str());
                if purity.is_none_or(|callbacks| !callbacks.is_empty()) {
                    return found(Effect::Call {
                        callee: name.as_str().to_owned(),
                        origin,
                    });
                }
                self.effect(*lhs, locals)
                    .or_else(|| self.effect(*rhs, locals))
            }
            // A formula is data: R does not evaluate its sides.
            ExpressionKind::Binary {
                operator: BinaryOperator::Tilde,
                ..
            }
            | ExpressionKind::Unary {
                operator: UnaryOperator::Tilde,
                ..
            } => None,
            ExpressionKind::Binary {
                operator: BinaryOperator::Help,
                ..
            }
            | ExpressionKind::Unary {
                operator: UnaryOperator::Help,
                ..
            } => found(Effect::Call {
                callee: "?".to_owned(),
                origin: CalleeOrigin::Other,
            }),
            ExpressionKind::Assign {
                spelling,
                target,
                value,
            } => {
                let target_name = self.root_name(*target);
                match (spelling, locals.as_mut()) {
                    (AssignSpelling::Super, _) => found(Effect::SuperAssignment),
                    (AssignSpelling::Destructure, Some(scope)) => {
                        let mut bound = FxHashSet::default();
                        self.pattern_names(*target, &mut bound);
                        for name in bound {
                            rebind(scope, name);
                        }
                        self.effect(*value, locals)
                    }
                    (AssignSpelling::Destructure, None) => {
                        found(Effect::NestedAssignment { target: None })
                    }
                    (AssignSpelling::Local | AssignSpelling::Equals, Some(scope)) => {
                        match &self.module.expression(*target).kind {
                            ExpressionKind::NameRef(name) => {
                                if matches!(
                                    self.module.expression(*value).kind,
                                    ExpressionKind::Function { .. }
                                ) {
                                    scope.names.insert(*name);
                                    scope.functions.insert(*name, (*value, scope.depth));
                                } else {
                                    rebind(scope, *name);
                                }
                            }
                            // A replacement changes a value only `local()` can see, and copies
                            // it into this environment first, so `body(f) <- ...` binds a new
                            // `f` here.
                            _ if let Some(name) = target_name
                                && scope.names.contains(&name) =>
                            {
                                rebind(scope, name);
                            }
                            // R copies an outer vector or list into `local()`'s environment
                            // before replacing in it, so only an environment would change.
                            _ if let Some(name) = target_name
                                && self.outer_value_is_copied(name) =>
                            {
                                rebind(scope, name);
                            }
                            _ => {
                                return found(Effect::Replacement {
                                    target: target_name.map(|name| name.as_str().to_owned()),
                                });
                            }
                        }
                        self.effect(*value, locals)
                    }
                    _ if !self.binds_name(*target) => found(Effect::Replacement {
                        target: target_name.map(|name| name.as_str().to_owned()),
                    }),
                    _ => found(Effect::NestedAssignment {
                        target: target_name.map(|name| name.as_str().to_owned()),
                    }),
                }
            }
            ExpressionKind::For { variable, .. } => {
                let Some(scope) = locals.as_mut() else {
                    return found(Effect::Loop);
                };
                if let Some(variable) = variable {
                    rebind(scope, self.module.name(*variable));
                }
                self.children_effect(id, locals)
            }
            ExpressionKind::While { .. } | ExpressionKind::Repeat { .. } => {
                if locals.is_none() {
                    return found(Effect::Loop);
                }
                self.children_effect(id, locals)
            }
            ExpressionKind::Local { body } => self.effect(*body, &mut Some(enter(locals))),
            _ => self.children_effect(id, locals),
        }
    }

    /// The first effect of calling the function literal `function` from the environment
    /// `locals`: its formals and assignments bind in a new one, where R also evaluates the
    /// defaults. `None` when `function` is not a literal or has none.
    fn function_effect(&self, function: ExprId, locals: &Option<LocalScope>) -> Option<TopLevel> {
        let ExpressionKind::Function { parameters, body } = &self.module.expression(function).kind
        else {
            return None;
        };
        let mut scope = Some(enter(locals));
        if let Some(scope) = scope.as_mut() {
            for parameter in &self.module[parameters] {
                rebind(scope, parameter.name);
            }
        }
        self.module[parameters]
            .iter()
            .filter_map(|parameter| parameter.default)
            .find_map(|default| self.effect(default, &mut scope))
            .or_else(|| self.effect(*body, &mut scope))
    }

    /// Whether calling the local name `name` here may have an effect: it is bound to something
    /// other than a function literal, or the literal's body has one in the environment that
    /// bound it, as that environment stands now.
    fn local_call_has_effect(&self, name: Symbol, scope: &LocalScope) -> bool {
        let Some(&(function, depth)) = scope.functions.get(&name) else {
            return true;
        };
        if self.active.borrow().contains(&function) {
            return false;
        }
        let Some(budget) = self.call_budget.get().checked_sub(1) else {
            return true;
        };
        self.call_budget.set(budget);
        self.active.borrow_mut().push(function);
        let effect = self.function_effect(function, &Some(ancestor(scope, depth).clone()));
        self.active.borrow_mut().pop();
        effect.is_some()
    }

    /// The first effect of the functions a call of `package::caller` is passed at the parameters
    /// that `@pure(...)` names, which the caller runs. The arguments are matched against the
    /// winning stub's parameters, so when that stub comes from another namespace than the
    /// purity, or declares no function, the call itself counts as the effect.
    fn callback_effect(
        &self,
        range: TextRange,
        package: Option<&str>,
        caller: &str,
        callbacks: &[String],
        arguments: Span<Argument>,
        locals: &mut Option<LocalScope>,
    ) -> Option<TopLevel> {
        let namespace = stub_namespace(self.db, package, caller);
        let function = stubs(self.db)
            .filter(|_| package.is_none() || stub_namespace(self.db, None, caller) == namespace)
            .and_then(|library| library.schemes.get(caller)?.first())
            .and_then(|scheme| match scheme.body.kind(self.db) {
                TyKind::Function(function) => Some(function),
                _ => None,
            });
        let Some(function) = function else {
            return Some(TopLevel::Effect {
                range,
                effect: Effect::Call {
                    callee: caller.to_owned(),
                    origin: CalleeOrigin::Other,
                },
            });
        };
        let arguments = &self.module[arguments];
        let arena = Bump::new();
        let shapes = arena.alloc_slice_fill_iter(arguments.iter().map(|argument| ArgumentShape {
            name: self.module.argument_name(argument).map(Symbol::as_str),
            forwards_dots: false,
        }));
        let targets = argument_targets(self.db, &arena, function, shapes);
        // purrr reads a string or a number passed as `.f` as an element to extract.
        let extracts = namespace == Some("purrr");
        arguments
            .iter()
            .zip(targets.iter())
            .filter(|(_, target)| {
                matches!(target, Some(ArgumentTarget::Named(index))
                if function.named.get(*index).is_some_and(|parameter| {
                    callbacks.iter().any(|callback| callback == parameter.name.text(self.db))
                }))
            })
            .filter_map(|(argument, _)| argument.value)
            .find_map(|value| self.callable_effect(caller, value, extracts, locals))
    }

    /// The first effect of calling `value`, a function a call of `caller` is passed.
    fn callable_effect(
        &self,
        caller: &str,
        value: ExprId,
        extracts: bool,
        locals: &mut Option<LocalScope>,
    ) -> Option<TopLevel> {
        let expression = self.module.expression(value);
        let callback = |function: Option<&str>, origin: CalleeOrigin| {
            Some(TopLevel::Effect {
                range: expression.range,
                effect: Effect::Callback {
                    caller: caller.to_owned(),
                    function: function.map(str::to_owned),
                    origin,
                },
            })
        };
        let named = |package: Option<&str>, name: &str| match callee_purity(self.db, package, name)
        {
            (Some([]), _) => None,
            (_, origin) => callback(Some(name), origin),
        };
        match &expression.kind {
            ExpressionKind::Function { .. } => self.function_effect(value, locals),
            ExpressionKind::Paren(inner) => self.callable_effect(caller, *inner, extracts, locals),
            ExpressionKind::NameRef(name) => {
                match locals.as_ref().filter(|scope| scope.names.contains(name)) {
                    Some(scope) if !self.local_call_has_effect(*name, scope) => None,
                    Some(_) => callback(Some(name.as_str()), CalleeOrigin::Local),
                    None => named(None, name.as_str()),
                }
            }
            ExpressionKind::Namespace {
                package: Some(package),
                name: Some(name),
                ..
            } => named(Some(package.as_str()), name.as_str()),
            ExpressionKind::Literal(LiteralKind::String(_)) if extracts => None,
            ExpressionKind::Literal(LiteralKind::String(text)) => named(None, &self.module[*text]),
            ExpressionKind::Literal(_) if extracts => None,
            // purrr's `~ .x * 2` is a function whose body is the formula's right side.
            ExpressionKind::Unary {
                operator: UnaryOperator::Tilde,
                operand,
            } if extracts => self.effect(*operand, &mut Some(enter(locals))),
            _ => callback(None, CalleeOrigin::Other),
        }
    }

    fn children_effect(&self, id: ExprId, locals: &mut Option<LocalScope>) -> Option<TopLevel> {
        let mut found = None;
        self.module.for_each_child(id, |child| {
            if found.is_none() {
                found = self.effect(child, locals);
            }
        });
        found
    }

    /// Whether the project binds `name` once, unconditionally, to a value that is plainly a
    /// vector or a list.
    fn outer_value_is_copied(&self, name: Symbol) -> bool {
        let definitions = name_layer(self.db, name, false);
        definitions.conditional_writers.is_empty()
            && definitions
                .winner
                .is_some_and(|winner| *item_binds_plain_value(self.db, winner))
    }

    /// Adds the names a zeallot pattern such as `c(a, c(b, ...rest))` binds.
    fn pattern_names(&self, pattern: ExprId, names: &mut FxHashSet<Symbol>) {
        match &self.module.expression(pattern).kind {
            ExpressionKind::NameRef(name) => {
                names.insert(*name);
            }
            ExpressionKind::Call { arguments, .. } => {
                for value in self.module[arguments]
                    .iter()
                    .filter_map(|argument| argument.value)
                {
                    self.pattern_names(value, names);
                }
            }
            _ => {}
        }
    }

    /// The variable an assignment target writes: `x` in `x`, `names(x)`, `x$a[[1]]`.
    fn root_name(&self, target: ExprId) -> Option<Symbol> {
        root_name(self.module, target)
    }
}
