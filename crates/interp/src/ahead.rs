//! Compiling a script's definitions before the script runs (`ry run --aot`).
//!
//! Before the first top-level expression runs, [`prepare`] lowers every expression of the script
//! and makes the closure of each definition written directly at the top level
//! (`name <- function(...) ...`). The optimizing tier then compiles those closures
//! ([`crate::bytecode::Tier::ahead`]), so the calls the script makes find code instead of
//! compiling at their first call. When the run reaches a definition, [`take`] gives the
//! `function` expression the closure made for it. The closure the script binds is therefore the
//! very object the code was compiled for, and nothing has to find the code again.
//!
//! Nothing of the script runs early. Making a closure has no effect a program can observe, and
//! neither has lowering an expression, with one exception: lowering signals a warning for a
//! literal such as `1.5L`, which a handler or `options(warn = 2)` set earlier in the script
//! must see when the expression's turn comes. An expression whose lowering warns or fails is
//! therefore left out here and lowered when the run reaches it, as without `--aot`.
//!
//! A file the script sources is compiled the same way when `source()` has parsed it
//! ([`parsed_file`]), before `source()` evaluates any of it.
//!
//! An installed typed package brings its own code, compiled at install into its typed code file.
//! When a namespace loads whose package has such a file only for another build of ry,
//! [`namespace_loaded`] compiles its typed functions as the namespace loads and says to reinstall
//! the package.

use {
    crate::{
        bytecode, interp,
        lower::{Script, lower_script_expression},
        unwind::R,
        values::{self, global_env, nil, symbols},
    },
    heap::{
        Marker,
        object::{Sexp, SexpType},
    },
    rustc_hash::FxHashMap,
    std::sync::OnceLock,
};

/// What a session compiling ahead of time holds between [`prepare`] and the end of the script.
#[derive(Default)]
pub struct State {
    /// Whether the session compiles a script's definitions before it runs the script.
    pub enabled: bool,
    /// The script's expressions as [`prepare`] lowered them, in order, each until the run takes
    /// it. `None` is one the run lowers itself.
    lowered: Vec<Option<Sexp>>,
    /// Whether [`prepare`] is lowering, so that a warning lowering would signal is held back.
    lowering: bool,
    /// Whether the expression being lowered asked to signal a warning.
    warned: bool,
    /// The closures made ahead whose definitions the run has not reached, by what identifies a
    /// definition: its formals, or its body when it has none.
    waiting: FxHashMap<Sexp, Sexp>,
    /// The packages whose namespaces [`namespace_loaded`] has looked at.
    namespaces: Vec<String>,
}

impl State {
    pub unsafe fn trace(&self, marker: &mut Marker<'_>) {
        for &expression in self.lowered.iter().flatten() {
            marker.mark(expression);
        }
        for &closure in self.waiting.values() {
            marker.mark(closure);
        }
    }
}

/// What a script defines at its top level, as the optimizing tier sees it before the script runs.
pub struct Program<'a> {
    /// The definitions in the order the script runs them.
    pub definitions: &'a [Definition],
}

/// A closure made ahead for a definition written at the top level of a script.
#[derive(Clone, Copy)]
pub struct Definition {
    /// The name the script binds the closure to.
    pub symbol: Sexp,
    pub closure: Sexp,
}

/// The expressions of a script lowered by [`prepare`].
pub struct Prepared {
    /// Which of them were lowered with source references, one bit per expression. An
    /// expression that runs with `options(keep.source)` set otherwise is lowered again, as a
    /// console would lower it.
    keep_source: Vec<u64>,
}

/// Lowers the expressions of `script`, makes the closures of its top-level definitions and has
/// the optimizing tier compile them.
pub unsafe fn prepare(script: &Script<'_>, keep_source: bool) -> R<Prepared> {
    // The session's state keeps what this makes alive, so the handles made on the way go.
    let mark = crate::heap().handle_mark();
    let prepared = prepare_script(script, keep_source);
    crate::heap().release_handles(mark);
    prepared
}

unsafe fn prepare_script(script: &Script<'_>, keep_source: bool) -> R<Prepared> {
    finish();
    let mut keep_source = keep_source;
    let mut kept = vec![0u64; script.expressions.len().div_ceil(64)];
    for (index, node) in script.expressions.iter().enumerate() {
        let state = &mut interp().ahead;
        state.lowering = true;
        state.warned = false;
        let lowered = lower_script_expression(script, node, keep_source);
        if keep_source {
            kept[index / 64] |= 1 << (index % 64);
        }
        let state = &mut interp().ahead;
        state.lowering = false;
        // The error is the expression's to report when it runs, so lowering it again finds it.
        let lowered = lowered.ok().filter(|_| !state.warned);
        state.lowered.push(lowered);
        if let Some(set) = lowered.and_then(|expression| sets_keep_source(expression)) {
            keep_source = set;
        }
    }
    let mut definitions = Vec::new();
    for index in 0..interp().ahead.lowered.len() {
        let Some(expression) = interp().ahead.lowered[index] else {
            continue;
        };
        if let Some(definition) = definition(expression, false)? {
            definitions.push(definition);
        }
    }
    compile(&definitions);
    Ok(Prepared { keep_source: kept })
}

/// Compiles the typed functions a file that `source()` parsed defines at its top level, before
/// `source()` evaluates the file, so that the calls the script makes after it find code. Called
/// with every file `parse()` reads whole in the frame `env` while the session compiles ahead.
/// Only a definition evaluated in the global environment binds the closure made here, so the
/// file compiles only when `env` is a frame that will evaluate it there: one whose `envir`,
/// read without forcing anything, is the global environment, as in `source()` and
/// `sys.source()`. A definition evaluated elsewhere makes its own closure, as without `--aot`.
pub unsafe fn parsed_file(expressions: Sexp, env: Sexp) -> R<()> {
    if !interp().ahead.enabled || interp().ahead.lowering || env == global_env() {
        return Ok(());
    }
    let Some(mut envir) = crate::eval::peek_variable(values::install("envir")?, env) else {
        return Ok(());
    };
    if envir.kind() == SexpType::Promise {
        envir = envir.car();
    }
    if envir != global_env() {
        return Ok(());
    }
    let heap = crate::heap();
    let mark = heap.handle_mark();
    let mut definitions = Vec::new();
    let mut result = Ok(());
    for index in 0..values::length(expressions) {
        match definition(expressions.element(index), true) {
            Ok(Some(definition)) => definitions.push(definition),
            Ok(None) => {}
            Err(error) => {
                result = Err(error);
                break;
            }
        }
    }
    if result.is_ok() {
        compile(&definitions);
    }
    heap.release_handles(mark);
    result
}

/// The closure made ahead for `expression` when it is a definition `name <- function(...) ...`,
/// which [`take`] gives the definition when the run reaches it. With `typed_only`, a definition
/// of an untyped function gets none, because nothing would compile for it.
unsafe fn definition(expression: Sexp, typed_only: bool) -> R<Option<Definition>> {
    let Some((symbol, value)) = assignment(expression)? else {
        return Ok(None);
    };
    if value.kind() != SexpType::Language || value.car() != symbols().function {
        return Ok(None);
    }
    let (formals, body, srcref) = (
        value.cdr().car(),
        value.cdr().cdr().car(),
        value.cdr().cdr().cdr().car(),
    );
    let Some(key) = definition_key(formals, body) else {
        return Ok(None);
    };
    let closure = values::triple(SexpType::Closure, formals, body, global_env())?;
    crate::heap().root(closure);
    if typed_only && !crate::annotations::is_typed(closure) {
        return Ok(None);
    }
    if srcref != nil() {
        values::set_attr(closure, symbols().srcref, srcref)?;
    }
    interp().ahead.waiting.insert(key, closure);
    Ok(Some(Definition { symbol, closure }))
}

/// Has the optimizing tier compile the typed closures among `definitions`.
unsafe fn compile(definitions: &[Definition]) {
    if let Some(tier) = bytecode::tier() {
        (tier.ahead)(&Program { definitions });
    }
    if discards_closures() {
        interp().ahead.waiting.clear();
    }
}

/// Compiles the typed functions of `namespace`, which `loadNamespace` just returned, when the
/// session compiles ahead and the package has a typed code file for another build of ry but none
/// for this one. Without this, those functions would compile at their first calls while the
/// program runs, which `--aot` asks to avoid. A package with no typed code file at all is either
/// untyped or was installed without one on purpose (`--ry-no-typed-code`), and telling the two
/// apart would read every function of every package the script loads.
///
/// Compiling ahead never changes whether a namespace loads: a binding this cannot read is left to
/// compile at its first call, as it would without `--aot`.
pub unsafe fn namespace_loaded(namespace: Sexp) {
    if !interp().ahead.enabled || namespace.kind() != SexpType::Environment {
        return;
    }
    let Some(package) = crate::envir::namespace_name(namespace) else {
        return;
    };
    if interp().ahead.namespaces.contains(&package) {
        return;
    }
    interp().ahead.namespaces.push(package.clone());
    let build = interp().code_image.build;
    let Some(folder) = namespace_folder(namespace) else {
        return;
    };
    let folder = folder.join("R");
    let database = folder.join(format!("{package}.rdb"));
    if build.is_empty() || rcmd::images::typed_code(&database, build).is_file() {
        return;
    }
    let prefix = format!("{package}.");
    let foreign = std::fs::read_dir(&folder).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(".ryt")
        })
    });
    if !foreign {
        return;
    }
    let heap = crate::heap();
    let mark = heap.handle_mark();
    let definitions = typed_closures(namespace);
    if let Some(tier) = bytecode::tier() {
        (tier.ahead)(&Program { definitions: &definitions });
        crate::write_console(
            format!(
                "ry: another build of ry installed {package}, so its typed functions compiled as \
                 it loaded; reinstall it to compile them at install\n"
            )
            .as_bytes(),
            true,
        );
    }
    heap.release_handles(mark);
}

/// The folder a namespace's package was loaded from, its `.__NAMESPACE__.$path`.
unsafe fn namespace_folder(namespace: Sexp) -> Option<std::path::PathBuf> {
    let info = crate::eval::find_var_in_frame(symbols().dot_namespace, namespace).ok()?;
    if info.kind() != SexpType::Environment {
        return None;
    }
    let path = crate::eval::find_var_in_frame(values::install("path").ok()?, info).ok()?;
    values::first_string(path).map(|path| std::path::PathBuf::from(&*path))
}

/// The typed closures `namespace` binds, each forced from the package's database. Forcing a
/// lazy-load promise reads the database and runs no code of the package; one that cannot be read
/// is left for the run to force.
unsafe fn typed_closures(namespace: Sexp) -> Vec<Definition> {
    let heap = crate::heap();
    let mut definitions = Vec::new();
    for symbol in heap.frame_symbols(namespace) {
        let Some(cell) = heap.frame_cell(namespace, symbol) else {
            continue;
        };
        let mut value = cell.cdr();
        if value.kind() == SexpType::Promise {
            if !crate::methods_core::is_lazy_load_promise(value).unwrap_or(false) {
                continue;
            }
            let Ok(forced) = crate::eval::force_promise(value) else {
                continue;
            };
            value = forced;
        }
        if value.kind() == SexpType::Closure && crate::annotations::is_typed(value) {
            definitions.push(Definition {
                symbol,
                closure: value,
            });
        }
    }
    definitions
}

/// Whether `RY_AHEAD_MISS` asks that no definition gets the closure made ahead for it. Every
/// definition then makes a new closure, as one that runs twice does, so all the code compiled
/// ahead was compiled for functions the script never binds. The run must still give what it
/// gives without compiling ahead, which is what the tests of that path use this for.
fn discards_closures() -> bool {
    static DISCARDS: OnceLock<bool> = OnceLock::new();
    *DISCARDS.get_or_init(|| std::env::var_os("RY_AHEAD_MISS").is_some())
}

/// The lowered expression at `index` of the prepared script, when [`prepare`] lowered it the
/// way the run would lower it now. The session holds the expression no longer: it is rooted in
/// the caller's handle scope, which lasts for the one time the caller runs it.
pub fn lowered(prepared: &Prepared, index: usize, keep_source: bool) -> Option<Sexp> {
    let expression = interp().ahead.lowered.get_mut(index)?.take()?;
    crate::heap().root(expression);
    let kept = prepared.keep_source.get(index / 64)? >> (index % 64) & 1 == 1;
    (kept == keep_source).then_some(expression)
}

/// The value a top-level `options(keep.source = TRUE)` or `FALSE` sets, so [`prepare`] lowers
/// the expressions after it as the run will. Any other way of setting the option is not
/// foreseen, and the expressions it affects are lowered again when they run.
unsafe fn sets_keep_source(expression: Sexp) -> Option<bool> {
    if expression.kind() != SexpType::Language
        || expression.car().kind() != SexpType::Symbol
        || expression.car().car().bytes() != b"options"
    {
        return None;
    }
    values::pairlist_items(expression.cdr())
        .iter()
        .rev()
        .find(|(tag, _)| tag.kind() == SexpType::Symbol && tag.car().bytes() == b"keep.source")
        .and_then(|&(_, value)| {
            (value.kind() == SexpType::Logical && values::length(value) == 1)
                .then(|| values::ints(value)[0])
                .filter(|&flag| flag != heap::object::NA_LOGICAL)
                .map(|flag| flag != 0)
        })
}

/// Whether a warning lowering is about to signal is held back because [`prepare`] is lowering
/// the expression ahead of its turn. The expression is then lowered again when the run reaches
/// it, which signals the warning.
pub fn holds_back_warning() -> bool {
    let state = &mut interp().ahead;
    state.warned |= state.lowering;
    state.lowering
}

/// Forgets the prepared script, which the session is done with, and the closures made for
/// definitions that never ran.
pub fn finish() {
    let state = &mut interp().ahead;
    state.lowered.clear();
    state.waiting.clear();
}

/// The closure made ahead for the definition with these formals and this body, when the
/// definition is evaluated in the global environment for the first time. A definition gives its
/// closure once: evaluated again, it makes a new closure, as R requires.
#[inline]
pub unsafe fn take(formals: Sexp, body: Sexp, env: Sexp) -> Option<Sexp> {
    let state = &mut interp().ahead;
    if state.waiting.is_empty() || env != global_env() {
        return None;
    }
    state.waiting.remove(&definition_key(formals, body)?)
}

/// The symbol a top-level expression assigns to and the expression of the value, for the plain
/// assignments `name <- value`, `name = value` and `name <<- value`, which all bind in the
/// global environment at the top level.
unsafe fn assignment(expression: Sexp) -> R<Option<(Sexp, Sexp)>> {
    if expression.kind() != SexpType::Language {
        return Ok(None);
    }
    let head = expression.car();
    let symbols = symbols();
    if head != symbols.assign && head != symbols.assign_eq && head != symbols.assign2 {
        return Ok(None);
    }
    let arguments = expression.cdr();
    if arguments.kind() != SexpType::Pairlist || arguments.cdr().kind() != SexpType::Pairlist {
        return Ok(None);
    }
    let target = match arguments.car() {
        target if target.kind() == SexpType::Symbol => target,
        // `"name" <- value` assigns to the variable of that name.
        target if target.kind() == SexpType::String && values::length(target) == 1 => {
            let Some(name) = values::string_at(target, 0) else {
                return Ok(None);
            };
            values::install(&name)?
        }
        _ => return Ok(None),
    };
    Ok(Some((target, arguments.cdr().car())))
}

/// What identifies a definition among every closure made from it: the formals, which lowering
/// builds once per `function` expression, or the body of a function without formals. A body
/// that is a symbol or a constant is shared between definitions and identifies none.
unsafe fn definition_key(formals: Sexp, body: Sexp) -> Option<Sexp> {
    if formals.kind() == SexpType::Pairlist {
        return Some(formals);
    }
    let body = bytecode::source_of_body(body);
    (body.kind() == SexpType::Language).then_some(body)
}
