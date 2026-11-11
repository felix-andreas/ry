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
    crate::{builtin_libraries, bytecode, dispatch, format, heap, interp, unwind::R, values::nil},
    heap::{
        Fixed, ImageError, Relocation,
        image::{ImageOrder, Reader, Writer},
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
    /// Builds the environment the host attaches to every session once base R has started, which
    /// the image carries built so that a start from it skips building it.
    pub environment: unsafe fn() -> R<Sexp>,
}

/// The environment the start-up image carried for the host ([`ImageHost::environment`]), or
/// `None` when the session started without an image.
///
/// # Safety
/// Called on the interpreter's thread once the session has started.
pub unsafe fn host_environment() -> Option<Sexp> {
    let environment = interp().globals.host_environment;
    (environment.kind() == SexpType::Environment).then_some(environment)
}

/// The start-up image's format stamp. Its last byte goes up whenever what an image holds or
/// how it lays it out changes, so that an image from an older format reads as stale and is
/// written again rather than misread.
pub const MAGIC: &[u8; 8] = b"RYSTART\x06";

/// Whether the file at `path` is a start-up image the runtime build `identity` loads: one of this
/// format that the same build wrote. A missing or unreadable file is not.
pub fn is_current(path: &Path, identity: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    // The magic, the header's length, then the header, which opens with the writer's identity
    // as a length and its bytes.
    let mut prefix = [0u8; 24];
    if read_at(&file, &mut prefix, 0).is_err() || &prefix[..MAGIC.len()] != MAGIC {
        return false;
    }
    let mut length = [0u8; 8];
    length.copy_from_slice(&prefix[16..]);
    if u64::from_le_bytes(length) != identity.len() as u64 {
        return false;
    }
    let mut written_by = vec![0u8; identity.len()];
    read_at(&file, &mut written_by, prefix.len() as u64).is_ok()
        && written_by == identity.as_bytes()
}

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

/// Reports on standard error, when `RY_IMAGE_LOG` is set, why a start-up or code image built
/// for this R home went unused, so a benchmark can refuse a run that measured a slow start
/// (`tools/bench/bench.py`). Images are tied to the build that wrote them, so two builds sharing
/// one R home differ by up to a sixth of their start-up cost without a visible sign otherwise.
/// A deliberate opt-out ([`applies`], `RY_CODE_IMAGE=0`) and a package installed without a code
/// image are not reported: neither is a build mismatch.
pub(crate) fn report_unused(reason: &str) {
    static LOGGING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *LOGGING
        .get_or_init(|| std::env::var_os("RY_IMAGE_LOG").is_some_and(|value| !value.is_empty()))
    {
        crate::write_console(format!("ry: image unused: {reason}\n").as_bytes(), true);
    }
}

/// Why an image did not load.
pub(crate) enum LoadError {
    /// The home has no image for this build: there is none, or another build of the runtime or
    /// an older format wrote it. The start goes the slow way without a word, unless `RY_IMAGE_LOG` asks.
    Absent(String),
    /// The image is unreadable or damaged; nothing changed yet, so the start can go the slow way.
    Untouched(String),
    /// The image failed partway, after the heap took some of it, so the session cannot start.
    Partial(String),
}

/// Loads the image of the R home `home`. On any error but [`LoadError::Partial`] the caller
/// starts the slow way.
pub(crate) unsafe fn load(host: &ImageHost, home: &str) -> Result<(), LoadError> {
    let path = &path(home);
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            return Err(LoadError::Absent(format!(
                "the start-up image {} does not exist",
                path.display()
            )));
        }
        Err(failure) => {
            return Err(LoadError::Untouched(format!(
                "cannot read {}: {failure}",
                path.display()
            )));
        }
    };
    let cannot_read = |failure: std::io::Error| {
        LoadError::Untouched(format!("cannot read {}: {failure}", path.display()))
    };
    let not_an_image =
        || LoadError::Untouched(format!("{} is not a start-up image", path.display()));
    let mut prefix = [0u8; 16];
    read_at(&file, &mut prefix, 0).map_err(cannot_read)?;
    if &prefix[..MAGIC.len()] != MAGIC {
        // An image of an older format is stale like one another build wrote: the start is slow
        // until the home is assembled again, which writes it anew.
        if prefix[..MAGIC.len() - 1] == MAGIC[..MAGIC.len() - 1] {
            return Err(LoadError::Absent(format!(
                "the start-up image {} has an older file format",
                path.display()
            )));
        }
        return Err(not_an_image());
    }
    let mut length = [0u8; 8];
    length.copy_from_slice(&prefix[MAGIC.len()..]);
    let header_length = usize::try_from(u64::from_le_bytes(length)).map_err(|_| not_an_image())?;
    let file_length = file.metadata().map_err(cannot_read)?.len();
    if header_length as u64 > file_length {
        return Err(not_an_image());
    }
    let mut header = vec![0u8; header_length];
    read_at(&file, &mut header, prefix.len() as u64).map_err(cannot_read)?;
    let mut input = Reader { bytes: &header };
    let untouched = |error: ImageError| LoadError::Untouched(error.to_string());
    // A development build changes its identity with every edit to the runtime, until the base
    // tree is rebuilt; that start is simply slow.
    if input.blob().map_err(untouched)? != host.identity.as_bytes() {
        return Err(LoadError::Absent(format!(
            "the start-up image {} was written by another build of the runtime",
            path.display()
        )));
    }
    let expected = input.u64().map_err(untouched)?;
    let damaged = || LoadError::Untouched(format!("{} is damaged", path.display()));
    if checksum(input.bytes) != expected {
        return Err(damaged());
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
    let metadata = input.blob().map_err(untouched)?;
    let state = input.blob().map_err(untouched)?;
    let pages_length = input.u64().map_err(untouched)?;
    let large_length = input.u64().map_err(untouched)?;
    if !input.bytes.is_empty() {
        return Err(damaged());
    }
    let pages_offset = section_start(prefix.len() as u64 + header_length as u64);
    let large_offset = section_start(pages_offset + pages_length);
    if large_offset + large_length > file_length {
        return Err(damaged());
    }
    let host_objects = (host.fixed_objects)().map_err(LoadError::Untouched)?;
    let primitives = primitives();
    let fixed = Fixed {
        primitives: &primitives,
        extra: &extra_objects(&host_objects),
    };
    let fill = |offset: u64, length: u64| {
        let file = &file;
        move |memory: &mut [u8]| -> Result<(), ImageError> {
            if memory.len() as u64 != length {
                return Err(ImageError::Invalid(
                    "its pages differ in size from their memory".to_owned(),
                ));
            }
            fill_from(file, memory, offset)
        }
    };
    let roots = heap()
        .load_image(
            metadata,
            &fixed,
            &relocation,
            &mut |token| builtin_libraries::routine_address(token),
            &mut fill(pages_offset, pages_length),
            &mut fill(large_offset, large_length),
        )
        .map_err(|error| match error {
            ImageError::Mismatch(_) => LoadError::Untouched(error.to_string()),
            _ => LoadError::Partial(error.to_string()),
        })?;
    touch::record(&heap().image_memory(), expected);
    restore_state(state, &roots, &relocation)
        .map_err(|error| LoadError::Partial(error.to_string()))?;
    Ok(())
}

/// Where a run of pages starts in the image file: the next 64 KiB boundary, so that the pages
/// can be mapped from the file as well as read.
fn section_start(offset: u64) -> u64 {
    offset.next_multiple_of(SECTION_ALIGNMENT)
}

const SECTION_ALIGNMENT: u64 = 64 * 1024;

/// Puts the bytes of `file` at `offset` into `memory`, which lies inside the heap's
/// reservation. On Unix the file is mapped privately over `memory`: pages that start-up never
/// writes stay the page cache's, shared by every process that loads the image, and a page is
/// copied only when it is first written. On an empty start that halves the image's share of
/// memory across concurrent workers and costs less than reading, because reading first has the
/// kernel zero the huge pages it then overwrites. The writer replaces the file by renaming, so a
/// mapped image never shrinks under a running process.
fn fill_from(file: &std::fs::File, memory: &mut [u8], offset: u64) -> Result<(), ImageError> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if memory.is_empty() {
            return Ok(());
        }
        let offset = libc::off_t::try_from(offset)
            .map_err(|_| ImageError::Invalid("its pages lie past the end of a file".to_owned()))?;
        // SAFETY: `memory` is committed address space the heap owns and holds no live object
        // while the load fills it; a fixed private mapping replaces it in place.
        let mapped = unsafe {
            libc::mmap(
                memory.as_mut_ptr().cast(),
                memory.len(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_FIXED,
                file.as_raw_fd(),
                offset,
            )
        };
        if mapped == libc::MAP_FAILED {
            return Err(ImageError::Invalid(format!(
                "its pages cannot be mapped: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        read_at(file, memory, offset)
            .map_err(|failure| ImageError::Invalid(format!("its pages cannot be read: {failure}")))
    }
}

/// Fills `memory` with the bytes of `file` at `offset`.
fn read_at(file: &std::fs::File, memory: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(file, memory, offset)
    }
    #[cfg(windows)]
    {
        let mut done = 0;
        while done < memory.len() {
            let read = std::os::windows::fs::FileExt::seek_read(
                file,
                &mut memory[done..],
                offset + done as u64,
            )?;
            if read == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += read;
        }
        Ok(())
    }
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

/// A checksum of an image's header, so that a damaged file is rejected before the heap takes
/// any of it rather than failing partway. The pages are left out: reading them twice would cost
/// as much as loading them.
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

/// The heap's state before start-up, taken before the start the writer images: the record
/// layout counts, since a layout start-up registers would not exist in the loading process, and
/// the digest a loading heap must match ([`heap::Heap::image_precondition`]).
pub(crate) struct Before {
    layouts: (usize, usize),
    precondition: u64,
}

/// Takes the state [`Before`] describes. It registers the host's fixed objects first, as a load
/// does before it compares the digest.
pub(crate) unsafe fn before(host: &ImageHost) -> Result<Before, String> {
    let host_objects = (host.fixed_objects)()?;
    let primitives = primitives();
    let fixed = Fixed {
        primitives: &primitives,
        extra: &extra_objects(&host_objects),
    };
    let precondition = heap().image_precondition(&fixed);
    heap().pin_for_image();
    Ok(Before {
        layouts: heap().layouts.counts(),
        precondition,
    })
}

/// Writes the image of the session start-up just finished in the R home `home` to `path`. The
/// file is the signature, the header's length, the header, and then the heap's pages and its
/// large objects, each from a 64 KiB boundary.
///
/// With a `layout` directory, the heap is laid out in the order of the stable ids its `order`
/// file lists, one decimal id per line, when it has one, and the image's ids are written to its
/// `ids` file ([`heap::HeapImage::ids`]), so that a touch profile recorded against this image
/// can order the next one.
pub(crate) unsafe fn write(
    host: &ImageHost,
    home: &str,
    path: &Path,
    layout: Option<&Path>,
    before: &Before,
) -> Result<(), String> {
    let order = match layout.map(|layout| layout.join("order")) {
        Some(file) if file.exists() => Some(read_order(&file)?),
        _ => None,
    };
    // Compiling comes before `check_empty`, so the check also covers any interpreter state a
    // rejected body could leave behind. Compiling runs no R code and adds no record layouts, so
    // it cannot change the allocation state the check compares against `before`.
    let mut fresh = bytecode::compile_namespaces_for_image()
        .map_err(|error| format!("compiling the namespaces' closures failed: {error:?}"))?;
    fresh.sort_unstable_by_key(|code| code.as_ptr() as usize);
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
    let mut homes = vec![home.to_owned()];
    if let Ok(canonical) = std::fs::canonicalize(home)
        && r_path(&canonical) != home
    {
        homes.push(r_path(&canonical));
    }
    let prefixes: Vec<Vec<u8>> = homes.iter().map(|home| home.as_bytes().to_vec()).collect();
    let image = heap()
        .write_image(
            &fixed,
            before.precondition,
            &roots,
            order.as_ref().map(|(numbering, ids)| ImageOrder {
                numbering: *numbering,
                ids,
            }),
            &prefixes,
            &|object| {
                (object.kind() == SexpType::Bytecode
                    && fresh
                        .binary_search_by_key(&(object.as_ptr() as usize), |code| {
                            code.as_ptr() as usize
                        })
                        .is_err())
                .then(|| bytecode::source_of(object))
            },
            &mut |pointer| builtin_libraries::routine_token(pointer),
        )
        .map_err(|error| error.to_string())?;
    let mut payload = Writer::default();
    payload.u32(homes.len() as u32);
    for home in &homes {
        payload.blob(home.as_bytes());
    }
    payload.blob(&image.metadata);
    payload.blob(&state);
    payload.u64(image.pages().len() as u64);
    payload.u64(image.large().len() as u64);
    let mut header = Writer::default();
    header.blob(host.identity.as_bytes());
    let image_checksum = checksum(&payload.bytes);
    header.u64(image_checksum);
    header.bytes.extend_from_slice(&payload.bytes);
    let mut out = Writer::default();
    out.bytes.extend_from_slice(MAGIC);
    out.u64(header.bytes.len() as u64);
    out.bytes.extend_from_slice(&header.bytes);
    let pages_offset = section_start(out.bytes.len() as u64);
    out.bytes.resize(pages_offset as usize, 0);
    out.bytes.extend_from_slice(image.pages());
    let large_offset = section_start(out.bytes.len() as u64);
    out.bytes.resize(large_offset as usize, 0);
    out.bytes.extend_from_slice(image.large());
    // A process that has the old image mapped keeps its pages: renaming replaces the name,
    // where writing in place would truncate the mapped file under it.
    let partial = path.with_extension("image.partial");
    let cannot_write =
        |failure: std::io::Error| format!("cannot write {}: {failure}", path.display());
    std::fs::write(&partial, &out.bytes).map_err(cannot_write)?;
    std::fs::rename(&partial, path).map_err(cannot_write)?;
    if let Some(layout) = layout {
        // The header ties the ids to this image, which touch profiles name by its checksum,
        // and to the numbering, which the next write's order must match.
        let mut ids = Vec::with_capacity(16 + image.ids().len());
        ids.extend_from_slice(&image_checksum.to_le_bytes());
        ids.extend_from_slice(&image.numbering().to_le_bytes());
        ids.extend_from_slice(image.ids());
        let file = layout.join("ids");
        std::fs::write(&file, ids)
            .map_err(|failure| format!("cannot write {}: {failure}", file.display()))?;
    }
    Ok(())
}

/// Reads a layout order (`tools/layout/image_order.py`): a first line `numbering HEX` naming
/// the numbering its ids belong to, then one decimal id per line.
fn read_order(file: &Path) -> Result<(u64, Vec<u32>), String> {
    let text = std::fs::read_to_string(file)
        .map_err(|failure| format!("cannot read {}: {failure}", file.display()))?;
    let mut lines = text.lines();
    let numbering = lines
        .next()
        .and_then(|line| line.strip_prefix("numbering "))
        .and_then(|digest| u64::from_str_radix(digest.trim(), 16).ok())
        .ok_or_else(|| format!("{} does not start with a `numbering` line", file.display()))?;
    let ids = lines
        .map(|line| {
            line.trim()
                .parse()
                .map_err(|_| format!("{} lists `{line}`, which is not an id", file.display()))
        })
        .collect::<Result<_, _>>()?;
    Ok((numbering, ids))
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
    out.u32(root(globals.host_environment));
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
    globals.host_environment = root(&mut input)?;
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

    if !input.bytes.is_empty() {
        return Err(invalid("the session state has bytes past its end"));
    }
    Ok(())
}

/// The bytes of a file of compiled code (`crate::code_image`). On Unix the file is mapped
/// read-only, which shares the page cache's pages instead of filling a private copy of each.
pub(crate) enum ImageFile {
    #[cfg(unix)]
    Mapped {
        start: *mut libc::c_void,
        length: usize,
    },
    Read(Vec<u8>),
}

impl ImageFile {
    pub(crate) fn open(path: &Path) -> std::io::Result<ImageFile> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let file = std::fs::File::open(path)?;
            let length = usize::try_from(file.metadata()?.len())
                .map_err(|_| std::io::Error::other("the image does not fit in memory"))?;
            if length == 0 {
                return Ok(ImageFile::Read(Vec::new()));
            }
            // SAFETY: a read-only private mapping of an open file. The base tree is build
            // output that nothing rewrites in place, so the bytes do not change under the map.
            let start = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE,
                    file.as_raw_fd(),
                    0,
                )
            };
            if start == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            Ok(ImageFile::Mapped { start, length })
        }
        #[cfg(not(unix))]
        {
            std::fs::read(path).map(ImageFile::Read)
        }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            // SAFETY: the mapping is `length` readable bytes and lives until `self` drops.
            #[cfg(unix)]
            ImageFile::Mapped { start, length } => unsafe {
                std::slice::from_raw_parts(start.cast::<u8>(), *length)
            },
            ImageFile::Read(bytes) => bytes,
        }
    }
}

impl Drop for ImageFile {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let ImageFile::Mapped { start, length } = *self {
            // SAFETY: the mapping was made by `open` and nothing borrows it past `self`.
            if unsafe { libc::munmap(start, length) } != 0 {
                panic!(
                    "cannot unmap an image file: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

/// The touch recorder behind `RY_IMAGE_TOUCH=FILE`, which lays out the next image
/// (`tools/layout/image_order.py`). It protects the loaded image's memory and records, in a
/// fault handler, the first touch of each 16-byte granule with the phase of the session it came
/// in: 0 while the session starts, 1 while a namespace loads after that, and 2 otherwise. After
/// each fault the page is opened for one instruction and closed again, so every granule's first
/// touch faults, until a page has faulted [`PAGE_FAULT_BUDGET`] times and stays open, which
/// bounds the cost of a page touched in a loop. At exit, `FILE` gets one line per first touch, in
/// order: the address in hexadecimal and the phase.
///
/// A system call that reads into protected memory fails instead of faulting, so a profiled
/// run can differ from a plain one; the profile is a training input, never a correctness
/// one. It needs single-stepping, so it exists on x86-64 Linux only.
pub(crate) mod touch {
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

    static STARTED: AtomicBool = AtomicBool::new(false);
    static LOADS: AtomicU32 = AtomicU32::new(0);
    static PHASE: AtomicU8 = AtomicU8::new(0);

    /// The phase from here on is the run's, outside namespace loads.
    pub(crate) fn finish_start_up() {
        STARTED.store(true, Ordering::Relaxed);
        update();
    }

    pub(crate) fn enter_namespace_load() {
        LOADS.fetch_add(1, Ordering::Relaxed);
        update();
    }

    pub(crate) fn leave_namespace_load() {
        LOADS.fetch_sub(1, Ordering::Relaxed);
        update();
    }

    fn update() {
        let phase = match (
            STARTED.load(Ordering::Relaxed),
            LOADS.load(Ordering::Relaxed),
        ) {
            (false, _) => 0,
            (true, 0) => 2,
            (true, _) => 1,
        };
        PHASE.store(phase, Ordering::Relaxed);
    }

    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    pub(crate) fn record(_memory: &[std::ops::Range<usize>; 2], _image: u64) {}

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub(crate) use recorder::record;

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    const PAGE_FAULT_BUDGET: u16 = 512;

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    mod recorder {
        use {
            super::{PAGE_FAULT_BUDGET, PHASE},
            std::{ffi::c_void, ops::Range, path::PathBuf, sync::atomic::Ordering},
        };

        const SYSTEM_PAGE: usize = 4096;
        const GRANULE: usize = 16;
        /// The x86 trap flag, which makes the processor trap after the next instruction.
        const TRAP_FLAG: i64 = 0x100;

        /// What the handlers read. A signal handler cannot take a lock, and the session is the
        /// only thread that touches the heap, so this lives in a static the handlers alone
        /// mutate once it is armed.
        struct Recorder {
            regions: [Range<usize>; 2],
            /// One bit per granule of the regions, laid end to end.
            seen: Vec<u64>,
            faults: Vec<u16>,
            /// First touches as (address, phase), with room reserved up front, so the handler
            /// never allocates.
            touches: Vec<(usize, u8)>,
            /// The page opened for the instruction that faulted.
            open: Option<usize>,
            previous_segv: libc::sigaction,
            output: PathBuf,
            /// The loaded image's header checksum, which names it in the profile.
            image: u64,
        }

        static mut RECORDER: Option<Recorder> = None;

        pub(crate) fn record(memory: &[Range<usize>; 2], image: u64) {
            let Some(output) = std::env::var_os("RY_IMAGE_TOUCH") else {
                return;
            };
            let granules: usize = memory.iter().map(|range| range.len() / GRANULE).sum();
            let pages: usize = memory.iter().map(|range| range.len() / SYSTEM_PAGE).sum();
            // SAFETY: the session's thread arms the recorder once, before any handler can run.
            unsafe {
                let recorder = &mut *std::ptr::addr_of_mut!(RECORDER);
                *recorder = Some(Recorder {
                    regions: memory.clone(),
                    seen: vec![0; granules.div_ceil(64)],
                    faults: vec![0; pages],
                    touches: Vec::with_capacity(granules),
                    open: None,
                    previous_segv: std::mem::zeroed(),
                    output: PathBuf::from(output),
                    image,
                });
                let Some(state) = recorder.as_mut() else {
                    return;
                };
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = segv as *const () as usize;
                action.sa_flags = libc::SA_SIGINFO;
                libc::sigaction(libc::SIGSEGV, &action, &mut state.previous_segv);
                let mut trap: libc::sigaction = std::mem::zeroed();
                trap.sa_sigaction = step as *const () as usize;
                trap.sa_flags = libc::SA_SIGINFO;
                libc::sigaction(libc::SIGTRAP, &trap, std::ptr::null_mut());
                libc::atexit(write);
                for region in &state.regions {
                    if !region.is_empty() {
                        libc::mprotect(region.start as *mut c_void, region.len(), libc::PROT_NONE);
                    }
                }
            }
        }

        /// The granule and system page indexes of `address`, counting through both regions.
        fn locate(regions: &[Range<usize>; 2], address: usize) -> Option<(usize, usize)> {
            let mut granules = 0;
            let mut pages = 0;
            for region in regions {
                if region.contains(&address) {
                    let offset = address - region.start;
                    return Some((granules + offset / GRANULE, pages + offset / SYSTEM_PAGE));
                }
                granules += region.len() / GRANULE;
                pages += region.len() / SYSTEM_PAGE;
            }
            None
        }

        extern "C" fn segv(_: i32, info: *mut libc::siginfo_t, context: *mut c_void) {
            // SAFETY: the kernel passes a valid siginfo and context, and the recorder is armed.
            unsafe {
                let Some(state) = (*std::ptr::addr_of_mut!(RECORDER)).as_mut() else {
                    return;
                };
                let address = (*info).si_addr() as usize;
                let Some((granule, page)) = locate(&state.regions, address) else {
                    // Not ours: the previous handler takes the fault when it repeats.
                    libc::sigaction(libc::SIGSEGV, &state.previous_segv, std::ptr::null_mut());
                    return;
                };
                let bit = 1 << (granule % 64);
                if state.seen[granule / 64] & bit == 0 {
                    state.seen[granule / 64] |= bit;
                    if state.touches.len() < state.touches.capacity() {
                        state.touches.push((address, PHASE.load(Ordering::Relaxed)));
                    }
                }
                let start = address & !(SYSTEM_PAGE - 1);
                libc::mprotect(
                    start as *mut c_void,
                    SYSTEM_PAGE,
                    libc::PROT_READ | libc::PROT_WRITE,
                );
                state.faults[page] = state.faults[page].saturating_add(1);
                if state.faults[page] < PAGE_FAULT_BUDGET {
                    state.open = Some(start);
                    let context = context.cast::<libc::ucontext_t>();
                    (*context).uc_mcontext.gregs[libc::REG_EFL as usize] |= TRAP_FLAG;
                }
            }
        }

        extern "C" fn step(_: i32, _: *mut libc::siginfo_t, context: *mut c_void) {
            // SAFETY: as in `segv`.
            unsafe {
                if let Some(state) = (*std::ptr::addr_of_mut!(RECORDER)).as_mut()
                    && let Some(start) = state.open.take()
                {
                    libc::mprotect(start as *mut c_void, SYSTEM_PAGE, libc::PROT_NONE);
                }
                let context = context.cast::<libc::ucontext_t>();
                (*context).uc_mcontext.gregs[libc::REG_EFL as usize] &= !TRAP_FLAG;
            }
        }

        extern "C" fn write() {
            // SAFETY: exit runs on one thread, after the session's last touch.
            unsafe {
                let Some(state) = (*std::ptr::addr_of_mut!(RECORDER)).take() else {
                    return;
                };
                for region in &state.regions {
                    if !region.is_empty() {
                        libc::mprotect(
                            region.start as *mut c_void,
                            region.len(),
                            libc::PROT_READ | libc::PROT_WRITE,
                        );
                    }
                }
                let mut text = String::with_capacity(state.touches.len() * 16 + 32);
                text.push_str(&format!("image {:016x}\n", state.image));
                for &(address, phase) in &state.touches {
                    text.push_str(&format!("{address:x} {phase}\n"));
                }
                if let Err(failure) = std::fs::write(&state.output, text) {
                    eprintln!("ry: cannot write {}: {failure}", state.output.display());
                }
            }
        }
    }
}
