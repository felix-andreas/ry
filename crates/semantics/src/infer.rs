//! Unification stays syntactic: a union unifies only with an equal union, plus
//! the member-wise `T | NULL` vs `U | NULL` case. Directional member-wise logic
//! belongs in compatibility checking, never here.

use {
    crate::{
        Db, TypeDefinitions,
        annotations::NamedDefinition,
        check::comparison_operand_shape,
        diagnostics::nearest_name,
        stubs::stubs,
        types::{
            Atomic, Binder, ComparedPair, Constraint, FamilyResult, FrameKind, FunctionType,
            InferenceVar, Name, NativeField, NativeScalar, RecordField, Ty, TyKind, array,
            array_element, contains_inference_var, family_result, for_each_child_type,
            frame_with_row, holds_ry_value, is_ry_value_type, map_child_types,
            native_record_with_row, native_scalar, r_column_type, scalar, substitute_rigid,
            union_of, unknown,
        },
    },
    rustc_hash::{FxHashMap, FxHashSet},
    smallvec::SmallVec,
    std::{
        cell::{Cell, RefCell},
        mem,
    },
    syntax::TextRange,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry<'db> {
    Unbound {
        level: u32,
        constraint: Constraint,
        /// The type every binding must fit, from an open argument passed where a union is
        /// expected (`vapply(path, ...)`): the variable keeps naming the argument's own type,
        /// where binding it to the union would hand every caller back the whole union.
        bound: Option<Ty<'db>>,
    },
    /// `widening` records how a later argument of another R type may rebind the variable.
    Bound {
        ty: Ty<'db>,
        widening: Widening,
    },
    Redirect(InferenceVar),
}

/// How a bound variable meets a later argument of another R type. A ry value never widens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Widening {
    Fixed,
    /// Numeric-constrained when it bound, so a wider number rebinds it (`f(1L, 0.5)`).
    Numeric,
    /// Bound under a comparison or equality constraint, so another comparable R type joins it
    /// as a union (`Bool.is_eq(1L, "a")` for `<T: eq> fn(a: T, b: T)`): R compares across types.
    Compared {
        equality: bool,
    },
    /// Settled as the outcome a callback keeps (`Compared<T> = T`), so another R type joins it
    /// as a union rather than rebinding it.
    Joined,
    /// Settled as R's outcome of arithmetic a fold's callback does on its accumulator
    /// (`Promoted<T> = T` binds `double`). The first value that meets it settles it: one that
    /// fits keeps `double`, and an init the variable's earlier `constraint` admits rebinds it
    /// to what the fold returns from that init: a number vector promotes (`integer[]` gives
    /// `double[]`), and a class with arithmetic (`Date`) or a ry number keeps its type.
    Folded {
        constraint: Constraint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifyError<'db> {
    Mismatch(Ty<'db>, Ty<'db>),
    Occurs(InferenceVar, Ty<'db>),
    ConstraintRejected(Constraint, Ty<'db>),
}

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    entries: usize,
    undo: usize,
    comparisons: usize,
}

/// A comparison of two operands that stay separate types, waiting for the function that
/// encloses it to keep it in its signature or to find its two sides settled.
#[derive(Debug, Clone, Copy)]
pub struct PendingComparison<'db> {
    pub pair: ComparedPair<'db>,
    pub range: TextRange,
    /// The operator of a comparison written in the function, or `None` for one a call left open.
    pub operator: Option<&'static str>,
}

#[derive(Debug, Default)]
pub struct InferenceTable<'db> {
    entries: Vec<Entry<'db>>,
    undo: Vec<(InferenceVar, Entry<'db>)>,
    /// The variables that type a literal whose use decides its type, oldest first, each with
    /// whether its default is a ry type (a `List` item) rather than R's (a variable written with
    /// a literal). The table owns them so that a rollback drops a discarded variable together
    /// with its entry: a stale record would default whichever variable reuses its id.
    literals: Vec<(InferenceVar, bool)>,
    /// Owned by the table, like `literals`, so that a rollback drops the comparisons of the
    /// discarded inference together with the variables they name.
    pub comparisons: Vec<PendingComparison<'db>>,
    pub level: u32,
    /// `None` (no globals at all) reads as an empty table.
    pub definitions: Option<TypeDefinitions<'db>>,
    /// Includes project classes: operator dispatch resolves methods through the
    /// global scope, so the numeric constraint must consult the same set.
    pub arithmetic_classes: Option<crate::ArithmeticClasses<'db>>,
    /// Declared bounds of rigid binders in scope (`<T: numeric>`): a rigid `T` is
    /// not a numeric atom, so constraint admission needs its declared bound.
    pub rigid_constraints: FxHashMap<Name<'db>, Constraint>,
    /// Declared type bounds of rigid binders in scope (`<U: list[T] | T[]>`): a rigid `U`
    /// fits wherever its bound does.
    pub rigid_bounds: FxHashMap<Name<'db>, Ty<'db>>,
    named_types: RefCell<NamedTypes<'db>>,
}

/// What each type name the item has asked about is, in the order it first asked. An item
/// names a handful of types and asks about each hundreds of times, while `definitions`
/// answers through two salsa fetches and a scan of the file's definitions, so the answers
/// are kept for the item: two parallel lists, searched front to back.
#[derive(Debug, Default)]
struct NamedTypes<'db> {
    names: SmallVec<[Name<'db>; 8]>,
    definitions: SmallVec<[Option<&'db NamedDefinition<'db>>; 8]>,
}

/// The state of one call of [`InferenceTable::resolve`].
#[derive(Default)]
struct Resolution<'db> {
    /// The variables and aliases the walk is inside of, innermost last.
    visiting: SmallVec<[InferenceVar; 16]>,
    steps: u32,
    /// The subtrees finished since the walk passed [`UNMEMOIZED_RESOLUTION_STEPS`], each
    /// with its resolution.
    done: FxHashMap<Ty<'db>, Ty<'db>>,
}

thread_local! {
    /// Per thread, because a counter every checking thread bumps on each step costs more
    /// than the step when the threads contend for it.
    static RESOLVE_CALLS: Cell<u64> = const { Cell::new(0) };
}

/// Resolution steps this thread has taken, read by the perf witnesses: must stay
/// near-linear in corpus size.
pub fn resolve_calls() -> u64 {
    RESOLVE_CALLS.get()
}

/// Past any alias nested by hand; bounds a self-referential alias.
const ALIAS_EXPANSION_LIMIT: usize = 64;

/// Types and bindings share subtrees, so a resolution that keeps no table of the subtrees it
/// has finished walks a shared one once per path, exponential in depth. Most resolutions end
/// within a few steps, where the table costs more than the walk, so a resolution starts
/// keeping one only when it has taken this many steps. The steps are counted rather than
/// read off the type's size, because a variable's binding and an alias's expansion are
/// outside the type the walk started from.
const UNMEMOIZED_RESOLUTION_STEPS: u32 = 64;

/// The name of the type an overload probe substitutes for an unchecked argument. Only `Any`
/// and an unconstrained type variable accept it, and no source annotation can spell it.
pub(crate) const UNCHECKED_WITNESS: &str = " unchecked argument";

impl<'db> InferenceTable<'db> {
    pub fn fresh(&mut self, constraint: Constraint) -> InferenceVar {
        let var = InferenceVar(self.entries.len() as u32);
        self.entries.push(Entry::Unbound {
            level: self.level,
            constraint,
            bound: None,
        });
        var
    }

    pub fn fresh_ty(&mut self, db: &'db dyn Db, constraint: Constraint) -> Ty<'db> {
        let var = self.fresh(constraint);
        Ty::new(db, TyKind::Var(var))
    }

    /// A fresh variable for a literal whose use decides its type; `native` makes its default a
    /// ry type.
    pub fn fresh_literal(&mut self, constraint: Constraint, native: bool) -> InferenceVar {
        let var = self.fresh(constraint);
        self.literals.push((var, native));
        var
    }

    /// The literal variables, oldest first, with whether each defaults to a ry type.
    pub fn literals(&self) -> &[(InferenceVar, bool)] {
        &self.literals
    }

    /// The literal variable that `representative` stands for, if any.
    pub fn literal_of(&self, representative: InferenceVar) -> Option<(InferenceVar, bool)> {
        self.literals
            .iter()
            .find(|(literal, _)| self.find(*literal) == representative)
            .copied()
    }

    /// The id the next fresh variable gets, so a caller can tell the variables it creates
    /// from here on.
    pub fn next_var(&self) -> InferenceVar {
        InferenceVar(self.entries.len() as u32)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self.entries.len(),
            undo: self.undo.len(),
            comparisons: self.comparisons.len(),
        }
    }

    pub fn rollback(&mut self, snapshot: Snapshot) {
        while self.undo.len() > snapshot.undo {
            let (var, previous) = self.undo.pop().expect("undo length checked");
            if (var.0 as usize) < snapshot.entries {
                self.entries[var.0 as usize] = previous;
            }
        }
        self.entries.truncate(snapshot.entries);
        self.comparisons.truncate(snapshot.comparisons);
        // Ids grow with creation, so the discarded literals are the newest.
        while self
            .literals
            .last()
            .is_some_and(|(var, _)| var.0 as usize >= snapshot.entries)
        {
            self.literals.pop();
        }
    }

    fn set(&mut self, var: InferenceVar, entry: Entry<'db>) {
        let previous = mem::replace(&mut self.entries[var.0 as usize], entry);
        self.undo.push((var, previous));
    }

    pub fn find(&self, var: InferenceVar) -> InferenceVar {
        let mut current = var;
        loop {
            match &self.entries[current.0 as usize] {
                Entry::Redirect(next) => current = *next,
                _ => return current,
            }
        }
    }

    /// Whether `var` is a variable of this table that nothing has bound or merged, which is
    /// the only kind a resolved type may still hold.
    pub fn is_unbound_root(&self, var: InferenceVar) -> bool {
        matches!(
            self.entries.get(var.0 as usize),
            Some(Entry::Unbound { .. })
        )
    }

    pub fn entry(&self, var: InferenceVar) -> &Entry<'db> {
        &self.entries[self.find(var).0 as usize]
    }

    pub fn definition(
        &self,
        db: &'db dyn Db,
        name: Name<'db>,
    ) -> Option<&'db NamedDefinition<'db>> {
        let table = self.definitions?;
        let mut named_types = self.named_types.borrow_mut();
        if let Some(position) = named_types.names.iter().position(|&seen| seen == name) {
            return named_types.definitions[position];
        }
        let definition = table.get(db, name);
        named_types.names.push(name);
        named_types.definitions.push(definition);
        definition
    }

    /// An undeclared name or a wrong type-argument count. Both already carry an
    /// annotation diagnostic, so relations treat the type like `Unknown`. The overload
    /// witness is undeclared on purpose and must stay incompatible with every concrete type.
    pub(crate) fn undeclared_nominal(&self, db: &'db dyn Db, ty: Ty<'db>) -> bool {
        let TyKind::Named(name, arguments) = ty.kind(db) else {
            return false;
        };
        if name.text(db) == UNCHECKED_WITNESS {
            return false;
        }
        match self.definition(db, *name) {
            // Includes the bare use of a generic (`Box` for `Box<T>`).
            Some(definition) => definition.parameters.len() != arguments.len(),
            None => !self
                .definitions
                .map_or_else(|| stubs(db), TypeDefinitions::library)
                .is_some_and(|library| library.nominals.contains(name.symbol())),
        }
    }

    pub fn shallow_resolve(&self, db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
        let mut current = ty;
        while let Some(var) = current.as_var(db) {
            let representative = self.find(var);
            match self.entries[representative.0 as usize] {
                Entry::Bound { ty: bound, .. } => current = bound,
                _ if representative == var => return current,
                _ => return Ty::new(db, TyKind::Var(representative)),
            }
        }
        match current.kind(db) {
            TyKind::Family(result, operand) => self.family(db, *result, *operand),
            _ => current,
        }
    }

    /// The result of `result` over `operand` as far as this table decides it: an open operand
    /// whose constraint admits only ry's numbers or only R's types picks that side's outcome.
    pub fn family(&self, db: &'db dyn Db, result: FamilyResult, operand: Ty<'db>) -> Ty<'db> {
        let operand = self.shallow_resolve(db, operand);
        let constraint = match operand.kind(db) {
            TyKind::Var(_) | TyKind::Rigid(_) => Some(self.family_operand_constraint(db, operand)),
            // A nominal or alias over a ry number (`type Count = I64`) computes as the number.
            TyKind::Named(name, arguments) => {
                let expanded = self.resolve(db, operand);
                if expanded != operand {
                    return self.family(db, result, expanded);
                }
                // A ry `List` compares and computes item by item.
                if let Some(item) = array_element(db, operand) {
                    return match result {
                        FamilyResult::Arithmetic => operand,
                        FamilyResult::Comparison => array(db, self.family(db, result, item)),
                    };
                }
                return match self.representation(db, *name, arguments) {
                    Some(representation) if is_ry_value_type(db, representation) => match result {
                        FamilyResult::Arithmetic => operand,
                        FamilyResult::Comparison => self.family(db, result, representation),
                    },
                    _ => result.r_outcome(db),
                };
            }
            _ => None,
        };
        match constraint {
            Some(
                Constraint::Number
                | Constraint::Int
                | Constraint::Frac
                | Constraint::Str
                | Constraint::ArrayItem,
            ) => result.ry_outcome(db, operand),
            // An operand no ry value of the result's kind satisfies is an R value: a number for
            // arithmetic, and any ry value for a comparison (`x == "a"` admits a `Str`).
            Some(constraint)
                if constraint != Constraint::Unconstrained
                    && constraint
                        .join(match result {
                            FamilyResult::Arithmetic => Constraint::Number,
                            FamilyResult::Comparison => Constraint::ArrayItem,
                        })
                        .is_none() =>
            {
                result.r_outcome(db)
            }
            _ => family_result(db, result, operand),
        }
    }

    /// Also expands aliases. Self-referential bindings and aliases cut to
    /// `Unknown` instead of expanding forever.
    pub fn resolve(&self, db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
        if !self.resolves(db, ty) {
            return ty;
        }
        self.resolve_rec(db, ty, &mut Resolution::default()).0
    }

    /// Whether resolving can change `ty`: it holds a variable, or applies a name that is an
    /// alias in this item's view. Both are answered from the type's id and the names it
    /// applies, without walking it.
    fn resolves(&self, db: &'db dyn Db, ty: Ty<'db>) -> bool {
        ty.may_resolve()
            && (contains_inference_var(db, ty)
                || ty.applied_names().iter().any(|&name| {
                    self.definition(db, name)
                        .is_some_and(|definition| definition.alias)
                }))
    }

    /// The bool is whether the subtree is clean (no cycle cut beneath it): only clean results
    /// are the same wherever the subtree occurs, so only those enter `done`.
    fn resolve_rec(
        &self,
        db: &'db dyn Db,
        ty: Ty<'db>,
        resolution: &mut Resolution<'db>,
    ) -> (Ty<'db>, bool) {
        RESOLVE_CALLS.set(RESOLVE_CALLS.get() + 1);
        resolution.steps = resolution.steps.saturating_add(1);
        if let Some(var) = ty.as_var(db) {
            let root = self.find(var);
            let shallow = self.shallow_resolve(db, ty);
            if shallow.is_var() {
                return (shallow, true);
            }
            if resolution.visiting.contains(&root) {
                return (unknown(db), false);
            }
            resolution.visiting.push(root);
            let resolved = self.resolve_rec(db, shallow, resolution);
            resolution.visiting.pop();
            return resolved;
        }

        if !self.resolves(db, ty) {
            return (ty, true);
        }
        if !resolution.done.is_empty()
            && let Some(&finished) = resolution.done.get(&ty)
        {
            return (finished, true);
        }

        let mut clean = true;
        let resolved = map_child_types(db, ty, &mut |child| {
            let (child, child_clean) = self.resolve_rec(db, child, resolution);
            clean &= child_clean;
            child
        });
        let resolved = match resolved.kind(db) {
            TyKind::Family(result, operand) => self.family(db, *result, *operand),
            _ => resolved,
        };
        // Aliases never survive into unification. A self-referential alias re-enters
        // through the same interned application, so the visiting stack bounds it.
        let alias = match resolved.kind(db) {
            TyKind::Named(name, arguments) => self
                .definition(db, *name)
                .filter(|definition| definition.alias)
                .map(|definition| (definition, arguments)),
            _ => None,
        };
        let (resolved, clean) = match alias {
            Some((definition, arguments)) if resolution.visiting.len() < ALIAS_EXPANSION_LIMIT => {
                let expanded = apply_definition(db, definition, arguments);
                // Sentinel slot: alias depth counts against the variable cycle stack.
                resolution.visiting.push(InferenceVar(u32::MAX));
                let (expanded, expanded_clean) = self.resolve_rec(db, expanded, resolution);
                resolution.visiting.pop();
                (expanded, clean && expanded_clean)
            }
            // Left as written past the bound, which other positions must not reuse.
            Some(_) => (resolved, false),
            None => (resolved, clean),
        };
        if clean && resolution.steps > UNMEMOIZED_RESOLUTION_STEPS {
            resolution.done.insert(ty, resolved);
        }
        (resolved, clean)
    }

    /// `None` for aliases, abstract nominals, and unknown names.
    pub fn representation(
        &self,
        db: &'db dyn Db,
        name: Name<'db>,
        arguments: &[Ty<'db>],
    ) -> Option<Ty<'db>> {
        let definition = self.definition(db, name)?;
        if definition.alias || matches!(definition.body.kind(db), TyKind::Unknown) {
            return None;
        }
        Some(apply_definition(db, definition, arguments))
    }

    fn bind(
        &mut self,
        db: &'db dyn Db,
        var: InferenceVar,
        ty: Ty<'db>,
    ) -> Result<(), UnifyError<'db>> {
        let representative = self.find(var);
        if !self.walk_unbound_vars(db, ty, &mut |found| found != representative) {
            return Err(UnifyError::Occurs(representative, ty));
        }
        let Entry::Unbound {
            level,
            constraint,
            bound,
        } = *self.entry(representative)
        else {
            unreachable!("bind target is always unbound after find");
        };
        // An open member of a union takes the bound, as it would bound alone: a `NULL`
        // default joined with a number (`T | double`) passed to arithmetic makes `T` a number.
        if constraint != Constraint::Unconstrained
            && let TyKind::Union(members) = ty.kind(db)
        {
            for &member in members.iter() {
                if let TyKind::Var(member) = self.shallow_resolve(db, member).kind(db)
                    && matches!(self.entry(self.find(*member)), Entry::Unbound { .. })
                {
                    self.constrain(db, *member, constraint)?;
                }
            }
        }
        let ty = match ty.kind(db) {
            TyKind::Family(result, operand) if constraint != Constraint::Unconstrained => {
                self.narrow_family(db, constraint, *result, *operand)?
            }
            _ => ty,
        };
        if let Some(error) = constraint_rejects(db, self, constraint, ty) {
            return Err(error);
        }
        if let Some(bound) = bound
            && !self.compatible(db, ty, bound)
        {
            return Err(UnifyError::Mismatch(ty, bound));
        }
        if matches!(
            constraint.r_side(),
            Constraint::Numeric | Constraint::Arithmetic
        ) && let TyKind::Vector(element) | TyKind::NamedVector(element) = ty.kind(db)
            && let TyKind::Var(element) = self.shallow_resolve(db, *element).kind(db)
        {
            let element = self.find(*element);
            if let Entry::Unbound {
                level: element_level,
                constraint: element_constraint,
                bound: element_bound,
            } = *self.entry(element)
            {
                self.set(
                    element,
                    Entry::Unbound {
                        level: element_level,
                        constraint: element_constraint
                            .join(Constraint::ScalarNumeric)
                            .unwrap_or(element_constraint),
                        bound: element_bound,
                    },
                );
            }
        }
        self.adjust_levels(db, level, ty);
        let widening = match constraint.r_side() {
            Constraint::Numeric | Constraint::Arithmetic | Constraint::ScalarNumeric => {
                Widening::Numeric
            }
            Constraint::Comparable => Widening::Compared { equality: false },
            Constraint::Eq | Constraint::Equatable => Widening::Compared { equality: true },
            _ => Widening::Fixed,
        };
        self.set(representative, Entry::Bound { ty, widening });
        Ok(())
    }

    /// Lowering levels keeps a generalization at an outer level from quantifying
    /// a variable still reachable from an inner one.
    pub(crate) fn adjust_levels(&mut self, db: &'db dyn Db, level: u32, ty: Ty<'db>) {
        if !contains_inference_var(db, ty) {
            return;
        }
        let shallow = self.shallow_resolve(db, ty);
        if let TyKind::Var(var) = shallow.kind(db) {
            let representative = self.find(*var);
            if let Entry::Unbound {
                level: var_level,
                constraint,
                bound,
            } = *self.entry(representative)
                && var_level > level
            {
                self.set(
                    representative,
                    Entry::Unbound {
                        level,
                        constraint,
                        bound,
                    },
                );
                if let Some(bound) = bound {
                    self.adjust_levels(db, level, bound);
                }
            }
            return;
        }
        for_each_child_type(db, shallow, &mut |child| {
            self.adjust_levels(db, level, child)
        });
    }

    pub fn unify(
        &mut self,
        db: &'db dyn Db,
        a: Ty<'db>,
        b: Ty<'db>,
    ) -> Result<(), UnifyError<'db>> {
        let a = self.shallow_resolve(db, a);
        let b = self.shallow_resolve(db, b);
        if a == b {
            return Ok(());
        }
        match (a.kind(db), b.kind(db)) {
            (TyKind::Var(left), TyKind::Var(right)) => {
                let left = self.find(*left);
                let right = self.find(*right);
                if left == right {
                    return Ok(());
                }
                let Entry::Unbound {
                    level: left_level,
                    constraint: left_constraint,
                    bound: left_bound,
                } = *self.entry(left)
                else {
                    unreachable!("shallow resolve leaves only unbound variables")
                };
                let Entry::Unbound {
                    level: right_level,
                    constraint: right_constraint,
                    bound: right_bound,
                } = *self.entry(right)
                else {
                    unreachable!("shallow resolve leaves only unbound variables")
                };
                // A text or logical literal that meets a `List` item takes the ry type, the only
                // member of its constraint a `List` holds.
                let literal_item = match (left_constraint, right_constraint) {
                    (Constraint::StrLiteral, Constraint::ArrayItem)
                    | (Constraint::ArrayItem, Constraint::StrLiteral) => Some(NativeScalar::Str),
                    (Constraint::BoolLiteral, Constraint::ArrayItem)
                    | (Constraint::ArrayItem, Constraint::BoolLiteral) => Some(NativeScalar::Bool),
                    _ => None,
                };
                if let Some(native) = literal_item {
                    let item = native_scalar(db, native);
                    self.bind(db, left, item)?;
                    return self.bind(db, right, item);
                }
                let Some(constraint) = left_constraint.join(right_constraint) else {
                    return Err(UnifyError::Mismatch(a, b));
                };
                // A variable holds a constraint or a bound, never both, and a bound never names
                // its own variable. A meet that would break either settles each bounded
                // variable to its bound and unifies what is left.
                let bound = match (left_bound, right_bound) {
                    (None, None) => None,
                    _ if constraint != Constraint::Unconstrained => {
                        return self.unify_settled(db, a, b, left, right);
                    }
                    (Some(left_bound), Some(right_bound)) => {
                        match self.meet_bounds(db, left_bound, right_bound, 0) {
                            Some(bound) => Some(bound),
                            None => return Err(UnifyError::Mismatch(a, b)),
                        }
                    }
                    (bound, None) | (None, bound) => bound,
                };
                if bound.is_some_and(|bound| {
                    !self.walk_unbound_vars(db, bound, &mut |found| found != left && found != right)
                }) {
                    return self.unify_settled(db, a, b, left, right);
                }
                let (winner, loser) = if left.0 <= right.0 {
                    (left, right)
                } else {
                    (right, left)
                };
                let level = left_level.min(right_level);
                self.set(
                    winner,
                    Entry::Unbound {
                        level,
                        constraint,
                        bound: None,
                    },
                );
                self.set(loser, Entry::Redirect(winner));
                if let Some(bound) = bound {
                    self.adjust_levels(db, level, bound);
                    if !matches!(bound.kind(db), TyKind::Union(_)) {
                        // A bound of one type leaves the variable nothing else to be.
                        return self.bind(db, winner, bound);
                    }
                    self.set(
                        winner,
                        Entry::Unbound {
                            level,
                            constraint,
                            bound: Some(bound),
                        },
                    );
                }
                Ok(())
            }
            (TyKind::Var(var), _) => self.bind(db, *var, b),
            (_, TyKind::Var(var)) => self.bind(db, *var, a),
            // `Any` fits everything and `Unknown` must not cascade. After the Var arms so
            // a variable still binds to them.
            (TyKind::Any, _) | (_, TyKind::Any) | (TyKind::Unknown, _) | (_, TyKind::Unknown) => {
                Ok(())
            }
            (TyKind::Family(left_result, left), TyKind::Family(right_result, right))
                if left_result == right_result =>
            {
                self.unify(db, *left, *right)
            }
            (TyKind::Family(result, operand), _) => {
                self.unify_family(db, *result, *operand, b, UnifyError::Mismatch(a, b))
            }
            (_, TyKind::Family(result, operand)) => {
                self.unify_family(db, *result, *operand, a, UnifyError::Mismatch(a, b))
            }
            (TyKind::Named(..), _) | (_, TyKind::Named(..))
                if self.undeclared_nominal(db, a) || self.undeclared_nominal(db, b) =>
            {
                Ok(())
            }
            (TyKind::Vector(left), TyKind::Vector(right))
            | (TyKind::NamedVector(left), TyKind::NamedVector(right))
            | (TyKind::List(left), TyKind::List(right))
            | (TyKind::NamedList(left), TyKind::NamedList(right)) => self.unify(db, *left, *right),
            (TyKind::Tuple(left), TyKind::Tuple(right)) => {
                if left.len() != right.len() {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (&left, &right) in left.iter().zip(right.iter()) {
                    self.unify(db, left, right)?;
                }
                Ok(())
            }
            (TyKind::Record(left), TyKind::Record(right))
            | (TyKind::OpenRecord(left), TyKind::OpenRecord(right))
            | (TyKind::Environment(left), TyKind::Environment(right)) => {
                if left.len() != right.len()
                    || left
                        .iter()
                        .zip(right.iter())
                        .any(|(l, r)| l.name != r.name || l.optional != r.optional)
                {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (left, right) in left.iter().zip(right.iter()) {
                    self.unify(db, left.ty, right.ty)?;
                }
                Ok(())
            }
            (TyKind::Frame(left_kind, _, _), TyKind::Frame(right_kind, _, _))
                if !same_frame_kind(*left_kind, *right_kind) =>
            {
                Err(UnifyError::Mismatch(a, b))
            }
            // A frame whose columns are unknown makes no claim to check.
            (TyKind::Frame(_, None, _), TyKind::Frame(..))
            | (TyKind::Frame(..), TyKind::Frame(_, None, _)) => Ok(()),
            (TyKind::Frame(_, Some(_), _), TyKind::Frame(_, Some(_), _))
            | (TyKind::NativeRecord(..), TyKind::NativeRecord(..)) => self.unify_open(db, a, b),
            (TyKind::Function(left), TyKind::Function(right)) => {
                if left.positional.len() != right.positional.len()
                    || left.named.len() != right.named.len()
                    || left.variadic.is_some() != right.variadic.is_some()
                    || left
                        .named
                        .iter()
                        .zip(right.named.iter())
                        .any(|(l, r)| l.name != r.name || l.optional != r.optional)
                {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (&left, &right) in left.positional.iter().zip(right.positional.iter()) {
                    self.unify(db, left, right)?;
                }
                for (left, right) in left.named.iter().zip(right.named.iter()) {
                    self.unify(db, left.ty, right.ty)?;
                }
                if let (Some(left), Some(right)) = (&left.variadic, &right.variadic) {
                    self.unify(db, left.element, right.element)?;
                }
                match self
                    .family_fixed_point(db, left.ret, right.ret)
                    .or_else(|| self.family_fixed_point(db, right.ret, left.ret))
                {
                    Some(settled) => settled?,
                    None => self.unify(db, left.ret, right.ret)?,
                }
                // A ry value compares only with its own type, so a pair one function compares
                // makes the other side of it that type.
                for pair in left.compared.iter().chain(right.compared) {
                    let compared_left = self.resolve(db, pair.left);
                    let compared_right = self.resolve(db, pair.right);
                    if (holds_ry_value(db, compared_left) || holds_ry_value(db, compared_right))
                        && self.unify(db, compared_left, compared_right).is_err()
                    {
                        return Err(UnifyError::Mismatch(a, b));
                    }
                }
                Ok(())
            }
            (
                TyKind::Named(left_name, left_arguments),
                TyKind::Named(right_name, right_arguments),
            ) => {
                if left_name != right_name || left_arguments.len() != right_arguments.len() {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (&left, &right) in left_arguments.iter().zip(right_arguments.iter()) {
                    self.unify(db, left, right)?;
                }
                Ok(())
            }
            (TyKind::NativeTuple(left), TyKind::NativeTuple(right)) => {
                if left.len() != right.len() {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (&left, &right) in left.iter().zip(right.iter()) {
                    self.unify(db, left, right)?;
                }
                Ok(())
            }
            (TyKind::Tag(left_name, left_payload), TyKind::Tag(right_name, right_payload))
                if left_name == right_name =>
            {
                match (left_payload, right_payload) {
                    (None, None) => Ok(()),
                    (Some(left), Some(right)) => self.unify(db, *left, *right),
                    _ => Err(UnifyError::Mismatch(a, b)),
                }
            }
            (TyKind::Union(_), TyKind::Union(_)) => self.unify_unions(db, a, b),
            _ => Err(UnifyError::Mismatch(a, b)),
        }
    }

    /// Unifies two tables' columns or two records' fields. A side's row stands for the names it
    /// does not list, so a name only one side lists goes into the other side's row, and when both
    /// sides list names the other lacks, their rows share a fresh row for the rest.
    fn unify_open(
        &mut self,
        db: &'db dyn Db,
        a: Ty<'db>,
        b: Ty<'db>,
    ) -> Result<(), UnifyError<'db>> {
        let mismatch = || UnifyError::Mismatch(a, b);
        let ((shape, left, left_row), (_, right, right_row)) =
            match (self.settled(db, a), self.settled(db, b)) {
                (Settled::Open(left), Settled::Open(right)) if left.0 == right.0 => (left, right),
                (Settled::Unknown, _) | (_, Settled::Unknown) => return Ok(()),
                _ => return Err(mismatch()),
            };
        for (name, ty) in &left {
            if let Some((_, other)) = right.iter().find(|(other, _)| other == name) {
                self.unify(db, *ty, *other)?;
            }
        }
        let only =
            |these: &[(Name<'db>, Ty<'db>)], those: &[(Name<'db>, Ty<'db>)]| -> OpenFields<'db> {
                these
                    .iter()
                    .filter(|(name, _)| !those.iter().any(|(other, _)| other == name))
                    .copied()
                    .collect()
            };
        let left_only = only(&left, &right);
        let right_only = only(&right, &left);
        let absorb = |table: &mut Self, row: Option<Ty<'db>>, rest: Ty<'db>| match row {
            Some(row) => table.unify(db, row, rest),
            None => Err(mismatch()),
        };
        match (left_only.is_empty(), right_only.is_empty()) {
            (true, true) => {
                let closed = row_type(db, shape, &[], None);
                match (left_row, right_row) {
                    (None, None) => Ok(()),
                    (Some(left_row), Some(right_row)) => self.unify(db, left_row, right_row),
                    (Some(row), None) | (None, Some(row)) => self.unify(db, row, closed),
                }
            }
            (false, true) => {
                let rest = row_type(db, shape, &left_only, left_row);
                absorb(self, right_row, rest)
            }
            (true, false) => {
                let rest = row_type(db, shape, &right_only, right_row);
                absorb(self, left_row, rest)
            }
            // One row cannot hold names only the other side lists on both sides at once: binding
            // it to the first rest would ask the same of its fresh tail, forever.
            (false, false) if left_row.is_some() && left_row == right_row => Err(mismatch()),
            (false, false) => {
                let tail = self.fresh_ty(db, Constraint::Unconstrained);
                let rest = row_type(db, shape, &left_only, Some(tail));
                absorb(self, right_row, rest)?;
                let rest = row_type(db, shape, &right_only, Some(tail));
                absorb(self, left_row, rest)
            }
        }
    }

    /// Whether a table or record `actual` fits `expected`: every name `expected` lists must be
    /// there and fit, and the names it does not list must fit its row. A closed `expected` takes
    /// no more names, a type parameter only itself, and a type variable binds to the rest.
    fn open_compatible(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
        depth: usize,
    ) -> bool {
        let ((shape, actual, actual_row), (_, expected, expected_row)) =
            match (self.settled(db, actual), self.settled(db, expected)) {
                (Settled::Open(actual), Settled::Open(expected)) if actual.0 == expected.0 => {
                    (actual, expected)
                }
                (Settled::Unknown, _) | (_, Settled::Unknown) => return true,
                _ => return false,
            };
        let mut missing = OpenFields::new();
        for (name, expected_ty) in &expected {
            match actual.iter().find(|(other, _)| other == name) {
                Some((_, actual_ty)) => {
                    if !self.compatible_probe(db, *actual_ty, *expected_ty, depth + 1) {
                        return false;
                    }
                }
                None => missing.push((*name, *expected_ty)),
            }
        }
        let rest: OpenFields<'db> = actual
            .iter()
            .filter(|(name, _)| !expected.iter().any(|(other, _)| other == name))
            .copied()
            .collect();
        // An inferred value whose row is still a variable, such as a parameter two calls read
        // different names of, takes the names `expected` lists and it lacks into that row.
        let actual_row = match actual_row {
            _ if missing.is_empty() => actual_row,
            Some(row) if self.resolve(db, row).is_var() => {
                if expected_row == Some(row) && !rest.is_empty() {
                    return false;
                }
                let tail = expected_row.map(|_| self.fresh_ty(db, Constraint::Unconstrained));
                if self
                    .unify(db, row, row_type(db, shape, &missing, tail))
                    .is_err()
                {
                    return false;
                }
                tail
            }
            _ => return false,
        };
        match expected_row {
            None => match actual_row {
                _ if !rest.is_empty() => false,
                None => true,
                Some(row) => self.unify(db, row, row_type(db, shape, &[], None)).is_ok(),
            },
            Some(expected_row) => {
                let rest = row_type(db, shape, &rest, actual_row);
                self.unify(db, expected_row, rest).is_ok()
            }
        }
    }

    /// A table's or record's names with every row bound since it was built merged in, so that
    /// its row is an unbound variable, a type parameter, or none. A row bound to something other
    /// than more names of the same kind, as when `R` in `..R` also types a parameter, is
    /// `WrongShape`.
    fn settled(&mut self, db: &'db dyn Db, ty: Ty<'db>) -> Settled<'db> {
        let Some((shape, mut names, mut row)) = open_parts(db, ty) else {
            return match ty.kind(db) {
                TyKind::Unknown | TyKind::Any | TyKind::Frame(_, None, _) => Settled::Unknown,
                _ => Settled::WrongShape,
            };
        };
        while let Some(current) = row {
            let resolved = self.shallow_resolve(db, current);
            if let TyKind::Var(_) | TyKind::Rigid(_) = resolved.kind(db) {
                row = Some(resolved);
                break;
            }
            if let TyKind::Unknown | TyKind::Any = resolved.kind(db) {
                return Settled::Unknown;
            }
            match open_parts(db, resolved) {
                Some((more_shape, more, rest)) if more_shape == shape => {
                    for (name, more_ty) in more {
                        if !names.iter().any(|(listed, _)| *listed == name) {
                            names.push((name, more_ty));
                        }
                    }
                    row = rest;
                }
                _ => return Settled::WrongShape,
            }
        }
        Settled::Open((shape, names, row))
    }

    fn unify_unions(
        &mut self,
        db: &'db dyn Db,
        a: Ty<'db>,
        b: Ty<'db>,
    ) -> Result<(), UnifyError<'db>> {
        let resolved_a = self.resolve(db, a);
        let resolved_b = self.resolve(db, b);
        if resolved_a == resolved_b {
            return Ok(());
        }
        let (TyKind::Union(left), TyKind::Union(right)) =
            (resolved_a.kind(db), resolved_b.kind(db))
        else {
            // Resolution collapsed one side; retry the general path.
            return self.unify(db, resolved_a, resolved_b);
        };
        if let (Some(left_inner), Some(right_inner)) = (
            nullable_single_member(db, left),
            nullable_single_member(db, right),
        ) {
            return self.unify(db, left_inner, right_inner);
        }
        if left.len() == right.len() && left.iter().all(|member| right.contains(member)) {
            return Ok(());
        }
        Err(UnifyError::Mismatch(a, b))
    }

    /// Gives a fresh variable the bound its scheme binder declares.
    pub fn set_bound(&mut self, var: InferenceVar, bound: Ty<'db>) {
        let representative = self.find(var);
        if let Entry::Unbound {
            level, constraint, ..
        } = *self.entry(representative)
        {
            self.set(
                representative,
                Entry::Unbound {
                    level,
                    constraint,
                    bound: Some(bound),
                },
            );
        }
    }

    /// Where two bounds meet: the narrower one, else the members of `existing` that fit
    /// `incoming` (`integer | logical` then `character | logical` leaves `logical`), else
    /// nothing.
    fn meet_bounds(
        &mut self,
        db: &'db dyn Db,
        existing: Ty<'db>,
        incoming: Ty<'db>,
        depth: usize,
    ) -> Option<Ty<'db>> {
        if self.compatible_probe(db, existing, incoming, depth + 1) {
            return Some(existing);
        }
        if self.compatible_probe(db, incoming, existing, depth + 1) {
            return Some(incoming);
        }
        let TyKind::Union(members) = existing.kind(db) else {
            return None;
        };
        let fitting: Vec<Ty<'db>> = members
            .iter()
            .copied()
            .filter(|&member| self.compatible_probe(db, member, incoming, depth + 1))
            .collect();
        (!fitting.is_empty()).then(|| union_of(db, fitting))
    }

    /// Binds a bounded variable to its bound, giving up the argument's own type where a
    /// constraint or a cycle would need the variable to hold both.
    pub fn settle_bound(
        &mut self,
        db: &'db dyn Db,
        var: InferenceVar,
    ) -> Result<(), UnifyError<'db>> {
        let representative = self.find(var);
        let Entry::Unbound {
            level,
            constraint,
            bound: Some(bound),
        } = *self.entry(representative)
        else {
            return Ok(());
        };
        self.set(
            representative,
            Entry::Unbound {
                level,
                constraint,
                bound: None,
            },
        );
        self.bind(db, representative, bound)
    }

    fn unify_settled(
        &mut self,
        db: &'db dyn Db,
        a: Ty<'db>,
        b: Ty<'db>,
        left: InferenceVar,
        right: InferenceVar,
    ) -> Result<(), UnifyError<'db>> {
        self.settle_bound(db, left)?;
        self.settle_bound(db, right)?;
        self.unify(db, a, b)
    }

    /// The bound of an unbound variable, when it has one.
    pub fn bound_of(&self, var: InferenceVar) -> Option<Ty<'db>> {
        match *self.entry(self.find(var)) {
            Entry::Unbound { bound, .. } => bound,
            _ => None,
        }
    }

    /// Puts a declared scheme's binders in scope while its body is checked.
    pub fn enter_rigid_binders(&mut self, binders: &[Binder<'db>]) {
        for binder in binders {
            self.rigid_constraints
                .insert(binder.name, binder.constraint());
            if let Some(bound) = binder.type_bound() {
                self.rigid_bounds.insert(binder.name, bound);
            }
        }
    }

    /// Whether a type parameter of the signature being checked meets a constraint, through its
    /// declared constraint or its type bound.
    pub fn rigid_satisfies(
        &self,
        db: &'db dyn Db,
        name: Name<'db>,
        constraint: Constraint,
    ) -> bool {
        constraint_rejects(db, self, constraint, Ty::new(db, TyKind::Rigid(name))).is_none()
    }

    /// Whether a type parameter's values are arithmetic operands: a `<T: num>` is, though it
    /// does not imply `arithmetic`, which admits R numbers only.
    pub fn rigid_operand(&self, db: &'db dyn Db, name: Name<'db>) -> bool {
        [
            Constraint::Arithmetic,
            Constraint::Num,
            Constraint::ArithmeticOrNumber,
        ]
        .into_iter()
        .any(|constraint| self.rigid_satisfies(db, name, constraint))
    }

    pub fn open_constraint(&self, db: &'db dyn Db, ty: Ty<'db>) -> Option<Constraint> {
        let resolved = self.resolve(db, ty);
        let TyKind::Var(var) = resolved.kind(db) else {
            return None;
        };
        let representative = self.find(*var);
        match *self.entry(representative) {
            Entry::Unbound { constraint, .. } => Some(constraint),
            _ => None,
        }
    }

    pub fn constrain(
        &mut self,
        db: &'db dyn Db,
        var: InferenceVar,
        constraint: Constraint,
    ) -> Result<(), UnifyError<'db>> {
        let representative = self.find(var);
        match *self.entry(representative) {
            Entry::Unbound { bound: Some(_), .. } if constraint != Constraint::Unconstrained => {
                self.settle_bound(db, representative)?;
                self.constrain(db, representative, constraint)
            }
            Entry::Unbound {
                level,
                constraint: existing,
                bound,
            } => {
                let Some(joined) = existing.join(constraint) else {
                    return Err(UnifyError::ConstraintRejected(
                        constraint,
                        Ty::new(db, TyKind::Var(representative)),
                    ));
                };
                if joined != existing {
                    self.set(
                        representative,
                        Entry::Unbound {
                            level,
                            constraint: joined,
                            bound,
                        },
                    );
                }
                Ok(())
            }
            Entry::Bound { ty, .. } => match constraint_rejects(db, self, constraint, ty) {
                Some(error) => Err(error),
                None => Ok(()),
            },
            Entry::Redirect(_) => unreachable!("find returns a representative"),
        }
    }

    /// A family result meets a concrete type, which decides the side of its operand:
    /// `Promoted<T>` is `F64` only when `T` is, and `double` only when `T` is an R number.
    fn unify_family(
        &mut self,
        db: &'db dyn Db,
        result: FamilyResult,
        operand: Ty<'db>,
        other: Ty<'db>,
        mismatch: UnifyError<'db>,
    ) -> Result<(), UnifyError<'db>> {
        match (result, other.kind(db)) {
            (FamilyResult::Arithmetic, TyKind::Native(native)) if native.is_number() => {
                self.unify(db, operand, other)
            }
            (FamilyResult::Comparison, TyKind::Native(NativeScalar::Bool)) => self
                .constrain_family_operand(db, operand, Constraint::ArrayItem)
                .map_err(|_| mismatch),
            (FamilyResult::Arithmetic, TyKind::Scalar(Atomic::Double))
            | (FamilyResult::Comparison, TyKind::Scalar(Atomic::Logical)) => {
                let side = self.family_operand_constraint(db, operand).r_side();
                self.constrain_family_operand(db, operand, side)
                    .map_err(|_| mismatch)
            }
            _ => Err(mismatch),
        }
    }

    /// [`Self::compatible`] for a function's return against the one expected of it.
    pub fn return_compatible(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
    ) -> bool {
        match self.family_fixed_point(db, actual, expected) {
            Some(settled) => settled.is_ok(),
            None => self.compatible(db, actual, expected),
        }
    }

    /// A callback returning `Promoted<T>` (alone or beside `T`) where `T` is expected back, as
    /// a fold's accumulator asks: `T` settles to a type the family keeps, R's outcome first
    /// (`.agents/memory/decisions/type-checker.md`). `None` when the returns are not that shape.
    fn family_fixed_point(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
    ) -> Option<Result<(), UnifyError<'db>>> {
        let TyKind::Var(expected) = *self.shallow_resolve(db, expected).kind(db) else {
            return None;
        };
        let var = self.find(expected);
        let Entry::Unbound { constraint, .. } = *self.entry(var) else {
            return None;
        };
        let actual = self.shallow_resolve(db, actual);
        let members = match actual.kind(db) {
            TyKind::Union(members) => *members,
            _ => std::slice::from_ref(&actual),
        };
        let mut family = None;
        for &member in members {
            let member = self.shallow_resolve(db, member);
            let operand = match *member.kind(db) {
                TyKind::Family(result, operand) if family.is_none_or(|known| known == result) => {
                    family = Some(result);
                    operand
                }
                _ => member,
            };
            let TyKind::Var(operand) = *self.shallow_resolve(db, operand).kind(db) else {
                return None;
            };
            if self.find(operand) != var {
                return None;
            }
        }
        let result = family?;
        let snapshot = self.snapshot();
        let r_outcome = result.r_outcome(db);
        let settled = if constraint_rejects(db, self, constraint, r_outcome).is_none() {
            self.bind(db, var, r_outcome).map(|()| {
                // A fold's result is its init or the callback's, so a later init of another
                // R type joins the outcome instead of rebinding it (`Compared<T> = T` must hold),
                // and one that arithmetic keeps or promotes rebinds it (`Promoted<T> = T`).
                let widening = match result {
                    FamilyResult::Comparison => Widening::Joined,
                    FamilyResult::Arithmetic => Widening::Folded { constraint },
                };
                self.set(
                    var,
                    Entry::Bound {
                        ty: r_outcome,
                        widening,
                    },
                );
            })
        } else {
            match result {
                FamilyResult::Arithmetic if constraint.join(Constraint::Number).is_some() => {
                    self.constrain(db, var, Constraint::Number)
                }
                FamilyResult::Comparison => {
                    self.bind(db, var, native_scalar(db, NativeScalar::Bool))
                }
                FamilyResult::Arithmetic => Err(UnifyError::Occurs(var, actual)),
            }
        };
        if settled.is_err() {
            self.rollback(snapshot);
        }
        Some(settled)
    }

    fn family_operand_constraint(&self, db: &'db dyn Db, operand: Ty<'db>) -> Constraint {
        match self.shallow_resolve(db, operand).kind(db) {
            TyKind::Var(var) => match self.entry(self.find(*var)) {
                Entry::Unbound { constraint, .. } => *constraint,
                _ => Constraint::Unconstrained,
            },
            TyKind::Rigid(name) => self
                .rigid_constraints
                .get(name)
                .copied()
                .unwrap_or(Constraint::Unconstrained),
            _ => Constraint::Unconstrained,
        }
    }

    /// Restricts a family's open operand to one side; a type parameter cannot be restricted,
    /// so it must already satisfy the constraint.
    fn constrain_family_operand(
        &mut self,
        db: &'db dyn Db,
        operand: Ty<'db>,
        constraint: Constraint,
    ) -> Result<(), UnifyError<'db>> {
        let operand = self.shallow_resolve(db, operand);
        match operand.kind(db) {
            TyKind::Var(var) => self.constrain(db, *var, constraint),
            _ => match constraint_rejects(db, self, constraint, operand) {
                Some(error) => Err(error),
                None => Ok(()),
            },
        }
    }

    /// A family result that binds a constrained variable: when the constraint admits only one
    /// side's outcome, the operand takes that side, and the decided result is what binds.
    fn narrow_family(
        &mut self,
        db: &'db dyn Db,
        constraint: Constraint,
        result: FamilyResult,
        operand: Ty<'db>,
    ) -> Result<Ty<'db>, UnifyError<'db>> {
        let r_admitted = constraint_rejects(db, self, constraint, result.r_outcome(db)).is_none();
        let ry_side = match result {
            FamilyResult::Arithmetic => constraint
                .families()
                .map_or(constraint, |(_, ry_half)| ry_half),
            FamilyResult::Comparison => Constraint::ArrayItem,
        };
        let ry_admitted = match result {
            FamilyResult::Arithmetic => constraint.join(Constraint::Number).is_some(),
            FamilyResult::Comparison => {
                constraint_rejects(db, self, constraint, native_scalar(db, NativeScalar::Bool))
                    .is_none()
            }
        };
        let side = match (r_admitted, ry_admitted) {
            (true, true) => None,
            (true, false) => Some(self.family_operand_constraint(db, operand).r_side()),
            (false, true) => Some(ry_side),
            (false, false) => {
                return Err(UnifyError::ConstraintRejected(
                    constraint,
                    Ty::new(db, TyKind::Family(result, operand)),
                ));
            }
        };
        if let Some(side) = side {
            self.constrain_family_operand(db, operand, side)?;
        }
        Ok(self.family(db, result, operand))
    }

    /// Directional compatibility: the coercions unification never applies (scalar to
    /// vector, numeric widening, dropping names, union membership, contravariant
    /// parameters). `true` keeps the bindings it made; `false` rolls back everything.
    pub fn compatible(&mut self, db: &'db dyn Db, actual: Ty<'db>, expected: Ty<'db>) -> bool {
        self.compatible_probe(db, actual, expected, 0)
    }

    /// Whether each function of a union fits `target` with its own return, and the join
    /// of those returns fits the target's return. Leaves no binding behind on failure.
    fn function_union_fits(
        &mut self,
        db: &'db dyn Db,
        members: &[Ty<'db>],
        target: &FunctionType<'db>,
        depth: usize,
    ) -> bool {
        let snapshot = self.snapshot();
        let mut returns = Vec::with_capacity(members.len());
        for &member in members {
            let ret = self.fresh_ty(db, Constraint::Unconstrained);
            let each = Ty::new(db, TyKind::Function(&FunctionType { ret, ..*target }));
            if !self.compatible_probe(db, member, each, depth + 1) {
                self.rollback(snapshot);
                return false;
            }
            returns.push(ret);
        }
        let resolved: Vec<Ty<'db>> = returns.iter().map(|&ret| self.resolve(db, ret)).collect();
        let joined = union_of(db, resolved);
        let fits = match self.family_fixed_point(db, joined, target.ret) {
            Some(settled) => settled.is_ok(),
            None => self.compatible_probe(db, joined, target.ret, depth + 1),
        };
        if fits {
            true
        } else {
            self.rollback(snapshot);
            false
        }
    }

    fn compatible_probe(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
        depth: usize,
    ) -> bool {
        // A resource guard, not a verdict: the occurs check refuses cycles, so real
        // programs never reach it.
        const DEPTH_LIMIT: usize = 128;
        let snapshot = self.snapshot();
        let verdict = depth < DEPTH_LIMIT && self.compatible_inner(db, actual, expected, depth);
        if !verdict {
            self.rollback(snapshot);
            // A rigid binder declared with a type bound fits wherever its bound does.
            if let TyKind::Rigid(name) = self.shallow_resolve(db, actual).kind(db)
                && let Some(&bound) = self.rigid_bounds.get(name)
            {
                return self.compatible_probe(db, bound, expected, depth + 1);
            }
        }
        verdict
    }

    /// A numeric type variable that bound to `integer` meets a `double` argument of the
    /// same shape (`f(1L, 0.5)` for `function(a, b) a * b`): R's arithmetic promotes, so
    /// the variable widens to the wider type. Earlier arguments still fit, because
    /// `integer` is compatible with `double`. Logicals never widen a binding, since a
    /// body like `stopifnot(x + 1L > 0L); x` returns the argument's own type. A `matrix`
    /// absorbs any numeric shape the same way.
    fn widen_binding(&mut self, db: &'db dyn Db, actual: Ty<'db>, expected: Ty<'db>) -> bool {
        let TyKind::Var(var) = expected.kind(db) else {
            return false;
        };
        let representative = self.find(*var);
        let Entry::Bound {
            ty: bound,
            widening,
        } = *self.entry(representative)
        else {
            return false;
        };
        let actual = self.resolve(db, actual);
        match widening {
            Widening::Fixed => false,
            Widening::Joined => {
                if !is_r_value(db, bound)
                    || !is_r_value(db, actual)
                    || self.compatible(db, actual, bound)
                {
                    return false;
                }
                self.set(
                    representative,
                    Entry::Bound {
                        ty: union_of(db, [bound, actual]),
                        widening,
                    },
                );
                true
            }
            Widening::Numeric => {
                self.widen_numeric_binding(db, representative, bound, actual, widening)
            }
            Widening::Folded { constraint } => {
                // The outcome is provisional until the first value meets it: one that fits
                // settles it, and one that does not rebinds it once, so a value checked
                // against the outcome never goes unchecked against what it rebinds to.
                let fixed = Entry::Bound {
                    ty: bound,
                    widening: Widening::Fixed,
                };
                if self.compatible(db, actual, bound) {
                    self.set(representative, fixed);
                    return false;
                }
                let folded = match numeric_join(db, [bound, actual]) {
                    _ if constraint_rejects(db, self, constraint, actual).is_some() => None,
                    Some(promoted) => Some(promoted),
                    None if is_ry_value_type(db, actual) || is_r_value(db, actual) => Some(actual),
                    None => None,
                };
                let Some(folded) = folded else {
                    self.set(representative, fixed);
                    return false;
                };
                self.set(
                    representative,
                    Entry::Bound {
                        ty: folded,
                        widening: Widening::Fixed,
                    },
                );
                true
            }
            Widening::Compared { equality } => {
                if numeric_rank(db, bound).is_some() && numeric_rank(db, actual).is_some() {
                    return self.widen_numeric_binding(db, representative, bound, actual, widening);
                }
                let constraint = if equality {
                    Constraint::Equatable
                } else {
                    Constraint::Comparable
                };
                if !is_r_value(db, bound)
                    || !is_r_value(db, actual)
                    || constraint_rejects(db, self, constraint, actual).is_some()
                    || self.compatible(db, actual, bound)
                {
                    return false;
                }
                self.set(
                    representative,
                    Entry::Bound {
                        ty: union_of(db, [bound, actual]),
                        widening,
                    },
                );
                true
            }
        }
    }

    /// `widening` is kept on the rebound entry, so a comparison's variable widened by a number
    /// still takes a later string.
    fn widen_numeric_binding(
        &mut self,
        db: &'db dyn Db,
        representative: InferenceVar,
        bound: Ty<'db>,
        actual: Ty<'db>,
        widening: Widening,
    ) -> bool {
        // Arithmetic between a matrix and a plain number or vector gives a matrix, so a
        // variable tied by `a * H` takes `f(2, m)` and `f(m, 2)` alike and binds the matrix.
        let is_matrix = |ty: Ty<'db>| matches!(ty.kind(db), TyKind::Named(name, _) if name.text(db) == "matrix");
        // An open argument meeting a variable a literal bound (`f(2, x)` inside a function of
        // `x`) is constrained numeric instead of pinned to the literal's type, so the caller's
        // parameter stays generic. The binding keeps the literal's type, which R's arithmetic
        // promotes every narrower argument to, and no variable moves between levels.
        let promotes = numeric_rank(db, bound).is_some_and(|(_, rank)| rank == 2);
        let open_numeric = promotes
            && match actual.kind(db) {
                TyKind::Var(actual_var) => {
                    let actual_representative = self.find(*actual_var);
                    actual_representative != representative
                        && matches!(
                            *self.entry(actual_representative),
                            Entry::Unbound {
                                constraint: Constraint::Unconstrained
                                    | Constraint::Numeric
                                    | Constraint::Num
                                    | Constraint::Arithmetic,
                                ..
                            }
                        )
                        && self
                            .constrain(db, actual_representative, Constraint::Arithmetic)
                            .is_ok()
                }
                TyKind::Rigid(name) => matches!(
                    self.rigid_constraints.get(name),
                    Some(
                        Constraint::Numeric
                            | Constraint::Num
                            | Constraint::Arithmetic
                            | Constraint::ArithmeticOrNumber
                            | Constraint::ArithmeticOrF64
                            | Constraint::NumericOrF64
                    )
                ),
                _ => false,
            };
        if open_numeric {
            return true;
        }
        if is_matrix(bound) && numeric_rank(db, actual).is_some() {
            return true;
        }
        if is_matrix(actual) && numeric_rank(db, bound).is_some() {
            self.set(
                representative,
                Entry::Bound {
                    ty: actual,
                    widening,
                },
            );
            return true;
        }
        match (numeric_rank(db, bound), numeric_rank(db, actual)) {
            (Some((bound_vector, bound_rank)), Some((actual_vector, actual_rank)))
                if actual_rank > bound_rank
                    || (actual_rank == bound_rank && !bound_vector && actual_vector) =>
            {
                let Some(widened) = numeric_join(db, [bound, actual]) else {
                    return false;
                };
                self.set(
                    representative,
                    Entry::Bound {
                        ty: widened,
                        widening,
                    },
                );
                true
            }
            _ => false,
        }
    }

    fn compatible_inner(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
        depth: usize,
    ) -> bool {
        if self.widen_binding(db, actual, expected) {
            return true;
        }
        let actual = self.resolve(db, actual);
        let expected = self.resolve(db, expected);
        // Mirrors `unify`: neither `Any` nor `Unknown` is a checkable claim.
        if matches!(actual.kind(db), TyKind::Any | TyKind::Unknown)
            || matches!(expected.kind(db), TyKind::Any | TyKind::Unknown)
        {
            return true;
        }
        if actual == expected {
            return true;
        }
        if matches!(expected.kind(db), TyKind::Family(..)) {
            return self.unify(db, actual, expected).is_ok();
        }
        // A family result fits where one of its outcomes does, and its operand takes that side.
        if let TyKind::Family(result, operand) = *actual.kind(db)
            && !matches!(expected.kind(db), TyKind::Var(_))
        {
            let r_side = self.family_operand_constraint(db, operand).r_side();
            let ry_side = match result {
                FamilyResult::Arithmetic => Constraint::Number,
                FamilyResult::Comparison => Constraint::ArrayItem,
            };
            let mut sides = [
                (r_side, result.r_outcome(db)),
                (ry_side, result.ry_outcome(db, operand)),
            ];
            if matches!(expected.kind(db), TyKind::Native(_)) {
                sides.reverse();
            }
            for (side, outcome) in sides {
                let snapshot = self.snapshot();
                if self.constrain_family_operand(db, operand, side).is_ok()
                    && self.compatible_inner(db, outcome, expected, depth + 1)
                {
                    return true;
                }
                self.rollback(snapshot);
            }
            return false;
        }
        // An argument fits an open `T | NULL` with `T` taking its non-`NULL` part. Binding an
        // open argument to the whole union would tell the caller its own value may be `NULL`
        // (`check(x); x$field` after `check <- function(x) if (is.null(x)) ...`), and binding
        // `T` to one member of a union argument would reject the others.
        if let TyKind::Union(members) = expected.kind(db)
            && let [first, second] = members[..]
            && let Some(open_member) = match (
                self.shallow_resolve(db, first).kind(db),
                self.shallow_resolve(db, second).kind(db),
            ) {
                (TyKind::Var(_), TyKind::Null) => Some(first),
                (TyKind::Null, TyKind::Var(_)) => Some(second),
                _ => None,
            }
        {
            let present = match actual.kind(db) {
                TyKind::Var(_) => Some(actual),
                TyKind::Union(actual_members)
                    if !actual_members
                        .iter()
                        .any(|member| matches!(member.kind(db), TyKind::OtherTags)) =>
                {
                    Some(union_of(
                        db,
                        actual_members
                            .iter()
                            .copied()
                            .filter(|member| !matches!(member.kind(db), TyKind::Null)),
                    ))
                }
                _ => None,
            };
            if let Some(present) = present {
                let snapshot = self.snapshot();
                if self.unify(db, open_member, present).is_ok() {
                    return true;
                }
                self.rollback(snapshot);
            }
        }
        // A constrained variable cannot bind to a union that has a member outside its
        // constraint, so `lapply(n, f)` after `f(n)` made `n` numeric fits `X`'s `T[]` member.
        // A union the constraint admits whole (`integer | double` for a number) binds whole,
        // because picking a member would reject the others at every later call.
        if let (TyKind::Var(var), TyKind::Union(members)) = (actual.kind(db), expected.kind(db))
            && !matches!(
                self.entry(self.find(*var)),
                Entry::Unbound {
                    constraint: Constraint::Unconstrained,
                    ..
                }
            )
        {
            let snapshot = self.snapshot();
            if self.unify(db, actual, expected).is_ok() {
                return true;
            }
            self.rollback(snapshot);
            return members
                .iter()
                .any(|&member| self.compatible_probe(db, actual, member, depth + 1));
        }
        // An open argument where a union is expected keeps naming its own type, bounded by the
        // union: `function(path) { vapply(path, f, logical(1)); path }` returns what it is
        // given, where binding `path` to `list[T] | T[]` would return that union to every caller.
        if let (TyKind::Var(var), TyKind::Union(_)) = (actual.kind(db), expected.kind(db))
            && let representative = self.find(*var)
            && let Entry::Unbound {
                level,
                constraint: Constraint::Unconstrained,
                bound,
            } = *self.entry(representative)
            && self.walk_unbound_vars(db, expected, &mut |found| found != representative)
        {
            let bound = match bound {
                None => expected,
                Some(existing) => match self.meet_bounds(db, existing, expected, depth) {
                    Some(bound) => bound,
                    None => return false,
                },
            };
            if !matches!(bound.kind(db), TyKind::Union(_)) {
                return self.bind(db, representative, bound).is_ok();
            }
            self.adjust_levels(db, level, bound);
            self.set(
                representative,
                Entry::Unbound {
                    level,
                    constraint: Constraint::Unconstrained,
                    bound: Some(bound),
                },
            );
            return true;
        }
        // A scalar is a length-one vector in R, so a variable that a scalar use bounded (`1:n`)
        // or a literal wrote (`p <- "a"`) fills a vector parameter as its element, as a scalar
        // argument does.
        if let (TyKind::Var(var), TyKind::Vector(element) | TyKind::NamedVector(element)) =
            (actual.kind(db), expected.kind(db))
            && let Entry::Unbound {
                constraint:
                    Constraint::ScalarNumeric
                    | Constraint::AtomicElement
                    | Constraint::Key
                    | Constraint::WholeLiteral { .. }
                    | Constraint::FracLiteral
                    | Constraint::StrLiteral
                    | Constraint::BoolLiteral,
                ..
            } = self.entry(self.find(*var))
        {
            let snapshot = self.snapshot();
            if self.unify(db, *element, actual).is_ok() {
                return true;
            }
            self.rollback(snapshot);
            return false;
        }
        if let (TyKind::Rigid(name), TyKind::Vector(element) | TyKind::NamedVector(element)) =
            (actual.kind(db), expected.kind(db))
            && matches!(
                self.rigid_constraints.get(name),
                Some(Constraint::ScalarNumeric | Constraint::AtomicElement | Constraint::Key)
            )
        {
            let snapshot = self.snapshot();
            if self.unify(db, *element, actual).is_ok() {
                return true;
            }
            self.rollback(snapshot);
        }
        // A union of open variables fits one of them when the others tie to it: a callback
        // `function(x, y) if (keep(x)) x else y` returns `U | T`, and `Reduce` needs a `U`.
        if let (TyKind::Union(members), TyKind::Var(var)) = (actual.kind(db), expected.kind(db)) {
            let representative = self.find(*var);
            let open_members: Option<Vec<InferenceVar>> = members
                .iter()
                .map(|&member| match self.shallow_resolve(db, member).kind(db) {
                    TyKind::Var(member_var) => Some(self.find(*member_var)),
                    _ => None,
                })
                .collect();
            if open_members.is_some_and(|open_members| open_members.contains(&representative)) {
                let snapshot = self.snapshot();
                if members
                    .iter()
                    .all(|&member| self.unify(db, member, expected).is_ok())
                {
                    return true;
                }
                self.rollback(snapshot);
                return false;
            }
        }
        if actual.is_var() || expected.is_var() {
            return self.unify(db, actual, expected).is_ok();
        }
        if self.undeclared_nominal(db, actual) || self.undeclared_nominal(db, expected) {
            return true;
        }
        match (actual.kind(db), expected.kind(db)) {
            // `character | list{..}` into `T | NULL`: the members the target does not list
            // bind its one variable together, where member by member the first would pin it.
            (TyKind::Union(members), TyKind::Union(expected_members))
                if !members
                    .iter()
                    .any(|member| matches!(member.kind(db), TyKind::OtherTags))
                    && expected_members
                        .iter()
                        .filter(|&&member| self.shallow_resolve(db, member).is_var())
                        .count()
                        == 1 =>
            {
                let mut variable = None;
                let mut concrete = Vec::new();
                for &member in expected_members.iter() {
                    let member = self.shallow_resolve(db, member);
                    if member.is_var() {
                        variable = Some(member);
                    } else {
                        concrete.push(member);
                    }
                }
                let rest: Vec<Ty<'db>> = members
                    .iter()
                    .copied()
                    .filter(|member| !concrete.contains(member))
                    .collect();
                match variable {
                    Some(_) if rest.is_empty() => true,
                    Some(variable) => self.unify(db, variable, union_of(db, rest)).is_ok(),
                    None => false,
                }
            }
            // First, so union-vs-union means every actual member fits some expected member.
            // An open source fits only an open target that it can vouch for: a tag hidden in
            // the source's `..` could carry any payload under a name the target lists.
            // A union of functions fits a function target when each member does, and the
            // call returns whichever member ran: `F <- if (wrap) function(i) list(i) else
            // function(i) i + 1` maps with `lapply(n, F)` to `list{T} | double` elements.
            // When the joined return does not fit (a member returning `Any` where the target
            // returns a number), each member fits the target as it is, below.
            (TyKind::Union(members), TyKind::Function(target))
                if members
                    .iter()
                    .all(|member| matches!(member.kind(db), TyKind::Function(_)))
                    && self.function_union_fits(db, members, target, depth) =>
            {
                true
            }
            (TyKind::Union(members), _) => {
                if members
                    .iter()
                    .any(|member| matches!(member.kind(db), TyKind::OtherTags))
                {
                    // A lone `..` is every tag, so any open union of tags fits it.
                    let expected_members = match expected.kind(db) {
                        TyKind::Union(expected_members) => expected_members,
                        TyKind::OtherTags => &[][..],
                        _ => return false,
                    };
                    let target_is_open = matches!(expected.kind(db), TyKind::OtherTags)
                        || expected_members
                            .iter()
                            .any(|member| matches!(member.kind(db), TyKind::OtherTags));
                    let every_target_tag_listed = expected_members.iter().all(|target| {
                        match target.kind(db) {
                            TyKind::Tag(name, _) => members.iter().any(|member| {
                                matches!(member.kind(db), TyKind::Tag(other, _) if other == name)
                            }),
                            _ => true,
                        }
                    });
                    if !target_is_open || !every_target_tag_listed {
                        return false;
                    }
                }
                // An open member goes last and binds only when it can, and is accepted either
                // way: it is a value the body never constrained, so nothing shows it cannot
                // fit. After `x <- list(x)` on one path, `list{U}` binds the target's element to
                // `U`, and `U` itself cannot then become a list of itself, which R never asks.
                // It fits a union target as a lone open argument does, taking the union as a
                // bound, so `character | U` passed where `list[T] | T[] | NULL` is expected
                // leaves `U` free to be `character | NULL` at a later call.
                // The rule above already decided where the source's own `..` goes.
                let (open, concrete): (Vec<Ty<'db>>, Vec<Ty<'db>>) = members
                    .iter()
                    .copied()
                    .filter(|member| !matches!(member.kind(db), TyKind::OtherTags))
                    .partition(|&member| {
                        matches!(
                            member.kind(db),
                            TyKind::Var(var) if matches!(
                                self.entry(self.find(*var)),
                                Entry::Unbound { constraint: Constraint::Unconstrained, .. }
                            )
                        )
                    });
                concrete
                    .iter()
                    .all(|&member| self.compatible_probe(db, member, expected, depth + 1))
                    && open.iter().all(|&member| {
                        self.compatible_probe(db, member, expected, depth + 1);
                        true
                    })
            }
            (TyKind::Tag(..), TyKind::OtherTags) => true,
            // A lone `..` is an open union that lists no tag, so it fits an open target only
            // when the target lists no tag either.
            (TyKind::OtherTags, TyKind::Union(members)) => {
                members
                    .iter()
                    .any(|member| matches!(member.kind(db), TyKind::OtherTags))
                    && !members
                        .iter()
                        .any(|member| matches!(member.kind(db), TyKind::Tag(..)))
            }
            // A tag the open target does not list falls into its `..`; a listed name must
            // fit that member, which the general search below decides.
            (TyKind::Tag(name, _), TyKind::Union(members))
                if members
                    .iter()
                    .any(|member| matches!(member.kind(db), TyKind::OtherTags))
                    && !members.iter().any(
                        |member| matches!(member.kind(db), TyKind::Tag(other, _) if other == name),
                    ) =>
            {
                true
            }
            // Concrete members first: `NULL` into `T | NULL` must match the `NULL` member
            // and bind nothing, leaving `T` for a later argument.
            // A tag reaches the `..` member only through the arm above, which first rules out
            // a listed name whose payload does not fit.
            (_, TyKind::Union(members)) => {
                let tag = matches!(actual.kind(db), TyKind::Tag(..));
                let (variables, concrete): (Vec<Ty<'db>>, Vec<Ty<'db>>) = members
                    .iter()
                    .filter(|member| !(tag && matches!(member.kind(db), TyKind::OtherTags)))
                    .partition(|&&member| self.shallow_resolve(db, member).is_var());
                concrete
                    .into_iter()
                    .chain(variables)
                    .any(|member| self.compatible_probe(db, actual, member, depth + 1))
            }
            // Each argument checks in its variance's direction; invariant when the
            // definition is missing (over-rejects, never widens unsoundly).
            (
                TyKind::Named(actual_name, actual_arguments),
                TyKind::Named(expected_name, expected_arguments),
            ) if actual_name == expected_name
                && actual_arguments.len() == expected_arguments.len() =>
            {
                let variances = self
                    .definition(db, *actual_name)
                    .map(|definition| parameter_variances(db, definition))
                    .unwrap_or_default();
                actual_arguments
                    .iter()
                    .zip(expected_arguments.iter())
                    .enumerate()
                    .all(|(index, (&actual_argument, &expected_argument))| {
                        match variances.get(index).copied().unwrap_or(Variance::Invariant) {
                            Variance::Bivariant => true,
                            Variance::Covariant => self.compatible_probe(
                                db,
                                actual_argument,
                                expected_argument,
                                depth + 1,
                            ),
                            Variance::Contravariant => self.compatible_probe(
                                db,
                                expected_argument,
                                actual_argument,
                                depth + 1,
                            ),
                            Variance::Invariant => {
                                self.compatible_probe(
                                    db,
                                    actual_argument,
                                    expected_argument,
                                    depth + 1,
                                ) && self.compatible_probe(
                                    db,
                                    expected_argument,
                                    actual_argument,
                                    depth + 1,
                                )
                            }
                        }
                    })
            }
            // `data.table{...}` is the `data.table` class with its columns known, so the two fit
            // each other both ways, as `data.frame{...}` and `data.frame` do.
            (TyKind::Named(name, arguments), TyKind::Frame(kind, _, _))
                if arguments.is_empty() && kind.class() == Some(name.text(db)) =>
            {
                true
            }
            // `new.env()` is an environment with no bindings yet, which an annotation then types;
            // a typed environment is still an environment.
            (TyKind::Named(name, _), TyKind::Environment(_))
            | (TyKind::Environment(_), TyKind::Named(name, _))
                if name.text(db) == "environment" =>
            {
                true
            }
            // An environment is mutable, so its bindings' types must match both ways.
            (TyKind::Environment(actual_fields), TyKind::Environment(expected_fields)) => {
                actual_fields.len() == expected_fields.len()
                    && expected_fields.iter().all(|expected_field| {
                        actual_fields
                            .iter()
                            .find(|field| field.name == expected_field.name)
                            .is_some_and(|actual_field| {
                                self.compatible_probe(
                                    db,
                                    actual_field.ty,
                                    expected_field.ty,
                                    depth + 1,
                                ) && self.compatible_probe(
                                    db,
                                    expected_field.ty,
                                    actual_field.ty,
                                    depth + 1,
                                )
                            })
                    })
            }
            // An Array is an R vector at run time: a scalar item type makes it an atomic vector
            // and a ry-value item type a list, so it passes to R code as one, never the reverse.
            (
                TyKind::Named(..),
                TyKind::Vector(expected_element) | TyKind::List(expected_element),
            ) if array_element(db, actual).is_some() => {
                let element = array_element(db, actual).map(|element| self.resolve(db, element));
                let Some(mut element) = element else {
                    return false;
                };
                // An undecided number item (`[1, 2]`) passed to R code is the `F64` that shares
                // R's `double`.
                if matches!(expected.kind(db), TyKind::Vector(_))
                    && matches!(
                        self.open_constraint(db, element),
                        Some(Constraint::Number | Constraint::Frac)
                    )
                {
                    let f64 = native_scalar(db, NativeScalar::F64);
                    if self.unify(db, element, f64).is_err() {
                        return false;
                    }
                    element = f64;
                }
                match (element.kind(db), expected.kind(db)) {
                    (TyKind::Native(native), TyKind::Vector(_)) => {
                        native.shared_atomic().is_some_and(|atomic| {
                            self.compatible_probe(
                                db,
                                scalar(db, atomic),
                                *expected_element,
                                depth + 1,
                            )
                        })
                    }
                    (TyKind::Native(_), _) | (_, TyKind::Vector(_)) => false,
                    _ => self.compatible_probe(db, element, *expected_element, depth + 1),
                }
            }
            // `F64`, `Bool` and `Str` share the representation of R's `double`, `logical` and
            // `character`, so they pass to R code as those; no other ry number does.
            (TyKind::Native(native), _) => match native.shared_atomic() {
                Some(atomic) => self.compatible_probe(db, scalar(db, atomic), expected, depth + 1),
                None => false,
            },
            // Only the projection direction: structural into nominal happens only via `@new`.
            (TyKind::Named(actual_name, actual_arguments), _) => {
                match self.representation(db, *actual_name, actual_arguments) {
                    Some(representation) => {
                        self.compatible_probe(db, representation, expected, depth + 1)
                    }
                    None => false,
                }
            }
            // Element recursion reaches the variable arms for a generic element, which is
            // how `sort(c(1L))` binds `T := integer`.
            (TyKind::Scalar(_), TyKind::Vector(element)) => {
                self.compatible_probe(db, actual, *element, depth + 1)
            }
            (TyKind::NamedVector(actual_element), TyKind::Vector(expected_element)) => {
                self.compatible_probe(db, *actual_element, *expected_element, depth + 1)
            }
            // R promotes `logical` and `integer` freely in numeric contexts (`sum(flags)`).
            // Directional only: unification never widens.
            (TyKind::Scalar(actual_atomic), TyKind::Scalar(expected_atomic)) => {
                numeric_ladder_rank(*actual_atomic)
                    .zip(numeric_ladder_rank(*expected_atomic))
                    .is_some_and(|(actual_rank, expected_rank)| actual_rank < expected_rank)
            }
            (TyKind::Vector(actual_element), TyKind::Vector(expected_element))
            | (TyKind::NamedVector(actual_element), TyKind::NamedVector(expected_element)) => {
                self.compatible_probe(db, *actual_element, *expected_element, depth + 1)
            }
            (TyKind::Tuple(actual_items), TyKind::Tuple(expected_items))
                if actual_items.len() == expected_items.len() =>
            {
                actual_items.iter().zip(expected_items.iter()).all(
                    |(&actual_item, &expected_item)| {
                        self.compatible_probe(db, actual_item, expected_item, depth + 1)
                    },
                )
            }
            // An open record asks for its fields and lets the rest through.
            (
                TyKind::Record(actual_fields) | TyKind::OpenRecord(actual_fields),
                TyKind::OpenRecord(expected_fields),
            ) => expected_fields.iter().all(|expected_field| {
                actual_fields
                    .iter()
                    .find(|field| field.name == expected_field.name)
                    .is_some_and(|actual_field| {
                        self.compatible_probe(db, actual_field.ty, expected_field.ty, depth + 1)
                    })
            }),
            (TyKind::Record(actual_fields), TyKind::Record(expected_fields))
                if actual_fields.len() == expected_fields.len() =>
            {
                expected_fields.iter().all(|expected_field| {
                    actual_fields
                        .iter()
                        .find(|field| field.name == expected_field.name)
                        .is_some_and(|actual_field| {
                            self.compatible_probe(db, actual_field.ty, expected_field.ty, depth + 1)
                        })
                })
            }
            (TyKind::Frame(actual_kind, _, _), TyKind::Frame(expected_kind, _, _))
                if !same_frame_kind(*actual_kind, *expected_kind) =>
            {
                false
            }
            (TyKind::Frame(_, None, _), TyKind::Frame(..))
            | (TyKind::Frame(..), TyKind::Frame(_, None, _)) => true,
            (TyKind::Frame(_, Some(_), _), TyKind::Frame(_, Some(_), _)) => {
                self.open_compatible(db, actual, expected, depth)
            }
            (TyKind::Frame(kind, _, _), TyKind::Named(name, arguments))
                if arguments.is_empty() && kind.class() == Some(name.text(db)) =>
            {
                true
            }
            // `as.list` turns a frame into the list of its columns, which is how `lapply`,
            // `Map` and `Filter` iterate one. A plan or a grouping is not a list of columns.
            (
                TyKind::Frame(FrameKind::LazyFrame | FrameKind::GroupBy { .. }, _, _),
                TyKind::List(_) | TyKind::NamedList(_),
            ) => false,
            // A row's columns are unknown, as are all the columns of a frame that lists none.
            (
                TyKind::Frame(_, None, _) | TyKind::Frame(_, Some(_), Some(_)),
                TyKind::List(_) | TyKind::NamedList(_),
            ) => true,
            (
                TyKind::Frame(_, Some(columns), _),
                TyKind::List(element) | TyKind::NamedList(element),
            ) if self.resolve(db, *element).is_var() => {
                let joined = union_of(
                    db,
                    columns.iter().map(|column| r_column_type(db, column.ty)),
                );
                self.unify(db, *element, joined).is_ok()
            }
            (
                TyKind::Frame(_, Some(columns), _),
                TyKind::List(element) | TyKind::NamedList(element),
            ) => columns.iter().all(|column| {
                self.compatible_probe(db, r_column_type(db, column.ty), *element, depth + 1)
            }),
            // An open `T` takes the join of the items: unifying each in turn would pin `T`
            // to the first item and reject the rest. An empty `list()` says nothing about `T`,
            // so it leaves `T` open; binding it to the empty join would fail every constraint
            // on `T`, as in `List.max(list())`.
            (TyKind::Tuple(items), TyKind::List(element))
                if self.resolve(db, *element).is_var() =>
            {
                if items.is_empty() {
                    return true;
                }
                let joined = union_of(db, items.iter().copied());
                self.unify(db, *element, joined).is_ok()
            }
            (TyKind::Record(fields), TyKind::List(element))
            | (TyKind::Record(fields), TyKind::NamedList(element))
                if self.resolve(db, *element).is_var() =>
            {
                let joined = union_of(db, fields.iter().map(|field| field.ty));
                self.unify(db, *element, joined).is_ok()
            }
            (TyKind::Tuple(items), TyKind::List(element)) => items
                .iter()
                .all(|&item| self.compatible_probe(db, item, *element, depth + 1)),
            // In R `list()` is also the empty named list, so it fits `list[named: T]`.
            (TyKind::Tuple([]), TyKind::NamedList(_)) => true,
            (TyKind::Record(fields), TyKind::List(element))
            | (TyKind::Record(fields), TyKind::NamedList(element)) => fields
                .iter()
                .all(|field| self.compatible_probe(db, field.ty, *element, depth + 1)),
            (TyKind::NamedList(actual_element), TyKind::List(expected_element))
            | (TyKind::NamedList(actual_element), TyKind::NamedList(expected_element))
            | (TyKind::List(actual_element), TyKind::List(expected_element)) => {
                self.compatible_probe(db, *actual_element, *expected_element, depth + 1)
            }
            // A list whose elements are not known at all, such as `as.list(x)` on an untyped
            // `x`, says nothing about its names, so it may be the named list the target wants.
            (TyKind::List(actual_element), TyKind::NamedList(_))
                if matches!(
                    self.shallow_resolve(db, *actual_element).kind(db),
                    TyKind::Any | TyKind::Unknown
                ) =>
            {
                true
            }
            (TyKind::Function(actual_function), TyKind::Function(expected_function)) => {
                self.function_compatible(db, actual_function, expected_function, depth)
            }
            (TyKind::NativeTuple(actual_items), TyKind::NativeTuple(expected_items))
                if actual_items.len() == expected_items.len() =>
            {
                actual_items.iter().zip(expected_items.iter()).all(
                    |(&actual_item, &expected_item)| {
                        self.compatible_probe(db, actual_item, expected_item, depth + 1)
                    },
                )
            }
            (TyKind::NativeRecord(..), TyKind::NativeRecord(..)) => {
                self.open_compatible(db, actual, expected, depth)
            }
            (
                TyKind::Tag(actual_name, actual_payload),
                TyKind::Tag(expected_name, expected_payload),
            ) if actual_name == expected_name => match (actual_payload, expected_payload) {
                (None, None) => true,
                (Some(actual_payload), Some(expected_payload)) => {
                    self.compatible_probe(db, *actual_payload, *expected_payload, depth + 1)
                }
                _ => false,
            },
            // Past the column-wise arms, an R frame passes wherever its class does:
            // `data.frame{...}` is a `data.frame` to `merge` and `sum`.
            (TyKind::Frame(kind, _, _), _) if let Some(class) = kind.class() => {
                let nominal = Ty::new(db, TyKind::Named(Name::new(db, class), &[]));
                self.compatible_probe(db, nominal, expected, depth + 1)
            }
            _ => false,
        }
    }

    fn function_compatible(
        &mut self,
        db: &'db dyn Db,
        actual: &FunctionType<'db>,
        expected: &FunctionType<'db>,
        depth: usize,
    ) -> bool {
        let Some(pairing) = pair_parameters(db, actual, expected) else {
            return false;
        };
        for (expected_element, actual_element) in pairing.rest {
            if !self.compatible_probe(db, expected_element, actual_element, depth + 1) {
                return false;
            }
        }
        for pair in pairing.parameters {
            // Contravariant.
            if !self.compatible_probe(db, pair.passed, pair.accepts, depth + 1) {
                return false;
            }
        }
        let returns = match self.family_fixed_point(db, actual.ret, expected.ret) {
            Some(settled) => settled.is_ok(),
            None => self.compatible_probe(db, actual.ret, expected.ret, depth + 1),
        };
        if !returns {
            return false;
        }
        // The parameters are bound by now, so a pair the actual function compares is checked
        // as a call would check it.
        actual
            .compared
            .iter()
            .all(|&pair| self.compared_pair_fits(db, pair, depth + 1))
    }

    /// Whether the two sides of a compared pair may meet as they resolve: a ry value on one
    /// side needs its own type on the other.
    pub fn compared_pair_fits(
        &mut self,
        db: &'db dyn Db,
        pair: ComparedPair<'db>,
        depth: usize,
    ) -> bool {
        let left = self.resolve(db, pair.left);
        let right = self.resolve(db, pair.right);
        if holds_ry_value(db, left) {
            self.compatible_probe(db, right, left, depth)
        } else if holds_ry_value(db, right) {
            self.compatible_probe(db, left, right, depth)
        } else {
            true
        }
    }

    /// Names the failing position: a parameter's constraint does not survive into
    /// the rendered type, so whole signatures can print identically. `None` when
    /// the shapes (arity, optionality, rest parameter) disagree.
    pub fn explain_function_mismatch(
        &mut self,
        db: &'db dyn Db,
        actual: &FunctionType<'db>,
        expected: &FunctionType<'db>,
    ) -> Option<FunctionMismatch<'db>> {
        let pairing = pair_parameters(db, actual, expected)?;
        for pair in pairing.parameters {
            if self.compatible(db, pair.passed, pair.accepts) {
                continue;
            }
            let accepts = self.resolve(db, pair.accepts);
            return Some(FunctionMismatch::Parameter {
                name: pair.name.map(|name| name.text(db).to_owned()),
                passed: self.resolve(db, pair.passed),
                constraint: self.variable_constraint(db, accepts),
                accepts,
            });
        }
        if !self.return_compatible(db, actual.ret, expected.ret) {
            let returns = self.resolve(db, actual.ret);
            let required = self.resolve(db, expected.ret);
            return Some(FunctionMismatch::Return {
                required,
                constraint: self.variable_constraint(db, returns),
                required_constraint: self.variable_constraint(db, required),
                returns,
            });
        }
        for &pair in actual.compared {
            if !self.compared_pair_fits(db, pair, 0) {
                let left = self.resolve(db, pair.left);
                let right = self.resolve(db, pair.right);
                let (expected, found) = if holds_ry_value(db, left) {
                    (left, right)
                } else {
                    (right, left)
                };
                return Some(FunctionMismatch::Compared { expected, found });
            }
        }
        None
    }

    /// Pairs fields by name like [`Self::compatible`], so the explanation cannot
    /// disagree with the verdict. Rolls back before returning: `compatible` keeps a
    /// `true` verdict's bindings, which must not leak out of a reporting path.
    pub fn explain_record_mismatch(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
    ) -> Option<RecordMismatch<'db>> {
        let snapshot = self.snapshot();
        let mut path = Vec::new();
        let mismatch = self.walk_record_mismatch(db, actual, expected, &mut path);
        self.rollback(snapshot);
        mismatch
    }

    fn walk_record_mismatch(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
        path: &mut Vec<String>,
    ) -> Option<RecordMismatch<'db>> {
        let (
            TyKind::Record(actual_fields) | TyKind::OpenRecord(actual_fields),
            TyKind::Record(expected_fields) | TyKind::OpenRecord(expected_fields),
        ) = (
            self.resolve(db, actual).kind(db),
            self.resolve(db, expected).kind(db),
        )
        else {
            return None;
        };
        let open = matches!(self.resolve(db, expected).kind(db), TyKind::OpenRecord(_));
        // Optionality is not compared: `compatible` does not compare it either.
        for expected_field in expected_fields.iter() {
            let name = expected_field.name.text(db);
            let Some(actual_field) = actual_fields
                .iter()
                .find(|field| field.name == expected_field.name)
            else {
                path.push(name.to_owned());
                return Some(RecordMismatch::Missing {
                    near: nearest_name(name, actual_fields.iter().map(|field| field.name.text(db))),
                    path: mem::take(path),
                });
            };
            if self.compatible(db, actual_field.ty, expected_field.ty) {
                continue;
            }
            path.push(name.to_owned());
            if let Some(inner) =
                self.walk_record_mismatch(db, actual_field.ty, expected_field.ty, path)
            {
                return Some(inner);
            }
            return Some(RecordMismatch::Field {
                expected: self.resolve(db, expected_field.ty),
                found: self.resolve(db, actual_field.ty),
                path: mem::take(path),
            });
        }
        if open {
            return None;
        }
        // Surplus fields last: a wrong type in a shared field is the likelier mistake.
        for actual_field in actual_fields.iter() {
            if expected_fields
                .iter()
                .any(|field| field.name == actual_field.name)
            {
                continue;
            }
            let name = actual_field.name.text(db);
            path.push(name.to_owned());
            return Some(RecordMismatch::Unexpected {
                near: nearest_name(
                    name,
                    expected_fields.iter().map(|field| field.name.text(db)),
                ),
                path: mem::take(path),
            });
        }
        None
    }

    /// The one fact the rendered type cannot show: `fn(s: U) -> U` prints the same
    /// whether `U` accepts anything or only numbers.
    pub(crate) fn variable_constraint(&self, db: &'db dyn Db, ty: Ty<'db>) -> Option<Constraint> {
        let TyKind::Var(var) = ty.kind(db) else {
            return None;
        };
        match self.entry(self.find(*var)) {
            Entry::Unbound { constraint, .. }
                if !matches!(constraint, Constraint::Unconstrained) =>
            {
                Some(*constraint)
            }
            _ => None,
        }
    }

    pub fn contains_unbound_var(&self, db: &'db dyn Db, ty: Ty<'db>) -> bool {
        !self.walk_unbound_vars(db, ty, &mut |_| false)
    }

    /// Canonicalized to representatives.
    pub fn collect_unbound_vars(
        &self,
        db: &'db dyn Db,
        ty: Ty<'db>,
        found: &mut FxHashSet<InferenceVar>,
    ) {
        self.walk_unbound_vars(db, ty, &mut |var| {
            found.insert(var);
            true
        });
    }

    /// Stops once `visit` returns `false`; returns whether everything was visited.
    fn walk_unbound_vars(
        &self,
        db: &'db dyn Db,
        ty: Ty<'db>,
        visit: &mut impl FnMut(InferenceVar) -> bool,
    ) -> bool {
        if !contains_inference_var(db, ty) {
            return true;
        }
        let shallow = self.shallow_resolve(db, ty);
        if let TyKind::Var(var) = shallow.kind(db) {
            return visit(*var);
        }
        let mut complete = true;
        for_each_child_type(db, shallow, &mut |child| {
            complete = complete && self.walk_unbound_vars(db, child, visit);
        });
        complete
    }
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum FunctionMismatch<'db> {
    Parameter {
        name: Option<String>,
        /// What the expected interface may pass into that position.
        passed: Ty<'db>,
        /// What the function's parameter accepts.
        accepts: Ty<'db>,
        constraint: Option<Constraint>,
    },
    Return {
        required: Ty<'db>,
        returns: Ty<'db>,
        /// The constraint the function's own result type carries.
        constraint: Option<Constraint>,
        /// The constraint the expected result type carries, such as `item` on `List.map`'s `B`.
        required_constraint: Option<Constraint>,
    },
    /// The function compares two of its operands, and the expected interface gives one side a
    /// ry value and the other a value of another type.
    Compared { expected: Ty<'db>, found: Ty<'db> },
}

/// `path` names enclosing fields first and ends in the field the finding is about.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum RecordMismatch<'db> {
    Field {
        path: Vec<String>,
        expected: Ty<'db>,
        found: Ty<'db>,
    },
    Missing {
        path: Vec<String>,
        near: Option<String>,
    },
    Unexpected {
        path: Vec<String>,
        near: Option<String>,
    },
}

struct Pairing<'db> {
    parameters: Vec<ParameterPair<'db>>,
    /// `(passed, accepts)` pairs handled by the function's rest parameter.
    rest: Vec<(Ty<'db>, Ty<'db>)>,
}

struct ParameterPair<'db> {
    name: Option<Name<'db>>,
    passed: Ty<'db>,
    /// Contravariant: `passed` must fit `accepts`.
    accepts: Ty<'db>,
}

/// Arity is a range: a function serves an interface when it accepts every call
/// shape the interface allows, so extra parameters are fine when they default.
/// A variadic interface pairs only with a variadic at the same formal position, unless its
/// `...` takes `Any`: a callback called with forwarded dots (`function(f, ...) f(...)`) may
/// receive any arguments, so any function serves it.
fn pair_parameters<'db>(
    db: &'db dyn Db,
    actual: &FunctionType<'db>,
    expected: &FunctionType<'db>,
) -> Option<Pairing<'db>> {
    let forwards_anything = expected
        .variadic
        .as_ref()
        .is_some_and(|rest| matches!(rest.element.kind(db), TyKind::Any));
    let mut rest = Vec::new();
    match (&actual.variadic, &expected.variadic) {
        (Some(actual_variadic), Some(expected_variadic)) => {
            // `fn(...: Any) -> T` promises nothing about the parameters before `...`,
            // so any function fits it, whatever it names ahead of its own `...`.
            if !forwards_anything
                && actual_variadic.preceding_named != expected_variadic.preceding_named
            {
                return None;
            }
            rest.push((expected_variadic.element, actual_variadic.element));
        }
        // An interface without `...` never passes extras, so the function's own `...`
        // receives at most the leftovers paired below.
        (Some(_), None) | (None, None) => {}
        (None, Some(_)) if forwards_anything => {}
        (None, Some(_)) => return None,
    }
    // R matches arguments to formals by name regardless of order; the rest pair
    // positionally.
    let mut actual_parameters: Vec<(Option<Name<'db>>, Ty<'db>, bool)> = actual
        .positional
        .iter()
        .map(|&ty| (None, ty, false))
        .collect();
    actual_parameters.extend(
        actual
            .named
            .iter()
            .map(|field| (Some(field.name), field.ty, field.optional)),
    );
    let mut paired: Vec<Option<(Ty<'db>, bool)>> = vec![None; actual_parameters.len()];
    let mut overflow = Vec::new();
    for field in expected.named.iter() {
        match actual_parameters
            .iter()
            .position(|(name, ..)| *name == Some(field.name))
        {
            Some(index) if paired[index].is_none() => {
                paired[index] = Some((field.ty, field.optional));
            }
            _ => overflow.push((field.ty, field.optional)),
        }
    }
    let mut positional_expected = expected
        .positional
        .iter()
        .map(|&ty| (ty, false))
        .chain(overflow);
    for slot in paired.iter_mut() {
        if slot.is_none() {
            *slot = positional_expected.next();
        }
    }
    // A leftover expected parameter can only go to the function's rest parameter.
    for (expected_parameter, _) in positional_expected {
        let variadic = actual.variadic.as_ref()?;
        rest.push((expected_parameter, variadic.element));
    }
    let mut parameters = Vec::new();
    for ((name, actual_parameter, actual_optional), slot) in
        actual_parameters.into_iter().zip(paired)
    {
        let Some((expected_parameter, expected_optional)) = slot else {
            if actual_optional || forwards_anything {
                continue;
            }
            return None;
        };
        // Callers of the interface may omit it, so the function must default it.
        if expected_optional && !actual_optional {
            return None;
        }
        parameters.push(ParameterPair {
            name,
            passed: expected_parameter,
            accepts: actual_parameter,
        });
    }
    Some(Pairing { parameters, rest })
}

/// Missing arguments fill as `Unknown`.
pub fn apply_definition<'db>(
    db: &'db dyn Db,
    definition: &NamedDefinition<'db>,
    arguments: &[Ty<'db>],
) -> Ty<'db> {
    if definition.parameters.is_empty() {
        return definition.body;
    }
    let substitution: SmallVec<[(Name<'db>, Ty<'db>); 4]> = definition
        .parameters
        .iter()
        .enumerate()
        .map(|(index, &parameter)| {
            (
                parameter,
                arguments.get(index).copied().unwrap_or_else(|| unknown(db)),
            )
        })
        .collect();
    substitute_rigid(db, definition.body, &substitution)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variance {
    /// Never occurs in the representation.
    Bivariant,
    Covariant,
    Contravariant,
    Invariant,
}

fn parameter_variances<'db>(db: &'db dyn Db, definition: &NamedDefinition<'db>) -> Vec<Variance> {
    // An abstract type (`@type Dict<K, V>`) hides its body, so its parameters are what tell two
    // values apart, and a `Dict<character, double>` must not pass for a `Dict<integer, double>`.
    // ry's own `List`, `Dict` and `Set` are immutable values, so a `List<:Cash>` is also a
    // `List<:Bond | :Cash>`: their parameters are covariant.
    if !definition.alias && matches!(definition.body.kind(db), TyKind::Unknown) {
        let variance = if crate::stubs::is_ry_type(db, definition.name.text(db)) {
            Variance::Covariant
        } else {
            Variance::Invariant
        };
        return vec![variance; definition.parameters.len()];
    }
    let mut occurrences = vec![(false, false); definition.parameters.len()];
    record_occurrences(
        db,
        definition.body,
        &definition.parameters,
        true,
        &mut occurrences,
    );
    occurrences
        .into_iter()
        .map(|occurrence| match occurrence {
            (false, false) => Variance::Bivariant,
            (true, false) => Variance::Covariant,
            (false, true) => Variance::Contravariant,
            (true, true) => Variance::Invariant,
        })
        .collect()
}

/// `occurrences` holds `(positive, negative)` per parameter.
fn record_occurrences<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    parameters: &[Name<'db>],
    positive: bool,
    occurrences: &mut [(bool, bool)],
) {
    match ty.kind(db) {
        TyKind::Rigid(name) => {
            if let Some(index) = parameters.iter().position(|parameter| parameter == name) {
                if positive {
                    occurrences[index].0 = true;
                } else {
                    occurrences[index].1 = true;
                }
            }
        }
        TyKind::Function(function) => {
            let parameter_types = function
                .positional
                .iter()
                .copied()
                .chain(function.named.iter().map(|field| field.ty))
                .chain(function.variadic.iter().map(|rest| rest.element));
            for parameter in parameter_types {
                record_occurrences(db, parameter, parameters, !positive, occurrences);
            }
            record_occurrences(db, function.ret, parameters, positive, occurrences);
        }
        // Nested nominal variance is not composed: conservatively invariant.
        TyKind::Named(_, arguments) => {
            for &argument in arguments.iter() {
                record_occurrences(db, argument, parameters, true, occurrences);
                record_occurrences(db, argument, parameters, false, occurrences);
            }
        }
        _ => for_each_child_type(db, ty, &mut |child| {
            record_occurrences(db, child, parameters, positive, occurrences)
        }),
    }
}

/// `character` and `raw` are off the ladder: R reaches them only by explicit
/// coercion, and accepting them would hide argument-order mistakes.
fn numeric_ladder_rank(atomic: Atomic) -> Option<u8> {
    match atomic {
        Atomic::Logical => Some(0),
        Atomic::Integer => Some(1),
        Atomic::Double => Some(2),
        Atomic::Complex => Some(3),
        Atomic::Character | Atomic::Raw => None,
    }
}

fn nullable_single_member<'db>(db: &'db dyn Db, members: &[Ty<'db>]) -> Option<Ty<'db>> {
    if members.len() != 2 {
        return None;
    }
    let null_at = members
        .iter()
        .position(|member| matches!(member.kind(db), TyKind::Null))?;
    Some(members[1 - null_at])
}

/// Beyond atoms: a class whose arithmetic takes two of it is numeric (`a + b` is legal R for
/// it), a class declaring any arithmetic method satisfies `arithmetic` (`Date + 1`), and a
/// rigid binder is admitted when its declared bound is at least as strong (self-recursive
/// calls unify it with a fresh variable).
pub(crate) fn constraint_rejects<'db>(
    db: &'db dyn Db,
    table: &InferenceTable<'db>,
    constraint: Constraint,
    ty: Ty<'db>,
) -> Option<UnifyError<'db>> {
    let arithmetic_classes = table.arithmetic_classes;
    let declares_arithmetic = |name: &Name<'db>| {
        arithmetic_classes.is_some_and(|classes| classes.contains(name.symbol()))
    };
    // The declared bound implies the required one when joining does not strengthen it, and a
    // type bound does when every type it admits does (`<N: integer | double>` is numeric).
    let binder_implies = |name: &Name<'db>| {
        table
            .rigid_constraints
            .get(name)
            .is_some_and(|declared| declared.join(constraint) == Some(*declared))
            || table
                .rigid_bounds
                .get(name)
                .is_some_and(|&bound| constraint_rejects(db, table, constraint, bound).is_none())
    };
    // A family result satisfies a bound when one of its outcomes does; `bind` narrows the
    // operand to that side.
    if let TyKind::Family(result, operand) = ty.kind(db) {
        let admissible = constraint_rejects(db, table, constraint, result.r_outcome(db)).is_none()
            || match result {
                FamilyResult::Arithmetic => constraint.join(Constraint::Number).is_some(),
                FamilyResult::Comparison => {
                    constraint_rejects(db, table, constraint, result.ry_outcome(db, *operand))
                        .is_none()
                }
            };
        return (!admissible).then_some(UnifyError::ConstraintRejected(constraint, ty));
    }
    // A union satisfies a bound when every member does: `double | integer` is a number either way.
    if let TyKind::Union(members) = ty.kind(db) {
        let every_member_admissible = members
            .iter()
            .all(|&member| constraint_rejects(db, table, constraint, member).is_none());
        return (!every_member_admissible)
            .then_some(UnifyError::ConstraintRejected(constraint, ty));
    }
    // An open member counts when its own bound implies the required one, which `bind`
    // arranges for the members of a union before it asks.
    if let TyKind::Var(var) = ty.kind(db) {
        let admissible = match table.entry(table.find(*var)) {
            Entry::Unbound {
                constraint: own, ..
            } => own.join(constraint) == Some(*own),
            Entry::Bound { ty, .. } => constraint_rejects(db, table, constraint, *ty).is_none(),
            Entry::Redirect(_) => false,
        };
        return (!admissible).then_some(UnifyError::ConstraintRejected(constraint, ty));
    }
    // A ry nominal compares as its representation does, so `type Count = I64` compares with a
    // number.
    if matches!(
        constraint,
        Constraint::ComparableOrNumber
            | Constraint::ComparableOrOrdered
            | Constraint::ComparableOrStr
            | Constraint::EquatableOrNumber
            | Constraint::EquatableOrStr
    ) && let TyKind::Named(name, _) = ty.kind(db)
    {
        // A ry `List` compares item by item, and a ry nominal as its representation.
        let unfolded = array_element(db, ty).or_else(|| {
            table
                .definitions
                .and_then(|definitions| definitions.get(db, *name))
                .map(|definition| definition.body)
                .filter(|body| is_ry_value_type(db, *body))
        });
        if let Some(unfolded) = unfolded {
            return constraint_rejects(db, table, constraint, unfolded)
                .map(|_| UnifyError::ConstraintRejected(constraint, ty));
        }
    }
    let admissible = match constraint {
        Constraint::Unconstrained => true,
        // A constraint of both families admits what either half admits, since no type is on
        // both sides of the divide.
        Constraint::Num
        | Constraint::ArithmeticOrNumber
        | Constraint::ArithmeticOrF64
        | Constraint::NumericOrF64
        | Constraint::ComparableOrNumber
        | Constraint::ComparableOrOrdered
        | Constraint::ComparableOrStr
        | Constraint::EquatableOrNumber
        | Constraint::EquatableOrStr => match (ty.kind(db), constraint.families()) {
            (TyKind::Rigid(name), _) if binder_implies(name) => true,
            (_, Some((r_half, ry_half))) => {
                constraint_rejects(db, table, r_half, ty).is_none()
                    || constraint_rejects(db, table, ry_half, ty).is_none()
            }
            (_, None) => false,
        },
        // R's arithmetic takes logicals (as 0 and 1) and complex numbers as well as numbers.
        Constraint::Numeric | Constraint::Arithmetic => match ty.kind(db) {
            TyKind::Scalar(
                Atomic::Logical | Atomic::Integer | Atomic::Double | Atomic::Complex,
            )
            | TyKind::Any
            | TyKind::Unknown => true,
            TyKind::Vector(element) | TyKind::NamedVector(element) => match element.kind(db) {
                TyKind::Scalar(
                    Atomic::Logical | Atomic::Integer | Atomic::Double | Atomic::Complex,
                ) => true,
                // `bind` carries the bound to an open element, so `T[]` is a number vector.
                TyKind::Var(var) => match table.entry(table.find(*var)) {
                    Entry::Unbound { .. } => true,
                    Entry::Bound { ty, .. } => {
                        constraint_rejects(db, table, Constraint::ScalarNumeric, *ty).is_none()
                    }
                    Entry::Redirect(_) => false,
                },
                TyKind::Rigid(name) => binder_implies(name),
                _ => false,
            },
            TyKind::Named(name, _) if constraint == Constraint::Arithmetic => {
                declares_arithmetic(name)
            }
            TyKind::Named(name, _) => arithmetic_classes
                .is_some_and(|classes| classes.closed_under_arithmetic(name.symbol())),
            // A frame with known columns carries its class like a nominal one does.
            TyKind::Frame(kind, _, _) => kind.class().is_some_and(|class| {
                arithmetic_classes.is_some_and(|classes| match constraint {
                    Constraint::Arithmetic => classes.contains(class),
                    _ => classes.closed_under_arithmetic(class),
                })
            }),
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::AtomicElement => match ty.kind(db) {
            TyKind::Scalar(_) | TyKind::Any | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::Key => match ty.kind(db) {
            TyKind::Scalar(
                Atomic::Logical | Atomic::Integer | Atomic::Double | Atomic::Character,
            )
            | TyKind::Native(_)
            | TyKind::Any
            | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => is_ry_value_type(db, ty),
        },
        Constraint::ScalarNumeric => match ty.kind(db) {
            TyKind::Scalar(Atomic::Logical | Atomic::Integer | Atomic::Double)
            | TyKind::Any
            | TyKind::Unknown => true,
            TyKind::Named(name, _) => declares_arithmetic(name),
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::ArrayItem => match ty.kind(db) {
            TyKind::Any | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            // A nominal `type Tree = :Leaf(I64) | ...` is a ry value when its representation
            // is, which is what a non-empty `[@Tree(...)]` checks too.
            TyKind::Named(name, _) if !is_ry_value_type(db, ty) => table
                .definitions
                .and_then(|definitions| definitions.get(db, *name))
                .is_some_and(|definition| is_ry_value_type(db, definition.body)),
            _ => is_ry_value_type(db, ty),
        },
        Constraint::Number | Constraint::Int | Constraint::Frac => match ty.kind(db) {
            TyKind::Native(native) => match constraint {
                Constraint::Int => native.is_integer(),
                Constraint::Frac => native.is_fraction(),
                _ => native.is_number(),
            },
            TyKind::Any | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::Eq => match ty.kind(db) {
            TyKind::Function(_) | TyKind::Environment(_) => false,
            TyKind::Named(name, _) if name.text(db) == "environment" => false,
            TyKind::Rigid(name) => binder_implies(name),
            _ => true,
        },
        Constraint::Comparable | Constraint::Equatable => match ty.kind(db) {
            // `NULL > 0` is `logical(0)`.
            TyKind::Any | TyKind::Unknown | TyKind::Frame(..) | TyKind::Null => true,
            TyKind::Rigid(name) => binder_implies(name),
            // R compares only atomic values and lists: `globalenv() == 1` is an error.
            TyKind::Environment(_) => false,
            TyKind::Named(name, _) if name.text(db) == "environment" => false,
            // A class compares through R's `Ops` methods; a ry nominal is a ry value.
            TyKind::Named(name, _) => {
                !is_ry_value_type(db, ty)
                    && !table
                        .definitions
                        .and_then(|definitions| definitions.get(db, *name))
                        .is_some_and(|definition| is_ry_value_type(db, definition.body))
            }
            _ => comparison_operand_shape(db, ty, constraint == Constraint::Equatable).is_some(),
        },
        Constraint::Str => match ty.kind(db) {
            TyKind::Native(native) => *native == NativeScalar::Str,
            TyKind::Any | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::Ordered => match ty.kind(db) {
            TyKind::Native(native) => native.is_number() || *native == NativeScalar::Str,
            TyKind::Any | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::WholeLiteral { integer } => match ty.kind(db) {
            TyKind::Native(native) => native.is_number(),
            TyKind::Scalar(Atomic::Double) => true,
            TyKind::Scalar(Atomic::Integer) => integer,
            TyKind::Any | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::FracLiteral => match ty.kind(db) {
            TyKind::Native(native) => native.is_fraction(),
            TyKind::Scalar(Atomic::Double) | TyKind::Any | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::StrLiteral => match ty.kind(db) {
            TyKind::Native(NativeScalar::Str)
            | TyKind::Scalar(Atomic::Character)
            | TyKind::Any
            | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::BoolLiteral => match ty.kind(db) {
            TyKind::Native(NativeScalar::Bool)
            | TyKind::Scalar(Atomic::Logical)
            | TyKind::Any
            | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
    };
    (!admissible).then_some(UnifyError::ConstraintRejected(constraint, ty))
}

/// The names and types a table or record lists, with its row.
type OpenFields<'db> = SmallVec<[(Name<'db>, Ty<'db>); 8]>;

/// Whether a row holds a table's columns or a record's fields.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowShape {
    Columns,
    Fields,
}

/// A table or record type as `Checker::unify_open` reads it.
enum Settled<'db> {
    Open((RowShape, OpenFields<'db>, Option<Ty<'db>>)),
    /// The type, or its row, is `Unknown`, so anything fits it.
    Unknown,
    WrongShape,
}

fn open_parts<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
) -> Option<(RowShape, OpenFields<'db>, Option<Ty<'db>>)> {
    match *ty.kind(db) {
        TyKind::Frame(_, Some(columns), row) => Some((
            RowShape::Columns,
            columns
                .iter()
                .map(|column| (column.name, column.ty))
                .collect(),
            row,
        )),
        TyKind::NativeRecord(fields, row) => Some((
            RowShape::Fields,
            fields.iter().map(|field| (field.name, field.ty)).collect(),
            row,
        )),
        _ => None,
    }
}

/// The row that holds `fields` and then `tail`: `tail` itself without fields, and the empty
/// closed row without either.
fn row_type<'db>(
    db: &'db dyn Db,
    shape: RowShape,
    fields: &[(Name<'db>, Ty<'db>)],
    tail: Option<Ty<'db>>,
) -> Ty<'db> {
    if fields.is_empty()
        && let Some(tail) = tail
    {
        return tail;
    }
    match shape {
        RowShape::Columns => {
            let columns: SmallVec<[RecordField<'db>; 8]> = fields
                .iter()
                .map(|&(name, ty)| RecordField {
                    name,
                    ty,
                    optional: false,
                })
                .collect();
            frame_with_row(db, FrameKind::DataFrame, &columns, tail)
        }
        RowShape::Fields => native_record_with_row(
            db,
            fields.iter().map(|&(name, ty)| NativeField { name, ty }),
            tail,
        ),
    }
}

/// Two table kinds fit when they are the same kind. A grouping's key count is part of its kind
/// only when both sides know it: a written `GroupBy{...}` names no keys.
fn same_frame_kind(left: FrameKind, right: FrameKind) -> bool {
    match (left, right) {
        (
            FrameKind::GroupBy {
                lazy: left_lazy,
                keys: left_keys,
            },
            FrameKind::GroupBy {
                lazy: right_lazy,
                keys: right_keys,
            },
        ) => {
            left_lazy == right_lazy
                && (left_keys == right_keys || left_keys == 0 || right_keys == 0)
        }
        _ => left == right,
    }
}

/// Where a numeric type sits in R's promotion order, `logical` < `integer` < `double`, and
/// whether it is a vector. A union of numeric types ranks as its widest member, since every
/// member coerces to that one (`double | integer` widens a binding to `double`).
/// The type R's arithmetic promotes a set of numeric values to: the widest of `logical`,
/// `integer` and `double`, as a vector when any of them is one (a scalar recycles against
/// it). `None` when a type is not numeric.
pub(crate) fn numeric_join<'db>(
    db: &'db dyn Db,
    types: impl IntoIterator<Item = Ty<'db>>,
) -> Option<Ty<'db>> {
    let (vector, rank) = types
        .into_iter()
        .try_fold((false, 0), |(vector, rank), ty| {
            let (ty_vector, ty_rank) = numeric_rank(db, ty)?;
            Some((vector || ty_vector, rank.max(ty_rank)))
        })?;
    let element = scalar(
        db,
        match rank {
            2 => Atomic::Double,
            1 => Atomic::Integer,
            _ => Atomic::Logical,
        },
    );
    Some(if vector {
        Ty::new(db, TyKind::Vector(element))
    } else {
        element
    })
}

fn numeric_rank<'db>(db: &'db dyn Db, ty: Ty<'db>) -> Option<(bool, u8)> {
    let atomic_rank = |ty: Ty<'db>| match ty.kind(db) {
        TyKind::Scalar(Atomic::Logical) => Some(0),
        TyKind::Scalar(Atomic::Integer) => Some(1),
        TyKind::Scalar(Atomic::Double) => Some(2),
        _ => None,
    };
    match ty.kind(db) {
        TyKind::Vector(element) => atomic_rank(*element).map(|rank| (true, rank)),
        TyKind::Union(members) => members
            .iter()
            .try_fold((false, 0), |(vector, rank), member| {
                let (member_vector, member_rank) = numeric_rank(db, *member)?;
                Some((vector || member_vector, rank.max(member_rank)))
            }),
        _ => atomic_rank(ty).map(|rank| (false, rank)),
    }
}

/// A concrete R value: not open, not `Any` or `Unknown`, and not a ry value.
fn is_r_value<'db>(db: &'db dyn Db, ty: Ty<'db>) -> bool {
    !matches!(
        ty.kind(db),
        TyKind::Var(_) | TyKind::Rigid(_) | TyKind::Any | TyKind::Unknown
    ) && !is_ry_value_type(db, ty)
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            RootDatabase,
            types::{Parameter, null, scalar},
        },
    };

    #[test]
    fn a_subtree_shared_through_variables_resolves_once() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let depth = 48;
        let variables: Vec<Ty<'_>> = (0..=depth)
            .map(|_| table.fresh_ty(&db, Constraint::Unconstrained))
            .collect();
        for pair in variables.windows(2) {
            let twice = Ty::new(&db, TyKind::Tuple(&[pair[1], pair[1]]));
            table.unify(&db, pair[0], twice).expect("binds");
        }
        let int = scalar(&db, Atomic::Integer);
        table.unify(&db, variables[depth], int).expect("binds");

        let before = resolve_calls();
        let resolved = table.resolve(&db, variables[0]);
        let steps = resolve_calls() - before;

        let expected =
            (0..depth).fold(int, |inner, _| Ty::new(&db, TyKind::Tuple(&[inner, inner])));
        assert_eq!(resolved, expected);
        // A walk that resolves each shared tuple once per path takes 2^48 steps.
        assert!(steps < 1_000, "{steps} steps");
    }

    #[test]
    fn constraint_joins_through_redirects() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let a = table.fresh(Constraint::Numeric);
        let b = table.fresh(Constraint::AtomicElement);
        let a_ty = Ty::new(&db, TyKind::Var(a));
        let b_ty = Ty::new(&db, TyKind::Var(b));
        table.unify(&db, a_ty, b_ty).expect("redirects");
        let Entry::Unbound { constraint, .. } = *table.entry(a) else {
            panic!("representative stays unbound")
        };
        assert_eq!(constraint, Constraint::ScalarNumeric);
        let chr = scalar(&db, Atomic::Character);
        assert!(table.unify(&db, a_ty, chr).is_err());
    }

    #[test]
    fn rollback_reverts_probes_completely() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let outer = table.fresh_ty(&db, Constraint::Unconstrained);
        let snapshot = table.snapshot();
        let probe = table.fresh_ty(&db, Constraint::Unconstrained);
        table.unify(&db, outer, probe).expect("unifies");
        table
            .unify(&db, probe, scalar(&db, Atomic::Double))
            .expect("binds");
        assert_eq!(table.resolve(&db, outer), scalar(&db, Atomic::Double));
        table.rollback(snapshot);
        assert!(matches!(
            table.entry(InferenceVar(0)),
            Entry::Unbound { .. }
        ));
        assert!(table.resolve(&db, outer).is_var());
    }

    #[test]
    fn unions_unify_by_set_equality_and_nullable_case() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let int = scalar(&db, Atomic::Integer);
        let chr = scalar(&db, Atomic::Character);
        let null = null(&db);
        let a = union_of(&db, [int, chr]);
        let b = union_of(&db, [chr, int]);
        assert!(table.unify(&db, a, b).is_ok());

        let var = table.fresh_ty(&db, Constraint::Unconstrained);
        let left = union_of(&db, [var, null]);
        let right = union_of(&db, [int, null]);
        table
            .unify(&db, left, right)
            .expect("nullable member-wise case");
        assert_eq!(table.resolve(&db, var), int);

        let c = union_of(&db, [int, null]);
        let d = union_of(&db, [chr, null]);
        assert!(table.unify(&db, c, d).is_err());
    }

    #[test]
    fn compatibility_widens_and_coerces_directionally() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let int = scalar(&db, Atomic::Integer);
        let dbl = scalar(&db, Atomic::Double);
        let chr = scalar(&db, Atomic::Character);
        assert!(table.compatible(&db, int, dbl));
        assert!(!table.compatible(&db, dbl, int));
        let dbl_vec = Ty::new(&db, TyKind::Vector(dbl));
        assert!(table.compatible(&db, int, dbl_vec));
        assert!(!table.compatible(&db, chr, dbl_vec));
        assert!(table.unify(&db, int, dbl).is_err());
    }

    #[test]
    fn compatibility_binds_generic_elements_and_prefers_concrete_members() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let int = scalar(&db, Atomic::Integer);
        let null = null(&db);
        let element = table.fresh_ty(&db, Constraint::Unconstrained);
        let generic_vector = Ty::new(&db, TyKind::Vector(element));
        assert!(table.compatible(&db, int, generic_vector));
        assert_eq!(table.resolve(&db, element), int);
        let t = table.fresh_ty(&db, Constraint::Unconstrained);
        let nullable = union_of(&db, [t, null]);
        assert!(table.compatible(&db, null, nullable));
        assert!(table.resolve(&db, t).is_var());
    }

    #[test]
    fn compatibility_fails_pure_and_pairs_function_formals_by_name() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let int = scalar(&db, Atomic::Integer);
        let chr = scalar(&db, Atomic::Character);
        let var = table.fresh_ty(&db, Constraint::Unconstrained);
        let tuple_with_var = Ty::new(&db, TyKind::Tuple(&[var, chr]));
        let tuple_expected = Ty::new(&db, TyKind::Tuple(&[int, int]));
        assert!(!table.compatible(&db, tuple_with_var, tuple_expected));
        assert!(table.resolve(&db, var).is_var());
        let named_lambda = Ty::new(
            &db,
            TyKind::Function(&FunctionType::declared(
                &[],
                &[Parameter {
                    name: Name::new(&db, "x"),
                    ty: int,
                    optional: false,
                    default: None,
                }],
                None,
                int,
            )),
        );
        let positional_interface = Ty::new(
            &db,
            TyKind::Function(&FunctionType::declared(
                &[int],
                &[],
                None,
                scalar(&db, Atomic::Double),
            )),
        );
        assert!(table.compatible(&db, named_lambda, positional_interface));
    }
}
