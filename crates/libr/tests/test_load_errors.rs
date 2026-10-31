//! An R error raised while `dlopen` runs a library's static initializers reaches `dyn.load`'s
//! caller as an ordinary error, as in GNU R. The jump leaves the dynamic loader's lock with
//! the jumping thread, so this binary runs on the main thread (`harness = false`), as ry's
//! runtime does: the lock is recursive, so later loads and the exit handlers still run.

mod harness;

use harness::*;

fn main() {
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
    r.load(&build_package("rytest", Headers::Ry));
    println!("test_load_errors: ok");
}
