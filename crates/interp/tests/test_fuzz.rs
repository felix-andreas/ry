//! Fuzz and property coverage for the interpreter's stages, run without base R or libR:
//!
//! - lowering never panics on mutated R source, and a lowered expression deparses to text that
//!   lowers back to an identical expression;
//! - the tier-0 bytecode agrees with the AST evaluator on random arithmetic, comparison, logic,
//!   branch and loop programs, including NA, integer overflow and errors, and on random reads and
//!   writes of attributes, names and elements and `c()` calls, whose compiled calls take the
//!   builtins' direct entries;
//! - tier 0 resumes a call at every pc it reaches with the result it gives alone, the way it
//!   resumes after the optimizing tier deoptimizes;
//! - a compiled call to a base function agrees with the AST evaluator while random code shadows,
//!   rebinds and moves the name's bindings;
//! - the closure frame stack agrees with a vector of the live frames, and a popped frame's slot
//!   keeps nothing alive;
//! - reading a corrupted serialized stream fails with an R error, never a panic;
//! - a compiled closure's code survives the code image's encoding, and a corrupted image or code
//!   object fails with an error, never a panic.
//!
//! The contracts live in `interp::testing`, shared with the coverage-guided `fuzz/` targets.
//!
//! `FUZZ_ITERS` raises the budget.

use {
    interp::{
        deparse, lower,
        testing::{
            check_argument_reference_agreement, check_bytecode_agreement, check_code_image_file,
            check_code_image_load, check_code_image_round_trip, check_element_access_agreement,
            check_environment_agreement, check_frame_stack, check_fused_agreement,
            check_image_bodies_round_trip, check_lowering_input, check_quickening_agreement,
            check_resume_at_every_pc, check_s3_lookup_agreement, check_shadowing_agreement,
            check_unserialize_input, with_interpreter,
        },
        values,
    },
    syntax::testing::{SplitMix64, for_each_seed, mutate_bytes},
};

const SEEDS: &[&str] = &[
    "f <- function(x, y = 2) { if (x > y) x - y else y * 2 }",
    "for (i in 1:10) { s <- s + i^2; if (s > 50) break }",
    "x[[\"a\"]] <- list(b = 1L, c = NA_character_)",
    "while (TRUE) { n <- n %/% 2L; if (n == 0) break }",
    "g <- function(...) ..1 + length(list(...))",
    "y <- -3:3 * 1e-3 + 0x1Fp2",
    "`my var` <- c(a = TRUE, b = FALSE) && !is.na(z)",
    "repeat { k <- k + 1; next }",
    "h <- \\(a) a |> sum(na.rm = TRUE)",
    "if (a) b else if (c) d else { e; f }",
    "xs <- [1, 2.5, -3]; nested <- [[1L], [], [(x = 1,)]]",
    "total <- function(v: Array<double>) -> double sum([v[[1]], 2][xs > 1])",
];

#[test]
fn fuzz_lowering_never_panics() {
    for_each_seed(400, |seed| {
        let mut rng = SplitMix64(seed);
        let sample = SEEDS[rng.below(SEEDS.len())];
        check_lowering_input(&String::from_utf8_lossy(&mutate_bytes(&mut rng, sample)));
    });
}

/// Mutated streams of serialized seeds must read or fail with an R error, never panic.
#[test]
fn fuzz_unserialize_never_panics() {
    let seeds = serialized_seeds();
    assert!(!seeds.is_empty(), "no serialized seeds");
    for_each_seed(400, |seed| {
        let mut rng = SplitMix64(seed);
        let mut bytes = seeds[rng.below(seeds.len())].clone();
        for _ in 0..1 + rng.below(4) {
            if bytes.is_empty() {
                break;
            }
            let at = rng.below(bytes.len());
            match rng.below(4) {
                0 => bytes[at] ^= 1 << rng.below(8),
                1 => bytes.truncate(at),
                2 => {
                    bytes.remove(at);
                }
                _ => bytes.insert(at, rng.next_u64() as u8),
            }
        }
        check_unserialize_input(&bytes);
    });
}

/// Streams that once read or wrote outside an object, or asked for more memory than the machine
/// has. Each is a version-3 XDR header and one object.
const UNSERIALIZE_REGRESSIONS: &[&str] = &[
    // The constant is a reference to slot 2147483647.
    "580a000000030004060200030500000000055554462d38\
     00000015000000010000000d0000000000000001000000f37fffffff",
    // The constant is a reference to slot -2.
    "580a000000030004060200030500000000055554462d38\
     00000015000000010000000d0000000000000001000000f3fffffffe",
    // The constant is a call that defines slot 2147483647.
    "580a000000030004060200030500000000055554462d38\
     00000015000000010000000d0000000000000001000000f47fffffff00000006",
    // A reference table of 2147483647 slots, which no definition follows.
    "580a000000030004060200030500000000055554462d38000000157fffffff",
    // A double vector of 2147483647 elements, with none of them in the stream.
    "580a000000030004060200030500000000055554462d380000000e7fffffff",
    // A list of 2147483647 elements, with none of them in the stream.
    "580a000000030004060200030500000000055554462d38000000137fffffff",
    // A compact integer sequence whose state is one number.
    "580a000000030004060200030500000000055554462d38\
     000000ee0000000200000001000400090000000e636f6d706163745f696e74736571000000fe0000000e000000014024000000000000000000fe",
    // A compact real sequence whose state is a string.
    "580a000000030004060200030500000000055554462d38\
     000000ee0000000200000001000400090000000f636f6d706163745f7265616c736571000000fe0000001000000001000400090000000161000000fe",
    // A compact real sequence of a billion elements.
    "580a000000030004060200030500000000055554462d38\
     000000ee0000000200000001000400090000000f636f6d706163745f7265616c736571000000fe0000000e0000000341cdcd65000000003ff00000000000003ff0000000000000000000fe",
    // An ALTREP object whose class is the marker for an unbound value, a symbol without a name.
    "580a000000030004060200030500000000055554462d38\
     000000ee00000002000000fc000000fe0000000e000000013ff0000000000000000000fe",
    // A compact integer sequence of three elements with a step past the integers.
    "580a000000030004060200030500000000055554462d38\
     000000ee0000000200000001000400090000000e636f6d706163745f696e74736571000000fe0000000e0000000340080000000000003ff00000000000007e37e43c8800759c000000fe",
];

#[test]
fn unserialize_regressions_read_or_fail() {
    for hex in UNSERIALIZE_REGRESSIONS {
        check_unserialize_input(&hex_bytes(hex));
    }
}

/// Streams the reader must refuse. Most put an object of the wrong kind where the heap takes the
/// kind from the position: a symbol's name, a string's element, an attribute list or an
/// attribute's name, an environment, the name of a binding, of an argument or of a formal
/// argument. Stored as written, such an object is traced and read as the kind its position
/// implies.
const UNSERIALIZE_REFUSALS: &[&str] = &[
    // A list of a symbol that a logical vector names, and a number.
    "580a000000030004060200030500000000055554462d38\
     0000001300000002000000010000000a00000001000000000000000e000000013ff0000000000000",
    // A string vector whose element is a list.
    "580a000000030004060200030500000000055554462d38\
     000000100000000100000013000000010000000e000000013ff0000000000000",
    // A number whose attributes are an integer vector.
    "580a000000030004060200030500000000055554462d38\
     0000020e000000013ff00000000000000000000d0000000100000001",
    // An environment whose enclosure is an integer vector.
    "580a000000030004060200030500000000055554462d38\
     00000004000000000000000d0000000100000001000000fe000000fe000000fe",
    // An environment with a binding that a string names.
    "580a000000030004060200030500000000055554462d38\
     0000000400000000000000fd000004020004000900000001780000000e000000013ff0000000000000000000fe000000fe000000fe",
    // A closure whose environment is a logical vector.
    "580a000000030004060200030500000000055554462d38\
     000004030000000a0000000100000000000000fe000000fe",
    // A closure with a formal argument that a string names.
    "580a000000030004060200030500000000055554462d38\
     00000403000000fd00000402000400090000000178000000fb000000fe000000fe",
    // A number with an attribute that an integer vector names.
    "580a000000030004060200030500000000055554462d38\
     0000020e000000013ff0000000000000000004020000000d00000001000000010000000e000000013ff0000000000000000000fe",
    // A number whose attribute list ends in an integer vector.
    "580a000000030004060200030500000000055554462d38\
     0000020e000000013ff00000000000000000040200000001000400090000000161\
     0000000e000000013ff00000000000000000000d0000000100000001",
    // The call `f(1)` with the argument named by an integer vector.
    "580a000000030004060200030500000000055554462d38\
     0000000600000001000400090000000166000004020000000d00000001000000010000000e000000013ff0000000000000000000fe",
    // A promise with neither a value nor an environment to compute one in.
    "580a000000030004060200030500000000055554462d3800000005000000fc000000fe",
    // A real-vector wrapper around `NULL`, with an attribute that would land on every `NULL`.
    "580a000000030004060200030500000000055554462d38\
     000000ee00000002000000010004000900000009777261705f7265616c000000fe00000002000000fe000000fe\
     0000040200000001000400090000000161\
     0000000e000000013ff0000000000000000000fe",
    // The pair of streams the fuzzer found, in the binary format. The first holds an environment
    // whose enclosure is a string and whose frame has a cell without a name. Stored as written,
    // it left the heap in a state in which a collection during a later read met an object whose
    // kind byte is no kind.
    "420a030000000206040000050300050000005554462d340400000000000000fd0000000204000001\
     00000009000400050000006c6174657205040000fdfeffff03000000040000000100000009000400\
     0400000073746f70020000001000000001000000090004000c0000006e6576657220666f72636564fe000000fe000000fe000000fe000099fd0100000000000000",
    "420a030000000206040000050300050000005554462d340400000000000000fd0000000204000001\
     00000009000400050000006c6174657205040000fd000000fc000000040000000100000009000400\
     0400000073746f70020000001000000001000000090004000c0000006e6576657220666f72636564fe000000fe000000fe000000fe000099fd0100000000000000",
];

/// Each refusal is an error, and the reader still reads every stream GNU R wrote. Collection
/// stress makes the collector trace what a stream left behind at the next allocation, which the
/// later streams provide.
#[test]
fn unserialize_refuses_an_object_of_the_wrong_kind() {
    let stress =
        |stress| with_interpreter(|| interp::heap().update_policy(|policy| policy.stress = stress));
    stress(heap::GcStress::EveryAllocation);
    for hex in UNSERIALIZE_REFUSALS {
        assert!(
            !check_unserialize_input(&hex_bytes(hex)),
            "the reader accepted {hex}"
        );
    }
    for (name, stream) in serialized_cases() {
        // These two name a namespace and an attached package, which exist only with base R.
        if name == "namespace" || name == "package_env" {
            continue;
        }
        assert!(
            check_unserialize_input(&stream),
            "the reader refused the stream GNU R wrote for `{name}`"
        );
    }
    stress(heap::GcStress::Off);
}

/// A stream cannot set the promise bits that describe a live frame, and a byte-code constant
/// cannot contain itself.
#[test]
fn unserialize_keeps_a_stream_out_of_the_runtime_state() {
    let read = |hex: &str| unsafe {
        interp::serialize::unserialize(&hex_bytes(hex), values::nil()).unwrap_or_else(|_| {
            panic!("the reader refused {hex}");
        })
    };
    with_interpreter(|| {
        let mark = interp::heap().handle_mark();
        // A list of one forced promise whose flags claim it checks an annotation and refers to
        // an argument's slot.
        let list = read(
            "580a000000030004060200030500000000055554462d38\
             0000001300000001030000050000000e000000013ff0000000000000000000fe",
        );
        // SAFETY: the list was read by the installed interpreter and is rooted until the mark
        // is released.
        let bits = unsafe { list.element(0).gp() };
        assert_eq!(
            bits & (heap::object::gp::ARGUMENT_REFERENCE | heap::object::gp::CHECKED_PROMISE),
            0
        );
        // A byte-code object whose source expression is a call that refers to itself as its own
        // function.
        let call = read(
            "580a000000030004060200030500000000055554462d38\
             00000015000000010000000d0000000000000001\
             000000f40000000000000006000000fe000000f30000000000000000000000fe",
        );
        // SAFETY: as above.
        assert!(unsafe { call.car() } != call);
        interp::heap().release_handles(mark);
    });
}

/// A list nested far deeper than a thread's stack holds frames of the reader.
#[test]
fn unserialize_reads_a_deeply_nested_list() {
    let mut bytes = hex_bytes("580a000000030004060200030500000000055554462d38");
    for _ in 0..20_000 {
        bytes.extend_from_slice(&[0, 0, 0, 0x13, 0, 0, 0, 1]);
    }
    bytes.extend_from_slice(&[0, 0, 0, 0xfe]);
    check_unserialize_input(&bytes);
}

fn serialized_seeds() -> Vec<Vec<u8>> {
    serialized_cases()
        .into_iter()
        .map(|(_, stream)| stream)
        .collect()
}

/// The streams GNU R wrote, recorded for libR's conformance tests, each with its case's name.
fn serialized_cases() -> Vec<(String, Vec<u8>)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../libr/tests/data/serialize/cases.tsv"
    );
    let text = std::fs::read_to_string(path).expect("the recorded serialize cases");
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?;
            Some((name.to_owned(), hex_bytes(fields.nth(2)?)))
        })
        .collect()
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .map(|at| u8::from_str_radix(&hex[2 * at..2 * at + 2], 16).expect("hex"))
        .collect()
}

#[test]
fn deparse_round_trips_lowered_seeds() {
    with_interpreter(|| unsafe {
        for seed in SEEDS {
            let Ok(expressions) = lower::parse_text(seed, "<seed>") else {
                panic!("seed does not parse: {seed}");
            };
            for index in 0..values::length(expressions) {
                let expression = expressions.element(index);
                let text = deparse::deparse_lines(expression, 60, deparse::options::KEEP_INTEGER)
                    .join("\n");
                let Ok(again) = lower::parse_text(&text, "<deparsed>") else {
                    panic!("{seed}: deparsed text does not parse: {text}");
                };
                let again = again.element(0);
                let text_again =
                    deparse::deparse_lines(again, 60, deparse::options::KEEP_INTEGER).join("\n");
                assert_eq!(text, text_again, "{seed}: deparse is not stable");
            }
        }
    });
}

#[test]
fn fuzz_bytecode_agrees_with_the_ast_evaluator() {
    for_each_seed(300, |seed| check_bytecode_agreement(&mut SplitMix64(seed)));
}

#[test]
fn fuzz_argument_references_agree_with_the_ast_evaluator() {
    for_each_seed(300, |seed| {
        check_argument_reference_agreement(&mut SplitMix64(seed))
    });
}

#[test]
fn fuzz_quickened_arithmetic_agrees_with_the_ast_evaluator() {
    for_each_seed(300, |seed| {
        check_quickening_agreement(&mut SplitMix64(seed))
    });
}

#[test]
fn fuzz_tier_zero_resumes_at_every_pc() {
    for_each_seed(60, |seed| check_resume_at_every_pc(&mut SplitMix64(seed)));
}

#[test]
fn fuzz_element_access_agrees_with_the_ast_evaluator() {
    for_each_seed(300, |seed| {
        check_element_access_agreement(&mut SplitMix64(seed))
    });
}

#[test]
fn fuzz_frame_stack_agrees_with_a_vector_model() {
    for_each_seed(200, |seed| check_frame_stack(&mut SplitMix64(seed)));
}

#[test]
fn fuzz_shadowed_guards_agree_with_the_ast_evaluator() {
    for_each_seed(600, |seed| check_shadowing_agreement(&mut SplitMix64(seed)));
}

#[test]
fn fuzz_s3_method_lookups_agree_with_a_full_walk() {
    for_each_seed(300, |seed| check_s3_lookup_agreement(&mut SplitMix64(seed)));
}

#[test]
fn fuzz_environment_variables_agree_with_the_c_library() {
    for_each_seed(200, |seed| {
        check_environment_agreement(&mut SplitMix64(seed))
    });
}

#[test]
fn fuzz_code_image_round_trip() {
    for_each_seed(150, |seed| {
        check_code_image_round_trip(&mut SplitMix64(seed))
    });
}

#[test]
fn code_image_round_trips_every_image_body() {
    check_image_bodies_round_trip();
}

#[test]
fn fuzz_code_image_file() {
    for_each_seed(400, |seed| check_code_image_file(&mut SplitMix64(seed)));
}

#[test]
fn fuzz_fused_passes_agree_with_the_ast_evaluator() {
    for_each_seed(300, |seed| check_fused_agreement(&mut SplitMix64(seed)));
}

#[test]
fn fuzz_code_image_load() {
    for_each_seed(300, |seed| check_code_image_load(&mut SplitMix64(seed)));
}
