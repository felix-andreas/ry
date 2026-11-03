//! An R error raised while `dlopen` runs a library's static initializers reaches `dyn.load`'s
//! caller as an ordinary error, as in GNU R. The jump leaves the dynamic loader's lock with
//! the jumping thread, so this binary runs on the main thread (`harness = false`), as ry's
//! runtime does: the lock is recursive, so later loads and the exit handlers still run.
//!
//! A library built from Fortran without registration names `subroutine cube` as `cube_`, and
//! `.Fortran("cube", ...)` finds it by the name without the underscore, as GNU R's `R_dlsym`
//! does.
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
    let fortran = build_package("fortransym", Headers::Ry);
    r.eval("dyn.load", &[(None, r.string(&fortran.to_string_lossy()))])
        .expect("the Fortran-style library loads");
    let values = r.keep(r.real(1.0));
    let values = r
        .eval("c", &[(None, values), (None, r.real(2.0)), (None, r.real(3.0))])
        .expect("a double vector");
    let result = r
        .eval(
            ".Fortran",
            &[
                (None, r.string("cube")),
                (Some("n"), r.integer(3)),
                (Some("x"), values),
            ],
        )
        .expect(".Fortran finds cube_ as cube");
    assert_eq!(r.reals(r.element(result, 1)), vec![1.0, 8.0, 27.0]);
    r.load(&build_package("rytest", Headers::Ry));
    println!("test_load_errors: ok");
}

/// The test exercises `dlopen`, which Windows does not have.
#[cfg(not(unix))]
fn main() {}
