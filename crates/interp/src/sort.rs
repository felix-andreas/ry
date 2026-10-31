//! Sorting, ordering, matching and set operations.

use crate::{
    builtins::BuiltinDef,
};
use std::cmp::Ordering;

pub const BUILTINS: &[BuiltinDef] = &[];

/// Compares two strings in the C collation GNU R uses for `sort` and `ls` under `LC_COLLATE=C`.
pub fn collate(left: &str, right: &str) -> Ordering {
    left.cmp(right)
}

pub fn sort_strings(names: &mut [String]) {
    names.sort_by(|left, right| collate(left, right));
}
