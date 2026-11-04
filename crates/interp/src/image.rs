//! The start-up image: the session as it stands once base R and the default packages have
//! loaded, saved to a file so that later starts load it instead of running base R's start-up
//! code. A start that finds no image for its key runs the slow way and writes one at the end;
//! a start that finds one loads it in place of everything from the C-level options to
//! `.First.sys()`. Anything an image cannot carry, and any failure to read or write one, sends
//! the start down the slow path with a one-line warning, never into a half-loaded session.
//!
//! An image is only correct for the inputs it was made from. The file name is a hash of the
//! runtime build, base R's files, the session flags the host set, which library directories
//! exist, and the environment variables that steer start-up in every session (`R_*`, the
//! locale, `TZ` and `HOME`). Every other variable start-up reads goes through [`variable`],
//! which records it, and the image stores those variables' values; a start whose values differ
//! runs the slow way and replaces the image. Hashing the whole environment instead would make
//! any variable a wrapper sets per process, such as a trace id, defeat the image. The site and
//! user profiles are not in the key, because a start that would source one never uses an
//! image. The heap part is [`heap::image`]; this module saves the interpreter state outside
//! the heap beside it and checks that every per-run structure is empty when the image is
//! written.

use {
    crate::{builtin_libraries, bytecode, data_table, dispatch, format, heap, interp, values::nil},
    heap::{
        Fixed, ImageError,
        image::{Reader, Writer},
        object::{Sexp, SexpType},
    },
    rustc_hash::FxHashSet,
    std::{
        cell::RefCell,
        env::VarError,
        ffi::OsString,
        path::{Path, PathBuf},
        time::UNIX_EPOCH,
    },
};

/// What a host contributes to its sessions' images.
#[derive(Clone, Copy)]
pub struct ImageHost {
    /// Identifies the build of the runtime, so that an image never loads into another one.
    pub identity: &'static str,
    /// The host's objects that exist before the session starts and that an image refers to by
    /// position, in an order every process of the build agrees on; or why the host's state
    /// rules out an image.
    pub fixed_objects: unsafe fn() -> Result<Vec<Sexp>, String>,
}

/// Where an eligible start reads or writes its image.
pub(crate) struct Plan {
    host: ImageHost,
    directory: PathBuf,
    path: PathBuf,
    /// The process environment the key was computed from, which the writer compares with the
    /// one start-up leaves to record what start-up set.
    environment: Vec<(String, String)>,
}

/// How many images a cache directory keeps; writing one more removes the oldest.
const KEPT_IMAGES: usize = 4;

const MAGIC: &[u8; 8] = b"RYSTART\x02";

thread_local! {
    /// The environment variables start-up has read, while an eligible start records them.
    static READS: RefCell<Option<Reads>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Reads {
    names: FxHashSet<String>,
    /// Whether start-up listed the whole environment, which makes every variable an input.
    every: bool,
}

/// Reads an environment variable, recording it as an input of the start-up image while
/// start-up runs. Every read the interpreter makes during start-up must come through here or
/// [`variable_os`], or a start with a different value would load an image made without it.
pub(crate) fn variable(name: &str) -> Result<String, VarError> {
    note(name);
    std::env::var(name)
}

/// [`variable`] for a value that need not be UTF-8.
pub(crate) fn variable_os(name: &str) -> Option<OsString> {
    note(name);
    std::env::var_os(name)
}

/// The whole environment, as `Sys.getenv()` lists it, which makes every variable an input of
/// the start-up image when start-up lists it.
pub(crate) fn variables() -> std::env::Vars {
    READS.with(|reads| {
        if let Some(reads) = reads.borrow_mut().as_mut() {
            reads.every = true;
        }
    });
    std::env::vars()
}

fn note(name: &str) {
    READS.with(|reads| {
        if let Some(reads) = reads.borrow_mut().as_mut()
            && !reads.names.contains(name)
        {
            reads.names.insert(name.to_owned());
        }
    });
}

/// Whether a variable steers start-up in every session, so that the image's file name
/// depends on it rather than the image recording it.
fn steers_start_up(name: &str) -> bool {
    name.starts_with("R_")
        || name.starts_with("LC_")
        || matches!(name, "LANG" | "LANGUAGE" | "TZ" | "HOME")
}

/// The plan for this start, or `None` when it cannot use an image: `RY_IMAGE=0` turns images
/// off, and an interactive session or one that sources a profile runs the slow way. Called
/// after `Renviron` is read, since the environment it leaves is part of the key.
pub(crate) unsafe fn plan(
    host: Option<ImageHost>,
    interactive: bool,
    profiles: bool,
    home: &str,
) -> Option<Plan> {
    let host = host?;
    if interactive || profiles || std::env::var("RY_IMAGE").is_ok_and(|value| value == "0") {
        return None;
    }
    let directory = match std::env::var_os("RY_IMAGE_DIR").filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let cache = std::env::var_os("XDG_CACHE_HOME")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))?;
            cache.join("ry").join("image")
        }
    };
    let environment = environment();
    let key = key(&host, home, &environment);
    let path = directory.join(format!("{key:016x}.image"));
    READS.with(|reads| *reads.borrow_mut() = Some(Reads::default()));
    Some(Plan {
        host,
        directory,
        path,
        environment,
    })
}

/// Why an image did not load.
pub(crate) enum LoadError {
    /// Nothing changed yet, so the start can go the slow way.
    Untouched(String),
    /// The image failed partway, after the heap took some of it, so the session cannot start.
    Partial(String),
}

/// Loads the plan's image, if there is one. `Ok(false)` means there is none, or it was made
/// with other values of the variables start-up reads, and the caller starts the slow way.
pub(crate) unsafe fn load(plan: &Plan) -> Result<bool, LoadError> {
    let bytes = match std::fs::read(&plan.path) {
        Ok(bytes) => bytes,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(failure) => {
            return Err(LoadError::Untouched(format!(
                "cannot read {}: {failure}",
                plan.path.display()
            )));
        }
    };
    let mut input = Reader { bytes: &bytes };
    let untouched = |error: ImageError| LoadError::Untouched(error.to_string());
    if input.take(MAGIC.len()).map_err(untouched)? != MAGIC {
        return Err(LoadError::Untouched(format!(
            "{} is not a start-up image",
            plan.path.display()
        )));
    }
    let expected = input.u64().map_err(untouched)?;
    if checksum(input.bytes) != expected {
        return Err(LoadError::Untouched(format!(
            "{} is damaged",
            plan.path.display()
        )));
    }
    let reads = input.blob().map_err(untouched)?;
    if !same_reads(reads).map_err(untouched)? {
        return Ok(false);
    }
    let heap_image = input.blob().map_err(untouched)?;
    let state = input.blob().map_err(untouched)?;
    if !input.bytes.is_empty() {
        return Err(LoadError::Untouched(format!(
            "{} has bytes past its end",
            plan.path.display()
        )));
    }
    let host_objects = (plan.host.fixed_objects)().map_err(LoadError::Untouched)?;
    let primitives = primitives();
    let fixed = Fixed {
        primitives: &primitives,
        extra: &extra_objects(&host_objects),
    };
    let roots = heap()
        .load_image(heap_image, &fixed, &mut |token| {
            builtin_libraries::routine_address(token)
        })
        .map_err(|error| match error {
            // The fixed objects are compared before the heap takes anything.
            ImageError::FixedObjects => LoadError::Untouched(error.to_string()),
            _ => LoadError::Partial(error.to_string()),
        })?;
    restore_state(state, &roots).map_err(|error| LoadError::Partial(error.to_string()))?;
    READS.with(|reads| *reads.borrow_mut() = None);
    // Pruning removes the image used least recently, so a load counts as a use. A file this
    // process cannot touch only loses its place in that order.
    if let Ok(file) = std::fs::File::options().append(true).open(&plan.path) {
        match file.set_modified(std::time::SystemTime::now()) {
            Ok(()) | Err(_) => {}
        }
    }
    Ok(true)
}

/// A checksum of an image's contents, so that a damaged file is rejected before the heap takes
/// any of it rather than failing partway. It reads eight bytes per step, because it runs over
/// the whole image on every start.
fn checksum(bytes: &[u8]) -> u64 {
    const SEED: u64 = 0xf135_7aea_2e62_a9c5;
    let mix = |hash: u64, word: u64| (hash.rotate_left(26) ^ word).wrapping_mul(SEED);
    let mut words = bytes.chunks_exact(8);
    let mut hash = (bytes.len() as u64).wrapping_mul(SEED);
    for word in &mut words {
        let mut array = [0u8; 8];
        array.copy_from_slice(word);
        hash = mix(hash, u64::from_le_bytes(array));
    }
    let mut tail = [0u8; 8];
    tail[..words.remainder().len()].copy_from_slice(words.remainder());
    mix(hash, u64::from_le_bytes(tail))
}

/// Whether the environment holds the values an image recorded for the variables start-up read.
fn same_reads(encoded: &[u8]) -> Result<bool, ImageError> {
    let mut input = Reader { bytes: encoded };
    let every = input.u8()? != 0;
    let count = input.u32()?;
    // When start-up listed the whole environment, a variable added since is a change too.
    let mut same = !every
        || std::env::vars_os()
            .filter(|(name, _)| name.to_str().is_none_or(|name| !steers_start_up(name)))
            .count()
            == count as usize;
    for _ in 0..count {
        let name = std::str::from_utf8(input.blob()?)
            .map_err(|_| ImageError::Invalid("a variable name is not UTF-8".to_owned()))?;
        let value = match input.u8()? {
            0 => None,
            _ => Some(input.blob()?),
        };
        let current = std::env::var_os(name);
        same &= current.as_ref().map(|value| value.as_encoded_bytes()) == value;
    }
    if !input.bytes.is_empty() {
        return Err(ImageError::Invalid(
            "the recorded variables have bytes past their end".to_owned(),
        ));
    }
    Ok(same)
}

/// Encodes the variables start-up read that the key leaves out, with their values before
/// start-up.
fn save_reads(plan: &Plan) -> Vec<u8> {
    let reads = READS
        .with(|reads| reads.borrow_mut().take())
        .unwrap_or_default();
    let mut names: Vec<String> = if reads.every {
        plan.environment
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    } else {
        reads.names.into_iter().collect()
    };
    names.retain(|name| !steers_start_up(name));
    names.sort_unstable();
    let mut out = Writer::default();
    out.u8(u8::from(reads.every));
    out.u32(names.len() as u32);
    for name in names {
        out.blob(name.as_bytes());
        let value = plan
            .environment
            .binary_search_by(|(other, _)| other.as_str().cmp(&name))
            .ok()
            .and_then(|position| plan.environment.get(position));
        match value {
            Some((_, value)) => {
                out.u8(1);
                out.blob(value.as_bytes());
            }
            None => out.u8(0),
        }
    }
    out.bytes
}

/// The record layout counts before start-up, taken at the very start of an eligible slow
/// start. A layout start-up registers would not exist in the loading process.
pub(crate) struct Before {
    layouts: (usize, usize),
}

pub(crate) unsafe fn before() -> Before {
    Before {
        layouts: heap().layouts.counts(),
    }
}

/// Writes the plan's image of the session start-up just finished.
pub(crate) unsafe fn write(plan: &Plan, before: &Before) -> Result<(), String> {
    let reads = save_reads(plan);
    if !eligible()? {
        return Ok(());
    }
    check_empty(before)?;
    let host_objects = (plan.host.fixed_objects)()?;
    let primitives = primitives();
    let extra = extra_objects(&host_objects);
    let fixed = Fixed {
        primitives: &primitives,
        extra: &extra,
    };
    let mut roots: Vec<Sexp> = Vec::new();
    let state = save_state(plan, &mut roots);
    let heap_image = heap()
        .write_image(
            &fixed,
            &roots,
            &|object| (object.kind() == SexpType::Bytecode).then(|| bytecode::source_of(object)),
            &mut |pointer| builtin_libraries::routine_token(pointer),
        )
        .map_err(|error| error.to_string())?;
    let mut payload = Writer::default();
    payload.blob(&reads);
    payload.blob(&heap_image);
    payload.blob(&state);
    let mut out = Writer::default();
    out.bytes.extend_from_slice(MAGIC);
    out.u64(checksum(&payload.bytes));
    out.bytes.extend_from_slice(&payload.bytes);

    std::fs::create_dir_all(&plan.directory)
        .map_err(|failure| format!("cannot create {}: {failure}", plan.directory.display()))?;
    let temporary = plan
        .path
        .with_extension(format!("{}.partial", std::process::id()));
    std::fs::write(&temporary, &out.bytes)
        .and_then(|()| std::fs::rename(&temporary, &plan.path))
        .map_err(|failure| {
            if let Err(cleanup) = std::fs::remove_file(&temporary)
                && cleanup.kind() != std::io::ErrorKind::NotFound
            {
                return format!(
                    "cannot write {}: {failure}, and cannot remove {}: {cleanup}",
                    plan.path.display(),
                    temporary.display()
                );
            }
            format!("cannot write {}: {failure}", plan.path.display())
        })?;
    prune(&plan.directory, &plan.path);
    Ok(())
}

/// Removes the plan's image, after it failed to load partway.
pub(crate) fn discard(plan: &Plan) {
    if let Err(failure) = std::fs::remove_file(&plan.path)
        && failure.kind() != std::io::ErrorKind::NotFound
    {
        crate::write_console(
            format!("ry: cannot remove {}: {failure}\n", plan.path.display()).as_bytes(),
            true,
        );
    }
}

/// Every primitive by index, which both processes create the same way before start-up.
fn primitives() -> Vec<Sexp> {
    interp()
        .builtins
        .iter()
        .map(|builtin| builtin.object.unwrap_or_else(nil))
        .collect()
}

/// The fixed objects beyond the primitives: `package:ry`, which builtin registration creates,
/// then the host's.
fn extra_objects(host: &[Sexp]) -> Vec<Sexp> {
    let mut extra: Vec<Sexp> = crate::standard_library::builtins_env()
        .into_iter()
        .collect();
    extra.extend_from_slice(host);
    extra
}

/// Whether this start's state can go into an image at all, like a start that sources no profile.
/// Base R's profile seeds the generator when `_R_RNG_VERSION_` is set, and every process loading
/// the image would draw the same numbers from that seed. The key covers only R home's packages,
/// so a namespace loaded from another library would outlive a reinstall, with lazy-load promises
/// into its old database.
unsafe fn eligible() -> Result<bool, String> {
    let seed = crate::values::install(".Random.seed").map_err(|error| format!("{error:?}"))?;
    if heap()
        .frame_cell(crate::values::global_env(), seed)
        .is_some()
    {
        return Ok(false);
    }
    let outside = crate::session::eval_text(OUTSIDE_R_HOME, crate::values::global_env())
        .map_err(|error| format!("cannot list the loaded namespaces: {error:?}"))?;
    Ok(crate::values::as_logical_scalar(outside) == 0)
}

/// Whether a loaded namespace lives outside `R_HOME/library`, whose entries may be links into
/// another library. The namespaces ry defines itself are not packages in any library.
const OUTSIDE_R_HOME: &str = "{ home <- normalizePath(list.files(file.path(R.home(), \"library\"), \
        full.names = TRUE)); \
    paths <- vapply(setdiff(loadedNamespaces(), c(\"base\", \"ry\", \"std\")), \
        function(name) getNamespaceInfo(name, \"path\"), \"\"); \
    !all(normalizePath(paths) %in% home) }";

/// Fails when start-up left state that an image does not carry.
unsafe fn check_empty(before: &Before) -> Result<(), String> {
    let state = interp();
    let mut left: Vec<&str> = Vec::new();
    if !state.frames.is_empty() || !state.foreign_calls.is_empty() {
        left.push("a call in progress");
    }
    if !state.warnings.is_empty() {
        left.push("pending warnings");
    }
    if !state.annotations.is_empty() {
        left.push("type annotation checks");
    }
    if crate::moves::tracking() {
        left.push("a value being moved");
    }
    if !state.library_tags.is_empty() {
        left.push("standard library tags");
    }
    if !state.globals.deferred_definitions.is_empty() {
        left.push("deferred S4 definitions");
    }
    if !state.suspended_handlers.is_empty() || state.parked_unwind.is_some() {
        left.push("suspended condition handlers");
    }
    if heap().layouts.counts() != before.layouts {
        left.push("record layouts");
    }
    if crate::graphics_engine::has_open_device() {
        left.push("an open graphics device");
    }
    if format::output_decimal() != "." {
        left.push("a decimal mark other than `.`");
    }
    if left.is_empty() {
        Ok(())
    } else {
        Err(format!("start-up left {}", left.join(", ")))
    }
}

/// Encodes the interpreter state outside the heap, with each object as its position in
/// `roots`, which the heap image maps to the loading process's objects.
unsafe fn save_state(plan: &Plan, roots: &mut Vec<Sexp>) -> Vec<u8> {
    let mut out = Writer::default();
    let mut root = |object: Sexp| -> u32 {
        roots.push(object);
        roots.len() as u32 - 1
    };
    let globals = &interp().globals;
    out.u32(root(globals.methods_namespace));
    out.u32(root(globals.methods_templates));
    out.u8(u8::from(globals.methods_dispatch));
    out.u8(u8::from(globals.allow_primitive_methods));

    let mut primitive_methods: Vec<_> = globals.primitive_methods.iter().collect();
    primitive_methods.sort_unstable_by_key(|(index, _)| **index);
    out.u32(primitive_methods.len() as u32);
    for (&index, methods) in primitive_methods {
        out.u32(index);
        out.u8(methods.state as u8);
        out.u32(root(methods.generic));
        out.u32(root(methods.methods));
    }

    out.u32(globals.keep.len() as u32);
    for &object in &globals.keep {
        out.u32(root(object));
    }

    let mut extends: Vec<_> = globals.s4_extends.iter().collect();
    extends.sort_unstable_by(|left, right| left.0.cmp(right.0));
    out.u32(extends.len() as u32);
    for (class, &object) in extends {
        out.blob(class);
        out.u32(root(object));
    }

    let mut implicit: Vec<_> = globals.implicit_classes.iter().collect();
    implicit.sort_unstable_by_key(|((shape, kind), _)| (*shape, *kind as u8));
    out.u32(implicit.len() as u32);
    for (&(shape, kind), &object) in implicit {
        out.u8(shape);
        out.u8(kind as u8);
        out.u32(root(object));
    }

    let libraries = &interp().builtin_libraries;
    out.u32(libraries.len() as u32);
    for (package, path) in libraries {
        out.blob(package.as_bytes());
        out.blob(path.as_bytes());
    }

    match data_table::namespace() {
        Some(namespace) => {
            out.u8(1);
            out.u32(root(namespace));
        }
        None => out.u8(0),
    }

    // What start-up changed in the process environment, which a loading process must change
    // the same way.
    let before: rustc_hash::FxHashMap<&str, &str> = plan
        .environment
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let after = environment();
    let mut changes: Vec<(String, Option<String>)> = Vec::new();
    for (name, value) in &after {
        if before.get(name.as_str()) != Some(&value.as_str()) {
            changes.push((name.clone(), Some(value.clone())));
        }
    }
    let names: FxHashSet<&str> = after.iter().map(|(name, _)| name.as_str()).collect();
    for (name, _) in &plan.environment {
        if !names.contains(name.as_str()) {
            changes.push((name.clone(), None));
        }
    }
    out.u32(changes.len() as u32);
    for (name, value) in changes {
        out.blob(name.as_bytes());
        match value {
            Some(value) => {
                out.u8(1);
                out.blob(value.as_bytes());
            }
            None => out.u8(0),
        }
    }
    out.bytes
}

/// Restores what [`save_state`] encoded.
unsafe fn restore_state(state: &[u8], roots: &[Sexp]) -> Result<(), ImageError> {
    let mut input = Reader { bytes: state };
    let invalid = |message: &str| ImageError::Invalid(message.to_owned());
    let root = |input: &mut Reader<'_>| -> Result<Sexp, ImageError> {
        let position = input.u32()? as usize;
        roots
            .get(position)
            .copied()
            .ok_or_else(|| invalid("the session state names an object the heap lacks"))
    };
    let text = |bytes: &[u8]| -> Result<String, ImageError> {
        String::from_utf8(bytes.to_vec()).map_err(|_| invalid("a name is not UTF-8"))
    };

    let globals = &mut interp().globals;
    globals.methods_namespace = root(&mut input)?;
    globals.methods_templates = root(&mut input)?;
    globals.methods_dispatch = input.u8()? != 0;
    globals.allow_primitive_methods = input.u8()? != 0;

    globals.primitive_methods.clear();
    for _ in 0..input.u32()? {
        let index = input.u32()?;
        let state = match input.u8()? {
            0 => dispatch::MethodsState::None,
            1 => dispatch::MethodsState::NeedsReset,
            2 => dispatch::MethodsState::Has,
            3 => dispatch::MethodsState::Suppressed,
            _ => return Err(invalid("a primitive's methods state is unknown")),
        };
        let generic = root(&mut input)?;
        let methods = root(&mut input)?;
        globals.primitive_methods.insert(
            index,
            dispatch::PrimitiveMethods {
                state,
                generic,
                methods,
            },
        );
    }

    globals.keep.clear();
    for _ in 0..input.u32()? {
        globals.keep.push(root(&mut input)?);
    }

    globals.s4_extends.clear();
    for _ in 0..input.u32()? {
        let class: Box<[u8]> = input.blob()?.into();
        globals.s4_extends.insert(class, root(&mut input)?);
    }

    globals.implicit_classes.clear();
    for _ in 0..input.u32()? {
        let shape = input.u8()?;
        let kind = SexpType::from_code(u32::from(input.u8()?))
            .ok_or_else(|| invalid("an implicit class has an unknown type"))?;
        globals
            .implicit_classes
            .insert((shape, kind), root(&mut input)?);
    }

    let mut libraries = Vec::new();
    for _ in 0..input.u32()? {
        let package = text(input.blob()?)?;
        let package = builtin_libraries::package_name(&package)
            .ok_or_else(|| invalid("a loaded library is not a builtin one"))?;
        libraries.push((package, text(input.blob()?)?));
    }
    interp().builtin_libraries = libraries;

    if input.u8()? != 0 {
        data_table::set_namespace(root(&mut input)?);
    }

    for _ in 0..input.u32()? {
        let name = text(input.blob()?)?;
        let value = match input.u8()? {
            0 => None,
            _ => Some(text(input.blob()?)?),
        };
        // SAFETY: the session starts before any other thread exists.
        unsafe {
            match value {
                Some(value) => std::env::set_var(&name, value),
                None => std::env::remove_var(&name),
            }
        }
    }
    if !input.bytes.is_empty() {
        return Err(invalid("the session state has bytes past its end"));
    }
    Ok(())
}

/// The process environment, sorted, without the variables a shell sets differently in every
/// process or terminal, which base R's start-up never reads.
fn environment() -> Vec<(String, String)> {
    const VOLATILE: &[&str] = &[
        "_",
        "PWD",
        "OLDPWD",
        "SHLVL",
        "TERM_SESSION_ID",
        "WINDOWID",
        "SSH_AUTH_SOCK",
        "SSH_CLIENT",
        "SSH_CONNECTION",
        "SSH_TTY",
        "TMUX_PANE",
        "STY",
        "XDG_SESSION_ID",
        "INVOCATION_ID",
        "JOURNAL_STREAM",
        "GPG_TTY",
        "RY_IMAGE",
        "RY_IMAGE_DIR",
    ];
    let mut variables: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| !VOLATILE.contains(&name.as_str()))
        .collect();
    variables.sort_unstable();
    variables
}

/// The image's key: a hash of the inputs every start-up reads.
fn key(host: &ImageHost, home: &str, environment: &[(String, String)]) -> u64 {
    let mut hash = crate::session::FNV_OFFSET;
    let mut mix = |bytes: &[u8]| {
        for &byte in bytes.iter().chain(&[0xff]) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(host.identity.as_bytes());
    let state = interp();
    mix(&[
        u8::from(state.prelude),
        u8::from(state.object_systems),
        u8::from(state.annotations.enabled),
    ]);
    for (name, value) in environment {
        if steers_start_up(name) {
            mix(name.as_bytes());
            mix(value.as_bytes());
        }
    }
    // Base R's files, by size and modification time: the loader, the profile, and every
    // package's R code, metadata and namespace file.
    let mut files: Vec<PathBuf> = vec![
        Path::new(home).join("library/base/R/base"),
        Path::new(home).join("library/base/R/Rprofile"),
        Path::new(home).join("etc/Renviron"),
    ];
    if let Ok(packages) = std::fs::read_dir(Path::new(home).join("library")) {
        let mut packages: Vec<PathBuf> = packages.flatten().map(|entry| entry.path()).collect();
        packages.sort_unstable();
        for package in packages {
            files.push(package.join("NAMESPACE"));
            for directory in ["R", "Meta"] {
                if let Ok(entries) = std::fs::read_dir(package.join(directory)) {
                    let mut entries: Vec<PathBuf> =
                        entries.flatten().map(|entry| entry.path()).collect();
                    entries.sort_unstable();
                    files.extend(entries);
                }
            }
        }
    }
    for file in &files {
        mix(file.as_os_str().as_encoded_bytes());
        if let Ok(metadata) = std::fs::metadata(file) {
            mix(&metadata.len().to_le_bytes());
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_nanos());
            mix(&modified.to_le_bytes());
        }
    }
    // The base profile puts only the library directories that exist on `.libPaths()`.
    for variable in ["R_LIBS", "R_LIBS_USER", "R_LIBS_SITE"] {
        if let Ok(paths) = std::env::var(variable) {
            for path in std::env::split_paths(&paths) {
                // The profile stores each directory as an absolute path, so a relative one
                // depends on the working directory as well.
                let path = crate::connections::expand_path(&path.to_string_lossy());
                let absolute = std::path::absolute(&path).unwrap_or_else(|_| PathBuf::from(&path));
                mix(absolute.as_os_str().as_encoded_bytes());
                mix(&[u8::from(absolute.is_dir())]);
            }
        }
    }
    hash
}

/// Removes the oldest images past [`KEPT_IMAGES`].
fn prune(directory: &Path, written: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut images: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == "image" && path != written).then_some(())?;
            Some((entry.metadata().ok()?.modified().ok()?, path))
        })
        .collect();
    images.sort_unstable();
    let excess = (images.len() + 1).saturating_sub(KEPT_IMAGES);
    for (_, path) in images.into_iter().take(excess) {
        // Another process may have removed it first, which leaves the same result.
        if let Err(failure) = std::fs::remove_file(&path)
            && failure.kind() != std::io::ErrorKind::NotFound
        {
            crate::write_console(
                format!(
                    "ry: cannot remove the old start-up image {}: {failure}\n",
                    path.display()
                )
                .as_bytes(),
                true,
            );
        }
    }
}
