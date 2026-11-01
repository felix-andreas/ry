//! The primitive and internal function table. A primitive is bound in the base environment, as
//! `.Primitive("name")` finds it; an internal hangs off its symbol, where `.Internal(name(...))`
//! finds it, as GNU R keeps it in `INTERNAL(sym)`. Builtins receive evaluated arguments,
//! specials the unevaluated ones.
//!
//! Each module that implements builtins exports a `BUILTINS` table; [`install_all`] registers
//! them, and the host adds its own with [`crate::register_builtins`].

use crate::{
    interp,
    unwind::{R, error, error_in},
    values::{self, nil},
};
use heap::object::{Sexp, SexpType};
use syntax::native_values::ACCEPTS_NATIVE;

/// How a builtin is called.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A primitive whose arguments are evaluated first.
    Builtin,
    /// A primitive that receives its arguments unevaluated.
    Special,
    /// An internal (reached through `.Internal`) whose arguments are evaluated first.
    Internal,
    /// An internal that receives its arguments unevaluated.
    InternalSpecial,
}

/// Whether a call leaves its value visible, as GNU R's `PRINT` column says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    On,
    Off,
    /// The builtin decides, by setting `interp().visible` itself.
    Keep,
}

/// Which dispatch a builtin performs before running, if its arguments are objects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Generic {
    None,
    /// S3 and S4 dispatch on the first argument (`length`, `[`, `as.character`, ...).
    Internal,
    Ops,
    /// `%*%`, which dispatches on either operand through the `matrixOps` group.
    MatrixOps,
    Math,
    Summary,
    Complex,
}

/// One argument: its tag (`R_NilValue` when unnamed) and its value, or for a special its
/// unevaluated expression.
#[derive(Clone, Copy, Debug)]
pub struct Arg {
    pub tag: Sexp,
    pub value: Sexp,
}

/// What a builtin receives.
pub struct Call<'a> {
    pub call: Sexp,
    /// The primitive object being called.
    pub op: Sexp,
    pub args: &'a [Arg],
    pub env: Sexp,
}

pub type BuiltinFn = unsafe fn(&Call<'_>) -> R<Sexp>;

pub struct BuiltinDef {
    pub name: &'static str,
    pub kind: Kind,
    /// The number of arguments an internal takes, checked before the call; -1 for any.
    pub arity: i32,
    pub visibility: Visibility,
    pub generic: Generic,
    /// Empty arguments reach the builtin as `R_MissingArg` instead of being an error, as `[`
    /// needs for `x[, 1]`.
    pub keep_missing: bool,
    /// Whether a ry record, tuple or tag may reach the builtin as a top-level argument; set at
    /// registration from [`ACCEPTS_NATIVE`].
    pub accepts_native: bool,
    pub function: BuiltinFn,
    pub object: Option<Sexp>,
}

/// A table entry. The constructors below keep the tables short.
pub const fn builtin(name: &'static str, arity: i32, function: BuiltinFn) -> BuiltinDef {
    BuiltinDef {
        name,
        kind: Kind::Builtin,
        arity,
        visibility: Visibility::On,
        generic: Generic::None,
        keep_missing: false,
        accepts_native: false,
        function,
        object: None,
    }
}

pub const fn special(name: &'static str, arity: i32, function: BuiltinFn) -> BuiltinDef {
    BuiltinDef {
        kind: Kind::Special,
        ..builtin(name, arity, function)
    }
}

pub const fn internal(name: &'static str, arity: i32, function: BuiltinFn) -> BuiltinDef {
    BuiltinDef {
        kind: Kind::Internal,
        ..builtin(name, arity, function)
    }
}

pub const fn internal_special(name: &'static str, arity: i32, function: BuiltinFn) -> BuiltinDef {
    BuiltinDef {
        kind: Kind::InternalSpecial,
        ..builtin(name, arity, function)
    }
}

impl BuiltinDef {
    pub const fn invisible(self) -> BuiltinDef {
        BuiltinDef {
            visibility: Visibility::Off,
            ..self
        }
    }

    pub const fn keep_visibility(self) -> BuiltinDef {
        BuiltinDef {
            visibility: Visibility::Keep,
            ..self
        }
    }

    pub const fn generic(self, generic: Generic) -> BuiltinDef {
        BuiltinDef { generic, ..self }
    }

    pub const fn keep_missing(self) -> BuiltinDef {
        BuiltinDef {
            keep_missing: true,
            ..self
        }
    }

    fn is_internal(&self) -> bool {
        matches!(self.kind, Kind::Internal | Kind::InternalSpecial)
    }

    fn copy(&self) -> BuiltinDef {
        BuiltinDef {
            name: self.name,
            kind: self.kind,
            arity: self.arity,
            visibility: self.visibility,
            generic: self.generic,
            keep_missing: self.keep_missing,
            accepts_native: self.accepts_native,
            function: self.function,
            object: self.object,
        }
    }
}

pub(crate) unsafe fn install_all() {
    let mut seen = rustc_hash::FxHashSet::default();
    for table in [
        crate::eval::BUILTINS,
        crate::envir::BUILTINS,
        crate::conditions::BUILTINS,
        crate::dispatch::BUILTINS,
        crate::arith::BUILTINS,
        crate::linalg::BUILTINS,
        crate::coerce::BUILTINS,
        crate::subset::BUILTINS,
        crate::strings::BUILTINS,
        crate::print::BUILTINS,
        crate::format::BUILTINS,
        crate::deparse::BUILTINS,
        crate::sort::BUILTINS,
        crate::random::BUILTINS,
        crate::distributions::BUILTINS,
        crate::serialize::BUILTINS,
        crate::connections::BUILTINS,
        crate::system::BUILTINS,
        crate::lower::BUILTINS,
        crate::bytecode::BUILTINS,
        crate::session::BUILTINS,
        crate::native::BUILTINS,
        crate::utils::BUILTINS,
        crate::records::BUILTINS,
    ] {
        // `register` lets a later definition replace an earlier one, which the host relies on;
        // within the interpreter's own tables a repeated name is a stale copy shadowing the real
        // one, so it is a bug to catch at start-up.
        for definition in table {
            assert!(
                seen.insert((definition.name, definition.is_internal())),
                "`{}` is defined twice in the interpreter's builtin tables",
                definition.name
            );
        }
        register(table);
    }
    for name in ACCEPTS_NATIVE {
        assert!(
            seen.contains(&(*name, false)) || seen.contains(&(*name, true)),
            "`{name}` is listed as accepting ry values but is not a builtin"
        );
    }
}

pub(crate) unsafe fn register(definitions: &[BuiltinDef]) {
    let heap = crate::heap();
    heap.inhibit_collection();
    for definition in definitions {
        let key = index_key(definition.name, definition.is_internal());
        let state = interp();
        if let Some(&index) = state.builtin_index.get(key) {
            let existing = &mut state.builtins[index as usize];
            let object = existing.object;
            *existing = definition.copy();
            existing.accepts_native = ACCEPTS_NATIVE.contains(&definition.name);
            existing.object = object;
            continue;
        }
        let index = state.builtins.len() as u32;
        let object_kind = match definition.kind {
            Kind::Builtin | Kind::Internal => SexpType::Builtin,
            Kind::Special | Kind::InternalSpecial => SexpType::Special,
        };
        let object = heap
            .alloc_primitive(object_kind, index)
            .unwrap_or_else(|failure| panic!("cannot create `{}`: {failure}", definition.name));
        let symbol = heap
            .install(definition.name.as_bytes())
            .unwrap_or_else(|failure| panic!("cannot install `{}`: {failure}", definition.name));
        if definition.is_internal() {
            heap.set_tag(symbol, object);
        } else {
            let base = heap.known().base_env;
            heap.define(base, symbol, object)
                .unwrap_or_else(|failure| panic!("cannot bind `{}`: {failure}", definition.name));
        }
        let mut entry = definition.copy();
        entry.accepts_native = ACCEPTS_NATIVE.contains(&definition.name);
        entry.object = Some(object);
        state.builtins.push(entry);
        state.builtin_index.insert(key, index);
    }
    heap.allow_collection();
}

/// Internals and primitives share one index map, so an internal's key carries a marker that no
/// R name contains.
fn index_key(name: &'static str, internal: bool) -> &'static str {
    if internal {
        // Leaked once per internal name at registration, which is bounded by the table size.
        Box::leak(format!("\u{1}{name}").into_boxed_str())
    } else {
        name
    }
}

/// The internal function `.Internal(name(...))` calls, if any.
pub unsafe fn internal_function(symbol: Sexp) -> Option<Sexp> {
    let object = symbol.tag();
    matches!(object.kind(), SexpType::Builtin | SexpType::Special).then_some(object)
}

/// The primitive named `name`, as `.Primitive` returns it.
pub unsafe fn primitive(name: &str) -> Option<Sexp> {
    let interp = interp();
    let &index = interp.builtin_index.get(name)?;
    interp.builtins[index as usize].object
}

impl Call<'_> {
    pub fn len(&self) -> usize {
        self.args.len()
    }

    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
    }

    /// Argument `index`'s value, or `NULL` when there are fewer arguments.
    pub fn arg(&self, index: usize) -> Sexp {
        self.args.get(index).map_or_else(nil, |arg| arg.value)
    }

    pub fn name(&self) -> &'static str {
        // SAFETY: `op` is a live primitive object.
        unsafe { values::primitive_name(self.op) }
    }

    /// An error reported against this call.
    pub fn error(&self, message: impl Into<String>) -> crate::unwind::Unwind {
        error_in(self.call, message)
    }

    /// Checks the argument count against `count`, with GNU R's message.
    pub fn check_arity(&self, count: usize) -> R<()> {
        if self.args.len() != count {
            return Err(self.error(format!(
                "{} argument{} passed to '{}' which requires {count}",
                self.args.len(),
                if self.args.len() == 1 { "" } else { "s" },
                self.name()
            )));
        }
        Ok(())
    }

    /// Matches the arguments to `formals` the way a closure would (exact, partial, then
    /// positional), for the primitives that take named arguments. A `...` formal collects the
    /// rest, which are returned separately.
    pub unsafe fn match_args(&self, formals: &[&str]) -> R<(Vec<Option<Sexp>>, Vec<Arg>)> {
        match_builtin_args(self.call, self.args, formals)
    }
}

/// Argument matching for builtins: the same three passes as closures, over `Arg`s.
pub unsafe fn match_builtin_args(
    call: Sexp,
    args: &[Arg],
    formals: &[&str],
) -> R<(Vec<Option<Sexp>>, Vec<Arg>)> {
    let mut matched: Vec<Option<Sexp>> = vec![None; formals.len()];
    let mut used = vec![false; args.len()];
    let dots = formals.iter().position(|formal| *formal == "...");
    let names: Vec<Option<String>> = args
        .iter()
        .map(|arg| {
            (arg.tag.kind() == SexpType::Symbol)
                .then(|| values::symbol_name(arg.tag).into_owned())
                .filter(|name| !name.is_empty())
        })
        .collect();
    for (formal_index, formal) in formals.iter().enumerate() {
        if *formal == "..." {
            continue;
        }
        for (arg_index, name) in names.iter().enumerate() {
            if !used[arg_index] && name.as_deref() == Some(*formal) {
                if matched[formal_index].is_some() {
                    return Err(error_in(
                        call,
                        format!(
                            "formal argument \"{formal}\" matched by multiple actual arguments"
                        ),
                    ));
                }
                matched[formal_index] = Some(args[arg_index].value);
                used[arg_index] = true;
            }
        }
    }
    for (formal_index, formal) in formals.iter().enumerate() {
        if *formal == "..." {
            break;
        }
        if matched[formal_index].is_some() {
            continue;
        }
        for (arg_index, name) in names.iter().enumerate() {
            if !used[arg_index]
                && let Some(name) = name
                && formal.starts_with(name.as_str())
            {
                matched[formal_index] = Some(args[arg_index].value);
                used[arg_index] = true;
                break;
            }
        }
    }
    let mut rest = Vec::new();
    let mut formal_index = 0;
    for (arg_index, arg) in args.iter().enumerate() {
        if used[arg_index] {
            continue;
        }
        if names[arg_index].is_some() {
            if dots.is_some() {
                rest.push(*arg);
                continue;
            }
            return Err(error_in(
                call,
                format!(
                    "unused argument ({} = {})",
                    names[arg_index].as_deref().unwrap_or(""),
                    crate::deparse::deparse_one_line(arg.value)
                ),
            ));
        }
        while formal_index < formals.len()
            && (matched[formal_index].is_some() || formals[formal_index] == "...")
        {
            if formals[formal_index] == "..." {
                break;
            }
            formal_index += 1;
        }
        if formal_index < formals.len() && formals[formal_index] != "..." {
            matched[formal_index] = Some(arg.value);
            formal_index += 1;
        } else if dots.is_some() {
            rest.push(*arg);
        } else {
            return Err(error_in(
                call,
                format!(
                    "unused argument ({})",
                    crate::deparse::deparse_one_line(arg.value)
                ),
            ));
        }
    }
    Ok((matched, rest))
}

/// A missing argument has the value `R_MissingArg`; this reads one as `None`.
pub unsafe fn present(value: Option<Sexp>) -> Option<Sexp> {
    value.filter(|&value| value != values::missing_arg())
}

pub unsafe fn require(value: Option<Sexp>, name: &str) -> R<Sexp> {
    present(value).ok_or_else(|| error(format!("argument \"{name}\" is missing, with no default")))
}
