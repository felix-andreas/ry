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
        hir::{
            AssignSpelling, BinaryOperator, ExprId, ExpressionKind, Module, Symbol, UnaryOperator,
        },
        inline_annotations_enabled, item_annotation_syntax, item_hir, item_syntax, package_name,
        stubs::stubs,
        types::{Name, QueryName},
    },
    rustc_hash::FxHashSet,
    syntax::{SyntaxKind, TextRange, native_values::object_system_function},
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
    Classifier { db, module }.statement(root)
}

/// Whether the item declares the function it binds pure, with `#: @pure` or an inline
/// `@pure function`. Trusted, not checked against the body: a body edit leaves the `bool`
/// unchanged, and salsa backdates it.
#[salsa::tracked]
pub fn item_declared_pure<'db>(db: &'db dyn Db, item: Item<'db>) -> bool {
    if item_annotation_syntax(db, item)
        .is_some_and(|syntax| crate::annotations::declares_pure(&syntax.syntax_node()))
    {
        return true;
    }
    let Some(syntax) = item_syntax(db, item) else {
        return false;
    };
    let root = syntax.syntax_node();
    let marked = root.kind() == SyntaxKind::BINARY_EXPR
        && root.children().any(|child| {
            child.kind() == SyntaxKind::FUNCTION_DEF
                && child
                    .children()
                    .any(|part| part.kind() == SyntaxKind::PURE_MARKER)
        });
    marked && inline_annotations_enabled(db)
}

/// Whether calling `package::name`, or a bare `name`, is declared to have no effect, and
/// where the declaration would live. A project definition shadows a stub's.
pub fn callee_purity(db: &dyn Db, package: Option<&str>, name: &str) -> (bool, CalleeOrigin) {
    if package.is_none() {
        let definitions = package_name(db, QueryName::new(db, Name::new(db, name.to_owned())));
        if !definitions.conditional_writers.is_empty() {
            return (false, CalleeOrigin::Conditional);
        }
        if let Some(winner) = definitions.winner {
            return (*item_declared_pure(db, winner), CalleeOrigin::Project);
        }
        if INTRINSIC_PURE.contains(&name) {
            return (true, CalleeOrigin::Other);
        }
    }
    let pure = stubs(db).is_some_and(|library| match package {
        Some(package) => library
            .pure_by_namespace
            .get(package)
            .is_some_and(|names| names.contains(name)),
        None => library.pure.contains(name),
    });
    (pure, CalleeOrigin::Other)
}

/// Recognized by the checker or the parser rather than declared by a stub.
const INTRINSIC_PURE: &[&str] = &["c", "list", "switch", "expression", "alist"];

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
}

impl Classifier<'_> {
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

    /// The value of `name <- value`, through a chain such as `a <- b <- value`.
    fn binding(&self, value: ExprId) -> TopLevel {
        match &self.module.expression(value).kind {
            ExpressionKind::Function { .. } => TopLevel::Definition,
            ExpressionKind::Assign {
                spelling: AssignSpelling::Local | AssignSpelling::Equals,
                target,
                value,
            } if self.binds_name(*target) => self.binding(*value),
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
    fn effect(&self, id: ExprId, locals: &mut Option<FxHashSet<Symbol>>) -> Option<TopLevel> {
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
                let local = package.is_none()
                    && locals
                        .as_ref()
                        .is_some_and(|names| names.contains(&Symbol::new(name)));
                if local {
                    return found(Effect::Call {
                        callee: name.to_owned(),
                        origin: CalleeOrigin::Local,
                    });
                }
                let (pure, origin) = callee_purity(self.db, package, name);
                if !pure {
                    return found(Effect::Call {
                        callee: name.to_owned(),
                        origin,
                    });
                }
                if QUOTING.contains(&name) {
                    return None;
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
                let (pure, origin) = callee_purity(self.db, None, name.as_str());
                if !pure {
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
                    (AssignSpelling::Destructure, Some(names)) => {
                        self.pattern_names(*target, names);
                        self.effect(*value, locals)
                    }
                    (AssignSpelling::Destructure, None) => {
                        found(Effect::NestedAssignment { target: None })
                    }
                    (AssignSpelling::Local | AssignSpelling::Equals, Some(names)) => {
                        match &self.module.expression(*target).kind {
                            ExpressionKind::NameRef(name) => {
                                names.insert(*name);
                            }
                            // A replacement changes a value only `local()` can see.
                            _ if target_name.is_some_and(|name| names.contains(&name)) => {}
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
                let Some(names) = locals.as_mut() else {
                    return found(Effect::Loop);
                };
                names.extend(*variable);
                self.children_effect(id, locals)
            }
            ExpressionKind::While { .. } | ExpressionKind::Repeat { .. } => {
                if locals.is_none() {
                    return found(Effect::Loop);
                }
                self.children_effect(id, locals)
            }
            ExpressionKind::Local { body } => {
                let mut inner = Some(locals.clone().unwrap_or_default());
                self.effect(*body, &mut inner)
            }
            _ => self.children_effect(id, locals),
        }
    }

    fn children_effect(
        &self,
        id: ExprId,
        locals: &mut Option<FxHashSet<Symbol>>,
    ) -> Option<TopLevel> {
        let mut found = None;
        self.module.for_each_child(id, |child| {
            if found.is_none() {
                found = self.effect(child, locals);
            }
        });
        found
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
        match &self.module.expression(target).kind {
            ExpressionKind::NameRef(name) => Some(*name),
            ExpressionKind::Index { target, .. } | ExpressionKind::Field { target, .. } => {
                self.root_name(*target)
            }
            ExpressionKind::Call { arguments, .. } => self.module[arguments]
                .first()
                .and_then(|argument| argument.value)
                .and_then(|value| self.root_name(value)),
            ExpressionKind::Paren(inner) => self.root_name(*inner),
            _ => None,
        }
    }
}
