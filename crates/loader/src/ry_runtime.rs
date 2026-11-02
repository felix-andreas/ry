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
        Code, Loader, Source,
        r::{RUNTIME_RY, RValue, error_value, plan_value},
    },
    heap::object::{Sexp, SexpType},
    interp::{
        builtins::{BuiltinDef, Call, internal, internal_function},
        eval::{define, eval, make_promise},
        lower::{file_srcfile, lower_node, lower_node_in_file, parse_text},
        methods::{DeferredDefinitions, defer_definitions, drop_deferred_definitions},
        scope,
        unwind::{R, error},
        values::{self, install, length, nil},
    },
    rustc_hash::{FxHashMap, FxHashSet},
    semantics::ANALYSIS_STACK_SIZE,
    std::{
        ffi::c_void,
        path::{Path, PathBuf},
        sync::{
            LazyLock, Mutex, PoisonError,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    },
    syntax::{SyntaxNode, TextRange, TextSize},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryLoaderPlan", 2, do_plan),
    internal("ryLoaderCommit", 1, do_commit).invisible(),
    internal("ryLoaderBind", 2, do_bind).invisible(),
    internal("ryLoaderRun", 3, do_run).invisible(),
    internal("ryLoaderForce", 3, do_force),
    internal("ryLoaderDefer", 5, do_defer).invisible(),
    internal("ryLoaderDeferred", 1, do_deferred),
    internal("ryLoaderRunDeferred", 3, do_run_deferred).invisible(),
    internal("ryLoaderUndefer", 1, do_undefer).invisible(),
];

/// Evaluates the loader's R side into a fresh environment and attaches it as `tools:ry`, which
/// gives the session `load_all()`.
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

/// Whether a loaded package's sources changed on disk since it was loaded, which the console
/// asks before each command to decide whether to reload. A change is reported once.
pub fn changed() -> bool {
    !session().loader.changed_roots().is_empty()
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
}

struct Deferred {
    /// Each statement with its position in the plan's `run`.
    code: Vec<(usize, Code)>,
    sources: FxHashMap<String, Source>,
}

struct Pending {
    /// The root as the loader names it, which commits the plan.
    root: PathBuf,
    bind: Vec<Code>,
    run: Vec<Code>,
    /// The code of every file the plan's code comes from.
    sources: FxHashMap<String, Source>,
}

/// Kept outside the session, so setting it never waits for a [`prepare`] that holds the
/// session.
static STRICT: AtomicBool = AtomicBool::new(false);

/// Whether the project is in strict mode, which the host reads from its `ry.toml`.
pub fn set_strict(strict: bool) {
    STRICT.store(strict, Ordering::Relaxed);
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

/// `.Internal(ryLoaderPlan(root, full))`: the plan as `runtime.R` reads it, or `list(error =)`.
unsafe fn do_plan(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let full = values::require_flag(call.arg(1), "full")?;
    let value = thread::scope(|threads| {
        thread::Builder::new()
            .name("ry-loader".to_owned())
            .stack_size(ANALYSIS_STACK_SIZE)
            .spawn_scoped(threads, || plan(&root, full))
            .map(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
    });
    let value =
        value.map_err(|failure| error(format!("cannot start the loader's thread: {failure}")))?;
    scope(|| r_value(&value))
}

fn plan(root: &str, full: bool) -> RValue {
    let mut session = session();
    session.loader.set_strict(STRICT.load(Ordering::Relaxed));
    let plan = match session.loader.plan(Path::new(root), full) {
        Ok(plan) => plan,
        Err(failure) => return error_value(&failure.to_string()),
    };
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

unsafe fn do_commit(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let mut session = session();
    if let Some(pending) = session.pending.remove(&root) {
        session.loader.commit(&pending.root);
    }
    Ok(nil())
}

/// `.Internal(ryLoaderBind(root, ns))`: binds every definition of the pending plan as a promise
/// that calls `ryLoaderForce` on its file's code.
unsafe fn do_bind(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let namespace = call.arg(1);
    let (bind, sources) = {
        let session = session();
        let pending = session
            .pending
            .get(&root)
            .ok_or_else(|| error(format!("no load of {root} is pending")))?;
        (pending.bind.clone(), pending.sources.clone())
    };
    let force = internal_function(install("ryLoaderForce")?)
        .ok_or_else(|| error("the loader's internals are not installed"))?;
    scope(|| {
        let mut handles: FxHashMap<&str, Sexp> = FxHashMap::default();
        for code in &bind {
            let handle = match handles.get(code.path.as_str()) {
                Some(&handle) => handle,
                None => {
                    let source = sources
                        .get(&code.path)
                        .ok_or_else(|| error(format!("{} was not analyzed", code.path)))?;
                    let srcfile = debug_srcfile(&root, &code.path, source)?;
                    let handle = source_handle(source.clone(), srcfile)?;
                    handles.insert(&code.path, handle);
                    handle
                }
            };
            let Some(name) = &code.name else { continue };
            scope(|| {
                let start = values::scalar_integer(code.source_start as i32)?;
                let end = values::scalar_integer(code.source_end as i32)?;
                let promise = make_promise(values::call(force, &[handle, start, end])?, namespace)?;
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
    let (code, source) = {
        let session = session();
        let pending = session
            .pending
            .get(&root)
            .ok_or_else(|| error(format!("no load of {root} is pending")))?;
        let code = usize::try_from(index - 1)
            .ok()
            .and_then(|index| pending.run.get(index))
            .ok_or_else(|| error(format!("the plan has no statement {index}")))?;
        let source = pending
            .sources
            .get(&code.path)
            .ok_or_else(|| error(format!("{} was not analyzed", code.path)))?;
        (code.clone(), source.clone())
    };
    let expression = scope(|| {
        let srcfile = debug_srcfile(&root, &code.path, &source)?;
        lower_range(&source, code.source_start, code.source_end, srcfile)
    })?;
    eval(expression, namespace)?;
    Ok(nil())
}

/// `.Internal(ryLoaderDefer(root, positions, classes, generics, force))`: holds back the
/// pending plan's statements at `positions` (1-based, in `run`) and registers `force` to run
/// when a lookup needs one of `classes` or `generics`.
unsafe fn do_defer(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let positions = call.arg(1);
    let names = |argument: Sexp| -> FxHashSet<Box<[u8]>> {
        (0..length(argument))
            .map(|index| Box::from(argument.element(index).bytes()))
            .collect()
    };
    let (classes, generics, force) = (names(call.arg(2)), names(call.arg(3)), call.arg(4));
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
    let sources = code
        .iter()
        .filter_map(|(_, code)| Some((code.path.clone(), pending.sources.get(&code.path)?.clone())))
        .collect();
    session
        .deferred
        .insert(root.clone(), Deferred { code, sources });
    defer_definitions(DeferredDefinitions {
        key: root,
        classes,
        generics,
        force,
    });
    Ok(nil())
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
    let (code, source) = {
        let session = session();
        let deferred = session
            .deferred
            .get(&root)
            .ok_or_else(|| error(format!("{root} holds back no definitions")))?;
        let (_, code) = usize::try_from(index - 1)
            .ok()
            .and_then(|index| deferred.code.get(index))
            .ok_or_else(|| error(format!("{root} holds back no statement {index}")))?;
        let source = deferred
            .sources
            .get(&code.path)
            .ok_or_else(|| error(format!("{} was not analyzed", code.path)))?;
        (code.clone(), source.clone())
    };
    let expression = scope(|| {
        let srcfile = debug_srcfile(&root, &code.path, &source)?;
        lower_range(&source, code.source_start, code.source_end, srcfile)
    })?;
    eval(expression, namespace)?;
    Ok(nil())
}

/// `.Internal(ryLoaderUndefer(root))`: forgets the statements held back, run or not.
unsafe fn do_undefer(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    session().deferred.remove(&root);
    drop_deferred_definitions(&root);
    Ok(nil())
}

/// The code of a lazy binding: lowers its function literal and evaluates it in the namespace
/// the promise was made in.
unsafe fn do_force(call: &Call<'_>) -> R<Sexp> {
    let handle = call.arg(0);
    if handle.kind() != SexpType::ExternalPtr || handle.external_tag() != source_tag()? {
        return Err(error("ryLoaderForce needs a source file from ry's loader"));
    }
    let address = handle.external_address() as *const Source;
    if address.is_null() {
        return Err(error("the source of this definition was released"));
    }
    // SAFETY: the address is a `Box<Source>` that `source_handle` leaked, and its finalizer
    // only runs once nothing references the handle, which this call does.
    let source = &*address;
    let offset = |argument: Sexp| u32::try_from(values::as_integer_scalar(argument)).ok();
    let (Some(start), Some(end)) = (offset(call.arg(1)), offset(call.arg(2))) else {
        return Err(error("ryLoaderForce needs two byte offsets"));
    };
    let function = lower_range(source, start, end, handle.external_protected())?;
    eval(function, call.env)
}

/// Lowers the code at `start..end` of `source`, with source references when `srcfile` is a
/// srcfile environment. In a tree that is the outermost node spanning exactly that range. In text
/// it is the code alone, parsed on its own: a top-level item parses the same either way, so a
/// file whose text is all the loader holds is never parsed whole, except for a debugger, whose
/// source references need offsets into the whole file.
unsafe fn lower_range(source: &Source, start: u32, end: u32, srcfile: Sexp) -> R<Sexp> {
    let missing = || error("a definition's code is not in its source file");
    let with_srcfile = srcfile.kind() == SexpType::Environment;
    let parsed;
    let tree = match source {
        Source::Tree(tree) => tree,
        Source::Text(text) if with_srcfile => {
            parsed = syntax::parse(text);
            &parsed
        }
        Source::Text(text) => {
            let code = text.get(start as usize..end as usize).ok_or_else(missing)?;
            let parse = syntax::parse(code);
            if !parse.errors().is_empty() {
                return Err(missing());
            }
            let root = SyntaxNode::new_root(parse.green().clone());
            let whole = root.text_range();
            let node = root
                .children()
                .find(|node| node.text_range() == whole)
                .ok_or_else(missing)?;
            return lower_node(&node);
        }
    };
    let root = SyntaxNode::new_root(tree.green().clone());
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
    if with_srcfile {
        lower_node_in_file(&node, &root.text().to_string(), srcfile)
    } else {
        lower_node(&node)
    }
}

/// A debugger can stop only where code has source references, so while one is attached the
/// loader keeps them, against a srcfile naming the file's absolute path. Otherwise it skips them,
/// as they cost memory on every statement of every loaded function.
unsafe fn debug_srcfile(root: &str, path: &str, source: &Source) -> R<Sexp> {
    if interp::interp().debugger.is_none() {
        return Ok(nil());
    }
    let file = Path::new(root).join(path);
    let text = match source {
        Source::Tree(tree) => SyntaxNode::new_root(tree.green().clone())
            .text()
            .to_string(),
        Source::Text(text) => text.to_string(),
    };
    file_srcfile(&file.to_string_lossy(), &text)
}

/// An external pointer owning `source`, freed by the collector once no promise references it. It
/// protects `srcfile`, which the definitions lowered from the source keep source references
/// against.
unsafe fn source_handle(source: Source, srcfile: Sexp) -> R<Sexp> {
    let heap = interp::heap();
    let address = Box::into_raw(Box::new(source)) as *mut c_void;
    let handle = heap.alloc_external_ptr(address, source_tag()?, srcfile);
    let handle = match handle {
        Ok(handle) => handle,
        Err(failure) => {
            drop(Box::from_raw(address as *mut Source));
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
        let address = handle.external_address() as *mut Source;
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
