//! Facts about ry's own records, tuples, tags, dictionaries and sets, and about the R object systems it
//! discourages, that both the checker and the runtime need, kept here so the two cannot drift apart.

use std::{collections::HashSet, sync::LazyLock};

/// The R functions that accept a record, tuple, tag, dictionary or set as a top-level argument.
/// The standard library's own functions are not listed: `types/std.Rtypes` declares them for the
/// checker, and the runtime marks them with `BuiltinDef::accepting_native`. Every other builtin
/// and internal refuses one with the runtime's boundary error before it runs, which is what keeps code
/// written for R lists from silently misreading these values. The list holds the builtins that
/// understand the values, the ones that only ask what a value is, and the ones that store a value
/// without looking inside it.
pub const ACCEPTS_NATIVE: &[&str] = &[
    // Understand native values.
    ".ry_record",
    ".ry_tuple",
    ".ry_tag",
    ".ry_array",
    ".ry_is_list",
    ".ry_update",
    ".ry_text",
    "as.record",
    "as.tuple",
    "as.tag",
    "as.list.ry_record",
    "as.list.ry_tuple",
    "as.list.ry_tag",
    "format.ry_record",
    "format.ry_tuple",
    "format.ry_tag",
    "all.equal.ry_record",
    "all.equal.ry_tuple",
    "all.equal.ry_tag",
    "as.list.ry_dict",
    "as.list.ry_set",
    "format.ry_dict",
    "format.ry_set",
    "all.equal.ry_dict",
    "all.equal.ry_set",
    "format.ry_number",
    "all.equal.ry_number",
    "print.default",
    "deparse",
    "dput",
    "identical",
    "==",
    "!=",
    "length",
    "[[",
    "[[<-",
    ".subset2",
    // Ask what a value is.
    "typeof",
    "class",
    "oldClass",
    ".class2",
    "inherits",
    "is.null",
    "is.list",
    "is.function",
    "is.vector",
    "is.atomic",
    "is.numeric",
    "is.character",
    "is.logical",
    "is.integer",
    "is.double",
    "is.complex",
    "is.raw",
    "is.symbol",
    "is.name",
    "is.environment",
    "is.object",
    "is.pairlist",
    "is.call",
    "is.language",
    "is.primitive",
    "is.expression",
    "is.recursive",
    "isS4",
    "refcnt",
    // Store or pass a value along without looking inside it.
    "list",
    "invisible",
    "return",
    "assign",
    "withVisible",
    "lapply",
    "vapply",
    "mapply",
    "do.call",
    "environment<-",
    ".Call",
    ".External",
    ".External2",
];

/// Whether a call to the R function `name` may take a ry record, tuple or tag as a top-level
/// argument. Besides [`ACCEPTS_NATIVE`], that holds for a generic with a method for the values
/// (`format` dispatches to `format.ry_record`) and for `print`, whose default method prints them.
pub fn accepts_native(name: &str) -> bool {
    // Asked on every call the checker sees, so the list is indexed once.
    static ACCEPTING: LazyLock<HashSet<&str>> = LazyLock::new(|| {
        let generics = ACCEPTS_NATIVE.iter().filter_map(|accepted| {
            [
                ".ry_record",
                ".ry_tuple",
                ".ry_tag",
                ".ry_dict",
                ".ry_set",
                ".ry_number",
            ]
            .iter()
            .find_map(|suffix| accepted.strip_suffix(suffix))
        });
        ACCEPTS_NATIVE
            .iter()
            .copied()
            .chain(generics)
            .chain(["print"])
            .collect()
    });
    ACCEPTING.contains(name)
}

/// What a function that defines a class of an R object system is: the system's name, what one
/// call defines, and the package that owns the function.
pub fn object_system_function(
    function: &str,
) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match function {
        "setClass" => ("S4 classes", "an S4 class", "methods"),
        "setClassUnion" => ("S4 classes", "an S4 class union", "methods"),
        "setGeneric" => ("S4 classes", "an S4 generic", "methods"),
        "setMethod" => ("S4 classes", "an S4 method", "methods"),
        "setValidity" => ("S4 classes", "an S4 validity check", "methods"),
        "setReplaceMethod" => ("S4 classes", "an S4 replacement method", "methods"),
        "setOldClass" => ("S4 classes", "an S4 registration of an S3 class", "methods"),
        "setAs" => ("S4 classes", "an S4 coercion", "methods"),
        "setRefClass" => ("Reference Classes", "a Reference Class", "methods"),
        "R6Class" => ("R6 classes", "an R6 class", "R6"),
        _ => return None,
    })
}

/// The error for a definition while `[check] object-systems = false`, shared by the lint and the
/// runtime.
pub fn object_systems_disabled_message(system: &str, defines: &str, function: &str) -> String {
    format!(
        "{system} are turned off in this project (`[check] object-systems = false`), so `{function}` cannot define {defines}. Use a record or a tagged union instead"
    )
}

/// Whether the base R function `name` reads or writes the frame it is called from, or records the
/// call made there, as `stop` and `warning` do for their message. Native code passes
/// an argument made only of R code as a promise in the function's enclosing environment, which
/// such a function would see instead of the frame the interpreter gives it, so a call of one is
/// never passed that way.
pub fn reads_its_frame(name: &str) -> bool {
    const FRAME: [&str; 31] = [
        "Recall",
        "assign",
        "bquote",
        "delayedAssign",
        "do.call",
        "environment",
        "eval",
        "evalq",
        "exists",
        "expression",
        "get",
        "get0",
        "library",
        "local",
        "ls",
        "match.arg",
        "match.call",
        "mget",
        "missing",
        "nargs",
        "on.exit",
        "parent.frame",
        "quote",
        "require",
        "rm",
        "stop",
        "stopifnot",
        "subset",
        "substitute",
        "transform",
        "warning",
    ];
    FRAME.contains(&name) || name.starts_with("sys.") || name.starts_with("with")
}

/// Whether native code must not call the base R function `name`: it reads or changes the
/// variables or the call of the frame it is called from, and native code calls R from the
/// function's enclosing environment. `stop`, `warning` and `stopifnot` only record the call for
/// their message, which native code supplies.
pub fn uses_its_frame(name: &str) -> bool {
    reads_its_frame(name) && !matches!(name, "stop" | "warning" | "stopifnot")
}
