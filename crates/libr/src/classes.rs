//! Classes: the implicit class of an object, and the S4 slot and class functions.
//!
//! An S4 slot is an attribute. A slot whose value is `NULL` is stored as the special symbol
//! `\001NULL\001`, because setting an attribute to `NULL` removes it. The functions that need
//! the class registry (`R_do_MAKE_CLASS`, `R_getClassDef`) call into the `methods` package
//! through the interpreter, as GNU R does.

use crate::{
    Rboolean,
    attributes::{get_attrib, install_attribute, raw_attribute, remove_attribute},
    boundary::{ApiResult, enter, error},
    eval::call_namespaced,
    globals::{R_ClassSymbol, R_DimSymbol, R_PackageSymbol},
    memory::{duplicate, scalar_string},
    runtime,
    strings::{c_bytes, utf8_text},
};
use heap::{
    object::{Sexp, SexpType, gp},
    strings::Encoding,
};
use std::ffi::{c_char, c_int};

/// The implicit class of `object`: its `class` attribute when it has one, otherwise the class
/// R derives from its dimensions and type (`R_data_class`). With `single`, only the first
/// element.
pub unsafe fn data_class(object: Sexp, single: bool) -> ApiResult<Sexp> {
    let runtime = runtime();
    let class = raw_attribute(object, R_ClassSymbol.get());
    if class.kind() == SexpType::String && class.length() > 0 {
        if single {
            return scalar_string(class.element(0));
        }
        return Ok(class);
    }
    let dim = raw_attribute(object, R_DimSymbol.get());
    let names: &[&str] = if dim.kind() != SexpType::Nil && dim.length() == 2 {
        if single {
            &["matrix"]
        } else {
            &["matrix", "array"]
        }
    } else if dim.kind() != SexpType::Nil {
        &["array"]
    } else {
        match object.kind() {
            SexpType::Closure | SexpType::Special | SexpType::Builtin => &["function"],
            SexpType::Real => &["numeric"],
            SexpType::Symbol => &["name"],
            SexpType::Language => &[language_class(object)],
            kind => &[kind.name()],
        }
    };
    let result = runtime
        .heap
        .alloc_vector(SexpType::String, names.len() as isize)?;
    runtime.heap.root(result);
    for (index, name) in names.iter().enumerate() {
        let name = runtime.heap.mk_char(name.as_bytes(), Encoding::Native)?;
        runtime.heap.set_element(result, index, name);
    }
    Ok(result)
}

/// The class of a call: the control-flow and grouping operators have classes of their own.
unsafe fn language_class(call: Sexp) -> &'static str {
    let head = call.car();
    if head.kind() == SexpType::Symbol {
        match head.car().bytes() {
            b"if" => return "if",
            b"while" => return "while",
            b"for" => return "for",
            b"=" => return "=",
            b"<-" => return "<-",
            b"(" => return "(",
            b"{" => return "{",
            _ => {}
        }
    }
    "call"
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_data_class(object: Sexp, single: Rboolean) -> Sexp {
    enter(|| data_class(object, single != 0))
}

/// `R_class(x)`: `class(x)` as R code sees it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_class(object: Sexp) -> Sexp {
    enter(|| data_class(object, false))
}

/// The symbol that stands for a slot whose value is `NULL`.
unsafe fn null_slot() -> ApiResult<Sexp> {
    Ok(runtime().heap.install(b"\x01NULL\x01")?)
}

unsafe fn slot_symbol(name: Sexp) -> ApiResult<Sexp> {
    match name.kind() {
        SexpType::Symbol => Ok(name),
        SexpType::String if crate::vectors::xlength(name)? == 1 => Ok(runtime()
            .heap
            .install_char(crate::vectors::sexp_elt(name, 0)?)?),
        _ => Err(error(format!("invalid type or length for slot name"))),
    }
}

unsafe fn class_label(object: Sexp) -> String {
    let class = raw_attribute(object, R_ClassSymbol.get());
    if class.kind() == SexpType::String && class.length() > 0 {
        utf8_text(class.element(0))
            .map(|text| text.into_owned())
            .unwrap_or_default()
    } else {
        object.kind().name().to_owned()
    }
}

pub unsafe fn get_slot(object: Sexp, name: Sexp) -> ApiResult<Sexp> {
    let symbol = slot_symbol(name)?;
    if symbol.car().bytes() == b".Data" {
        return call_namespaced("methods", "getDataPart", &[object]);
    }
    let value = get_attrib(object, symbol)?;
    if value.kind() == SexpType::Nil {
        let slot = utf8_text(symbol.car())?.into_owned();
        if symbol == R_ClassSymbol.get() {
            return data_class(object, true);
        }
        if slot == ".S3Class" {
            return data_class(object, false);
        }
        if slot == "names" && object.kind() == SexpType::List {
            return Ok(value);
        }
        return Err(error(format!(
            "no slot of name \"{slot}\" for this object of class \"{}\"",
            class_label(object)
        )));
    }
    if value == null_slot()? {
        return Ok(runtime().nil());
    }
    Ok(value)
}

pub unsafe fn set_slot(object: Sexp, name: Sexp, value: Sexp) -> ApiResult<Sexp> {
    let symbol = slot_symbol(name)?;
    if symbol.car().bytes() == b".Data" {
        return call_namespaced("methods", "setDataPart", &[object, value]);
    }
    let value = if value.kind() == SexpType::Nil {
        null_slot()?
    } else {
        value
    };
    install_attribute(object, symbol, value)?;
    if symbol == R_ClassSymbol.get() {
        object.set_object(true);
    }
    Ok(object)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_slot(object: Sexp, name: Sexp) -> Sexp {
    enter(|| get_slot(object, name))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_slot_assign(object: Sexp, name: Sexp, value: Sexp) -> Sexp {
    enter(|| set_slot(object, name, value))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_has_slot(object: Sexp, name: Sexp) -> c_int {
    enter(|| {
        let symbol = slot_symbol(name)?;
        if symbol.car().bytes() == b".Data" && object.kind() != SexpType::Object {
            return Ok(1);
        }
        Ok((raw_attribute(object, symbol).kind() != SexpType::Nil) as c_int)
    })
}

/// `R_do_MAKE_CLASS(what)`: the class definition of the class called `what`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_MAKE_CLASS(what: *const c_char) -> Sexp {
    enter(|| {
        if what.is_null() {
            return Err(error(
                "C level MAKE_CLASS macro called with NULL string pointer",
            ));
        }
        let name = crate::strings::mk_char(c_bytes(what), Encoding::Native)?;
        let name = scalar_string(name)?;
        runtime().heap.root(name);
        call_namespaced("methods", "getClass", &[name])
    })
}

/// `R_getClassDef(what)`: the class definition, or `NULL` when no class is called `what`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_getClassDef(what: *const c_char) -> Sexp {
    enter(|| {
        if what.is_null() {
            return Err(error("R_getClassDef(.) called with NULL string pointer"));
        }
        let name = crate::strings::mk_char(c_bytes(what), Encoding::Native)?;
        let name = scalar_string(name)?;
        runtime().heap.root(name);
        call_namespaced("methods", "getClassDef", &[name])
    })
}

/// `R_do_new_object(class_def)`: a fresh object of the class, copied from its prototype.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_do_new_object(class_def: Sexp) -> Sexp {
    enter(|| new_object(class_def))
}

pub unsafe fn new_object(class_def: Sexp) -> ApiResult<Sexp> {
    if class_def.kind() == SexpType::Nil {
        return Err(error(
            "C level NEW macro called with null class definition pointer",
        ));
    }
    let runtime = runtime();
    let virtual_symbol = runtime.heap.install(b"virtual")?;
    let is_virtual = get_attrib(class_def, virtual_symbol)?;
    if is_virtual.kind() != SexpType::Nil && crate::vectors::as_logical(is_virtual)? == 1 {
        let class_name = get_slot(class_def, runtime.heap.install(b"className")?)?;
        let label = if class_name.kind() == SexpType::String && class_name.length() > 0 {
            utf8_text(class_name.element(0))?.into_owned()
        } else {
            String::new()
        };
        return Err(error(format!(
            "cannot allocate an object of a virtual class (\"{label}\")"
        )));
    }
    let class_name = get_slot(class_def, runtime.heap.install(b"className")?)?;
    runtime.heap.root(class_name);
    let prototype = get_slot(class_def, runtime.heap.install(b"prototype")?)?;
    let value = duplicate(prototype, true)?;
    runtime.heap.root(value);
    if value.kind() == SexpType::Object
        || raw_attribute(class_name, R_PackageSymbol.get()).kind() != SexpType::Nil
    {
        install_attribute(value, R_ClassSymbol.get(), class_name)?;
        value.set_object(true);
        value.set_gp_bits(gp::S4_OBJECT, true);
    }
    Ok(value)
}

/// `Rf_asS4(x, flag, complete)`: `x` with its S4 bit set (`flag`) or cleared. Clearing it with
/// `complete` turns an S4 object that extends a basic type into that basic type, through
/// `methods`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Rf_asS4(object: Sexp, flag: Rboolean, complete: c_int) -> Sexp {
    enter(|| {
        let flag = flag != 0;
        if flag == object.has_gp(gp::S4_OBJECT) {
            return Ok(object);
        }
        let object = if object.refcount() > 1 {
            let copy = crate::memory::duplicate(object, false)?;
            runtime().heap.root(copy);
            copy
        } else {
            object
        };
        if flag {
            object.set_gp_bits(gp::S4_OBJECT, true);
            return Ok(object);
        }
        if complete != 0 && object.kind() == SexpType::Object {
            let data = call_namespaced("methods", "getDataPart", &[object]);
            return match data {
                Ok(data) => Ok(data),
                Err(unwind) if complete == 1 => Err(unwind),
                Err(_) => Ok(object),
            };
        }
        object.set_gp_bits(gp::S4_OBJECT, false);
        Ok(object)
    })
}

/// `R_check_class_etc(x, valid)`: the index into the null-terminated array `valid` of the
/// first class of `x` it names, or -1. For an S4 object whose own class is not listed, the
/// superclasses `methods` reports are tried in order.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn R_check_class_etc(object: Sexp, valid: *const *const c_char) -> c_int {
    enter(|| {
        let mut names = Vec::new();
        let mut cursor = valid;
        while !(*cursor).is_null() {
            names.push(c_bytes(*cursor));
            cursor = cursor.add(1);
        }
        let find = |class: &[u8]| names.iter().position(|name| *name == class);
        let class = raw_attribute(object, R_ClassSymbol.get());
        if class.kind() == SexpType::String {
            for index in 0..class.length() as usize {
                if let Some(found) = find(class.element(index).bytes()) {
                    return Ok(found as c_int);
                }
            }
        }
        if object.has_gp(gp::S4_OBJECT) && class.kind() == SexpType::String && class.length() > 0 {
            let class_def = R_getClassDef_bytes(class.element(0).bytes())?;
            runtime().heap.root(class_def);
            if class_def.kind() != SexpType::Nil {
                let contains = get_slot(class_def, runtime().heap.install(b"contains")?)?;
                let names_attribute = get_attrib(contains, crate::globals::R_NamesSymbol.get())?;
                if names_attribute.kind() == SexpType::String {
                    for index in 0..names_attribute.length() as usize {
                        if let Some(found) = find(names_attribute.element(index).bytes()) {
                            return Ok(found as c_int);
                        }
                    }
                }
            }
        }
        Ok(-1)
    })
}

#[allow(non_snake_case)]
unsafe fn R_getClassDef_bytes(name: &[u8]) -> ApiResult<Sexp> {
    let name = crate::strings::mk_char(name, Encoding::Native)?;
    let name = scalar_string(name)?;
    runtime().heap.root(name);
    call_namespaced("methods", "getClassDef", &[name])
}

/// Removes the `class` attribute and the object bits, which `R_do_slot_assign` never does but
/// `unclass` needs.
pub unsafe fn unclass(object: Sexp) {
    remove_attribute(object, R_ClassSymbol.get());
    object.set_object(false);
    object.set_gp_bits(gp::S4_OBJECT, false);
}
