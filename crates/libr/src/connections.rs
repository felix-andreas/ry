//! The C side of R's connections. The table and every connection kind live in
//! `interp::connections`; this module exports the `R_ext/Connections.h` entry points and runs a
//! package's own connection methods through the C trampolines in `native.c`, so that an
//! `Rf_error` inside a method comes back as an unwind instead of a `longjmp` across Rust frames.

use crate::boundary::{ApiResult, enter, with_native_scope};
use heap::object::Sexp;
use interp::connections::{
    IntMethod, NativeMethod, OpaqueMethod, OpenMethod, Rconnection, ReadMethod, SeekMethod,
    VoidMethod, WriteMethod,
};
use std::ffi::{CStr, c_char, c_int, c_void};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_new_custom_connection(
    description: *const c_char,
    mode: *const c_char,
    class_name: *const c_char,
    ptr: *mut Rconnection,
) -> Sexp {
    enter(|| {
        let (object, con) =
            interp::connections::new_custom(c_text(description), c_text(mode), c_text(class_name))?;
        if !ptr.is_null() {
            *ptr = con;
        }
        Ok(object)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_GetConnection(sConn: Sexp) -> Rconnection {
    enter(|| Ok(interp::connections::resolve(sConn, "invalid connection")?.1))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_ReadConnection(con: Rconnection, buf: *mut c_void, n: usize) -> usize {
    enter(|| {
        if n == 0 {
            return Ok(0);
        }
        interp::connections::read_raw(con, std::slice::from_raw_parts_mut(buf.cast::<u8>(), n))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_WriteConnection(con: Rconnection, buf: *mut c_void, n: usize) -> usize {
    enter(|| {
        if n == 0 {
            return Ok(0);
        }
        interp::connections::write_raw(con, std::slice::from_raw_parts(buf.cast::<u8>(), n))
    })
}

/// How many connection slots are in use, for tests that check connections are released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ry_connection_count() -> c_int {
    interp::connections::count() as c_int
}

/// The interpreter's `connection_method` hook: runs a package's method inside a native scope.
pub(crate) unsafe fn call_method(con: Rconnection, call: NativeMethod<'_>) -> ApiResult<()> {
    match call {
        NativeMethod::Open { method, result } => {
            with_native_scope(|| ry_connection_open(method, con, result))
        }
        NativeMethod::Void(method) => with_native_scope(|| ry_connection_void(method, con)),
        NativeMethod::Int { method, result } => {
            with_native_scope(|| ry_connection_int(method, con, result))
        }
        NativeMethod::Seek {
            method,
            position,
            origin,
            rw,
            result,
        } => with_native_scope(|| ry_connection_seek(method, con, position, origin, rw, result)),
        NativeMethod::Read {
            method,
            buffer,
            result,
        } => with_native_scope(|| {
            ry_connection_read(
                method,
                buffer.as_mut_ptr().cast(),
                1,
                buffer.len(),
                con,
                result,
            )
        }),
        NativeMethod::Write {
            method,
            bytes,
            result,
        } => with_native_scope(|| {
            ry_connection_write(method, bytes.as_ptr().cast(), 1, bytes.len(), con, result)
        }),
        NativeMethod::Print {
            method,
            text,
            result,
        } => with_native_scope(|| ry_connection_print(method, con, text.as_ptr(), result)),
    }
}

/// `native.c`'s default `vfprintf`, which formats its arguments and calls the connection's
/// `write`. Every connection starts with it, so C code that calls `con->vfprintf` works.
pub(crate) fn default_vfprintf() -> OpaqueMethod {
    ry_connection_vfprintf
}

unsafe fn c_text<'a>(text: *const c_char) -> &'a [u8] {
    if text.is_null() {
        b""
    } else {
        CStr::from_ptr(text).to_bytes()
    }
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
