//! Connections: R's streams, as the C API exposes them through `struct Rconn` and as the R
//! functions `file()`, `readLines()`, `writeBin()` and friends use them.
//!
//! A connection lives in a slot of a fixed table of [`NCONNECTIONS`], as in GNU R: slots 0, 1
//! and 2 are stdin, stdout and stderr, and the R-level connection object is an integer vector
//! holding the slot number, classed `c(<class>, "connection")`. Every connection but the
//! standard three also carries a `conn_id` attribute, an external pointer whose address is the
//! slot's `Rconn`. Closing a connection frees its slot and clears that address, so an R object
//! that outlives its connection reads as an invalid connection instead of dangling.
//!
//! Every connection is driven through the method pointers of its `Rconn`, because packages fill
//! them in for custom connections (curl, processx) and call them directly on any connection
//! (vroom). ry's built-in kinds install Rust methods whose state sits behind `private`. Calling
//! a method from Rust goes through a C trampoline in `native.c` unless the pointer is ry's own
//! method, which then runs directly: package code is never called from a Rust frame, and the
//! common path pays no context switch.

use crate::{
    R_xlen_t, Rboolean,
    attributes::{inherits, raw_attribute, set_attrib},
    boundary::{ApiResult, enter, error, error_in, warn, with_native_scope},
    environments::define_var,
    globals::{R_ClassSymbol, R_NamesSymbol},
    memory::{alloc_vector, scalar},
    runtime,
    strings::utf8_text,
};
use flate2::{Compression, read::MultiGzDecoder, write::GzEncoder};
use heap::{
    object::{NA_INTEGER, Rcomplex, Sexp, SexpType, is_na_real},
    strings::Encoding,
};
use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    fs::{File, OpenOptions},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    ptr::null_mut,
};

/// The version of the C interface in `R_ext/Connections.h`.
pub const R_CONNECTIONS_VERSION: c_int = 1;
/// How many connections can exist at once, the three standard ones included.
pub const NCONNECTIONS: usize = 128;

/// GNU R's `struct Rconn`, member for member.
#[repr(C)]
pub struct Rconn {
    pub class: *mut c_char,
    pub description: *mut c_char,
    pub enc: c_int,
    pub mode: [c_char; 5],
    pub text: Rboolean,
    pub isopen: Rboolean,
    pub incomplete: Rboolean,
    pub canread: Rboolean,
    pub canwrite: Rboolean,
    pub canseek: Rboolean,
    pub blocking: Rboolean,
    pub isGzcon: Rboolean,
    pub open: Option<OpenMethod>,
    pub close: Option<VoidMethod>,
    pub destroy: Option<VoidMethod>,
    /// Takes a `va_list`, whose representation differs between platforms, so Rust never calls
    /// it and keeps it opaque; `native.c` casts it back.
    pub vfprintf: Option<OpaqueMethod>,
    pub fgetc: Option<IntMethod>,
    pub fgetc_internal: Option<IntMethod>,
    pub seek: Option<SeekMethod>,
    pub truncate: Option<VoidMethod>,
    pub fflush: Option<IntMethod>,
    pub read: Option<ReadMethod>,
    pub write: Option<WriteMethod>,
    pub nPushBack: c_int,
    pub posPushBack: c_int,
    pub PushBack: *mut *mut c_char,
    pub save: c_int,
    pub save2: c_int,
    pub encname: [c_char; 101],
    pub inconv: *mut c_void,
    pub outconv: *mut c_void,
    pub iconvbuff: [c_char; 25],
    pub oconvbuff: [c_char; 50],
    pub next: *mut c_char,
    pub init_out: [c_char; 25],
    pub navail: i16,
    pub inavail: i16,
    pub EOF_signalled: Rboolean,
    pub UTF8out: Rboolean,
    pub id: *mut c_void,
    pub ex_ptr: *mut c_void,
    pub private: *mut c_void,
    pub status: c_int,
    pub buff: *mut u8,
    pub buff_len: usize,
    pub buff_stored_len: usize,
    pub buff_pos: usize,
}

pub type Rconnection = *mut Rconn;
pub type OpenMethod = unsafe extern "C" fn(Rconnection) -> Rboolean;
pub type VoidMethod = unsafe extern "C" fn(Rconnection);
pub type OpaqueMethod = unsafe extern "C" fn();
pub type IntMethod = unsafe extern "C" fn(Rconnection) -> c_int;
pub type SeekMethod = unsafe extern "C" fn(Rconnection, f64, c_int, c_int) -> f64;
pub type ReadMethod = unsafe extern "C" fn(*mut c_void, usize, usize, Rconnection) -> usize;
pub type WriteMethod = unsafe extern "C" fn(*const c_void, usize, usize, Rconnection) -> usize;

/// `save` and `save2` hold this when they hold no character.
const NO_SAVE: c_int = -1000;
const EOF: c_int = -1;
const READ_CHUNK: usize = 64 * 1024;

/// Which position `seek` reports and moves, as `seek(rw = )` chooses it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeekSide {
    LastUsed = 0,
    Read = 1,
    Write = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeekOrigin {
    Start = 1,
    Current = 2,
    End = 3,
}

/// A text connection's target variable: each completed line updates it.
#[derive(Clone, Copy)]
pub struct TextTarget {
    pub symbol: Sexp,
    pub env: Sexp,
}

/// The element types `readBin` and `writeBin` move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinaryType {
    Integer,
    Double,
    Logical,
    Complex,
    Character,
    Raw,
}

unsafe extern "C" {
    fn ry_connection_vfprintf();
    fn ry_connection_open(method: OpenMethod, con: Rconnection, result: *mut c_int) -> c_int;
    fn ry_connection_void(method: VoidMethod, con: Rconnection) -> c_int;
    fn ry_connection_int(method: IntMethod, con: Rconnection, result: *mut c_int) -> c_int;
    fn ry_connection_seek(
        method: SeekMethod,
        con: Rconnection,
        position: f64,
        origin: c_int,
        rw: c_int,
        result: *mut f64,
    ) -> c_int;
    fn ry_connection_read(
        method: ReadMethod,
        buffer: *mut c_void,
        size: usize,
        count: usize,
        con: Rconnection,
        result: *mut usize,
    ) -> c_int;
    fn ry_connection_write(
        method: WriteMethod,
        buffer: *const c_void,
        size: usize,
        count: usize,
        con: Rconnection,
        result: *mut usize,
    ) -> c_int;
    fn ry_connection_print(
        method: OpaqueMethod,
        con: Rconnection,
        text: *const c_char,
        result: *mut c_int,
    ) -> c_int;
}

struct Entry {
    con: Rconnection,
    /// Whether `private` holds a ry `Device`. A custom connection's `private` is the package's.
    builtin: bool,
}

static TABLE: crate::Global<Option<Vec<Option<Entry>>>> = crate::Global::new(None);

fn table() -> &'static mut Vec<Option<Entry>> {
    // SAFETY: single-threaded access, as for the runtime.
    let slot = unsafe { &mut *TABLE.as_ptr() };
    slot.get_or_insert_with(|| {
        let mut entries: Vec<Option<Entry>> = (0..NCONNECTIONS).map(|_| None).collect();
        for (index, (stream, description, mode)) in [
            (Standard::Input, "stdin", "r"),
            (Standard::Output, "stdout", "w"),
            (Standard::Error, "stderr", "w"),
        ]
        .into_iter()
        .enumerate()
        {
            // SAFETY: allocating a fresh struct touches no R object.
            let con = unsafe {
                let con = allocate_rconn(
                    b"terminal",
                    description.as_bytes(),
                    mode.as_bytes(),
                    &BUILTIN_METHODS,
                );
                (*con).private = Box::into_raw(Box::new(Device::Standard(stream))).cast();
                (*con).isopen = 1;
                (*con).canread = (stream == Standard::Input) as Rboolean;
                (*con).canwrite = (stream != Standard::Input) as Rboolean;
                (*con).blocking = 1;
                con
            };
            entries[index] = Some(Entry { con, builtin: true });
        }
        entries
    })
}

fn entry(slot: usize) -> Option<&'static Entry> {
    table().get(slot).and_then(Option::as_ref)
}

/// The slot an R connection object names, after checking it is live. `not_connection` is the
/// message for an object that is not classed as a connection at all.
unsafe fn resolve(object: Sexp, not_connection: &str) -> ApiResult<(usize, Rconnection)> {
    if !inherits(object, "connection")? {
        return Err(error(not_connection));
    }
    let invalid = || error("invalid connection");
    let slot = match object.kind() {
        SexpType::Integer if object.length() >= 1 => {
            *object.slice::<i32>().first().ok_or_else(invalid)?
        }
        SexpType::Real if object.length() >= 1 => {
            *object.slice::<f64>().first().ok_or_else(invalid)? as i32
        }
        _ => return Err(invalid()),
    };
    let entry = usize::try_from(slot)
        .ok()
        .and_then(entry)
        .ok_or_else(invalid)?;
    let id = raw_attribute(object, install("conn_id")?);
    if id.kind() == SexpType::ExternalPtr && id.external_address() != entry.con.cast() {
        return Err(invalid());
    }
    Ok((slot as usize, entry.con))
}

unsafe fn resolve_con(object: Sexp) -> ApiResult<(usize, Rconnection)> {
    resolve(object, "'con' is not a connection")
}

unsafe fn install(name: &str) -> ApiResult<Sexp> {
    Ok(runtime().heap.install(name.as_bytes())?)
}

/// Takes a free slot for `con`, failing as GNU R does when the table is full.
fn claim_slot(con: Rconnection, builtin: bool) -> ApiResult<usize> {
    let table = table();
    let slot = (3..NCONNECTIONS)
        .find(|&slot| table[slot].is_none())
        .ok_or_else(|| error("all connections are in use"))?;
    table[slot] = Some(Entry { con, builtin });
    Ok(slot)
}

struct Methods {
    open: OpenMethod,
    close: VoidMethod,
    destroy: VoidMethod,
    fgetc: IntMethod,
    fgetc_internal: IntMethod,
    seek: SeekMethod,
    truncate: VoidMethod,
    fflush: IntMethod,
    read: ReadMethod,
    write: WriteMethod,
}

static BUILTIN_METHODS: Methods = Methods {
    open: builtin_open,
    close: builtin_close,
    destroy: builtin_destroy,
    fgetc: builtin_fgetc,
    fgetc_internal: builtin_fgetc,
    seek: builtin_seek,
    truncate: builtin_truncate,
    fflush: builtin_fflush,
    read: builtin_read,
    write: builtin_write,
};

/// What `R_new_custom_connection` installs: each method reports that it is not enabled, except
/// that `fgetc` reads through `fgetc_internal`, the method packages actually supply.
static CUSTOM_METHODS: Methods = Methods {
    open: null_open,
    close: null_close,
    destroy: null_destroy,
    fgetc: custom_fgetc,
    fgetc_internal: null_fgetc,
    seek: null_seek,
    truncate: null_truncate,
    fflush: null_fflush,
    read: null_read,
    write: null_write,
};

/// A zeroed struct with the given names and methods, closed, reading and writing allowed, as
/// GNU R initializes one before its class opens it.
unsafe fn allocate_rconn(
    class: &[u8],
    description: &[u8],
    mode: &[u8],
    methods: &Methods,
) -> Rconnection {
    let con = Box::into_raw(Box::<Rconn>::new_zeroed().assume_init());
    (*con).class = malloc_string(class);
    (*con).description = malloc_string(description);
    set_mode(con, mode);
    (*con).text = 1;
    (*con).canread = 1;
    (*con).canwrite = 1;
    (*con).blocking = 1;
    (*con).open = Some(methods.open);
    (*con).close = Some(methods.close);
    (*con).destroy = Some(methods.destroy);
    (*con).vfprintf = Some(ry_connection_vfprintf);
    (*con).fgetc = Some(methods.fgetc);
    (*con).fgetc_internal = Some(methods.fgetc_internal);
    (*con).seek = Some(methods.seek);
    (*con).truncate = Some(methods.truncate);
    (*con).fflush = Some(methods.fflush);
    (*con).read = Some(methods.read);
    (*con).write = Some(methods.write);
    (*con).save = NO_SAVE;
    (*con).save2 = NO_SAVE;
    (*con).status = NA_INTEGER;
    for (target, &byte) in (*con).encname.iter_mut().zip(b"native.enc") {
        *target = byte as c_char;
    }
    con
}

/// The class and description are `malloc`ed, as in GNU R, because a package may replace one
/// with its own `malloc`ed string and the runtime frees whatever is there.
unsafe fn malloc_string(bytes: &[u8]) -> *mut c_char {
    let text = libc::malloc(bytes.len() + 1) as *mut c_char;
    if text.is_null() {
        panic!("out of memory allocating a connection string");
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), text.cast(), bytes.len());
    *text.add(bytes.len()) = 0;
    text
}

unsafe fn set_mode(con: Rconnection, mode: &[u8]) {
    (*con).mode = [0; 5];
    for (target, &byte) in (*con).mode.iter_mut().zip(mode.iter().take(4)) {
        *target = byte as c_char;
    }
}

unsafe fn mode_of(con: Rconnection) -> Vec<u8> {
    CStr::from_ptr((*con).mode.as_ptr()).to_bytes().to_vec()
}

unsafe fn description_of(con: Rconnection) -> String {
    String::from_utf8_lossy(c_text((*con).description)).into_owned()
}

unsafe fn class_of(con: Rconnection) -> String {
    String::from_utf8_lossy(c_text((*con).class)).into_owned()
}

unsafe fn c_text<'a>(text: *const c_char) -> &'a [u8] {
    if text.is_null() {
        &[]
    } else {
        CStr::from_ptr(text).to_bytes()
    }
}

/// The R object for a slot: the slot number, its classes, and the `conn_id` pointer.
unsafe fn connection_object(slot: usize) -> ApiResult<Sexp> {
    let con = entry(slot)
        .ok_or_else(|| error(format!("there is no connection {slot}")))?
        .con;
    let runtime = runtime();
    let object = scalar(SexpType::Integer, slot as c_int)?;
    let classes = alloc_vector(SexpType::String, 2)?;
    for (index, name) in [c_text((*con).class), b"connection"]
        .into_iter()
        .enumerate()
    {
        let charsxp = runtime.heap.mk_char(name, Encoding::Native)?;
        runtime.heap.set_element(classes, index, charsxp);
    }
    set_attrib(object, R_ClassSymbol.get(), classes)?;
    if let Some(id) = Sexp::from_raw((*con).ex_ptr.cast()) {
        set_attrib(object, install("conn_id")?, id)?;
    }
    Ok(object)
}

/// Registers a new connection and gives it its `conn_id`. A collection that finds the pointer
/// unreachable closes the connection, with GNU R's warning.
unsafe fn register(con: Rconnection, builtin: bool) -> ApiResult<Sexp> {
    let slot = match claim_slot(con, builtin) {
        Ok(slot) => slot,
        Err(unwind) => {
            free_rconn(con, builtin);
            return Err(unwind);
        }
    };
    let runtime = runtime();
    let nil = runtime.nil();
    let tag = install("connection")?;
    let id = runtime.heap.alloc_external_ptr(con.cast(), tag, nil)?;
    runtime.heap.root(id);
    runtime.heap.alloc_weak_ref(
        id,
        nil,
        nil,
        connection_finalizer as *const () as usize,
        false,
    )?;
    (*con).ex_ptr = id.as_ptr().cast();
    (*con).id = con.cast();
    connection_object(slot)
}

/// Runs when a connection's `conn_id` becomes unreachable while the connection is open.
unsafe extern "C" fn connection_finalizer(id: Sexp) {
    enter(|| {
        let address = id.external_address();
        if address.is_null() {
            return Ok(());
        }
        let found = (3..NCONNECTIONS)
            .find(|&slot| entry(slot).is_some_and(|entry| entry.con.cast() == address));
        if let Some(slot) = found {
            let con = address as Rconnection;
            warn(format!(
                "closing unused connection {slot} ({})",
                description_of(con)
            ));
            close_slot(slot)?;
        }
        Ok(())
    })
}

/// Closes and destroys the connection in `slot`, freeing the slot. Returns the close status.
unsafe fn close_slot(slot: usize) -> ApiResult<c_int> {
    let Some(Entry { con, builtin }) = table().get_mut(slot).and_then(Option::take) else {
        return Err(error("invalid connection"));
    };
    let closed = if (*con).isopen != 0 {
        method_close(con)
    } else {
        Ok(())
    };
    let status = (*con).status;
    let destroyed = method_destroy(con);
    if let Some(id) = Sexp::from_raw((*con).ex_ptr.cast()) {
        id.set_external_address(null_mut());
    }
    free_rconn(con, builtin);
    closed?;
    destroyed?;
    Ok(status)
}

unsafe fn free_rconn(con: Rconnection, builtin: bool) {
    clear_push_back_lines(con);
    if builtin && !(*con).private.is_null() {
        drop(Box::from_raw((*con).private as *mut Device));
    }
    libc::free((*con).class.cast());
    libc::free((*con).description.cast());
    drop(Box::from_raw(con));
}

fn is_method<F: Copy>(method: F, own: F) -> bool {
    // SAFETY: both are function pointers of the same type, compared by address.
    unsafe {
        std::mem::transmute_copy::<F, usize>(&method) == std::mem::transmute_copy::<F, usize>(&own)
    }
}

fn missing(what: &str) -> crate::boundary::Unwind {
    error(format!("'{what}' not enabled for this connection"))
}

unsafe fn method_open(con: Rconnection) -> ApiResult<bool> {
    let method = (*con)
        .open
        .ok_or_else(|| error("open not enabled for this connection"))?;
    if is_method(method, builtin_open as OpenMethod) {
        return device_open(con);
    }
    let mut result = 0;
    with_native_scope(|| ry_connection_open(method, con, &mut result))?;
    Ok(result != 0)
}

unsafe fn method_close(con: Rconnection) -> ApiResult<()> {
    match (*con).close {
        Some(method) if is_method(method, builtin_close as VoidMethod) => device_close(con),
        Some(method) => with_native_scope(|| ry_connection_void(method, con)),
        None => {
            (*con).isopen = 0;
            Ok(())
        }
    }
}

unsafe fn method_destroy(con: Rconnection) -> ApiResult<()> {
    match (*con).destroy {
        Some(method) if is_method(method, builtin_destroy as VoidMethod) => Ok(()),
        Some(method) => with_native_scope(|| ry_connection_void(method, con)),
        None => Ok(()),
    }
}

unsafe fn method_fgetc(con: Rconnection) -> ApiResult<c_int> {
    let method = (*con).fgetc.ok_or_else(|| missing("getc"))?;
    if is_method(method, builtin_fgetc as IntMethod) {
        return device_fgetc(con);
    }
    if is_method(method, custom_fgetc as IntMethod) {
        return method_fgetc_internal(con);
    }
    let mut result = 0;
    with_native_scope(|| ry_connection_int(method, con, &mut result))?;
    Ok(result)
}

unsafe fn method_fgetc_internal(con: Rconnection) -> ApiResult<c_int> {
    let method = (*con).fgetc_internal.ok_or_else(|| missing("getc"))?;
    if is_method(method, builtin_fgetc as IntMethod) {
        return device_fgetc(con);
    }
    let mut result = 0;
    with_native_scope(|| ry_connection_int(method, con, &mut result))?;
    Ok(result)
}

unsafe fn method_seek(
    con: Rconnection,
    position: f64,
    origin: SeekOrigin,
    side: SeekSide,
) -> ApiResult<f64> {
    let method = (*con).seek.ok_or_else(|| missing("seek"))?;
    if is_method(method, builtin_seek as SeekMethod) {
        return device_seek(con, position, origin, side);
    }
    let mut result = 0.0;
    with_native_scope(|| {
        ry_connection_seek(
            method,
            con,
            position,
            origin as c_int,
            side as c_int,
            &mut result,
        )
    })?;
    Ok(result)
}

unsafe fn method_truncate(con: Rconnection) -> ApiResult<()> {
    let method = (*con)
        .truncate
        .ok_or_else(|| error("truncation not enabled for this connection"))?;
    if is_method(method, builtin_truncate as VoidMethod) {
        return device_truncate(con);
    }
    with_native_scope(|| ry_connection_void(method, con))
}

unsafe fn method_fflush(con: Rconnection) -> ApiResult<c_int> {
    let Some(method) = (*con).fflush else {
        return Ok(0);
    };
    if is_method(method, builtin_fflush as IntMethod) {
        return device_flush(con).map(|()| 0);
    }
    let mut result = 0;
    with_native_scope(|| ry_connection_int(method, con, &mut result))?;
    Ok(result)
}

unsafe fn method_read(con: Rconnection, buffer: &mut [u8]) -> ApiResult<usize> {
    let method = (*con).read.ok_or_else(|| missing("read"))?;
    if is_method(method, builtin_read as ReadMethod) {
        return device_read(con, buffer);
    }
    let mut result = 0;
    with_native_scope(|| {
        ry_connection_read(
            method,
            buffer.as_mut_ptr().cast(),
            1,
            buffer.len(),
            con,
            &mut result,
        )
    })?;
    // A method that claims more than it was given would make every caller read past the end.
    Ok(result.min(buffer.len()))
}

unsafe fn method_write(con: Rconnection, bytes: &[u8]) -> ApiResult<usize> {
    let method = (*con).write.ok_or_else(|| missing("write"))?;
    if is_method(method, builtin_write as WriteMethod) {
        return device_write(con, bytes, false);
    }
    let mut result = 0;
    with_native_scope(|| {
        ry_connection_write(
            method,
            bytes.as_ptr().cast(),
            1,
            bytes.len(),
            con,
            &mut result,
        )
    })?;
    Ok(result)
}

/// Writes text the way `Rconn_printf` does: through `vfprintf`, which by default formats and
/// calls `write`, so the default goes straight to `write`. ry's own connections print through
/// the device, because a text output connection takes text but has no `write` method.
unsafe fn method_print(con: Rconnection, text: &[u8]) -> ApiResult<()> {
    match (*con).vfprintf {
        Some(method) if !is_method(method, ry_connection_vfprintf as OpaqueMethod) => {
            let text = CString::new(text)
                .map_err(|_| error("embedded nul in a string written to a connection"))?;
            let mut result = 0;
            with_native_scope(|| ry_connection_print(method, con, text.as_ptr(), &mut result))
        }
        _ => match (*con).write {
            Some(method) if is_method(method, builtin_write as WriteMethod) => {
                device_write(con, text, true).map(drop)
            }
            _ => method_write(con, text).map(drop),
        },
    }
}

unsafe extern "C" fn builtin_open(con: Rconnection) -> Rboolean {
    enter(|| device_open(con).map(Rboolean::from))
}

unsafe extern "C" fn builtin_close(con: Rconnection) {
    enter(|| device_close(con))
}

/// The device itself is dropped when the slot is freed, so destroy has nothing left to do.
unsafe extern "C" fn builtin_destroy(_con: Rconnection) {}

unsafe extern "C" fn builtin_fgetc(con: Rconnection) -> c_int {
    enter(|| device_fgetc(con))
}

unsafe extern "C" fn builtin_seek(
    con: Rconnection,
    position: f64,
    origin: c_int,
    rw: c_int,
) -> f64 {
    enter(|| device_seek(con, position, seek_origin(origin), seek_side(rw)))
}

unsafe extern "C" fn builtin_truncate(con: Rconnection) {
    enter(|| device_truncate(con))
}

unsafe extern "C" fn builtin_fflush(con: Rconnection) -> c_int {
    enter(|| device_flush(con).map(|()| 0))
}

unsafe extern "C" fn builtin_read(
    buffer: *mut c_void,
    size: usize,
    count: usize,
    con: Rconnection,
) -> usize {
    enter(|| {
        let bytes = size
            .checked_mul(count)
            .ok_or_else(|| error("read request too large"))?;
        if bytes == 0 {
            return Ok(0);
        }
        let slice = std::slice::from_raw_parts_mut(buffer as *mut u8, bytes);
        Ok(device_read(con, slice)? / size)
    })
}

unsafe extern "C" fn builtin_write(
    buffer: *const c_void,
    size: usize,
    count: usize,
    con: Rconnection,
) -> usize {
    enter(|| {
        let bytes = size
            .checked_mul(count)
            .ok_or_else(|| error("write request too large"))?;
        if bytes == 0 {
            return Ok(0);
        }
        let slice = std::slice::from_raw_parts(buffer as *const u8, bytes);
        Ok(device_write(con, slice, false)? / size)
    })
}

fn seek_origin(code: c_int) -> SeekOrigin {
    match code {
        2 => SeekOrigin::Current,
        3 => SeekOrigin::End,
        _ => SeekOrigin::Start,
    }
}

fn seek_side(code: c_int) -> SeekSide {
    match code {
        1 => SeekSide::Read,
        2 => SeekSide::Write,
        _ => SeekSide::LastUsed,
    }
}

unsafe extern "C" fn null_open(_con: Rconnection) -> Rboolean {
    enter(|| Err::<Rboolean, _>(error("open not enabled for this connection")))
}

unsafe extern "C" fn null_close(con: Rconnection) {
    (*con).isopen = 0;
}

unsafe extern "C" fn null_destroy(_con: Rconnection) {}

unsafe extern "C" fn custom_fgetc(con: Rconnection) -> c_int {
    enter(|| method_fgetc_internal(con))
}

unsafe extern "C" fn null_fgetc(_con: Rconnection) -> c_int {
    enter(|| Err::<c_int, _>(missing("getc")))
}

unsafe extern "C" fn null_seek(
    _con: Rconnection,
    _position: f64,
    _origin: c_int,
    _rw: c_int,
) -> f64 {
    enter(|| Err::<f64, _>(missing("seek")))
}

unsafe extern "C" fn null_truncate(_con: Rconnection) {
    enter(|| Err::<(), _>(error("truncation not enabled for this connection")))
}

unsafe extern "C" fn null_fflush(_con: Rconnection) -> c_int {
    0
}

unsafe extern "C" fn null_read(
    _buffer: *mut c_void,
    _size: usize,
    _count: usize,
    _con: Rconnection,
) -> usize {
    enter(|| Err::<usize, _>(missing("read")))
}

unsafe extern "C" fn null_write(
    _buffer: *const c_void,
    _size: usize,
    _count: usize,
    _con: Rconnection,
) -> usize {
    enter(|| Err::<usize, _>(missing("write")))
}

enum Device {
    Standard(Standard),
    File(FileDevice),
    Raw(RawDevice),
    TextInput(TextInput),
    TextOutput(TextOutput),
    Pipe(PipeDevice),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Standard {
    Input,
    Output,
    Error,
}

/// `file()` and `gzfile()`: a path, and the stream while open.
struct FileDevice {
    path: PathBuf,
    /// `file("")`: an anonymous file, removed when closed.
    temporary: bool,
    /// `gzfile()`: writes compress; reads decompress when the file is compressed.
    gzip: bool,
    /// `file(raw = TRUE)`: reads never decompress.
    raw: bool,
    compression: u32,
    stream: Option<Stream>,
}

enum Stream {
    Plain(PlainFile),
    Inflate(Inflate),
    Deflate {
        encoder: GzEncoder<File>,
        position: u64,
    },
}

/// A file read and written through separate positions, as GNU R's file connections are.
struct PlainFile {
    file: File,
    append: bool,
    read_position: u64,
    write_position: u64,
    /// Where the operating system's file offset is, so a seek happens only when needed.
    os_position: u64,
    last_was_write: bool,
    /// Bytes read ahead; `read_buffer[read_offset..]` start at `read_position`.
    read_buffer: Vec<u8>,
    read_offset: usize,
    /// Bytes written but not yet passed to the operating system; they end at `write_position`.
    write_buffer: Vec<u8>,
}

/// A file read through gzip decompression when it starts with gzip's magic bytes.
struct Inflate {
    reader: Box<dyn Read>,
    path: PathBuf,
    position: u64,
}

struct RawDevice {
    data: Vec<u8>,
    position: usize,
}

struct TextInput {
    data: Vec<u8>,
    position: usize,
}

struct TextOutput {
    lines: Vec<Vec<u8>>,
    partial: Vec<u8>,
    /// The variable each completed line updates; its symbol and environment are preserved.
    target: Option<TextTarget>,
}

struct PipeDevice {
    command: CString,
    stream: *mut libc::FILE,
}

impl Drop for TextOutput {
    fn drop(&mut self) {
        if let Some(target) = self.target {
            let heap = &mut runtime().heap;
            heap.release(target.symbol);
            heap.release(target.env);
        }
    }
}

impl Drop for PipeDevice {
    fn drop(&mut self) {
        if !self.stream.is_null() {
            // SAFETY: the stream came from popen and is closed once.
            unsafe { libc::pclose(self.stream) };
        }
    }
}

unsafe fn device<'a>(con: Rconnection) -> ApiResult<&'a mut Device> {
    let device = (*con).private as *mut Device;
    if device.is_null() {
        return Err(error("invalid connection"));
    }
    Ok(&mut *device)
}

/// The parts of an fopen-style mode string that matter.
#[derive(Clone, Copy, Debug)]
struct Mode {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    binary: bool,
}

fn parse_mode(mode: &[u8]) -> Option<Mode> {
    let plus = mode.contains(&b'+');
    let binary = mode.contains(&b'b');
    match mode.first()? {
        b'r' => Some(Mode {
            read: true,
            write: plus,
            append: false,
            truncate: false,
            binary,
        }),
        b'w' => Some(Mode {
            read: plus,
            write: true,
            append: false,
            truncate: true,
            binary,
        }),
        b'a' => Some(Mode {
            read: plus,
            write: true,
            append: true,
            truncate: false,
            binary,
        }),
        _ => None,
    }
}

fn os_message(failure: &std::io::Error) -> String {
    match failure.raw_os_error() {
        // SAFETY: strerror returns a valid C string for any error number.
        Some(code) => unsafe {
            String::from_utf8_lossy(CStr::from_ptr(libc::strerror(code)).to_bytes()).into_owned()
        },
        None => failure.to_string(),
    }
}

fn io_error(failure: std::io::Error) -> crate::boundary::Unwind {
    error(os_message(&failure))
}

/// Opens the device for the struct's current mode. A device that cannot open warns why and
/// returns false; the caller turns that into "cannot open the connection".
unsafe fn device_open(con: Rconnection) -> ApiResult<bool> {
    let mode_bytes = mode_of(con);
    let device = device(con)?;
    let Some(mode) = parse_mode(&mode_bytes) else {
        let description = description_of(con);
        warn(format!(
            "cannot open file '{description}': Invalid argument"
        ));
        return Ok(false);
    };
    match device {
        Device::File(file) => {
            if file.stream.is_some() {
                return Ok(true);
            }
            match open_file(file, mode) {
                Ok(stream) => file.stream = Some(stream),
                Err(failure) => {
                    warn(format!(
                        "cannot open file '{}': {}",
                        file.path.display(),
                        os_message(&failure)
                    ));
                    return Ok(false);
                }
            }
            (*con).canseek = 1;
        }
        Device::Pipe(pipe) => {
            if pipe.stream.is_null() {
                let kind: &CStr = if mode.write { c"w" } else { c"r" };
                pipe.stream = libc::popen(pipe.command.as_ptr(), kind.as_ptr());
                if pipe.stream.is_null() {
                    let failure = std::io::Error::last_os_error();
                    warn(format!(
                        "cannot open pipe() cmd '{}': {}",
                        pipe.command.to_string_lossy(),
                        os_message(&failure)
                    ));
                    return Ok(false);
                }
            }
            (*con).canseek = 0;
            (*con).isopen = 1;
            (*con).canread = (!mode.write) as Rboolean;
            (*con).canwrite = mode.write as Rboolean;
            (*con).text = (!mode.binary) as Rboolean;
            return Ok(true);
        }
        Device::Standard(_) | Device::Raw(_) | Device::TextInput(_) | Device::TextOutput(_) => {}
    }
    (*con).isopen = 1;
    (*con).canread = mode.read as Rboolean;
    (*con).canwrite = mode.write as Rboolean;
    (*con).text = (!mode.binary) as Rboolean;
    (*con).save = NO_SAVE;
    Ok(true)
}

fn open_file(device: &mut FileDevice, mode: Mode) -> std::io::Result<Stream> {
    if device.gzip && mode.write {
        if mode.read {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(!mode.append)
            .append(mode.append)
            .open(&device.path)?;
        let encoder = GzEncoder::new(file, Compression::new(device.compression));
        return Ok(Stream::Deflate {
            encoder,
            position: 0,
        });
    }
    if mode.read && !mode.write && !device.raw && is_gzip(&device.path)? {
        return Ok(Stream::Inflate(open_inflate(&device.path)?));
    }
    let file = OpenOptions::new()
        .read(mode.read)
        .write(mode.write && !mode.append)
        .append(mode.append)
        .create(mode.write)
        .truncate(mode.truncate)
        .open(&device.path)?;
    let mut plain = PlainFile {
        file,
        append: mode.append,
        read_position: 0,
        write_position: 0,
        os_position: 0,
        last_was_write: false,
        read_buffer: Vec::new(),
        read_offset: 0,
        write_buffer: Vec::new(),
    };
    if mode.append {
        let end = plain.file.seek(SeekFrom::End(0))?;
        plain.os_position = end;
        plain.write_position = end;
    }
    Ok(Stream::Plain(plain))
}

fn is_gzip(path: &PathBuf) -> std::io::Result<bool> {
    let mut magic = [0u8; 2];
    let mut file = File::open(path)?;
    let mut filled = 0;
    while filled < 2 {
        let count = file.read(&mut magic[filled..])?;
        if count == 0 {
            return Ok(false);
        }
        filled += count;
    }
    Ok(magic == [0x1f, 0x8b])
}

/// A gzip reader that reads an uncompressed file as it is, as GNU R's gzfile does.
fn open_inflate(path: &PathBuf) -> std::io::Result<Inflate> {
    let compressed = is_gzip(path)?;
    let file = BufReader::new(File::open(path)?);
    let reader: Box<dyn Read> = if compressed {
        Box::new(MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    Ok(Inflate {
        reader,
        path: path.clone(),
        position: 0,
    })
}

unsafe fn device_close(con: Rconnection) -> ApiResult<()> {
    (*con).isopen = 0;
    match device(con)? {
        Device::File(file) => {
            let stream = file.stream.take();
            let finished = match stream {
                Some(Stream::Plain(mut plain)) => {
                    // GNU R reports fclose's result for a plain file and nothing for the others.
                    (*con).status = 0;
                    flush_writes(&mut plain)
                }
                Some(Stream::Deflate { encoder, .. }) => encoder.finish().map(drop),
                Some(Stream::Inflate(_)) | None => Ok(()),
            };
            if file.temporary {
                // The file may already be gone, and an anonymous file has no one to tell.
                drop(std::fs::remove_file(&file.path));
            }
            finished.map_err(io_error)
        }
        Device::Pipe(pipe) => {
            if !pipe.stream.is_null() {
                let status = libc::pclose(pipe.stream);
                pipe.stream = null_mut();
                (*con).status = if status == -1 { NA_INTEGER } else { status };
            }
            Ok(())
        }
        Device::TextOutput(output) => {
            if !output.partial.is_empty() {
                let line = std::mem::take(&mut output.partial);
                output.lines.push(line);
                update_text_target(output)?;
            }
            Ok(())
        }
        Device::Standard(_) | Device::Raw(_) | Device::TextInput(_) => Ok(()),
    }
}

unsafe fn device_fgetc(con: Rconnection) -> ApiResult<c_int> {
    let mut byte = [0u8];
    Ok(if device_read(con, &mut byte)? == 1 {
        byte[0] as c_int
    } else {
        EOF
    })
}

/// Reads up to `buffer.len()` bytes, fewer only at the end of the data.
unsafe fn device_read(con: Rconnection, buffer: &mut [u8]) -> ApiResult<usize> {
    match device(con)? {
        Device::Standard(Standard::Input) => {
            read_fully(&mut std::io::stdin().lock(), buffer).map_err(io_error)
        }
        Device::Standard(_) => Err(error("cannot read from this connection")),
        Device::File(file) => match &mut file.stream {
            Some(Stream::Plain(plain)) => plain_read(plain, buffer).map_err(io_error),
            Some(Stream::Inflate(inflate)) => {
                let count = read_fully(&mut inflate.reader, buffer).map_err(io_error)?;
                inflate.position += count as u64;
                Ok(count)
            }
            Some(Stream::Deflate { .. }) => Err(error("cannot read from this connection")),
            None => Err(error("connection is not open")),
        },
        Device::Raw(raw) => {
            let available = raw.data.len().saturating_sub(raw.position);
            let count = available.min(buffer.len());
            buffer[..count].copy_from_slice(&raw.data[raw.position..raw.position + count]);
            raw.position += count;
            Ok(count)
        }
        Device::TextInput(text) => {
            let available = text.data.len().saturating_sub(text.position);
            let count = available.min(buffer.len());
            buffer[..count].copy_from_slice(&text.data[text.position..text.position + count]);
            text.position += count;
            Ok(count)
        }
        Device::TextOutput(_) => Err(error("cannot read from this connection")),
        Device::Pipe(pipe) => {
            if pipe.stream.is_null() {
                return Err(error("connection is not open"));
            }
            Ok(libc::fread(
                buffer.as_mut_ptr().cast(),
                1,
                buffer.len(),
                pipe.stream,
            ))
        }
    }
}

fn read_fully(reader: &mut dyn Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(failure) if failure.kind() == std::io::ErrorKind::Interrupted => {}
            Err(failure) => return Err(failure),
        }
    }
    Ok(filled)
}

fn plain_read(plain: &mut PlainFile, buffer: &mut [u8]) -> std::io::Result<usize> {
    if plain.last_was_write {
        flush_writes(plain)?;
        plain.last_was_write = false;
    }
    let mut done = 0;
    while done < buffer.len() {
        if plain.read_offset == plain.read_buffer.len() {
            plain.read_buffer.clear();
            plain.read_offset = 0;
            seek_os(plain, plain.read_position)?;
            if buffer.len() - done >= READ_CHUNK {
                let count = read_fully(&mut plain.file, &mut buffer[done..])?;
                plain.os_position += count as u64;
                plain.read_position += count as u64;
                done += count;
                break;
            }
            plain.read_buffer.resize(READ_CHUNK, 0);
            let count = read_fully(&mut plain.file, &mut plain.read_buffer)?;
            plain.read_buffer.truncate(count);
            plain.os_position += count as u64;
            if count == 0 {
                break;
            }
        }
        let available = &plain.read_buffer[plain.read_offset..];
        let count = available.len().min(buffer.len() - done);
        buffer[done..done + count].copy_from_slice(&available[..count]);
        plain.read_offset += count;
        plain.read_position += count as u64;
        done += count;
    }
    Ok(done)
}

fn seek_os(plain: &mut PlainFile, position: u64) -> std::io::Result<()> {
    if plain.os_position != position {
        plain.file.seek(SeekFrom::Start(position))?;
        plain.os_position = position;
    }
    Ok(())
}

fn flush_writes(plain: &mut PlainFile) -> std::io::Result<()> {
    if plain.write_buffer.is_empty() {
        return Ok(());
    }
    let pending = std::mem::take(&mut plain.write_buffer);
    if plain.append {
        plain.file.write_all(&pending)?;
        plain.os_position = plain.file.stream_position()?;
        plain.write_position = plain.os_position;
    } else {
        seek_os(plain, plain.write_position - pending.len() as u64)?;
        plain.file.write_all(&pending)?;
        plain.os_position = plain.write_position;
    }
    Ok(())
}

/// Writes bytes, or printed text when `printing`, which is the only way into a text output
/// connection, as in GNU R.
unsafe fn device_write(con: Rconnection, bytes: &[u8], printing: bool) -> ApiResult<usize> {
    match device(con)? {
        Device::Standard(Standard::Input) => Err(error("cannot write to this connection")),
        Device::Standard(stream) => {
            crate::boundary::write_console(bytes, *stream == Standard::Error);
            Ok(bytes.len())
        }
        Device::File(file) => match &mut file.stream {
            Some(Stream::Plain(plain)) => {
                if !plain.last_was_write {
                    plain.read_buffer.clear();
                    plain.read_offset = 0;
                    plain.last_was_write = true;
                }
                plain.write_buffer.extend_from_slice(bytes);
                plain.write_position += bytes.len() as u64;
                if plain.write_buffer.len() >= READ_CHUNK {
                    flush_writes(plain).map_err(io_error)?;
                }
                Ok(bytes.len())
            }
            Some(Stream::Deflate { encoder, position }) => {
                encoder.write_all(bytes).map_err(io_error)?;
                *position += bytes.len() as u64;
                Ok(bytes.len())
            }
            Some(Stream::Inflate(_)) => Err(error("cannot write to this connection")),
            None => Err(error("connection is not open")),
        },
        Device::Raw(raw) => {
            let end = raw.position + bytes.len();
            if end > raw.data.len() {
                raw.data.resize(end, 0);
            }
            raw.data[raw.position..end].copy_from_slice(bytes);
            raw.position = end;
            Ok(bytes.len())
        }
        Device::TextInput(_) => Err(error("cannot write to this connection")),
        Device::TextOutput(_) if !printing => Err(missing("write")),
        Device::TextOutput(output) => {
            let mut completed = false;
            for &byte in bytes {
                if byte == b'\n' {
                    let line = std::mem::take(&mut output.partial);
                    output.lines.push(line);
                    completed = true;
                } else {
                    output.partial.push(byte);
                }
            }
            if completed {
                update_text_target(output)?;
            }
            Ok(bytes.len())
        }
        Device::Pipe(pipe) => {
            if pipe.stream.is_null() {
                return Err(error("connection is not open"));
            }
            Ok(libc::fwrite(
                bytes.as_ptr().cast(),
                1,
                bytes.len(),
                pipe.stream,
            ))
        }
    }
}

unsafe fn update_text_target(output: &TextOutput) -> ApiResult<()> {
    let Some(target) = output.target else {
        return Ok(());
    };
    let value = strings_vector(&output.lines, Encoding::Native)?;
    define_var(target.symbol, value, target.env)
}

unsafe fn strings_vector(lines: &[Vec<u8>], encoding: Encoding) -> ApiResult<Sexp> {
    let runtime = runtime();
    let vector = alloc_vector(SexpType::String, lines.len() as R_xlen_t)?;
    for (index, line) in lines.iter().enumerate() {
        let charsxp = runtime.heap.mk_char(line, encoding)?;
        runtime.heap.set_element(vector, index, charsxp);
    }
    Ok(vector)
}

unsafe fn device_seek(
    con: Rconnection,
    position: f64,
    origin: SeekOrigin,
    side: SeekSide,
) -> ApiResult<f64> {
    let query = position.is_nan();
    match device(con)? {
        Device::Standard(_) | Device::Pipe(_) => Err(missing("seek")),
        Device::TextInput(_) | Device::TextOutput(_) => {
            if !query && position >= 0.0 {
                return Err(error("seek is not relevant for text connection"));
            }
            Ok(0.0)
        }
        Device::Raw(raw) => {
            let current = raw.position as f64;
            if query {
                return Ok(current);
            }
            let target = match origin {
                SeekOrigin::Start => position,
                SeekOrigin::Current => current + position,
                SeekOrigin::End => raw.data.len() as f64 + position,
            };
            if target < 0.0 || target > raw.data.len() as f64 {
                return Err(error(
                    "attempt to seek outside the range of the raw connection",
                ));
            }
            raw.position = target as usize;
            Ok(current)
        }
        Device::File(file) => match &mut file.stream {
            Some(Stream::Plain(plain)) => {
                flush_writes(plain).map_err(io_error)?;
                let use_write = match side {
                    SeekSide::Read => false,
                    SeekSide::Write => true,
                    SeekSide::LastUsed => plain.last_was_write,
                };
                let current = if use_write {
                    plain.write_position
                } else {
                    plain.read_position
                };
                if query {
                    return Ok(current as f64);
                }
                let base = match origin {
                    SeekOrigin::Start => 0.0,
                    SeekOrigin::Current => current as f64,
                    SeekOrigin::End => plain.file.metadata().map_err(io_error)?.len() as f64,
                };
                let target = base + position;
                if target < 0.0 {
                    return Err(error("seek to a negative position"));
                }
                if use_write {
                    plain.write_position = target as u64;
                } else {
                    plain.read_position = target as u64;
                    plain.read_buffer.clear();
                    plain.read_offset = 0;
                }
                Ok(current as f64)
            }
            Some(Stream::Inflate(inflate)) => {
                let current = inflate.position as f64;
                if query {
                    return Ok(current);
                }
                let target = match origin {
                    SeekOrigin::Start => position,
                    SeekOrigin::Current => current + position,
                    SeekOrigin::End => {
                        return Err(error(
                            "whence = \"end\" is not implemented for gzfile connections",
                        ));
                    }
                };
                if target < 0.0 {
                    return Err(error("seek to a negative position"));
                }
                let target = target as u64;
                if target < inflate.position {
                    *inflate = open_inflate(&inflate.path).map_err(io_error)?;
                }
                let skip = target - inflate.position;
                let skipped =
                    std::io::copy(&mut (&mut inflate.reader).take(skip), &mut std::io::sink())
                        .map_err(io_error)?;
                inflate.position += skipped;
                Ok(current)
            }
            Some(Stream::Deflate {
                position: current, ..
            }) => {
                if query {
                    return Ok(*current as f64);
                }
                Err(error(
                    "seek on a gzfile connection open for writing is not supported",
                ))
            }
            None => Err(error("connection is not open")),
        },
    }
}

unsafe fn device_truncate(con: Rconnection) -> ApiResult<()> {
    if (*con).isopen == 0 || (*con).canwrite == 0 {
        return Err(error("can only truncate connections open for writing"));
    }
    match device(con)? {
        Device::File(FileDevice {
            stream: Some(Stream::Plain(plain)),
            ..
        }) => {
            flush_writes(plain).map_err(io_error)?;
            let position = if plain.last_was_write {
                plain.write_position
            } else {
                plain.read_position
            };
            plain.file.set_len(position).map_err(io_error)?;
            plain.read_buffer.clear();
            plain.read_offset = 0;
            Ok(())
        }
        Device::Raw(raw) => {
            raw.data.truncate(raw.position);
            Ok(())
        }
        _ => Err(error("truncation not enabled for this connection")),
    }
}

unsafe fn device_flush(con: Rconnection) -> ApiResult<()> {
    match device(con)? {
        Device::File(FileDevice {
            stream: Some(Stream::Plain(plain)),
            ..
        }) => {
            flush_writes(plain).map_err(io_error)?;
            plain.file.flush().map_err(io_error)
        }
        Device::File(FileDevice {
            stream: Some(Stream::Deflate { encoder, .. }),
            ..
        }) => encoder.flush().map_err(io_error),
        Device::Pipe(pipe) if !pipe.stream.is_null() => {
            libc::fflush(pipe.stream);
            Ok(())
        }
        Device::Standard(Standard::Output) => {
            crate::boundary::R_FlushConsole();
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A built-in connection, registered and, when `open` is not empty, opened.
unsafe fn new_builtin(
    class: &str,
    description: &[u8],
    open: &str,
    default_mode: &str,
    device: Device,
) -> ApiResult<Sexp> {
    let mode = if open.is_empty() { default_mode } else { open };
    let con = allocate_rconn(
        class.as_bytes(),
        description,
        mode.as_bytes(),
        &BUILTIN_METHODS,
    );
    (*con).private = Box::into_raw(Box::new(device)).cast();
    let object = register(con, true)?;
    runtime().heap.root(object);
    if !open.is_empty() {
        let slot = resolve_con(object)?.0;
        if let Err(unwind) = open_con(con) {
            close_slot(slot)?;
            return Err(unwind);
        }
    }
    Ok(object)
}

/// `file(description, open)`. An empty description is an anonymous file opened `"w+"`, and a
/// file opened for reading that holds gzip data reads decompressed with class `gzfile`.
pub unsafe fn new_file(
    description: &str,
    open: &str,
    blocking: bool,
    raw: bool,
) -> ApiResult<Sexp> {
    if description.contains("://") {
        return Err(error("ry does not support URL connections yet"));
    }
    let (path, temporary, open) = if description.is_empty() {
        (temporary_path(), true, "w+")
    } else {
        (expand_tilde(description), false, open)
    };
    let gzip = !raw
        && !open.is_empty()
        && parse_mode(open.as_bytes()).is_some_and(|mode| mode.read && !mode.write)
        && is_gzip(&path).unwrap_or(false);
    let device = Device::File(FileDevice {
        path,
        temporary,
        gzip: false,
        raw,
        compression: 6,
        stream: None,
    });
    let object = new_builtin("file", description.as_bytes(), open, "r", device)?;
    let con = resolve_con(object)?.1;
    if !blocking {
        (*con).blocking = 0;
    }
    // The object keeps the class it was created with; only the struct says what it became.
    if gzip {
        libc::free((*con).class.cast());
        (*con).class = malloc_string(b"gzfile");
    }
    Ok(object)
}

/// `gzfile(description, open, compression)`: gzip-compressed writes, and reads that
/// decompress gzip data and pass anything else through.
pub unsafe fn new_gzfile(description: &str, open: &str, compression: i32) -> ApiResult<Sexp> {
    if !(0..=9).contains(&compression) {
        return Err(error("invalid 'compress' argument"));
    }
    let device = Device::File(FileDevice {
        path: expand_tilde(description),
        temporary: false,
        gzip: true,
        raw: false,
        compression: compression as u32,
        stream: None,
    });
    new_builtin("gzfile", description.as_bytes(), open, "rb", device)
}

/// `bzfile()` and `xzfile()`, which ry does not implement yet.
pub fn new_unsupported(class: &str) -> ApiResult<Sexp> {
    Err(error(format!(
        "ry does not support {class} connections yet"
    )))
}

/// `pipe(command, open)`: the command's standard output to read, or its input to write.
pub unsafe fn new_pipe(command: &str, open: &str) -> ApiResult<Sexp> {
    let command_text =
        CString::new(command).map_err(|_| error("embedded nul in a pipe command"))?;
    let device = Device::Pipe(PipeDevice {
        command: command_text,
        stream: null_mut(),
    });
    let object = new_builtin("pipe", command.as_bytes(), open, "r", device)?;
    // A pipe's reader may be waiting on the command, so an unfinished last line is kept back.
    (*resolve_con(object)?.1).blocking = 0;
    Ok(object)
}

/// `rawConnection(object, open)`: reads from or writes into a copy of a raw vector.
pub unsafe fn new_raw_connection(description: &[u8], object: Sexp, open: &str) -> ApiResult<Sexp> {
    if object.kind() != SexpType::Raw {
        return Err(error("invalid 'raw' argument"));
    }
    let open = if open.is_empty() { "r" } else { open };
    let mode = parse_mode(open.as_bytes()).ok_or_else(|| error("unsupported mode"))?;
    let mut data = object.bytes().to_vec();
    if mode.truncate {
        data.clear();
    }
    let position = if mode.append { data.len() } else { 0 };
    let object = new_builtin(
        "rawConnection",
        description,
        open,
        open,
        Device::Raw(RawDevice { data, position }),
    )?;
    let con = resolve_con(object)?.1;
    (*con).text = 0;
    (*con).canseek = 1;
    Ok(object)
}

/// `textConnection(object, open)`: reads the lines of a character vector, or collects written
/// lines, into `target` when given.
pub unsafe fn new_text_connection(
    description: &[u8],
    object: Sexp,
    open: &str,
    target: Option<TextTarget>,
) -> ApiResult<Sexp> {
    let device = match open {
        "" | "r" => {
            if object.kind() != SexpType::String {
                return Err(error("invalid 'text' argument"));
            }
            let mut data = Vec::new();
            for &element in object.slice::<Sexp>() {
                data.extend_from_slice(utf8_text(element)?.as_bytes());
                data.push(b'\n');
            }
            Device::TextInput(TextInput { data, position: 0 })
        }
        "w" | "a" => {
            let mut lines = Vec::new();
            if let Some(target) = target {
                let heap = &mut runtime().heap;
                heap.preserve(target.symbol);
                heap.preserve(target.env);
                if open == "a" {
                    let existing = crate::environments::find_var(target.symbol, target.env)?;
                    if existing.kind() == SexpType::String {
                        for &element in existing.slice::<Sexp>() {
                            lines.push(element.bytes().to_vec());
                        }
                    }
                }
            }
            let output = TextOutput {
                lines,
                partial: Vec::new(),
                target,
            };
            update_text_target(&output)?;
            Device::TextOutput(output)
        }
        _ => return Err(error("unsupported mode")),
    };
    let open = if open.is_empty() { "r" } else { open };
    new_builtin("textConnection", description, open, open, device)
}

/// `stdin()`, `stdout()` and `stderr()`, for slots 0, 1 and 2.
pub unsafe fn standard(slot: usize) -> ApiResult<Sexp> {
    if slot > 2 {
        return Err(error(format!("there is no standard connection {slot}")));
    }
    connection_object(slot)
}

fn temporary_path() -> PathBuf {
    static COUNTER: crate::Global<u64> = crate::Global::new(0);
    COUNTER.set(COUNTER.get() + 1);
    std::env::temp_dir().join(format!(
        "ry-connection-{}-{}",
        std::process::id(),
        COUNTER.get()
    ))
}

fn expand_tilde(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            let mut expanded = PathBuf::from(home);
            expanded.push(rest.trim_start_matches('/'));
            expanded
        }
        _ => PathBuf::from(path),
    }
}

/// `getConnection(what)`.
pub unsafe fn get_connection(slot: i32) -> ApiResult<Sexp> {
    match usize::try_from(slot)
        .ok()
        .filter(|&slot| entry(slot).is_some())
    {
        Some(slot) => connection_object(slot),
        None => Err(error(format!("there is no connection {slot}"))),
    }
}

/// `getAllConnections()`: the slots in use, ascending.
pub unsafe fn all_connections() -> ApiResult<Sexp> {
    let slots: Vec<i32> = (0..NCONNECTIONS)
        .filter(|&slot| entry(slot).is_some())
        .map(|slot| slot as i32)
        .collect();
    let vector = alloc_vector(SexpType::Integer, slots.len() as R_xlen_t)?;
    vector.slice_mut::<i32>().copy_from_slice(&slots);
    Ok(vector)
}

/// `closeAllConnections()`: closes every connection but the standard three.
pub unsafe fn close_all() -> ApiResult<()> {
    let mut first_failure = None;
    for slot in 3..NCONNECTIONS {
        if entry(slot).is_some()
            && let Err(unwind) = close_slot(slot)
        {
            first_failure.get_or_insert(unwind);
        }
    }
    first_failure.map_or(Ok(()), Err)
}

/// `open(con, mode)`. Opening an open connection only warns, as in GNU R.
pub unsafe fn open(con: Sexp, mode: &str, blocking: bool) -> ApiResult<()> {
    let (slot, con) = resolve_con(con)?;
    if slot <= 2 {
        return Err(error("cannot open standard connections"));
    }
    if (*con).isopen != 0 {
        warn("connection is already open");
        return Ok(());
    }
    if !mode.is_empty() {
        set_mode(con, mode.as_bytes());
    }
    (*con).blocking = blocking as Rboolean;
    open_con(con)
}

unsafe fn open_con(con: Rconnection) -> ApiResult<()> {
    if !method_open(con)? {
        return Err(error("cannot open the connection"));
    }
    Ok(())
}

/// `close(con)`: closes the connection and frees its slot. Returns the close status where the
/// kind has one (a pipe's exit status), `NULL` otherwise.
pub unsafe fn close(con: Sexp) -> ApiResult<Sexp> {
    let (slot, _) = resolve_con(con)?;
    if slot <= 2 {
        return Err(error("cannot close standard connections"));
    }
    let status = close_slot(slot)?;
    if status == NA_INTEGER {
        Ok(runtime().nil())
    } else {
        scalar(SexpType::Integer, status)
    }
}

/// `isOpen(con, rw)`.
pub unsafe fn is_open(con: Sexp, side: SeekSide) -> ApiResult<bool> {
    let con = resolve_con(con)?.1;
    let open = (*con).isopen != 0;
    Ok(match side {
        SeekSide::LastUsed => open,
        SeekSide::Read => open && (*con).canread != 0,
        SeekSide::Write => open && (*con).canwrite != 0,
    })
}

/// `isIncomplete(con)`: whether the last read stopped at a partial line of a non-blocking
/// connection.
pub unsafe fn is_incomplete(con: Sexp) -> ApiResult<bool> {
    Ok((*resolve_con(con)?.1).incomplete != 0)
}

/// Runs `body` with the connection open in `mode`, opening it first and closing it afterwards
/// when it was closed, as R's readers and writers do for an unopened connection.
unsafe fn with_open<T>(
    con: Rconnection,
    mode: &str,
    body: impl FnOnce() -> ApiResult<T>,
) -> ApiResult<T> {
    if (*con).isopen != 0 {
        return body();
    }
    let saved = mode_of(con);
    set_mode(con, mode.as_bytes());
    let opened = open_con(con);
    set_mode(con, &saved);
    opened?;
    let result = body();
    let closed = method_close(con);
    match (result, closed) {
        (Err(unwind), _) => Err(unwind),
        (Ok(_), Err(unwind)) => Err(unwind),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// One character as text reads see it: pushback first, then the look-ahead, then the device,
/// with `\r` and `\r\n` both read as `\n`.
unsafe fn read_char(con: Rconnection) -> ApiResult<c_int> {
    if (*con).save2 != NO_SAVE {
        let character = (*con).save2;
        (*con).save2 = NO_SAVE;
        return Ok(character);
    }
    while (*con).nPushBack > 0 {
        let top_index = (*con).nPushBack as usize - 1;
        let top = *(*con).PushBack.add(top_index);
        let position = (*con).posPushBack as usize;
        let character = *top.add(position) as u8;
        if character == 0 {
            pop_push_back(con);
            continue;
        }
        (*con).posPushBack += 1;
        if *top.add(position + 1) == 0 {
            pop_push_back(con);
        }
        return Ok(character as c_int);
    }
    if (*con).save != NO_SAVE {
        let character = (*con).save;
        (*con).save = NO_SAVE;
        return Ok(character);
    }
    let character = method_fgetc(con)?;
    if character == b'\r' as c_int {
        let next = method_fgetc(con)?;
        if next != b'\n' as c_int {
            (*con).save = if next != b'\r' as c_int {
                next
            } else {
                b'\n' as c_int
            };
        }
        return Ok(b'\n' as c_int);
    }
    Ok(character)
}

unsafe fn pop_push_back(con: Rconnection) {
    let count = (*con).nPushBack as usize;
    libc::free((*(*con).PushBack.add(count - 1)).cast());
    (*con).nPushBack -= 1;
    (*con).posPushBack = 0;
    if (*con).nPushBack == 0 {
        libc::free((*con).PushBack.cast());
        (*con).PushBack = null_mut();
    }
}

unsafe fn clear_push_back_lines(con: Rconnection) {
    for _ in 0..(*con).nPushBack {
        pop_push_back(con);
    }
}

/// `readLines(con, n, ok, warn, encoding, skipNul)`. `n < 0` reads every line.
pub unsafe fn read_lines(
    con: Sexp,
    n: i64,
    ok: bool,
    warn_incomplete: bool,
    encoding: Encoding,
    skip_nul: bool,
) -> ApiResult<Sexp> {
    let con = resolve_con(con)?.1;
    with_open(con, "rt", || {
        if (*con).canread == 0 {
            return Err(error("cannot read from this connection"));
        }
        (*con).incomplete = 0;
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut line = Vec::new();
        while n < 0 || (lines.len() as i64) < n {
            line.clear();
            let mut nul = false;
            let character = loop {
                let character = read_char(con)?;
                if character == EOF || character == b'\n' as c_int {
                    break character;
                }
                if character == 0 {
                    if !skip_nul {
                        nul = true;
                    }
                    continue;
                }
                if !nul {
                    line.push(character as u8);
                }
            };
            if nul {
                warn(format!(
                    "line {} appears to contain an embedded nul",
                    lines.len() + 1
                ));
            }
            if character == EOF {
                if !line.is_empty() || nul {
                    if (*con).text != 0 && (*con).blocking == 0 && class_of(con) != "gzfile" {
                        push_back_bytes(con, &line);
                        (*con).incomplete = 1;
                    } else {
                        lines.push(std::mem::take(&mut line));
                        if warn_incomplete {
                            warn(format!(
                                "incomplete final line found on '{}'",
                                description_of(con)
                            ));
                        }
                    }
                }
                break;
            }
            lines.push(line.clone());
        }
        if n > 0 && (lines.len() as i64) < n && !ok {
            return Err(error("too few lines read in readLines"));
        }
        strings_vector(&lines, encoding)
    })
}

/// `writeLines(text, con, sep)`.
pub unsafe fn write_lines(text: Sexp, con: Sexp, separator: &[u8]) -> ApiResult<()> {
    if text.kind() != SexpType::String {
        return Err(error("can only write character objects"));
    }
    let con = resolve_con(con)?.1;
    with_open(con, "wt", || {
        if (*con).canwrite == 0 {
            return Err(error("cannot write to this connection"));
        }
        let mut line = Vec::new();
        for &element in text.slice::<Sexp>() {
            line.clear();
            line.extend_from_slice(utf8_text(element)?.as_bytes());
            line.extend_from_slice(separator);
            method_print(con, &line)?;
        }
        Ok(())
    })
}

/// `readChar(con, nchars, useBytes)`: one string per count, stopping at the end of the data.
pub unsafe fn read_char_strings(con: Sexp, counts: &[i32], use_bytes: bool) -> ApiResult<Sexp> {
    let con = resolve_con(con)?.1;
    with_open(con, "rb", || {
        if (*con).canread == 0 {
            return Err(error("cannot read from this connection"));
        }
        if (*con).text != 0 {
            warn("text connection used with readChar(), results may be incorrect");
        }
        let mut strings = Vec::new();
        for &count in counts {
            if count == NA_INTEGER || count < 0 {
                return Err(error("invalid 'nchars' argument"));
            }
            let mut text = Vec::new();
            let mut wanted = count as usize;
            while wanted > 0 {
                let mut lead = [0u8];
                if method_read(con, &mut lead)? == 0 {
                    break;
                }
                text.push(lead[0]);
                if !use_bytes {
                    let continuation = utf8_continuation_bytes(lead[0]);
                    let mut rest = vec![0u8; continuation];
                    let got = read_all(con, &mut rest)?;
                    text.extend_from_slice(&rest[..got]);
                }
                wanted -= 1;
            }
            if text.is_empty() && count > 0 {
                break;
            }
            if let Some(end) = text.iter().position(|&byte| byte == 0) {
                text.truncate(end);
            }
            strings.push(text);
        }
        strings_vector(&strings, Encoding::Native)
    })
}

fn utf8_continuation_bytes(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf7 => 3,
        _ => 0,
    }
}

/// Reads until `buffer` is full or a read returns nothing.
unsafe fn read_all(con: Rconnection, buffer: &mut [u8]) -> ApiResult<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let count = method_read(con, &mut buffer[filled..])?;
        if count == 0 {
            break;
        }
        filled += count;
    }
    Ok(filled)
}

/// The size each `readBin`/`writeBin` element type takes by default, and the sizes it allows.
fn binary_size(kind: BinaryType, size: Option<usize>) -> ApiResult<usize> {
    let (default, allowed): (usize, &[usize]) = match kind {
        BinaryType::Integer => (4, &[1, 2, 4, 8]),
        BinaryType::Logical => (4, &[1, 2, 4, 8]),
        BinaryType::Double => (8, &[4, 8]),
        BinaryType::Complex => (16, &[16]),
        BinaryType::Raw => (1, &[1]),
        BinaryType::Character => (1, &[1]),
    };
    match size {
        None => Ok(default),
        Some(size) if allowed.contains(&size) => Ok(size),
        Some(size) => Err(error(format!("size {size} is unknown on this machine"))),
    }
}

/// `readBin(con, what, n, size, signed, endian)` on a connection.
pub unsafe fn read_bin(
    con: Sexp,
    kind: BinaryType,
    n: usize,
    size: Option<usize>,
    signed: bool,
    swap: bool,
) -> ApiResult<Sexp> {
    let con = resolve_con(con)?.1;
    with_open(con, "rb", || {
        if (*con).canread == 0 {
            return Err(error("cannot read from this connection"));
        }
        if (*con).text != 0 {
            return Err(error("can only read from a binary connection"));
        }
        if kind == BinaryType::Character {
            let mut strings = Vec::new();
            'strings: for _ in 0..n {
                let mut text = Vec::new();
                loop {
                    let mut byte = [0u8];
                    if method_read(con, &mut byte)? == 0 {
                        break 'strings;
                    }
                    if byte[0] == 0 {
                        break;
                    }
                    text.push(byte[0]);
                }
                strings.push(text);
            }
            return strings_vector(&strings, Encoding::Native);
        }
        let size = binary_size(kind, size)?;
        let mut bytes = vec![0u8; n.checked_mul(size).ok_or_else(|| error("too many items"))?];
        let got = read_all(con, &mut bytes)? / size;
        decode_binary(&bytes[..got * size], kind, size, signed, swap)
    })
}

/// `readBin` on bytes in memory, as for a raw vector argument.
pub unsafe fn read_bin_bytes(
    bytes: &[u8],
    kind: BinaryType,
    n: usize,
    size: Option<usize>,
    signed: bool,
    swap: bool,
) -> ApiResult<Sexp> {
    if kind == BinaryType::Character {
        let strings: Vec<Vec<u8>> = bytes
            .split_inclusive(|&byte| byte == 0)
            .filter(|piece| piece.last() == Some(&0))
            .take(n)
            .map(|piece| piece[..piece.len() - 1].to_vec())
            .collect();
        return strings_vector(&strings, Encoding::Native);
    }
    let size = binary_size(kind, size)?;
    let count = (bytes.len() / size).min(n);
    decode_binary(&bytes[..count * size], kind, size, signed, swap)
}

unsafe fn decode_binary(
    bytes: &[u8],
    kind: BinaryType,
    size: usize,
    signed: bool,
    swap: bool,
) -> ApiResult<Sexp> {
    let count = bytes.len() / size;
    let element = |index: usize| -> [u8; 16] {
        let mut value = [0u8; 16];
        value[..size].copy_from_slice(&bytes[index * size..(index + 1) * size]);
        if swap {
            value[..size].reverse();
        }
        value
    };
    match kind {
        BinaryType::Integer | BinaryType::Logical => {
            let vector = alloc_vector(
                if kind == BinaryType::Integer {
                    SexpType::Integer
                } else {
                    SexpType::Logical
                },
                count as R_xlen_t,
            )?;
            for (index, target) in vector.slice_mut::<i32>().iter_mut().enumerate() {
                let raw = element(index);
                *target = match (size, signed) {
                    (1, true) => raw[0] as i8 as i32,
                    (1, false) => raw[0] as i32,
                    (2, true) => i16::from_ne_bytes([raw[0], raw[1]]) as i32,
                    (2, false) => u16::from_ne_bytes([raw[0], raw[1]]) as i32,
                    (8, _) => i64::from_ne_bytes([
                        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                    ]) as i32,
                    _ => i32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]),
                };
            }
            Ok(vector)
        }
        BinaryType::Double => {
            let vector = alloc_vector(SexpType::Real, count as R_xlen_t)?;
            for (index, target) in vector.slice_mut::<f64>().iter_mut().enumerate() {
                let raw = element(index);
                *target = if size == 4 {
                    f32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64
                } else {
                    f64::from_ne_bytes([
                        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                    ])
                };
            }
            Ok(vector)
        }
        BinaryType::Complex => {
            let vector = alloc_vector(SexpType::Complex, count as R_xlen_t)?;
            for (index, target) in vector.slice_mut::<Rcomplex>().iter_mut().enumerate() {
                let mut raw = [0u8; 16];
                raw.copy_from_slice(&bytes[index * 16..(index + 1) * 16]);
                if swap {
                    raw[..8].reverse();
                    raw[8..].reverse();
                }
                let mut half = [0u8; 8];
                half.copy_from_slice(&raw[..8]);
                target.r = f64::from_ne_bytes(half);
                half.copy_from_slice(&raw[8..]);
                target.i = f64::from_ne_bytes(half);
            }
            Ok(vector)
        }
        BinaryType::Raw => {
            let vector = alloc_vector(SexpType::Raw, count as R_xlen_t)?;
            vector.slice_mut::<u8>().copy_from_slice(bytes);
            Ok(vector)
        }
        BinaryType::Character => Err(error("character data has no fixed size")),
    }
}

/// The bytes `writeBin(object, size, endian)` writes.
pub unsafe fn encode_binary(object: Sexp, size: Option<usize>, swap: bool) -> ApiResult<Vec<u8>> {
    let kind = match object.kind() {
        SexpType::Integer => BinaryType::Integer,
        SexpType::Logical => BinaryType::Logical,
        SexpType::Real => BinaryType::Double,
        SexpType::Complex => BinaryType::Complex,
        SexpType::Raw => return Ok(object.bytes().to_vec()),
        SexpType::String => {
            let mut bytes = Vec::new();
            for &element in object.slice::<Sexp>() {
                bytes.extend_from_slice(utf8_text(element)?.as_bytes());
                bytes.push(0);
            }
            return Ok(bytes);
        }
        _ => return Err(error("can only write vector objects")),
    };
    let size = binary_size(kind, size)?;
    let mut bytes = Vec::with_capacity(object.length() as usize * size);
    let mut push = |mut element: Vec<u8>| {
        if swap {
            element.reverse();
        }
        bytes.extend_from_slice(&element);
    };
    match kind {
        BinaryType::Integer | BinaryType::Logical => {
            for &value in object.slice::<i32>() {
                push(match size {
                    1 => vec![value as u8],
                    2 => (value as i16).to_ne_bytes().to_vec(),
                    8 => (value as i64).to_ne_bytes().to_vec(),
                    _ => value.to_ne_bytes().to_vec(),
                });
            }
        }
        BinaryType::Double => {
            for &value in object.slice::<f64>() {
                push(if size == 4 {
                    (value as f32).to_ne_bytes().to_vec()
                } else {
                    value.to_ne_bytes().to_vec()
                });
            }
        }
        BinaryType::Complex => {
            for value in object.slice::<Rcomplex>() {
                push(value.r.to_ne_bytes().to_vec());
                push(value.i.to_ne_bytes().to_vec());
            }
        }
        BinaryType::Raw | BinaryType::Character => {}
    }
    Ok(bytes)
}

/// `writeBin(object, con, size, endian)` on a connection.
pub unsafe fn write_bin(object: Sexp, con: Sexp, size: Option<usize>, swap: bool) -> ApiResult<()> {
    let bytes = encode_binary(object, size, swap)?;
    let con = resolve_con(con)?.1;
    with_open(con, "wb", || {
        if (*con).canwrite == 0 {
            return Err(error("cannot write to this connection"));
        }
        if (*con).text != 0 {
            return Err(error("can only write to a binary connection"));
        }
        write(con, &bytes)
    })
}

/// Writes every byte, as `R_WriteConnection` callers expect.
unsafe fn write(con: Rconnection, bytes: &[u8]) -> ApiResult<()> {
    if !bytes.is_empty() {
        method_write(con, bytes)?;
    }
    Ok(())
}

/// `seek(con, where, origin, rw)`: the position before the move; a NaN `where` only queries.
/// Moving discards pushback.
pub unsafe fn seek(con: Sexp, position: f64, origin: SeekOrigin, side: SeekSide) -> ApiResult<f64> {
    let con = resolve_con(con)?.1;
    if (*con).isopen == 0 {
        return Err(error("connection is not open"));
    }
    if !position.is_nan() {
        clear_push_back_lines(con);
        (*con).save = NO_SAVE;
        (*con).save2 = NO_SAVE;
    }
    method_seek(con, position, origin, side)
}

/// `truncate(con)`: cuts the file at the current position.
pub unsafe fn truncate(con: Sexp) -> ApiResult<()> {
    let con = resolve_con(con)?.1;
    if (*con).isopen == 0 {
        return Err(error("can only truncate an open connection"));
    }
    method_truncate(con)
}

/// `flush(con)`.
pub unsafe fn flush(con: Sexp) -> ApiResult<()> {
    let con = resolve_con(con)?.1;
    if (*con).canwrite != 0 {
        method_fflush(con)?;
    }
    Ok(())
}

/// `summary.connection(object)`: description, class, mode, text, opened, can read, can write.
pub unsafe fn summary(con: Sexp) -> ApiResult<Sexp> {
    let con = resolve_con(con)?.1;
    let yes_no = |flag: Rboolean| if flag != 0 { "yes" } else { "no" };
    let values: [(&str, String); 7] = [
        ("description", description_of(con)),
        ("class", class_of(con)),
        ("mode", String::from_utf8_lossy(&mode_of(con)).into_owned()),
        (
            "text",
            if (*con).text != 0 { "text" } else { "binary" }.to_owned(),
        ),
        (
            "opened",
            if (*con).isopen != 0 {
                "opened"
            } else {
                "closed"
            }
            .to_owned(),
        ),
        ("can read", yes_no((*con).canread).to_owned()),
        ("can write", yes_no((*con).canwrite).to_owned()),
    ];
    let runtime = runtime();
    let list = alloc_vector(SexpType::List, values.len() as R_xlen_t)?;
    let names = alloc_vector(SexpType::String, values.len() as R_xlen_t)?;
    for (index, (name, value)) in values.iter().enumerate() {
        let charsxp = runtime.heap.mk_char(value.as_bytes(), Encoding::Native)?;
        let element = crate::memory::scalar_string(charsxp)?;
        runtime.heap.set_element(list, index, element);
        let name = runtime.heap.mk_char(name.as_bytes(), Encoding::Native)?;
        runtime.heap.set_element(names, index, name);
    }
    set_attrib(list, R_NamesSymbol.get(), names)?;
    Ok(list)
}

/// `pushBack(data, connection, newLine)`: the lines are read before anything else, in order,
/// and before any pushback already waiting.
pub unsafe fn push_back(con: Sexp, data: Sexp, newline: bool) -> ApiResult<()> {
    if data.kind() != SexpType::String {
        return Err(error("invalid 'data' argument"));
    }
    let con = resolve(con, "'con' is not a connection")?.1;
    let mut lines = Vec::new();
    for &element in data.slice::<Sexp>() {
        let mut line = utf8_text(element)?.as_bytes().to_vec();
        if newline {
            line.push(b'\n');
        }
        lines.push(line);
    }
    for line in lines.iter().rev() {
        push_back_bytes(con, line);
    }
    Ok(())
}

/// Pushes one line on top of the pushback stack. A partly read top line is first cut to its
/// unread rest, so `posPushBack` can apply to the new top.
unsafe fn push_back_bytes(con: Rconnection, line: &[u8]) {
    if (*con).nPushBack > 0 && (*con).posPushBack > 0 {
        let top_slot = (*con).PushBack.add((*con).nPushBack as usize - 1);
        let rest = c_text((*top_slot).add((*con).posPushBack as usize)).to_vec();
        libc::free((*top_slot).cast());
        *top_slot = malloc_string(&rest);
        (*con).posPushBack = 0;
    }
    let count = (*con).nPushBack as usize + 1;
    let lines =
        libc::realloc((*con).PushBack.cast(), count * size_of::<*mut c_char>()) as *mut *mut c_char;
    if lines.is_null() {
        panic!("out of memory pushing back onto a connection");
    }
    *lines.add(count - 1) = malloc_string(line);
    (*con).PushBack = lines;
    (*con).nPushBack = count as c_int;
}

/// `pushBackLength(connection)`.
pub unsafe fn push_back_length(con: Sexp) -> ApiResult<i32> {
    Ok((*resolve_con(con)?.1).nPushBack)
}

/// `clearPushBack(connection)`.
pub unsafe fn clear_push_back(con: Sexp) -> ApiResult<()> {
    clear_push_back_lines(resolve_con(con)?.1);
    Ok(())
}

/// `rawConnectionValue(con)`: everything written so far.
pub unsafe fn raw_connection_value(con: Sexp) -> ApiResult<Sexp> {
    let (slot, con) = resolve_con(con)?;
    let builtin = entry(slot).is_some_and(|entry| entry.builtin);
    let data = match device(con) {
        Ok(Device::Raw(raw)) if builtin => {
            if (*con).canwrite == 0 {
                return Err(error("'con' is not an output rawConnection"));
            }
            raw.data.clone()
        }
        _ => return Err(error("'con' is not a rawConnection")),
    };
    let vector = alloc_vector(SexpType::Raw, data.len() as R_xlen_t)?;
    vector.slice_mut::<u8>().copy_from_slice(&data);
    Ok(vector)
}

/// `textConnectionValue(con)`: the completed lines written so far.
pub unsafe fn text_connection_value(con: Sexp) -> ApiResult<Sexp> {
    let (slot, con) = resolve_con(con)?;
    let builtin = entry(slot).is_some_and(|entry| entry.builtin);
    match device(con) {
        Ok(Device::TextOutput(output)) if builtin => {
            strings_vector(&output.lines, Encoding::Native)
        }
        Ok(Device::TextInput(_)) if builtin => Err(error("'con' is not an output textConnection")),
        _ => Err(error("'con' is not a textConnection")),
    }
}

/// Reads bytes as `R_ReadConnection` does: straight from the device, past any pushback.
pub unsafe fn read(con: Sexp, buffer: &mut [u8]) -> ApiResult<usize> {
    read_raw(resolve_con(con)?.1, buffer)
}

/// Writes bytes as `R_WriteConnection` does.
pub unsafe fn write_bytes(con: Sexp, bytes: &[u8]) -> ApiResult<usize> {
    write_raw(resolve_con(con)?.1, bytes)
}

unsafe fn read_raw(con: Rconnection, buffer: &mut [u8]) -> ApiResult<usize> {
    if (*con).isopen == 0 {
        return Err(error("connection is not open"));
    }
    if (*con).canread == 0 {
        return Err(error("cannot read from this connection"));
    }
    method_read(con, buffer)
}

unsafe fn write_raw(con: Rconnection, bytes: &[u8]) -> ApiResult<usize> {
    if (*con).isopen == 0 {
        return Err(error("connection is not open"));
    }
    if (*con).canwrite == 0 {
        return Err(error("cannot write to this connection"));
    }
    method_write(con, bytes)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_new_custom_connection(
    description: *const c_char,
    mode: *const c_char,
    class_name: *const c_char,
    ptr: *mut Rconnection,
) -> Sexp {
    enter(|| {
        let mode = c_text(mode);
        if mode.len() > 4 {
            return Err(error("invalid 'mode' argument"));
        }
        let con = allocate_rconn(
            c_text(class_name),
            c_text(description),
            mode,
            &CUSTOM_METHODS,
        );
        (*con).blocking = 0;
        let object = register(con, false)?;
        if !ptr.is_null() {
            *ptr = con;
        }
        Ok(object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetConnection(sConn: Sexp) -> Rconnection {
    enter(|| Ok(resolve(sConn, "invalid connection")?.1))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ReadConnection(con: Rconnection, buf: *mut c_void, n: usize) -> usize {
    enter(|| {
        if n == 0 {
            return Ok(0);
        }
        read_raw(con, std::slice::from_raw_parts_mut(buf as *mut u8, n))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_WriteConnection(con: Rconnection, buf: *mut c_void, n: usize) -> usize {
    enter(|| {
        if n == 0 {
            return Ok(0);
        }
        write_raw(con, std::slice::from_raw_parts(buf as *const u8, n))
    })
}

// The R functions are primitives until the interpreter provides closures for them, so each
// matches its own arguments: by exact name, then by position, against GNU R's formals.

pub(crate) type Primitive =
    unsafe fn(call: Sexp, args: &[(Sexp, Sexp)], env: Sexp) -> ApiResult<Sexp>;

pub(crate) const PRIMITIVES: &[(&str, SexpType, Primitive)] = &[
    ("file", SexpType::Builtin, primitive_file),
    ("gzfile", SexpType::Builtin, primitive_gzfile),
    ("bzfile", SexpType::Builtin, primitive_bzfile),
    ("xzfile", SexpType::Builtin, primitive_xzfile),
    ("pipe", SexpType::Builtin, primitive_pipe),
    ("rawConnection", SexpType::Builtin, primitive_raw_connection),
    (
        "rawConnectionValue",
        SexpType::Builtin,
        primitive_raw_connection_value,
    ),
    (
        "textConnection",
        SexpType::Builtin,
        primitive_text_connection,
    ),
    (
        "textConnectionValue",
        SexpType::Builtin,
        primitive_text_connection_value,
    ),
    ("stdin", SexpType::Builtin, primitive_stdin),
    ("stdout", SexpType::Builtin, primitive_stdout),
    ("stderr", SexpType::Builtin, primitive_stderr),
    ("open", SexpType::Builtin, primitive_open),
    ("close", SexpType::Builtin, primitive_close),
    ("isOpen", SexpType::Builtin, primitive_is_open),
    ("isIncomplete", SexpType::Builtin, primitive_is_incomplete),
    ("readLines", SexpType::Builtin, primitive_read_lines),
    ("writeLines", SexpType::Builtin, primitive_write_lines),
    ("readChar", SexpType::Builtin, primitive_read_char),
    ("readBin", SexpType::Builtin, primitive_read_bin),
    ("writeBin", SexpType::Builtin, primitive_write_bin),
    ("seek", SexpType::Builtin, primitive_seek),
    ("truncate", SexpType::Builtin, primitive_truncate),
    ("flush", SexpType::Builtin, primitive_flush),
    ("summary.connection", SexpType::Builtin, primitive_summary),
    ("pushBack", SexpType::Builtin, primitive_push_back),
    (
        "pushBackLength",
        SexpType::Builtin,
        primitive_push_back_length,
    ),
    (
        "clearPushBack",
        SexpType::Builtin,
        primitive_clear_push_back,
    ),
    ("getConnection", SexpType::Builtin, primitive_get_connection),
    (
        "getAllConnections",
        SexpType::Builtin,
        primitive_get_all_connections,
    ),
    (
        "closeAllConnections",
        SexpType::Builtin,
        primitive_close_all_connections,
    ),
];

/// Matches supplied arguments to `formals`: exact names first, then positions.
unsafe fn match_arguments<const N: usize>(
    call: Sexp,
    formals: [&str; N],
    supplied: &[(Sexp, Sexp)],
) -> ApiResult<[Option<Sexp>; N]> {
    let mut matched = [None; N];
    let mut positional = Vec::new();
    for &(tag, value) in supplied {
        if tag.kind() == SexpType::Symbol {
            let name = utf8_text(tag.car())?;
            match formals.iter().position(|formal| *formal == name) {
                Some(index) if matched[index].is_none() => matched[index] = Some(value),
                Some(_) => {
                    return Err(error_in(
                        call,
                        format!("formal argument \"{name}\" matched by multiple actual arguments"),
                    ));
                }
                None => return Err(error_in(call, format!("unused argument ({name} = ...)"))),
            }
        } else {
            positional.push(value);
        }
    }
    let mut free = (0..N)
        .filter(|&index| matched[index].is_none())
        .collect::<Vec<_>>()
        .into_iter();
    for value in positional {
        let index = free
            .next()
            .ok_or_else(|| error_in(call, "unused argument"))?;
        matched[index] = Some(value);
    }
    Ok(matched)
}

unsafe fn required(call: Sexp, value: Option<Sexp>, name: &str) -> ApiResult<Sexp> {
    value.ok_or_else(|| {
        error_in(
            call,
            format!("argument \"{name}\" is missing, with no default"),
        )
    })
}

unsafe fn string_argument(
    call: Sexp,
    value: Option<Sexp>,
    name: &str,
    default: &str,
) -> ApiResult<String> {
    let Some(value) = value else {
        return Ok(default.to_owned());
    };
    if value.kind() != SexpType::String || value.length() < 1 {
        return Err(error_in(call, format!("invalid '{name}' argument")));
    }
    Ok(utf8_text(value.element(0))?.into_owned())
}

unsafe fn logical_argument(
    call: Sexp,
    value: Option<Sexp>,
    name: &str,
    default: bool,
) -> ApiResult<bool> {
    let Some(value) = value else {
        return Ok(default);
    };
    match crate::vectors::as_logical(value)? {
        NA_INTEGER => Err(error_in(call, format!("invalid '{name}' argument"))),
        flag => Ok(flag != 0),
    }
}

unsafe fn integer_argument(value: Option<Sexp>, default: i32) -> ApiResult<i32> {
    match value {
        Some(value) => crate::vectors::as_integer(value),
        None => Ok(default),
    }
}

fn logical(value: bool) -> ApiResult<Sexp> {
    // SAFETY: allocating a scalar touches no other object.
    unsafe { scalar(SexpType::Logical, value as c_int) }
}

fn invisible_null() -> Sexp {
    runtime().nil()
}

/// The deparsed argument expression, as `deparse(substitute(object))` would give it.
unsafe fn argument_text(call: Sexp, position: usize) -> String {
    let mut cell = call.cdr();
    for _ in 0..position {
        if cell.kind() != SexpType::Pairlist {
            break;
        }
        cell = cell.cdr();
    }
    if cell.kind() != SexpType::Pairlist {
        return String::new();
    }
    crate::eval::deparse(cell.car(), 0)
}

/// A connection argument given as a file name: a temporary connection in `mode`.
unsafe fn connection_argument(con: Sexp, mode: &str) -> ApiResult<(Sexp, bool)> {
    if con.kind() == SexpType::String {
        let path = utf8_text(con.element(0))?.into_owned();
        let object = new_file(&path, mode, true, false)?;
        runtime().heap.root(object);
        Ok((object, true))
    } else {
        Ok((con, false))
    }
}

unsafe fn with_connection_argument(
    con: Sexp,
    mode: &str,
    body: impl FnOnce(Sexp) -> ApiResult<Sexp>,
) -> ApiResult<Sexp> {
    let (con, temporary) = connection_argument(con, mode)?;
    let result = body(con);
    if temporary {
        let closed = close(con);
        return match (result, closed) {
            (Err(unwind), _) => Err(unwind),
            (Ok(_), Err(unwind)) => Err(unwind),
            (Ok(value), Ok(_)) => Ok(value),
        };
    }
    result
}

unsafe fn primitive_file(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [description, open, blocking, _encoding, raw, _method] = match_arguments(
        call,
        [
            "description",
            "open",
            "blocking",
            "encoding",
            "raw",
            "method",
        ],
        args,
    )?;
    let description = string_argument(call, description, "description", "")?;
    let open = string_argument(call, open, "open", "")?;
    let blocking = logical_argument(call, blocking, "blocking", true)?;
    let raw = logical_argument(call, raw, "raw", false)?;
    new_file(&description, &open, blocking, raw)
}

unsafe fn primitive_gzfile(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [description, open, _encoding, compression] = match_arguments(
        call,
        ["description", "open", "encoding", "compression"],
        args,
    )?;
    let description = string_argument(
        call,
        required(call, description, "description").map(Some)?,
        "description",
        "",
    )?;
    let open = string_argument(call, open, "open", "")?;
    new_gzfile(&description, &open, integer_argument(compression, 6)?)
}

unsafe fn primitive_bzfile(_call: Sexp, _args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    new_unsupported("bzfile")
}

unsafe fn primitive_xzfile(_call: Sexp, _args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    new_unsupported("xzfile")
}

unsafe fn primitive_pipe(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [description, open, _encoding] =
        match_arguments(call, ["description", "open", "encoding"], args)?;
    let command = string_argument(
        call,
        Some(required(call, description, "description")?),
        "description",
        "",
    )?;
    let open = string_argument(call, open, "open", "")?;
    new_pipe(&command, &open)
}

unsafe fn primitive_raw_connection(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    let [object, open] = match_arguments(call, ["object", "open"], args)?;
    let object = required(call, object, "object")?;
    let open = string_argument(call, open, "open", "r")?;
    new_raw_connection(argument_text(call, 0).as_bytes(), object, &open)
}

unsafe fn primitive_raw_connection_value(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    let [con] = match_arguments(call, ["con"], args)?;
    raw_connection_value(required(call, con, "con")?)
}

unsafe fn primitive_text_connection(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    let [object, open, local, name, _encoding] =
        match_arguments(call, ["object", "open", "local", "name", "encoding"], args)?;
    let object = required(call, object, "object")?;
    let open = string_argument(call, open, "open", "r")?;
    if logical_argument(call, local, "local", false)? {
        return Err(error_in(
            call,
            "ry cannot create local text connections until the interpreter exists",
        ));
    }
    let description = match name {
        Some(_) => string_argument(call, name, "name", "")?,
        None => argument_text(call, 0),
    };
    let target = if open != "r" && object.kind() == SexpType::String && object.length() == 1 {
        let symbol = runtime().heap.install(object.element(0).bytes())?;
        Some(TextTarget {
            symbol,
            env: runtime().heap.known().global_env,
        })
    } else {
        None
    };
    new_text_connection(description.as_bytes(), object, &open, target)
}

unsafe fn primitive_text_connection_value(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    let [con] = match_arguments(call, ["con"], args)?;
    text_connection_value(required(call, con, "con")?)
}

unsafe fn primitive_stdin(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    match_arguments(call, [], args)?;
    standard(0)
}

unsafe fn primitive_stdout(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    match_arguments(call, [], args)?;
    standard(1)
}

unsafe fn primitive_stderr(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    match_arguments(call, [], args)?;
    standard(2)
}

unsafe fn primitive_open(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con, mode, blocking] = match_arguments(call, ["con", "open", "blocking"], args)?;
    let mode = string_argument(call, mode, "open", "r")?;
    open(
        required(call, con, "con")?,
        &mode,
        logical_argument(call, blocking, "blocking", true)?,
    )?;
    Ok(invisible_null())
}

unsafe fn primitive_close(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con, _type] = match_arguments(call, ["con", "type"], args)?;
    close(required(call, con, "con")?)
}

fn seek_side_argument(text: &str) -> SeekSide {
    match text {
        "read" => SeekSide::Read,
        "write" => SeekSide::Write,
        _ => SeekSide::LastUsed,
    }
}

unsafe fn primitive_is_open(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con, rw] = match_arguments(call, ["con", "rw"], args)?;
    let rw = string_argument(call, rw, "rw", "")?;
    logical(is_open(
        required(call, con, "con")?,
        seek_side_argument(&rw),
    )?)
}

unsafe fn primitive_is_incomplete(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    let [con] = match_arguments(call, ["con"], args)?;
    logical(is_incomplete(required(call, con, "con")?)?)
}

fn encoding_argument(text: &str) -> Encoding {
    match text {
        "UTF-8" | "utf8" => Encoding::Utf8,
        "latin1" => Encoding::Latin1,
        "bytes" => Encoding::Bytes,
        _ => Encoding::Native,
    }
}

unsafe fn primitive_read_lines(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con, n, ok, warn_incomplete, encoding, skip_nul] = match_arguments(
        call,
        ["con", "n", "ok", "warn", "encoding", "skipNul"],
        args,
    )?;
    let con = match con {
        Some(con) => con,
        None => standard(0)?,
    };
    let n = integer_argument(n, -1)?;
    let n = if n == NA_INTEGER { -1 } else { n as i64 };
    let ok = logical_argument(call, ok, "ok", true)?;
    let warn_incomplete = logical_argument(call, warn_incomplete, "warn", true)?;
    let encoding = encoding_argument(&string_argument(call, encoding, "encoding", "unknown")?);
    let skip_nul = logical_argument(call, skip_nul, "skipNul", false)?;
    with_connection_argument(con, "rt", |con| {
        read_lines(con, n, ok, warn_incomplete, encoding, skip_nul)
    })
}

unsafe fn primitive_write_lines(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [text, con, sep, _use_bytes] =
        match_arguments(call, ["text", "con", "sep", "useBytes"], args)?;
    let text = required(call, text, "text")?;
    let con = match con {
        Some(con) => con,
        None => standard(1)?,
    };
    let separator = string_argument(call, sep, "sep", "\n")?;
    with_connection_argument(con, "wt", |con| {
        write_lines(text, con, separator.as_bytes())?;
        Ok(invisible_null())
    })
}

unsafe fn primitive_read_char(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con, nchars, use_bytes] = match_arguments(call, ["con", "nchars", "useBytes"], args)?;
    let con = required(call, con, "con")?;
    let counts = crate::vectors::integers(required(call, nchars, "nchars")?)?;
    let use_bytes = logical_argument(call, use_bytes, "useBytes", false)?;
    with_connection_argument(con, "rb", |con| read_char_strings(con, &counts, use_bytes))
}

/// `what` as `readBin` reads it: a type name, or any object standing for its type.
unsafe fn binary_type(what: Sexp) -> ApiResult<BinaryType> {
    let name = if what.kind() == SexpType::String && what.length() == 1 {
        utf8_text(what.element(0))?.into_owned()
    } else {
        what.kind().name().to_owned()
    };
    Ok(match name.as_str() {
        "numeric" | "double" => BinaryType::Double,
        "integer" | "int" => BinaryType::Integer,
        "logical" => BinaryType::Logical,
        "complex" => BinaryType::Complex,
        "raw" => BinaryType::Raw,
        _ => BinaryType::Character,
    })
}

unsafe fn size_argument(value: Option<Sexp>) -> ApiResult<Option<usize>> {
    match integer_argument(value, NA_INTEGER)? {
        NA_INTEGER => Ok(None),
        size if size <= 0 => Err(error(format!("size {size} is unknown on this machine"))),
        size => Ok(Some(size as usize)),
    }
}

unsafe fn swap_argument(call: Sexp, endian: Option<Sexp>) -> ApiResult<bool> {
    let endian = string_argument(call, endian, "endian", "little")?;
    let native = if cfg!(target_endian = "big") {
        "big"
    } else {
        "little"
    };
    match endian.as_str() {
        "big" | "little" => Ok(endian != native),
        "swap" => Ok(true),
        _ => Err(error_in(call, "invalid 'endian' argument")),
    }
}

unsafe fn primitive_read_bin(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con, what, n, size, signed, endian] =
        match_arguments(call, ["con", "what", "n", "size", "signed", "endian"], args)?;
    let con = required(call, con, "con")?;
    let kind = binary_type(required(call, what, "what")?)?;
    let count = match n {
        Some(n) => crate::vectors::as_real(n)?,
        None => 1.0,
    };
    if is_na_real(count) || count < 0.0 {
        return Err(error_in(call, "invalid 'n' argument"));
    }
    let size = size_argument(size)?;
    let signed = logical_argument(call, signed, "signed", true)?;
    let swap = swap_argument(call, endian)?;
    if con.kind() == SexpType::Raw {
        return read_bin_bytes(con.bytes(), kind, count as usize, size, signed, swap);
    }
    with_connection_argument(con, "rb", |con| {
        read_bin(con, kind, count as usize, size, signed, swap)
    })
}

unsafe fn primitive_write_bin(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [object, con, size, endian, _use_bytes] =
        match_arguments(call, ["object", "con", "size", "endian", "useBytes"], args)?;
    let object = required(call, object, "object")?;
    let con = required(call, con, "con")?;
    let size = size_argument(size)?;
    let swap = swap_argument(call, endian)?;
    if con.kind() == SexpType::Raw {
        let bytes = encode_binary(object, size, swap)?;
        let vector = alloc_vector(SexpType::Raw, bytes.len() as R_xlen_t)?;
        vector.slice_mut::<u8>().copy_from_slice(&bytes);
        return Ok(vector);
    }
    with_connection_argument(con, "wb", |con| {
        write_bin(object, con, size, swap)?;
        Ok(invisible_null())
    })
}

unsafe fn primitive_seek(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con, position, origin, rw] =
        match_arguments(call, ["con", "where", "origin", "rw"], args)?;
    let position = match position {
        Some(position) => crate::vectors::as_real(position)?,
        None => f64::NAN,
    };
    let origin = match string_argument(call, origin, "origin", "start")?.as_str() {
        "current" => SeekOrigin::Current,
        "end" => SeekOrigin::End,
        _ => SeekOrigin::Start,
    };
    let rw = seek_side_argument(&string_argument(call, rw, "rw", "")?);
    let previous = seek(required(call, con, "con")?, position, origin, rw)?;
    scalar(SexpType::Real, previous)
}

unsafe fn primitive_truncate(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con] = match_arguments(call, ["con"], args)?;
    truncate(required(call, con, "con")?)?;
    Ok(invisible_null())
}

unsafe fn primitive_flush(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [con] = match_arguments(call, ["con"], args)?;
    flush(required(call, con, "con")?)?;
    Ok(invisible_null())
}

unsafe fn primitive_summary(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [object, _rest] = match_arguments(call, ["object", "..."], args)?;
    summary(required(call, object, "object")?)
}

unsafe fn primitive_push_back(call: Sexp, args: &[(Sexp, Sexp)], _env: Sexp) -> ApiResult<Sexp> {
    let [data, con, newline, _encoding] =
        match_arguments(call, ["data", "connection", "newLine", "encoding"], args)?;
    let newline = logical_argument(call, newline, "newLine", true)?;
    push_back(
        required(call, con, "connection")?,
        required(call, data, "data")?,
        newline,
    )?;
    Ok(invisible_null())
}

unsafe fn primitive_push_back_length(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    let [con] = match_arguments(call, ["connection"], args)?;
    scalar(
        SexpType::Integer,
        push_back_length(required(call, con, "connection")?)?,
    )
}

unsafe fn primitive_clear_push_back(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    let [con] = match_arguments(call, ["connection"], args)?;
    clear_push_back(required(call, con, "connection")?)?;
    Ok(invisible_null())
}

unsafe fn primitive_get_connection(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    let [what] = match_arguments(call, ["what"], args)?;
    get_connection(integer_argument(
        Some(required(call, what, "what")?),
        NA_INTEGER,
    )?)
}

unsafe fn primitive_get_all_connections(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    match_arguments(call, [], args)?;
    all_connections()
}

unsafe fn primitive_close_all_connections(
    call: Sexp,
    args: &[(Sexp, Sexp)],
    _env: Sexp,
) -> ApiResult<Sexp> {
    match_arguments(call, [], args)?;
    close_all()?;
    Ok(invisible_null())
}

/// How many slots are in use, for tests that check connections are not leaked.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_connection_count() -> c_int {
    (0..NCONNECTIONS)
        .filter(|&slot| entry(slot).is_some())
        .count() as c_int
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The Rust mirror of `struct Rconn` must have GNU R's layout, which
    /// `tests/packages/ryconn/layout.expected` records from GNU R's own header.
    #[test]
    fn the_struct_matches_gnu_r_layout() {
        let expected = include_str!("../tests/packages/ryconn/layout.expected");
        let actual: Vec<(&str, usize)> = vec![
            ("sizeof", size_of::<Rconn>()),
            ("class", offset_of!(Rconn, class)),
            ("description", offset_of!(Rconn, description)),
            ("enc", offset_of!(Rconn, enc)),
            ("mode", offset_of!(Rconn, mode)),
            ("text", offset_of!(Rconn, text)),
            ("isopen", offset_of!(Rconn, isopen)),
            ("incomplete", offset_of!(Rconn, incomplete)),
            ("canread", offset_of!(Rconn, canread)),
            ("canwrite", offset_of!(Rconn, canwrite)),
            ("canseek", offset_of!(Rconn, canseek)),
            ("blocking", offset_of!(Rconn, blocking)),
            ("isGzcon", offset_of!(Rconn, isGzcon)),
            ("open", offset_of!(Rconn, open)),
            ("close", offset_of!(Rconn, close)),
            ("destroy", offset_of!(Rconn, destroy)),
            ("vfprintf", offset_of!(Rconn, vfprintf)),
            ("fgetc", offset_of!(Rconn, fgetc)),
            ("fgetc_internal", offset_of!(Rconn, fgetc_internal)),
            ("seek", offset_of!(Rconn, seek)),
            ("truncate", offset_of!(Rconn, truncate)),
            ("fflush", offset_of!(Rconn, fflush)),
            ("read", offset_of!(Rconn, read)),
            ("write", offset_of!(Rconn, write)),
            ("nPushBack", offset_of!(Rconn, nPushBack)),
            ("posPushBack", offset_of!(Rconn, posPushBack)),
            ("PushBack", offset_of!(Rconn, PushBack)),
            ("save", offset_of!(Rconn, save)),
            ("save2", offset_of!(Rconn, save2)),
            ("encname", offset_of!(Rconn, encname)),
            ("inconv", offset_of!(Rconn, inconv)),
            ("outconv", offset_of!(Rconn, outconv)),
            ("iconvbuff", offset_of!(Rconn, iconvbuff)),
            ("oconvbuff", offset_of!(Rconn, oconvbuff)),
            ("next", offset_of!(Rconn, next)),
            ("init_out", offset_of!(Rconn, init_out)),
            ("navail", offset_of!(Rconn, navail)),
            ("inavail", offset_of!(Rconn, inavail)),
            ("EOF_signalled", offset_of!(Rconn, EOF_signalled)),
            ("UTF8out", offset_of!(Rconn, UTF8out)),
            ("id", offset_of!(Rconn, id)),
            ("ex_ptr", offset_of!(Rconn, ex_ptr)),
            ("private", offset_of!(Rconn, private)),
            ("status", offset_of!(Rconn, status)),
            ("buff", offset_of!(Rconn, buff)),
            ("buff_len", offset_of!(Rconn, buff_len)),
            ("buff_stored_len", offset_of!(Rconn, buff_stored_len)),
            ("buff_pos", offset_of!(Rconn, buff_pos)),
        ];
        for (name, offset) in actual {
            let line = expected
                .lines()
                .find(|line| line.split(' ').next() == Some(name))
                .unwrap_or_else(|| panic!("{name} is not in the expected layout"));
            let recorded: usize = line
                .split(' ')
                .nth(1)
                .and_then(|text| text.parse().ok())
                .unwrap_or_else(|| panic!("a malformed layout line: {line}"));
            assert_eq!(offset, recorded, "{name}");
        }
    }
}
