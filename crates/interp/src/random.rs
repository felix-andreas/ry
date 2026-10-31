//! Random numbers: GNU R's default generators, `set.seed`, `sample` and the distributions base
//! R needs.

use crate::builtins::BuiltinDef;

pub const BUILTINS: &[BuiltinDef] = &[];

#[derive(Default)]
pub struct Rng {}
