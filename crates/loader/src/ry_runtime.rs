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
        Code, Loader,
        r::{RUNTIME_RY, RValue, error_value, plan_value},
    },
    heap::object::{Sexp, SexpType},
    interp::{
        builtins::{BuiltinDef, Call, internal, internal_function},
        eval::{define, eval, make_promise},
        lower::{file_srcfile, lower_node, lower_node_in_file, parse_text},
        scope,
        unwind::{R, error},
        values::{self, install, length, nil},
    },
    rustc_hash::FxHashMap,
    semantics::ANALYSIS_STACK_SIZE,
    std::{
        ffi::c_void,
        path::{Path, PathBuf},
        sync::{LazyLock, Mutex, PoisonError},
        thread,
    },
    syntax::{Parse, SyntaxNode, TextRange, TextSize},
};

pub const BUILTINS: &[BuiltinDef] = &[
    internal("ryLoaderPlan", 2, do_plan),
    internal("ryLoaderCommit", 1, do_commit).invisible(),
    internal("ryLoaderBind", 2, do_bind).invisible(),
    internal("ryLoaderRun", 3, do_run).invisible(),
    internal("ryLoaderForce", 3, do_force),
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
}

struct Pending {
    /// The root as the loader names it, which commits the plan.
    root: PathBuf,
    bind: Vec<Code>,
    run: Vec<Code>,
    /// The syntax tree of every file the plan's code comes from.
    trees: FxHashMap<String, Parse>,
}

/// Whether the project is in strict mode, which the host reads from its `ry.toml`.
pub fn set_strict(strict: bool) {
    session().loader.set_strict(strict);
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
    let plan = match session.loader.plan(Path::new(root), full) {
        Ok(plan) => plan,
        Err(failure) => return error_value(&failure.to_string()),
    };
    let mut trees = FxHashMap::default();
    for code in plan.bind.iter().chain(&plan.run) {
        if !trees.contains_key(&code.path)
            && let Some(tree) = session.loader.syntax(&plan.root, &code.path)
        {
            trees.insert(code.path.clone(), tree);
        }
    }
    let value = plan_value(&plan, false);
    session.pending.insert(
        root.to_owned(),
        Pending {
            root: plan.root,
            bind: plan.bind,
            run: plan.run,
            trees,
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
/// that calls `ryLoaderForce` on its file's tree.
unsafe fn do_bind(call: &Call<'_>) -> R<Sexp> {
    let root = values::require_string(call.arg(0), "root")?;
    let namespace = call.arg(1);
    let (bind, trees) = {
        let session = session();
        let pending = session
            .pending
            .get(&root)
            .ok_or_else(|| error(format!("no load of {root} is pending")))?;
        (pending.bind.clone(), pending.trees.clone())
    };
    let force = internal_function(install("ryLoaderForce")?)
        .ok_or_else(|| error("the loader's internals are not installed"))?;
    scope(|| {
        let mut handles: FxHashMap<&str, Sexp> = FxHashMap::default();
        for code in &bind {
            let handle = match handles.get(code.path.as_str()) {
                Some(&handle) => handle,
                None => {
                    let tree = trees
                        .get(&code.path)
                        .ok_or_else(|| error(format!("{} was not analyzed", code.path)))?;
                    let handle =
                        tree_handle(tree.clone(), debug_srcfile(&root, &code.path, tree)?)?;
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
    let (code, tree) = {
        let session = session();
        let pending = session
            .pending
            .get(&root)
            .ok_or_else(|| error(format!("no load of {root} is pending")))?;
        let code = usize::try_from(index - 1)
            .ok()
            .and_then(|index| pending.run.get(index))
            .ok_or_else(|| error(format!("the plan has no statement {index}")))?;
        let tree = pending
            .trees
            .get(&code.path)
            .ok_or_else(|| error(format!("{} was not analyzed", code.path)))?;
        (code.clone(), tree.clone())
    };
    let expression = scope(|| {
        let srcfile = debug_srcfile(&root, &code.path, &tree)?;
        lower_range(&tree, code.source_start, code.source_end, srcfile)
    })?;
    eval(expression, namespace)?;
    Ok(nil())
}

/// The code of a lazy binding: lowers its function literal and evaluates it in the namespace
/// the promise was made in.
unsafe fn do_force(call: &Call<'_>) -> R<Sexp> {
    let handle = call.arg(0);
    if handle.kind() != SexpType::ExternalPtr || handle.external_tag() != tree_tag()? {
        return Err(error("ryLoaderForce needs a syntax tree from ry's loader"));
    }
    let address = handle.external_address() as *const Parse;
    if address.is_null() {
        return Err(error("the syntax tree of this definition was released"));
    }
    // SAFETY: the address is a `Box<Parse>` that `tree_handle` leaked, and its finalizer only
    // runs once nothing references the handle, which this call does.
    let tree = &*address;
    let offset = |argument: Sexp| u32::try_from(values::as_integer_scalar(argument)).ok();
    let (Some(start), Some(end)) = (offset(call.arg(1)), offset(call.arg(2))) else {
        return Err(error("ryLoaderForce needs two byte offsets"));
    };
    let function = lower_range(tree, start, end, handle.external_protected())?;
    eval(function, call.env)
}

/// Lowers the outermost node spanning exactly `start..end` of `tree`, with source references
/// when `srcfile` is a srcfile environment.
unsafe fn lower_range(tree: &Parse, start: u32, end: u32, srcfile: Sexp) -> R<Sexp> {
    let root = SyntaxNode::new_root(tree.green().clone());
    if start > end
        || !root
            .text_range()
            .contains_range(TextRange::new(start.into(), end.into()))
    {
        return Err(error("a definition's code is not in its syntax tree"));
    }
    let range = TextRange::new(TextSize::from(start), TextSize::from(end));
    let element = root.covering_element(range);
    let mut node = match element.as_node() {
        Some(node) => node.clone(),
        None => element
            .parent()
            .ok_or_else(|| error("a definition's code is not in its syntax tree"))?,
    };
    while let Some(parent) = node.parent()
        && parent.text_range() == range
        && parent != root
    {
        node = parent;
    }
    if node == root || node.text_range() != range {
        return Err(error("a definition's code is not in its syntax tree"));
    }
    match srcfile.kind() {
        SexpType::Environment => lower_node_in_file(&node, &root.text().to_string(), srcfile),
        _ => lower_node(&node),
    }
}

/// A debugger can stop only where code has source references, so while one is attached the
/// loader keeps them, against a srcfile naming the file's absolute path. Otherwise it skips them,
/// as they cost memory on every statement of every loaded function.
unsafe fn debug_srcfile(root: &str, path: &str, tree: &Parse) -> R<Sexp> {
    if interp::interp().debugger.is_none() {
        return Ok(nil());
    }
    let file = Path::new(root).join(path);
    let text = SyntaxNode::new_root(tree.green().clone())
        .text()
        .to_string();
    file_srcfile(&file.to_string_lossy(), &text)
}

/// An external pointer owning `tree`, freed by the collector once no promise references it. It
/// protects `srcfile`, which the definitions lowered from the tree keep source references against.
unsafe fn tree_handle(tree: Parse, srcfile: Sexp) -> R<Sexp> {
    let heap = interp::heap();
    let address = Box::into_raw(Box::new(tree)) as *mut c_void;
    let handle = heap.alloc_external_ptr(address, tree_tag()?, srcfile);
    let handle = match handle {
        Ok(handle) => handle,
        Err(failure) => {
            drop(Box::from_raw(address as *mut Parse));
            return Err(failure.into());
        }
    };
    heap.root(handle);
    heap.alloc_weak_ref(
        handle,
        nil(),
        nil(),
        release_tree as *const () as usize,
        false,
    )?;
    Ok(handle)
}

extern "C" fn release_tree(handle: Sexp) {
    // SAFETY: the collector calls this once, with the handle `tree_handle` made, after which
    // nothing can reach the handle's address.
    unsafe {
        let address = handle.external_address() as *mut Parse;
        if !address.is_null() {
            handle.set_external_address(std::ptr::null_mut());
            drop(Box::from_raw(address));
        }
    }
}

unsafe fn tree_tag() -> R<Sexp> {
    install("ry_loader_syntax_tree")
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
