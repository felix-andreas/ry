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
        Db, TypeDefinitions,
        check::GlobalEnv,
        dense::{Bitset, Column},
        hir::{
            Argument, AssignSpelling, BinaryOperator, ExprId, ExpressionKind, LiteralKind, Module,
            PatId, PatternKind, Symbol, UnaryOperator,
        },
        naming::{BindingKind, ItemNaming},
        stubs::{declaring_namespace, is_native_function, is_ry_type},
        types::{
            Atomic, Constraint, FunctionType, Name, NativeScalar, Ty, TyKind, TypeScheme,
            array_element, dict_entries,
        },
    },
    rustc_hash::FxHashSet,
    syntax::{
        TextRange, TextSize,
        annotation::{Paired, pair_formals},
        native_values::reads_its_frame,
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
    FunctionParameter(Symbol),
    InvisibleResult,
    SuperAssignment,
    AssignmentTarget,
    LocalFunctionInBranch(Symbol),
    ReturnInLocalFunction,
    FunctionValue,
    CallsVariable(Symbol),
    NamedArguments(Symbol),
    ArgumentCount(Symbol),
    StdFunction(Symbol),
    MappedFunction,
    DictEntries,
    MatchGuard,
    MatchPattern,
    MatchScrutinee,
    TableOperand,
    BranchOnlyName(Symbol),
    TypeChange(Symbol),
    TagReassigned(Symbol),
    BranchTypes,
    GlobalRead(Symbol),
    EnclosingRead(Symbol),
    ComputedCall,
    Try,
    AliasedSignature,
    IfWithoutElse,
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
        }
    }

    /// An R value in a native function breaks the rule the function is written under, which is
    /// an error. A construct the typed core does not compile yet is the compiler's gap, not the
    /// program's, so it is a warning that says where native code ends.
    pub fn is_error(&self) -> bool {
        !matches!(self, NativeBlockerKind::Uncompiled(_))
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
            Uncompiled::FunctionParameter(name) => format!(
                "`{name}` takes a function, and a function passed as a value {YET}. Define the \
                 function inside this one, or give a parameter an R type so this function is a \
                 boundary function that the interpreter runs"
            ),
            Uncompiled::InvisibleResult => format!(
                "the function ends in an expression whose value R returns invisibly, which {YET}. \
                 End the function with the value itself, or with `NULL`"
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
            Uncompiled::ReturnInLocalFunction => format!(
                "`return` inside a local function {YET}: native code runs a local \
                 function in place. End the function with the value instead"
            ),
            Uncompiled::FunctionValue => format!(
                "a function used as a value {YET}. Native code runs a function literal only where \
                 it is called, or where a standard library function such as `List.map` takes it"
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
                "this call of `{function}` does not pass every parameter, which {YET}. Pass each \
                 argument by position"
            ),
            Uncompiled::StdFunction(name) => format!(
                "`{name}` {YET}. Write the step with a loop or another standard library \
                 function"
            ),
            Uncompiled::MappedFunction => format!(
                "a function passed by name {YET} unless it is a typed function of the project or \
                 a local function. Wrap it in a function literal, as in \
                 `function(x: I64) -> F64 Num.to_f64(x)`"
            ),
            Uncompiled::DictEntries => format!(
                "a `Dict` whose keys or values are not `I64`, `F64` or `Str` {YET}. Keep the \
                 values in a `List` and store their positions in the dictionary"
            ),
            Uncompiled::MatchGuard => format!(
                "a `match` arm with an `if` guard {YET}. Test the condition inside the arm's body"
            ),
            Uncompiled::MatchPattern => format!(
                "this `match` pattern {YET}. Native code matches a tag, a record or a tuple whose \
                 fields are names or `_`. Bind the field to a name and match it in the arm's body"
            ),
            Uncompiled::MatchScrutinee => format!(
                "a `match` on a value that is not a tag, a record or a tuple {YET}. Compare the \
                 value with `if` and `==`"
            ),
            Uncompiled::TableOperand => format!(
                "an operator applied directly to a table expression that a local function, a \
                 function whose result type is an alias, an `if` or a `match` gives {YET}. \
                 Assign the expression to a variable first, and apply the operator to the variable"
            ),
            Uncompiled::BranchOnlyName(name) => format!(
                "`{name}` is read before this function sets it, or after branches that set it to \
                 values of different types, which {YET}. Give `{name}` one type, and a value \
                 before the branch or loop"
            ),
            Uncompiled::TypeChange(name) => format!(
                "`{name}` holds values of different types, which {YET}: a native variable keeps \
                 one type. Use a new name for the second value"
            ),
            Uncompiled::TagReassigned(name) => format!(
                "`{name}` holds a tag and is assigned again, which {YET}: native code fixes the \
                 tags a variable can hold where it is first set. Use a new name for each value, \
                 or compute the final value in one expression"
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
            Uncompiled::Try => format!(
                "`try` {YET}. Match the result instead, and return the error from its arm: \
                 `match (result) {{ :Ok(value) => ..., :Err(error) => :Err(error) }}`"
            ),
            Uncompiled::AliasedSignature => format!(
                "a function whose signature is a type alias {YET}: the runtime reads the types \
                 where they are written, and an alias only names them. Write the function type \
                 out in place of the alias, as in `fn(x: I64) -> I64`, or write the types on the \
                 parameters"
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
    "Dict.contains",
    "Dict.empty",
    "Dict.get",
    "Dict.insert",
    "Dict.keys",
    "Dict.len",
    "Dict.remove",
    "Dict.to_list",
    "Dict.update",
    "Dict.values",
    "List.append",
    "List.chunks_of",
    "List.concat",
    "List.contains",
    "List.count_if",
    "List.drop_first",
    "List.drop_last",
    "List.find_first",
    "List.find_first_index",
    "List.is_empty",
    "List.join_map",
    "List.keep_errs",
    "List.keep_if",
    "List.keep_oks",
    "List.len",
    "List.map",
    "List.map2",
    "List.map3",
    "List.max",
    "List.min",
    "List.range",
    "List.repeat",
    "List.reverse",
    "List.sort_asc",
    "List.sort_desc",
    "List.sort_with",
    "List.sublist",
    "List.sum",
    "List.swap",
    "List.take_first",
    "List.take_last",
    "List.walk",
    "List.with_capacity",
    "Num.abs",
    "Num.bitwise_and",
    "Num.bitwise_not",
    "Num.bitwise_or",
    "Num.bitwise_xor",
    "Num.compare",
    "Num.exp",
    "Num.is_nan",
    "Num.max",
    "Num.min",
    "Num.pow",
    "Num.pow_int",
    "Num.round",
    "Num.shift_left_by",
    "Num.shift_right_by",
    "Num.shift_right_zf_by",
    "Num.sqrt",
    "Num.to_f64",
    "Num.to_str",
    "Num.truncate",
    "Result.with_default",
    "Set.from_list",
    "Set.len",
    "Set.to_list",
    "Str.concat",
    "Str.count_utf8_bytes",
    "Str.ends_with",
    "Str.from_utf8_lossy",
    "Str.join_with",
    "Str.repeat",
    "Str.replace_each",
    "Str.split_on",
    "Str.starts_with",
    "Str.to_f64",
    "Str.to_i64",
    "Str.to_utf8",
    "Str.with_ascii_lowercased",
    "Str.with_ascii_uppercased",
];

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
        reported_dict: false,
        control_depth: 0,
        nested: 0,
        local_functions: Vec::new(),
        r_arguments: None,
        invisible: None,
        hoist: None,
        converted_locals: Vec::new(),
    };
    judge.find(root);
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
/// `<T: numeric>`. Such a signature names an R type, so its function is a boundary function.
pub fn has_r_bound(scheme: &TypeScheme<'_>) -> bool {
    scheme.binders.iter().any(|binder| {
        matches!(
            binder.constraint(),
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
    /// A parameter that takes a function. It is reported once, where it is declared, so its uses
    /// are not reported again.
    FunctionParameter,
    /// A variable, with the type of the first value it was given and the standard library
    /// function that value came from.
    Value(Option<Ty<'db>>, Option<Symbol>),
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
    /// A top-level native function, with how many arguments a call passes it unless it takes
    /// `...`.
    Native(Symbol, Option<usize>),
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
    reported_dict: bool,
    control_depth: u32,
    /// How many local functions or function literals enclose the walk inside the judged function.
    nested: u32,
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

    /// Reports a dictionary the typed core has no layout for, once per judged function: every
    /// later use of the same dictionary would repeat the finding.
    fn dict_entries(&mut self, range: TextRange) {
        if !std::mem::replace(&mut self.reported_dict, true) {
            self.block_at(
                range,
                NativeBlockerKind::Uncompiled(Uncompiled::DictEntries),
            );
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
    /// native signature is judged, and any other is searched for ones nested in it.
    fn find(&mut self, id: ExprId) {
        let module = self.judged.module;
        if let ExpressionKind::Function { parameters, .. } = *module.kind(id)
            && let Some(signature) = self.judged.signatures.get(id)
        {
            let formals = module[parameters]
                .iter()
                .filter(|parameter| parameter.name != "...")
                .count();
            if is_native_signature(self.judged.db, self.judged.definitions, signature, formals) {
                self.judge(id, signature);
                return;
            }
        }
        module.for_each_child(id, |child| self.find(child));
    }

    fn judge(&mut self, function: ExprId, signature: &FunctionType<'db>) {
        let module = self.judged.module;
        let db = self.judged.db;
        let ExpressionKind::Function { parameters, body } = *module.kind(function) else {
            return;
        };
        self.scope.clear();
        self.reported.clear();
        self.reported_dict = false;
        self.locals.clear();
        self.hidden.clear();
        self.control_depth = 0;
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
                if matches!(self.peel(declared).kind(db), TyKind::Function(_)) {
                    self.block_at(
                        parameter.range,
                        NativeBlockerKind::Uncompiled(Uncompiled::FunctionParameter(
                            parameter.name,
                        )),
                    );
                    slot = Slot::FunctionParameter;
                } else if !self.dict_compiles(declared) {
                    self.dict_entries(parameter.range);
                }
            }
            self.scope.push((parameter.name, slot));
        }
        if let Some(invisible) = self.invisible_tail(body) {
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

    /// The expression that makes the function's result invisible, which the typed core cannot
    /// return: an assignment, a loop or an `if` without `else` in the last position.
    fn invisible_tail(&self, id: ExprId) -> Option<ExprId> {
        let module = self.judged.module;
        match *module.kind(id) {
            ExpressionKind::Assign { .. }
            | ExpressionKind::For { .. }
            | ExpressionKind::While { .. }
            | ExpressionKind::Repeat { .. } => Some(id),
            ExpressionKind::If {
                then_branch,
                else_branch,
                ..
            } => match else_branch {
                None => Some(id),
                Some(else_branch) => self
                    .invisible_tail(then_branch)
                    .or_else(|| self.invisible_tail(else_branch)),
            },
            ExpressionKind::Block { statements } => module[statements]
                .last()
                .and_then(|&last| self.invisible_tail(last)),
            ExpressionKind::Call { callee, .. } => {
                matches!(*module.kind(callee), ExpressionKind::NameRef(name) if name == "invisible")
                    .then_some(id)
            }
            _ => None,
        }
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
                    let hoist = self.hoist.replace(statement);
                    self.walk(statement, statement_use);
                    self.hoist = hoist;
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
                if branch_use == Use::Value && else_branch.is_none() && self.invisible != Some(id) {
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
                if let Some(variable) = variable {
                    let item = self
                        .ty(sequence)
                        .and_then(|ty| array_element(db, self.peel(ty)));
                    self.bind_assigned(module.name_range(variable), module.name(variable), item);
                }
                self.in_loop(body, mark);
            }
            ExpressionKind::While { condition, body } => {
                let hoist = self.hoist.take();
                self.walk(condition, Use::Value);
                self.hoist = hoist;
                self.in_loop(body, self.scope.len());
            }
            ExpressionKind::Repeat { body } => self.in_loop(body, self.scope.len()),
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
                    let hoist = self.hoist.take();
                    self.walk(rhs, Use::Value);
                    self.hoist = hoist;
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
                // Single brackets keep the items a list of `Bool` marks, which compiles.
                let by_mask = arguments
                    .first()
                    .and_then(|argument| argument.value)
                    .and_then(|position| self.ty(position))
                    .is_some_and(|ty| array_element(db, self.peel(ty)).is_some());
                if !double
                    && !by_mask
                    && target_type.is_some_and(|ty| array_element(db, ty).is_some())
                {
                    self.uncompiled(
                        id,
                        Uncompiled::Advised(
                            "a part of a list taken by position with single brackets",
                            "Read one item with `xs[[i]]`, or take a part with `List.sublist`",
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
            ExpressionKind::Try(_) => self.uncompiled(id, Uncompiled::Try),
            ExpressionKind::Match { scrutinee, arms } => {
                self.walk(scrutinee, Use::Value);
                let matched = self.ty(scrutinee).map(|ty| self.peel(ty));
                let matches_shape = matched.is_none_or(|ty| match ty.kind(db) {
                    TyKind::NativeRecord(_, _)
                    | TyKind::NativeTuple(_)
                    | TyKind::Tag(..)
                    | TyKind::OtherTags => true,
                    TyKind::Union(_) => !self.is_mixed_union(ty),
                    _ => false,
                });
                if !matches_shape {
                    self.uncompiled(scrutinee, Uncompiled::MatchScrutinee);
                }
                let branch_use = if usage == Use::Unused {
                    Use::Unused
                } else {
                    Use::Value
                };
                let mut ends = Vec::new();
                for arm in &module[arms] {
                    if matches_shape && !self.pattern_compiles(arm.pattern, true) {
                        self.block_at(
                            module.pattern(arm.pattern).range,
                            NativeBlockerKind::Uncompiled(Uncompiled::MatchPattern),
                        );
                    }
                    let mark = self.scope.len();
                    self.control_depth += 1;
                    for &binding in &module[arm.bindings] {
                        if let ExpressionKind::NameRef(name) = *module.kind(binding) {
                            self.bind_assigned(module.range(binding), name, self.ty(binding));
                        }
                    }
                    if let Some(guard) = arm.guard {
                        self.uncompiled(guard, Uncompiled::MatchGuard);
                    }
                    let hoist = self.hoist.take();
                    self.walk(arm.body, branch_use);
                    self.hoist = hoist;
                    let names = self.scope.split_off(mark);
                    if !self.diverges(arm.body) {
                        ends.push(names);
                    }
                    self.control_depth -= 1;
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
            // The typed core reads the result type as it is written, so an alias of a table
            // type tells it nothing.
            Callee::Native(name, _) => !self
                .judged
                .globals
                .and_then(|globals| globals.scheme(name, true))
                .is_some_and(|scheme| match scheme.body.kind(db) {
                    TyKind::Function(function) => matches!(
                        function.ret.kind(db),
                        TyKind::Named(written, _) if !matches!(
                            written.text(db),
                            "Expr" | "DataFrame" | "LazyFrame" | "GroupBy" | "LazyGroupBy"
                        )
                    ),
                    _ => false,
                }),
            _ => false,
        });
        if !told {
            self.uncompiled(operand, Uncompiled::TableOperand);
        }
    }

    /// Walks a loop body. After the loop, each name the body created is set only if the body
    /// ran far enough to set it.
    fn in_loop(&mut self, body: ExprId, mark: usize) {
        self.control_depth += 1;
        let hoist = self.hoist.take();
        self.walk(body, Use::Unused);
        self.hoist = hoist;
        self.control_depth -= 1;
        for (name, slot) in self.scope.split_off(mark) {
            let ty = match slot {
                Slot::Function(_) => {
                    self.hidden.insert(name);
                    continue;
                }
                Slot::Value(ty, _) | Slot::Parameter(ty) | Slot::Maybe(ty) => ty,
                Slot::FunctionParameter => None,
            };
            self.scope.push((name, Slot::Maybe(ty)));
        }
    }

    /// Binds a loop variable or a pattern's name, which the typed core assigns: a name the
    /// function already holds keeps its variable, so a value of another type, or any tag, which
    /// fixes the variables it is stored in, gives the function up.
    fn bind_assigned(&mut self, at: TextRange, name: Symbol, ty: Option<Ty<'db>>) {
        match self.lookup(name) {
            Some(Slot::Value(held, _) | Slot::Parameter(held)) => {
                if ty.is_some_and(|ty| self.holds_tag(ty)) {
                    let kind = Uncompiled::TagReassigned(name);
                    self.block_at(at, NativeBlockerKind::Uncompiled(kind));
                } else if let (Some(held), Some(ty)) = (held, ty)
                    && self.changes_type(held, ty)
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
                    Some(Slot::Value(ty, _) | Slot::Parameter(ty)) => ty,
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
            let TyKind::Named(name, _) = ty.kind(db) else {
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
            ty = definition.body;
        }
        ty
    }

    /// The typed core keys and fills a dictionary with `I64`, `F64` or `Str` only.
    fn dict_compiles(&self, ty: Ty<'db>) -> bool {
        let db = self.judged.db;
        let Some((key, value)) = dict_entries(db, ty) else {
            return true;
        };
        [key, value].into_iter().all(|entry| {
            matches!(
                entry.kind(db),
                TyKind::Native(NativeScalar::I64 | NativeScalar::F64 | NativeScalar::Str)
                    | TyKind::Scalar(Atomic::Double | Atomic::Character)
                    | TyKind::Var(_)
            )
        })
    }

    fn pattern_compiles(&self, pattern: PatId, top: bool) -> bool {
        let module = self.judged.module;
        let leaf = |pattern: PatId| {
            matches!(
                module.pattern(pattern).kind,
                PatternKind::Wildcard | PatternKind::Bind(_) | PatternKind::Missing
            )
        };
        match module.pattern(pattern).kind {
            PatternKind::Missing | PatternKind::Wildcard | PatternKind::Bind(_) => true,
            PatternKind::Tag { payload, .. } => {
                top && payload.is_none_or(|payload| self.pattern_compiles(payload, true))
            }
            PatternKind::Tuple(items) => top && module[items].iter().all(|&item| leaf(item)),
            PatternKind::Record { fields, rest } => {
                top && rest.is_none() && module[fields].iter().all(|field| leaf(field.pattern))
            }
            PatternKind::Or(alternatives) => {
                top && module[alternatives].iter().all(|&alternative| {
                    !binds(module, alternative) && self.pattern_compiles(alternative, true)
                })
            }
            PatternKind::Literal { .. } | PatternKind::List { .. } | PatternKind::As { .. } => {
                false
            }
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
                let source = match *module.kind(value) {
                    ExpressionKind::Call { callee, .. } => match self.callee(callee) {
                        Callee::Std(function) => Some(function),
                        _ => None,
                    },
                    _ => None,
                };
                let held = match self.lookup(name) {
                    Some(Slot::Value(first, first_source)) => Some((first, first_source)),
                    Some(Slot::Parameter(first)) => Some((first, None)),
                    Some(Slot::FunctionParameter) => Some((None, None)),
                    // A name set on some paths takes a new variable, so only a tag, which fixes
                    // the variables it can be stored in where it is first set, is held.
                    Some(Slot::Maybe(first)) if ty.is_some_and(|ty| self.holds_tag(ty)) => {
                        Some((first, None))
                    }
                    Some(Slot::Function(_) | Slot::Maybe(_)) | None => None,
                };
                match held {
                    Some((first, first_source)) => {
                        let same_source = source.is_some() && source == first_source;
                        if !same_source && ty.is_some_and(|later| self.holds_tag(later)) {
                            self.uncompiled(id, Uncompiled::TagReassigned(name));
                        } else if let (Some(first), Some(later)) = (first, ty)
                            && self.changes_type(first, later)
                        {
                            self.uncompiled(id, Uncompiled::TypeChange(name));
                        }
                    }
                    None => self.bind(name, Slot::Value(ty, source)),
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

    /// Whether a variable first given a value of type `first` cannot take one of type `later`:
    /// the typed core stores the two differently, or `later` holds a tag `first` does not.
    fn changes_type(&self, first: Ty<'db>, later: Ty<'db>) -> bool {
        let db = self.judged.db;
        if self.layout_differs(first, later) {
            return true;
        }
        let tags = |ty: Ty<'db>| -> Option<Vec<Name<'db>>> {
            match self.peel(ty).kind(db) {
                TyKind::Tag(name, _) => Some(vec![*name]),
                TyKind::Union(members) => members
                    .iter()
                    .map(|member| match member.kind(db) {
                        TyKind::Tag(name, _) => Some(*name),
                        _ => None,
                    })
                    .collect(),
                _ => None,
            }
        };
        match (tags(first), tags(later)) {
            (Some(first), Some(later)) => later.iter().any(|tag| !first.contains(tag)),
            _ => false,
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

    /// Whether `ty` is a tag or a union of tags.
    fn holds_tag(&self, ty: Ty<'db>) -> bool {
        let db = self.judged.db;
        match self.peel(ty).kind(db) {
            TyKind::Tag(..) => true,
            TyKind::Union(members) => members
                .iter()
                .any(|member| matches!(self.peel(*member).kind(db), TyKind::Tag(..))),
            _ => false,
        }
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
        let r_arguments = self.r_arguments.take();
        let hoist = self.hoist.take();
        self.nested += 1;
        self.walk(body, Use::Value);
        self.nested -= 1;
        self.hoist = hoist;
        self.r_arguments = r_arguments;
        self.control_depth = control_depth;
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
                    let formals = function.positional.len() + function.named.len();
                    Callee::Native(name, function.variadic.is_none().then_some(formals))
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
                if self.nested > 0 {
                    self.uncompiled(id, Uncompiled::ReturnInLocalFunction);
                }
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
                let formals = match *module.kind(function) {
                    ExpressionKind::Function { parameters, .. } => module[parameters].len(),
                    _ => arguments.len(),
                };
                let name = match *module.kind(callee) {
                    ExpressionKind::NameRef(name) => name,
                    _ => Symbol::default(),
                };
                if named {
                    self.uncompiled(id, Uncompiled::NamedArguments(name));
                } else if formals != arguments.len() {
                    self.uncompiled(id, Uncompiled::ArgumentCount(name));
                }
                for &(_, value) in arguments {
                    self.walk(value, Use::Value);
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
                for &(_, value) in arguments {
                    self.walk(value, Use::Value);
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
                } else if named {
                    self.uncompiled(id, Uncompiled::NamedArguments(name));
                }
                for &(_, value) in arguments {
                    self.std_argument(value);
                }
                if self.ty(id).is_some_and(|ty| !self.dict_compiles(ty)) {
                    self.dict_entries(module.range(id));
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
            Callee::Native(name, formals) => {
                if named {
                    self.uncompiled(id, Uncompiled::NamedArguments(name));
                } else if formals.is_some_and(|formals| formals != arguments.len()) {
                    self.uncompiled(id, Uncompiled::ArgumentCount(name));
                }
                for &(_, value) in arguments {
                    self.walk(value, Use::Value);
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

    /// An argument of a standard library function: a function it takes is run in place, so a
    /// function literal, a local function or a native project function may stand there.
    fn std_argument(&mut self, value: ExprId) {
        let module = self.judged.module;
        let db = self.judged.db;
        match *module.kind(value) {
            ExpressionKind::Function { .. } => self.function_body(value),
            ExpressionKind::NameRef(name)
                if self
                    .ty(value)
                    .is_some_and(|ty| matches!(ty.kind(db), TyKind::Function(_))) =>
            {
                let compiles = match self.lookup(name) {
                    Some(Slot::Function(function)) => {
                        self.local_function(function);
                        true
                    }
                    Some(Slot::FunctionParameter) => true,
                    Some(_) => false,
                    None => matches!(self.global_callee(value, name), Callee::Native(..)),
                };
                if !compiles {
                    self.uncompiled(value, Uncompiled::MappedFunction);
                }
            }
            _ => self.walk(value, Use::Value),
        }
    }

    fn r_call(
        &mut self,
        id: ExprId,
        name: Symbol,
        origin: ROrigin,
        arguments: &[(Option<Symbol>, ExprId)],
        usage: Use,
    ) {
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

fn binds(module: &Module, pattern: PatId) -> bool {
    match module.pattern(pattern).kind {
        PatternKind::Bind(_) | PatternKind::As { .. } => true,
        PatternKind::Missing | PatternKind::Wildcard | PatternKind::Literal { .. } => false,
        PatternKind::Tag { payload, .. } => payload.is_some_and(|payload| binds(module, payload)),
        PatternKind::Tuple(items) | PatternKind::Or(items) => {
            module[items].iter().any(|&item| binds(module, item))
        }
        PatternKind::Record { fields, rest } => {
            rest.is_some_and(|rest| rest.binding.is_some())
                || module[fields]
                    .iter()
                    .any(|field| binds(module, field.pattern))
        }
        PatternKind::List { items, rest } => {
            rest.is_some_and(|rest| rest.rest.binding.is_some())
                || module[items].iter().any(|&item| binds(module, item))
        }
    }
}
