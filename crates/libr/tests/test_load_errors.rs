//! An R error raised while `dlopen` runs a library's static initializers reaches `dyn.load`'s
//! caller as an ordinary error, as in GNU R. The jump leaves the dynamic loader's lock with
//! the jumping thread, so this binary runs on the main thread (`harness = false`), as ry's
//! runtime does: the lock is recursive, so later loads and the exit handlers still run.
//!
//! GNU R's `tcltk.so` is compiled against GNU R's internal object layout, so ry refuses it as a
//! GNU R built without Tcl/Tk would, before the loader ever opens it.

#[cfg(unix)]
mod harness;

#[cfg(unix)]
fn main() {
    use harness::*;
    let r = r();
    let failing = build_package("initerror", Headers::Ry);
    let path = r.string(&failing.to_string_lossy());
    let error = r
        .eval("dyn.load", &[(None, path)])
        .expect_err("the static initializer raises an error");
    assert!(
        error.contains("initerror: the static initializer failed"),
        "{error}"
    );
    assert_eq!(r.runtime_protect_depth(), 0);
    let tcltk = r.string("/usr/lib/R/library/tcltk/libs/tcltk.so");
    let error = r
        .eval("dyn.load", &[(None, tcltk)])
        .expect_err("GNU R's tcltk library is refused");
    assert!(
        error.contains("Tcl/Tk support is not available on this system"),
        "{error}"
    );
    r.load(&build_package("rytest", Headers::Ry));
    println!("test_load_errors: ok");
}

/// The test exercises `dlopen`, which Windows does not have.
#[cfg(not(unix))]
fn main() {}
