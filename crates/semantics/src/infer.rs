//! The inference substrate: a dense union-find table over interned types.
//!
//! Unification stays syntactic — the invariant floor: a union unifies only
//! with a structurally equal union (set equality after resolution), plus the
//! single `T | NULL`-vs-`U | NULL` member-wise case; all directional
//! member-wise logic lives in compatibility checking, never here. A union may
//! be *bound to* a variable, but no union constraint is ever imposed on one
//! (the HM-speed guardrail). Constraints ride on unbound entries and join
//! through the lattice on redirect.
//!
//! Probes snapshot the table and roll back completely: the entry vector
//! truncates to its snapshot length and mutated older entries revert through
//! an undo log, so a failed probe leaves no trace.

use crate::Db;
use crate::annotations::NamedDefinition;
use crate::diagnostics::nearest_field_name;
use crate::types::{
    Atomic, Constraint, FunctionType, InferenceVar, Name, Ty, TyKind, substitute_rigid, union_of,
};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry<'db> {
    Unbound { level: u32, constraint: Constraint },
    Bound(Ty<'db>),
    Redirect(InferenceVar),
}

/// Why a unification failed; the compatibility layer turns this into wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifyError<'db> {
    Mismatch(Ty<'db>, Ty<'db>),
    /// The occurs check refused an infinite type.
    Occurs(InferenceVar, Ty<'db>),
    /// A constraint rejected the bound type (e.g. numeric vs character).
    ConstraintRejected(Constraint, Ty<'db>),
}

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    entries: usize,
    undo: usize,
}

/// The dense entry table: the id counter IS the length, so a dangling id is
/// unrepresentable.
#[derive(Debug, Default)]
pub struct InferenceTable<'db> {
    entries: Vec<Entry<'db>>,
    /// Previous values of entries mutated below a snapshot boundary.
    undo: Vec<(InferenceVar, Entry<'db>)>,
    pub level: u32,
    /// The `@type` / `@alias` definitions visible here: aliases expand during
    /// resolution, nominals project to their representation in compatibility.
    /// Borrowed from the memoized per-file table — see `GlobalEnv`. `None` when
    /// the check runs with no globals at all, which reads as an empty table.
    pub definitions: Option<&'db FxHashMap<Name<'db>, NamedDefinition<'db>>>,
    /// Classes that declare an arithmetic operator method, from the standard
    /// library AND from the project's own sources. Operator dispatch resolves
    /// such a method through the global scope, so the numeric constraint has to
    /// consult the same set or it refuses a class whose `+` the checker itself
    /// would have dispatched.
    pub arithmetic_classes: Option<&'db FxHashSet<String>>,
    /// Declared constraints of the rigid binders in scope (`<T: numeric>`).
    ///
    /// This lives with the solver rather than with the expression walker
    /// because admissibility is decided here: a rigid `T` is not a numeric
    /// atom, so without its declared bound every check that unifies it with a
    /// constrained variable refuses it — which is how an annotation the checker
    /// wrote itself could turn a clean file red on a self-recursive call.
    pub rigid_constraints: FxHashMap<Name<'db>, Constraint>,
    /// Per-node resolve memo over the interned type DAG: without it, shared
    /// subtrees re-resolve once per occurrence and self-referential bindings
    /// walk an exponential tree. Entries are valid for one binding epoch
    /// (any bind or rollback clears) and only subtrees whose resolution hit
    /// no cycle cut are stored.
    resolve_cache: std::cell::RefCell<(u64, rustc_hash::FxHashMap<Ty<'db>, Ty<'db>>)>,
    /// Bumped on every binding mutation and rollback; tags `resolve_cache`.
    epoch: std::cell::Cell<u64>,
}

/// Inner resolve steps since process start — a cheap standing instrument for
/// the perf witnesses: the step count must stay near-linear in corpus size,
/// so a blowup here flags a resolve-memoization regression before wall-clock
/// does.
pub static RESOLVE_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How deep resolution expands alias applications before leaving one as
/// written. Well past any alias a reader would nest by hand, and short enough
/// that an alias whose body mentions itself cannot expand forever.
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
        let previous = std::mem::replace(&mut self.entries[var.0 as usize], entry);
        self.undo.push((var, previous));
    }

    /// The representative variable at the end of a redirect chain.
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

    /// Whether `ty` is an UNUSABLE nominal reference: a name no vocabulary
    /// declares (neither the project's `@type`/`@alias` table nor the stub
    /// corpus), or a declared name applied with the wrong number of type
    /// arguments. Both states already carry their own annotation diagnostic
    /// (unknown type name / generic arity); the relations treat the type
    /// like `Unknown` so the one mistake never cascades.
    pub(crate) fn undeclared_nominal(&self, db: &'db dyn Db, ty: Ty<'db>) -> bool {
        let TyKind::Named(name, arguments) = ty.kind(db) else {
            return false;
        };
        match self.definitions.and_then(|table| table.get(name)) {
            // A wrong argument count includes the bare use of a generic
            // (`Box` for a one-parameter `Box<T>`) — `@new` is unaffected,
            // its representation check infers arguments without the
            // relations.
            Some(definition) => definition.parameters.len() != arguments.len(),
            None => !crate::stubs::stubs(db)
                .is_some_and(|library| library.nominals.contains(name.text(db))),
        }
    }

    /// Shallow-resolve: follow variables to their binding, without walking
    /// into structure.
    pub fn shallow_resolve(&self, db: &'db dyn Db, ty: Ty<'db>) -> Ty<'db> {
        let mut current = ty;
        loop {
            match current.kind(db) {
                TyKind::Var(var) => match self.entry(*var) {
                    Entry::Bound(bound) => current = *bound,
                    _ => {
                        // Canonicalize to the representative.
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

    /// Deep-resolve: replace every bound variable in the structure and expand
    /// alias applications. Memoized per binding epoch over the interned type
    /// DAG; self-referential bindings and aliases cut to `Unknown` instead of
    /// expanding forever (see `resolve_rec`).
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

    /// One resolve step over the interned DAG. Returns the resolved type and
    /// whether the subtree was CLEAN — no cycle cut beneath it — because only
    /// clean results are position-independent enough to memoize (a node
    /// containing a variable that is currently being expanded resolves
    /// differently at top level). Expanding a variable that is already on the
    /// expansion stack is an infinite type: it cuts to `Unknown`, matching
    /// the pin-to-Unknown semantics used everywhere self-reference grows.
    fn resolve_rec(
        &self,
        db: &'db dyn Db,
        ty: Ty<'db>,
        visiting: &mut Vec<InferenceVar>,
    ) -> (Ty<'db>, bool) {
        RESOLVE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        let resolved = crate::types::map_child_types(db, ty, &mut |child| {
            let (child, child_clean) = self.resolve_rec(db, child, visiting);
            clean &= child_clean;
            child
        });
        // An alias is a transparent shorthand: it expands here and never
        // survives into unification or compatibility. A self-referential alias
        // would re-enter through the SAME interned application, so the
        // expansion is bounded by the visiting stack's length.
        let alias = match resolved.kind(db) {
            TyKind::Named(name, arguments) => self
                .definitions
                .and_then(|table| table.get(name))
                .filter(|definition| definition.alias)
                .map(|definition| (definition, arguments.clone())),
            _ => None,
        };
        let (resolved, clean) = match alias {
            Some((definition, arguments)) if visiting.len() < ALIAS_EXPANSION_LIMIT => {
                let expanded = apply_definition(db, definition, &arguments);
                // Guard alias self-reference with a sentinel slot on the same
                // stack the variable cycle check uses.
                visiting.push(InferenceVar(u32::MAX));
                let (expanded, expanded_clean) = self.resolve_rec(db, expanded, visiting);
                visiting.pop();
                (expanded, clean && expanded_clean)
            }
            // Past the bound the application stays as written, which is not an
            // answer other positions may reuse.
            Some(_) => (resolved, false),
            None => (resolved, clean),
        };
        if clean {
            self.resolve_cache.borrow_mut().1.insert(ty, resolved);
        }
        (resolved, clean)
    }

    /// A non-alias nominal's representation with its parameters applied;
    /// `None` for aliases, opaque nominals (no representation), and unknown
    /// names.
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

    /// Bind `var` to `ty` after the occurs check and constraint admission.
    fn bind(
        &mut self,
        db: &'db dyn Db,
        var: InferenceVar,
        ty: Ty<'db>,
    ) -> Result<(), UnifyError<'db>> {
        let representative = self.find(var);
        if self.occurs(db, representative, ty) {
            return Err(UnifyError::Occurs(representative, ty));
        }
        let Entry::Unbound { level, constraint } = *self.entry(representative) else {
            unreachable!("bind target is always unbound after find");
        };
        if let Some(error) = constraint_rejects(db, self, constraint, ty) {
            return Err(error);
        }
        // Level-adjust free variables in `ty` up to this entry's level so
        // generalization at an outer level cannot quantify them away.
        self.adjust_levels(db, level, ty);
        self.set(representative, Entry::Bound(ty));
        Ok(())
    }

    fn adjust_levels(&mut self, db: &'db dyn Db, level: u32, ty: Ty<'db>) {
        let shallow = self.shallow_resolve(db, ty);
        match shallow.kind(db) {
            TyKind::Var(var) => {
                let representative = self.find(*var);
                if let Entry::Unbound {
                    level: var_level,
                    constraint,
                } = *self.entry(representative)
                    && var_level > level
                {
                    self.set(representative, Entry::Unbound { level, constraint });
                }
            }
            TyKind::Vector(inner)
            | TyKind::NamedVector(inner)
            | TyKind::List(inner)
            | TyKind::NamedList(inner) => self.adjust_levels(db, level, *inner),
            TyKind::Tuple(items) => {
                for &item in items.clone().iter() {
                    self.adjust_levels(db, level, item);
                }
            }
            TyKind::Record(fields) => {
                for field in fields.clone().iter() {
                    self.adjust_levels(db, level, field.ty);
                }
            }
            TyKind::Function(function) => {
                let function = function.clone();
                for &ty in &function.positional {
                    self.adjust_levels(db, level, ty);
                }
                for ty in function
                    .named
                    .iter()
                    .flat_map(|parameter| parameter.types())
                {
                    self.adjust_levels(db, level, ty);
                }
                if let Some(rest) = &function.variadic {
                    self.adjust_levels(db, level, rest.element);
                }
                self.adjust_levels(db, level, function.ret);
            }
            TyKind::Union(members) => {
                for &member in members.clone().iter() {
                    self.adjust_levels(db, level, member);
                }
            }
            TyKind::Named(_, arguments) => {
                for &argument in arguments.clone().iter() {
                    self.adjust_levels(db, level, argument);
                }
            }
            _ => {}
        }
    }

    fn occurs(&self, db: &'db dyn Db, var: InferenceVar, ty: Ty<'db>) -> bool {
        let shallow = self.shallow_resolve(db, ty);
        match shallow.kind(db) {
            TyKind::Var(other) => self.find(*other) == var,
            TyKind::Vector(inner)
            | TyKind::NamedVector(inner)
            | TyKind::List(inner)
            | TyKind::NamedList(inner) => self.occurs(db, var, *inner),
            TyKind::Tuple(items) => items.iter().any(|&item| self.occurs(db, var, item)),
            TyKind::Record(fields) => fields.iter().any(|field| self.occurs(db, var, field.ty)),
            TyKind::Function(function) => {
                function
                    .positional
                    .iter()
                    .any(|&ty| self.occurs(db, var, ty))
                    || function
                        .named
                        .iter()
                        .flat_map(|parameter| parameter.types())
                        .any(|ty| self.occurs(db, var, ty))
                    || function
                        .variadic
                        .as_ref()
                        .is_some_and(|rest| self.occurs(db, var, rest.element))
                    || self.occurs(db, var, function.ret)
            }
            TyKind::Union(members) => members.iter().any(|&member| self.occurs(db, var, member)),
            TyKind::Named(_, arguments) => arguments
                .iter()
                .any(|&argument| self.occurs(db, var, argument)),
            _ => false,
        }
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
                // Redirect the younger to the older, joining constraints and
                // keeping the minimum level.
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
            // The tolerance floor: `Any` is the compatible-with-all top, and
            // `Unknown` must never cascade a second error after its own gap
            // was already diagnosed. (After the Var arms, so a variable still
            // binds to `Any`/`Unknown` rather than being skipped.)
            (TyKind::Any, _) | (_, TyKind::Any) | (TyKind::Unknown, _) | (_, TyKind::Unknown) => {
                Ok(())
            }
            // An undeclared nominal — a name neither the project's type
            // table nor the stub corpus declares — already carries its own
            // unknown-type annotation error; comparisons treat it like
            // `Unknown` so the typo never cascades.
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
                let pairs: Vec<(Ty<'db>, Ty<'db>)> =
                    left.iter().copied().zip(right.iter().copied()).collect();
                for (left, right) in pairs {
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
                let pairs: Vec<(Ty<'db>, Ty<'db>)> = left
                    .iter()
                    .map(|f| f.ty)
                    .zip(right.iter().map(|f| f.ty))
                    .collect();
                for (left, right) in pairs {
                    self.unify(db, left, right)?;
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
                let left = left.clone();
                let right = right.clone();
                for (l, r) in left.positional.iter().zip(right.positional.iter()) {
                    self.unify(db, *l, *r)?;
                }
                for (l, r) in left.named.iter().zip(right.named.iter()) {
                    self.unify(db, l.ty, r.ty)?;
                }
                if let (Some(l), Some(r)) = (&left.variadic, &right.variadic) {
                    self.unify(db, l.element, r.element)?;
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
                let pairs: Vec<(Ty<'db>, Ty<'db>)> = left_arguments
                    .iter()
                    .copied()
                    .zip(right_arguments.iter().copied())
                    .collect();
                for (left, right) in pairs {
                    self.unify(db, left, right)?;
                }
                Ok(())
            }
            (TyKind::Union(_), TyKind::Union(_)) => self.unify_unions(db, a, b),
            _ => Err(UnifyError::Mismatch(a, b)),
        }
    }

    /// Unions unify by set equality after resolution, plus the single
    /// `T | NULL` vs `U | NULL` member-wise case (both two-member nullable
    /// unions: unify the non-NULL members).
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

    /// Raise a variable's constraint through the lattice (or verify an
    /// already-bound one admits it).
    /// The constraint a still-unbound type carries, when it is a bare
    /// variable. `Unconstrained` means the program has demanded nothing of it
    /// yet — the state that makes pinning it from elsewhere an invention.
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
            Entry::Bound(ty) => match constraint_rejects(db, self, constraint, ty) {
                Some(error) => Err(error),
                None => Ok(()),
            },
            Entry::Redirect(_) => unreachable!("find returns a representative"),
        }
    }

    /// The directional argument-compatibility relation — the coercions that
    /// apply where a value flows into an expected type (parameter positions,
    /// checked annotations) but never inside unification: scalar-to-vector,
    /// integer-to-double widening, names dropping into unnamed containers,
    /// union membership, and contravariant function parameters.
    ///
    /// Runs as a probe: a `true` verdict keeps the variable bindings it made
    /// (binding against the two `Var` arms is how a generic parameter like
    /// `T[]` infers `T` from a call), while `false` reverses every mutation,
    /// so the predicate is pure on failure and its result order-independent.
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
        // A resource guard, not a verdict: interned types are finite (the
        // occurs check refuses cycles), so this bound is unreachable for any
        // real program and refusing is safer than recursing on.
        const DEPTH_LIMIT: usize = 128;
        let snapshot = self.snapshot();
        let verdict = depth < DEPTH_LIMIT && self.compatible_inner(db, actual, expected, depth);
        if !verdict {
            self.rollback(snapshot);
        }
        verdict
    }

    fn compatible_inner(
        &mut self,
        db: &'db dyn Db,
        actual: Ty<'db>,
        expected: Ty<'db>,
        depth: usize,
    ) -> bool {
        let actual = self.resolve(db, actual);
        let expected = self.resolve(db, expected);
        // The tolerance floor, mirroring `unify`: `Any` is the sanctioned
        // escape hatch and `Unknown` an absent fact — neither side of an
        // unknown is a checkable claim, so it is compatible with everything.
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
        // An undeclared nominal compares like `Unknown` — see `unify`.
        if self.undeclared_nominal(db, actual) || self.undeclared_nominal(db, expected) {
            return true;
        }
        match (actual.kind(db).clone(), expected.kind(db).clone()) {
            // A union value must be accepted in every shape it can take, so
            // each actual member checks against the expected type. This arm
            // comes first so union-vs-union reduces to "every actual member
            // fits somewhere in the expected union".
            (TyKind::Union(members), _) => members
                .iter()
                .all(|&member| self.compatible_probe(db, member, expected, depth + 1)),
            // A value fits an expected union when it fits any member; concrete
            // members are tried before unbound-variable members so a value
            // that already fits a concrete member — `NULL` fitting the `NULL`
            // in an instantiated `T | NULL` — matches it and binds nothing,
            // rather than greedily pinning `T` and robbing a later argument of
            // the chance to determine it.
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
            // Same-name nominals check each type argument in the direction
            // its variance dictates: covariant for return/container/direct
            // positions, contravariant for function-parameter positions,
            // invariant (both directions) when a parameter occurs in
            // conflicting positions or the definition is missing —
            // conservative over-rejection, never an unsound widening.
            (
                TyKind::Named(actual_name, actual_arguments),
                TyKind::Named(expected_name, expected_arguments),
            ) if actual_name == expected_name
                && actual_arguments.len() == expected_arguments.len() =>
            {
                let variances = self
                    .definitions
                    .and_then(|table| table.get(&actual_name))
                    .map(|definition| parameter_variances(db, definition))
                    .unwrap_or_default();
                actual_arguments
                    .iter()
                    .zip(expected_arguments.iter())
                    .enumerate()
                    .all(|(index, (&actual_argument, &expected_argument))| {
                        match variances.get(index).copied().unwrap_or(Variance::Invariant) {
                            // The parameter never occurs in the
                            // representation, so any argument is accepted.
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
            // A nominal value is compatible with anything its representation
            // is (the projection direction); the reverse — a structural value
            // flowing INTO a nominal position — happens only through `@new`.
            (TyKind::Named(actual_name, actual_arguments), _) => {
                match self.representation(db, actual_name, &actual_arguments) {
                    Some(representation) => {
                        self.compatible_probe(db, representation, expected, depth + 1)
                    }
                    None => false,
                }
            }
            // A scalar coerces into a vector position; a named vector drops
            // its names into a plain vector position. Element recursion lands
            // on the scalar arm below for concrete elements (so integer
            // widening applies inside vectors too) and on the variable arms
            // above for a generic element (`T[]`), which is how a call like
            // `sort(c(1L))` binds `T := integer`.
            (TyKind::Scalar(_), TyKind::Vector(element)) => {
                self.compatible_probe(db, actual, element, depth + 1)
            }
            (TyKind::NamedVector(actual_element), TyKind::Vector(expected_element)) => {
                self.compatible_probe(db, actual_element, expected_element, depth + 1)
            }
            // The numeric ladder widens in compatibility (a directional check
            // only — unification never widens): R freely promotes `logical`
            // and `integer` in numeric contexts (`sum(flags)`,
            // `mean(x > threshold)`, `(x > 0) * weight`), and without this
            // every numeric parameter in the stub corpus would have to be
            // `Any`.
            (TyKind::Scalar(actual_atomic), TyKind::Scalar(expected_atomic)) => {
                numeric_ladder_rank(actual_atomic)
                    .zip(numeric_ladder_rank(expected_atomic))
                    .is_some_and(|(actual_rank, expected_rank)| actual_rank < expected_rank)
            }
            (TyKind::Vector(actual_element), TyKind::Vector(expected_element))
            | (TyKind::NamedVector(actual_element), TyKind::NamedVector(expected_element)) => {
                self.compatible_probe(db, actual_element, expected_element, depth + 1)
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
            // A fixed-shape list flowing into `list[T]`: when `T` is still
            // open, it takes the JOIN of the items rather than unifying with
            // each in turn — otherwise the first item pins `T` and every later
            // one is a mismatch, so `lapply(list(1L, "a"), f)` failed while
            // `for` over the same list is documented to bind
            // `integer | character`. A concrete `T` keeps the all-must-fit
            // rule.
            (TyKind::Tuple(items), TyKind::List(element))
                if matches!(self.resolve(db, element).kind(db), TyKind::Var(_)) =>
            {
                let joined = union_of(db, items);
                self.unify(db, element, joined).is_ok()
            }
            (TyKind::Record(fields), TyKind::List(element))
            | (TyKind::Record(fields), TyKind::NamedList(element))
                if matches!(self.resolve(db, element).kind(db), TyKind::Var(_)) =>
            {
                let joined = union_of(db, fields.iter().map(|field| field.ty));
                self.unify(db, element, joined).is_ok()
            }
            (TyKind::Tuple(items), TyKind::List(element)) => items
                .iter()
                .all(|&item| self.compatible_probe(db, item, element, depth + 1)),
            // `list()` is both the empty unnamed and the empty map-like list in
            // R — it has no element whose name could be missing — so it
            // satisfies a `list[named: T]` parameter. That is what makes
            // `= list()` a usable default for one.
            (TyKind::Tuple(items), TyKind::NamedList(_)) if items.is_empty() => true,
            (TyKind::Record(fields), TyKind::List(element))
            | (TyKind::Record(fields), TyKind::NamedList(element)) => fields
                .iter()
                .all(|field| self.compatible_probe(db, field.ty, element, depth + 1)),
            (TyKind::NamedList(actual_element), TyKind::List(expected_element))
            | (TyKind::NamedList(actual_element), TyKind::NamedList(expected_element))
            | (TyKind::List(actual_element), TyKind::List(expected_element)) => {
                self.compatible_probe(db, actual_element, expected_element, depth + 1)
            }
            (TyKind::Function(actual_function), TyKind::Function(expected_function)) => {
                self.function_compatible(db, &actual_function, &expected_function, depth)
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
            // Parameters are contravariant: a function used where `expected`
            // is wanted must accept every argument that interface may pass.
            if !self.compatible_probe(db, pair.passed, pair.accepts, depth + 1) {
                return false;
            }
        }
        // Return types stay covariant.
        self.compatible_probe(db, actual.ret, expected.ret, depth + 1)
    }

    /// Why `actual` does not serve `expected`, for a caller about to report
    /// the failure. [`Self::compatible`] only answers yes or no, and two whole
    /// signatures printed side by side leave the reader to find the position
    /// that failed — worse, a parameter's constraint does not survive into the
    /// rendered type at all, so an acceptable and an unacceptable function can
    /// print identically. Returns `None` when the shapes disagree rather than
    /// one pairing (arity, optionality, the rest parameter), which the whole
    /// signatures do show.
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

    /// Which field keeps `actual` from serving `expected`, for a caller about
    /// to report the failure. Two record types printed side by side are long
    /// near-identical strings the reader has to diff by eye, and a nested one
    /// never names the path at all.
    ///
    /// Pairs fields by name — the rule [`Self::compatible`] uses, so the
    /// explanation cannot disagree with the verdict — and recurses while both
    /// sides are records, so the path reads outermost first. `None` when the
    /// failure is not about one field, which leaves the whole types to say so.
    ///
    /// Rolls the table back before returning. [`Self::compatible`] *keeps* the
    /// bindings a `true` verdict made — that is how a generic parameter infers
    /// its argument — so probing the fields that fit would otherwise leak those
    /// bindings out of a reporting path and change a later verdict in the same
    /// item. Explaining a failure must not alter what is being explained.
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
            self.resolve(db, actual).kind(db).clone(),
            self.resolve(db, expected).kind(db).clone(),
        ) else {
            return None;
        };
        // Optionality is deliberately not compared: `compatible` does not
        // either, so treating it as a difference here would explain a failure
        // that something else caused.
        for expected_field in &expected_fields {
            let name = expected_field.name.text(db);
            let Some(actual_field) = actual_fields
                .iter()
                .find(|field| field.name == expected_field.name)
            else {
                path.push(name.to_owned());
                return Some(RecordMismatch::Missing {
                    near: nearest_field_name(
                        name,
                        actual_fields.iter().map(|field| field.name.text(db)),
                    ),
                    path: std::mem::take(path),
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
                path: std::mem::take(path),
            });
        }
        // A surplus field, checked only after every paired one: a wrong type in
        // a field both sides declare is the likelier mistake, and a renamed
        // field shows up as the missing half above with this one as its hint.
        for actual_field in &actual_fields {
            if expected_fields
                .iter()
                .any(|field| field.name == actual_field.name)
            {
                continue;
            }
            let name = actual_field.name.text(db);
            path.push(name.to_owned());
            return Some(RecordMismatch::Unexpected {
                near: nearest_field_name(
                    name,
                    expected_fields.iter().map(|field| field.name.text(db)),
                ),
                path: std::mem::take(path),
            });
        }
        None
    }

    /// The constraint an unbound variable carries. This is the one fact the
    /// rendered type cannot show: `fn(s: U) -> U` prints the same whether `U`
    /// accepts anything or only numbers, which is why a plain mismatch between
    /// two whole signatures can describe a call that should have fit.
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

    /// Whether the resolved form of `ty` still contains an unbound inference
    /// variable anywhere in its structure.
    pub fn contains_unbound_var(&self, db: &'db dyn Db, ty: Ty<'db>) -> bool {
        !self.walk_unbound_vars(db, ty, &mut |_| false)
    }

    /// Every unbound inference variable reachable from `ty`, canonicalized to
    /// its representative.
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

    /// Visits the unbound variables of `ty`'s resolved form, stopping as soon
    /// as `visit` returns `false`. Returns whether the whole structure was
    /// visited, so a `visit` that always stops answers "is there one".
    fn walk_unbound_vars(
        &self,
        db: &'db dyn Db,
        ty: Ty<'db>,
        visit: &mut impl FnMut(InferenceVar) -> bool,
    ) -> bool {
        let shallow = self.shallow_resolve(db, ty);
        match shallow.kind(db) {
            TyKind::Var(var) => visit(*var),
            TyKind::Vector(inner)
            | TyKind::NamedVector(inner)
            | TyKind::List(inner)
            | TyKind::NamedList(inner) => self.walk_unbound_vars(db, *inner, visit),
            TyKind::Tuple(items) => items
                .iter()
                .all(|&item| self.walk_unbound_vars(db, item, visit)),
            TyKind::Record(fields) => fields
                .iter()
                .all(|field| self.walk_unbound_vars(db, field.ty, visit)),
            TyKind::Function(function) => {
                function
                    .positional
                    .iter()
                    .all(|&ty| self.walk_unbound_vars(db, ty, visit))
                    && function
                        .named
                        .iter()
                        .flat_map(|parameter| parameter.types())
                        .all(|ty| self.walk_unbound_vars(db, ty, visit))
                    && function
                        .variadic
                        .as_ref()
                        .is_none_or(|rest| self.walk_unbound_vars(db, rest.element, visit))
                    && self.walk_unbound_vars(db, function.ret, visit)
            }
            TyKind::Union(members) => members
                .iter()
                .all(|&member| self.walk_unbound_vars(db, member, visit)),
            TyKind::Named(_, arguments) => arguments
                .iter()
                .all(|&argument| self.walk_unbound_vars(db, argument, visit)),
            _ => true,
        }
    }
}

/// The one position that keeps a function value from serving an expected
/// function type, from [`InferenceTable::explain_function_mismatch`]. Each
/// variant carries the constraint of its own side when that side is a
/// constrained variable — the fact the rendered type drops.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum FunctionMismatch<'db> {
    /// A parameter: the interface passes a value the function will not take.
    Parameter {
        /// The function's own name for the parameter, when it has one.
        name: Option<String>,
        /// What the expected interface may pass into that position.
        passed: Ty<'db>,
        /// What the function's parameter accepts.
        accepts: Ty<'db>,
        constraint: Option<Constraint>,
    },
    /// The return: the function produces a value the interface will not take.
    Return {
        /// What the expected interface requires back.
        required: Ty<'db>,
        /// What the function produces.
        returns: Ty<'db>,
        constraint: Option<Constraint>,
    },
}

/// The one field that keeps a record from serving an expected record type,
/// from [`InferenceTable::explain_record_mismatch`]. `path` always ends in the
/// field the finding is about and names its enclosing fields before it, so a
/// nested failure reads `config.retries`.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum RecordMismatch<'db> {
    /// A field both sides declare, whose types do not fit.
    Field {
        path: Vec<String>,
        expected: Ty<'db>,
        found: Ty<'db>,
    },
    /// A field the expected type declares that the value does not have.
    Missing {
        path: Vec<String>,
        /// A field the value does have whose name is a near miss — which is
        /// how a renamed field reads, since it goes missing and turns up
        /// misspelled at the same time.
        near: Option<String>,
    },
    /// A field the value has that the expected type does not declare.
    Unexpected {
        path: Vec<String>,
        near: Option<String>,
    },
}

/// How a call's arguments would land on a function's parameters, shared by the
/// compatibility verdict and the explanation so there is one pairing rule.
/// `None` when the two shapes cannot pair at all.
struct Pairing<'db> {
    parameters: Vec<ParameterPair<'db>>,
    /// Expected parameters absorbed by the function's rest parameter, plus the
    /// two rest elements themselves, as (passed, accepts) pairs.
    rest: Vec<(Ty<'db>, Ty<'db>)>,
}

struct ParameterPair<'db> {
    name: Option<Name<'db>>,
    /// The expected interface's parameter type — what it may pass in.
    passed: Ty<'db>,
    /// The function's own parameter type — what it accepts. Parameters are
    /// contravariant, so `passed` must fit `accepts`, not the other way round.
    accepts: Ty<'db>,
}

/// Pairs a function's parameters with an expected interface's.
///
/// Arity is a range, not a number. An interface promises its callers every call
/// shape from its required count up to everything it declares, and a function
/// serves that interface when it accepts all of them. So it may declare MORE
/// parameters than the interface ever passes, as long as the extras default —
/// `mean(x, trim, na.rm)` serves a one-argument callback interface — and it may
/// not require more than the interface supplies.
///
/// Variadic pairing is conservative: a variadic function pairs only with
/// another variadic, and the rest parameters must sit at the same formal
/// position — the position decides which parameters callers may fill
/// positionally. This over-rejects some safe pairings but never admits an
/// unsound one.
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
    // Parameters pair by NAME where both sides name them (R matches call
    // arguments against formal names regardless of order); unnamed parameters
    // consume the remaining slots left to right. A named expected parameter
    // with no same-named actual falls back to positional pairing.
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
    // An expected parameter with no slot left is one the interface may pass
    // and the function cannot receive — unless the function is variadic, whose
    // rest parameter absorbs it (contravariantly, like every other parameter).
    for (expected_parameter, _) in positional_expected {
        let variadic = actual.variadic.as_ref()?;
        rest.push((expected_parameter, variadic.element));
    }
    let mut parameters = Vec::new();
    for ((name, actual_parameter, actual_optional), slot) in
        actual_parameters.into_iter().zip(paired)
    {
        let Some((expected_parameter, expected_optional)) = slot else {
            // A parameter the interface never passes is fine when the function
            // defaults it, and a missing argument otherwise.
            if actual_optional {
                continue;
            }
            return None;
        };
        // An expected-optional parameter promises callers they may omit it, so
        // the actual function must default it.
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

/// A definition's body with its parameters replaced by the given arguments
/// (missing arguments fill as `Unknown`).
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
                arguments
                    .get(index)
                    .copied()
                    .unwrap_or_else(|| crate::types::unknown(db)),
            )
        })
        .collect();
    substitute_rigid(db, definition.body, &substitution)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variance {
    /// The parameter never occurs in the representation.
    Bivariant,
    Covariant,
    Contravariant,
    /// The parameter occurs in conflicting positions.
    Invariant,
}

/// Each parameter's variance, derived from where it occurs in the
/// representation: covariant for direct/container/field/return positions,
/// contravariant (flipped) under function parameters, invariant when both.
pub fn parameter_variances<'db>(
    db: &'db dyn Db,
    definition: &NamedDefinition<'db>,
) -> Vec<Variance> {
    let mut positive = vec![false; definition.parameters.len()];
    let mut negative = vec![false; definition.parameters.len()];
    record_occurrences(
        db,
        definition.body,
        &definition.parameters,
        true,
        &mut positive,
        &mut negative,
    );
    positive
        .into_iter()
        .zip(negative)
        .map(|(positive, negative)| match (positive, negative) {
            (false, false) => Variance::Bivariant,
            (true, false) => Variance::Covariant,
            (false, true) => Variance::Contravariant,
            (true, true) => Variance::Invariant,
        })
        .collect()
}

fn record_occurrences<'db>(
    db: &'db dyn Db,
    ty: Ty<'db>,
    parameters: &[Name<'db>],
    positive_position: bool,
    positive: &mut [bool],
    negative: &mut [bool],
) {
    match ty.kind(db) {
        TyKind::Rigid(name) => {
            if let Some(index) = parameters.iter().position(|parameter| parameter == name) {
                if positive_position {
                    positive[index] = true;
                } else {
                    negative[index] = true;
                }
            }
        }
        TyKind::Vector(inner)
        | TyKind::NamedVector(inner)
        | TyKind::List(inner)
        | TyKind::NamedList(inner) => {
            record_occurrences(
                db,
                *inner,
                parameters,
                positive_position,
                positive,
                negative,
            );
        }
        TyKind::Tuple(items) => {
            for &item in items {
                record_occurrences(db, item, parameters, positive_position, positive, negative);
            }
        }
        TyKind::Record(fields) => {
            for field in fields {
                record_occurrences(
                    db,
                    field.ty,
                    parameters,
                    positive_position,
                    positive,
                    negative,
                );
            }
        }
        TyKind::Function(function) => {
            for &parameter in &function.positional {
                record_occurrences(
                    db,
                    parameter,
                    parameters,
                    !positive_position,
                    positive,
                    negative,
                );
            }
            for field in &function.named {
                record_occurrences(
                    db,
                    field.ty,
                    parameters,
                    !positive_position,
                    positive,
                    negative,
                );
            }
            if let Some(rest) = &function.variadic {
                record_occurrences(
                    db,
                    rest.element,
                    parameters,
                    !positive_position,
                    positive,
                    negative,
                );
            }
            record_occurrences(
                db,
                function.ret,
                parameters,
                positive_position,
                positive,
                negative,
            );
        }
        TyKind::Union(members) => {
            for &member in members {
                record_occurrences(
                    db,
                    member,
                    parameters,
                    positive_position,
                    positive,
                    negative,
                );
            }
        }
        // A nested nominal application's variance is not composed here:
        // conservative invariant (both directions).
        TyKind::Named(_, arguments) => {
            for &argument in arguments {
                record_occurrences(db, argument, parameters, true, positive, negative);
                record_occurrences(db, argument, parameters, false, positive, negative);
            }
        }
        _ => {}
    }
}

/// R's numeric promotion ladder — `logical` < `integer` < `double` <
/// `complex` — as ranks, so a lower rank is accepted where a higher one is
/// expected. `character` and `raw` are deliberately off the ladder: R reaches
/// `character` only through an explicit coercion, and accepting it implicitly
/// would hide the argument-order mistakes this check exists to catch.
fn numeric_ladder_rank(atomic: Atomic) -> Option<u8> {
    match atomic {
        Atomic::Logical => Some(0),
        Atomic::Integer => Some(1),
        Atomic::Double => Some(2),
        Atomic::Complex => Some(3),
        Atomic::Character | Atomic::Raw => None,
    }
}

/// `T | NULL` (exactly two members, one NULL) yields `T`.
fn nullable_single_member<'db>(db: &'db dyn Db, members: &[Ty<'db>]) -> Option<Ty<'db>> {
    if members.len() != 2 {
        return None;
    }
    let null_at = members
        .iter()
        .position(|member| matches!(member.kind(db), TyKind::Null))?;
    Some(members[1 - null_at])
}

/// Whether a type may inhabit a constrained variable.
///
/// Two admissions are not obvious from the constraint alone. A **class that
/// declares an arithmetic operator method** participates in arithmetic even
/// though it is not a numeric atom: the numeric constraint comes from a body
/// doing `a + b`, which for such a class is legal R, so rejecting it would
/// refuse `add_days <- function(d, n) d + n` for every date, matrix and
/// units-style class in the language. The result may be imprecise (the variable
/// takes the class), never a false rejection.
///
/// A **rigid binder** is admitted when its own declared bound is at least as
/// strong. `<T: numeric> fn(x: T) -> T` promises every instantiation of `T` is
/// numeric, so `T` satisfies a numeric constraint even though it is not a
/// numeric atom either — and a self-recursive call is where that matters, since
/// it unifies the rigid `T` with the fresh constrained variable the recursive
/// instantiation produced.
fn constraint_rejects<'db>(
    db: &'db dyn Db,
    table: &InferenceTable<'db>,
    constraint: Constraint,
    ty: Ty<'db>,
) -> Option<UnifyError<'db>> {
    use crate::types::Atomic;
    let arithmetic_classes = table.arithmetic_classes;
    let declares_arithmetic = |name: &Name<'db>| {
        arithmetic_classes.is_some_and(|classes| classes.contains(name.text(db)))
    };
    // A binder's declared bound implies the required one when joining the two
    // does not strengthen it — the lattice order, so nothing here has to
    // enumerate the pairs a second time.
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
    use crate::types::{Atomic, scalar};

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
        // A character scalar now violates the joined constraint.
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
        // The pre-probe variable is unbound again.
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
        let null = crate::types::null(&db);
        let a = union_of(&db, [int, chr]);
        let b = union_of(&db, [chr, int]);
        assert!(table.unify(&db, a, b).is_ok());

        // `T | NULL` vs `integer | NULL` binds T to integer.
        let var = table.fresh_ty(&db, Constraint::Unconstrained);
        let left = union_of(&db, [var, null]);
        let right = union_of(&db, [int, null]);
        table
            .unify(&db, left, right)
            .expect("nullable member-wise case");
        assert_eq!(table.resolve(&db, var), int);

        // Distinct non-nullable unions refuse.
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
        // Directional widening: integer fits double, never the reverse.
        assert!(table.compatible(&db, int, dbl));
        assert!(!table.compatible(&db, dbl, int));
        // A scalar coerces into a vector position, with widening inside.
        let dbl_vec = Ty::new(&db, TyKind::Vector(dbl));
        assert!(table.compatible(&db, int, dbl_vec));
        assert!(!table.compatible(&db, chr, dbl_vec));
        // Unification never widens.
        assert!(table.unify(&db, int, dbl).is_err());
    }

    #[test]
    fn compatibility_binds_generic_elements_and_prefers_concrete_members() {
        let db = RootDatabase::default();
        let mut table = InferenceTable::default();
        let int = scalar(&db, Atomic::Integer);
        let null = crate::types::null(&db);
        // `sort(c(1L))`-shape: a scalar into `T[]` binds `T := integer`.
        let element = table.fresh_ty(&db, Constraint::Unconstrained);
        let generic_vector = Ty::new(&db, TyKind::Vector(element));
        assert!(table.compatible(&db, int, generic_vector));
        assert_eq!(table.resolve(&db, element), int);
        // `NULL` into an instantiated `T | NULL` must match the concrete
        // `NULL` member and bind nothing, leaving `T` for a later argument.
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
        // A failing check must leak no bindings.
        let var = table.fresh_ty(&db, Constraint::Unconstrained);
        let tuple_with_var = Ty::new(&db, TyKind::Tuple(vec![var, chr]));
        let tuple_expected = Ty::new(&db, TyKind::Tuple(vec![int, int]));
        assert!(!table.compatible(&db, tuple_with_var, tuple_expected));
        assert!(matches!(table.resolve(&db, var).kind(&db), TyKind::Var(_)));
        // A lambda carrying named formals fits a positional interface: the
        // unnamed expected parameter pairs positionally (contravariant).
        let named_lambda = Ty::new(
            &db,
            TyKind::Function(FunctionType {
                positional: Vec::new(),
                named: vec![crate::types::Parameter {
                    name: crate::types::Name::new(&db, "x".to_owned()),
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
