//! The loader's R side, which both runtimes share: `runtime.R` applies a plan to the session,
//! and a plan reaches it as an R list ([`plan_value`]). On GNU R the list travels as an
//! uncompressed RDS file ([`rds`]), which `readRDS` reads without parsing any R code; on ry's
//! runtime [`crate::ry_runtime`] builds it on the heap.

use {
    crate::{Code, Plan, SysdataChange},
    semantics::{
        load_index::Registration,
        metadata::{NamespaceImportDirective, S3MethodDirective},
    },
};

/// The R side for GNU R. A host evaluates it once per session into the environment it attaches
/// as `tools:ry`, after defining `.ry_request(root, full)`, which returns the plan, and
/// `.ry_commit(root)` there.
pub const RUNTIME_GNUR: &str = concat!(include_str!("runtime.R"), "\n", include_str!("gnur.R"));

/// The R side for ry's runtime, transport included.
pub const RUNTIME_RY: &str = concat!(include_str!("runtime.R"), "\n", include_str!("ry.R"));

/// The subset of R values a plan needs.
#[derive(Debug, Clone, PartialEq)]
pub enum RValue {
    Null,
    Logical(bool),
    Integer(Vec<i32>),
    Character(Vec<String>),
    /// A named list.
    List(Vec<(String, RValue)>),
    /// An unnamed list.
    Vector(Vec<RValue>),
}

/// The plan as `runtime.R` reads it. `with_sources` includes the text of every new source
/// file, which only a backend that parses in R needs.
pub fn plan_value(plan: &Plan, with_sources: bool) -> RValue {
    let strings = |values: &[String]| RValue::Character(values.to_vec());
    let column = |codes: &[Code], field: fn(&Code) -> String| {
        RValue::Character(codes.iter().map(field).collect())
    };
    let integers = |codes: &[Code], field: fn(&Code) -> u32| {
        RValue::Integer(
            codes
                .iter()
                .map(|code| i32::try_from(field(code)).unwrap_or(i32::MAX))
                .collect(),
        )
    };
    let s3 = |methods: &[S3MethodDirective]| {
        RValue::Vector(
            methods
                .iter()
                .map(|method| {
                    RValue::List(vec![
                        (
                            "generic".into(),
                            RValue::Character(vec![method.generic.clone()]),
                        ),
                        (
                            "class".into(),
                            RValue::Character(vec![method.class.clone()]),
                        ),
                        (
                            "function".into(),
                            RValue::Character(vec![method.function.clone()]),
                        ),
                    ])
                })
                .collect(),
        )
    };
    let namespace = &plan.namespace;
    RValue::List(vec![
        (
            "package".into(),
            RValue::Character(vec![plan.package.clone()]),
        ),
        (
            "version".into(),
            RValue::Character(vec![plan.version.clone()]),
        ),
        ("full".into(), RValue::Logical(plan.full)),
        ("noop".into(), RValue::Logical(plan.is_noop())),
        ("depends".into(), strings(&plan.depends)),
        (
            "imports".into(),
            plan.imports.as_ref().map_or(RValue::Null, |imports| {
                RValue::Vector(imports.iter().map(import_value).collect())
            }),
        ),
        (
            "source_path".into(),
            RValue::Character(if with_sources {
                plan.sources
                    .iter()
                    .map(|source| source.path.clone())
                    .collect()
            } else {
                Vec::new()
            }),
        ),
        (
            "source_text".into(),
            RValue::Character(if with_sources {
                plan.sources
                    .iter()
                    .map(|source| source.text.clone())
                    .collect()
            } else {
                Vec::new()
            }),
        ),
        ("dropped_sources".into(), strings(&plan.dropped_sources)),
        ("remove".into(), strings(&plan.remove)),
        (
            "unregister".into(),
            RValue::Vector(plan.unregister.iter().map(registration_value).collect()),
        ),
        (
            "bind_name".into(),
            column(&plan.bind, |code| code.name.clone().unwrap_or_default()),
        ),
        (
            "bind_path".into(),
            column(&plan.bind, |code| code.path.clone()),
        ),
        ("bind_line".into(), integers(&plan.bind, |code| code.line)),
        (
            "bind_column".into(),
            integers(&plan.bind, |code| code.column),
        ),
        ("bind_start".into(), integers(&plan.bind, |code| code.start)),
        ("bind_end".into(), integers(&plan.bind, |code| code.end)),
        (
            "run_path".into(),
            column(&plan.run, |code| code.path.clone()),
        ),
        ("run_line".into(), integers(&plan.run, |code| code.line)),
        ("run_column".into(), integers(&plan.run, |code| code.column)),
        ("run_start".into(), integers(&plan.run, |code| code.start)),
        ("run_end".into(), integers(&plan.run, |code| code.end)),
        (
            "touched".into(),
            plan.touched_names().map_or(RValue::Null, RValue::Character),
        ),
        (
            "keeps_names".into(),
            RValue::Logical(plan.keeps_names && plan.touched_names().is_some()),
        ),
        (
            "registers_s4".into(),
            RValue::Logical(plan.run.iter().any(|code| code.registers_s4)),
        ),
        (
            "deferred_s4".into(),
            plan.deferred_s4.as_ref().map_or(RValue::Null, |deferred| {
                RValue::List(vec![
                    (
                        "run".into(),
                        RValue::Integer(
                            deferred
                                .run
                                .iter()
                                .map(|&position| position as i32 + 1)
                                .collect(),
                        ),
                    ),
                    ("classes".into(), strings(&deferred.classes)),
                    ("generics".into(), strings(&deferred.generics)),
                    ("names".into(), strings(&deferred.names)),
                ])
            }),
        ),
        (
            "sysdata".into(),
            RValue::Character(vec![
                match plan.sysdata {
                    SysdataChange::Keep => "keep",
                    SysdataChange::Load => "load",
                    SysdataChange::Remove => "remove",
                }
                .to_owned(),
            ]),
        ),
        ("s3_register".into(), s3(&plan.s3_register)),
        ("s3_unregister".into(), s3(&plan.s3_unregister)),
        (
            "s3_declared".into(),
            plan.s3_declared.as_deref().map_or(RValue::Null, s3),
        ),
        (
            "dynamic_libraries".into(),
            plan.dynamic_libraries
                .as_ref()
                .map_or(RValue::Null, |libraries| {
                    RValue::Vector(
                        libraries
                            .iter()
                            .map(|library| {
                                RValue::List(vec![
                                    (
                                        "library".into(),
                                        RValue::Character(vec![library.library.clone()]),
                                    ),
                                    ("registration".into(), RValue::Logical(library.registration)),
                                    (
                                        "fixes".into(),
                                        RValue::Character(vec![library.fixes.clone()]),
                                    ),
                                    (
                                        "symbol_names".into(),
                                        RValue::Character(
                                            library
                                                .symbols
                                                .iter()
                                                .map(|(name, _)| name.clone())
                                                .collect(),
                                        ),
                                    ),
                                    (
                                        "symbols".into(),
                                        RValue::Character(
                                            library
                                                .symbols
                                                .iter()
                                                .map(|(_, symbol)| symbol.clone())
                                                .collect(),
                                        ),
                                    ),
                                ])
                            })
                            .collect(),
                    )
                }),
        ),
        (
            "namespace".into(),
            RValue::List(vec![
                ("exports".into(), strings(&namespace.exports)),
                (
                    "export_patterns".into(),
                    strings(&namespace.export_patterns),
                ),
                ("export_classes".into(), strings(&namespace.export_classes)),
                (
                    "export_class_patterns".into(),
                    strings(&namespace.export_class_patterns),
                ),
                ("export_methods".into(), strings(&namespace.export_methods)),
            ]),
        ),
        ("run_on_load".into(), RValue::Logical(plan.run_on_load)),
        ("warnings".into(), strings(&plan.warnings)),
    ])
}

/// What R reads instead of a plan when planning failed.
pub fn error_value(message: &str) -> RValue {
    RValue::List(vec![(
        "error".into(),
        RValue::Character(vec![message.to_owned()]),
    )])
}

fn import_value(import: &NamespaceImportDirective) -> RValue {
    let (kind, package, names, except) = match import {
        NamespaceImportDirective::Namespace { package, except } => {
            ("namespace", package, &[][..], except.as_slice())
        }
        NamespaceImportDirective::Names { package, names } => {
            ("names", package, names.as_slice(), &[][..])
        }
        NamespaceImportDirective::Classes { package, names } => {
            ("classes", package, names.as_slice(), &[][..])
        }
        NamespaceImportDirective::Methods { package, names } => {
            ("methods", package, names.as_slice(), &[][..])
        }
    };
    RValue::List(vec![
        ("kind".into(), RValue::Character(vec![kind.to_owned()])),
        ("package".into(), RValue::Character(vec![package.clone()])),
        ("names".into(), RValue::Character(names.to_vec())),
        ("except".into(), RValue::Character(except.to_vec())),
    ])
}

fn registration_value(registration: &Registration) -> RValue {
    let one = |value: &String| RValue::Character(vec![value.clone()]);
    let kind = |kind: &str| ("kind".to_owned(), RValue::Character(vec![kind.to_owned()]));
    RValue::List(match registration {
        Registration::S4Class { class } => vec![kind("s4_class"), ("class".into(), one(class))],
        Registration::S4Generic { generic } => {
            vec![kind("s4_generic"), ("generic".into(), one(generic))]
        }
        Registration::S4Method { generic, signature } => vec![
            kind("s4_method"),
            ("generic".into(), one(generic)),
            ("signature".into(), RValue::Character(signature.clone())),
        ],
        Registration::R6Class { class } => vec![kind("r6_class"), ("class".into(), one(class))],
        Registration::S3Method { generic, class } => vec![
            kind("s3_method"),
            ("generic".into(), one(generic)),
            ("class".into(), one(class)),
        ],
    })
}

/// Serializes `value` in R's XDR serialization format, version 3, uncompressed.
pub fn rds(value: &RValue) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"X\n");
    write_int(&mut out, 3);
    // The writer's R version (4.3.0), and the oldest reader that understands version 3.
    write_int(&mut out, 0x0004_0300);
    write_int(&mut out, 0x0003_0500);
    write_int(&mut out, 5);
    out.extend_from_slice(b"UTF-8");
    write_value(&mut out, value);
    out
}

const NILVALUE_SXP: i32 = 254;
const SYMSXP: i32 = 1;
const LISTSXP: i32 = 2;
const CHARSXP: i32 = 9;
const LGLSXP: i32 = 10;
const INTSXP: i32 = 13;
const STRSXP: i32 = 16;
const VECSXP: i32 = 19;
const HAS_ATTRIBUTES: i32 = 1 << 9;
const HAS_TAG: i32 = 1 << 10;
const UTF8_LEVEL: i32 = 1 << 3;
const ASCII_LEVEL: i32 = 1 << 6;

fn write_int(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn write_length(out: &mut Vec<u8>, length: usize) {
    write_int(out, i32::try_from(length).unwrap_or(i32::MAX));
}

fn write_string(out: &mut Vec<u8>, text: &str) {
    let level = if text.is_ascii() {
        ASCII_LEVEL
    } else {
        UTF8_LEVEL
    };
    write_int(out, CHARSXP | (level << 12));
    write_length(out, text.len());
    out.extend_from_slice(text.as_bytes());
}

fn write_value(out: &mut Vec<u8>, value: &RValue) {
    match value {
        RValue::Null => write_int(out, NILVALUE_SXP),
        RValue::Logical(flag) => {
            write_int(out, LGLSXP);
            write_length(out, 1);
            write_int(out, i32::from(*flag));
        }
        RValue::Integer(values) => {
            write_int(out, INTSXP);
            write_length(out, values.len());
            for value in values {
                write_int(out, *value);
            }
        }
        RValue::Character(values) => {
            write_int(out, STRSXP);
            write_length(out, values.len());
            for value in values {
                write_string(out, value);
            }
        }
        RValue::Vector(values) => {
            write_int(out, VECSXP);
            write_length(out, values.len());
            for value in values {
                write_value(out, value);
            }
        }
        RValue::List(fields) => {
            write_int(out, VECSXP | HAS_ATTRIBUTES);
            write_length(out, fields.len());
            for (_, value) in fields {
                write_value(out, value);
            }
            // The attribute pairlist: one `names` entry, then the terminator.
            write_int(out, LISTSXP | HAS_TAG);
            write_int(out, SYMSXP);
            write_string(out, "names");
            write_int(out, STRSXP);
            write_length(out, fields.len());
            for (name, _) in fields {
                write_string(out, name);
            }
            write_int(out, NILVALUE_SXP);
        }
    }
}
