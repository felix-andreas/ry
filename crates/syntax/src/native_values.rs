//! Facts about ry's own records, tuples, tags, dictionaries and sets that both the checker and the runtime need,
//! kept here so the two cannot drift apart.

use std::collections::HashSet;
use std::sync::LazyLock;

/// The R functions that accept a record, tuple, tag, dictionary or set as a top-level argument.
/// The standard library's own functions are not listed: `types/ry.Rtypes` declares them for the
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
    "print.default",
    "deparse",
    "dput",
    "identical",
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
    // Store or pass a value along without looking inside it.
    "list",
    "invisible",
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
            [".ry_record", ".ry_tuple", ".ry_tag", ".ry_dict", ".ry_set"]
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
