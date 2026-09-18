//! Naming, which is the R variable model. A variable is a mutable slot, and
//! the module resolves reads against reaching writes.
//!
//! Every `<-` or `=` of a name in a frame resolves to one `BindingId` slot,
//! because R assignment mutates the scope's environment. A `<<-` walks the
//! lexical chain to the nearest enclosing slot. A read resolves to a slot only
//! when a write can reach it, so an unassignable slot does not shadow and
//! lookup falls outward exactly as it does at R's runtime. The read is flagged
//! maybe-undefined when an unassigned path also reaches it. The unused check
//! reports an **assignment site** whose write no read reaches.
//!
//! Control flow forks and joins the per-slot reaching-write sets. A loop body
//! iterates to a fixed point. Binding creation is idempotent by site, so a
//! re-walk mints no new id, and effects are recorded only on the final pass.
//! `switch` is control flow too, despite being spelled as a call. Exactly one
//! alternative runs, so its branches fork and join like an `if`'s arms rather
//! than executing in sequence.
//!
//! Data-masked evaluation is recognized structurally. A `[` carrying an
//! unambiguous data.table signature masks its index arguments, which are
//! recorded in `masked_subsets`. The base masking family, which is `with`,
//! `within`, `subset`, and `transform`, masks every argument after the data. A
//! masked read that resolves keeps its ordinary resolution. One that does not
//! resolve stays quiet, because a column reference is not an unresolved name.
//!
//! Two things are deferred, and the structure already carries them. They are
//! the nested-loop region memo and the unused-parameter lint.

use crate::hir::{
    Argument, AssignSpelling, ExprId, ExpressionKind, LiteralKind, Module, UnaryOperator,
};
use rustc_hash::FxHashMap;
use std::collections::{BTreeMap, BTreeSet};
use syntax::TextRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingInfo {
    pub id: BindingId,
    pub name: String,
    /// The first write's site, which is the slot's definition site for hover
    /// and goto.
    pub range: TextRange,
    pub kind: BindingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// The item's own top-level binding (package-visible; never unused).
    TopLevel,
    /// A variable slot in a function or block scope.
    Local,
    /// A function parameter's slot (the parameter itself is never reported).
    Parameter,
    /// A `for (v in …)` loop variable (often intentionally unused).
    ForVariable,
}

/// What a read found: the slot it resolved to, and whether an unassigned path
/// also reaches it.
struct SlotRead {
    slot: BindingId,
    maybe_undefined: bool,
}

/// A dead store: an assignment whose written value no read reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusedAssignment {
    pub name: String,
    pub range: TextRange,
}

/// A qualified `pkg::name` / `pkg:::name` read, recorded for validation
/// against the stub corpus's per-namespace exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRead {
    pub internal: bool,
    pub package: String,
    pub name: Option<String>,
}

/// The naming facts of one item, item-relative like all HIR spans.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemNaming {
    pub bindings: BTreeMap<BindingId, BindingInfo>,
    /// Reads and assignment targets resolved to their slot.
    pub resolutions: BTreeMap<ExprId, BindingId>,
    /// Reads an unassigned path can reach.
    pub maybe_undefined: BTreeSet<ExprId>,
    /// Reads no lexical slot resolves. Each is a package global, a stub, or
    /// unresolved.
    pub non_locals: BTreeMap<ExprId, String>,
    /// The subset of `non_locals` read from inside a nested function: the
    /// read happens when the function is *called*, after the enclosing frame
    /// finished executing.
    pub deferred_non_locals: BTreeSet<ExprId>,
    /// Reads no lexical slot resolves that sit in a quiet context (data
    /// masking, an unsupported operator's operands): never reported as
    /// unresolved, but a read all the same. A masked expression falls back
    /// to enclosing bindings at runtime, so these keep cross-item bindings
    /// alive for the unused check.
    pub quiet_reads: BTreeMap<ExprId, String>,
    /// The subset of `quiet_reads` read from inside a nested function.
    pub deferred_quiet_reads: BTreeSet<ExprId>,
    /// Names read as `%…%` operators (`a %||% b`). R calls a function of that
    /// name, so the read keeps the project's own operator definition alive.
    /// Held by name rather than by expression id, because the operator is a
    /// token and not a `NameRef` node; quiet like the other opaque-construct
    /// reads, so an undeclared operator is never reported unresolved.
    pub quiet_operator_reads: BTreeSet<String>,
    /// Dead stores (surfaced only when the unused check is enabled).
    pub unused_assignments: Vec<UnusedAssignment>,
    /// Parameters no read resolves to (`...` excluded), for the default-off
    /// unused-parameter lint: R signatures legitimately carry ignored formals
    /// (an S3 method must match its generic), so a project opts in.
    pub unused_parameters: Vec<UnusedAssignment>,
    /// Names of the item's own top-level slots that some read within the item
    /// resolved to (a loop reading its own carried variable, a capture). The
    /// cross-item unused check seeds these definers as already used.
    pub used_top_level_names: BTreeSet<String>,
    /// Slots read or super-assigned from inside a function nested below the
    /// slot's frame: every write to them stays observable.
    pub captured_slots: BTreeSet<BindingId>,
    /// `[` expressions recognized as data.table syntax: their indexes
    /// evaluate in the data's frame, and the whole bracket types `Unknown`.
    pub masked_subsets: BTreeSet<ExprId>,
    /// Qualified reads, skipping quieted (opaque-construct) contexts.
    pub namespace_reads: BTreeMap<ExprId, NamespaceRead>,
    /// Names written by `<<-` with no enclosing binding: R creates them in
    /// the global environment, so they resolve package-wide.
    pub super_globals: BTreeSet<String>,
    /// Assignment targets R refuses to assign to, which are a computed value
    /// and a number where a name belongs. R parses these and fails at run time
    /// with "target of assignment expands to non-language object", so nothing
    /// in the parse marks them. The reads inside such a target are suppressed,
    /// because they are the mistake's consequences rather than findings of
    /// their own.
    pub invalid_assignment_targets: Vec<InvalidAssignmentTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidAssignmentTarget {
    pub range: TextRange,
    /// For a computed target, the source between the two operands, which holds
    /// the operator. A line break *after* the operator means the previous line
    /// dangled and swallowed the next one. That is the commonest way to arrive
    /// here, and it is worth naming in the message. Deciding it needs the
    /// source text, which this pass does not have, so the target carries the
    /// span instead.
    pub operand_gap: Option<TextRange>,
}

/// Resolve one item's HIR. The item's own top-level assignment target (if any)
/// becomes a `TopLevel` binding; everything below follows the slot model.
pub fn resolve_item(module: &Module) -> ItemNaming {
    resolve_item_with_masked_verbs(module, &FxHashMap::default())
}

/// Like [`resolve_item`], with the stub corpus's `@masked` verbs: a call to
/// one of them (bare or `pkg::name`, unless locally shadowed) evaluates the
/// arguments its `...` absorbs in the data's frame, so those reads stay
/// quiet like the base masking family's. Each verb maps to the names of its
/// formals declared before the `...`. Those are the data arguments, and they
/// resolve normally by position or by name. An empty list masks every
/// argument.
pub fn resolve_item_with_masked_verbs(
    module: &Module,
    masked_verbs: &FxHashMap<String, Vec<String>>,
) -> ItemNaming {
    let mut context = Context {
        module,
        masked_verbs,
        next_binding: 0,
        scopes: vec![Scope::new(ScopeKind::TopLevel, 0)],
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
        naming: ItemNaming::default(),
    };
    if let Some(root) = module.root {
        context.resolve(root);
    }
    context.collect_unused();
    context.naming
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    /// A function body's frame: executes when *called*, so cross-frame reads
    /// from below are captures.
    Function,
    /// A `local({...})` block's environment: writes bind here and vanish with
    /// it, but execution is immediate, so it is not a capture boundary.
    Local,
    /// The item's top level.
    TopLevel,
}

struct Scope {
    /// Stable frame identity, one per defining expression, so a loop re-walk
    /// reuses it. Capture liveness marks a write of the SAME frame only,
    /// because a shadowed outer binding is not what the closure reads.
    id: u32,
    kind: ScopeKind,
    slots: BTreeMap<String, BindingId>,
}

impl Scope {
    fn new(kind: ScopeKind, id: u32) -> Scope {
        Scope {
            id,
            kind,
            slots: BTreeMap::new(),
        }
    }
}

/// One element of a slot's reaching-write set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    /// Some path reaches here without any write.
    Unassigned,
    /// A write with no reportable site (parameter, for-variable).
    Implicit,
    /// An assignment write, by index into `writes`.
    Assignment(u32),
}

type FlowState = BTreeMap<BindingId, BTreeSet<Reach>>;

struct AssignmentWrite {
    name: String,
    range: TextRange,
    /// The written slot's owning frame (see [`Scope::id`]).
    scope: u32,
    used: bool,
    reportable: bool,
}

struct Context<'a> {
    module: &'a Module,
    /// Stub-declared `@masked` verbs, whose `...` is data-masked in the dplyr
    /// style, mapped to the formal names declared before the `...`. Those are
    /// the data arguments, and they resolve normally.
    masked_verbs: &'a FxHashMap<String, Vec<String>>,
    next_binding: u32,
    scopes: Vec<Scope>,
    /// Loop re-walks must mint no new ids: one binding per (site, name).
    bindings_by_site: FxHashMap<(u32, u32, String), BindingId>,
    /// Frame ids per defining expression (the top level is 0); loop
    /// re-walks reuse them like binding ids.
    scope_ids: FxHashMap<ExprId, u32>,
    flow: FlowState,
    writes: Vec<AssignmentWrite>,
    write_by_expression: FxHashMap<ExprId, u32>,
    /// Parameter slots some read resolved to (like write `used` marking,
    /// monotone and recorded on every walk).
    read_parameter_slots: BTreeSet<BindingId>,
    /// Diagnostic-bearing sets are recorded only on a loop region's final walk
    /// (used-marking is monotone and always on).
    emit: bool,
    /// The reaching-write state at each `break` of the innermost loop being
    /// walked. A loop that cannot be skipped exits only through these, so the
    /// state after it is their join with the body's end state. Joining the
    /// loop *head* instead kept the first iteration's unassigned paths alive
    /// past a loop that always assigns.
    loop_exits: Vec<FlowState>,
    /// Non-zero inside an unsupported operator's operands: reads resolve but
    /// unresolved names stay quiet.
    quiet_depth: u32,
    /// Non-zero while walking an expression R evaluates LATER than this line.
    /// `on.exit`'s argument is such an expression, because it runs at function
    /// exit. A read inside it sees the final value of the names it mentions, so
    /// it keeps every write of those names alive.
    deferred_depth: u32,
    /// Non-zero inside a quoting form's argument, which is `quote`,
    /// `substitute`, `bquote`, or `expression`. R never evaluates one, so an
    /// assignment written there binds NOTHING. Reads are still walked and kept alive,
    /// because `eval` may run the expression later; only the binding is
    /// withheld. Treating a quoted assignment as a binding hid real
    /// `unresolved` findings: `quote(x <- 1)` followed by a read of `x` is an
    /// `object 'x' not found` error in R and reported clean here.
    quoted_depth: u32,
    naming: ItemNaming,
}

impl Context<'_> {
    /// Stable per defining expression, so loop re-walks reuse the id (the
    /// top-level frame is 0).
    fn stable_scope_id(&mut self, expression: ExprId) -> u32 {
        let next = self.scope_ids.len() as u32 + 1;
        *self.scope_ids.entry(expression).or_insert(next)
    }

    fn mint_binding(&mut self, name: &str, range: TextRange, kind: BindingKind) -> BindingId {
        let key = (
            u32::from(range.start()),
            u32::from(range.end()),
            name.to_owned(),
        );
        if let Some(&existing) = self.bindings_by_site.get(&key) {
            return existing;
        }
        let id = BindingId(self.next_binding);
        self.next_binding += 1;
        self.bindings_by_site.insert(key, id);
        self.naming.bindings.insert(
            id,
            BindingInfo {
                id,
                name: name.to_owned(),
                range,
                kind,
            },
        );
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

    fn resolve_many(&mut self, ids: &[ExprId]) {
        for &id in ids {
            self.resolve(id);
        }
    }

    fn resolve(&mut self, id: ExprId) {
        let expression = self.module.expression(id).clone();
        match &expression.kind {
            ExpressionKind::Missing | ExpressionKind::Next => {}
            ExpressionKind::Break => self.loop_exits.push(self.flow.clone()),
            ExpressionKind::Literal(_) => {}
            ExpressionKind::NameRef(name) => self.resolve_read(id, name),
            ExpressionKind::Assign {
                spelling,
                target,
                value,
            } => {
                // The value is walked first, because `x <- x + 1` reads the
                // previous state. A replacement-form target resolves first
                // instead. Its base binds before the value is examined, so the
                // value's reads of the same name land on the fresh binding.
                // `self$i <- self$i + 1` therefore reports `self` once, at the
                // target.
                let name_target = matches!(
                    self.module.expression(*target).kind,
                    ExpressionKind::NameRef(_)
                );
                if name_target {
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
                // `a %op% b` calls a function named `%op%`, so the operator
                // name is a read like any other. That is what keeps a project's
                // own `%||%` from being reported unused. It is a QUIET read,
                // because the construct is opaque to the checker, so an
                // undeclared operator is not a reportable unresolved name.
                if let Some(name) = special_name {
                    self.naming.quiet_operator_reads.insert(name.clone());
                    // The set above is by name, which is what the cross-item
                    // check for a package's own operator needs. A `%op%`
                    // defined as a LOCAL slot needs the slot marked too, or its
                    // definition reads as a dead store while the calls to it
                    // sit right below.
                    self.mark_slot_read(name);
                }
                match operator {
                    // A formula quotes its operands: names inside are model
                    // syntax, exactly like the unary form.
                    BinaryOperator::Tilde | BinaryOperator::Help => {}
                    // The operands of an operator the checker does not model
                    // still resolve, because the IDE needs goto and references
                    // inside a pipe. `&`, `|`, `|>`, and a user `%op%` are such
                    // operators. A non-local read there stays quiet, because
                    // the construct is opaque, so an unresolved name in it is
                    // not a reportable finding.
                    BinaryOperator::And
                    | BinaryOperator::Or
                    | BinaryOperator::Pipe
                    | BinaryOperator::Special => {
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
                // `library(pkg)` / `require(pkg)` / `help(topic)` quote a
                // bare positional first argument: the name is the package or
                // topic name, not a value reference. The rule is syntactic
                // (a rebound `library` does not change it), and only the
                // bare-name first-positional form quotes. A string, a named
                // first argument, and a qualified callee are each an ordinary
                // call.
                let quoting_callee = matches!(
                    &self.module.expression(*callee).kind,
                    ExpressionKind::NameRef(name) if matches!(name.as_str(), "library" | "require" | "help")
                );
                if quoting_callee
                    && let Some(first) = arguments.first()
                    && first.name.is_none()
                    && let Some(value) = first.value
                    && matches!(
                        self.module.expression(value).kind,
                        ExpressionKind::NameRef(_)
                    )
                {
                    for argument in arguments.iter().skip(1) {
                        if let Some(value) = argument.value {
                            self.resolve(value);
                        }
                    }
                    return;
                }
                // `on.exit(expr)` does not evaluate `expr` here. R stores it
                // and runs it when the function returns, so it observes the
                // LAST value of everything it reads rather than the value at
                // this line. Walking it as an ordinary argument made the canonical
                // rollback guard a dead store:
                //
                //     committed <- FALSE
                //     on.exit(if (!committed) rollback(con))
                //     ...
                //     committed <- TRUE          # "assigned but never used"
                //
                // The obvious response to that warning is to delete the
                // write, which makes every transaction roll back. A read inside
                // the deferred expression therefore keeps every write of that
                // name in this frame alive, exactly as a closure capture
                // does.
                let deferred_callee = matches!(
                    &self.module.expression(*callee).kind,
                    ExpressionKind::NameRef(name)
                        if name == "on.exit" && !self.naming.resolutions.contains_key(callee)
                );
                if deferred_callee {
                    self.deferred_depth += 1;
                    for argument in arguments {
                        if let Some(value) = argument.value {
                            self.resolve(value);
                        }
                    }
                    self.deferred_depth -= 1;
                    return;
                }
                // `switch` is control flow wearing a call's clothes: exactly
                // one alternative runs, so its branches fork and join like the
                // arms of an `if` rather than executing one after another.
                // Walked as an ordinary call, the first branch's write looked
                // dead the moment a later branch overwrote it. That is a false
                // "assigned but never used" on a line that runs whenever its
                // key is chosen.
                let is_switch = matches!(
                    &self.module.expression(*callee).kind,
                    ExpressionKind::NameRef(name)
                        if name == "switch" && !self.naming.resolutions.contains_key(callee)
                );
                if is_switch && !arguments.is_empty() {
                    self.resolve_switch(arguments);
                    return;
                }
                // A quoting form builds an expression instead of running it,
                // so nothing inside it binds. Verified against R: `quote`,
                // `substitute`, `bquote` and `expression` all leave the name
                // in `quote(x <- 1)` unbound. Reads are still walked, and
                // walked as deferred, because `eval` may run the expression
                // later. A write the quoted code mentions therefore stays alive
                // rather than turning into a false "assigned but never used".
                // The rule is syntactic, like the `library` and `on.exit` rules
                // above, so a locally defined function of the same name is an
                // ordinary call.
                let quoting_form = matches!(
                    &self.module.expression(*callee).kind,
                    ExpressionKind::NameRef(name)
                        if matches!(
                            name.as_str(),
                            "quote" | "substitute" | "bquote" | "expression"
                        ) && !self.naming.resolutions.contains_key(callee)
                );
                if quoting_form {
                    // The reads are quiet as well. A name inside a quoted
                    // expression need not exist yet, because `quote(x <- 1)` is
                    // how metaprogramming names a variable it is about to
                    // create. An unresolved read there is therefore not a
                    // reportable finding, and only a read of the name
                    // afterwards is.
                    self.quoted_depth += 1;
                    self.deferred_depth += 1;
                    self.quiet_depth += 1;
                    self.resolve_arguments(arguments);
                    self.quiet_depth -= 1;
                    self.deferred_depth -= 1;
                    self.quoted_depth -= 1;
                    return;
                }
                // A masking call evaluates the arguments its `...` absorbs
                // inside the data's frame; arguments matching the declared
                // data formals (by position or name) resolve normally. The
                // base family takes one data argument (`data` for the
                // `with` pair, `x` for `subset`/`transform`); a locally
                // defined function of the same name masks nothing.
                let masking_family: Option<Vec<&str>> = match &self.module.expression(*callee).kind
                {
                    ExpressionKind::NameRef(name)
                        if !self.naming.resolutions.contains_key(callee) =>
                    {
                        base_masking_family(name)
                            .or_else(|| declared_masked_verb(self.masked_verbs, name))
                    }
                    // Namespace access cannot be shadowed by a local binding.
                    // The base family is recognized only under `base`, since
                    // another package's `with` is its own function; a
                    // stub-declared verb is matched by name, as it is bare.
                    ExpressionKind::Namespace {
                        package,
                        name: Some(name),
                        ..
                    } => match package.as_deref() {
                        Some("base") => base_masking_family(name)
                            .or_else(|| declared_masked_verb(self.masked_verbs, name)),
                        _ => declared_masked_verb(self.masked_verbs, name),
                    },
                    _ => None,
                };
                if let Some(leading) = masking_family {
                    // R matches named arguments to formals first and fills the
                    // rest positionally, so a positional argument takes the
                    // next data formal no name has already claimed. Counting
                    // positions without that skip made
                    // `with(data = frame, column_a)` read `column_a` as the
                    // data and evaluate it in the caller's frame, where a
                    // column name is not defined.
                    let claimed_by_name = arguments
                        .iter()
                        .filter_map(|argument| argument.name.as_deref())
                        .filter(|name| leading.contains(name))
                        .count();
                    let mut unclaimed = leading.len().saturating_sub(claimed_by_name);
                    for argument in arguments {
                        let data_argument = match &argument.name {
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
                } else {
                    self.resolve_arguments(arguments);
                }
            }
            ExpressionKind::Index {
                double,
                target,
                arguments,
            } => {
                self.resolve(*target);
                if !*double && self.data_table_signature(arguments) {
                    self.naming.masked_subsets.insert(id);
                    self.quiet_depth += 1;
                    self.resolve_arguments(arguments);
                    self.quiet_depth -= 1;
                } else {
                    self.resolve_arguments(arguments);
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
                            package: package.clone(),
                            name: name.clone(),
                        },
                    );
                }
            }
            ExpressionKind::Function { parameters, body } => {
                let scope_id = self.stable_scope_id(id);
                self.scopes.push(Scope::new(ScopeKind::Function, scope_id));
                let saved_flow = std::mem::take(&mut self.flow);
                let parameters = parameters.clone();
                for parameter in &parameters {
                    let slot =
                        self.mint_binding(&parameter.name, parameter.range, BindingKind::Parameter);
                    self.scopes
                        .last_mut()
                        .expect("scope stack is never empty")
                        .slots
                        .insert(parameter.name.clone(), slot);
                    self.record_write(slot, Reach::Implicit);
                }
                self.premint_frame_assignments(*body);
                for parameter in &parameters {
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
                let after_then = std::mem::replace(&mut self.flow, entry);
                if let Some(else_branch) = else_branch {
                    self.resolve(*else_branch);
                }
                join_flow(&mut self.flow, &after_then);
            }
            ExpressionKind::For {
                variable,
                variable_range,
                sequence,
                body,
            } => {
                self.resolve(*sequence);
                if let (Some(variable), Some(range)) = (variable, variable_range) {
                    let slot = self.mint_binding(variable, *range, BindingKind::ForVariable);
                    self.scopes
                        .last_mut()
                        .expect("scope stack is never empty")
                        .slots
                        .insert(variable.clone(), slot);
                    self.record_write(slot, Reach::Implicit);
                }
                self.loop_body(*body, true);
            }
            ExpressionKind::While { condition, body } => {
                self.resolve(*condition);
                self.loop_body(*body, true);
            }
            ExpressionKind::Repeat { body } => {
                // `repeat` runs at least once: the exit state applies without
                // joining the never-entered path.
                self.loop_body(*body, false);
            }
            ExpressionKind::Local { body } => {
                // Reads fall through to enclosing scopes as ordinary reads
                // and control flow runs straight through; only the bindings
                // are scoped.
                let scope_id = self.stable_scope_id(id);
                self.scopes.push(Scope::new(ScopeKind::Local, scope_id));
                self.premint_frame_assignments(*body);
                self.resolve(*body);
                self.scopes.pop();
            }
            ExpressionKind::Block { statements, .. } => {
                let statements = statements.clone();
                self.resolve_many(&statements);
            }
            ExpressionKind::Paren(inner) => self.resolve(*inner),
        }
    }

    /// `switch(EXPR, ...)` as the branch it is: the subject evaluates in the
    /// caller's frame, then every alternative runs from the state the switch
    /// was entered in, and their end states join.
    ///
    /// Whether a **default** exists decides what a read afterwards sees. R
    /// takes a single unnamed alternative as the default; with named
    /// alternatives only, an unmatched key runs nothing and returns invisible
    /// `NULL`, so a name introduced in the branches is genuinely undefined on
    /// that path, and R says `object 'r' not found`. The all-unnamed form
    /// selects
    /// positionally and can also match nothing, so it is treated as having no
    /// default rather than guessing which argument was meant as one.
    fn resolve_switch(&mut self, arguments: &[Argument]) {
        let subject = arguments
            .iter()
            .position(|argument| argument.name.as_deref() == Some("EXPR"))
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
            // An empty alternative falls through to the next one, as in
            // `switch(k, a = , b = 2)`, so it contributes no writes of its
            // own.
            let Some(value) = alternative.value else {
                continue;
            };
            self.flow = entry.clone();
            self.resolve(value);
            // A branch that cannot fall through never reaches the code after
            // the switch, so its state is not part of what follows.
            if diverges(self.module, value) {
                continue;
            }
            match &mut joined {
                Some(state) => join_flow(state, &self.flow),
                None => joined = Some(std::mem::take(&mut self.flow)),
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
        for argument in arguments.iter() {
            if let Some(value) = argument.value {
                self.resolve(value);
            }
        }
    }

    /// A `[` bracket carrying an unambiguous data.table signature: a `by =` /
    /// `keyby =` argument, a `:=` column assignment, a `.()` list call, or a
    /// `.SD`-family special anywhere in its index arguments.
    fn data_table_signature(&self, arguments: &[Argument]) -> bool {
        arguments.iter().any(|argument| {
            if matches!(argument.name.as_deref(), Some("by") | Some("keyby")) {
                return true;
            }
            argument
                .value
                .is_some_and(|value| self.data_table_marker(value))
        })
    }

    fn data_table_marker(&self, id: ExprId) -> bool {
        let kind = &self.module.expression(id).kind;
        match kind {
            ExpressionKind::Call { callee, .. }
                if matches!(
                    &self.module.expression(*callee).kind,
                    ExpressionKind::NameRef(name) if name == ":="
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
            ExpressionKind::Call { callee, .. }
                if matches!(
                    &self.module.expression(*callee).kind,
                    ExpressionKind::NameRef(name) if name == "."
                ) =>
            {
                true
            }
            // Markers can sit anywhere inside the index expressions, but a
            // nested function body is its own evaluation context.
            ExpressionKind::Function { .. } => false,
            _ => kind
                .child_ids()
                .iter()
                .any(|&child| self.data_table_marker(child)),
        }
    }

    /// A loop body re-walks to a fixed point (the reach lattice is finite and
    /// joins only grow); effects are recorded only on the converged final
    /// pass. `may_skip` joins the never-entered path back in (`for`/`while`;
    /// `repeat` runs at least once).
    fn loop_body(&mut self, body: ExprId, may_skip: bool) {
        let entry = self.flow.clone();
        let saved_emit = self.emit;
        let enclosing_exits = std::mem::take(&mut self.loop_exits);
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
        // The final pass over the converged state records the diagnostics
        // once. It is also the only pass whose `break` states describe the
        // converged loop.
        let converged = self.flow.clone();
        self.loop_exits.clear();
        self.resolve(body);
        let breaks = std::mem::replace(&mut self.loop_exits, enclosing_exits);

        // The loop is left either by falling off the end of the body or
        // through a `break`, so the state after it is the join of those. A
        // loop that may be skipped entirely can also be left at its head.
        for exit in &breaks {
            join_flow(&mut self.flow, exit);
        }
        if may_skip {
            join_flow(&mut self.flow, &entry);
        } else if breaks.is_empty() {
            // No `break` anywhere: the only way out is a jump the walk does
            // not model (`return`, a condition), so keep the conservative
            // head-state join rather than claim the body always completed.
            join_flow(&mut self.flow, &converged);
        }
    }

    /// Pre-mint slots for every direct assignment target (and `for` variable)
    /// of a frame's body, so a closure defined earlier in the frame can
    /// resolve a name written later, because the closure runs after the frame
    /// has executed. A nested function body and a `local()` body bind their own
    /// frames.
    fn premint_frame_assignments(&mut self, id: ExprId) {
        let kind = self.module.expression(id).kind.clone();
        match &kind {
            ExpressionKind::Function { .. } | ExpressionKind::Local { .. } => {}
            ExpressionKind::Assign {
                spelling,
                target,
                value,
            } => {
                if *spelling != AssignSpelling::Super
                    && let ExpressionKind::NameRef(name) = &self.module.expression(*target).kind
                {
                    let range = self.module.expression(*target).range;
                    self.premint_slot(name, range, BindingKind::Local);
                }
                self.premint_frame_assignments(*value);
            }
            ExpressionKind::For {
                variable,
                variable_range,
                sequence,
                body,
            } => {
                if let (Some(variable), Some(range)) = (variable, variable_range) {
                    self.premint_slot(variable, *range, BindingKind::ForVariable);
                }
                self.premint_frame_assignments(*sequence);
                self.premint_frame_assignments(*body);
            }
            _ => {
                for child in kind.child_ids() {
                    self.premint_frame_assignments(child);
                }
            }
        }
    }

    fn premint_slot(&mut self, name: &str, range: TextRange, kind: BindingKind) {
        if self
            .scopes
            .last()
            .expect("scope stack is never empty")
            .slots
            .contains_key(name)
        {
            return;
        }
        let slot = self.mint_binding(name, range, kind);
        self.scopes
            .last_mut()
            .expect("scope stack is never empty")
            .slots
            .insert(name.to_owned(), slot);
    }

    /// The slot half of a read: find the name's slot and mark every write that
    /// reaches it used. This is separate from [`Self::resolve_read`] because a
    /// `%op%` read has a name but no expression node of its own. The operator
    /// is a token, so the read can mark a slot used but has no id to
    /// resolve.
    ///
    /// A slot resolves when a write can reach the read, or when the read
    /// crosses a function boundary (a capture: the closure runs after the
    /// frame has executed, so every frame write is observable). An
    /// unassignable slot does not shadow, so the lookup falls outward, exactly
    /// as it does at R's runtime.
    fn mark_slot_read(&mut self, name: &str) -> Option<SlotRead> {
        let function_depth = self.current_function_depth();
        let (depth, slot) = self
            .scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(depth, scope)| {
                let &slot = scope.slots.get(name)?;
                (self.flow.contains_key(&slot) || depth < function_depth).then_some((depth, slot))
            })?;
        match self.naming.bindings.get(&slot).map(|binding| binding.kind) {
            Some(BindingKind::Parameter) => {
                self.read_parameter_slots.insert(slot);
            }
            Some(BindingKind::TopLevel) => {
                self.naming.used_top_level_names.insert(name.to_owned());
            }
            _ => {}
        }
        // Mark every reaching write used; an unassigned reaching path makes the
        // read maybe-undefined.
        let mut maybe_undefined = false;
        if let Some(reaches) = self.flow.get(&slot) {
            let reaches: Vec<Reach> = reaches.iter().copied().collect();
            for reach in reaches {
                match reach {
                    Reach::Assignment(index) => {
                        self.writes[index as usize].used = true;
                    }
                    Reach::Unassigned => maybe_undefined = true,
                    Reach::Implicit => {}
                }
            }
        }
        if depth < self.current_function_depth() || self.deferred_depth > 0 {
            // A read of an enclosing frame's slot from inside a function is a
            // capture. Every write of the name IN THAT FRAME stays observable,
            // because sequential rebindings are one runtime variable, so none
            // of them is a dead store. A same-named binding in a DIFFERENT
            // frame is not what this closure reads, so a shadowed outer binding
            // stays reportably dead.
            self.naming.captured_slots.insert(slot);
            let frame = self.scopes[depth].id;
            for index in 0..self.writes.len() {
                if self.writes[index].scope == frame && self.writes[index].name == *name {
                    self.writes[index].used = true;
                }
            }
        }
        Some(SlotRead {
            slot,
            maybe_undefined,
        })
    }

    fn resolve_read(&mut self, id: ExprId, name: &str) {
        if name == "..." || name.starts_with("..") && name[2..].chars().all(|c| c.is_ascii_digit())
        {
            return;
        }
        match self.mark_slot_read(name) {
            Some(SlotRead {
                slot,
                maybe_undefined,
            }) => {
                self.naming.resolutions.insert(id, slot);
                if maybe_undefined && self.emit {
                    self.naming.maybe_undefined.insert(id);
                }
            }
            None => {
                if self.emit {
                    if self.quiet_depth == 0 {
                        self.naming.non_locals.insert(id, name.to_owned());
                        if self.current_function_depth() > 0 {
                            self.naming.deferred_non_locals.insert(id);
                        }
                    } else {
                        self.naming.quiet_reads.insert(id, name.to_owned());
                        if self.current_function_depth() > 0 {
                            self.naming.deferred_quiet_reads.insert(id);
                        }
                    }
                }
            }
        }
    }

    fn resolve_assignment_target(
        &mut self,
        assignment: ExprId,
        target: ExprId,
        spelling: AssignSpelling,
    ) {
        // A quoting form's argument is data, not code: R builds the expression
        // and binds nothing, so neither does this. The target still resolves as
        // a read where it names something that exists, which is what an editor
        // needs inside `quote(x <- f(y))`.
        if self.quoted_depth > 0 {
            self.resolve(target);
            return;
        }
        let target_expression = self.module.expression(target).clone();
        match &target_expression.kind {
            // A string where a name belongs binds that name: `"x" <- 1` is
            // `x <- 1` in R, and the literal already carries the name it
            // spells with its quotes stripped. Backticks never reach here,
            // because `` `y` <- 1 `` lowers to a `NameRef` like any other
            // name.
            ExpressionKind::NameRef(name) | ExpressionKind::Literal(LiteralKind::String(name)) => {
                let range = target_expression.range;
                match spelling {
                    AssignSpelling::Super => {
                        // `<<-` binds to the nearest slot in a *strictly
                        // enclosing* scope; without one it targets the global
                        // environment (non-local).
                        //
                        // The bound is the scope stack minus the current scope,
                        // not the enclosing function: `local()` opens a scope
                        // that is not a function, so stopping at the nearest
                        // function boundary skipped that function's own frame
                        // and sent `function() { v <- 1; local({ v <<- 2 }) }`
                        // to the global environment instead of to `v`. Where
                        // the current scope *is* the function frame the two
                        // bounds coincide, which is why the closure spelling of
                        // the same thing was always right.
                        let enclosing = (0..self.scopes.len() - 1)
                            .rev()
                            .find_map(|depth| Some((depth, *self.scopes[depth].slots.get(name)?)));
                        match enclosing {
                            Some((depth, slot)) => {
                                self.naming.resolutions.insert(target, slot);
                                self.naming.captured_slots.insert(slot);
                                // The write the slot came from is what makes
                                // `<<-` find a slot at all: delete it and the
                                // write escapes to the global environment
                                // instead. So a super-assignment keeps that
                                // frame's writes alive exactly as a capturing
                                // read does. Otherwise the initializer of a
                                // write-only counter reports as a dead store,
                                // and removing it changes what the program
                                // does.
                                let frame = self.scopes[depth].id;
                                for write in &mut self.writes {
                                    if write.scope == frame && write.name == *name {
                                        write.used = true;
                                    }
                                }
                                self.record_write_site(assignment, target, name, range, slot, true);
                            }
                            None => {
                                if self.emit {
                                    self.naming.non_locals.insert(target, name.clone());
                                    self.naming.super_globals.insert(name.clone());
                                }
                            }
                        }
                    }
                    _ => {
                        let scope_kind =
                            self.scopes.last().expect("scope stack is never empty").kind;
                        let kind = match scope_kind {
                            ScopeKind::TopLevel => BindingKind::TopLevel,
                            ScopeKind::Function | ScopeKind::Local => BindingKind::Local,
                        };
                        let slot = match self
                            .scopes
                            .last()
                            .expect("scope stack is never empty")
                            .slots
                            .get(name)
                        {
                            Some(&slot) => slot,
                            None => {
                                let slot = self.mint_binding(name, range, kind);
                                self.scopes
                                    .last_mut()
                                    .expect("scope stack is never empty")
                                    .slots
                                    .insert(name.clone(), slot);
                                slot
                            }
                        };
                        self.naming.resolutions.insert(target, slot);
                        let reportable = kind == BindingKind::Local;
                        self.record_write_site(assignment, target, name, range, slot, reportable);
                    }
                }
            }
            // A replacement form, such as `attr(x, "a") <- v` or
            // `x$field <- v`, reads its target's base as a value, so an
            // unresolved base is a reportable read. It then rebinds the base in
            // the current scope, because R evaluates `x[i] <- v` as
            // `x <- \`[<-\`(x, i, v)`. An occurrence after the first write
            // therefore resolves silently.
            _ => match self.replacement_base(target) {
                Some((base_expression, name, range)) => {
                    self.resolve(base_expression);
                    let scope_kind = self.scopes.last().expect("scope stack is never empty").kind;
                    let kind = match scope_kind {
                        ScopeKind::TopLevel => BindingKind::TopLevel,
                        ScopeKind::Function | ScopeKind::Local => BindingKind::Local,
                    };
                    let current = self
                        .scopes
                        .last()
                        .expect("scope stack is never empty")
                        .slots
                        .get(&name)
                        .copied();
                    let slot = match current {
                        Some(slot) => slot,
                        None => {
                            let slot = self.mint_binding(&name, range, kind);
                            self.scopes
                                .last_mut()
                                .expect("scope stack is never empty")
                                .slots
                                .insert(name.clone(), slot);
                            slot
                        }
                    };
                    self.naming.resolutions.insert(base_expression, slot);
                    self.record_write_site(assignment, base_expression, &name, range, slot, false);
                    self.resolve_replacement_subscripts(target, base_expression);
                }
                // R accepts a name, a string (`"x" <- 1` binds `x`), and a
                // replacement call bottoming out at one. A computed value or a
                // number is refused, and refused at run time, so the parse
                // looks clean. The reads inside the target would otherwise be
                // reported as though the author had written them
                // deliberately. That is
                // what a dangling operator on the previous line produces: the
                // line below is swallowed as the target, and its names come
                // back `unresolved` while nothing mentions the operator.
                None if self.emit && refuses_assignment(&target_expression.kind) => {
                    // Recovery can leave the operands out of order or
                    // overlapping, in which case there is no gap to read.
                    let operand_gap = match &target_expression.kind {
                        ExpressionKind::Binary { lhs, rhs, .. } => {
                            let start = self.module.expression(*lhs).range.end();
                            let end = self.module.expression(*rhs).range.start();
                            (start <= end).then(|| TextRange::new(start, end))
                        }
                        _ => None,
                    };
                    self.naming
                        .invalid_assignment_targets
                        .push(InvalidAssignmentTarget {
                            range: target_expression.range,
                            operand_gap,
                        });
                }
                None => self.resolve(target),
            },
        }
    }

    /// The base `NameRef` at the bottom of a replacement target chain
    /// (`x` in `attr(x$f, "a")[i] <- v`).
    fn replacement_base(&self, target: ExprId) -> Option<(ExprId, String, TextRange)> {
        let expression = self.module.expression(target);
        match &expression.kind {
            ExpressionKind::NameRef(name) => Some((target, name.clone(), expression.range)),
            ExpressionKind::Field { target: inner, .. }
            | ExpressionKind::Index { target: inner, .. } => self.replacement_base(*inner),
            ExpressionKind::Call { arguments, .. } => arguments
                .first()
                .and_then(|argument| argument.value)
                .and_then(|value| self.replacement_base(value)),
            _ => None,
        }
    }

    /// Resolves everything in a replacement target except the (already
    /// handled) base name: indexes, further call arguments, callees.
    fn resolve_replacement_subscripts(&mut self, target: ExprId, base: ExprId) {
        if target == base {
            return;
        }
        let expression = self.module.expression(target).clone();
        match &expression.kind {
            ExpressionKind::Field { target: inner, .. } => {
                self.resolve_replacement_subscripts(*inner, base);
            }
            ExpressionKind::Index {
                target: inner,
                arguments,
                ..
            } => {
                self.resolve_replacement_subscripts(*inner, base);
                for argument in arguments {
                    if let Some(value) = argument.value {
                        self.resolve(value);
                    }
                }
            }
            ExpressionKind::Call { callee, arguments } => {
                // The function actually invoked is the `name<-` replacement
                // form, which is a different name from the callee as written.
                // Resolution still runs for the IDE, but an unresolved callee
                // here is not a reportable finding.
                self.quiet_depth += 1;
                self.resolve(*callee);
                self.quiet_depth -= 1;
                let mut arguments = arguments.iter();
                if let Some(first) = arguments.next()
                    && let Some(value) = first.value
                {
                    self.resolve_replacement_subscripts(value, base);
                }
                for argument in arguments {
                    if let Some(value) = argument.value {
                        self.resolve(value);
                    }
                }
            }
            _ => self.resolve(target),
        }
    }

    fn record_write_site(
        &mut self,
        assignment: ExprId,
        _target: ExprId,
        name: &str,
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
                    .find(|scope| scope.slots.get(name) == Some(&slot))
                    .map(|scope| scope.id)
                    .unwrap_or_else(|| self.scopes.last().expect("scope stack is never empty").id);
                self.writes.push(AssignmentWrite {
                    name: name.to_owned(),
                    range,
                    scope,
                    used: false,
                    reportable,
                });
                self.write_by_expression.insert(assignment, index);
                index
            }
        };
        // A write to a captured slot stays observable through the closure.
        if self.naming.captured_slots.contains(&slot) {
            self.writes[index as usize].used = true;
        }
        self.record_write(slot, Reach::Assignment(index));
    }

    fn collect_unused(&mut self) {
        for write in &self.writes {
            // A `.`- or `_`-prefixed name is conventionally an intentional
            // hold-over (hidden helpers, ignored results); the convention
            // opts it out of dead-store reporting.
            if write.reportable
                && !write.used
                && !write.name.starts_with('.')
                && !write.name.starts_with('_')
            {
                self.naming.unused_assignments.push(UnusedAssignment {
                    name: write.name.clone(),
                    range: write.range,
                });
            }
        }
        for binding in self.naming.bindings.values() {
            // Dots reads never resolve (`resolve_read` skips `...`/`..1`), so
            // dots parameters cannot be marked and are exempt instead.
            if binding.kind == BindingKind::Parameter
                && binding.name != "..."
                && !self.read_parameter_slots.contains(&binding.id)
            {
                self.naming.unused_parameters.push(UnusedAssignment {
                    name: binding.name.clone(),
                    range: binding.range,
                });
            }
        }
    }
}

/// A verb the stub corpus declares `@masked`, and the names of the formals it
/// declares before its `...`. Those are the data arguments, and they resolve
/// normally. An empty list masks every argument. This is a free function rather
/// than a method, so the borrowed names live as long as the corpus map rather
/// than as long as the walker.
fn declared_masked_verb<'a>(
    masked_verbs: &'a FxHashMap<String, Vec<String>>,
    name: &str,
) -> Option<Vec<&'a str>> {
    masked_verbs
        .get(name)
        .map(|leading| leading.iter().map(String::as_str).collect())
}

/// Whether an expression cannot fall through to what follows it. The same rule
/// the checker applies, kept in step with it: a branch that ends the call or
/// leaves the loop contributes no state to the code after the construct it
/// sits in.
fn diverges(module: &Module, id: ExprId) -> bool {
    match &module.expression(id).kind {
        ExpressionKind::Break | ExpressionKind::Next => true,
        ExpressionKind::Call { callee, .. } => matches!(
            &module.expression(*callee).kind,
            ExpressionKind::NameRef(name) if name == "return" || name == "stop"
        ),
        ExpressionKind::Block { statements, .. } => statements
            .last()
            .is_some_and(|&last| diverges(module, last)),
        ExpressionKind::If {
            then_branch,
            else_branch: Some(else_branch),
            ..
        } => diverges(module, *then_branch) && diverges(module, *else_branch),
        ExpressionKind::Paren(inner) => diverges(module, *inner),
        _ => false,
    }
}

/// The base data-masking verbs, and the names of the formals that hold the
/// data rather than the masked expression. Every other argument evaluates in
/// the data's frame, so a name there is a column and not a variable.
fn base_masking_family(name: &str) -> Option<Vec<&'static str>> {
    match name {
        "with" | "within" => Some(vec!["data"]),
        "subset" | "transform" => Some(vec!["x"]),
        _ => None,
    }
}

/// Join two control-flow paths: union the reaching sets slot-wise. A slot
/// present on only one path was unassigned on the other, so the missing side
/// contributes `Unassigned`.
fn join_flow(into: &mut FlowState, other: &FlowState) {
    for (slot, reaches) in other {
        match into.entry(*slot) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.get_mut().extend(reaches.iter().copied());
            }
            std::collections::btree_map::Entry::Vacant(vacant) => {
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

/// Whether R refuses to assign to a target of this shape. Only the shapes it
/// certainly refuses: a computed value, or a literal that is not a string. A
/// parenthesized target is left alone, because R's own handling of `(x) <- 1`
/// is odd enough that refusing it here would be guessing. A `!`-headed target
/// is left alone too. `!` binds tighter than `<-`, so metaprogramming that
/// *builds* an assignment with the unquote operator, as `expr(!!name <- value)`
/// does, parses as an assignment to `!!name`, and nothing is ever assigned
/// there.
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
    use super::*;
    use crate::hir::lower_item;

    fn naming_of(source: &str) -> ItemNaming {
        let parse = syntax::parse(source);
        let root = parse.syntax_node();
        let item = root.children().next().expect("one top-level item");
        let module = lower_item(&item);
        resolve_item(&module)
    }

    #[test]
    fn conditional_write_joins_and_read_resolves() {
        let naming = naming_of("f <- function(flag) {\n  x <- 1\n  if (flag) x <- 2\n  x\n}");
        // Both writes reach the final read: neither is a dead store.
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
