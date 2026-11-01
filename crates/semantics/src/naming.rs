//! The R variable model: mutable slots with reaching-write flow. A read resolves to a
//! slot only when a write can reach it; an unassignable slot does not shadow, so lookup
//! falls outward exactly like R's runtime.

use {
    crate::{
        hir::{
            Argument, AssignSpelling, ExprId, ExpressionKind, LiteralKind, MatchArm, Module,
            NameSite, PatId, PatternKind, Symbol, UnaryOperator, destructure_leaves,
        },
        sorted::{SortedMap, SortedSet},
    },
    rustc_hash::FxHashMap,
    std::{
        collections::{BTreeMap, BTreeSet, btree_map::Entry},
        mem,
    },
    syntax::TextRange,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingInfo {
    pub name: Symbol,
    /// The first write's site.
    pub range: TextRange,
    pub kind: BindingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    TopLevel,
    Local,
    Parameter,
    ForVariable,
    /// A name a `match` arm's pattern binds.
    PatternVariable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusedAssignment {
    pub name: Symbol,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRead {
    pub internal: bool,
    pub package: Symbol,
    pub name: Option<Symbol>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemNaming {
    pub bindings: SortedMap<BindingId, BindingInfo>,
    pub resolutions: SortedMap<ExprId, BindingId>,
    pub maybe_undefined: SortedSet<ExprId>,
    pub non_locals: SortedMap<ExprId, Symbol>,
    /// The subset of `non_locals` read inside a nested function, which runs when called.
    pub deferred_non_locals: SortedSet<ExprId>,
    /// Unresolved reads in a quiet context (data masking, opaque operators): never reported,
    /// but they keep cross-item bindings alive, since masked code falls back to enclosing ones.
    pub quiet_reads: SortedMap<ExprId, Symbol>,
    pub deferred_quiet_reads: SortedSet<ExprId>,
    /// `%op%` names read as operators; by name because the operator is a token, not a `NameRef`.
    pub quiet_operator_reads: SortedSet<Symbol>,
    pub unused_assignments: Box<[UnusedAssignment]>,
    pub unused_parameters: Box<[UnusedAssignment]>,
    /// Own top-level slots read within the item; the cross-item unused check seeds them as used.
    pub used_top_level_names: SortedSet<Symbol>,
    /// Slots read or super-assigned from a nested function: every write stays observable.
    pub captured_slots: SortedSet<BindingId>,
    /// data.table `[`s: indexes evaluate in the data's frame, and the bracket types `Unknown`.
    pub masked_subsets: SortedSet<ExprId>,
    /// `f[, y := v]` statements whose subject the current scope owns: ry models the update as
    /// a new write of that slot, so later reads see the added or removed column.
    pub column_updates: SortedMap<ExprId, BindingId>,
    /// Qualified reads outside quiet contexts.
    pub namespace_reads: SortedMap<ExprId, NamespaceRead>,
    /// Names R creates in the global environment as a side effect: `<<-` targets with no
    /// enclosing binding, and the variables of top-level `for` loops.
    pub super_globals: SortedSet<Symbol>,
    /// Targets R parses but refuses at run time (a computed value or a number); reads inside
    /// them are suppressed as consequences of the mistake.
    pub invalid_assignment_targets: Box<[InvalidAssignmentTarget]>,
    /// Alternatives of a `p | q` pattern that leave out a name another alternative binds.
    pub or_pattern_mismatches: Box<[OrPatternMismatch]>,
    /// Call heads that resolved to a slot. R looks a called name up as a function, skipping
    /// every binding that does not hold one, so the checker may fall through to these.
    pub callee_fallbacks: SortedMap<ExprId, CalleeFallback>,
    /// Bare names read as the routine of `.Call` and the other native interfaces, which a
    /// registering `useDynLib` may bind.
    pub native_routine_reads: SortedSet<ExprId>,
}

/// Where a call head's lookup continues when its nearest binding cannot hold a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalleeFallback {
    /// The enclosing slots of the same name, nearest first.
    pub outer: Box<[BindingId]>,
    /// Whether the call runs inside a function, so a cross-item lookup sees the final binding.
    pub deferred: bool,
}

/// `ItemNaming` while resolution builds it; frozen into compact sorted slices at the end.
#[derive(Default)]
struct NamingDraft {
    bindings: BTreeMap<BindingId, BindingInfo>,
    resolutions: BTreeMap<ExprId, BindingId>,
    maybe_undefined: BTreeSet<ExprId>,
    non_locals: BTreeMap<ExprId, Symbol>,
    deferred_non_locals: BTreeSet<ExprId>,
    quiet_reads: BTreeMap<ExprId, Symbol>,
    deferred_quiet_reads: BTreeSet<ExprId>,
    quiet_operator_reads: BTreeSet<Symbol>,
    unused_assignments: Vec<UnusedAssignment>,
    unused_parameters: Vec<UnusedAssignment>,
    used_top_level_names: BTreeSet<Symbol>,
    captured_slots: BTreeSet<BindingId>,
    masked_subsets: BTreeSet<ExprId>,
    column_updates: BTreeMap<ExprId, BindingId>,
    namespace_reads: BTreeMap<ExprId, NamespaceRead>,
    super_globals: BTreeSet<Symbol>,
    invalid_assignment_targets: Vec<InvalidAssignmentTarget>,
    or_pattern_mismatches: Vec<OrPatternMismatch>,
    callee_fallbacks: BTreeMap<ExprId, CalleeFallback>,
    native_routine_reads: BTreeSet<ExprId>,
}

impl NamingDraft {
    fn freeze(self) -> ItemNaming {
        ItemNaming {
            bindings: self.bindings.into(),
            resolutions: self.resolutions.into(),
            maybe_undefined: self.maybe_undefined.into(),
            non_locals: self.non_locals.into(),
            deferred_non_locals: self.deferred_non_locals.into(),
            quiet_reads: self.quiet_reads.into(),
            deferred_quiet_reads: self.deferred_quiet_reads.into(),
            quiet_operator_reads: self.quiet_operator_reads.into(),
            unused_assignments: self.unused_assignments.into_boxed_slice(),
            unused_parameters: self.unused_parameters.into_boxed_slice(),
            used_top_level_names: self.used_top_level_names.into(),
            captured_slots: self.captured_slots.into(),
            masked_subsets: self.masked_subsets.into(),
            column_updates: self.column_updates.into(),
            namespace_reads: self.namespace_reads.into(),
            super_globals: self.super_globals.into(),
            invalid_assignment_targets: self.invalid_assignment_targets.into_boxed_slice(),
            or_pattern_mismatches: self.or_pattern_mismatches.into_boxed_slice(),
            callee_fallbacks: self.callee_fallbacks.into(),
            native_routine_reads: self.native_routine_reads.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrPatternMismatch {
    pub name: Symbol,
    /// The alternative that does not bind `name`.
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidAssignmentTarget {
    pub range: TextRange,
    /// The source between a computed target's operands; a line break after the operator
    /// means the previous line dangled, which the message names.
    pub operand_gap: Option<TextRange>,
}

/// `masked_verbs` maps each stub `@masked` verb to its formals before `...` (the data
/// arguments, resolved normally); the rest stay quiet. An empty list masks every argument.
pub fn resolve_item(module: &Module, masked_verbs: &FxHashMap<String, Vec<String>>) -> ItemNaming {
    let mut context = Context {
        module,
        masked_verbs,
        next_binding: 0,
        scopes: vec![Scope {
            id: 0,
            kind: ScopeKind::TopLevel,
            slots: FxHashMap::default(),
        }],
        bindings_by_site: FxHashMap::default(),
        scope_ids: FxHashMap::default(),
        flow: FlowState::new(),
        loop_exits: Vec::new(),
        writes: Vec::new(),
        write_by_expression: FxHashMap::default(),
        read_parameter_slots: BTreeSet::new(),
        emit: true,
        quiet_depth: 0,
        deferred_depth: 0,
        quoted_depth: 0,
        bracket_depth: 0,
        naming: NamingDraft::default(),
    };
    if let Some(root) = module.root {
        context.resolve(root);
    }
    context.collect_unused();
    context.naming.freeze()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    /// Executes when called, so cross-frame reads from below are captures.
    Function,
    /// `local({...})`: writes vanish with it, but it runs immediately, so it is no capture
    /// boundary.
    Local,
    TopLevel,
}

struct Scope {
    /// Stable per defining expression; capture liveness marks writes of this frame only,
    /// since a shadowed outer binding is not what the closure reads.
    id: u32,
    kind: ScopeKind,
    slots: FxHashMap<Symbol, BindingId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    Unassigned,
    /// A write with no reportable site (parameter, for-variable).
    Implicit,
    /// Index into `Context::writes`.
    Assignment(u32),
}

type FlowState = BTreeMap<BindingId, BTreeSet<Reach>>;

struct AssignmentWrite {
    name: Symbol,
    range: TextRange,
    scope: u32,
    used: bool,
    reportable: bool,
}

struct Context<'a> {
    module: &'a Module,
    masked_verbs: &'a FxHashMap<String, Vec<String>>,
    next_binding: u32,
    scopes: Vec<Scope>,
    /// Loop re-walks must mint no new ids: one binding per (site, name).
    bindings_by_site: FxHashMap<(u32, u32, Symbol), BindingId>,
    scope_ids: FxHashMap<ExprId, u32>,
    flow: FlowState,
    writes: Vec<AssignmentWrite>,
    write_by_expression: FxHashMap<ExprId, u32>,
    read_parameter_slots: BTreeSet<BindingId>,
    /// Diagnostic-bearing sets are recorded only on a loop region's final walk.
    emit: bool,
    /// Flow state at each `break` of the innermost loop. A loop that cannot be skipped exits
    /// only through these; joining the loop head would keep first-iteration paths alive.
    loop_exits: Vec<FlowState>,
    quiet_depth: u32,
    /// Inside an expression R evaluates later (`on.exit`): its reads see the final value, so
    /// they keep every write of the name alive.
    deferred_depth: u32,
    /// Inside a quoting form (`quote`, `substitute`, `bquote`, `expression`): assignments bind
    /// nothing, but reads are still walked because `eval` may run the expression later.
    quoted_depth: u32,
    /// Inside a single `[`'s index arguments, where data.table reads `..name` as the variable
    /// `name` from the enclosing scope.
    bracket_depth: u32,
    naming: NamingDraft,
}

impl Context<'_> {
    fn stable_scope_id(&mut self, expression: ExprId) -> u32 {
        let next = self.scope_ids.len() as u32 + 1;
        *self.scope_ids.entry(expression).or_insert(next)
    }

    fn mint_binding(&mut self, name: Symbol, range: TextRange, kind: BindingKind) -> BindingId {
        let key = (u32::from(range.start()), u32::from(range.end()), name);
        if let Some(&existing) = self.bindings_by_site.get(&key) {
            return existing;
        }
        let id = BindingId(self.next_binding);
        self.next_binding += 1;
        self.bindings_by_site.insert(key, id);
        self.naming
            .bindings
            .insert(id, BindingInfo { name, range, kind });
        id
    }

    fn current_function_depth(&self) -> usize {
        self.scopes
            .iter()
            .rposition(|scope| scope.kind == ScopeKind::Function)
            .unwrap_or(0)
    }

    fn record_write(&mut self, slot: BindingId, reach: Reach) {
        let set = self.flow.entry(slot).or_default();
        set.clear();
        set.insert(reach);
    }

    fn current_scope(&mut self) -> &mut Scope {
        self.scopes.last_mut().expect("scope stack is never empty")
    }

    /// The current frame's slot for `name`, minted at this site if the frame has none yet.
    /// A frame's `:=` is copy-on-write, which ry models as a new write of the subject's slot,
    /// so later reads see the updated columns. It writes only a slot the current scope already
    /// owns (a top-level name, a local, or a parameter): on any other subject, such as a global
    /// `data.table` updated inside a function, the update works by reference, so it must not
    /// shadow the subject with a local binding that would capture navigation and rename. The
    /// target's own read keeps its resolution, since it reads the value before the update.
    fn record_column_update(&mut self, id: ExprId, target: ExprId) {
        let module = self.module;
        let ExpressionKind::NameRef(name) = module.kind(target) else {
            return;
        };
        if self.quoted_depth != 0 {
            return;
        }
        let owned = match self.current_scope().kind {
            ScopeKind::TopLevel => true,
            ScopeKind::Function | ScopeKind::Local => self.current_scope().slots.contains_key(name),
        };
        if owned {
            let range = module.range(target);
            let kind = self.assignment_binding_kind();
            let slot = self.frame_slot(*name, range, kind);
            self.record_write_site(id, *name, range, slot, false);
            self.naming.column_updates.insert(id, slot);
        }
    }

    fn frame_slot(&mut self, name: Symbol, range: TextRange, kind: BindingKind) -> BindingId {
        if let Some(&slot) = self.current_scope().slots.get(&name) {
            return slot;
        }
        let slot = self.mint_binding(name, range, kind);
        self.current_scope().slots.insert(name, slot);
        slot
    }

    fn assignment_binding_kind(&mut self) -> BindingKind {
        match self.current_scope().kind {
            ScopeKind::TopLevel => BindingKind::TopLevel,
            ScopeKind::Function | ScopeKind::Local => BindingKind::Local,
        }
    }

    fn resolve(&mut self, id: ExprId) {
        let module = self.module;
        match module.kind(id) {
            ExpressionKind::Missing | ExpressionKind::Next => {}
            ExpressionKind::Break => self.loop_exits.push(self.flow.clone()),
            ExpressionKind::Literal(_) => {}
            ExpressionKind::NameRef(name) => self.resolve_read(id, *name),
            ExpressionKind::Assign {
                spelling,
                target,
                value,
            } => {
                // Value first: `x <- x + 1` reads the previous state. A replacement target goes
                // first, so in `self$i <- self$i + 1` the value's reads land on the fresh binding.
                if *spelling == AssignSpelling::Destructure
                    || matches!(module.kind(*target), ExpressionKind::NameRef(_))
                {
                    self.resolve(*value);
                    self.resolve_assignment_target(id, *target, *spelling);
                } else {
                    self.resolve_assignment_target(id, *target, *spelling);
                    self.resolve(*value);
                }
            }
            ExpressionKind::Unary { operand, operator } => {
                // A formula quotes its operand: names inside are model syntax.
                if *operator != UnaryOperator::Tilde && *operator != UnaryOperator::Help {
                    self.resolve(*operand);
                }
            }
            ExpressionKind::Binary {
                operator,
                special_name,
                lhs,
                rhs,
            } => {
                use crate::hir::BinaryOperator;
                // `a %op% b` calls `%op%`: a quiet read that keeps a project definition alive.
                if let Some(name) = special_name {
                    self.naming.quiet_operator_reads.insert(*name);
                    // A local `%op%` needs its slot marked too, or it reads as a dead store.
                    self.mark_slot_read(*name);
                }
                match operator {
                    BinaryOperator::Tilde | BinaryOperator::Help => {}
                    // Operands of unmodeled operators still resolve (for IDE navigation), but
                    // unresolved reads stay quiet: the construct is opaque to the checker.
                    BinaryOperator::Pipe | BinaryOperator::Special => {
                        self.quiet_depth += 1;
                        self.resolve(*lhs);
                        self.resolve(*rhs);
                        self.quiet_depth -= 1;
                    }
                    _ => {
                        self.resolve(*lhs);
                        self.resolve(*rhs);
                    }
                }
            }
            ExpressionKind::Call { callee, arguments } => {
                self.resolve(*callee);
                let callee_symbol = match module.kind(*callee) {
                    ExpressionKind::NameRef(name) => Some(*name),
                    _ => None,
                };
                if let Some(name) = callee_symbol
                    && let Some(&slot) = self.naming.resolutions.get(callee)
                {
                    self.record_callee_fallback(*callee, name, slot);
                }
                let callee_name = callee_symbol.map(Symbol::as_str);
                // `library(pkg)`, `require(pkg)` and `help(topic)` quote a bare first positional
                // name. The rule is syntactic: a rebound `library` does not change it.
                if matches!(callee_name, Some("library" | "require" | "help"))
                    && let Some(first) = module[arguments].first()
                    && first.name.is_none()
                    && let Some(value) = first.value
                    && matches!(module.kind(value), ExpressionKind::NameRef(_))
                {
                    self.resolve_arguments(&module[arguments][1..]);
                    return;
                }
                // The forms below are recognized by name; a local function of that name is an
                // ordinary call.
                let builtin_callee =
                    callee_name.filter(|_| !self.naming.resolutions.contains_key(callee));
                // `match.fun` looks a string up as a function name, so `vapply(xs, "g", ...)`
                // reads `g`.
                if builtin_callee.is_some_and(|name| MATCH_FUN_CALLERS.contains(&name)) {
                    for value in module[arguments]
                        .iter()
                        .filter_map(|argument| argument.value)
                    {
                        if let ExpressionKind::Literal(LiteralKind::String(text)) =
                            module.kind(value)
                        {
                            self.resolve_function_name_string(value, Symbol::new(&module[text]));
                        }
                    }
                }
                if builtin_callee.is_some_and(|name| NATIVE_INTERFACES.contains(&name))
                    && let Some(routine) = module[arguments]
                        .iter()
                        .find(|argument| module.argument_name(argument).as_deref() == Some(".NAME"))
                        .or_else(|| {
                            module[arguments]
                                .iter()
                                .find(|argument| argument.name.is_none())
                        })
                        .and_then(|argument| argument.value)
                    && matches!(module.kind(routine), ExpressionKind::NameRef(_))
                {
                    self.naming.native_routine_reads.insert(routine);
                }
                match builtin_callee {
                    // `on.exit(expr)` runs `expr` at function exit, so its reads observe the last
                    // value and keep every write of those names alive.
                    Some("on.exit") => {
                        self.deferred_depth += 1;
                        self.resolve_arguments(&module[arguments]);
                        self.deferred_depth -= 1;
                        return;
                    }
                    // `switch` runs exactly one alternative: its branches fork and join like `if`
                    // arms.
                    Some("switch") if !arguments.is_empty() => {
                        self.resolve_switch(&module[arguments]);
                        return;
                    }
                    // data.table's `setnames(f, ...)` and `setcolorder(f, ...)` change the
                    // frame's columns by reference, which ry models like `:=`: a new write of
                    // the subject's slot. `write_tokens` screens items for these names, so a
                    // new by-reference writer belongs there too.
                    Some("setnames" | "setcolorder") if !arguments.is_empty() => {
                        self.resolve_arguments(&module[arguments]);
                        if let Some(subject) = module[arguments][0]
                            .value
                            .filter(|_| module[arguments][0].name.is_none())
                        {
                            self.record_column_update(id, subject);
                        }
                        return;
                    }
                    // A quoting form binds nothing inside it. Reads are still walked, as deferred,
                    // because `eval` may run the expression later. Quiet too: a quoted name need
                    // not exist yet, as in `quote(x <- 1)`.
                    Some("quote" | "substitute" | "bquote" | "expression") => {
                        self.quoted_depth += 1;
                        self.deferred_depth += 1;
                        self.quiet_depth += 1;
                        self.resolve_arguments(&module[arguments]);
                        self.quiet_depth -= 1;
                        self.deferred_depth -= 1;
                        self.quoted_depth -= 1;
                        return;
                    }
                    _ => {}
                }
                // A masking call evaluates its `...` arguments in the data's frame; arguments
                // matching the data formals resolve normally.
                let masking_family = match (module.kind(*callee), builtin_callee) {
                    (_, Some(name)) => base_masking_family(name)
                        .or_else(|| declared_masked_verb(self.masked_verbs, name)),
                    // Only `base::` names the base family; another package's `with` is its own.
                    (
                        ExpressionKind::Namespace {
                            package,
                            name: Some(name),
                            ..
                        },
                        None,
                    ) => base_masking_family(name)
                        .filter(|_| package.as_deref() == Some("base"))
                        .or_else(|| declared_masked_verb(self.masked_verbs, name)),
                    _ => None,
                };
                let Some(leading) = masking_family else {
                    self.resolve_arguments(&module[arguments]);
                    return;
                };
                // R matches named arguments first and fills the rest positionally, so a
                // positional argument takes the next data formal no name has claimed.
                let claimed_by_name = module[arguments]
                    .iter()
                    .filter_map(|argument| module.argument_name(argument))
                    .filter(|name| leading.contains(&name.as_str()))
                    .count();
                let mut unclaimed = leading.len().saturating_sub(claimed_by_name);
                for argument in &module[arguments] {
                    let data_argument = match module.argument_name(argument) {
                        Some(name) => leading.contains(&name.as_str()),
                        None => {
                            let takes_a_data_slot = unclaimed > 0;
                            unclaimed = unclaimed.saturating_sub(1);
                            takes_a_data_slot
                        }
                    };
                    let Some(value) = argument.value else {
                        continue;
                    };
                    if data_argument {
                        self.resolve(value);
                    } else {
                        self.quiet_depth += 1;
                        self.resolve(value);
                        self.quiet_depth -= 1;
                    }
                }
            }
            ExpressionKind::Index {
                double,
                target,
                arguments,
            } => {
                self.resolve(*target);
                if !*double {
                    self.bracket_depth += 1;
                }
                if !*double && data_table_signature(module, &module[arguments]) {
                    self.naming.masked_subsets.insert(id);
                    self.quiet_depth += 1;
                    self.resolve_arguments(&module[arguments]);
                    self.quiet_depth -= 1;
                } else {
                    self.resolve_arguments(&module[arguments]);
                }
                if !*double {
                    self.bracket_depth -= 1;
                }
                // A frame's `:=` is copy-on-write, which ry models as a new write of the
                // subject's slot, so later reads see the updated columns. It writes only a slot
                // the current scope already owns (a top-level name, a local, or a parameter):
                // on any other subject, such as a global `data.table` updated inside a
                // function, `:=` works by reference, so it must not shadow the subject with a
                // local binding that would capture navigation and rename. The target's own
                // read keeps its resolution, since it reads the value before the update.
                if !*double && is_column_update(module, &module[arguments]) {
                    self.record_column_update(id, *target);
                }
            }
            ExpressionKind::Field { target, .. } => self.resolve(*target),
            ExpressionKind::Namespace {
                internal,
                package,
                name,
            } => {
                if self.emit
                    && self.quiet_depth == 0
                    && let Some(package) = package
                {
                    self.naming.namespace_reads.insert(
                        id,
                        NamespaceRead {
                            internal: *internal,
                            package: *package,
                            name: *name,
                        },
                    );
                }
            }
            ExpressionKind::Function { parameters, body } => {
                let scope_id = self.stable_scope_id(id);
                self.scopes.push(Scope {
                    id: scope_id,
                    kind: ScopeKind::Function,
                    slots: FxHashMap::default(),
                });
                let saved_flow = mem::take(&mut self.flow);
                for parameter in &module[parameters] {
                    let slot =
                        self.mint_binding(parameter.name, parameter.range, BindingKind::Parameter);
                    self.current_scope().slots.insert(parameter.name, slot);
                    self.record_write(slot, Reach::Implicit);
                }
                self.premint_frame_assignments(*body);
                for parameter in &module[parameters] {
                    if let Some(default) = parameter.default {
                        self.resolve(default);
                    }
                }
                self.resolve(*body);
                self.scopes.pop();
                self.flow = saved_flow;
            }
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.resolve(*condition);
                let entry = self.flow.clone();
                self.resolve(*then_branch);
                let after_then = mem::replace(&mut self.flow, entry);
                if let Some(else_branch) = else_branch {
                    self.resolve(*else_branch);
                }
                join_flow(&mut self.flow, &after_then);
            }
            ExpressionKind::For {
                variable,
                sequence,
                body,
            } => {
                self.resolve(*sequence);
                if let Some(variable) = variable {
                    let NameSite {
                        name: variable,
                        range,
                    } = self.module[variable];
                    let slot = self.mint_binding(variable, range, BindingKind::ForVariable);
                    // A top-level loop leaves its variable bound in the global environment.
                    if self.current_scope().kind == ScopeKind::TopLevel {
                        self.naming.super_globals.insert(variable);
                    }
                    self.current_scope().slots.insert(variable, slot);
                    self.record_write(slot, Reach::Implicit);
                }
                self.loop_body(*body, true);
            }
            ExpressionKind::While { condition, body } => {
                self.resolve(*condition);
                self.loop_body(*body, true);
            }
            ExpressionKind::Repeat { body } => {
                self.loop_body(*body, false);
            }
            ExpressionKind::Local { body } => {
                let scope_id = self.stable_scope_id(id);
                self.scopes.push(Scope {
                    id: scope_id,
                    kind: ScopeKind::Local,
                    slots: FxHashMap::default(),
                });
                self.premint_frame_assignments(*body);
                self.resolve(*body);
                self.scopes.pop();
            }
            ExpressionKind::Block { statements } => {
                for &statement in &module[statements] {
                    self.resolve(statement);
                }
            }
            ExpressionKind::Paren(inner) => self.resolve(*inner),
            ExpressionKind::Tuple(items) | ExpressionKind::Array(items) => {
                for &item in &module[items] {
                    self.resolve(item);
                }
            }
            ExpressionKind::Record(fields) => {
                for field in &module[fields] {
                    self.resolve(field.value);
                }
            }
            ExpressionKind::Tag { payload, .. } => {
                if let Some(payload) = payload {
                    self.resolve(*payload);
                }
            }
            ExpressionKind::Match { scrutinee, arms } => {
                self.resolve_match(*scrutinee, &module[arms])
            }
        }
    }

    /// Each arm runs from the state after the scrutinee, like an `if` branch. Its pattern's
    /// names are written in the current frame, so the guard and body read them, and after the
    /// `match` a name only some arms bind may be undefined.
    fn resolve_match(&mut self, scrutinee: ExprId, arms: &[MatchArm]) {
        let module = self.module;
        self.resolve(scrutinee);
        let entry = self.flow.clone();
        let mut joined: Option<FlowState> = None;
        for arm in arms {
            self.flow = entry.clone();
            if self.emit {
                self.check_or_patterns(arm.pattern);
            }
            for &leaf in &module[arm.bindings] {
                if let ExpressionKind::NameRef(name) = module.kind(leaf) {
                    let range = module.range(leaf);
                    let slot = self.frame_slot(*name, range, BindingKind::PatternVariable);
                    self.naming.resolutions.insert(leaf, slot);
                    self.record_write(slot, Reach::Implicit);
                }
            }
            if let Some(guard) = arm.guard {
                self.resolve(guard);
            }
            self.resolve(arm.body);
            if diverges(module, arm.body) {
                continue;
            }
            match &mut joined {
                Some(state) => join_flow(state, &self.flow),
                None => joined = Some(mem::take(&mut self.flow)),
            }
        }
        self.flow = joined.unwrap_or(entry);
    }

    fn check_or_patterns(&mut self, pattern: PatId) {
        let module = self.module;
        match &module.pattern(pattern).kind {
            PatternKind::Missing
            | PatternKind::Wildcard
            | PatternKind::Bind(_)
            | PatternKind::Literal { .. } => {}
            PatternKind::Tag { payload, .. } => {
                if let Some(payload) = payload {
                    self.check_or_patterns(*payload);
                }
            }
            PatternKind::Tuple(items) => {
                for &item in &module[items] {
                    self.check_or_patterns(item);
                }
            }
            PatternKind::Record { fields, .. } => {
                for field in &module[fields] {
                    self.check_or_patterns(field.pattern);
                }
            }
            PatternKind::Or(alternatives) => {
                let names_of = |alternative: PatId| -> BTreeSet<Symbol> {
                    module
                        .pattern_bindings(alternative)
                        .into_iter()
                        .filter_map(|leaf| match module.kind(leaf) {
                            ExpressionKind::NameRef(name) => Some(*name),
                            _ => None,
                        })
                        .collect()
                };
                let all: BTreeSet<Symbol> = module[alternatives]
                    .iter()
                    .flat_map(|&alternative| names_of(alternative))
                    .collect();
                for &alternative in &module[alternatives] {
                    let bound = names_of(alternative);
                    for name in all.difference(&bound) {
                        self.naming.or_pattern_mismatches.push(OrPatternMismatch {
                            name: *name,
                            range: module.pattern(alternative).range,
                        });
                    }
                    self.check_or_patterns(alternative);
                }
            }
        }
    }

    /// R takes a single unnamed alternative among named ones as the default; without one an
    /// unmatched key runs nothing, so names bound in the branches may be undefined after.
    /// The all-unnamed positional form can match nothing too, so it never has a default.
    fn resolve_switch(&mut self, arguments: &[Argument]) {
        let subject = arguments
            .iter()
            .position(|argument| self.module.argument_name(argument).as_deref() == Some("EXPR"))
            .or_else(|| {
                arguments
                    .iter()
                    .position(|argument| argument.name.is_none())
            });
        if let Some(value) = subject.and_then(|index| arguments[index].value) {
            self.resolve(value);
        }

        let alternatives: Vec<&Argument> = arguments
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != subject)
            .map(|(_, argument)| argument)
            .collect();
        let named = alternatives
            .iter()
            .filter(|argument| argument.name.is_some())
            .count();
        let unnamed_bodies = alternatives
            .iter()
            .filter(|argument| argument.name.is_none() && argument.value.is_some())
            .count();
        let has_default = named > 0 && unnamed_bodies == 1;

        let entry = self.flow.clone();
        let mut joined: Option<FlowState> = None;
        for alternative in alternatives {
            // `switch(k, a = , b = 2)`: an empty alternative falls through to the next.
            let Some(value) = alternative.value else {
                continue;
            };
            self.flow = entry.clone();
            self.resolve(value);
            if diverges(self.module, value) {
                continue;
            }
            match &mut joined {
                Some(state) => join_flow(state, &self.flow),
                None => joined = Some(mem::take(&mut self.flow)),
            }
        }

        self.flow = match joined {
            Some(mut state) => {
                if !has_default {
                    join_flow(&mut state, &entry);
                }
                state
            }
            None => entry,
        };
    }

    fn resolve_arguments(&mut self, arguments: &[Argument]) {
        for argument in arguments {
            if let Some(value) = argument.value {
                self.resolve(value);
            }
        }
    }

    /// Re-walks to a fixed point (the reach lattice is finite and joins only grow), then
    /// records effects on one final pass. `may_skip` joins the never-entered path back in.
    fn loop_body(&mut self, body: ExprId, may_skip: bool) {
        let entry = self.flow.clone();
        let saved_emit = self.emit;
        let enclosing_exits = mem::take(&mut self.loop_exits);
        self.emit = false;
        loop {
            let before = self.flow.clone();
            self.resolve(body);
            join_flow(&mut self.flow, &before);
            if self.flow == before {
                break;
            }
        }
        self.emit = saved_emit;
        // Only the final pass's `break` states describe the converged loop.
        self.loop_exits.clear();
        self.resolve(body);
        let breaks = mem::replace(&mut self.loop_exits, enclosing_exits);

        for exit in &breaks {
            join_flow(&mut self.flow, exit);
        }
        if may_skip {
            join_flow(&mut self.flow, &entry);
        }
    }

    /// Pre-mints slots for the frame's direct assignment targets, so a closure defined earlier
    /// can resolve a name written later: it runs after the frame has executed.
    fn premint_frame_assignments(&mut self, id: ExprId) {
        let module = self.module;
        let kind = module.kind(id);
        match kind {
            ExpressionKind::Function { .. } | ExpressionKind::Local { .. } => {}
            ExpressionKind::Assign {
                spelling,
                target,
                value,
            } => {
                if *spelling == AssignSpelling::Destructure {
                    for leaf in destructure_leaves(module, *target).unwrap_or_default() {
                        if let ExpressionKind::NameRef(name) = module.kind(leaf) {
                            self.frame_slot(*name, module.range(leaf), BindingKind::Local);
                        }
                    }
                }
                let target = *target;
                if *spelling != AssignSpelling::Super
                    && let ExpressionKind::NameRef(name) = module.kind(target)
                {
                    self.frame_slot(*name, module.range(target), BindingKind::Local);
                }
                self.premint_frame_assignments(*value);
            }
            ExpressionKind::For {
                variable,
                sequence,
                body,
            } => {
                if let Some(variable) = variable {
                    let NameSite { name, range } = module[variable];
                    self.frame_slot(name, range, BindingKind::ForVariable);
                }
                self.premint_frame_assignments(*sequence);
                self.premint_frame_assignments(*body);
            }
            ExpressionKind::Match { scrutinee, arms } => {
                self.premint_frame_assignments(*scrutinee);
                for arm in &module[arms] {
                    for &leaf in &module[arm.bindings] {
                        if let ExpressionKind::NameRef(name) = module.kind(leaf) {
                            self.frame_slot(
                                *name,
                                module.range(leaf),
                                BindingKind::PatternVariable,
                            );
                        }
                    }
                    if let Some(guard) = arm.guard {
                        self.premint_frame_assignments(guard);
                    }
                    self.premint_frame_assignments(arm.body);
                }
            }
            _ => {
                let module = self.module;
                module.for_each_child(id, |child| self.premint_frame_assignments(child));
            }
        }
    }

    /// Separate from [`Self::resolve_read`] because a `%op%` read is a token with no expression
    /// id. A slot resolves when a write reaches the read or the read crosses a function
    /// boundary (a capture); an unassignable slot does not shadow.
    fn mark_slot_read(&mut self, name: Symbol) -> Option<(BindingId, bool)> {
        let function_depth = self.current_function_depth();
        let (depth, slot) = self
            .scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(depth, scope)| {
                let &slot = scope.slots.get(&name)?;
                (self.flow.contains_key(&slot) || depth < function_depth).then_some((depth, slot))
            })?;
        let maybe_undefined = self.mark_slot_reached(name, depth, slot, function_depth);
        Some((slot, maybe_undefined))
    }

    /// Records that a read at the current point observes `slot`, which lives at scope `depth`;
    /// true when some path reaches the read with the slot still unassigned.
    fn mark_slot_reached(
        &mut self,
        name: Symbol,
        depth: usize,
        slot: BindingId,
        function_depth: usize,
    ) -> bool {
        match self.naming.bindings.get(&slot).map(|binding| binding.kind) {
            Some(BindingKind::Parameter) => {
                self.read_parameter_slots.insert(slot);
            }
            Some(BindingKind::TopLevel) => {
                self.naming.used_top_level_names.insert(name);
            }
            _ => {}
        }
        let mut maybe_undefined = false;
        for reach in self.flow.get(&slot).into_iter().flatten() {
            match reach {
                Reach::Assignment(index) => self.writes[*index as usize].used = true,
                Reach::Unassigned => maybe_undefined = true,
                Reach::Implicit => {}
            }
        }
        if depth < function_depth || self.deferred_depth > 0 {
            // A capture keeps every write of the name in that frame observable; a same-named
            // binding in a different frame is shadowed and stays reportably dead.
            self.naming.captured_slots.insert(slot);
            let frame = self.scopes[depth].id;
            for write in &mut self.writes {
                if write.scope == frame && write.name == name {
                    write.used = true;
                }
            }
        }
        maybe_undefined
    }

    /// Every enclosing slot that could serve the call is marked read, since the checker may
    /// pick any of them once it knows which bindings hold functions.
    fn record_callee_fallback(&mut self, callee: ExprId, name: Symbol, nearest: BindingId) {
        let function_depth = self.current_function_depth();
        let Some(nearest_depth) = self
            .scopes
            .iter()
            .rposition(|scope| scope.slots.get(&name) == Some(&nearest))
        else {
            return;
        };
        let mut outer = Vec::new();
        for depth in (0..nearest_depth).rev() {
            let Some(&slot) = self.scopes[depth].slots.get(&name) else {
                continue;
            };
            if !self.flow.contains_key(&slot) && depth >= function_depth {
                continue;
            }
            self.mark_slot_reached(name, depth, slot, function_depth);
            outer.push(slot);
        }
        self.naming.callee_fallbacks.insert(
            callee,
            CalleeFallback {
                outer: outer.into_boxed_slice(),
                deferred: function_depth > 0,
            },
        );
    }

    fn resolve_read(&mut self, id: ExprId, name: Symbol) {
        if name == "..." || name.starts_with("..") && name[2..].chars().all(|c| c.is_ascii_digit())
        {
            return;
        }
        let name = match dot_dot_variable(&name) {
            Some(variable) if self.bracket_depth > 0 => Symbol::new(variable),
            _ => name,
        };
        match self.mark_slot_read(name) {
            Some((slot, maybe_undefined)) => {
                self.naming.resolutions.insert(id, slot);
                if maybe_undefined && self.emit {
                    self.naming.maybe_undefined.insert(id);
                }
            }
            None => {
                if self.emit {
                    if self.quiet_depth == 0 {
                        self.naming.non_locals.insert(id, name);
                        if self.current_function_depth() > 0 {
                            self.naming.deferred_non_locals.insert(id);
                        }
                    } else {
                        self.naming.quiet_reads.insert(id, name);
                        if self.current_function_depth() > 0 {
                            self.naming.deferred_quiet_reads.insert(id);
                        }
                    }
                }
            }
        }
    }

    /// Keeps the named binding alive without making the string a read the checker types.
    fn resolve_function_name_string(&mut self, id: ExprId, name: Symbol) {
        if self.mark_slot_read(name).is_some() || !self.emit {
            return;
        }
        self.naming.quiet_reads.insert(id, name);
        if self.current_function_depth() > 0 {
            self.naming.deferred_quiet_reads.insert(id);
        }
    }

    fn resolve_assignment_target(
        &mut self,
        assignment: ExprId,
        target: ExprId,
        spelling: AssignSpelling,
    ) {
        // Inside a quoting form the target binds nothing but still resolves as an editor read.
        if self.quoted_depth > 0 {
            self.resolve(target);
            return;
        }
        let module = self.module;
        if spelling == AssignSpelling::Destructure {
            for leaf in destructure_leaves(module, target).unwrap_or_default() {
                if let ExpressionKind::NameRef(name) = module.kind(leaf) {
                    let range = module.range(leaf);
                    let kind = self.assignment_binding_kind();
                    let slot = self.frame_slot(*name, range, kind);
                    self.naming.resolutions.insert(leaf, slot);
                    self.record_write_site(assignment, *name, range, slot, false);
                }
            }
            return;
        }
        let target_expression = target;
        match module.kind(target_expression) {
            // `"x" <- 1` is `x <- 1` in R.
            ExpressionKind::NameRef(_) | ExpressionKind::Literal(LiteralKind::String(_)) => {
                let name = &match module.kind(target_expression) {
                    ExpressionKind::NameRef(name) => *name,
                    ExpressionKind::Literal(LiteralKind::String(text)) => {
                        Symbol::new(&module[text])
                    }
                    _ => return,
                };
                let range = module.range(target_expression);
                match spelling {
                    AssignSpelling::Super => {
                        // `<<-` binds the nearest slot in a strictly enclosing scope, else the
                        // global environment. Bounded by the scope stack, not the enclosing
                        // function: `local()` scopes are not functions.
                        let enclosing = (0..self.scopes.len() - 1)
                            .rev()
                            .find_map(|depth| Some((depth, *self.scopes[depth].slots.get(&name)?)));
                        match enclosing {
                            Some((depth, slot)) => {
                                self.naming.resolutions.insert(target, slot);
                                self.naming.captured_slots.insert(slot);
                                // `<<-` finds this slot only because of that frame's writes, so it
                                // keeps them alive like a capturing read; otherwise a write-only
                                // counter's initializer looks like a dead store.
                                let frame = self.scopes[depth].id;
                                for write in &mut self.writes {
                                    if write.scope == frame && write.name == *name {
                                        write.used = true;
                                    }
                                }
                                self.record_write_site(assignment, *name, range, slot, true);
                            }
                            None => {
                                if self.emit {
                                    self.naming.non_locals.insert(target, *name);
                                    self.naming.super_globals.insert(*name);
                                }
                            }
                        }
                    }
                    _ => {
                        let kind = self.assignment_binding_kind();
                        let slot = self.frame_slot(*name, range, kind);
                        self.naming.resolutions.insert(target, slot);
                        let reportable = kind == BindingKind::Local;
                        self.record_write_site(assignment, *name, range, slot, reportable);
                    }
                }
            }
            // Replacement forms read their base as a value, then rebind it in the current scope
            // (R evaluates `x[i] <- v` as `x <- `[<-`(x, i, v)`).
            _ => match replacement_base(module, target) {
                Some((base_expression, name, range)) => {
                    self.resolve(base_expression);
                    let kind = self.assignment_binding_kind();
                    let slot = self.frame_slot(name, range, kind);
                    self.naming.resolutions.insert(base_expression, slot);
                    self.record_write_site(assignment, name, range, slot, false);
                    self.resolve_replacement_subscripts(target, base_expression);
                }
                // R refuses a computed or numeric target only at run time, so its reads would be
                // reported as deliberate. A dangling operator on the previous line produces this.
                None if self.emit && refuses_assignment(module.kind(target_expression)) => {
                    // Parser recovery can leave the operands out of order.
                    let operand_gap = match module.kind(target_expression) {
                        ExpressionKind::Binary { lhs, rhs, .. } => {
                            let start = module.range(*lhs).end();
                            let end = module.range(*rhs).start();
                            (start <= end).then(|| TextRange::new(start, end))
                        }
                        _ => None,
                    };
                    self.naming
                        .invalid_assignment_targets
                        .push(InvalidAssignmentTarget {
                            range: module.range(target_expression),
                            operand_gap,
                        });
                }
                None => self.resolve(target),
            },
        }
    }

    fn resolve_replacement_subscripts(&mut self, target: ExprId, base: ExprId) {
        if target == base {
            return;
        }
        let module = self.module;
        match module.kind(target) {
            ExpressionKind::Field { target: inner, .. } => {
                self.resolve_replacement_subscripts(*inner, base);
            }
            ExpressionKind::Index {
                target: inner,
                arguments,
                ..
            } => {
                self.resolve_replacement_subscripts(*inner, base);
                self.resolve_arguments(&module[arguments]);
            }
            ExpressionKind::Call { callee, arguments } => {
                // R invokes the `name<-` form, not the callee as written, so it resolves quietly.
                self.quiet_depth += 1;
                self.resolve(*callee);
                self.quiet_depth -= 1;
                if let Some(first) = module[arguments].first()
                    && let Some(value) = first.value
                {
                    self.resolve_replacement_subscripts(value, base);
                }
                self.resolve_arguments(module[arguments].get(1..).unwrap_or_default());
            }
            _ => self.resolve(target),
        }
    }

    fn record_write_site(
        &mut self,
        assignment: ExprId,
        name: Symbol,
        range: TextRange,
        slot: BindingId,
        reportable: bool,
    ) {
        let index = match self.write_by_expression.get(&assignment) {
            Some(&index) => index,
            None => {
                let index = self.writes.len() as u32;
                let scope = self
                    .scopes
                    .iter()
                    .rev()
                    .find(|scope| scope.slots.get(&name) == Some(&slot))
                    .map(|scope| scope.id)
                    .unwrap_or_else(|| self.current_scope().id);
                self.writes.push(AssignmentWrite {
                    name,
                    range,
                    scope,
                    used: false,
                    reportable,
                });
                self.write_by_expression.insert(assignment, index);
                index
            }
        };
        if self.naming.captured_slots.contains(&slot) {
            self.writes[index as usize].used = true;
        }
        self.record_write(slot, Reach::Assignment(index));
    }

    fn collect_unused(&mut self) {
        // A `.`- or `_`-prefixed name is conventionally an intentional hold-over.
        let throwaway = |name: &str| name.starts_with('.') || name.starts_with('_');
        for write in &self.writes {
            if write.reportable && !write.used && !throwaway(&write.name) {
                self.naming.unused_assignments.push(UnusedAssignment {
                    name: write.name,
                    range: write.range,
                });
            }
        }
        for (id, binding) in &self.naming.bindings {
            // The prefix rule also exempts `...`, whose reads never resolve.
            if binding.kind == BindingKind::Parameter
                && !throwaway(&binding.name)
                && !self.read_parameter_slots.contains(id)
            {
                self.naming.unused_parameters.push(UnusedAssignment {
                    name: binding.name,
                    range: binding.range,
                });
            }
        }
    }
}

/// Free rather than a method so the borrow lives as long as the corpus map, not the walker.
fn declared_masked_verb<'a>(
    masked_verbs: &'a FxHashMap<String, Vec<String>>,
    name: &str,
) -> Option<Vec<&'a str>> {
    masked_verbs
        .get(name)
        .map(|leading| leading.iter().map(String::as_str).collect())
}

fn replacement_base(module: &Module, target: ExprId) -> Option<(ExprId, Symbol, TextRange)> {
    let expression = target;
    match module.kind(expression) {
        ExpressionKind::NameRef(name) => Some((target, *name, module.range(expression))),
        ExpressionKind::Field { target: inner, .. }
        | ExpressionKind::Index { target: inner, .. } => replacement_base(module, *inner),
        ExpressionKind::Call { arguments, .. } => module[arguments]
            .first()
            .and_then(|argument| argument.value)
            .and_then(|value| replacement_base(module, value)),
        _ => None,
    }
}

fn data_table_signature(module: &Module, arguments: &[Argument]) -> bool {
    arguments.iter().any(|argument| {
        matches!(
            module.argument_name(argument).as_deref(),
            Some("by" | "keyby")
        ) || argument
            .value
            .is_some_and(|value| data_table_marker(module, value))
    })
}

/// The variable a data.table `..name` read inside `[` refers to: `name`, looked up in the
/// enclosing scope rather than among the columns.
pub fn dot_dot_variable(name: &str) -> Option<&str> {
    name.strip_prefix("..")
        .filter(|variable| !variable.is_empty() && !variable.starts_with('.'))
}

/// The `j` argument is a `:=` call.
pub(crate) fn is_column_update(module: &Module, arguments: &[Argument]) -> bool {
    let (_, j) = query_index_arguments(module, arguments);
    j.and_then(|argument| argument.value).is_some_and(|value| {
        matches!(
            module.kind(value),
            ExpressionKind::Call { callee, .. }
                if matches!(module.kind(*callee), ExpressionKind::NameRef(name) if name == ":=")
        )
    })
}

/// A query's `i` and `j`, matched as R matches `[`'s formals: by name first, then the
/// unnamed arguments fill whichever of the two is still open, in order.
pub(crate) fn query_index_arguments<'a>(
    module: &Module,
    arguments: &'a [Argument],
) -> (Option<&'a Argument>, Option<&'a Argument>) {
    let named = |formal: &str| {
        arguments
            .iter()
            .find(|argument| module.argument_name(argument).as_deref() == Some(formal))
    };
    let mut positional = arguments.iter().filter(|argument| argument.name.is_none());
    let i = named("i").or_else(|| positional.next());
    let j = named("j").or_else(|| positional.next());
    (i, j)
}

fn data_table_marker(module: &Module, id: ExprId) -> bool {
    let kind = module.kind(id);
    match kind {
        ExpressionKind::Call { callee, .. }
            if matches!(
                module.kind(*callee),
                ExpressionKind::NameRef(name) if name == ":=" || name == "."
            ) =>
        {
            true
        }
        ExpressionKind::NameRef(name) => {
            matches!(
                name.as_str(),
                ".SD" | ".N" | ".I" | ".BY" | ".GRP" | ".EACHI"
            )
        }
        // A nested function body is its own evaluation context.
        ExpressionKind::Function { .. } => false,
        _ => module.any_child(id, |child| data_table_marker(module, child)),
    }
}

/// Kept in step with the checker's divergence rule.
fn diverges(module: &Module, id: ExprId) -> bool {
    match module.kind(id) {
        ExpressionKind::Break | ExpressionKind::Next => true,
        ExpressionKind::Call { callee, .. } => matches!(
            module.kind(*callee),
            ExpressionKind::NameRef(name) if name == "return" || name == "stop"
        ),
        ExpressionKind::Block { statements, .. } => module[statements]
            .last()
            .is_some_and(|&last| diverges(module, last)),
        ExpressionKind::If {
            then_branch,
            else_branch: Some(else_branch),
            ..
        } => diverges(module, *then_branch) && diverges(module, *else_branch),
        ExpressionKind::Paren(inner) => diverges(module, *inner),
        ExpressionKind::Match { arms, .. } => {
            !arms.is_empty() && module[arms].iter().all(|arm| diverges(module, arm.body))
        }
        _ => false,
    }
}

/// Base functions that pass their function argument through `match.fun`.
pub(crate) const MATCH_FUN_CALLERS: [&str; 20] = [
    "lapply",
    "sapply",
    "vapply",
    "mapply",
    "Map",
    "Filter",
    "Find",
    "Position",
    "Reduce",
    "do.call",
    "outer",
    "apply",
    "tapply",
    "sweep",
    "rapply",
    "eapply",
    "Vectorize",
    "Negate",
    "aggregate",
    "match.fun",
];

/// The base functions that call a compiled routine named by their `.NAME` argument.
const NATIVE_INTERFACES: [&str; 5] = [".Call", ".External", ".External2", ".C", ".Fortran"];

/// The formals that hold the data; every other argument evaluates in the data's frame.
fn base_masking_family(name: &str) -> Option<Vec<&'static str>> {
    match name {
        "with" | "within" => Some(vec!["data"]),
        "subset" => Some(vec!["x"]),
        // `transform(`_data`, ...)`: a column assignment such as `x = mpg * 2` must
        // stay masked, so `x` cannot be the data formal here as it is for `subset`.
        "transform" => Some(vec!["_data"]),
        _ => None,
    }
}

/// A slot present on only one path was unassigned on the other.
fn join_flow(into: &mut FlowState, other: &FlowState) {
    for (slot, reaches) in other {
        match into.entry(*slot) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().extend(reaches.iter().copied());
            }
            Entry::Vacant(vacant) => {
                let mut set = reaches.clone();
                set.insert(Reach::Unassigned);
                vacant.insert(set);
            }
        }
    }
    for (slot, set) in into.iter_mut() {
        if !other.contains_key(slot) {
            set.insert(Reach::Unassigned);
        }
    }
}

/// Only shapes R certainly refuses. `(x) <- 1` is left alone (R's handling is odd), and so
/// is `!`: `expr(!!name <- value)` parses as an assignment to `!!name` that never runs.
fn refuses_assignment(kind: &ExpressionKind) -> bool {
    match kind {
        ExpressionKind::Binary { .. } => true,
        ExpressionKind::Unary { operator, .. } => *operator != UnaryOperator::Not,
        ExpressionKind::Literal(literal) => !matches!(literal, LiteralKind::String(_)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::hir::lower_item};

    fn naming_of(source: &str) -> ItemNaming {
        let parse = syntax::parse(source);
        let root = parse.syntax_node();
        let item = root.children().next().expect("one top-level item");
        let module = lower_item(&item);
        resolve_item(&module, &FxHashMap::default())
    }

    #[test]
    fn conditional_write_joins_and_read_resolves() {
        let naming = naming_of("f <- function(flag) {\n  x <- 1\n  if (flag) x <- 2\n  x\n}");
        assert!(naming.unused_assignments.is_empty());
        assert!(naming.maybe_undefined.is_empty());
    }

    #[test]
    fn dead_store_is_reported() {
        let naming = naming_of("f <- function() {\n  x <- 1\n  x <- 2\n  x\n}");
        assert_eq!(naming.unused_assignments.len(), 1);
    }

    #[test]
    fn loop_accumulator_is_not_a_dead_store() {
        let naming = naming_of(
            "f <- function(n) {\n  total <- 0\n  for (i in 1:n) total <- total + i\n  total\n}",
        );
        assert!(
            naming.unused_assignments.is_empty(),
            "loop accumulator writes reach across iterations: {:?}",
            naming.unused_assignments
        );
    }

    #[test]
    fn maybe_undefined_on_partial_paths() {
        let naming = naming_of("f <- function(flag) {\n  if (flag) y <- 1\n  y\n}");
        assert_eq!(naming.maybe_undefined.len(), 1);
    }

    #[test]
    fn unresolved_reads_are_non_local() {
        let naming = naming_of("f <- function() unknown_helper(1)");
        assert!(
            naming
                .non_locals
                .values()
                .any(|name| name == "unknown_helper")
        );
    }
}
