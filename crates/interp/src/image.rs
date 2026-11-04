//! The start-up image: the session as it stands once base R and the default packages have
//! loaded, built with ry's base R tree and shipped in it read-only, at
//! `library/base/R/ry-start.image`. A start the image applies to loads it in place of base R's
//! loader, the namespace loads and attaching the default packages, then runs the start-up steps
//! that read the process environment, so those always see this process's values. Nothing writes
//! an image at run time: `ry-rcmd write-start-image` writes it while the base tree is built
//! (`crates/rcmd/src/base.rs`), in a fixed environment.
//!
//! Base R's profile and the options-setting `.onLoad` hooks of utils, grDevices and stats read
//! the environment, and they run again after every load. The image is for one set of namespaces
//! loaded one way, so a start whose environment changes what loads, or how, starts the slow
//! way: see [`applies`]. The heap part is [`heap::image`]; this module saves the interpreter
//! state outside the heap beside it and checks that every per-run structure is empty when the
//! image is written.

use {
    crate::{builtin_libraries, bytecode, data_table, dispatch, format, heap, interp, values::nil},
    heap::{
        Fixed, ImageError, Relocation,
        image::{Reader, Writer},
        object::{Sexp, SexpType},
    },
    std::path::{Path, PathBuf},
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

const MAGIC: &[u8; 8] = b"RYSTART\x03";

/// The variables that change which namespaces start-up loads or how it loads them, which the
/// image holds as the fixed build environment loaded them.
const LOADING_VARIABLES: &[&str] = &[
    "R_DEFAULT_PACKAGES",
    "RY_METHODS_NATIVE",
    "R_TESTS",
    "_R_TRACE_LOADNAMESPACE_",
    "_R_NO_REPORT_MISSING_NAMESPACES_",
    "_R_METHODS_DONT_CHECKSUBCLASSES",
];

/// The image of the R home `home`.
pub fn path(home: &str) -> PathBuf {
    Path::new(home).join("library/base/R/ry-start.image")
}

/// Whether a start can load the image: it is not interactive, sources no profile, does not turn
/// images off with `RY_IMAGE=0`, and leaves every variable that changes what start-up loads
/// unset. `R_TESTS` names a file base R's profile sources before the default packages load.
pub(crate) fn applies(interactive: bool, profiles: bool) -> bool {
    !interactive
        && !profiles
        && std::env::var_os("RY_IMAGE").is_none_or(|value| value != "0")
        && std::env::var_os("R_KEEP_PKG_SOURCE").is_none_or(|value| value != "yes")
        && LOADING_VARIABLES
            .iter()
            .all(|name| std::env::var_os(name).is_none_or(|value| value.is_empty()))
}

/// Why an image did not load.
pub(crate) enum LoadError {
    /// Nothing changed yet, so the start can go the slow way.
    Untouched(String),
    /// The image failed partway, after the heap took some of it, so the session cannot start.
    Partial(String),
}

/// Loads the image of the R home `home`. `Ok(false)` means there is none, or another build of
/// the runtime wrote it, and the caller starts the slow way.
pub(crate) unsafe fn load(host: &ImageHost, home: &str) -> Result<bool, LoadError> {
    let path = &path(home);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(failure) => {
            return Err(LoadError::Untouched(format!(
                "cannot read {}: {failure}",
                path.display()
            )));
        }
    };
    let mut input = Reader { bytes: &bytes };
    let untouched = |error: ImageError| LoadError::Untouched(error.to_string());
    if input.take(MAGIC.len()).map_err(untouched)? != MAGIC {
        return Err(LoadError::Untouched(format!(
            "{} is not a start-up image",
            path.display()
        )));
    }
    // A development build changes its identity with every edit to the runtime, until the base
    // tree is rebuilt; that start is simply slow.
    if input.blob().map_err(untouched)? != host.identity.as_bytes() {
        return Ok(false);
    }
    let expected = input.u64().map_err(untouched)?;
    if checksum(input.bytes) != expected {
        return Err(LoadError::Untouched(format!(
            "{} is damaged",
            path.display()
        )));
    }
    // The writer's R home was the base tree in its build directory, and every path start-up
    // recorded under it (the lazy-load databases, the namespaces' directories) moves to where
    // the tree is now. That is the directory the image's real path is in, not this home, which
    // links to the tree's packages: a slow start records the packages' resolved paths too.
    let tree = std::fs::canonicalize(path)
        .ok()
        .and_then(|image| Some(r_path(image.ancestors().nth(4)?)))
        .unwrap_or_else(|| home.to_owned());
    let mut relocation = Relocation::default();
    for _ in 0..input.u32().map_err(untouched)? {
        let from = input.blob().map_err(untouched)?.to_vec();
        relocation.prefixes.push((from, tree.as_bytes().to_vec()));
    }
    let heap_image = input.blob().map_err(untouched)?;
    let state = input.blob().map_err(untouched)?;
    if !input.bytes.is_empty() {
        return Err(LoadError::Untouched(format!(
            "{} has bytes past its end",
            path.display()
        )));
    }
    let host_objects = (host.fixed_objects)().map_err(LoadError::Untouched)?;
    let primitives = primitives();
    let fixed = Fixed {
        primitives: &primitives,
        extra: &extra_objects(&host_objects),
    };
    let roots = heap()
        .load_image(heap_image, &fixed, &relocation, &mut |token| {
            builtin_libraries::routine_address(token)
        })
        .map_err(|error| match error {
            // The fixed objects are compared before the heap takes anything.
            ImageError::FixedObjects => LoadError::Untouched(error.to_string()),
            _ => LoadError::Partial(error.to_string()),
        })?;
    restore_state(state, &roots, &relocation)
        .map_err(|error| LoadError::Partial(error.to_string()))?;
    Ok(true)
}

/// `path` as R spells a resolved path: without Windows' verbatim `\\?\` prefix and with forward
/// slashes, as the Windows front end exports `R_HOME`.
fn r_path(path: &Path) -> String {
    let text = crate::platform::path_text(path);
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    }
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

/// The record layout counts before start-up, taken before the start the writer images. A layout
/// start-up registers would not exist in the loading process.
pub(crate) struct Before {
    layouts: (usize, usize),
}

pub(crate) unsafe fn before() -> Before {
    Before {
        layouts: heap().layouts.counts(),
    }
}

/// Writes the image of the session start-up just finished in the R home `home` to `path`.
pub(crate) unsafe fn write(
    host: &ImageHost,
    home: &str,
    path: &Path,
    before: &Before,
) -> Result<(), String> {
    check_empty(before)?;
    let host_objects = (host.fixed_objects)()?;
    let primitives = primitives();
    let extra = extra_objects(&host_objects);
    let fixed = Fixed {
        primitives: &primitives,
        extra: &extra,
    };
    let mut roots: Vec<Sexp> = Vec::new();
    let state = save_state(&mut roots);
    let heap_image = heap()
        .write_image(
            &fixed,
            &roots,
            &|object| (object.kind() == SexpType::Bytecode).then(|| bytecode::source_of(object)),
            &mut |pointer| builtin_libraries::routine_token(pointer),
        )
        .map_err(|error| error.to_string())?;
    let mut payload = Writer::default();
    let mut homes = vec![home.to_owned()];
    if let Ok(canonical) = std::fs::canonicalize(home)
        && r_path(&canonical) != home
    {
        homes.push(r_path(&canonical));
    }
    payload.u32(homes.len() as u32);
    for home in &homes {
        payload.blob(home.as_bytes());
    }
    payload.blob(&heap_image);
    payload.blob(&state);
    let mut out = Writer::default();
    out.bytes.extend_from_slice(MAGIC);
    out.blob(host.identity.as_bytes());
    out.u64(checksum(&payload.bytes));
    out.bytes.extend_from_slice(&payload.bytes);
    std::fs::write(path, &out.bytes)
        .map_err(|failure| format!("cannot write {}: {failure}", path.display()))
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
    let seed = crate::values::install(".Random.seed").map_err(|error| format!("{error:?}"))?;
    if heap()
        .frame_cell(crate::values::global_env(), seed)
        .is_some()
    {
        left.push("a random seed");
    }
    if left.is_empty() {
        Ok(())
    } else {
        Err(format!("start-up left {}", left.join(", ")))
    }
}

/// Encodes the interpreter state outside the heap, with each object as its position in
/// `roots`, which the heap image maps to the loading process's objects.
unsafe fn save_state(roots: &mut Vec<Sexp>) -> Vec<u8> {
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

    out.bytes
}

/// Restores what [`save_state`] encoded.
unsafe fn restore_state(
    state: &[u8],
    roots: &[Sexp],
    relocation: &Relocation,
) -> Result<(), ImageError> {
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
        let (path, _) = relocation.apply(input.blob()?, 0);
        libraries.push((package, text(&path)?));
    }
    interp().builtin_libraries = libraries;

    if input.u8()? != 0 {
        data_table::set_namespace(root(&mut input)?);
    }

    if !input.bytes.is_empty() {
        return Err(invalid("the session state has bytes past its end"));
    }
    Ok(())
}
