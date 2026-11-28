//! What keeps a function written in ry's types from running on native code.
//!
//! A function literal whose declared signature is written entirely in ry types is a native
//! function: the typed core compiles it to machine code, and inside it every value is a ry
//! value. R values enter only where the code says so, through `Num.from_r` and its siblings, and
//! an R function runs either there or as a statement whose value nothing reads. A function with
//! an R type anywhere in its signature is a boundary function, which is not judged.
//!
//! The typed core decides what compiles when the program runs, from the values a call passes.
//! This pass states the same rule over the checker's types, so the two can only be kept equal by
//! a test: `crates/ry/tests/test_scenarios.rs` runs a corpus through both and fails when a
//! function this pass accepts falls back to the interpreter.

use {
    crate::{
        Db, TypeDefinitions, canonical_spelling,
        check::GlobalEnv,
        dense::{Bitset, Column},
        hir::{
            Argument, AssignSpelling, BinaryOperator, ExprId, ExpressionKind, LiteralKind, Module,
            Parameter, PatId, PatternKind, Symbol, UnaryOperator,
        },
        infer::apply_definition,
        naming::{BindingKind, ItemNaming},
        stubs::{declaring_namespace, is_native_function, is_ry_type},
        types::{
            Atomic, Constraint, FunctionType, Name, NativeScalar, Ty, TyKind, TypeScheme,
            array_element, dict_entries, set_element, stored_spelling,
        },
    },
    rustc_hash::FxHashSet,
    smallvec::SmallVec,
    syntax::{
        TextRange, TextSize,
        annotation::{Paired, pair_formals},
        native_values::{reads_its_frame, uses_its_frame},
    },
    thin_vec::ThinVec,
};

/// One construct that keeps a native function off native code, at the construct's own range.
#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub struct NativeBlocker {
    pub range: TextRange,
    pub kind: NativeBlockerKind,
    /// The rewrite the message names, when it is one exact edit that keeps the program's meaning.
    pub fix: Option<NativeFix>,
}

/// The mechanical rewrite of a blocked construct, which `ry check --fix` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub enum NativeFix {
    /// Wrap the construct in the conversion, as in `Num.from_r(nchar(label))`.
    Convert(Conversion),
    /// Compute the construct into a new variable just before the statement at `before`, wrapped
    /// in `convert` when it is not a conversion already, and use the variable in its place.
    Hoist {
        before: TextRange,
        convert: Option<Conversion>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, salsa::SalsaValue)]
pub enum NativeBlockerKind {
    /// The value of an R call reaches ry code without a conversion.
    RValue {
        function: Symbol,
        origin: ROrigin,
        conversion: Option<Conversion>,
    },
    /// An R call in the arguments of another: R's lazy arguments decide when the inner one runs.
    NestedRCall {
        outer: Symbol,
        inner: Symbol,
        conversion: Option<Conversion>,
        /// The conversion gives an integer, which R takes back only through `to_r`.
        integer: bool,
    },
    /// A read of a variable from outside the function that holds an R value.
    RVariable {
        name: Symbol,
        conversion: Option<Conversion>,
        /// A parameter or local of the enclosing function rather than a global.
        enclosing: bool,
    },
    /// `table$column` or `table[["column"]]` outside a conversion.
    ColumnRead,
    /// `from:to`, which builds an R vector.
    Range,
    /// `Num.to_r` or `List.to_r` anywhere but directly in an R call's arguments.
    ToROutsideCall(Symbol),
    /// A literal such as `NULL` or `NA`, which has no ry type.
    RLiteral,
    /// Legitimate ry that sends the function to the interpreter until the typed core compiles it.
    Uncompiled(Uncompiled),
    /// A default native code cannot compute at the call, which sends a call that leaves the
    /// parameter out to the interpreter.
    LazyDefault,
}

/// Why a callee counts as an R function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub enum ROrigin {
    /// A function of R or of a package.
    Library,
    /// A table function whose result is an R value, such as `DataFrame.height`.
    Table,
    /// A project function whose signature holds an R type.
    Boundary,
    /// A project function with no declared signature.
    Unannotated,
}

/// The standard library module whose `from_r` takes an R value of the type found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub enum Conversion {
    Num,
    Str,
    Bool,
    List,
}

/// A construct the typed core has no native code for yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub enum Uncompiled {
    Dots,
    RecursiveFunctionArgument(Symbol),
    InvisibleResult,
    SuperAssignment,
    AssignmentTarget,
    LocalFunctionInBranch(Symbol),
    FunctionValue,
    CallsVariable(Symbol),
    NamedArguments(Symbol),
    ArgumentCount(Symbol),
    StdFunction(Symbol),
    MappedFunction,
    DictEntries,
    SetElements,
    MatchGuard,
    MatchPattern,
    MatchScrutinee,
    TableOperand,
    BranchOnlyName(Symbol),
    TypeChange(Symbol),
    BranchTypes,
    GlobalRead(Symbol),
    EnclosingRead(Symbol),
    ComputedCall,
    AliasedSignature,
    IfWithoutElse,
    FrameCall(Symbol),
    RArgument(Symbol, Crossing),
    Construct(&'static str),
    /// A construct, then what to write instead.
    Advised(&'static str, &'static str),
}

/// A ry value that R takes only through a conversion, or not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, salsa::SalsaValue)]
pub enum Crossing {
    I64,
    I64List,
    Dict,
    Set,
}

impl NativeBlockerKind {
    /// The diagnostic's text. `source` is the blocked construct as written, which the fix
    /// repeats when it is short enough to read inline.
    pub fn message(&self, source: &str) -> String {
        let shown = |function: Symbol| {
            if !source.contains('\n') && source.chars().count() <= 48 {
                source.to_owned()
            } else {
                format!("{function}(...)")
            }
        };
        const HOLDS: &str = "a native function holds only ry values";
        const MOVE: &str = "If only its effect matters, call it in a statement of its own. \
                            Otherwise move the call into a function with an R type in its \
                            signature, which is where R values and ry values meet";
        match *self {
            NativeBlockerKind::RValue {
                function,
                origin,
                conversion,
            } => {
                let why = match origin {
                    ROrigin::Library => format!("`{function}` is an R function"),
                    ROrigin::Table => format!("`{function}` gives an R value"),
                    ROrigin::Boundary => format!(
                        "`{function}` has an R type in its signature, so here it counts as an R \
                         function"
                    ),
                    ROrigin::Unannotated => format!(
                        "`{function}` has no type annotations, so here it counts as an R function"
                    ),
                };
                let fix = match conversion {
                    Some(conversion) => format!(
                        "Convert the result where it returns: `{}.from_r({})`",
                        conversion.module(),
                        shown(function)
                    ),
                    None => MOVE.to_owned(),
                };
                format!("{why}, and {HOLDS}, so its result cannot be used here. {fix}")
            }
            NativeBlockerKind::NestedRCall {
                outer,
                inner,
                conversion,
                integer,
            } => {
                let fix = match conversion {
                    Some(conversion) if integer => format!(
                        "Compute the inner call into a variable first, as in \
                         `value <- {module}.from_r({inner}(...))`, and pass \
                         `{module}.to_r(value)`",
                        module = conversion.module()
                    ),
                    Some(conversion) => format!(
                        "Compute the inner call into a variable first, as in \
                         `value <- {}.from_r({inner}(...))`, and pass the variable",
                        conversion.module()
                    ),
                    None => format!(
                        "Call `{inner}` in a statement of its own if only its effect matters, or \
                         make both calls in a function with an R type in its signature"
                    ),
                };
                format!(
                    "this calls R's `{inner}` inside the arguments of R's `{outer}`. R evaluates \
                     an argument only when the function reads it, so native code cannot run the \
                     two in a fixed order. {fix}"
                )
            }
            NativeBlockerKind::RVariable {
                name,
                conversion,
                enclosing,
            } => {
                let place = if enclosing {
                    "the enclosing function"
                } else {
                    "outside the function"
                };
                let fix = match conversion {
                    Some(conversion) => format!(
                        "Convert it once where it is made, as in `{}.from_r(...)`, and pass the ry \
                         value in as a parameter",
                        conversion.module()
                    ),
                    None => String::from(
                        "Convert it to a ry value in a function with an R type in its signature, \
                         and pass that in as a parameter",
                    ),
                };
                format!("`{name}` comes from {place} and holds an R value, and {HOLDS}. {fix}")
            }
            NativeBlockerKind::ColumnRead => {
                let read = if source.contains('\n') {
                    "table$column"
                } else {
                    source
                };
                format!(
                    "reading a column gives an R vector, and {HOLDS}. Convert it where it is \
                     read: `List.from_r({read})`"
                )
            }
            NativeBlockerKind::Range => {
                format!("`:` builds an R vector, and {HOLDS}. Write `List.range(from, to)`")
            }
            NativeBlockerKind::ToROutsideCall(name) => format!(
                "`{name}` gives an R value, which a native function holds only as the argument of \
                 an R call. Write it directly inside the call, as in `cat({name}(x))`"
            ),
            NativeBlockerKind::RLiteral => format!(
                "this literal is an R value with no ry type, and {HOLDS}. Use a `Result` or a tag \
                 for a missing value"
            ),
            NativeBlockerKind::Uncompiled(construct) => format!(
                "this function runs in the interpreter, because {}",
                construct.reason()
            ),
            NativeBlockerKind::LazyDefault => String::from(
                "a call that leaves out this parameter runs in the interpreter, because R \
                 computes a default where the body first reads it, and native code computes one \
                 at the call only when it is made of constants, operators, standard library \
                 calls and parameters or local variables the body does not set. Compute the \
                 value in the body instead",
            ),
        }
    }

    /// An R value in a native function breaks the rule the function is written under, which is
    /// an error. A construct the typed core does not compile yet is the compiler's gap, not the
    /// program's, so it is a warning that says where native code ends.
    pub fn is_error(&self) -> bool {
        !matches!(
            self,
            NativeBlockerKind::Uncompiled(_) | NativeBlockerKind::LazyDefault
        )
    }

    pub fn code(&self) -> &'static str {
        if self.is_error() {
            "native"
        } else {
            "interpreted"
        }
    }
}

impl Conversion {
    pub fn module(self) -> &'static str {
        match self {
            Conversion::Num => "Num",
            Conversion::Str => "Str",
            Conversion::Bool => "Bool",
            Conversion::List => "List",
        }
    }
}

impl Uncompiled {
    /// What is not compiled, then what to write instead.
    fn reason(self) -> String {
        const YET: &str = "is not compiled natively yet";
        match self {
            Uncompiled::Dots => format!(
                "a function with `...` {YET}. Name each parameter, or take the values as one \
                 `List`"
            ),
            Uncompiled::RecursiveFunctionArgument(name) => format!(
                "a function passed to a recursive call of `{name}` that calls a function \
                 parameter {YET}: each level would wrap the function once more. Loop instead, \
                 or pass the function on as it is and what the wrapper adds as a parameter of \
                 its own"
            ),
            Uncompiled::InvisibleResult => format!(
                "R returns the value this path gives invisibly, as after an assignment or a loop, \
                 but other paths give a visible value, and a function whose paths disagree {YET}. \
                 End this path with the value itself, or end every path the same way"
            ),
            Uncompiled::SuperAssignment => format!(
                "`<<-` {YET}: a native function changes only its own variables. Return the new \
                 value and assign it in the caller"
            ),
            Uncompiled::AssignmentTarget => format!(
                "an assignment of this form {YET}. Native code assigns to a variable, or to one \
                 item of a list this function made, as in `xs[[i]] <- value`. Build a new record \
                 with `(..record, field = value)`"
            ),
            Uncompiled::LocalFunctionInBranch(name) => format!(
                "`{name}` is a function defined inside a branch or a loop and used after it, or \
                 defined again there, which {YET}. Define it once at the top of the function"
            ),
            Uncompiled::FunctionValue => format!(
                "a function used as a value {YET}. Native code runs a function only where it is \
                 called, or where it is passed straight to a function that calls it"
            ),
            Uncompiled::CallsVariable(name) => format!(
                "`{name}` is a variable that holds a function, and calling one {YET}. Define the \
                 function with `{name} <- function(...)` in this function, or call a top-level \
                 function"
            ),
            Uncompiled::NamedArguments(function) => format!(
                "a call of `{function}` with named arguments {YET}. Pass the arguments by \
                 position"
            ),
            Uncompiled::ArgumentCount(function) => format!(
                "this call of `{function}` leaves out a parameter without a default, or passes an \
                 argument that matches no parameter, which {YET}. Pass each parameter without a \
                 default, by position or by its name"
            ),
            Uncompiled::StdFunction(name) => format!(
                "`{name}` {YET}. Write the step with a loop or another standard library \
                 function"
            ),
            Uncompiled::MappedFunction => format!(
                "a function passed by name {YET} unless it is a typed function of the project, a \
                 local function, a parameter or a standard library function with native code. \
                 Pass the standard library function, such as `Num.sqrt` for `sqrt`, or a \
                 function literal"
            ),
            Uncompiled::DictEntries => format!(
                "a `Dict` whose keys are not `I64`, `F64` or `Str`, or whose values hold \
                 something other than numbers, strings, `Bool`s, lists, records, tuples and tags, \
                 {YET}. Convert the keys to `I64` or `Str`, and keep such values in a `List` whose \
                 positions the dictionary stores"
            ),
            Uncompiled::SetElements => format!(
                "a `Set` whose elements are not `I64` or `Str` {YET}. Convert the elements to \
                 `I64` or `Str`, or keep them in a `List`"
            ),
            Uncompiled::MatchGuard => format!(
                "a `match` guard that assigns a variable {YET}. Compute the value before the \
                 `match` and test it in the guard"
            ),
            Uncompiled::MatchPattern => format!(
                "this `match` pattern {YET}. Native code matches a literal other than `NA`, \
                 `NaN` or a complex number. Bind the value to a name and match it in the arm's body"
            ),
            Uncompiled::MatchScrutinee => format!(
                "a `match` on a value that is not a number, a string, a logical, a `List`, a \
                 tag, a record or a tuple {YET}. Compare the value with `if` and `==`"
            ),
            Uncompiled::TableOperand => format!(
                "an operator applied directly to a table expression that a local function, a \
                 function whose result type is an alias of an open or generic type, an `if` or \
                 a `match` gives {YET}. \
                 Assign the expression to a variable first, and apply the operator to the variable"
            ),
            Uncompiled::BranchOnlyName(name) => format!(
                "`{name}` is read before this function sets it, or after branches that set it to \
                 values of different types, which {YET}. Give `{name}` one type, and a value \
                 before the branch or loop"
            ),
            Uncompiled::TypeChange(name) => format!(
                "`{name}` holds one type before this branch or loop and takes another inside \
                 it, which {YET}. Use a new name for the new value"
            ),
            Uncompiled::BranchTypes => format!(
                "the branches give values of different types, which {YET}: a native value has one \
                 type. Make every branch give the same type"
            ),
            Uncompiled::GlobalRead(name) => {
                format!("reading the global `{name}` {YET}. Pass the value in as a parameter")
            }
            Uncompiled::EnclosingRead(name) => format!(
                "reading `{name}` from the enclosing function {YET}: that function is \
                 not native. Pass the value in as a parameter"
            ),
            Uncompiled::ComputedCall => {
                format!("a call of a computed function {YET}. Call a function by its name")
            }
            Uncompiled::AliasedSignature => format!(
                "a function whose signature is a type alias that cannot be written out in full \
                 {YET}: the runtime reads the types where they are written. Write the function \
                 type out in place of the alias, as in `fn(x: I64) -> I64`, or write the types \
                 on the parameters"
            ),
            Uncompiled::IfWithoutElse => format!(
                "the value of an `if` without `else` {YET}: it is `NULL` when the condition is \
                 false, so it has two types. Add an `else` branch that gives a value of the same \
                 type"
            ),
            Uncompiled::RArgument(function, crossing) => match crossing {
                Crossing::I64 => format!(
                    "an `I64` passed to R's `{function}` as it is {YET}: R has no 64-bit \
                     integer, so the call says how the number crosses. Write `Num.to_r(...)` \
                     around the argument"
                ),
                Crossing::I64List => format!(
                    "a `List<I64>` passed to R's `{function}` as it is {YET}: R has no 64-bit \
                     integer, so the call says how the numbers cross. Write `List.to_r(...)` \
                     around the argument"
                ),
                Crossing::Dict => format!(
                    "a `Dict` passed to R's `{function}` {YET}. Pass `Dict.keys(...)` and \
                     `Dict.values(...)` instead"
                ),
                Crossing::Set => format!(
                    "a `Set` passed to R's `{function}` {YET}. Pass `Set.to_list(...)` instead"
                ),
            },
            Uncompiled::FrameCall(function) if function == "missing" => format!(
                "`missing` {YET}: a native call passes values, so it does not keep how R called \
                 the function. Give the parameter a default, such as a tag that marks it \
                 missing, and test that"
            ),
            Uncompiled::FrameCall(function) => format!(
                "a call of R's `{function}` {YET}: it looks at the frame it is called from, \
                 which native code does not keep. Move the call into a function with an R type \
                 in its signature"
            ),
            Uncompiled::Construct(construct) => format!("{construct} {YET}"),
            Uncompiled::Advised(construct, fix) => format!("{construct} {YET}. {fix}"),
        }
    }
}

/// The standard library functions the typed core has native code for, as its lowering spells
/// them. A function the list leaves out sends its caller to the interpreter wherever it stands.
/// The typed core's lowering is the source of truth, and it lives in a crate this one cannot
/// read, so `native_diagnostics_agree_with_the_typed_core` checks each name against it.
pub const COMPILED_STD: &[&str] = &[
    "Bool.and",
    "Bool.is_eq",
    "Bool.is_not_eq",
    "Bool.not",
    "Bool.or",
    "Dict.clear",
    "Dict.contains",
    "Dict.drop_if",
    "Dict.empty",
    "Dict.from_list",
    "Dict.get",
    "Dict.insert",
    "Dict.insert_all",
    "Dict.is_empty",
    "Dict.join_map",
    "Dict.keep_if",
    "Dict.keep_shared",
    "Dict.keys",
    "Dict.len",
    "Dict.map",
    "Dict.release_excess_capacity",
    "Dict.remove",
    "Dict.remove_all",
    "Dict.reserve",
    "Dict.single",
    "Dict.to_list",
    "Dict.update",
    "Dict.values",
    "Dict.walk",
    "Dict.walk_until",
    "Dict.with_capacity",
    "List.all",
    "List.any",
    "List.append",
    "List.append_if_ok",
    "List.chunks_of",
    "List.concat",
    "List.contains",
    "List.count_if",
    "List.drop_at",
    "List.drop_first",
    "List.drop_if",
    "List.drop_last",
    "List.ends_with",
    "List.find_first",
    "List.find_first_index",
    "List.find_last",
    "List.find_last_index",
    "List.first",
    "List.for_each",
    "List.get",
    "List.intersperse",
    "List.is_empty",
    "List.join",
    "List.join_map",
    "List.keep_errs",
    "List.keep_if",
    "List.keep_if_try",
    "List.keep_oks",
    "List.last",
    "List.len",
    "List.map",
    "List.map2",
    "List.map3",
    "List.map4",
    "List.map_try",
    "List.map_with_index",
    "List.max",
    "List.min",
    "List.prepend",
    "List.prepend_if_ok",
    "List.product",
    "List.range",
    "List.release_excess_capacity",
    "List.repeat",
    "List.replace",
    "List.reserve",
    "List.reverse",
    "List.set",
    "List.single",
    "List.sort_asc",
    "List.sort_desc",
    "List.sort_with",
    "List.split_at",
    "List.split_first",
    "List.split_last",
    "List.split_on",
    "List.split_on_list",
    "List.starts_with",
    "List.sublist",
    "List.sum",
    "List.swap",
    "List.take_first",
    "List.take_last",
    "List.update",
    "List.walk",
    "List.walk_backwards",
    "List.walk_backwards_until",
    "List.walk_from",
    "List.walk_from_until",
    "List.walk_try",
    "List.walk_until",
    "List.walk_with_index",
    "List.walk_with_index_until",
    "List.with_capacity",
    "Num.abs",
    "Num.abs_diff",
    "Num.acos",
    "Num.add",
    "Num.add_checked",
    "Num.add_saturated",
    "Num.add_wrap",
    "Num.asin",
    "Num.atan",
    "Num.bitwise_and",
    "Num.bitwise_not",
    "Num.bitwise_or",
    "Num.bitwise_xor",
    "Num.ceiling",
    "Num.compare",
    "Num.cos",
    "Num.count_leading_zero_bits",
    "Num.count_one_bits",
    "Num.count_trailing_zero_bits",
    "Num.div",
    "Num.div_ceil",
    "Num.div_ceil_checked",
    "Num.div_checked",
    "Num.div_trunc",
    "Num.div_trunc_checked",
    "Num.exp",
    "Num.floor",
    "Num.is_approx_eq",
    "Num.is_even",
    "Num.is_finite",
    "Num.is_gt",
    "Num.is_gte",
    "Num.is_infinite",
    "Num.is_lt",
    "Num.is_lte",
    "Num.is_multiple_of",
    "Num.is_nan",
    "Num.is_negative",
    "Num.is_odd",
    "Num.is_positive",
    "Num.is_zero",
    "Num.log",
    "Num.log_checked",
    "Num.max",
    "Num.min",
    "Num.mul",
    "Num.mul_checked",
    "Num.mul_saturated",
    "Num.mul_wrap",
    "Num.neg",
    "Num.pow",
    "Num.pow_int",
    "Num.rem",
    "Num.rem_checked",
    "Num.round",
    "Num.shift_left_by",
    "Num.shift_right_by",
    "Num.shift_right_zf_by",
    "Num.sin",
    "Num.sqrt",
    "Num.sqrt_checked",
    "Num.sub",
    "Num.sub_checked",
    "Num.sub_saturated",
    "Num.sub_wrap",
    "Num.tan",
    "Num.to_f64",
    "Num.to_i64",
    "Num.to_i64_checked",
    "Num.to_str",
    "Num.truncate",
    "Result.and_then",
    "Result.is_err",
    "Result.is_ok",
    "Result.map",
    "Result.map2",
    "Result.map_both",
    "Result.map_err",
    "Result.on_err",
    "Result.with_default",
    "Set.contains",
    "Set.difference",
    "Set.drop_if",
    "Set.empty",
    "Set.from_list",
    "Set.insert",
    "Set.intersection",
    "Set.is_empty",
    "Set.join_map",
    "Set.keep_if",
    "Set.len",
    "Set.map",
    "Set.release_excess_capacity",
    "Set.remove",
    "Set.reserve",
    "Set.single",
    "Set.to_list",
    "Set.union",
    "Set.walk",
    "Set.walk_until",
    "Set.with_capacity",
    "Str.caseless_ascii_equals",
    "Str.concat",
    "Str.contains",
    "Str.count_utf8_bytes",
    "Str.drop_prefix",
    "Str.drop_suffix",
    "Str.ends_with",
    "Str.from_utf8_lossy",
    "Str.is_empty",
    "Str.join_with",
    "Str.repeat",
    "Str.replace_each",
    "Str.replace_first",
    "Str.replace_last",
    "Str.split_first",
    "Str.split_last",
    "Str.split_on",
    "Str.starts_with",
    "Str.to_f64",
    "Str.to_i64",
    "Str.to_utf8",
    "Str.trim",
    "Str.trim_end",
    "Str.trim_start",
    "Str.with_ascii_lowercased",
    "Str.with_ascii_uppercased",
    "Str.with_prefix",
];

/// The functions of [`COMPILED_STD`] whose native code matches named arguments as R does.
const STD_NAMED_ARGUMENTS: &[&str] = &["Num.is_approx_eq"];

const FROM_R: [&str; 6] = [
    "Num.from_r",
    "Str.from_r",
    "Bool.from_r",
    "List.from_r",
    "List.from_r_or",
    "List.from_r_missing",
];
const TO_R: [&str; 2] = ["Num.to_r", "List.to_r"];

const TABLE_MODULES: [&str; 5] = [
    "Expr.",
    "DataFrame.",
    "LazyFrame.",
    "GroupBy.",
    "LazyGroupBy.",
];

/// The table functions that give an R value rather than a table or an expression.
const TABLE_R_CALLS: [&str; 9] = [
    "DataFrame.height",
    "DataFrame.width",
    "DataFrame.columns",
    "DataFrame.schema",
    "DataFrame.key",
    "DataFrame.set_key",
    "DataFrame.write_csv",
    "DataFrame.write_parquet",
    "LazyFrame.explain",
];

/// Whether `name` heads a table type, as `lower::returns_table` in `crates/typed` reads a
/// written result type.
fn is_table_type(name: &str) -> bool {
    matches!(
        name,
        "Expr" | "DataFrame" | "LazyFrame" | "GroupBy" | "LazyGroupBy"
    )
}

/// Whether `name` is a table function whose result is a table, a group or an expression, which
/// native code holds as it holds any ry value.
pub fn gives_table_value(name: &str) -> bool {
    (name == "DataFrame"
        || name == "as.DataFrame"
        || TABLE_MODULES.iter().any(|module| name.starts_with(module)))
        && !TABLE_R_CALLS.contains(&name)
}

/// What the pass reads of one item's check.
pub struct Judged<'a, 'db> {
    pub db: &'db dyn Db,
    pub module: &'a Module,
    pub naming: &'a ItemNaming,
    pub definitions: Option<TypeDefinitions<'db>>,
    pub globals: Option<&'a dyn GlobalEnv<'db>>,
    /// Each expression's resolved type, where the check recorded one.
    pub types: &'a dyn Fn(ExprId) -> Option<Ty<'db>>,
    /// The declared signature of each function literal that has one.
    pub signatures: &'a Column<'a, ExprId, &'db FunctionType<'db>>,
    /// The function literals whose signature only a type alias gives, so that no type is
    /// written where the runtime reads one (see `Annotation::written_signature`).
    pub aliased: &'a Bitset<'a, ExprId>,
}

/// Every blocker in the native functions of one item, in source order.
pub fn native_blockers(judged: Judged<'_, '_>) -> ThinVec<NativeBlocker> {
    let Some(root) = judged.module.root else {
        return ThinVec::new();
    };
    let mut judge = Judge {
        judged,
        blockers: Vec::new(),
        scope: Vec::new(),
        locals: FxHashSet::default(),
        hidden: FxHashSet::default(),
        reported: FxHashSet::default(),
        reported_keyed: false,
        control_depth: 0,
        region_mark: 0,
        frame_mark: 0,
        probing: 0,
        nested: 0,
        early_reads: Vec::new(),
        declared_loops: Vec::new(),
        local_functions: Vec::new(),
        r_arguments: None,
        invisible: None,
        hoist: None,
        converted_locals: Vec::new(),
        own: None,
    };
    judge.find(root, None);
    let mut blockers = judge.blockers;
    blockers.sort_by_key(|blocker| (blocker.range.start(), blocker.range.end()));
    blockers.dedup();
    ThinVec::from(blockers)
}

/// Which side of the boundary a type is on. `NULL` and an undetermined type are on neither.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Ry,
    R,
    Neutral,
}

fn side<'db>(
    db: &'db dyn Db,
    definitions: Option<TypeDefinitions<'db>>,
    ty: Ty<'db>,
    expanding: &mut Vec<Name<'db>>,
) -> Side {
    let all = |types: &mut dyn Iterator<Item = Ty<'db>>, expanding: &mut Vec<Name<'db>>| {
        for ty in types {
            if side(db, definitions, ty, expanding) == Side::R {
                return Side::R;
            }
        }
        Side::Ry
    };
    match ty.kind(db) {
        TyKind::Null | TyKind::Var(_) => Side::Neutral,
        TyKind::Native(_) | TyKind::Rigid(_) | TyKind::OtherTags => Side::Ry,
        TyKind::NativeTuple(items) => all(&mut items.iter().copied(), expanding),
        TyKind::NativeRecord(fields, _) => all(&mut fields.iter().map(|field| field.ty), expanding),
        TyKind::Tag(_, payload) => all(&mut payload.iter().copied(), expanding),
        // A union of tags is a ry type; any other union, `F64 | NULL` included, is how R code
        // spells a value of several types.
        TyKind::Union(members) => {
            if members
                .iter()
                .all(|member| matches!(member.kind(db), TyKind::Tag(..) | TyKind::OtherTags))
            {
                all(&mut members.iter().copied(), expanding)
            } else {
                Side::R
            }
        }
        TyKind::Frame(kind, _, _) if kind.class().is_some() => Side::R,
        TyKind::Frame(..) => Side::Ry,
        TyKind::Function(function) => all(
            &mut function
                .positional
                .iter()
                .copied()
                .chain(function.named.iter().map(|parameter| parameter.ty))
                .chain([function.ret]),
            expanding,
        ),
        TyKind::Named(name, arguments) => {
            let text = name.text(db);
            if matches!(text, "List" | "Dict" | "Set" | "Expr") && is_ry_type(db, text) {
                return all(&mut arguments.iter().copied(), expanding);
            }
            let Some(definition) = definitions.and_then(|definitions| definitions.get(db, *name))
            else {
                return Side::R;
            };
            // A recursive type is ry when everything else it holds is.
            if expanding.contains(name) {
                return Side::Ry;
            }
            expanding.push(*name);
            let body = side(db, definitions, definition.body, expanding);
            expanding.pop();
            if body == Side::R {
                Side::R
            } else {
                all(&mut arguments.iter().copied(), expanding)
            }
        }
        _ => Side::R,
    }
}

/// Whether a declared signature has a type parameter that stands for R values, such as
/// `<T: numeric>` or `<T: num>`. Such a signature admits an R type, so its function is a
/// boundary function.
pub fn has_r_bound(scheme: &TypeScheme<'_>) -> bool {
    scheme.binders.iter().any(|binder| {
        matches!(
            binder.constraint().r_side(),
            Constraint::Numeric
                | Constraint::Arithmetic
                | Constraint::ScalarNumeric
                | Constraint::AtomicElement
        )
    })
}

/// Whether a declared signature is written entirely in ry types: every one of `formals`
/// parameters has a ry type, the result has one or is `NULL` or left out, and a ry type appears
/// at least once.
fn is_native_signature<'db>(
    db: &'db dyn Db,
    definitions: Option<TypeDefinitions<'db>>,
    function: &FunctionType<'db>,
    formals: usize,
) -> bool {
    let mut expanding = Vec::new();
    let mut parameters = function
        .positional
        .iter()
        .copied()
        .chain(function.named.iter().map(|parameter| parameter.ty));
    let declared = function.positional.len() + function.named.len();
    if declared < formals
        || !parameters.all(|ty| side(db, definitions, ty, &mut expanding) == Side::Ry)
    {
        return false;
    }
    match function.ret.kind(db) {
        // An inline signature without a result type declares none.
        TyKind::Unknown => declared > 0,
        _ => match side(db, definitions, function.ret, &mut expanding) {
            Side::Ry => true,
            Side::Neutral => declared > 0,
            Side::R => false,
        },
    }
}

/// What a name in scope holds.
#[derive(Clone, Copy)]
enum Slot<'db> {
    /// A parameter, a loop variable or a name a `match` arm binds, with its type where known.
    Parameter(Option<Ty<'db>>),
    /// A parameter that takes a function, which a native caller passes as a function it knows.
    FunctionParameter,
    /// A variable, with the type of the first value it was given.
    Value(Option<Ty<'db>>),
    /// A local function, which native code runs in place where it is called.
    Function(ExprId),
    /// A name set on some paths to this point only, with its type where known. A read compiles,
    /// and where the name is unset the call gives up and the interpreter runs it; an assignment
    /// gives the name a new variable.
    Maybe(Option<Ty<'db>>),
}

/// How the value of an expression is used, which decides whether an R call may stand there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    /// Ry code reads the value.
    Value,
    /// A statement: nothing reads the value.
    Unused,
    /// The argument of `Num.from_r` or one of its siblings.
    Converted,
    /// An argument of the named R function.
    Argument(Symbol),
    /// An argument of a table function, or an operand of an operator that builds a DataFrame
    /// expression.
    TableArgument,
}

/// Whether a value used as `usage` is an argument of an R call, which R evaluates itself.
fn is_r_argument(usage: Use) -> bool {
    matches!(usage, Use::Argument(_) | Use::TableArgument)
}

enum Callee {
    Return,
    Stop,
    Local(ExprId),
    Variable(Symbol),
    FunctionParameter,
    Conversion,
    ToR(Symbol),
    Std(Symbol),
    Table,
    /// A top-level native function.
    Native(Symbol),
    R(Symbol, ROrigin),
    Computed,
}

struct Judge<'a, 'db> {
    judged: Judged<'a, 'db>,
    blockers: Vec<NativeBlocker>,
    /// The names a read may use where the walk stands. A name set in a branch or a loop leaves
    /// with it, as it does in the typed core.
    scope: Vec<(Symbol, Slot<'db>)>,
    /// Every name the function being judged binds anywhere, so a read of one that is out of
    /// scope is told apart from a read of a global.
    locals: FxHashSet<Symbol>,
    /// The local functions a branch or a loop body defined, out of scope after it.
    hidden: FxHashSet<Symbol>,
    /// Outside names already reported, so each is reported where it first appears.
    reported: FxHashSet<Symbol>,
    reported_keyed: bool,
    control_depth: u32,
    /// Where the innermost branch or loop body starts in `scope`: a name bound from there on
    /// leaves scope with it, so it may change type (the assignment arm of [`Judge::walk`]).
    region_mark: usize,
    /// Where the innermost local function or function literal starts in `scope`: an assignment
    /// to a name bound before it binds a name of the body's own.
    frame_mark: usize,
    /// How many loop trials enclose the walk ([`Judge::in_loop`]).
    probing: u32,
    /// How many local functions or function literals enclose the walk inside the judged function.
    nested: u32,
    /// Each read of a name the function sets only later, with the `nested` depth of the read.
    early_reads: Vec<(Symbol, u32)>,
    /// The names each loop body at the judged function's own level declares before the loop,
    /// from its first walk outside a trial, as the typed core remembers them
    /// (`lower::Lowering::looped`).
    declared_loops: Vec<(ExprId, Declared<'db>)>,
    /// The local functions defined so far and whether each body was walked. Native code runs a
    /// local function in place where it is called, so its body is judged with the names in scope
    /// at its first call.
    local_functions: Vec<(ExprId, bool)>,
    /// The R or table function whose arguments the walk is inside.
    r_arguments: Option<Symbol>,
    /// The expression already reported for making the function's result invisible.
    invisible: Option<ExprId>,
    /// The statement of a `{` block that an R call found here may be computed before, which
    /// is none where that would change when or whether the call runs: in a branch, in a loop,
    /// after `&&` or `||`, in a function literal, or among the arguments of an R function
    /// that evaluates them in its own way ([`defers_arguments`]).
    hoist: Option<ExprId>,
    /// Each local assigned an R value whose blocker offers to convert it, with that blocker's
    /// index. A later read of the local that converts it or passes it to R withdraws the fix,
    /// because converting at the assignment would break that read.
    converted_locals: Vec<(Symbol, usize)>,
    /// The name the judged function is assigned to, which a recursive call calls.
    own: Option<Symbol>,
}

/// The names a loop declares before it, with their types ([`Judge::in_loop`]).
type Declared<'db> = SmallVec<[(Symbol, Option<Ty<'db>>); 2]>;

/// A `for` loop's variable: where it is written, its name and the type of its items.
type LoopVariable<'db> = (TextRange, Symbol, Option<Ty<'db>>);

/// What a walk changes, which a loop trial puts back ([`Judge::in_loop`]).
#[derive(Clone)]
struct WalkState<'db> {
    scope: Vec<(Symbol, Slot<'db>)>,
    blockers: usize,
    early_reads: usize,
    hidden: FxHashSet<Symbol>,
    reported: FxHashSet<Symbol>,
    reported_keyed: bool,
    local_functions: Vec<(ExprId, bool)>,
    invisible: Option<ExprId>,
    converted_locals: Vec<(Symbol, usize)>,
}

impl<'a, 'db> Judge<'a, 'db> {
    fn block(&mut self, expression: ExprId, kind: NativeBlockerKind) {
        self.block_at(self.judged.module.range(expression), kind);
    }

    fn block_at(&mut self, range: TextRange, kind: NativeBlockerKind) {
        self.blockers.push(NativeBlocker {
            range,
            kind,
            fix: None,
        });
    }

    fn block_with_fix(
        &mut self,
        expression: ExprId,
        kind: NativeBlockerKind,
        fix: Option<NativeFix>,
    ) {
        self.blockers.push(NativeBlocker {
            range: self.judged.module.range(expression),
            kind,
            fix,
        });
    }

    fn uncompiled(&mut self, expression: ExprId, construct: Uncompiled) {
        self.block(expression, NativeBlockerKind::Uncompiled(construct));
    }

    /// Reports a dictionary or set the typed core has no layout for, once per judged function:
    /// every later use of the same collection would repeat the finding.
    fn keyed_layout(&mut self, range: TextRange, uncompiled: Uncompiled) {
        if !std::mem::replace(&mut self.reported_keyed, true) {
            self.block_at(range, NativeBlockerKind::Uncompiled(uncompiled));
        }
    }

    fn ty(&self, expression: ExprId) -> Option<Ty<'db>> {
        (self.judged.types)(expression)
    }

    fn side(&self, ty: Ty<'db>) -> Side {
        side(self.judged.db, self.judged.definitions, ty, &mut Vec::new())
    }

    fn lookup(&self, name: Symbol) -> Option<Slot<'db>> {
        self.scope
            .iter()
            .rev()
            .find(|(bound, _)| *bound == name)
            .map(|(_, slot)| *slot)
    }

    /// Looks for native functions outside any native function: a function literal with a
    /// native signature is judged, and any other is searched for ones nested in it. `own` is the
    /// name a function literal is assigned to.
    fn find(&mut self, id: ExprId, own: Option<Symbol>) {
        let module = self.judged.module;
        if let ExpressionKind::Assign { target, value, .. } = *module.kind(id)
            && let ExpressionKind::NameRef(name) = *module.kind(target)
            && matches!(module.kind(value), ExpressionKind::Function { .. })
        {
            self.find(value, Some(name));
            return;
        }
        if let ExpressionKind::Function { parameters, .. } = *module.kind(id)
            && let Some(signature) = self.judged.signatures.get(id)
        {
            let formals = module[parameters]
                .iter()
                .filter(|parameter| parameter.name != "...")
                .count();
            if is_native_signature(self.judged.db, self.judged.definitions, signature, formals) {
                self.own = own;
                self.judge(id, signature);
                return;
            }
        }
        module.for_each_child(id, |child| self.find(child, None));
    }

    fn judge(&mut self, function: ExprId, signature: &FunctionType<'db>) {
        let module = self.judged.module;
        let db = self.judged.db;
        let ExpressionKind::Function { parameters, body } = *module.kind(function) else {
            return;
        };
        self.scope.clear();
        self.reported.clear();
        self.reported_keyed = false;
        self.locals.clear();
        self.hidden.clear();
        self.control_depth = 0;
        self.region_mark = 0;
        self.frame_mark = 0;
        self.nested = 0;
        self.local_functions.clear();
        self.r_arguments = None;
        self.invisible = None;
        self.hoist = None;
        self.converted_locals.clear();
        self.collect_locals(function);
        if self.judged.aliased.contains(function) {
            let parameters = &module[parameters];
            let range = match (parameters.first(), parameters.last()) {
                (Some(first), Some(last)) => first.range.cover(last.range),
                _ => TextRange::at(module.range(function).start(), TextSize::of("function")),
            };
            self.block_at(
                range,
                NativeBlockerKind::Uncompiled(Uncompiled::AliasedSignature),
            );
        }
        let named: Vec<&str> = signature
            .named
            .iter()
            .map(|named| named.name.text(db))
            .collect();
        let paired = pair_formals(
            module[parameters]
                .iter()
                .map(|parameter| parameter.name.as_str()),
            &named,
            signature.positional.len(),
        );
        for (parameter, paired) in module[parameters].iter().zip(paired) {
            let declared = match paired {
                Paired::Rest => {
                    self.block_at(
                        parameter.range,
                        NativeBlockerKind::Uncompiled(Uncompiled::Dots),
                    );
                    continue;
                }
                Paired::Named(index) => signature.named.get(index).map(|named| named.ty),
                Paired::Positional(index) => signature.positional.get(index).copied(),
                Paired::Undeclared => None,
            };
            let mut slot = Slot::Parameter(declared);
            if let Some(declared) = declared {
                // Native code holds no function values: a function a native caller passes here
                // is lowered in place of the parameter.
                if matches!(self.peel(declared).kind(db), TyKind::Function(_)) {
                    slot = Slot::FunctionParameter;
                } else if let Some(uncompiled) = self.keyed_blocker(declared) {
                    self.keyed_layout(parameter.range, uncompiled);
                }
            }
            self.scope.push((parameter.name, slot));
        }
        self.defaults(&module[parameters], body, 0);
        if let Shown {
            visible: Some(_),
            invisible: Some(invisible),
        } = self.shown(body)
        {
            self.uncompiled(invisible, Uncompiled::InvisibleResult);
            self.invisible = Some(invisible);
        }
        self.walk(body, Use::Value);
        // A local function nothing ran is judged with the names in scope where the function
        // ends, and walking one may define more.
        let mut next = 0;
        while let Some(&(local, _)) = self.local_functions.get(next) {
            next += 1;
            self.local_function(local);
        }
        self.scope.clear();
    }

    /// Reports each default of `parameters` outside the mask `supplied` that native code cannot
    /// compute at the call: R computes a default where the body first reads it, so a call that
    /// leaves out such a parameter runs in the interpreter.
    fn defaults(&mut self, parameters: &[Parameter], body: ExprId, supplied: u64) {
        for (index, parameter) in parameters.iter().enumerate() {
            let Some(default) = parameter.default else {
                continue;
            };
            if index < 64 && supplied & (1 << index) != 0 || self.computed_ahead(default, body) {
                continue;
            }
            let range = self.judged.module.range(default);
            let reported = self.blockers.iter().any(|blocker| {
                blocker.range == range && matches!(blocker.kind, NativeBlockerKind::LazyDefault)
            });
            if !reported {
                self.block_at(range, NativeBlockerKind::LazyDefault);
            }
        }
    }

    /// Whether the default `default` of a function whose body is `body` has the same value and
    /// effect computed at the call as where the body first reads it, which is how native code
    /// takes a default (`computed_ahead` in `typed::lower`): it is made of constants, values of
    /// the frame the body does not set, the standard library's constants, operators, ry literals
    /// and calls of the standard library. A global may change under any R call the body makes.
    fn computed_ahead(&self, default: ExprId, body: ExprId) -> bool {
        let module = self.judged.module;
        let db = self.judged.db;
        let ahead = |expression: ExprId| self.computed_ahead(expression, body);
        match *module.kind(default) {
            ExpressionKind::Literal(_) => true,
            ExpressionKind::NameRef(name) => {
                let function = self
                    .ty(default)
                    .is_some_and(|ty| matches!(self.peel(ty).kind(db), TyKind::Function(_)));
                !function
                    && match self.lookup(name) {
                        None => {
                            is_native_function(name.as_str())
                                && !self
                                    .judged
                                    .globals
                                    .is_some_and(|globals| globals.defined_in_project(name, true))
                        }
                        Some(Slot::Function(_) | Slot::FunctionParameter) => false,
                        Some(_) => !assigns_name(module, body, name),
                    }
            }
            // The body's own definitions are not in scope at the call, so an operator or a
            // function it binds (`` `+` <- ``, `Num.abs <- `) would differ where R reads the default.
            ExpressionKind::Unary { operator, operand } => {
                let spelling = match operator {
                    UnaryOperator::Minus => "-",
                    UnaryOperator::Plus => "+",
                    UnaryOperator::Not => "!",
                    _ => return false,
                };
                !assigns_name(module, body, Symbol::from(spelling)) && ahead(operand)
            }
            ExpressionKind::Binary {
                operator, lhs, rhs, ..
            } => {
                let spelling = match operator {
                    BinaryOperator::Add => "+",
                    BinaryOperator::Subtract => "-",
                    BinaryOperator::Multiply => "*",
                    BinaryOperator::Divide => "/",
                    BinaryOperator::Power => "^",
                    BinaryOperator::Modulo => "%%",
                    BinaryOperator::IntegerDivide => "%/%",
                    BinaryOperator::Less => "<",
                    BinaryOperator::Greater => ">",
                    BinaryOperator::LessEq => "<=",
                    BinaryOperator::GreaterEq => ">=",
                    BinaryOperator::Equal => "==",
                    BinaryOperator::NotEqual => "!=",
                    BinaryOperator::And => "&",
                    BinaryOperator::And2 => "&&",
                    BinaryOperator::Or => "|",
                    BinaryOperator::Or2 => "||",
                    BinaryOperator::Special
                    | BinaryOperator::Sequence
                    | BinaryOperator::Pipe
                    | BinaryOperator::Tilde
                    | BinaryOperator::Help => return false,
                };
                !assigns_name(module, body, Symbol::from(spelling)) && ahead(lhs) && ahead(rhs)
            }
            ExpressionKind::Paren(inner) => ahead(inner),
            ExpressionKind::Tuple(items) | ExpressionKind::Array(items) => {
                module[items].iter().all(|&item| ahead(item))
            }
            ExpressionKind::Record(fields) => module[fields].iter().all(|field| ahead(field.value)),
            ExpressionKind::Tag { payload, .. } => payload.is_none_or(ahead),
            ExpressionKind::Call { callee, arguments } => {
                matches!(self.callee(callee), Callee::Std(name) if COMPILED_STD.contains(&name.as_str()))
                    && !matches!(*module.kind(callee), ExpressionKind::NameRef(name) if assigns_name(module, body, name))
                    && module[arguments]
                        .iter()
                        .all(|argument| argument.value.is_some_and(ahead))
            }
            _ => false,
        }
    }

    /// Walks the body of a local function the first time something runs it.
    fn local_function(&mut self, function: ExprId) {
        let unwalked = self
            .local_functions
            .iter_mut()
            .find(|(defined, walked)| *defined == function && !walked);
        if let Some(entry) = unwalked {
            entry.1 = true;
            self.function_body(function);
        }
    }

    fn collect_locals(&mut self, id: ExprId) {
        let module = self.judged.module;
        match *module.kind(id) {
            ExpressionKind::Function { parameters, .. } => {
                self.locals
                    .extend(module[parameters].iter().map(|parameter| parameter.name));
            }
            ExpressionKind::Assign { target, .. } => {
                if let ExpressionKind::NameRef(name) = *module.kind(target) {
                    self.locals.insert(name);
                }
            }
            ExpressionKind::For {
                variable: Some(variable),
                ..
            } => {
                self.locals.insert(module.name(variable));
            }
            ExpressionKind::Match { arms, .. } => {
                for arm in &module[arms] {
                    for &binding in &module[arm.bindings] {
                        if let ExpressionKind::NameRef(name) = *module.kind(binding) {
                            self.locals.insert(name);
                        }
                    }
                }
            }
            _ => {}
        }
        module.for_each_child(id, |child| self.collect_locals(child));
    }

    /// Where the paths of a function with this body give its value visibly and where invisibly,
    /// as R shows it. An assignment, a loop, an `if` without `else`, `invisible` and
    /// `List.for_each` give an invisible value, and so does a call of a local function that ends
    /// in one. The typed core returns either kind, but not a body whose paths disagree.
    fn shown(&self, body: ExprId) -> Shown {
        let mut shown = Shown::default();
        let mut walk = ShownWalk {
            module: self.judged.module,
            locals: Vec::new(),
            seen: Vec::new(),
        };
        walk.body(body, &mut shown);
        shown
    }

    /// Whether control never leaves `id` by its end.
    fn diverges(&self, id: ExprId) -> bool {
        let module = self.judged.module;
        match *module.kind(id) {
            ExpressionKind::Break | ExpressionKind::Next => true,
            ExpressionKind::Paren(inner) => self.diverges(inner),
            ExpressionKind::Block { statements } => module[statements]
                .last()
                .is_some_and(|&last| self.diverges(last)),
            ExpressionKind::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => self.diverges(then_branch) && self.diverges(else_branch),
            ExpressionKind::Call { callee, .. } => {
                matches!(self.callee(callee), Callee::Return | Callee::Stop)
            }
            _ => false,
        }
    }

    fn walk(&mut self, id: ExprId, usage: Use) {
        let module = self.judged.module;
        let db = self.judged.db;
        match *module.kind(id) {
            ExpressionKind::Missing | ExpressionKind::Break | ExpressionKind::Next => {}
            // R evaluates such a literal itself where it stands among an R call's arguments.
            ExpressionKind::Literal(LiteralKind::Complex(_) | LiteralKind::Na(_)) => {
                if !is_r_argument(usage) {
                    self.block(id, NativeBlockerKind::RLiteral);
                }
            }
            ExpressionKind::Literal(_) => {}
            ExpressionKind::NameRef(name) => self.read(id, name, usage),
            ExpressionKind::Paren(inner) => self.walk(inner, Use::Value),
            ExpressionKind::Block { statements } => {
                let statements = &module[statements];
                for (position, &statement) in statements.iter().enumerate() {
                    let last = position + 1 == statements.len();
                    let statement_use = if !last || usage == Use::Unused {
                        Use::Unused
                    } else {
                        Use::Value
                    };
                    let trial = (self.probing > 0).then(|| self.walk_state());
                    let hoist = self.hoist.replace(statement);
                    self.walk(statement, statement_use);
                    self.hoist = hoist;
                    // A loop trial leaves out a statement that reads a name too early, as the
                    // typed core's does (`lower::Lowering::trial_part`).
                    if let Some(state) = trial
                        && self.blockers[state.blockers..].iter().any(|blocker| {
                            matches!(
                                blocker.kind,
                                NativeBlockerKind::Uncompiled(Uncompiled::BranchOnlyName(_))
                            )
                        })
                    {
                        self.restore(state);
                    }
                }
            }
            ExpressionKind::Assign {
                spelling,
                target,
                value,
            } => self.assign(id, spelling, target, value),
            ExpressionKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.walk(condition, Use::Value);
                let branch_use = if usage == Use::Unused {
                    Use::Unused
                } else {
                    Use::Value
                };
                let mark = self.scope.len();
                self.control_depth += 1;
                let region_mark = std::mem::replace(&mut self.region_mark, mark);
                let hoist = self.hoist.take();
                self.walk(then_branch, branch_use);
                let then_names = self.scope.split_off(mark);
                let else_names = match else_branch {
                    Some(else_branch) => {
                        self.walk(else_branch, branch_use);
                        self.scope.split_off(mark)
                    }
                    None => Vec::new(),
                };
                self.hoist = hoist;
                self.control_depth -= 1;
                self.region_mark = region_mark;
                let mut ends = Vec::new();
                if !self.diverges(then_branch) {
                    ends.push(then_names);
                }
                if else_branch.is_none_or(|branch| !self.diverges(branch)) {
                    ends.push(else_names);
                }
                self.join(&ends);
                if branch_use == Use::Value
                    && else_branch.is_some()
                    && self.ty(id).is_some_and(|ty| self.is_mixed_union(ty))
                {
                    self.uncompiled(id, Uncompiled::BranchTypes);
                }
                // A branch that gives `NULL` gives it on both paths.
                if branch_use == Use::Value
                    && else_branch.is_none()
                    && self.invisible != Some(id)
                    && !self
                        .ty(then_branch)
                        .is_some_and(|ty| matches!(ty.kind(db), TyKind::Null))
                {
                    self.uncompiled(id, Uncompiled::IfWithoutElse);
                }
            }
            ExpressionKind::For {
                variable,
                sequence,
                body,
            } => {
                self.walk(sequence, Use::Value);
                let mark = self.scope.len();
                let variable = variable.map(|variable| {
                    let item = self
                        .ty(sequence)
                        .and_then(|ty| array_element(db, self.peel(ty)));
                    (module.name_range(variable), module.name(variable), item)
                });
                self.in_loop(body, mark, variable);
            }
            ExpressionKind::While { condition, body } => {
                // The condition runs on every pass, so it is a region of its own, whose names
                // stay in scope after it, as the typed core lowers it.
                let hoist = self.hoist.take();
                self.control_depth += 1;
                let region_mark = std::mem::replace(&mut self.region_mark, self.scope.len());
                self.walk(condition, Use::Value);
                self.region_mark = region_mark;
                self.control_depth -= 1;
                self.hoist = hoist;
                self.in_loop(body, self.scope.len(), None);
            }
            ExpressionKind::Repeat { body } => self.in_loop(body, self.scope.len(), None),
            ExpressionKind::Local { .. } => {
                self.r_position(id, Symbol::from("local"), ROrigin::Library, usage);
            }
            ExpressionKind::Unary { operator, operand } => match operator {
                UnaryOperator::Tilde => self.uncompiled(id, Uncompiled::Construct("a formula")),
                UnaryOperator::Help => self.uncompiled(id, Uncompiled::Construct("a help query")),
                _ => {
                    let usage = self.operand_use(id);
                    if usage == Use::TableArgument {
                        self.table_operand(operand);
                    }
                    self.walk(operand, usage);
                }
            },
            ExpressionKind::Binary {
                operator,
                special_name,
                lhs,
                rhs,
            } => match operator {
                BinaryOperator::Pipe => match *module.kind(rhs) {
                    ExpressionKind::Call { callee, arguments } => {
                        let mut values: Vec<(Option<Symbol>, ExprId)> = vec![(None, lhs)];
                        values.extend(named_values(module, &module[arguments]));
                        self.call(id, callee, &values, usage);
                    }
                    _ => self.uncompiled(id, Uncompiled::Construct("a pipe into this form")),
                },
                BinaryOperator::Special => {
                    let name = special_name.unwrap_or(Symbol::from("%%"));
                    self.r_call(
                        id,
                        name,
                        ROrigin::Library,
                        &[(None, lhs), (None, rhs)],
                        usage,
                    );
                }
                BinaryOperator::Sequence => {
                    self.block(id, NativeBlockerKind::Range);
                    self.walk(lhs, Use::Value);
                    self.walk(rhs, Use::Value);
                }
                BinaryOperator::Tilde => self.uncompiled(id, Uncompiled::Construct("a formula")),
                BinaryOperator::Help => self.uncompiled(id, Uncompiled::Construct("a help query")),
                BinaryOperator::And2 | BinaryOperator::Or2 => {
                    self.walk(lhs, Use::Value);
                    let mark = self.scope.len();
                    self.control_depth += 1;
                    let region_mark = std::mem::replace(&mut self.region_mark, mark);
                    let hoist = self.hoist.take();
                    self.walk(rhs, Use::Value);
                    self.hoist = hoist;
                    self.region_mark = region_mark;
                    self.control_depth -= 1;
                    self.scope.truncate(mark);
                }
                _ => {
                    let operand = self.operand_use(id);
                    if operand == Use::TableArgument {
                        self.table_operand(lhs);
                        self.table_operand(rhs);
                    }
                    self.walk(lhs, operand);
                    self.walk(rhs, operand);
                }
            },
            ExpressionKind::Call { callee, arguments } => {
                let values: Vec<_> = named_values(module, &module[arguments]).collect();
                self.call(id, callee, &values, usage);
            }
            ExpressionKind::Index {
                double,
                target,
                arguments,
                ..
            } => {
                self.walk(target, Use::Value);
                let arguments = &module[arguments];
                for argument in arguments {
                    if let Some(value) = argument.value {
                        self.walk(value, Use::Value);
                    }
                }
                let target_type = self.ty(target).map(|ty| self.peel(ty));
                if target_type.is_some_and(|ty| matches!(ty.kind(db), TyKind::Frame(..))) {
                    let by_name = arguments
                        .first()
                        .and_then(|argument| argument.value)
                        .and_then(|position| self.ty(position))
                        .is_none_or(|ty| {
                            matches!(
                                self.peel(ty).kind(db),
                                TyKind::Native(NativeScalar::Str)
                                    | TyKind::Scalar(Atomic::Character)
                            )
                        });
                    if !by_name {
                        self.uncompiled(
                            id,
                            Uncompiled::Advised(
                                "a column read by its position",
                                "Read the column by its name, as in `table[[\"price\"]]`",
                            ),
                        );
                    }
                    self.column_read(id, usage);
                    return;
                }
                // Single brackets take the items a list of `Bool` marks, an `I64` position or a
                // list of `I64` positions picks, which compiles.
                let by_position = arguments
                    .first()
                    .and_then(|argument| argument.value)
                    .and_then(|position| self.ty(position))
                    .is_some_and(|ty| {
                        let ty = self.peel(ty);
                        let scalar = array_element(db, ty).map_or(ty, |item| self.peel(item));
                        matches!(scalar.kind(db), TyKind::Native(NativeScalar::I64))
                            || array_element(db, ty).is_some_and(|_| {
                                matches!(scalar.kind(db), TyKind::Native(NativeScalar::Bool))
                            })
                    });
                let whole_literal = arguments.first().is_some_and(|argument| {
                    let value = argument.value.map(|value| match *module.kind(value) {
                        ExpressionKind::Unary {
                            operator: UnaryOperator::Minus,
                            operand,
                        } => operand,
                        _ => value,
                    });
                    value.is_some_and(|value| match *module.kind(value) {
                        ExpressionKind::Literal(
                            LiteralKind::Integer(text) | LiteralKind::Double(text),
                        ) => module[text]
                            .trim_end_matches('L')
                            .parse::<f64>()
                            .is_ok_and(|number| number.fract() == 0.0),
                        _ => false,
                    })
                });
                if !double
                    && !by_position
                    && !whole_literal
                    && target_type.is_some_and(|ty| array_element(db, ty).is_some())
                {
                    let empty = arguments
                        .first()
                        .is_none_or(|argument| argument.value.is_none());
                    self.uncompiled(
                        id,
                        Uncompiled::Advised(
                            if empty {
                                "a list indexed with empty single brackets"
                            } else {
                                "a list indexed with single brackets by something other than positions or a mask"
                            },
                            if empty {
                                "Use the list itself"
                            } else {
                                "Index the list with an `I64` position, a `List<I64>` of positions or a `List<Bool>` mask"
                            },
                        ),
                    );
                    return;
                }
                let one_position = matches!(
                    arguments,
                    [Argument {
                        name: None,
                        value: Some(_)
                    }]
                );
                let literal_position = arguments.first().is_some_and(|argument| {
                    argument.value.is_some_and(|value| {
                        matches!(*module.kind(value), ExpressionKind::Literal(_))
                    })
                });
                let compiles = one_position
                    && match target_type.map(|ty| ty.kind(db)) {
                        Some(TyKind::NativeTuple(_)) => double && literal_position,
                        Some(TyKind::NativeRecord(_, _)) => false,
                        _ => true,
                    };
                if !compiles {
                    self.uncompiled(id, Uncompiled::Construct("an index of this form"));
                }
            }
            ExpressionKind::Field { at, target, .. } => {
                self.walk(target, Use::Value);
                if at {
                    self.uncompiled(id, Uncompiled::Construct("`@`"));
                } else if self
                    .ty(target)
                    .is_some_and(|ty| matches!(self.peel(ty).kind(db), TyKind::Frame(..)))
                {
                    self.column_read(id, usage);
                }
            }
            ExpressionKind::Namespace { .. } | ExpressionKind::Function { .. } => {
                self.uncompiled(id, Uncompiled::FunctionValue);
            }
            ExpressionKind::Tuple(items) | ExpressionKind::Array(items) => {
                for &item in &module[items] {
                    self.boxed(item);
                    self.walk(item, Use::Value);
                }
            }
            ExpressionKind::FormatString(pieces) => {
                for &piece in &module[pieces] {
                    self.walk(piece, Use::Value);
                }
            }
            ExpressionKind::Record(fields) => {
                for field in &module[fields] {
                    self.boxed(field.value);
                    self.walk(field.value, Use::Value);
                }
            }
            ExpressionKind::RecordUpdate { base, fields } => {
                self.walk(base, Use::Value);
                for field in &module[fields] {
                    self.boxed(field.value);
                    self.walk(field.value, Use::Value);
                }
            }
            ExpressionKind::Tag { payload, .. } => {
                if let Some(payload) = payload {
                    self.boxed(payload);
                    self.walk(payload, Use::Value);
                }
            }
            ExpressionKind::Try(operand) => self.walk(operand, Use::Value),
            ExpressionKind::Match { scrutinee, arms } => {
                self.walk(scrutinee, Use::Value);
                let matched = self.ty(scrutinee).map(|ty| self.peel(ty));
                let matches_shape = matched.is_none_or(|ty| match ty.kind(db) {
                    TyKind::NativeRecord(_, _)
                    | TyKind::NativeTuple(_)
                    | TyKind::Tag(..)
                    | TyKind::OtherTags
                    | TyKind::Native(_)
                    | TyKind::Scalar(
                        Atomic::Double | Atomic::Integer | Atomic::Logical | Atomic::Character,
                    ) => true,
                    TyKind::Union(_) => !self.is_mixed_union(ty),
                    _ => array_element(db, ty).is_some(),
                });
                // A pattern that tests nothing compiles on a value of any type.
                let tests = module[arms]
                    .iter()
                    .any(|arm| !irrefutable(module, arm.pattern));
                if !matches_shape && tests {
                    self.uncompiled(scrutinee, Uncompiled::MatchScrutinee);
                }
                let branch_use = if usage == Use::Unused {
                    Use::Unused
                } else {
                    Use::Value
                };
                let mut ends = Vec::new();
                for arm in &module[arms] {
                    if matches_shape && !self.pattern_compiles(arm.pattern) {
                        self.block_at(
                            module.pattern(arm.pattern).range,
                            NativeBlockerKind::Uncompiled(Uncompiled::MatchPattern),
                        );
                    }
                    let mark = self.scope.len();
                    self.control_depth += 1;
                    let region_mark = std::mem::replace(&mut self.region_mark, mark);
                    for &binding in &module[arm.bindings] {
                        if let ExpressionKind::NameRef(name) = *module.kind(binding) {
                            self.bind_assigned(module.range(binding), name, self.ty(binding));
                        }
                    }
                    if let Some(guard) = arm.guard {
                        if writes(module, guard) {
                            self.uncompiled(guard, Uncompiled::MatchGuard);
                        } else {
                            self.walk(guard, Use::Value);
                        }
                    }
                    let hoist = self.hoist.take();
                    self.walk(arm.body, branch_use);
                    self.hoist = hoist;
                    let names = self.scope.split_off(mark);
                    if !self.diverges(arm.body) {
                        ends.push(names);
                    }
                    self.control_depth -= 1;
                    self.region_mark = region_mark;
                }
                self.join(&ends);
                if branch_use == Use::Value && self.ty(id).is_some_and(|ty| self.is_mixed_union(ty))
                {
                    self.uncompiled(id, Uncompiled::BranchTypes);
                }
            }
        }
    }

    /// How an operator reads its operands. One that builds a DataFrame expression is a table
    /// call, so an operand may be what an argument of one may be.
    fn operand_use(&self, operator: ExprId) -> Use {
        if self.is_expression(operator) {
            Use::TableArgument
        } else {
            Use::Value
        }
    }

    fn is_expression(&self, id: ExprId) -> bool {
        let db = self.judged.db;
        self.ty(id).is_some_and(
            |ty| matches!(self.peel(ty).kind(db), TyKind::Named(name, _) if name.text(db) == "Expr"),
        )
    }

    /// Reports a table value written as a field of a record, a tuple, a tag or a list, which
    /// hold ry values only.
    fn boxed(&mut self, item: ExprId) {
        let db = self.judged.db;
        let frame = self
            .ty(item)
            .is_some_and(|ty| matches!(self.peel(ty).kind(db), TyKind::Frame(..)));
        if frame || self.is_expression(item) {
            self.uncompiled(
                item,
                Uncompiled::Advised(
                    "a table value inside a record, a tuple, a tag or a list",
                    "Keep the table value in a variable of its own",
                ),
            );
        }
    }

    /// The typed core tells a table expression among an operator's operands by the operand's
    /// form, before it compiles the operand: a variable, another operator, or a call of a table
    /// function or of a top-level native function that names a table type as its result. An expression of any other form reaches the
    /// operator as a value native code cannot compare.
    fn table_operand(&mut self, operand: ExprId) {
        if !self.is_expression(operand) {
            return;
        }
        let module = self.judged.module;
        let mut inner = operand;
        while let ExpressionKind::Paren(next) = *module.kind(inner) {
            inner = next;
        }
        let called = match *module.kind(inner) {
            ExpressionKind::Binary {
                operator: BinaryOperator::Pipe,
                rhs,
                ..
            } => match *module.kind(rhs) {
                ExpressionKind::Call { callee, .. } => Some(callee),
                _ => return,
            },
            ExpressionKind::NameRef(_)
            | ExpressionKind::Unary { .. }
            | ExpressionKind::Binary { .. } => return,
            ExpressionKind::Call { callee, .. } => Some(callee),
            _ => None,
        };
        let db = self.judged.db;
        let told = called.is_some_and(|callee| match self.callee(callee) {
            Callee::Table => true,
            // The typed core reads the result type as the preflight spells it, so an alias of a
            // table type tells it something only when it has a canonical spelling.
            Callee::Native(name) => !self
                .judged
                .globals
                .and_then(|globals| globals.scheme(name, true))
                .is_some_and(|scheme| match scheme.body.kind(db) {
                    TyKind::Function(function) => match function.ret.kind(db) {
                        TyKind::Named(written, _) if !is_table_type(written.text(db)) => !self
                            .judged
                            .definitions
                            .and_then(|definitions| {
                                canonical_spelling(db, definitions, function.ret)
                            })
                            .is_some_and(|text| {
                                is_table_type(
                                    text.split(|next: char| !next.is_alphanumeric())
                                        .next()
                                        .unwrap_or_default(),
                                )
                            }),
                        _ => false,
                    },
                    _ => false,
                }),
            _ => false,
        });
        if !told {
            self.uncompiled(operand, Uncompiled::TableOperand);
        }
    }

    /// Walks the loop body `body`, after which each name the body created is set only if the
    /// body ran far enough to set it. A name the body reads before a later statement of it sets
    /// it is declared before the loop with the type a trial walk that leaves out the early reads
    /// gives it, round by round as earlier names' types show more, as the typed core declares
    /// it (`lower::Lowering::looped`). A read inside a local function or a function literal is
    /// not declared, because the core inlines that body in a scope of its own.
    fn in_loop(&mut self, body: ExprId, mark: usize, variable: Option<LoopVariable<'db>>) {
        let entry = self.walk_state();
        let remembers = self.nested == 0;
        let module = self.judged.module;
        let mut declared = self
            .declared_loops
            .iter()
            .find(|&&(loop_body, _)| remembers && loop_body == body)
            .map(|(_, names)| names.clone())
            .unwrap_or_default();
        loop {
            self.declare(&declared);
            self.loop_body(body, mark, variable);
            let mut forward: SmallVec<[Symbol; 2]> = SmallVec::new();
            for &(name, nested) in &self.early_reads[entry.early_reads..] {
                if nested == self.nested
                    && !forward.contains(&name)
                    && !declared.iter().any(|&(declared, _)| declared == name)
                    && assigns_name(module, body, name)
                {
                    forward.push(name);
                }
            }
            if forward.is_empty() {
                if remembers && self.probing == 0 && !declared.is_empty() {
                    self.declared_loops
                        .retain(|&(loop_body, _)| loop_body != body);
                    self.declared_loops.push((body, declared));
                }
                return;
            }
            self.restore(entry.clone());
            self.declare(&declared);
            self.probing += 1;
            self.loop_body(body, mark, variable);
            self.probing -= 1;
            let learned: Declared<'db> = forward
                .into_iter()
                .filter_map(|name| match self.lookup(name) {
                    Some(Slot::Maybe(ty) | Slot::Value(ty)) => Some((name, ty)),
                    _ => None,
                })
                .collect();
            self.restore(entry.clone());
            if learned.is_empty() {
                self.declare(&declared);
                self.loop_body(body, mark, variable);
                return;
            }
            declared.extend(learned);
        }
    }

    fn declare(&mut self, declared: &[(Symbol, Option<Ty<'db>>)]) {
        for &(name, ty) in declared {
            self.scope.push((name, Slot::Maybe(ty)));
        }
    }

    fn walk_state(&self) -> WalkState<'db> {
        WalkState {
            scope: self.scope.clone(),
            blockers: self.blockers.len(),
            early_reads: self.early_reads.len(),
            hidden: self.hidden.clone(),
            reported: self.reported.clone(),
            reported_keyed: self.reported_keyed,
            local_functions: self.local_functions.clone(),
            invisible: self.invisible,
            converted_locals: self.converted_locals.clone(),
        }
    }

    fn restore(&mut self, state: WalkState<'db>) {
        self.scope = state.scope;
        self.blockers.truncate(state.blockers);
        self.early_reads.truncate(state.early_reads);
        self.hidden = state.hidden;
        self.reported = state.reported;
        self.reported_keyed = state.reported_keyed;
        self.local_functions = state.local_functions;
        self.invisible = state.invisible;
        self.converted_locals = state.converted_locals;
    }

    /// Walks a loop body, whose names from `mark` on leave scope with it. The names declared
    /// before the body ([`Judge::declare`]) stand outside the body's region, since the typed
    /// core binds them before it enters the body, and the loop variable is the region's first.
    fn loop_body(&mut self, body: ExprId, mark: usize, variable: Option<LoopVariable<'db>>) {
        self.control_depth += 1;
        let region_mark = std::mem::replace(&mut self.region_mark, self.scope.len());
        if let Some((at, name, item)) = variable {
            self.bind_assigned(at, name, item);
        }
        let hoist = self.hoist.take();
        self.walk(body, Use::Unused);
        self.hoist = hoist;
        self.region_mark = region_mark;
        self.control_depth -= 1;
        for (name, slot) in self.scope.split_off(mark) {
            let ty = match slot {
                Slot::Function(_) => {
                    self.hidden.insert(name);
                    continue;
                }
                Slot::Value(ty) | Slot::Parameter(ty) | Slot::Maybe(ty) => ty,
                Slot::FunctionParameter => None,
            };
            self.scope.push((name, Slot::Maybe(ty)));
        }
    }

    /// Binds a loop variable or a pattern's name, which the typed core assigns: a name the
    /// function already holds keeps its variable, so a value stored another way gives the
    /// function up.
    fn bind_assigned(&mut self, at: TextRange, name: Symbol, ty: Option<Ty<'db>>) {
        match self.lookup(name) {
            Some(Slot::Value(held) | Slot::Parameter(held)) => {
                if let (Some(held), Some(ty)) = (held, ty)
                    && self.layout_differs(held, ty)
                {
                    let kind = Uncompiled::TypeChange(name);
                    self.block_at(at, NativeBlockerKind::Uncompiled(kind));
                }
            }
            _ => self.scope.push((name, Slot::Parameter(ty))),
        }
    }

    /// Brings the names that the paths reaching the end of a branch created into scope after
    /// it, as the typed core joins them. A name every path set, to types stored alike, keeps its
    /// slot; one some paths set, all to one type, is `Maybe`; any other leaves scope.
    fn join(&mut self, ends: &[Vec<(Symbol, Slot<'db>)>]) {
        let mut names: Vec<Symbol> = Vec::new();
        for &(name, slot) in ends.iter().flatten() {
            if matches!(slot, Slot::Function(_)) {
                self.hidden.insert(name);
            } else if !names.contains(&name) {
                names.push(name);
            }
        }
        for name in names {
            let mut everywhere = true;
            let mut agree = true;
            let mut same = true;
            let mut first: Option<(Slot<'db>, Option<Ty<'db>>)> = None;
            for end in ends {
                let slot = end
                    .iter()
                    .find(|(bound, _)| *bound == name)
                    .map(|&(_, slot)| slot);
                let ty = match slot {
                    Some(Slot::Value(ty) | Slot::Parameter(ty)) => ty,
                    Some(Slot::Maybe(ty)) => {
                        everywhere = false;
                        ty
                    }
                    Some(Slot::Function(_) | Slot::FunctionParameter) | None => {
                        everywhere = false;
                        continue;
                    }
                };
                match (first, slot) {
                    (None, Some(slot)) => first = Some((slot, ty)),
                    (Some((_, held)), _) => {
                        same &= held == ty;
                        if let (Some(held), Some(ty)) = (held, ty) {
                            agree &= !self.layout_differs(held, ty);
                        }
                    }
                    _ => {}
                }
            }
            // The typed core joins types only where every path sets the name, so a name some
            // paths leave unset keeps one variable, and its paths must agree on the exact type.
            if let (true, Some((slot, ty))) = (agree && (everywhere || same), first) {
                let slot = if everywhere { slot } else { Slot::Maybe(ty) };
                self.scope.push((name, slot));
            }
        }
    }

    /// Whether `ty` is a union of anything but tags, which no single native value holds.
    fn is_mixed_union(&self, ty: Ty<'db>) -> bool {
        let db = self.judged.db;
        match ty.kind(db) {
            TyKind::Union(members) => !members
                .iter()
                .all(|member| matches!(member.kind(db), TyKind::Tag(..) | TyKind::OtherTags)),
            _ => false,
        }
    }

    /// `ty` with the aliases and nominal types around it replaced by what they stand for.
    fn peel(&self, mut ty: Ty<'db>) -> Ty<'db> {
        let db = self.judged.db;
        for _ in 0..8 {
            let TyKind::Named(name, arguments) = ty.kind(db) else {
                break;
            };
            let text = name.text(db);
            if matches!(text, "List" | "Dict" | "Set" | "Expr") && is_ry_type(db, text) {
                break;
            }
            let Some(definition) = self
                .judged
                .definitions
                .and_then(|definitions| definitions.get(db, *name))
            else {
                break;
            };
            ty = apply_definition(db, definition, arguments);
        }
        ty
    }

    /// Why the typed core has no layout for `ty`: it keys and fills a dictionary with `I64`,
    /// `F64` or `Str` only, and a set, whose elements are a dictionary's keys, with `I64` or
    /// `Str` (a float's equality is not a key's).
    fn keyed_blocker(&self, ty: Ty<'db>) -> Option<Uncompiled> {
        let db = self.judged.db;
        if let Some((key, value)) = dict_entries(db, ty) {
            let key_fits = matches!(
                key.kind(db),
                TyKind::Native(NativeScalar::I64 | NativeScalar::F64 | NativeScalar::Str)
                    | TyKind::Scalar(Atomic::Double | Atomic::Character)
                    | TyKind::Var(_)
            );
            // The lowering lays the values out by the checker's spelling of their type, so a
            // value fits exactly when it has one; a variable is settled by a later insert.
            let value_fits = matches!(value.kind(db), TyKind::Var(_))
                || stored_spelling(db, value, &|ty| self.peel(ty), &mut String::new(), 0).is_some();
            return (!(key_fits && value_fits)).then_some(Uncompiled::DictEntries);
        }
        let element = set_element(db, ty)?;
        let fits = matches!(
            element.kind(db),
            TyKind::Native(NativeScalar::I64 | NativeScalar::Str)
                | TyKind::Scalar(Atomic::Character)
                | TyKind::Var(_)
        );
        (!fits).then_some(Uncompiled::SetElements)
    }

    /// Whether the typed core compiles `pattern`. `top` says it stands at the top of an arm, or
    /// inside an alternative or an `as` there, over a value whose shapes are listed, where a record
    /// pattern may end in `..`.
    fn pattern_compiles(&self, pattern: PatId) -> bool {
        let module = self.judged.module;
        match module.pattern(pattern).kind {
            PatternKind::Missing | PatternKind::Wildcard | PatternKind::Bind(_) => true,
            PatternKind::Literal { literal, .. } => matches!(
                literal,
                LiteralKind::Integer(_)
                    | LiteralKind::Double(_)
                    | LiteralKind::Suffixed { .. }
                    | LiteralKind::String(_)
                    | LiteralKind::Logical(_)
                    | LiteralKind::Inf
            ),
            PatternKind::Tag { payload, .. } => {
                payload.is_none_or(|payload| self.pattern_compiles(payload))
            }
            PatternKind::Tuple(items) => module[items]
                .iter()
                .all(|&item| self.pattern_compiles(item)),
            PatternKind::Record { fields, .. } => module[fields]
                .iter()
                .all(|field| self.pattern_compiles(field.pattern)),
            PatternKind::List { items, .. } => module[items]
                .iter()
                .all(|&item| self.pattern_compiles(item)),
            PatternKind::As { pattern, .. } => self.pattern_compiles(pattern),
            PatternKind::Or(alternatives) => module[alternatives]
                .iter()
                .all(|&alternative| self.pattern_compiles(alternative)),
        }
    }

    fn read(&mut self, id: ExprId, name: Symbol, usage: Use) {
        let db = self.judged.db;
        match self.lookup(name) {
            Some(Slot::Function(_)) => {
                self.uncompiled(id, Uncompiled::FunctionValue);
                return;
            }
            Some(_) => {
                // Passed to R, a converted `I64` or list of them would need `Num.to_r`.
                for &(local, index) in &self.converted_locals {
                    let blocker = &mut self.blockers[index];
                    let withdrawn = usage == Use::Converted
                        || (is_r_argument(usage)
                            && matches!(
                                blocker.fix,
                                Some(NativeFix::Convert(Conversion::Num | Conversion::List))
                            ));
                    if local == name && withdrawn {
                        blocker.fix = None;
                    }
                }
                return;
            }
            None => {}
        }
        if self.hidden.contains(&name) {
            self.uncompiled(id, Uncompiled::LocalFunctionInBranch(name));
            return;
        }
        if self.locals.contains(&name) {
            self.early_reads.push((name, self.nested));
            self.uncompiled(id, Uncompiled::BranchOnlyName(name));
            return;
        }
        // Among an R call's arguments, R reads a variable from outside the function itself, when
        // the call runs, so its value needs no ry type.
        if is_r_argument(usage) {
            return;
        }
        let ty = self.ty(id);
        let enclosing = self
            .judged
            .naming
            .resolutions
            .get(&id)
            .and_then(|binding| self.judged.naming.bindings.get(binding))
            .is_some_and(|binding| binding.kind != BindingKind::TopLevel);
        let project = self
            .judged
            .globals
            .is_some_and(|globals| globals.defined_in_project(name, true));
        let is_function = ty.is_some_and(|ty| matches!(ty.kind(db), TyKind::Function(_)));
        if is_function {
            self.uncompiled(id, Uncompiled::FunctionValue);
            return;
        }
        // `Num.pi` and the other constants of the standard library are compiled in.
        if !enclosing && !project && is_native_function(name.as_str()) {
            return;
        }
        if !self.reported.insert(name) {
            return;
        }
        let holds_r = ty.is_none_or(|ty| self.side(ty) != Side::Ry);
        let kind = if holds_r {
            NativeBlockerKind::RVariable {
                name,
                conversion: ty.and_then(|ty| self.conversion(ty)),
                enclosing,
            }
        } else if enclosing {
            NativeBlockerKind::Uncompiled(Uncompiled::EnclosingRead(name))
        } else if ty.is_some_and(|ty| self.reads_globally(ty)) {
            return;
        } else {
            NativeBlockerKind::Uncompiled(Uncompiled::GlobalRead(name))
        };
        self.block(id, kind);
    }

    /// Whether native code reads a global of type `ty` where it runs: an `F64`, `I64`, `Bool` or
    /// `Str`, a `List` of them, or a table value. The typed core takes the type from the value
    /// it finds, and a record, a tuple, a tag, a `Dict` or a `Set` from R has none it can read.
    fn reads_globally(&self, ty: Ty<'db>) -> bool {
        let db = self.judged.db;
        let ty = self.peel(ty);
        let scalar = |ty: Ty<'db>| matches!(ty.kind(db), TyKind::Native(_));
        scalar(ty)
            || array_element(db, ty).is_some_and(|item| scalar(self.peel(item)))
            || matches!(ty.kind(db), TyKind::Frame(..))
            || matches!(ty.kind(db), TyKind::Named(name, _) if name.text(db) == "Expr")
    }

    /// The `from_r` that takes an R value of type `ty`.
    fn conversion(&self, ty: Ty<'db>) -> Option<Conversion> {
        let db = self.judged.db;
        match ty.kind(db) {
            TyKind::Scalar(Atomic::Double | Atomic::Integer) => Some(Conversion::Num),
            TyKind::Scalar(Atomic::Character) => Some(Conversion::Str),
            TyKind::Scalar(Atomic::Logical) => Some(Conversion::Bool),
            TyKind::Vector(_) | TyKind::NamedVector(_) => Some(Conversion::List),
            TyKind::Native(NativeScalar::F64 | NativeScalar::I64) => Some(Conversion::Num),
            TyKind::Native(NativeScalar::Str) => Some(Conversion::Str),
            TyKind::Native(NativeScalar::Bool) => Some(Conversion::Bool),
            _ if array_element(db, ty).is_some() => Some(Conversion::List),
            _ => None,
        }
    }

    fn assign(&mut self, id: ExprId, spelling: AssignSpelling, target: ExprId, value: ExprId) {
        let module = self.judged.module;
        match spelling {
            AssignSpelling::Super => {
                self.uncompiled(id, Uncompiled::SuperAssignment);
                self.walk(value, Use::Value);
                return;
            }
            AssignSpelling::Destructure => {
                self.uncompiled(id, Uncompiled::AssignmentTarget);
                self.walk(value, Use::Value);
                return;
            }
            AssignSpelling::Local | AssignSpelling::Equals => {}
        }
        match *module.kind(target) {
            ExpressionKind::NameRef(name) => {
                if matches!(*module.kind(value), ExpressionKind::Function { .. }) {
                    // Native code inlines a local function where its definition stands, so one
                    // name cannot stand for a second definition inside a branch or a loop.
                    // A name another branch hid may be defined again, as in both branches of an
                    // `if`, since it stays hidden after them.
                    if self.control_depth > 0 {
                        if self.lookup(name).is_some() {
                            self.uncompiled(id, Uncompiled::LocalFunctionInBranch(name));
                        }
                    } else {
                        self.hidden.remove(&name);
                    }
                    self.local_functions.push((value, false));
                    self.bind(name, Slot::Function(value));
                    return;
                }
                let first = self.blockers.len();
                self.walk(value, Use::Value);
                let range = module.range(value);
                if let Some(index) = (first..self.blockers.len()).find(|&index| {
                    self.blockers[index].range == range
                        && matches!(self.blockers[index].fix, Some(NativeFix::Convert(_)))
                }) {
                    self.converted_locals.push((name, index));
                }
                let ty = self.ty(value);
                // A name set on some paths takes a new variable. A variable holding tags widens
                // to every tag assigned to it, so only another kind of value changes its type.
                // That takes a new variable too wherever no later code can read the old one:
                // outside any branch or loop, or for a name the innermost one bound.
                let held = match self.lookup(name) {
                    Some(Slot::Value(first) | Slot::Parameter(first)) => Some(first),
                    Some(Slot::FunctionParameter) => Some(None),
                    Some(Slot::Function(_) | Slot::Maybe(_)) | None => None,
                };
                match held {
                    Some(first) => {
                        if let (Some(first), Some(later)) = (first, ty)
                            && self.layout_differs(first, later)
                        {
                            let index = self.scope.iter().rposition(|&(bound, _)| bound == name);
                            // A local function's assignment binds a name of its own, as in R,
                            // so the enclosing function's slot keeps its type.
                            let enclosing = index.is_some_and(|index| index < self.frame_mark);
                            let bound_here = self.control_depth == 0
                                || enclosing
                                || index.is_some_and(|index| index >= self.region_mark);
                            if enclosing {
                                self.scope.push((name, Slot::Value(ty)));
                            } else if bound_here {
                                self.bind(name, Slot::Value(ty));
                            } else {
                                self.uncompiled(id, Uncompiled::TypeChange(name));
                            }
                        }
                    }
                    None => self.bind(name, Slot::Value(ty)),
                }
            }
            ExpressionKind::Index {
                target: base,
                arguments,
                ..
            } => {
                self.walk(value, Use::Value);
                let arguments = &module[arguments];
                for argument in arguments {
                    if let Some(position) = argument.value {
                        self.walk(position, Use::Value);
                    }
                }
                let local = matches!(*module.kind(base), ExpressionKind::NameRef(name)
                    if matches!(self.lookup(name), Some(Slot::Value(..) | Slot::Parameter(_))));
                let one_position = matches!(
                    arguments,
                    [Argument {
                        name: None,
                        value: Some(_)
                    }]
                );
                if !local || !one_position {
                    self.uncompiled(id, Uncompiled::AssignmentTarget);
                }
            }
            _ => {
                self.uncompiled(id, Uncompiled::AssignmentTarget);
                self.walk(value, Use::Value);
            }
        }
    }

    fn bind(&mut self, name: Symbol, slot: Slot<'db>) {
        match self
            .scope
            .iter_mut()
            .rev()
            .find(|(bound, _)| *bound == name)
        {
            Some(entry) => entry.1 = slot,
            None => self.scope.push((name, slot)),
        }
    }

    /// Whether the typed core stores values of the two types differently: they are different
    /// kinds of ry value, or the same kind with a field, an item or an entry that differs. Two
    /// tags never differ here, and a type the check left open or an R type differs from nothing.
    fn layout_differs(&self, first: Ty<'db>, later: Ty<'db>) -> bool {
        let db = self.judged.db;
        let (first, later) = (self.peel(first), self.peel(later));
        if first == later {
            return false;
        }
        if let (Some(first), Some(later)) = (self.storage(first), self.storage(later)) {
            return first != later;
        }
        match (first.kind(db), later.kind(db)) {
            (TyKind::NativeRecord(first, _), TyKind::NativeRecord(later, _)) => {
                first.len() != later.len()
                    || first.iter().zip(later.iter()).any(|(first, later)| {
                        first.name != later.name || self.layout_differs(first.ty, later.ty)
                    })
            }
            (TyKind::NativeTuple(first), TyKind::NativeTuple(later)) => {
                first.len() != later.len()
                    || first
                        .iter()
                        .zip(later.iter())
                        .any(|(&first, &later)| self.layout_differs(first, later))
            }
            (TyKind::Named(first_name, first), TyKind::Named(later_name, later)) => {
                first_name != later_name
                    || first.len() != later.len()
                    || first
                        .iter()
                        .zip(later.iter())
                        .any(|(&first, &later)| self.layout_differs(first, later))
            }
            _ => match (self.kind_of_value(first), self.kind_of_value(later)) {
                (Some(first), Some(later)) => first != later,
                _ => false,
            },
        }
    }

    /// Which kind of ry value the typed core holds for `ty`, an already peeled type: a number
    /// names one kind per scalar type, and every table value is one kind.
    fn kind_of_value(&self, ty: Ty<'db>) -> Option<u8> {
        let db = self.judged.db;
        if let Some(scalar) = self.storage(ty) {
            return Some(scalar);
        }
        Some(match ty.kind(db) {
            TyKind::Null => 32,
            TyKind::NativeRecord(_, _) => 33,
            TyKind::NativeTuple(_) => 34,
            TyKind::Tag(..) | TyKind::OtherTags => 35,
            TyKind::Union(_) if !self.is_mixed_union(ty) => 35,
            TyKind::Frame(kind, _, _) if kind.class().is_none() => 36,
            TyKind::Named(name, _) => match name.text(db) {
                "Expr" => 36,
                "List" => 37,
                "Dict" => 38,
                "Set" => 39,
                _ => return None,
            },
            _ => return None,
        })
    }

    /// How the typed core stores a scalar or a list of scalars. `F64` and `double` share a
    /// representation, as do `Bool` and `logical`, and `Str` and `character`.
    fn storage(&self, ty: Ty<'db>) -> Option<u8> {
        let db = self.judged.db;
        let scalar = |ty: Ty<'db>| match ty.kind(db) {
            TyKind::Native(NativeScalar::F64) | TyKind::Scalar(Atomic::Double) => Some(1),
            TyKind::Native(NativeScalar::I64) => Some(2),
            TyKind::Scalar(Atomic::Integer) => Some(3),
            TyKind::Native(NativeScalar::Bool) | TyKind::Scalar(Atomic::Logical) => Some(4),
            TyKind::Native(NativeScalar::Str) | TyKind::Scalar(Atomic::Character) => Some(5),
            _ => None,
        };
        scalar(ty).or_else(|| array_element(db, ty).and_then(scalar).map(|code| code + 8))
    }

    /// Walks the body of a local function or of a function literal a standard library function
    /// takes, which native code runs in place with the names in scope where it was written.
    fn function_body(&mut self, function: ExprId) {
        let module = self.judged.module;
        let ExpressionKind::Function { parameters, body } = *module.kind(function) else {
            return;
        };
        let mark = self.scope.len();
        for parameter in &module[parameters] {
            if parameter.name == "..." {
                self.block_at(
                    parameter.range,
                    NativeBlockerKind::Uncompiled(Uncompiled::Dots),
                );
                continue;
            }
            self.scope.push((parameter.name, Slot::Parameter(None)));
        }
        let control_depth = std::mem::replace(&mut self.control_depth, 0);
        let frame_mark = std::mem::replace(&mut self.frame_mark, mark);
        let r_arguments = self.r_arguments.take();
        let hoist = self.hoist.take();
        self.nested += 1;
        self.walk(body, Use::Value);
        self.nested -= 1;
        self.hoist = hoist;
        self.r_arguments = r_arguments;
        self.control_depth = control_depth;
        self.frame_mark = frame_mark;
        self.scope.truncate(mark);
    }

    fn callee(&self, callee: ExprId) -> Callee {
        let module = self.judged.module;
        match *module.kind(callee) {
            ExpressionKind::NameRef(name) => {
                match self.lookup(name) {
                    Some(Slot::Function(function)) => return Callee::Local(function),
                    Some(Slot::FunctionParameter) => return Callee::FunctionParameter,
                    Some(_) => return Callee::Variable(name),
                    None => {}
                }
                let enclosing = self
                    .judged
                    .naming
                    .resolutions
                    .get(&callee)
                    .and_then(|binding| self.judged.naming.bindings.get(binding))
                    .is_some_and(|binding| binding.kind != BindingKind::TopLevel);
                if enclosing || self.locals.contains(&name) {
                    return Callee::Variable(name);
                }
                self.global_callee(callee, name)
            }
            ExpressionKind::Namespace {
                package: Some(package),
                name: Some(name),
                ..
            } => {
                if package == "std" || package == "ry" {
                    library_callee(name)
                } else {
                    Callee::R(name, ROrigin::Library)
                }
            }
            _ => Callee::Computed,
        }
    }

    fn global_callee(&self, callee: ExprId, name: Symbol) -> Callee {
        let db = self.judged.db;
        let project = self
            .judged
            .globals
            .is_some_and(|globals| globals.defined_in_project(name, true));
        if !project {
            return library_callee(name);
        }
        let ty = self.ty(callee).or_else(|| {
            self.judged
                .globals
                .and_then(|globals| globals.scheme(name, true))
                .filter(|scheme| !has_r_bound(scheme))
                .map(|scheme| scheme.body)
        });
        match ty.map(|ty| ty.kind(db)) {
            Some(TyKind::Function(function)) if !function.inferred => {
                if is_native_signature(db, self.judged.definitions, function, 0) {
                    Callee::Native(name)
                } else {
                    Callee::R(name, ROrigin::Boundary)
                }
            }
            _ => Callee::R(name, ROrigin::Unannotated),
        }
    }

    fn call(
        &mut self,
        id: ExprId,
        callee: ExprId,
        arguments: &[(Option<Symbol>, ExprId)],
        usage: Use,
    ) {
        let module = self.judged.module;
        let named = arguments.iter().any(|(name, _)| name.is_some());
        let callee_kind = self.callee(callee);
        if let ExpressionKind::Namespace { .. } = *module.kind(callee)
            && matches!(
                callee_kind,
                Callee::Conversion | Callee::ToR(_) | Callee::Std(_) | Callee::Table
            )
        {
            self.uncompiled(
                callee,
                Uncompiled::Advised(
                    "a standard library function called with a `ry::` prefix",
                    "Write the name without the prefix",
                ),
            );
        }
        match callee_kind {
            Callee::Return => {
                if arguments.len() > 1 {
                    self.uncompiled(id, Uncompiled::Construct("`return` with several values"));
                }
                for &(_, value) in arguments {
                    self.walk(value, Use::Value);
                }
            }
            // Native code hands a `stop` it cannot raise itself to the interpreter, which
            // builds the message, so its arguments may hold anything.
            Callee::Stop => {}
            Callee::Local(function) => {
                let name = match *module.kind(callee) {
                    ExpressionKind::NameRef(name) => name,
                    _ => Symbol::default(),
                };
                // The call computes the defaults it leaves out with the names in scope here.
                if let ExpressionKind::Function { parameters, body } = *module.kind(function) {
                    let parameters = &module[parameters];
                    match matched_formals(parameters, arguments) {
                        Some(supplied) => {
                            let mark = self.scope.len();
                            self.scope.extend(
                                parameters
                                    .iter()
                                    .map(|parameter| (parameter.name, Slot::Parameter(None))),
                            );
                            self.defaults(parameters, body, supplied);
                            self.scope.truncate(mark);
                        }
                        None => self.uncompiled(id, Uncompiled::ArgumentCount(name)),
                    }
                }
                for &(_, value) in arguments {
                    self.function_argument(value);
                }
                self.local_function(function);
            }
            Callee::Variable(name) => {
                let gap = if self.lookup(name).is_none() && self.hidden.contains(&name) {
                    Uncompiled::LocalFunctionInBranch(name)
                } else {
                    Uncompiled::CallsVariable(name)
                };
                self.uncompiled(callee, gap);
                for &(_, value) in arguments {
                    self.walk(value, Use::Value);
                }
            }
            Callee::FunctionParameter => {
                if named {
                    self.uncompiled(
                        id,
                        Uncompiled::Advised(
                            "a call of a function parameter with named arguments",
                            "Pass the arguments by position",
                        ),
                    );
                }
                for &(_, value) in arguments {
                    self.function_argument(value);
                }
            }
            Callee::Conversion => {
                let vector = arguments
                    .iter()
                    .position(|(name, _)| name.is_some_and(|name| name.as_str() == "vector"))
                    .or_else(|| arguments.iter().position(|(name, _)| name.is_none()));
                for (position, &(_, value)) in arguments.iter().enumerate() {
                    // `List.from_r_or`'s `default` is ordinary ry code.
                    if Some(position) != vector {
                        self.walk(value, Use::Value);
                        continue;
                    }
                    // The conversion gives the inner call's value a ry type, and the call still
                    // runs while R's lazy arguments are being computed.
                    if let Some(outer) = self.r_arguments
                        && let Some(inner) = self.r_function(value)
                    {
                        let (conversion, integer) = self.nested_conversion(id);
                        let fix = self.hoist_fix(conversion, true).filter(|_| !integer);
                        self.block_with_fix(
                            id,
                            NativeBlockerKind::NestedRCall {
                                outer,
                                inner,
                                conversion,
                                integer,
                            },
                            fix,
                        );
                    }
                    self.walk(value, Use::Converted);
                }
            }
            Callee::ToR(name) => {
                if !is_r_argument(usage) {
                    self.block(id, NativeBlockerKind::ToROutsideCall(name));
                }
                for &(_, value) in arguments {
                    self.walk(value, Use::Value);
                }
            }
            Callee::Std(name) => {
                if !COMPILED_STD.contains(&name.as_str()) {
                    self.uncompiled(callee, Uncompiled::StdFunction(name));
                } else if named && !STD_NAMED_ARGUMENTS.contains(&name.as_str()) {
                    self.uncompiled(id, Uncompiled::NamedArguments(name));
                }
                for &(_, value) in arguments {
                    self.function_argument(value);
                }
                if let Some(uncompiled) = self.ty(id).and_then(|ty| self.keyed_blocker(ty)) {
                    self.keyed_layout(module.range(id), uncompiled);
                }
            }
            Callee::Table => {
                let name = match *module.kind(callee) {
                    ExpressionKind::NameRef(name)
                    | ExpressionKind::Namespace {
                        name: Some(name), ..
                    } => name,
                    _ => Symbol::default(),
                };
                let outer = self.r_arguments.replace(name);
                for &(_, value) in arguments {
                    self.r_argument(name, value);
                    self.walk(value, Use::TableArgument);
                }
                self.r_arguments = outer;
            }
            // The type check matches the arguments to the formals, and native code matches them
            // as R does, left-out formals taking their defaults.
            Callee::Native(name) => {
                for &(_, value) in arguments {
                    if self.function_argument(value)
                        && self.own == Some(name)
                        && !matches!(*module.kind(value), ExpressionKind::NameRef(forwarded)
                            if matches!(self.lookup(forwarded), Some(Slot::FunctionParameter)))
                        && self.wraps_function_parameter(value)
                    {
                        self.uncompiled(value, Uncompiled::RecursiveFunctionArgument(name));
                    }
                }
            }
            Callee::R(name, origin) => self.r_call(id, name, origin, arguments, usage),
            Callee::Computed => {
                self.uncompiled(callee, Uncompiled::ComputedCall);
                for &(_, value) in arguments {
                    self.walk(value, Use::Value);
                }
            }
        }
    }

    /// An argument of a function native code calls. A function passed there is lowered into
    /// the call, so a function literal, a local function, a parameter, a native project function
    /// or a compiled standard library function may stand there. Returns whether the argument is
    /// a function.
    fn function_argument(&mut self, value: ExprId) -> bool {
        let module = self.judged.module;
        let db = self.judged.db;
        match *module.kind(value) {
            ExpressionKind::Function { .. } => {
                self.function_body(value);
                true
            }
            ExpressionKind::NameRef(name)
                if self
                    .ty(value)
                    .is_some_and(|ty| matches!(self.peel(ty).kind(db), TyKind::Function(_))) =>
            {
                let compiles = match self.lookup(name) {
                    Some(Slot::Function(function)) => {
                        self.local_function(function);
                        true
                    }
                    Some(Slot::FunctionParameter) => true,
                    Some(_) => false,
                    None => match self.global_callee(value, name) {
                        Callee::Native(..) => true,
                        Callee::Std(name) => COMPILED_STD.contains(&name.as_str()),
                        _ => false,
                    },
                };
                if !compiles {
                    self.uncompiled(value, Uncompiled::MappedFunction);
                }
                true
            }
            _ => {
                self.walk(value, Use::Value);
                false
            }
        }
    }

    /// Whether a function passed to a recursive call reads a function parameter, itself or
    /// through a local function it reads. Native code compiles the recursion once per function
    /// it passes, so a function that wraps the function parameter would need a copy per level.
    fn wraps_function_parameter(&self, value: ExprId) -> bool {
        let module = self.judged.module;
        let mut pending: SmallVec<[ExprId; 16]> = SmallVec::from_slice(&[value]);
        let mut seen: SmallVec<[ExprId; 4]> = SmallVec::new();
        while let Some(id) = pending.pop() {
            match *module.kind(id) {
                ExpressionKind::NameRef(name) => match self.lookup(name) {
                    Some(Slot::FunctionParameter) => return true,
                    Some(Slot::Function(function)) if !seen.contains(&function) => {
                        seen.push(function);
                        pending.push(function);
                    }
                    _ => {}
                },
                _ => module.for_each_child(id, |child| pending.push(child)),
            }
        }
        false
    }

    fn r_call(
        &mut self,
        id: ExprId,
        name: Symbol,
        origin: ROrigin,
        arguments: &[(Option<Symbol>, ExprId)],
        usage: Use,
    ) {
        if origin == ROrigin::Library
            && uses_its_frame(name.as_str())
            && declaring_namespace(self.judged.db, name.as_str()) == Some("base")
        {
            self.uncompiled(id, Uncompiled::FrameCall(name));
        }
        // R evaluates a call made only of R code where it stands among another call's arguments,
        // lazily, as the interpreter would, so its value needs no ry type.
        if !(is_r_argument(usage) && self.is_r_code(id)) {
            self.r_position(id, name, origin, usage);
        }
        let outer = self.r_arguments.replace(name);
        let hoist = self.hoist;
        if defers_arguments(name.as_str()) {
            self.hoist = None;
        }
        for &(_, value) in arguments {
            self.r_argument(name, value);
            self.walk(value, Use::Argument(name));
        }
        self.hoist = hoist;
        self.r_arguments = outer;
    }

    /// Reports a ry value R does not take as it is: an `I64` crosses through `Num.to_r`, a list
    /// of them through `List.to_r`, and a `Dict` or a `Set` does not cross.
    fn r_argument(&mut self, function: Symbol, value: ExprId) {
        let db = self.judged.db;
        let Some(ty) = self.ty(value).map(|ty| self.peel(ty)) else {
            return;
        };
        let is_i64 = |ty: Ty<'db>| matches!(ty.kind(db), TyKind::Native(NativeScalar::I64));
        let crossing = match ty.kind(db) {
            TyKind::Native(NativeScalar::I64) => Crossing::I64,
            TyKind::Named(name, _) if name.text(db) == "Set" && is_ry_type(db, "Set") => {
                Crossing::Set
            }
            _ if dict_entries(db, ty).is_some() => Crossing::Dict,
            _ if array_element(db, ty).is_some_and(is_i64) => Crossing::I64List,
            _ => return,
        };
        self.uncompiled(value, Uncompiled::RArgument(function, crossing));
    }

    /// Whether `id` is R code that R can evaluate in the function's enclosing environment just as
    /// in its frame: a literal, a variable from outside the function, or a call of a base R
    /// function on such code. The typed core passes it as a promise, so it runs when R asks for it.
    fn is_r_code(&self, id: ExprId) -> bool {
        let module = self.judged.module;
        match *module.kind(id) {
            ExpressionKind::Literal(_) => true,
            ExpressionKind::Paren(inner) => self.is_r_code(inner),
            ExpressionKind::NameRef(name) => {
                self.lookup(name).is_none() && !self.locals.contains(&name)
            }
            ExpressionKind::Call { callee, arguments } => {
                let base = match *module.kind(callee) {
                    ExpressionKind::NameRef(name) => {
                        matches!(self.callee(callee), Callee::R(_, ROrigin::Library))
                            && self.is_base_function(name)
                    }
                    ExpressionKind::Namespace {
                        package: Some(package),
                        name: Some(name),
                        ..
                    } => package == "base" && !reads_its_frame(name.as_str()),
                    _ => false,
                };
                base && named_values(module, &module[arguments]).all(|(_, value)| {
                    matches!(*module.kind(value), ExpressionKind::Missing) || self.is_r_code(value)
                })
            }
            ExpressionKind::Binary {
                operator: BinaryOperator::Special,
                special_name: Some(name),
                lhs,
                rhs,
            } => self.is_base_function(name) && self.is_r_code(lhs) && self.is_r_code(rhs),
            _ => false,
        }
    }

    /// Whether `name` is a function of base R that does not look at the frame it is called from.
    /// A function of another package may, as `glue::glue` does to find the variables it
    /// interpolates, and from a promise it would see the function's enclosing environment.
    fn is_base_function(&self, name: Symbol) -> bool {
        !reads_its_frame(name.as_str())
            && declaring_namespace(self.judged.db, name.as_str()) == Some("base")
    }

    /// The R function `id` calls, when `id` is a call of one.
    fn r_function(&self, id: ExprId) -> Option<Symbol> {
        let module = self.judged.module;
        let callee = match *module.kind(id) {
            ExpressionKind::Paren(inner) => return self.r_function(inner),
            ExpressionKind::Call { callee, .. } => callee,
            ExpressionKind::Binary {
                operator: BinaryOperator::Pipe,
                rhs,
                ..
            } => match *module.kind(rhs) {
                ExpressionKind::Call { callee, .. } => callee,
                _ => return None,
            },
            _ => return None,
        };
        match self.callee(callee) {
            Callee::R(name, _) => Some(name),
            _ => None,
        }
    }

    /// Reports an R call whose value is used where only a ry value may stand.
    fn r_position(&mut self, id: ExprId, function: Symbol, origin: ROrigin, usage: Use) {
        let conversion = self.ty(id).and_then(|ty| self.conversion(ty));
        match usage {
            Use::Unused | Use::Converted => {}
            Use::Argument(outer) => {
                let (conversion, integer) = self.nested_conversion(id);
                let fix = self.hoist_fix(conversion, false).filter(|_| !integer);
                self.block_with_fix(
                    id,
                    NativeBlockerKind::NestedRCall {
                        outer,
                        inner: function,
                        conversion,
                        integer,
                    },
                    fix,
                );
            }
            Use::Value | Use::TableArgument => self.block_with_fix(
                id,
                NativeBlockerKind::RValue {
                    function,
                    origin,
                    conversion,
                },
                conversion.map(NativeFix::Convert),
            ),
        }
    }

    fn column_read(&mut self, id: ExprId, usage: Use) {
        if matches!(usage, Use::Unused | Use::Converted) {
            return;
        }
        let fix = self
            .ty(id)
            .is_some_and(|ty| {
                matches!(self.peel(ty).kind(self.judged.db), TyKind::Vector(_))
                    && self.converts_plainly(ty, matches!(usage, Use::Argument(_)))
            })
            .then_some(NativeFix::Convert(Conversion::List));
        self.block_with_fix(id, NativeBlockerKind::ColumnRead, fix);
    }

    /// The conversion that gives the value of `id`, an R call among another R call's arguments,
    /// a ry type, and whether that type is an integer, which R takes back only through `to_r`.
    fn nested_conversion(&self, id: ExprId) -> (Option<Conversion>, bool) {
        let Some(ty) = self.ty(id) else {
            return (None, false);
        };
        let conversion = self.conversion(ty);
        (
            conversion,
            conversion.is_some() && !self.converts_plainly(ty, true),
        )
    }

    /// Whether `from_r` takes the R value of type `ty` to a plain ry number, string, flag or list
    /// of them, which `into_r` requires R to take back as it is: R's integers arrive as `I64`,
    /// which crosses back only through `Num.to_r`.
    fn converts_plainly(&self, ty: Ty<'db>, into_r: bool) -> bool {
        let db = self.judged.db;
        let scalar = |ty: Ty<'db>| match ty.kind(db) {
            TyKind::Scalar(Atomic::Double | Atomic::Character | Atomic::Logical)
            | TyKind::Native(NativeScalar::F64 | NativeScalar::Str | NativeScalar::Bool) => true,
            TyKind::Scalar(Atomic::Integer) | TyKind::Native(NativeScalar::I64) => !into_r,
            _ => false,
        };
        let ty = self.peel(ty);
        match ty.kind(db) {
            TyKind::Vector(item) | TyKind::NamedVector(item) => scalar(self.peel(*item)),
            _ => scalar(ty) || array_element(db, ty).is_some_and(|item| scalar(self.peel(item))),
        }
    }

    /// The fix that computes an R call among another R call's arguments into a variable first,
    /// which needs a conversion to give the variable a ry type and a statement to stand before.
    /// `converted` says the blocked construct is that conversion already.
    fn hoist_fix(&self, conversion: Option<Conversion>, converted: bool) -> Option<NativeFix> {
        let conversion = conversion?;
        Some(NativeFix::Hoist {
            before: self.judged.module.range(self.hoist?),
            convert: (!converted).then_some(conversion),
        })
    }
}

fn named_values<'a>(
    module: &'a Module,
    arguments: &'a [Argument],
) -> impl Iterator<Item = (Option<Symbol>, ExprId)> + 'a {
    arguments
        .iter()
        .filter_map(|argument| Some((argument.name.map(|name| module.name(name)), argument.value?)))
}

/// Whether the R function `name` may evaluate an argument late, more than once, not at all, in a
/// condition handler or in another environment, so computing the argument before the call could
/// change what the program does.
fn defers_arguments(name: &str) -> bool {
    const DEFERRING: [&str; 21] = [
        "Recall",
        "bquote",
        "capture.output",
        "delayedAssign",
        "eval",
        "evalq",
        "expression",
        "ifelse",
        "invokeRestart",
        "local",
        "missing",
        "on.exit",
        "quote",
        "replicate",
        "stopifnot",
        "substitute",
        "switch",
        "system.time",
        "try",
        "tryCatch",
        "tryCatchLog",
    ];
    DEFERRING.contains(&name)
        || ["with", "suppress", "expect_", "local_", "test_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// The mask of the parameters a call passing `arguments` supplies, when it matches them to
/// `parameters` as R does (by exact name, then by a prefix that fits one parameter, then by
/// position) and leaves out only parameters with a default, which is how native code calls a
/// local function.
fn matched_formals(
    parameters: &[Parameter],
    arguments: &[(Option<Symbol>, ExprId)],
) -> Option<u64> {
    if parameters.iter().any(|parameter| parameter.name == "...") {
        return Some(u64::MAX);
    }
    if parameters.len() > 64 {
        return None;
    }
    let bit = |index: usize| 1u64 << index;
    let mut supplied = 0u64;
    let mut partial: SmallVec<[Symbol; 4]> = SmallVec::new();
    for (name, _) in arguments {
        let Some(name) = name else { continue };
        match parameters
            .iter()
            .position(|parameter| parameter.name == *name)
        {
            Some(index) if supplied & bit(index) == 0 => supplied |= bit(index),
            Some(_) => return None,
            None => partial.push(*name),
        }
    }
    let exact = supplied;
    for name in partial {
        let mut fits = (0..parameters.len()).filter(|&index| {
            exact & bit(index) == 0 && parameters[index].name.as_str().starts_with(name.as_str())
        });
        match (fits.next(), fits.next()) {
            (Some(index), None) if supplied & bit(index) == 0 => supplied |= bit(index),
            _ => return None,
        }
    }
    let mut open = (0..parameters.len()).filter(|&index| supplied & bit(index) == 0);
    let mut filled = supplied;
    for (name, _) in arguments {
        if name.is_none() {
            filled |= bit(open.next()?);
        }
    }
    parameters
        .iter()
        .enumerate()
        .all(|(index, parameter)| filled & bit(index) != 0 || parameter.default.is_some())
        .then_some(filled)
}

/// What a name of R, of a package or of ry's own library calls.
fn library_callee(name: Symbol) -> Callee {
    let text = name.as_str();
    match text {
        "return" => return Callee::Return,
        "stop" => return Callee::Stop,
        _ => {}
    }
    if !is_native_function(text) && text != "DataFrame" && text != "as.DataFrame" {
        return Callee::R(name, ROrigin::Library);
    }
    if FROM_R.contains(&text) {
        Callee::Conversion
    } else if TO_R.contains(&text) {
        Callee::ToR(name)
    } else if gives_table_value(text) {
        Callee::Table
    } else if TABLE_R_CALLS.contains(&text) {
        Callee::R(name, ROrigin::Table)
    } else if text.starts_with(|first: char| first.is_ascii_uppercase()) {
        Callee::Std(name)
    } else {
        Callee::R(name, ROrigin::Library)
    }
}

/// Whether `pattern` matches every value, so it tests nothing.
fn irrefutable(module: &Module, pattern: PatId) -> bool {
    match module.pattern(pattern).kind {
        PatternKind::Missing | PatternKind::Wildcard | PatternKind::Bind(_) => true,
        PatternKind::As { pattern, .. } => irrefutable(module, pattern),
        _ => false,
    }
}

/// Whether `id` assigns a name, has a `for` loop or a `match`, which a `match` guard may not
/// for the typed core, because the interpreter runs a guard in a scratch environment.
fn writes(module: &Module, id: ExprId) -> bool {
    if matches!(
        *module.kind(id),
        ExpressionKind::Assign { .. } | ExpressionKind::For { .. } | ExpressionKind::Match { .. }
    ) {
        return true;
    }
    let mut found = false;
    module.for_each_child(id, |child| {
        found = found || writes(module, child);
    });
    found
}

/// Whether `id` sets the local `name`, by an assignment (of a part of it, as in `xs[[i]] <- v`,
/// included), as a `for` loop's variable or in a `match` pattern.
fn assigns_name(module: &Module, id: ExprId, name: Symbol) -> bool {
    let sets = match *module.kind(id) {
        ExpressionKind::Assign { mut target, .. } => loop {
            match *module.kind(target) {
                ExpressionKind::NameRef(target) => break target == name,
                ExpressionKind::Index { target: base, .. }
                | ExpressionKind::Field { target: base, .. } => target = base,
                ExpressionKind::Call { arguments, .. } => match module[arguments].first() {
                    Some(&Argument {
                        value: Some(base), ..
                    }) => target = base,
                    _ => break false,
                },
                _ => break false,
            }
        },
        ExpressionKind::Match { arms, .. } => module[arms].iter().any(|arm| {
            module[arm.bindings]
                .iter()
                .any(|&bound| matches!(*module.kind(bound), ExpressionKind::NameRef(bound) if bound == name))
        }),
        ExpressionKind::For {
            variable: Some(variable),
            ..
        } => module.name(variable) == name,
        _ => false,
    };
    let mut found = sets;
    module.for_each_child(id, |child| {
        found = found || assigns_name(module, child, name);
    });
    found
}

/// The first expression found that gives a function's value visibly, and the first that gives
/// it invisibly ([`Judge::shown`]).
#[derive(Default, Clone, Copy)]
struct Shown {
    visible: Option<ExprId>,
    invisible: Option<ExprId>,
}

/// How many local function bodies a visibility walk follows calls through.
const MAX_SHOWN_DEPTH: usize = 8;

struct ShownWalk<'a> {
    module: &'a Module,
    /// The local functions in view, by name: the literal each assignment in a body walked binds.
    locals: Vec<(Symbol, ExprId)>,
    /// The bodies being walked. A call back into one adds nothing its other paths do not.
    seen: Vec<ExprId>,
}

impl ShownWalk<'_> {
    fn body(&mut self, body: ExprId, shown: &mut Shown) {
        if self.seen.contains(&body) || self.seen.len() >= MAX_SHOWN_DEPTH {
            return;
        }
        self.seen.push(body);
        let held = self.locals.len();
        self.collect_locals(body);
        self.tail(body, shown);
        self.returns(body, shown);
        self.locals.truncate(held);
        self.seen.pop();
    }

    fn tail(&mut self, id: ExprId, shown: &mut Shown) {
        let module = self.module;
        match *module.kind(id) {
            // A loop that only a `return` leaves gives no value of its own.
            ExpressionKind::Repeat { body } if !breaks(module, body) => {}
            ExpressionKind::While { condition, body }
                if matches!(
                    module.kind(condition),
                    ExpressionKind::Literal(LiteralKind::Logical(true))
                ) && !breaks(module, body) => {}
            ExpressionKind::Assign { .. }
            | ExpressionKind::For { .. }
            | ExpressionKind::While { .. }
            | ExpressionKind::Repeat { .. } => {
                shown.invisible.get_or_insert(id);
            }
            ExpressionKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.tail(then_branch, shown);
                match else_branch {
                    Some(else_branch) => self.tail(else_branch, shown),
                    None => {
                        shown.invisible.get_or_insert(id);
                    }
                }
            }
            ExpressionKind::Block { statements } => match module[statements].last() {
                Some(&last) => self.tail(last, shown),
                None => {
                    shown.visible.get_or_insert(id);
                }
            },
            ExpressionKind::Match { arms, .. } => {
                for arm in &module[arms] {
                    self.tail(arm.body, shown);
                }
            }
            ExpressionKind::Call { callee, arguments } => {
                let ExpressionKind::NameRef(name) = *module.kind(callee) else {
                    shown.visible.get_or_insert(id);
                    return;
                };
                if name == "invisible" || name == "List.for_each" {
                    shown.invisible.get_or_insert(id);
                } else if name == "stop" {
                    // A path that stops gives no value.
                } else if name == "return" {
                    match module[arguments]
                        .first()
                        .and_then(|argument| argument.value)
                    {
                        Some(value) => self.tail(value, shown),
                        None => {
                            shown.visible.get_or_insert(id);
                        }
                    }
                } else if let Some(literals) = self.local(name) {
                    for literal in literals {
                        if let ExpressionKind::Function { body, .. } = *module.kind(literal) {
                            self.body(body, shown);
                        }
                    }
                } else {
                    shown.visible.get_or_insert(id);
                }
            }
            ExpressionKind::Break | ExpressionKind::Next => {}
            _ => {
                shown.visible.get_or_insert(id);
            }
        }
    }

    fn local(&self, name: Symbol) -> Option<Vec<ExprId>> {
        let literals: Vec<ExprId> = self
            .locals
            .iter()
            .filter(|&&(local, _)| local == name)
            .map(|&(_, literal)| literal)
            .collect();
        (!literals.is_empty()).then_some(literals)
    }

    /// Records what each `return` and `?` outside the functions `id` defines gives.
    fn returns(&mut self, id: ExprId, shown: &mut Shown) {
        let module = self.module;
        match *module.kind(id) {
            ExpressionKind::Function { .. } => return,
            ExpressionKind::Try(_) => {
                shown.visible.get_or_insert(id);
            }
            ExpressionKind::Call { callee, .. } if matches!(*module.kind(callee), ExpressionKind::NameRef(name) if name == "return") =>
            {
                self.tail(id, shown);
            }
            _ => {}
        }
        module.for_each_child(id, |child| self.returns(child, shown));
    }

    /// Records each local function `id` binds by assignment, outside the functions it defines.
    fn collect_locals(&mut self, id: ExprId) {
        let module = self.module;
        match *module.kind(id) {
            ExpressionKind::Function { .. } => return,
            ExpressionKind::Assign { target, value, .. } => {
                if let (ExpressionKind::NameRef(name), ExpressionKind::Function { .. }) =
                    (*module.kind(target), *module.kind(value))
                {
                    self.locals.push((name, value));
                    return;
                }
            }
            _ => {}
        }
        module.for_each_child(id, |child| self.collect_locals(child));
    }
}

/// Whether a `break` in `id` leaves the loop `id` is the body of: one outside the loops and
/// functions `id` holds.
fn breaks(module: &Module, id: ExprId) -> bool {
    match module.kind(id) {
        ExpressionKind::Break => true,
        ExpressionKind::Function { .. }
        | ExpressionKind::For { .. }
        | ExpressionKind::While { .. }
        | ExpressionKind::Repeat { .. } => false,
        _ => {
            let mut found = false;
            module.for_each_child(id, |child| found = found || breaks(module, child));
            found
        }
    }
}
