//! GNU R's data symbols: the constant objects and ready-made symbols that native code reads
//! directly, plus the numeric constants. They are set once when the runtime starts and never
//! change afterwards, so each is a copy of the heap's own well-known object.

use crate::Global;
use heap::{
    Heap,
    object::{NA_INTEGER, NA_REAL, Sexp},
};
use std::ffi::c_int;

macro_rules! handle_globals {
    ($($name:ident),* $(,)?) => {
        $(
            #[unsafe(no_mangle)]
            pub static $name: Global<Sexp> = Global::new(Sexp::dangling());
        )*
    };
}

handle_globals!(
    R_NilValue,
    R_GlobalEnv,
    R_EmptyEnv,
    R_BaseEnv,
    R_BaseNamespace,
    R_NamespaceRegistry,
    R_UnboundValue,
    R_MissingArg,
    R_NaString,
    R_BlankString,
    R_BlankScalarString,
    R_TrueValue,
    R_FalseValue,
    R_LogicalNAValue,
    R_Srcref,
    R_CurrentExpression,
    R_MethodsNamespace,
    R_AsCharacterSymbol,
    R_AtsignSymbol,
    R_BaseSymbol,
    R_baseSymbol,
    R_BraceSymbol,
    R_Bracket2Symbol,
    R_BracketSymbol,
    R_ClassSymbol,
    R_ColonSymbol,
    R_DeviceSymbol,
    R_DimNamesSymbol,
    R_DimSymbol,
    R_DollarSymbol,
    R_DotEnvSymbol,
    R_DotsSymbol,
    R_DoubleColonSymbol,
    R_DropSymbol,
    R_EvalSymbol,
    R_ExactSymbol,
    R_FunctionSymbol,
    R_LastvalueSymbol,
    R_LevelsSymbol,
    R_ModeSymbol,
    R_NaRmSymbol,
    R_NameSymbol,
    R_NamesSymbol,
    R_NamespaceEnvSymbol,
    R_PackageSymbol,
    R_PreviousSymbol,
    R_QuoteSymbol,
    R_RowNamesSymbol,
    R_SeedsSymbol,
    R_SortListSymbol,
    R_SourceSymbol,
    R_SpecSymbol,
    R_SrcfileSymbol,
    R_SrcrefSymbol,
    R_TripleColonSymbol,
    R_TspSymbol,
    R_UseNamesSymbol,
    R_WholeSrcrefSymbol,
    R_dot_defined,
    R_dot_Method,
    R_dot_packageName,
    R_dot_target,
    R_dot_Generic,
);

#[unsafe(no_mangle)]
pub static R_NaInt: Global<c_int> = Global::new(NA_INTEGER);
#[unsafe(no_mangle)]
pub static R_NaReal: Global<f64> = Global::new(NA_REAL);
#[unsafe(no_mangle)]
pub static R_NaN: Global<f64> = Global::new(f64::NAN);
#[unsafe(no_mangle)]
pub static R_PosInf: Global<f64> = Global::new(f64::INFINITY);
#[unsafe(no_mangle)]
pub static R_NegInf: Global<f64> = Global::new(f64::NEG_INFINITY);
#[unsafe(no_mangle)]
pub static R_Interactive: Global<c_int> = Global::new(0);
#[unsafe(no_mangle)]
pub static R_InBCInterpreter: Global<c_int> = Global::new(0);

/// The names behind the ready-made symbols.
const SYMBOLS: &[(&Global<Sexp>, &str)] = &[
    (&R_AsCharacterSymbol, "as.character"),
    (&R_AtsignSymbol, "@"),
    (&R_BaseSymbol, "base"),
    (&R_baseSymbol, "base"),
    (&R_BraceSymbol, "{"),
    (&R_Bracket2Symbol, "[["),
    (&R_BracketSymbol, "["),
    (&R_ClassSymbol, "class"),
    (&R_ColonSymbol, ":"),
    (&R_DeviceSymbol, ".Device"),
    (&R_DimNamesSymbol, "dimnames"),
    (&R_DimSymbol, "dim"),
    (&R_DollarSymbol, "$"),
    (&R_DotEnvSymbol, ".Environment"),
    (&R_DotsSymbol, "..."),
    (&R_DoubleColonSymbol, "::"),
    (&R_DropSymbol, "drop"),
    (&R_EvalSymbol, "eval"),
    (&R_ExactSymbol, "exact"),
    (&R_FunctionSymbol, "function"),
    (&R_LastvalueSymbol, ".Last.value"),
    (&R_LevelsSymbol, "levels"),
    (&R_ModeSymbol, "mode"),
    (&R_NaRmSymbol, "na.rm"),
    (&R_NameSymbol, "name"),
    (&R_NamesSymbol, "names"),
    (&R_NamespaceEnvSymbol, ".__NAMESPACE__."),
    (&R_PackageSymbol, "package"),
    (&R_PreviousSymbol, "previous"),
    (&R_QuoteSymbol, "quote"),
    (&R_RowNamesSymbol, "row.names"),
    (&R_SeedsSymbol, ".Random.seed"),
    (&R_SortListSymbol, "sort.list"),
    (&R_SourceSymbol, "source"),
    (&R_SpecSymbol, "spec"),
    (&R_SrcfileSymbol, "srcfile"),
    (&R_SrcrefSymbol, "srcref"),
    (&R_TripleColonSymbol, ":::"),
    (&R_TspSymbol, "tsp"),
    (&R_UseNamesSymbol, "use.names"),
    (&R_WholeSrcrefSymbol, "wholeSrcref"),
    (&R_dot_defined, ".defined"),
    (&R_dot_Method, ".Method"),
    (&R_dot_packageName, ".packageName"),
    (&R_dot_target, ".target"),
    (&R_dot_Generic, ".Generic"),
];

pub(crate) unsafe fn install_symbols(heap: &mut Heap) {
    for (global, name) in SYMBOLS {
        let symbol = heap
            .install(name.as_bytes())
            .unwrap_or_else(|error| panic!("cannot install the symbol `{name}`: {error}"));
        global.set(symbol);
    }
}

/// Copies the heap's well-known objects into the exported data symbols.
pub(crate) fn publish(runtime: &crate::Runtime) {
    let known = runtime.heap.known();
    R_NilValue.set(known.nil);
    R_GlobalEnv.set(known.global_env);
    R_EmptyEnv.set(known.empty_env);
    R_BaseEnv.set(known.base_env);
    R_BaseNamespace.set(known.base_namespace);
    R_NamespaceRegistry.set(known.namespace_registry);
    R_UnboundValue.set(known.unbound);
    R_MissingArg.set(known.missing_arg);
    R_NaString.set(known.na_string);
    R_BlankString.set(known.blank_string);
    R_BlankScalarString.set(known.blank_scalar_string);
    R_TrueValue.set(known.true_value);
    R_FalseValue.set(known.false_value);
    R_LogicalNAValue.set(known.logical_na_value);
    R_Srcref.set(known.nil);
    R_CurrentExpression.set(known.nil);
    // GNU R points this at the global environment until the methods package is loaded, and
    // the interpreter updates it then.
    R_MethodsNamespace.set(known.global_env);
}
