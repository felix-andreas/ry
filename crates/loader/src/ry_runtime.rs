//! The loader's backend for ry's own runtime. The interpreter runs the same R code as GNU R
//! ([`RUNTIME_RY`]), but the plan never leaves the process and no source text reaches R: a
//! lazy definition is a promise over the analysis's syntax tree for its file, and forcing it
//! lowers the definition's node with the interpreter's lowering, so the only parse is the
//! analysis's own.
//!
//! The internals below are what `ry.R` calls. Planning runs the analysis on its own thread,
//! because the analysis recurses as deeply as the code nests.

#![allow(unsafe_op_in_unsafe_fn)]

use {
    crate::{
        Code, Loader, NamespaceConditions, Source, find_package_root, parse_namespace_conditions,
        r::{RUNTIME_RY, RValue, error_value, failure_value, plan_value},
    },
    heap::object::{Sexp, SexpType},
    interp::{
        builtins::{BuiltinDef, Call, internal, internal_function},
        eval::{define, eval, make_promise},
        lower::{file_srcfile, lower_node, lower_node_at_line, parse_text},
        methods::{
            DeferredDefinitions, defer_definitions, drop_deferred_definitions, force_deferred_unit,
        },
        scope,
        unwind::{R, error},
        values::{self, install, length, nil},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    semantics::ANALYSIS_STACK_SIZE,
    std::{
        cell::RefCell,
        ffi::{CString, c_char, c_int, c_void},
        path::{Path, PathBuf},
        sync::{
            LazyLock, Mutex, OnceLock, PoisonError,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    },
    syntax::{SyntaxNode, TextRange, TextSize},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryLoaderPlan", 3, do_plan),
    internal("ryLoaderCommit", 1, do_commit).invisible(),
    internal("ryLoaderBind", 2, do_bind).invisible(),
    internal("ryLoaderRun", 3, do_run).invisible(),
    internal("ryLoaderForce", 2, do_force),
    internal("ryLoaderAwaitVerdict", 0, do_await_verdict).invisible(),
    internal("ryLoaderDefer", 7, do_defer).invisible(),
    internal("ryLoaderForceUnit", 2, do_force_unit).invisible(),
    internal("ryLoaderDeferred", 1, do_deferred),
    internal("ryLoaderRunDeferred", 3, do_run_deferred).invisible(),
    internal("ryLoaderUndefer", 1, do_undefer).invisible(),
    internal("ryLoaderChanged", 0, do_changed),
    internal("ryLoaderCallers", 2, do_callers),
    internal("ryLoaderWaitEdits", 2, do_wait_edits),
    internal("ryLoaderCompile", 2, do_compile).invisible(),
];

/// Evaluates the loader's R side into a fresh environment and attaches it as `tools:ry`, which
/// gives the session `load_all()`.
///
/// # Safety
/// Called on the interpreter's thread after base R has loaded.
pub unsafe fn attach() -> R<()> {
    scope(|| {
        let runtime = values::new_env(values::base_env())?;
        let expressions = parse_text(RUNTIME_RY, "<ry loader>")?;
        for index in 0..length(expressions) {
            eval(expressions.element(index), runtime)?;
        }
        let setup = values::new_env(values::global_env())?;
        define(install("runtime")?, runtime, setup)?;
        let expressions = parse_text(ATTACH, "<ry loader>")?;
        for index in 0..length(expressions) {
            eval(expressions.element(index), setup)?;
        }
        Ok(())
    })
}

/// Whether a loaded package's sources changed on disk since it was loaded, or a package whose
/// load failed changed since then, which the console asks before each command to decide
/// whether to reload. A change is reported once, and `.ry_autoreload()` reads the roots that
/// changed with `.Internal(ryLoaderChanged())`.
pub fn changed() -> bool {
    let mut session = session();
    session.changed = session.loader.changed_roots();
    !session.changed.is_empty()
}

/// `.Internal(ryLoaderChanged())`: the roots [`changed`] last reported, which it forgets.
unsafe fn do_changed(_call: &Call<'_>) -> R<Sexp> {
    let roots: Vec<String> = std::mem::take(&mut session().changed)
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    values::string_vector(&roots)
}

const ATTACH: &str = r#"
runtime$.ry_packages <- new.env(parent = emptyenv())
runtime$.ry_timing <- new.env(parent = emptyenv())
runtime$.ry_timing$last <- 0
if ("tools:ry" %in% search()) detach("tools:ry", character.only = TRUE)
attach(runtime, name = "tools:ry", warn.conflicts = FALSE)
"#;

/// The process's loader. The interpreter runs on one thread, so the lock is never contended;
/// it exists because planning hands the loader to the analysis thread.
static SESSION: LazyLock<Mutex<Session>> = LazyLock::new(Mutex::default);

#[derive(Default)]
struct Session {
    loader: Loader,
    /// The plan being applied, per package root as R names it.
    pending: FxHashMap<String, Pending>,
    /// Statements a load held back until first use, per package root as R names it.
    deferred: FxHashMap<String, Deferred>,
    /// The roots whose sources changed, until the console's reload reads them.
    changed: Vec<PathBuf>,
}

struct Deferred {
    /// Each statement with its position in the plan's `run`.
    code: Vec<(usize, Code)>,
}

struct Pending {
    /// The root as the loader names it, which commits the plan.
    root: PathBuf,
    bind: Vec<Code>,
    run: Vec<Code>,
    /// The code of every file the plan's code comes from.
    sources: FxHashMap<String, Source>,
}

thread_local! {
    /// The source handle ([`source_handle`]) of each file whose code a load lowered, per package
    /// root as R names it and then per file: the pending plan's, and the held-back statements'.
    /// They live on the interpreter's thread, which owns every R object, and are preserved, so
    /// a srcfile built for one statement of a file serves the next.
    static HANDLES: RefCell<Handles> = RefCell::default();
}

#[derive(Default)]
struct Handles {
    pending: FxHashMap<String, FxHashMap<String, Sexp>>,
    deferred: FxHashMap<String, FxHashMap<String, Sexp>>,
}

/// What a source handle owns: one file's code, its absolute path, which its source references
/// name, and whether the load that made it keeps source references (`options(keep.source)`).
struct FileSource {
    source: Source,
    path: String,
    keep: bool,
}

/// Where a lazy definition or a statement is in its file: the byte range of its syntax node,
/// and the 1-based line and the column it starts at.
#[derive(Clone, Copy)]
struct Position {
    start: u32,
    end: u32,
    line: u32,
    column: u32,
}

impl Position {
    fn of(code: &Code) -> Position {
        Position {
            start: code.source_start,
            end: code.source_end,
            line: code.line,
            column: code.column,
        }
    }
}

/// Kept outside the session, so setting it never waits for a [`prepare`] that holds the
/// session.
static STRICT: AtomicBool = AtomicBool::new(false);

/// Whether the project is in strict mode, which the host reads from its `ry.toml`.
pub fn set_strict(strict: bool) {
    STRICT.store(strict, Ordering::Relaxed);
}

/// Asks the host whether the package may run: 0 lets it, and any other value is the status the
/// process exits with, after the host has reported why. The host may block until it knows.
pub type Verdict = extern "C" fn() -> c_int;

static VERDICT: OnceLock<Verdict> = OnceLock::new();

/// Lets the host check the package while it loads. Binding definitions and running the
/// statements the plan marks effect-free never wait; any other statement, the hooks, compiled
/// code and everything after the load wait for `verdict`, so code the host refuses never runs.
pub fn set_verdict(verdict: Verdict) {
    if VERDICT.set(verdict).is_err() {
        panic!("the host sets the package verdict once per session");
    }
}

/// Asks the host about the package at `root`, a NUL-terminated path. As the strict check of
/// [`set_check`], it runs strict mode's check before a load runs any of the package: 0 lets the
/// load run, 1 means strict mode refused the package, and any other value that the check failed,
/// after the host has reported why. It may block. As the strict query, it returns 1 when the
/// configuration that governs `root` turns strict mode on, and 0 otherwise.
pub type Check = extern "C" fn(*const c_char) -> c_int;

static CHECK: OnceLock<Check> = OnceLock::new();

static STRICT_CONFIG: OnceLock<Check> = OnceLock::new();

/// Whether a plan has been made in this session, whose first load the verdict covers.
static PLANNED: AtomicBool = AtomicBool::new(false);

/// Lets the host check every load of a strict package that changes something, before the
/// load runs any of it. The first load of a session with a [`set_verdict`] is the verdict's
/// to check instead, while its definitions bind. Whether a package is strict is the
/// `strict_config` query's answer for its own root, so a strict package that a non-strict
/// script loads is checked, as it is on GNU R.
pub fn set_check(strict_config: Check, check: Check) {
    if STRICT_CONFIG.set(strict_config).is_err() || CHECK.set(check).is_err() {
        panic!("the host sets the strict check once per session");
    }
}

/// Returns once the host lets the package run, and otherwise ends the process with the
/// host's status. A session without a verdict runs everything.
fn await_verdict() {
    let Some(verdict) = VERDICT.get() else {
        return;
    };
    let status = verdict();
    if status != 0 {
        use std::io::Write;
        drop(std::io::stdout().flush());
        std::process::exit(status);
    }
}

/// Starts analyzing the package at or above `path` on a thread of its own and returns at once,
/// so the analysis overlaps starting R and the load that follows finds it done. That load
/// waits for the session while this holds it.
pub fn prepare(path: PathBuf, strict: bool) {
    set_strict(strict);
    let spawned = thread::Builder::new()
        .name("ry-loader-prepare".to_owned())
        .stack_size(ANALYSIS_STACK_SIZE)
        .spawn(move || {
            let mut session = session();
            session.loader.set_strict(strict);
            session.loader.prepare(&path);
        });
    if let Err(error) = spawned {
        // Preparing only saves time: without the thread, the load derives everything itself.
        drop(error);
    }
}

fn session() -> std::sync::MutexGuard<'static, Session> {
    SESSION.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `.Internal(ryLoaderPlan(root, full, conditions))`: the plan as `runtime.R` reads it, or
/// `list(error =)`, or the `if` directives of `NAMESPACE` to evaluate. `conditions` is
/// encoded as [`parse_namespace_conditions`] reads it.
unsafe fn do_plan(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let full = values::require_flag(call.arg(1), "full")?;
    let conditions =
        parse_namespace_conditions(&values::require_string(call.arg(2), "conditions")?);
    let value = thread::scope(|threads| {
        thread::Builder::new()
            .name("ry-loader".to_owned())
            .stack_size(ANALYSIS_STACK_SIZE)
            .spawn_scoped(threads, || plan(&root, full, &conditions))
            .map(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
    });
    let value =
        value.map_err(|failure| error(format!("cannot start the loader's thread: {failure}")))?;
    release_handles(|handles| handles.pending.remove(&root));
    scope(|| r_value(&value))
}

fn plan(root: &str, full: bool, conditions: &NamespaceConditions) -> RValue {
    let Ok(path) = CString::new(root) else {
        return error_value(&format!("{root} has a NUL byte in its path"));
    };
    let strict = match STRICT_CONFIG.get() {
        Some(strict_config) => strict_config(path.as_ptr()) == 1,
        None => STRICT.load(Ordering::Relaxed),
    };
    let mut session = session();
    session.loader.set_strict(strict);
    let plan = match session.loader.plan(Path::new(root), full, conditions) {
        Ok(plan) => plan,
        Err(failure) => return failure_value(&failure),
    };
    let covered = VERDICT.get().is_some() && !PLANNED.swap(true, Ordering::Relaxed);
    if plan.strict
        && !plan.is_noop()
        && !covered
        && let Some(check) = CHECK.get()
    {
        let Ok(path) = CString::new(plan.root.to_string_lossy().into_owned()) else {
            return error_value(&format!("{} has a NUL byte in its path", plan.package));
        };
        let reason = match check(path.as_ptr()) {
            0 => None,
            1 => Some("strict mode refused it"),
            _ => Some("its strict check failed"),
        };
        if let Some(reason) = reason {
            return error_value(&format!(
                "{} was not loaded, because {reason}",
                plan.package
            ));
        }
    }
    let mut sources = FxHashMap::default();
    for code in plan.bind.iter().chain(&plan.run) {
        if !sources.contains_key(&code.path)
            && let Some(source) = session.loader.source(&plan.root, &code.path)
        {
            sources.insert(code.path.clone(), source);
        }
    }
    let value = plan_value(&plan, false);
    session.pending.insert(
        root.to_owned(),
        Pending {
            root: plan.root,
            bind: plan.bind,
            run: plan.run,
            sources,
        },
    );
    value
}

/// `.Internal(ryLoaderAwaitVerdict())`, which the loader calls once the effect-free part of a
/// load has run.
unsafe fn do_await_verdict(_call: &Call<'_>) -> R<Sexp> {
    await_verdict();
    Ok(nil())
}

unsafe fn do_commit(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let mut session = session();
    if let Some(pending) = session.pending.remove(&root) {
        session.loader.commit(&pending.root);
    }
    drop(session);
    release_handles(|handles| handles.pending.remove(&root));
    Ok(nil())
}

/// `.Internal(ryLoaderBind(root, ns))`: binds every definition of the pending plan as a promise
/// that calls `ryLoaderForce` on its file's source handle and its position there.
unsafe fn do_bind(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let namespace = call.arg(1);
    let bind = session()
        .pending
        .get(&root)
        .ok_or_else(|| error(format!("no load of {root} is pending")))?
        .bind
        .clone();
    let force = internal_function(install("ryLoaderForce")?)
        .ok_or_else(|| error("the loader's internals are not installed"))?;
    scope(|| {
        for code in &bind {
            let Some(name) = &code.name else { continue };
            let handle = pending_handle(&root, &code.path)?;
            scope(|| {
                let position = position_value(Position::of(code))?;
                let promise = make_promise(values::call(force, &[handle, position])?, namespace)?;
                define(install(name)?, promise, namespace)
            })?;
        }
        Ok(nil())
    })
}

/// `.Internal(ryLoaderRun(root, ns, i))`: evaluates the pending plan's `i`th statement.
unsafe fn do_run(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let namespace = call.arg(1);
    let index = values::as_integer_scalar(call.arg(2));
    let code = {
        let session = session();
        let pending = session
            .pending
            .get(&root)
            .ok_or_else(|| error(format!("no load of {root} is pending")))?;
        usize::try_from(index - 1)
            .ok()
            .and_then(|index| pending.run.get(index))
            .ok_or_else(|| error(format!("the plan has no statement {index}")))?
            .clone()
    };
    if !code.effect_free {
        await_verdict();
    }
    let expression = scope(|| {
        let handle = pending_handle(&root, &code.path)?;
        lower_handle_code(handle, Position::of(&code))
    })?;
    eval(expression, namespace)?;
    Ok(nil())
}

/// `.Internal(ryLoaderDefer(root, positions, classes, generics, primitives, lazy, force))`:
/// holds back the pending plan's statements at `positions` (1-based, in `run`) and registers
/// `force` to run as [`DeferredDefinitions`] describes for the other arguments.
unsafe fn do_defer(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let positions = call.arg(1);
    let names = |argument: Sexp| -> FxHashSet<Box<[u8]>> {
        (0..length(argument))
            .map(|index| Box::from(argument.element(index).bytes()))
            .collect()
    };
    let (classes, generics, primitives, lazy, force) = (
        names(call.arg(2)),
        names(call.arg(3)),
        names(call.arg(4)),
        names(call.arg(5)),
        call.arg(6),
    );
    if force.kind() != SexpType::Closure {
        return Err(error(
            "ryLoaderDefer needs a function to run the definitions",
        ));
    }
    if positions.kind() != SexpType::Integer {
        return Err(error("ryLoaderDefer needs integer positions"));
    }
    let mut session = session();
    let pending = session
        .pending
        .get(&root)
        .ok_or_else(|| error(format!("no load of {root} is pending")))?;
    let mut code = Vec::with_capacity(length(positions));
    for &position in values::ints(positions) {
        let found = usize::try_from(position - 1)
            .ok()
            .and_then(|position| Some((position, pending.run.get(position)?.clone())))
            .ok_or_else(|| error(format!("the plan has no statement {position}")))?;
        code.push(found);
    }
    session
        .deferred
        .insert(root.clone(), Deferred { code: code.clone() });
    drop(session);
    let mut handles = FxHashMap::default();
    let held = scope(|| {
        for (_, code) in &code {
            if !handles.contains_key(&code.path) {
                let handle = pending_handle(&root, &code.path)?;
                interp::heap().preserve(handle);
                handles.insert(code.path.clone(), handle);
            }
        }
        Ok(())
    });
    let previous = HANDLES.with_borrow_mut(|all| all.deferred.insert(root.clone(), handles));
    release(previous);
    held?;
    defer_definitions(DeferredDefinitions {
        key: root,
        classes,
        generics,
        primitives,
        lazy,
        first_pending: true,
        force,
    });
    Ok(nil())
}

/// `.Internal(ryLoaderForceUnit(root, generic))`: runs the held-back unit of the package at `root`
/// that `generic` names, or the first unit for `NULL`, unless it ran already.
unsafe fn do_force_unit(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let generic = call.arg(1);
    let generic = (generic != nil()).then(|| generic.element(0).bytes());
    force_deferred_unit(&root, generic)?;
    Ok(nil())
}

/// `.Internal(ryLoaderCallers(root, names))`: [`Loader::callers`] of the package at `root`.
unsafe fn do_callers(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let changed: Vec<String> = values::strings(call.arg(1)).into_iter().flatten().collect();
    let root = find_package_root(Path::new(&root)).map_err(|failure| error(failure.to_string()))?;
    let callers = session().loader.callers(&root, &changed);
    values::string_vector(&callers)
}

/// `.Internal(ryLoaderWaitEdits(root, timeout))`: the paths edited under the package, relative
/// to its root, after waiting up to `timeout` seconds for one; `NA` stands for lost
/// notifications.
unsafe fn do_wait_edits(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let timeout = values::as_real_scalar(call.arg(1));
    let root = find_package_root(Path::new(&root)).map_err(|failure| error(failure.to_string()))?;
    let timeout = Duration::try_from_secs_f64(timeout)
        .map_err(|_| error(format!("{timeout} is not a timeout in seconds")))?;
    let edits = session()
        .loader
        .wait_edits(&root, timeout)
        .map_err(|failure| error(failure.to_string()))?;
    let mut paths: Vec<Option<String>> = edits
        .paths
        .iter()
        .map(|path| Some(path.to_string_lossy().into_owned()))
        .collect();
    if edits.lost {
        paths.push(None);
    }
    values::string_vector_na(&paths)
}

/// `.Internal(ryLoaderCompile(root, libraries))`: builds the compiled code of the package at
/// `root` with ry's own toolchain, against ry's R home, so the shared object links ry's `libR`
/// and its `Makevars` runs ry's `bin/R`. `libraries` are the session's library paths, searched
/// for the headers of the packages in `LinkingTo`.
unsafe fn do_compile(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let libraries: Vec<PathBuf> = values::strings(call.arg(1))
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .collect();
    let home = ry_home().map_err(error)?;
    rcmd::compile(&home, Path::new(&root), &libraries)
        .map_err(|failure| error(format!("compiling src/ failed\n{failure}")))?;
    Ok(nil())
}

/// ry's R home for this runtime (`rcmd::runtime_home`), with the base R the session runs.
fn ry_home() -> Result<PathBuf, String> {
    let libr = interp::platform::library_path(do_compile as *const c_void)
        .map(PathBuf::from)
        .ok_or("cannot tell which file ry's runtime was loaded from")?;
    let gnu_r_home = PathBuf::from(interp::system::r_home());
    rcmd::runtime_home(&libr, &gnu_r_home).map_err(|failure| failure.to_string())
}

/// `.Internal(ryLoaderDeferred(root))`: `list(position, path, line)` of the statements held
/// back, in order.
unsafe fn do_deferred(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let session = session();
    let code: &[(usize, Code)] = session
        .deferred
        .get(&root)
        .map_or(&[], |deferred| &deferred.code);
    let value = RValue::List(vec![
        (
            "position".into(),
            RValue::Integer(
                code.iter()
                    .map(|(position, _)| i32::try_from(position + 1).unwrap_or(i32::MAX))
                    .collect(),
            ),
        ),
        (
            "path".into(),
            RValue::Character(code.iter().map(|(_, code)| code.path.clone()).collect()),
        ),
        (
            "line".into(),
            RValue::Integer(
                code.iter()
                    .map(|(_, code)| i32::try_from(code.line).unwrap_or(i32::MAX))
                    .collect(),
            ),
        ),
    ]);
    drop(session);
    scope(|| r_value(&value))
}

/// `.Internal(ryLoaderRunDeferred(root, ns, i))`: evaluates the `i`th statement held back.
unsafe fn do_run_deferred(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let namespace = call.arg(1);
    let index = values::as_integer_scalar(call.arg(2));
    let code = {
        let session = session();
        let deferred = session
            .deferred
            .get(&root)
            .ok_or_else(|| error(format!("{root} holds back no definitions")))?;
        let (_, code) = usize::try_from(index - 1)
            .ok()
            .and_then(|index| deferred.code.get(index))
            .ok_or_else(|| error(format!("{root} holds back no statement {index}")))?;
        code.clone()
    };
    if !code.effect_free {
        await_verdict();
    }
    let handle = HANDLES
        .with_borrow(|handles| handles.deferred.get(&root)?.get(&code.path).copied())
        .ok_or_else(|| error(format!("{} was not analyzed", code.path)))?;
    let expression = scope(|| lower_handle_code(handle, Position::of(&code)))?;
    eval(expression, namespace)?;
    Ok(nil())
}

/// `.Internal(ryLoaderUndefer(root))`: forgets the statements held back, run or not.
unsafe fn do_undefer(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    session().deferred.remove(&root);
    release_handles(|handles| handles.deferred.remove(&root));
    drop_deferred_definitions(&root);
    Ok(nil())
}

/// The code of a lazy binding: lowers its function literal and evaluates it in the namespace
/// the promise was made in.
unsafe fn do_force(call: &Call<'_>) -> R<Sexp> {
    let function = lower_handle_code(call.arg(0), position_of_value(call.arg(1))?)?;
    eval(function, call.env)
}

/// The handle of the pending plan's file `path`, made on first use.
unsafe fn pending_handle(root: &str, path: &str) -> R<Sexp> {
    let found = HANDLES.with_borrow(|handles| handles.pending.get(root)?.get(path).copied());
    if let Some(handle) = found {
        return Ok(handle);
    }
    let source = session()
        .pending
        .get(root)
        .and_then(|pending| pending.sources.get(path).cloned())
        .ok_or_else(|| error(format!("{path} was not analyzed")))?;
    let keep = values::as_logical_scalar(interp::envir::get_option("keep.source")) == 1;
    let file = Path::new(root).join(path).to_string_lossy().into_owned();
    let handle = source_handle(FileSource {
        source,
        path: file,
        keep,
    })?;
    interp::heap().preserve(handle);
    HANDLES.with_borrow_mut(|handles| {
        handles
            .pending
            .entry(root.to_owned())
            .or_default()
            .insert(path.to_owned(), handle)
    });
    Ok(handle)
}

/// Releases the handles `take` removes from the session's.
unsafe fn release_handles(take: impl FnOnce(&mut Handles) -> Option<FxHashMap<String, Sexp>>) {
    release(HANDLES.with_borrow_mut(take));
}

unsafe fn release(handles: Option<FxHashMap<String, Sexp>>) {
    let heap = interp::heap();
    for handle in handles.into_iter().flat_map(FxHashMap::into_values) {
        heap.release(handle);
    }
}

/// A position as the one integer vector a promise carries: start, end, line, column.
unsafe fn position_value(position: Position) -> R<Sexp> {
    let numbers = [position.start, position.end, position.line, position.column]
        .map(|number| i32::try_from(number).unwrap_or(i32::MAX));
    values::integer_vector(&numbers)
}

unsafe fn position_of_value(value: Sexp) -> R<Position> {
    let invalid = || error("ryLoaderForce needs a definition's position");
    if value.kind() != SexpType::Integer {
        return Err(invalid());
    }
    let number = |index: usize| {
        values::ints(value)
            .get(index)
            .and_then(|&number| u32::try_from(number).ok())
            .ok_or_else(invalid)
    };
    Ok(Position {
        start: number(0)?,
        end: number(1)?,
        line: number(2)?,
        column: number(3)?,
    })
}

/// Lowers the code at `position` in the file `handle` owns, with source references when the
/// load that made the handle keeps them or a debugger is attached, which can stop only where
/// code has them. The file's srcfile is built on first use and kept in the handle, because
/// building it costs a string per line of the file and most files of a package never need one.
unsafe fn lower_handle_code(handle: Sexp, position: Position) -> R<Sexp> {
    if handle.kind() != SexpType::ExternalPtr || handle.external_tag() != source_tag()? {
        return Err(error("ry's loader needs a source file it made"));
    }
    let address = handle.external_address() as *const FileSource;
    if address.is_null() {
        return Err(error("the source of this definition was released"));
    }
    // SAFETY: the address is a `Box<FileSource>` that `source_handle` leaked, and its finalizer
    // only runs once nothing references the handle, which the caller does.
    let file = &*address;
    if !file.keep && interp::interp().debugger.is_none() {
        return lower_code(&file.source, position, None);
    }
    let mut srcfile = handle.external_protected();
    if srcfile.kind() != SexpType::Environment {
        let text = match &file.source {
            Source::Tree(tree) => tree.syntax_node().text().to_string(),
            Source::Text(text) => text.to_string(),
        };
        srcfile = file_srcfile(&file.path, &text)?;
        interp::heap().set_external_protected(handle, srcfile);
    }
    lower_code(&file.source, position, Some(srcfile))
}

/// Lowers the code at `position` of `source`, with source references against `srcfile` when
/// there is one. In a tree the code is the outermost node spanning exactly its range, and in text
/// it is the code alone, parsed on its own: a top-level item parses the same either way, so a
/// file whose text is all the loader holds is never parsed whole. Source references count lines,
/// bytes and columns as the file does, so the code is parsed after as many spaces as its line
/// has bytes before it, its source references read the line's real text in their place, and its
/// lines are shifted to the line it starts on.
unsafe fn lower_code(source: &Source, position: Position, srcfile: Option<Sexp>) -> R<Sexp> {
    let missing = || error("a definition's code is not in its source file");
    let Position {
        start, end, line, ..
    } = position;
    let (code, prefix) = match source {
        Source::Tree(tree) => {
            let node = node_spanning(tree, start, end)?;
            if srcfile.is_none() {
                return lower_node(&node);
            }
            (node.text().to_string(), line_prefix(&node))
        }
        Source::Text(text) => {
            let code = text.get(start as usize..end as usize).ok_or_else(missing)?;
            let before = text.get(..start as usize).ok_or_else(missing)?;
            let prefix = &before[before.rfind('\n').map_or(0, |newline| newline + 1)..];
            (code.to_owned(), prefix.to_owned())
        }
    };
    let Some(srcfile) = srcfile else {
        return lower_node(&sole_node(&code, 0)?);
    };
    let node = sole_node(
        &format!("{}{code}", " ".repeat(prefix.len())),
        prefix.len() as u32,
    )?;
    lower_node_at_line(&node, &format!("{prefix}{code}"), line as usize, srcfile)
}

/// The text of `node`'s line before it, read back token by token to the line's start.
fn line_prefix(node: &SyntaxNode) -> String {
    let mut pieces = Vec::new();
    let mut token = node.first_token().and_then(|token| token.prev_token());
    while let Some(current) = token {
        let text = current.text();
        if let Some(newline) = text.rfind('\n') {
            pieces.push(text[newline + 1..].to_owned());
            break;
        }
        pieces.push(text.to_owned());
        token = current.prev_token();
    }
    pieces.iter().rev().map(String::as_str).collect()
}

/// The outermost node of `tree` spanning exactly `start..end`.
fn node_spanning(tree: &syntax::Parse, start: u32, end: u32) -> R<SyntaxNode> {
    let missing = || error("a definition's code is not in its source file");
    let root = tree.syntax_node();
    if start > end
        || !root
            .text_range()
            .contains_range(TextRange::new(start.into(), end.into()))
    {
        return Err(missing());
    }
    let range = TextRange::new(TextSize::from(start), TextSize::from(end));
    let element = root.covering_element(range);
    let mut node = match element.as_node() {
        Some(node) => node.clone(),
        None => element.parent().ok_or_else(missing)?,
    };
    while let Some(parent) = node.parent()
        && parent.text_range() == range
        && parent != root
    {
        node = parent;
    }
    if node == root || node.text_range() != range {
        return Err(missing());
    }
    Ok(node)
}

/// The one top-level node of `text` that starts at byte `start` and runs to its end.
fn sole_node(text: &str, start: u32) -> R<SyntaxNode> {
    let missing = || error("a definition's code is not in its source file");
    let parse = syntax::parse(text);
    if !parse.errors().is_empty() {
        return Err(missing());
    }
    let root = parse.syntax_node();
    let range = TextRange::new(TextSize::from(start), root.text_range().end());
    root.children()
        .find(|node| node.text_range() == range)
        .ok_or_else(missing)
}

/// An external pointer owning `file`, freed by the collector once nothing references it. It
/// protects the file's srcfile once one is built.
unsafe fn source_handle(file: FileSource) -> R<Sexp> {
    let heap = interp::heap();
    let address = Box::into_raw(Box::new(file)) as *mut c_void;
    let handle = heap.alloc_external_ptr(address, source_tag()?, nil());
    let handle = match handle {
        Ok(handle) => handle,
        Err(failure) => {
            drop(Box::from_raw(address as *mut FileSource));
            return Err(failure.into());
        }
    };
    heap.root(handle);
    heap.alloc_weak_ref(
        handle,
        nil(),
        nil(),
        release_source as *const () as usize,
        false,
    )?;
    Ok(handle)
}

extern "C" fn release_source(handle: Sexp) {
    // SAFETY: the collector calls this once, with the handle `source_handle` made, after which
    // nothing can reach the handle's address.
    unsafe {
        let address = handle.external_address() as *mut FileSource;
        if !address.is_null() {
            handle.set_external_address(std::ptr::null_mut());
            drop(Box::from_raw(address));
        }
    }
}

unsafe fn source_tag() -> R<Sexp> {
    install("ry_loader_source")
}

unsafe fn r_value(value: &RValue) -> R<Sexp> {
    match value {
        RValue::Null => Ok(nil()),
        RValue::Logical(flag) => values::scalar_bool(*flag),
        RValue::Integer(numbers) => values::integer_vector(numbers),
        RValue::Character(strings) => values::string_vector(strings),
        RValue::Vector(items) => {
            let list = values::alloc(SexpType::List, items.len())?;
            for (index, item) in items.iter().enumerate() {
                values::set_elt(list, index, r_value(item)?);
            }
            Ok(list)
        }
        RValue::List(fields) => {
            let list = values::alloc(SexpType::List, fields.len())?;
            for (index, (_, item)) in fields.iter().enumerate() {
                values::set_elt(list, index, r_value(item)?);
            }
            let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
            values::set_attr(
                list,
                values::symbols().names,
                values::string_vector(&names)?,
            )?;
            Ok(list)
        }
    }
}
