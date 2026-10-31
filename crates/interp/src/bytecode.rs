//! The tier-0 bytecode interpreter.

use crate::{
    builtins::BuiltinDef,
    eval,
    unwind::{R, error},
};
use heap::{
    Marker,
    object::{Sexp, SexpType},
};

pub const BUILTINS: &[BuiltinDef] = &[];

#[derive(Default)]
pub struct State {}

impl State {
    pub(crate) unsafe fn trace(&self, marker: &mut Marker<'_>) {
        let _ = marker;
    }
}

/// Runs a byte-code object.
pub unsafe fn execute(code: Sexp, env: Sexp) -> R<Sexp> {
    let _ = (code, env);
    Err(error("cannot run byte code"))
}

/// Runs a closure's body.
pub unsafe fn run_body(function: Sexp, body: Sexp, env: Sexp) -> R<Sexp> {
    let _ = function;
    eval::eval(body, env)
}

/// Records that `symbol` was bound in `env`, which invalidates cached lookups.
pub unsafe fn note_binding(symbol: Sexp, env: Sexp) {
    let _ = (symbol, env);
}

/// A closure's body as source: the expression of compiled code.
pub unsafe fn closure_body(function: Sexp) -> Sexp {
    let body = function.cdr();
    if body.kind() == SexpType::Bytecode {
        return body.cdr().element(0);
    }
    body
}

pub unsafe fn is_compiled(function: Sexp) -> bool {
    function.cdr().kind() == SexpType::Bytecode
}
