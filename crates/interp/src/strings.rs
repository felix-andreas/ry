//! Character vectors: `paste`, `nchar`, `substr`, regular expressions, `sprintf` and friends.

use crate::builtins::BuiltinDef;

pub const BUILTINS: &[BuiltinDef] = &[];

/// Translates an R regular expression (POSIX extended, as TRE reads it, or PCRE) into the
/// syntax of the `regex` crate.
pub fn translate_regex(pattern: &str, extended: bool) -> String {
    let _ = extended;
    pattern.to_owned()
}
