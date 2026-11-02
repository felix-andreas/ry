//! `.Internal(inspect(x, deep, pvec))` and the `R_inspect` entry points: a dump of an object's
//! cells in GNU R's format, which packages parse to test object bits and ALTREP classes.
//!
//! ry's heap has no generations or size classes, so every cell reports `g0c0`, and it has no
//! symbol values or hashed frames in GNU R's sense, so symbols never say `(has value)` and an
//! environment shows only its enclosure.

use {
    crate::{
        altrep,
        boundary::{ApiResult, enter, write_console},
        vectors,
    },
    heap::object::{Sexp, SexpType, gp},
    interp::{
        builtins::{BuiltinDef, Call, internal},
        format::format_g,
        values,
    },
    std::ffi::c_int,
};

pub const BUILTINS: &[BuiltinDef] =
    &[internal("inspect", -1, |call| unsafe { do_inspect(call) }).invisible()];

/// `R_inspect`: the whole tree of `object`, five elements per vector.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_inspect(object: Sexp) -> Sexp {
    unsafe {
        enter(|| {
            tree(0, object, -1, 5)?;
            Ok(object)
        })
    }
}

/// `R_inspect3`: the tree of `object` down to `deep` levels, `pvec` elements per vector.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_inspect3(object: Sexp, deep: c_int, pvec: c_int) -> Sexp {
    unsafe {
        enter(|| {
            tree(0, object, deep, pvec)?;
            Ok(object)
        })
    }
}

unsafe fn do_inspect(call: &Call<'_>) -> ApiResult<Sexp> {
    let object = call.arg(0);
    let deep = if call.len() > 1 {
        values::as_integer_scalar(call.arg(1))
    } else {
        -1
    };
    let pvec = if call.len() > 2 {
        values::as_integer_scalar(call.arg(2))
    } else {
        5
    };
    tree(0, object, deep, pvec)?;
    Ok(object)
}

/// The subtree callback an ALTREP class's inspect method prints its parts through.
unsafe extern "C" fn subtree(object: Sexp, pre: c_int, deep: c_int, pvec: c_int) {
    unsafe { enter(|| tree(pre, object, deep, pvec)) }
}

unsafe fn tree(pre: c_int, object: Sexp, deep: c_int, pvec: c_int) -> ApiResult<()> {
    let mut out = indent(pre);
    let kind = object.kind();
    let mut flags: Vec<String> = Vec::new();
    if object.is_object() {
        flags.push("OBJ".into());
    }
    if object.refcount() > 0 {
        flags.push(format!("REF({})", object.refcount()));
    }
    if object.has_gp(gp::S4_OBJECT) && kind != SexpType::Char {
        flags.push("S4".into());
    }
    let attributes = if kind == SexpType::Char {
        values::nil()
    } else {
        object.attrib()
    };
    if attributes != values::nil() {
        flags.push("ATT".into());
    }
    out.push_str(&format!(
        "@{:x} {:02} {} g0c0 [{}] ",
        object.as_ptr() as usize,
        kind.code(),
        kind.constant_name(),
        flags.join(",")
    ));
    if object.is_altrep() {
        write_console(out.as_bytes(), false);
        out.clear();
        if altrep::inspect(object, pre, deep, pvec, subtree)? {
            return print_attributes(pre, attributes, deep, pvec);
        }
    }
    let count = if kind.is_vector() {
        values::length(object)
    } else {
        0
    };
    if matches!(
        kind,
        SexpType::List
            | SexpType::String
            | SexpType::Logical
            | SexpType::Integer
            | SexpType::Raw
            | SexpType::Real
            | SexpType::Complex
            | SexpType::Expression
    ) {
        out.push_str(&format!("(len={count}, tl=0)"));
    }
    match kind {
        SexpType::Char => {
            for (bit, label) in [
                (gp::BYTES, "[bytes] "),
                (gp::LATIN1, "[latin1] "),
                (gp::UTF8, "[UTF8] "),
                (gp::ASCII, "[ASCII] "),
                (gp::CACHED, "[cached] "),
            ] {
                if object.has_gp(bit) {
                    out.push_str(label);
                }
            }
            out.push_str(&format!("\"{}\"", String::from_utf8_lossy(object.bytes())));
        }
        SexpType::Symbol => out.push_str(&format!("\"{}\"", symbol_text(object))),
        SexpType::Logical | SexpType::Integer | SexpType::Raw | SexpType::Real => {
            let shown = count.min(pvec.max(0) as usize);
            for index in 0..shown {
                out.push_str(if index > 0 { "," } else { " " });
                // GNU R prints the raw C values, so `NA_integer_` shows as `-2147483648` and
                // `NA_real_` as `nan`.
                match kind {
                    SexpType::Real => out.push_str(&format_g(vectors::real_elt(object, index)?)),
                    SexpType::Raw => out.push_str(&format!("{:02x}", values::raws(object)[index])),
                    _ => out.push_str(&vectors::int_elt(object, index)?.to_string()),
                }
            }
            if shown < count {
                out.push_str(",...");
            }
        }
        _ => {}
    }
    out.push('\n');
    write_console(out.as_bytes(), false);
    if deep != 0 {
        match kind {
            SexpType::List | SexpType::Expression | SexpType::String => {
                let shown = count.min(pvec.max(0) as usize);
                for index in 0..shown {
                    tree(pre + 2, vectors::sexp_elt(object, index)?, deep - 1, pvec)?;
                }
                if shown < count {
                    write_console(format!("{}  ...\n", indent(pre)).as_bytes(), false);
                }
            }
            SexpType::Pairlist | SexpType::Language => {
                let mut cell = object;
                while cell != values::nil() {
                    if !matches!(cell.kind(), SexpType::Pairlist | SexpType::Language) {
                        write_console(format!("{}.\n", indent(pre + 2)).as_bytes(), false);
                        tree(pre + 2, cell, deep - 1, pvec)?;
                        break;
                    }
                    if cell.tag() != values::nil() {
                        write_console(format!("{}TAG: ", indent(pre + 2)).as_bytes(), false);
                        tree(0, cell.tag(), deep - 1, pvec)?;
                    }
                    tree(pre + 2, cell.car(), deep - 1, pvec)?;
                    cell = cell.cdr();
                }
            }
            SexpType::Environment => {
                write_console(format!("{}ENCLOS:\n", indent(pre)).as_bytes(), false);
                tree(pre + 2, object.enclosure(), 0, pvec)?;
            }
            SexpType::Closure => {
                for (label, part, depth) in [
                    ("FORMALS", object.car(), deep - 1),
                    (
                        "BODY",
                        crate::lists::bytecode_expression(object.cdr()),
                        deep - 1,
                    ),
                    ("CLOENV", object.tag(), 0),
                ] {
                    write_console(format!("{}{label}:\n", indent(pre)).as_bytes(), false);
                    tree(pre + 2, part, depth, pvec)?;
                }
            }
            _ => {}
        }
    }
    print_attributes(pre, attributes, deep, pvec)
}

unsafe fn print_attributes(
    pre: c_int,
    attributes: Sexp,
    deep: c_int,
    pvec: c_int,
) -> ApiResult<()> {
    if attributes != values::nil() {
        write_console(format!("{}ATTRIB:\n", indent(pre)).as_bytes(), false);
        tree(pre + 2, attributes, deep, pvec)?;
    }
    Ok(())
}

fn indent(pre: c_int) -> String {
    " ".repeat(pre.max(0) as usize)
}

unsafe fn symbol_text(symbol: Sexp) -> String {
    if symbol.kind() == SexpType::Symbol {
        String::from_utf8_lossy(symbol.car().bytes()).into_owned()
    } else {
        String::new()
    }
}
