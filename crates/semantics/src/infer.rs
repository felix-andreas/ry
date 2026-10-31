//! Unification stays syntactic: a union unifies only with an equal union, plus
//! the member-wise `T | NULL` vs `U | NULL` case. Directional member-wise logic
//! belongs in compatibility checking, never here.

use crate::Db;
use crate::annotations::NamedDefinition;
use crate::diagnostics::nearest_name;
use crate::stubs::stubs;
use crate::types::{
    Atomic, Constraint, FunctionType, InferenceVar, Name, Ty, TyKind, for_each_child_type,
    map_child_types, scalar, substitute_rigid, union_of, unknown,
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::{Cell, RefCell};
use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry<'db> {
    Unbound {
        level: u32,
        constraint: Constraint,
    },
    /// `numeric` records that the variable was numeric-constrained when it bound, which
    /// lets a later, wider numeric argument rebind it (`f(1L, 0.5)`).
    Bound {
        ty: Ty<'db>,
        numeric: bool,
    },
    Redirect(InferenceVar),
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
}

#[derive(Debug, Default)]
pub struct InferenceTable<'db> {
    entries: Vec<Entry<'db>>,
    undo: Vec<(InferenceVar, Entry<'db>)>,
    pub level: u32,
    /// `None` (no globals at all) reads as an empty table.
    pub definitions: Option<&'db FxHashMap<Name<'db>, NamedDefinition<'db>>>,
    /// Includes project classes: operator dispatch resolves methods through the
    /// global scope, so the numeric constraint must consult the same set.
    pub arithmetic_classes: Option<&'db FxHashSet<String>>,
    /// Declared bounds of rigid binders in scope (`<T: numeric>`): a rigid `T` is
    /// not a numeric atom, so constraint admission needs its declared bound.
    pub rigid_constraints: FxHashMap<Name<'db>, Constraint>,
    /// Valid for one binding epoch; only subtrees whose resolution hit no cycle
    /// cut are stored.
    resolve_cache: RefCell<(u64, FxHashMap<Ty<'db>, Ty<'db>>)>,
    epoch: Cell<u64>,
}

/// Read by the perf witnesses: must stay near-linear in corpus size.
pub static RESOLVE_CALLS: AtomicU64 = AtomicU64::new(0);

/// Past any alias nested by hand; bounds a self-referential alias.
const ALIAS_EXPANSION_LIMIT: usize = 64;

impl<'db> InferenceTable<'db> {
    pub fn fresh(&mut self, constraint: Constraint) -> InferenceVar {
        let var = InferenceVar(self.entries.len() as u32);
        self.entries.push(Entry::Unbound {
            level: self.level,
            constraint,
        });
        var
    }

    pub fn fresh_ty(&mut self, db: &'db dyn Db, constraint: Constraint) -> Ty<'db> {
        let var = self.fresh(constraint);
        Ty::new(db, TyKind::Var(var))
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self.entries.len(),
            undo: self.undo.len(),
        }
    }

    pub fn rollback(&mut self, snapshot: Snapshot) {
        self.epoch.set(self.epoch.get() + 1);
        while self.undo.len() > snapshot.undo {
            let (var, previous) = self.undo.pop().expect("undo length checked");
            if (var.0 as usize) < snapshot.entries {
                self.entries[var.0 as usize] = previous;
            }
        }
        self.entries.truncate(snapshot.entries);
    }

    fn set(&mut self, var: InferenceVar, entry: Entry<'db>) {
        self.epoch.set(self.epoch.get() + 1);
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

    pub fn entry(&self, var: InferenceVar) -> &Entry<'db> {
        &self.entries[self.find(var).0 as usize]
    }

    /// An undeclared name or a wrong type-argument count. Both already carry an
    /// annotation diagnostic, so relations treat the type like `Unknown`.
    pub(crate) fn undeclared_nominal(&self, db: &'db dyn Db, ty: Ty<'db>) -> bool {
        let TyKind::Named(name, arguments) = ty.kind(db) else {
            return false;
        };
        match self.definitions.and_then(|table| table.get(name)) {
            // Includes the bare use of a generic (`Box` for `Box<T>`).
            Some(definition) => definition.parameters.len() != arguments.len(),
            None => !stubs(db).is_some_and(|library| library.nominals.contains(name.text(db))),
        }
    }

    pub fn shallow_resolve(&self, db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
        let mut current = ty;
        loop {
            match current.kind(db) {
                TyKind::Var(var) => match self.entry(*var) {
                    Entry::Bound { ty: bound, .. } => current = *bound,
                    _ => {
                        let representative = self.find(*var);
                        return if representative == *var {
                            current
                        } else {
                            Ty::new(db, TyKind::Var(representative))
                        };
                    }
                },
                _ => return current,
            }
        }
    }

    /// Also expands aliases. Self-referential bindings and aliases cut to
    /// `Unknown` instead of expanding forever.
    pub fn resolve(&self, db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
        {
            let mut cache = self.resolve_cache.borrow_mut();
            if cache.0 != self.epoch.get() {
                cache.0 = self.epoch.get();
                cache.1.clear();
            }
        }
        let mut visiting = Vec::new();
        self.resolve_rec(db, ty, &mut visiting).0
    }

    /// The bool is whether the subtree is clean (no cycle cut beneath it): only
    /// clean results are position-independent enough to memoize.
    fn resolve_rec(
        &self,
        db: &'db dyn Db,
        ty: Ty<'db>,
        visiting: &mut Vec<InferenceVar>,
    ) -> (Ty<'db>, bool) {
        RESOLVE_CALLS.fetch_add(1, Ordering::Relaxed);
        if let TyKind::Var(var) = ty.kind(db) {
            let root = self.find(*var);
            let shallow = self.shallow_resolve(db, ty);
            if let TyKind::Var(_) = shallow.kind(db) {
                return (shallow, true);
            }
            if visiting.contains(&root) {
                return (Ty::new(db, TyKind::Unknown), false);
            }
            visiting.push(root);
            let resolved = self.resolve_rec(db, shallow, visiting);
            visiting.pop();
            return resolved;
        }

        if let Some(&hit) = self.resolve_cache.borrow().1.get(&ty) {
            return (hit, true);
        }

        let mut clean = true;
        let resolved = map_child_types(db, ty, &mut |child| {
            let (child, child_clean) = self.resolve_rec(db, child, visiting);
            clean &= child_clean;
            child
        });
        // Aliases never survive into unification. A self-referential alias re-enters
        // through the same interned application, so the visiting stack bounds it.
        let alias = match resolved.kind(db) {
            TyKind::Named(name, arguments) => self
                .definitions
                .and_then(|table| table.get(name))
                .filter(|definition| definition.alias)
                .map(|definition| (definition, arguments)),
            _ => None,
        };
        let (resolved, clean) = match alias {
            Some((definition, arguments)) if visiting.len() < ALIAS_EXPANSION_LIMIT => {
                let expanded = apply_definition(db, definition, arguments);
                // Sentinel slot: alias depth counts against the variable cycle stack.
                visiting.push(InferenceVar(u32::MAX));
                let (expanded, expanded_clean) = self.resolve_rec(db, expanded, visiting);
                visiting.pop();
                (expanded, clean && expanded_clean)
            }
            // Left as written past the bound, which other positions must not reuse.
            Some(_) => (resolved, false),
            None => (resolved, clean),
        };
        if clean {
            self.resolve_cache.borrow_mut().1.insert(ty, resolved);
        }
        (resolved, clean)
    }

    /// `None` for aliases, opaque nominals, and unknown names.
    pub fn representation(
        &self,
        db: &'db dyn Db,
        name: Name<'db>,
        arguments: &[Ty<'db>],
    ) -> Option<Ty<'db>> {
        let definition = self.definitions.and_then(|table| table.get(&name))?;
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
        let Entry::Unbound { level, constraint } = *self.entry(representative) else {
            unreachable!("bind target is always unbound after find");
        };
        if let Some(error) = constraint_rejects(db, self, constraint, ty) {
            return Err(error);
        }
        self.adjust_levels(db, level, ty);
        let numeric = matches!(constraint, Constraint::Numeric | Constraint::ScalarNumeric);
        self.set(representative, Entry::Bound { ty, numeric });
        Ok(())
    }

    /// Lowering levels keeps a generalization at an outer level from quantifying
    /// a variable still reachable from an inner one.
    pub(crate) fn adjust_levels(&mut self, db: &'db dyn Db, level: u32, ty: Ty<'db>) {
        let shallow = self.shallow_resolve(db, ty);
        if let TyKind::Var(var) = shallow.kind(db) {
            let representative = self.find(*var);
            if let Entry::Unbound {
                level: var_level,
                constraint,
            } = *self.entry(representative)
                && var_level > level
            {
                self.set(representative, Entry::Unbound { level, constraint });
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
                } = *self.entry(left)
                else {
                    unreachable!("shallow resolve leaves only unbound variables")
                };
                let Entry::Unbound {
                    level: right_level,
                    constraint: right_constraint,
                } = *self.entry(right)
                else {
                    unreachable!("shallow resolve leaves only unbound variables")
                };
                let (winner, loser) = if left.0 <= right.0 {
                    (left, right)
                } else {
                    (right, left)
                };
                self.set(
                    winner,
                    Entry::Unbound {
                        level: left_level.min(right_level),
                        constraint: left_constraint.join(right_constraint),
                    },
                );
                self.set(loser, Entry::Redirect(winner));
                Ok(())
            }
            (TyKind::Var(var), _) => self.bind(db, *var, b),
            (_, TyKind::Var(var)) => self.bind(db, *var, a),
            // `Any` fits everything and `Unknown` must not cascade. After the Var arms so
            // a variable still binds to them.
            (TyKind::Any, _) | (_, TyKind::Any) | (TyKind::Unknown, _) | (_, TyKind::Unknown) => {
                Ok(())
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
                for (&left, &right) in left.iter().zip(right) {
                    self.unify(db, left, right)?;
                }
                Ok(())
            }
            (TyKind::Record(left), TyKind::Record(right)) => {
                if left.len() != right.len()
                    || left
                        .iter()
                        .zip(right.iter())
                        .any(|(l, r)| l.name != r.name || l.optional != r.optional)
                {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (left, right) in left.iter().zip(right) {
                    self.unify(db, left.ty, right.ty)?;
                }
                Ok(())
            }
            // A frame whose columns are unknown makes no claim to check.
            (TyKind::Frame(None), TyKind::Frame(_)) | (TyKind::Frame(_), TyKind::Frame(None)) => {
                Ok(())
            }
            (TyKind::Frame(Some(left)), TyKind::Frame(Some(right))) => {
                if left.len() != right.len() {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for column in left {
                    let Some(other) = right.iter().find(|other| other.name == column.name) else {
                        return Err(UnifyError::Mismatch(a, b));
                    };
                    self.unify(db, column.ty, other.ty)?;
                }
                Ok(())
            }
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
                for (&left, &right) in left.positional.iter().zip(&right.positional) {
                    self.unify(db, left, right)?;
                }
                for (left, right) in left.named.iter().zip(&right.named) {
                    self.unify(db, left.ty, right.ty)?;
                }
                if let (Some(left), Some(right)) = (&left.variadic, &right.variadic) {
                    self.unify(db, left.element, right.element)?;
                }
                self.unify(db, left.ret, right.ret)
            }
            (
                TyKind::Named(left_name, left_arguments),
                TyKind::Named(right_name, right_arguments),
            ) => {
                if left_name != right_name || left_arguments.len() != right_arguments.len() {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (&left, &right) in left_arguments.iter().zip(right_arguments) {
                    self.unify(db, left, right)?;
                }
                Ok(())
            }
            (TyKind::NativeTuple(left), TyKind::NativeTuple(right)) => {
                if left.len() != right.len() {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (&left, &right) in left.iter().zip(right) {
                    self.unify(db, left, right)?;
                }
                Ok(())
            }
            (TyKind::NativeRecord(left), TyKind::NativeRecord(right)) => {
                if left.len() != right.len()
                    || left.iter().zip(right).any(|(l, r)| l.name != r.name)
                {
                    return Err(UnifyError::Mismatch(a, b));
                }
                for (left, right) in left.iter().zip(right) {
                    self.unify(db, left.ty, right.ty)?;
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
            Entry::Unbound {
                level,
                constraint: existing,
            } => {
                let joined = existing.join(constraint);
                if joined != existing {
                    self.set(
                        representative,
                        Entry::Unbound {
                            level,
                            constraint: joined,
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

    /// Directional compatibility: the coercions unification never applies (scalar to
    /// vector, numeric widening, dropping names, union membership, contravariant
    /// parameters). `true` keeps the bindings it made; `false` rolls back everything.
    pub fn compatible(&mut self, db: &'db dyn Db, actual: Ty<'db>, expected: Ty<'db>) -> bool {
        self.compatible_probe(db, actual, expected, 0)
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
        }
        verdict
    }

    /// A numeric type variable that bound to `integer` meets a `double` argument of the
    /// same shape (`f(1L, 0.5)` for `function(a, b) a * b`): R's arithmetic promotes, so
    /// the variable widens to the wider type. Earlier arguments still fit, because
    /// `integer` is compatible with `double`. Logicals never widen a binding, since a
    /// body like `stopifnot(x + 1L > 0L); x` returns the argument's own type.
    fn widen_numeric_binding(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
    ) -> bool {
        let TyKind::Var(var) = expected.kind(db) else {
            return false;
        };
        let representative = self.find(*var);
        let Entry::Bound {
            ty: bound,
            numeric: true,
        } = *self.entry(representative)
        else {
            return false;
        };
        let actual = self.resolve(db, actual);
        let rank = |ty: Ty<'db>| match ty.kind(db) {
            TyKind::Scalar(Atomic::Integer) => Some((false, 1)),
            TyKind::Scalar(Atomic::Double) => Some((false, 2)),
            TyKind::Vector(element) => match element.kind(db) {
                TyKind::Scalar(Atomic::Integer) => Some((true, 1)),
                TyKind::Scalar(Atomic::Double) => Some((true, 2)),
                _ => None,
            },
            _ => None,
        };
        match (rank(bound), rank(actual)) {
            (Some((bound_vector, bound_rank)), Some((actual_vector, actual_rank)))
                if actual_rank > bound_rank
                    || (actual_rank == bound_rank && !bound_vector && actual_vector) =>
            {
                // A scalar meeting a vector widens to the vector: R recycles it.
                let element = scalar(
                    db,
                    if actual_rank.max(bound_rank) == 2 {
                        Atomic::Double
                    } else {
                        Atomic::Integer
                    },
                );
                let widened = if bound_vector || actual_vector {
                    Ty::new(db, TyKind::Vector(element))
                } else {
                    element
                };
                self.set(
                    representative,
                    Entry::Bound {
                        ty: widened,
                        numeric: true,
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
        if self.widen_numeric_binding(db, actual, expected) {
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
        if matches!(actual.kind(db), TyKind::Var(_)) || matches!(expected.kind(db), TyKind::Var(_))
        {
            return self.unify(db, actual, expected).is_ok();
        }
        if self.undeclared_nominal(db, actual) || self.undeclared_nominal(db, expected) {
            return true;
        }
        match (actual.kind(db), expected.kind(db)) {
            // First, so union-vs-union means every actual member fits some expected member.
            // An open source fits only an open target that it can vouch for: a tag hidden in
            // the source's `..` could carry any payload under a name the target lists.
            (TyKind::Union(members), _) => {
                if members
                    .iter()
                    .any(|member| matches!(member.kind(db), TyKind::OtherTags))
                {
                    let TyKind::Union(expected_members) = expected.kind(db) else {
                        return false;
                    };
                    let target_is_open = expected_members
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
                members
                    .iter()
                    .all(|&member| self.compatible_probe(db, member, expected, depth + 1))
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
            (_, TyKind::Union(members)) => {
                let (variables, concrete): (Vec<Ty<'db>>, Vec<Ty<'db>>) =
                    members.iter().partition(|&&member| {
                        matches!(self.shallow_resolve(db, member).kind(db), TyKind::Var(_))
                    });
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
                    .definitions
                    .and_then(|table| table.get(actual_name))
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
            (TyKind::Frame(None), TyKind::Frame(_)) | (TyKind::Frame(_), TyKind::Frame(None)) => {
                true
            }
            (TyKind::Frame(Some(actual_columns)), TyKind::Frame(Some(expected_columns)))
                if actual_columns.len() == expected_columns.len() =>
            {
                expected_columns.iter().all(|expected_column| {
                    actual_columns
                        .iter()
                        .find(|column| column.name == expected_column.name)
                        .is_some_and(|actual_column| {
                            self.compatible_probe(
                                db,
                                actual_column.ty,
                                expected_column.ty,
                                depth + 1,
                            )
                        })
                })
            }
            // A frame's run-time class is `c("frame", "data.frame")`, and it is a list of
            // its columns.
            (TyKind::Frame(_), TyKind::Named(name, arguments))
                if arguments.is_empty() && name.text(db) == "data.frame" =>
            {
                true
            }
            (TyKind::Frame(None), TyKind::List(_) | TyKind::NamedList(_)) => true,
            (TyKind::Frame(Some(columns)), TyKind::List(element) | TyKind::NamedList(element))
                if matches!(self.resolve(db, *element).kind(db), TyKind::Var(_)) =>
            {
                let joined = union_of(db, columns.iter().map(|column| column.ty));
                self.unify(db, *element, joined).is_ok()
            }
            (TyKind::Frame(Some(columns)), TyKind::List(element) | TyKind::NamedList(element)) => {
                columns
                    .iter()
                    .all(|column| self.compatible_probe(db, column.ty, *element, depth + 1))
            }
            // An open `T` takes the join of the items: unifying each in turn would pin `T`
            // to the first item and reject the rest.
            (TyKind::Tuple(items), TyKind::List(element))
                if matches!(self.resolve(db, *element).kind(db), TyKind::Var(_)) =>
            {
                let joined = union_of(db, items.iter().copied());
                self.unify(db, *element, joined).is_ok()
            }
            (TyKind::Record(fields), TyKind::List(element))
            | (TyKind::Record(fields), TyKind::NamedList(element))
                if matches!(self.resolve(db, *element).kind(db), TyKind::Var(_)) =>
            {
                let joined = union_of(db, fields.iter().map(|field| field.ty));
                self.unify(db, *element, joined).is_ok()
            }
            (TyKind::Tuple(items), TyKind::List(element)) => items
                .iter()
                .all(|&item| self.compatible_probe(db, item, *element, depth + 1)),
            // In R `list()` is also the empty named list, so it fits `list[named: T]`.
            (TyKind::Tuple(items), TyKind::NamedList(_)) if items.is_empty() => true,
            (TyKind::Record(fields), TyKind::List(element))
            | (TyKind::Record(fields), TyKind::NamedList(element)) => fields
                .iter()
                .all(|field| self.compatible_probe(db, field.ty, *element, depth + 1)),
            (TyKind::NamedList(actual_element), TyKind::List(expected_element))
            | (TyKind::NamedList(actual_element), TyKind::NamedList(expected_element))
            | (TyKind::List(actual_element), TyKind::List(expected_element)) => {
                self.compatible_probe(db, *actual_element, *expected_element, depth + 1)
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
            // Both sides are sorted by name, so equal names pair up position by position.
            (TyKind::NativeRecord(actual_fields), TyKind::NativeRecord(expected_fields))
                if actual_fields.len() == expected_fields.len() =>
            {
                actual_fields
                    .iter()
                    .zip(expected_fields.iter())
                    .all(|(actual_field, expected_field)| {
                        actual_field.name == expected_field.name
                            && self.compatible_probe(
                                db,
                                actual_field.ty,
                                expected_field.ty,
                                depth + 1,
                            )
                    })
            }
            (TyKind::Tag(actual_name, actual_payload), TyKind::Tag(expected_name, expected_payload))
                if actual_name == expected_name =>
            {
                match (actual_payload, expected_payload) {
                    (None, None) => true,
                    (Some(actual_payload), Some(expected_payload)) => {
                        self.compatible_probe(db, *actual_payload, *expected_payload, depth + 1)
                    }
                    _ => false,
                }
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
        let Some(pairing) = pair_parameters(actual, expected) else {
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
        self.compatible_probe(db, actual.ret, expected.ret, depth + 1)
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
        let pairing = pair_parameters(actual, expected)?;
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
        if !self.compatible(db, actual.ret, expected.ret) {
            let returns = self.resolve(db, actual.ret);
            return Some(FunctionMismatch::Return {
                required: self.resolve(db, expected.ret),
                constraint: self.variable_constraint(db, returns),
                returns,
            });
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
        let (TyKind::Record(actual_fields), TyKind::Record(expected_fields)) = (
            self.resolve(db, actual).kind(db),
            self.resolve(db, expected).kind(db),
        ) else {
            return None;
        };
        // Optionality is not compared: `compatible` does not compare it either.
        for expected_field in expected_fields {
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
        // Surplus fields last: a wrong type in a shared field is the likelier mistake.
        for actual_field in actual_fields {
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
    fn variable_constraint(&self, db: &'db dyn Db, ty: Ty<'db>) -> Option<Constraint> {
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
        constraint: Option<Constraint>,
    },
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
/// Variadics pair only with variadics at the same formal position (conservative).
fn pair_parameters<'db>(
    actual: &FunctionType<'db>,
    expected: &FunctionType<'db>,
) -> Option<Pairing<'db>> {
    let mut rest = Vec::new();
    match (&actual.variadic, &expected.variadic) {
        (Some(actual_variadic), Some(expected_variadic)) => {
            if actual_variadic.preceding_named != expected_variadic.preceding_named {
                return None;
            }
            rest.push((expected_variadic.element, actual_variadic.element));
        }
        (None, None) => {}
        _ => return None,
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
    for field in &expected.named {
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
            if actual_optional {
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
    let substitution: FxHashMap<Name<'db>, Ty<'db>> = definition
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
            for &argument in arguments {
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

/// Beyond atoms: a class declaring an arithmetic operator method is numeric
/// (`a + b` is legal R for it), and a rigid binder is admitted when its declared
/// bound is at least as strong (self-recursive calls unify it with a fresh variable).
fn constraint_rejects<'db>(
    db: &'db dyn Db,
    table: &InferenceTable<'db>,
    constraint: Constraint,
    ty: Ty<'db>,
) -> Option<UnifyError<'db>> {
    let arithmetic_classes = table.arithmetic_classes;
    let declares_arithmetic = |name: &Name<'db>| {
        arithmetic_classes.is_some_and(|classes| classes.contains(name.text(db)))
    };
    // The declared bound implies the required one when joining does not strengthen it.
    let binder_implies = |name: &Name<'db>| {
        table
            .rigid_constraints
            .get(name)
            .is_some_and(|declared| declared.join(constraint) == *declared)
    };
    let admissible = match constraint {
        Constraint::Unconstrained => true,
        Constraint::Numeric => match ty.kind(db) {
            TyKind::Scalar(Atomic::Integer | Atomic::Double) | TyKind::Any | TyKind::Unknown => {
                true
            }
            TyKind::Vector(element) | TyKind::NamedVector(element) => matches!(
                element.kind(db),
                TyKind::Scalar(Atomic::Integer | Atomic::Double)
            ),
            TyKind::Named(name, _) => declares_arithmetic(name),
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::AtomicElement => match ty.kind(db) {
            TyKind::Scalar(_) | TyKind::Any | TyKind::Unknown => true,
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
        Constraint::ScalarNumeric => match ty.kind(db) {
            TyKind::Scalar(Atomic::Integer | Atomic::Double) | TyKind::Any | TyKind::Unknown => {
                true
            }
            TyKind::Named(name, _) => declares_arithmetic(name),
            TyKind::Rigid(name) => binder_implies(name),
            _ => false,
        },
    };
    (!admissible).then_some(UnifyError::ConstraintRejected(constraint, ty))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RootDatabase;
    use crate::types::{Parameter, null, scalar};

    #[test]
    fn unify_binds_and_resolves() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let var = table.fresh_ty(&db, Constraint::Unconstrained);
        let int = scalar(&db, Atomic::Integer);
        table.unify(&db, var, int).expect("binds");
        assert_eq!(table.resolve(&db, var), int);
    }

    #[test]
    fn occurs_check_refuses_infinite_types() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let var = table.fresh(Constraint::Unconstrained);
        let var_ty = Ty::new(&db, TyKind::Var(var));
        let list = Ty::new(&db, TyKind::List(var_ty));
        assert!(matches!(
            table.unify(&db, var_ty, list),
            Err(UnifyError::Occurs(..))
        ));
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
        assert!(matches!(
            table.resolve(&db, outer).kind(&db),
            TyKind::Var(_)
        ));
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
        assert!(matches!(table.resolve(&db, t).kind(&db), TyKind::Var(_)));
    }

    #[test]
    fn compatibility_fails_pure_and_pairs_function_formals_by_name() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let int = scalar(&db, Atomic::Integer);
        let chr = scalar(&db, Atomic::Character);
        let var = table.fresh_ty(&db, Constraint::Unconstrained);
        let tuple_with_var = Ty::new(&db, TyKind::Tuple(vec![var, chr]));
        let tuple_expected = Ty::new(&db, TyKind::Tuple(vec![int, int]));
        assert!(!table.compatible(&db, tuple_with_var, tuple_expected));
        assert!(matches!(table.resolve(&db, var).kind(&db), TyKind::Var(_)));
        let named_lambda = Ty::new(
            &db,
            TyKind::Function(FunctionType {
                positional: Vec::new(),
                named: vec![Parameter {
                    name: Name::new(&db, "x".to_owned()),
                    ty: int,
                    optional: false,
                    default: None,
                }],
                variadic: None,
                ret: int,
            }),
        );
        let positional_interface = Ty::new(
            &db,
            TyKind::Function(FunctionType {
                positional: vec![int],
                named: Vec::new(),
                variadic: None,
                ret: scalar(&db, Atomic::Double),
            }),
        );
        assert!(table.compatible(&db, named_lambda, positional_interface));
    }
}
