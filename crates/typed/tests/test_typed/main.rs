//! The typed core's tests, in one binary so that an edit to the crate relinks once. Each module
//! is one area; run one with `cargo test -p typed --test test_typed <module>::`.

mod annotations;
mod file;
mod fusion;
mod loops;
mod ranges;
mod unbox;
mod values;
