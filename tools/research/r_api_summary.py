#!/usr/bin/env python3
"""Summarize the CSV files that r_api_usage.py writes.

Usage:
    python3 -I r_api_summary.py DATA_DIR

Prints the figures the R runtime design document quotes: code volume per
language, binding layers, symbol counts per API class, and the coverage curve,
which answers "if ry implements the N most widely used symbols, how many
packages have every symbol they need?". A package that links to a header-only
binding layer (Rcpp, cpp11, RcppArmadillo, ...) needs that layer's symbols too,
so the curve charges each package with the union of its own symbols and those
of every package it lists in LinkingTo.
"""

import collections
import csv
import os
import sys


def main():
    data = sys.argv[1]
    packages = list(csv.DictReader(open(os.path.join(data, "packages.csv"))))
    symbols = list(csv.DictReader(open(os.path.join(data, "symbols.csv"))))
    calls = list(csv.DictReader(open(os.path.join(data, "r_calls.csv"))))

    own = collections.defaultdict(set)
    for row in symbols:
        for package in row["used_by"].split():
            own[package].add(row["symbol"])
    linking = {row["package"]: row["linking_to"].split() for row in packages}
    needs = {}
    for package in own:
        needed = set(own[package])
        for dependency in linking.get(package, []):
            needed |= own.get(dependency, set())
        needs[package] = needed

    compiled = [row for row in packages if row["package"] in own]
    print(f"packages in closure: {len(packages)}")
    print(f"packages whose compiled code touches the R API: {len(compiled)}")
    totals = collections.Counter()
    for row in packages:
        for column in ("lines_c", "lines_cpp", "lines_headers", "lines_fortran", "lines_rust"):
            totals[column] += int(row[column])
    print("lines of compiled source:", dict(totals))

    layers = collections.Counter()
    for row in compiled:
        layer = row["linking_to"].split()
        if "Rcpp" in layer:
            layers["Rcpp"] += 1
        elif "cpp11" in layer:
            layers["cpp11"] += 1
        elif int(row["lines_rust"]):
            layers["Rust"] += 1
        else:
            layers["C API directly"] += 1
    print("binding layer:", dict(layers))

    by_class = collections.Counter(row["class"] for row in symbols)
    occurrences = collections.Counter()
    for row in symbols:
        occurrences[row["class"]] += int(row["occurrences"])
    print("distinct symbols per class:", dict(by_class), "total", len(symbols))
    print("occurrences per class:", dict(occurrences))

    ranked = [row["symbol"] for row in symbols]
    print("coverage curve (top N symbols -> packages fully covered, out of", len(needs), "):")
    for size in (25, 50, 100, 150, 200, 300, 400, 500, len(ranked)):
        chosen = set(ranked[:size])
        covered = sum(1 for needed in needs.values() if needed <= chosen)
        print(f"  {size:4d}: {covered}")

    print("packages needing the most symbols (own + LinkingTo):")
    for package, needed in sorted(needs.items(), key=lambda item: -len(item[1]))[:15]:
        print(f"  {package}: {len(needed)}")

    for marker in ("altrep", "ccallable", "external_ptr", "unwind"):
        users = [row["package"] for row in compiled if int(row[marker])]
        print(f"{marker}: {len(users)}: {' '.join(users)}")
    print("non-API users:")
    for row in compiled:
        names = [s["symbol"] for s in symbols if s["class"] == "non-api" and row["package"] in s["used_by"].split()]
        if names:
            print(f"  {row['package']}: {' '.join(names)}")

    print("subsystems (distinct symbols / packages using any):")
    for subsystem, members in group_by_subsystem(symbols).items():
        users = set()
        for row in members:
            users |= set(row["used_by"].split())
        print(f"  {subsystem}: {len(members)} / {len(users)}")

    call_totals = collections.Counter()
    users = collections.Counter()
    for row in calls:
        for kind, value in row.items():
            if kind != "package" and int(value):
                call_totals[kind] += int(value)
                users[kind] += 1
    print("R-level foreign calls (occurrences / packages):", {k: (call_totals[k], users[k]) for k in call_totals})


SUBSYSTEMS = [
    ("ALTREP", ("R_make_alt", "R_set_alt", "ALTREP", "R_altrep", "R_new_altrep", "ALTVEC", "ALTINTEGER", "ALTREAL", "ALTSTRING", "ALTLOGICAL", "ALTRAW", "ALTCOMPLEX", "ALTLIST")),
    ("graphics engine and devices", ("GE", "R_GE_", "pGE", "pDev", "DevDesc", "R_RGB", "R_RED", "R_GREEN", "R_BLUE", "R_ALPHA", "R_TRAN", "R_OPAQUE", "LTY_", "Rf_RGBpar", "R_CheckDeviceAvailable")),
    ("connections", ("R_GetConnection", "R_ReadConnection", "R_WriteConnection", "R_new_custom_connection", "Rconnection", "R_CONNECTIONS_VERSION")),
    ("protection and GC", ("Rf_protect", "Rf_unprotect", "PROTECT_", "R_Reprotect", "R_ProtectWithIndex", "R_PreserveObject", "R_ReleaseObject", "R_PreserveInMSet", "R_ReleaseFromMSet", "R_NewPreciousMSet", "R_gc", "R_alloc", "vmaxget", "vmaxset", "R_Calloc", "R_Free", "R_chk_", "S_alloc", "R_CheckStack")),
    ("external pointers, weak references, finalizers", ("R_MakeExternalPtr", "R_ExternalPtr", "R_SetExternalPtr", "R_ClearExternalPtr", "R_RegisterCFinalizer", "R_RegisterFinalizer", "R_MakeWeakRef", "R_WeakRef", "EXTPTR_", "R_RunPendingFinalizers")),
    ("non-local exits and errors", ("Rf_error", "Rf_errorcall", "Rf_warning", "Rf_warningcall", "R_UnwindProtect", "R_ContinueUnwind", "R_MakeUnwindCont", "R_ToplevelExec", "R_tryEval", "R_tryEvalSilent", "R_tryCatch", "R_tryCatchError", "R_CheckUserInterrupt", "Rf_onintr", "R_ExecWithCleanup")),
    ("registration and cross-package C calls", ("R_registerRoutines", "R_useDynamicSymbols", "R_forceSymbols", "R_RegisterCCallable", "R_GetCCallable", "DllInfo", "R_CallMethodDef", "R_CMethodDef", "R_ExternalMethodDef", "R_FortranMethodDef", "R_getEmbeddingDllInfo", "R_FindSymbol")),
    ("environments, closures, promises", ("R_GlobalEnv", "R_BaseEnv", "R_EmptyEnv", "R_BaseNamespace", "R_NamespaceRegistry", "Rf_findVar", "Rf_findFun", "Rf_defineVar", "Rf_setVar", "R_getVar", "R_existsVarInFrame", "R_removeVarFromFrame", "R_lsInternal", "R_ParentEnv", "R_NewEnv", "Rf_NewEnvironment", "ENCLOS", "SET_ENCLOS", "FRAME", "SET_FRAME", "HASHTAB", "SET_HASHTAB", "CLOENV", "SET_CLOENV", "FORMALS", "SET_FORMALS", "BODY", "SET_BODY", "R_ClosureExpr", "R_ClosureBody", "R_ClosureFormals", "R_ClosureEnv", "R_mkClosure", "PRENV", "PRVALUE", "PRCODE", "SET_PR", "R_PromiseExpr", "R_IsPackageEnv", "R_IsNamespaceEnv", "R_FindNamespace", "Rf_isEnvironment", "R_MakeActiveBinding", "R_BindingIsActive", "R_LockBinding", "R_unLockBinding", "R_LockEnvironment", "R_EnvironmentIsLocked", "R_BindingIsLocked", "R_GetCurrentEnv", "R_ActiveBinding")),
    ("evaluation and calls", ("Rf_eval", "Rf_lang", "Rf_lcons", "LCONS", "Rf_allocLang", "R_forceAndCall", "Rf_applyClosure", "R_ParseVector", "Rf_install", "PRINTNAME", "Rf_list", "Rf_cons", "CONS", "CAR", "CDR", "CAD", "CDD", "SETCAR", "SETCDR", "SETCAD", "TAG", "SET_TAG", "R_MissingArg", "R_DotsSymbol", "Rf_elt", "Rf_lastElt", "Rf_nthcdr", "Rf_allocList", "Rf_allocSExp", "R_ParseEvalString", "R_ParseString")),
    ("strings and encodings", ("R_CHAR", "Rf_mkChar", "Rf_mkString", "STRING_ELT", "SET_STRING_ELT", "STRING_PTR", "R_NaString", "R_BlankString", "CE_", "Rf_translateChar", "Rf_getCharCE", "Rf_reEnc", "IS_UTF8", "IS_ASCII", "Rf_ScalarString", "Rf_ucstoutf8", "Rf_utf8Toutf8NoPUA", "cetype_t", "R_nchar")),
    ("vector data access", ("INTEGER", "REAL", "LOGICAL", "COMPLEX", "RAW", "DATAPTR", "VECTOR_ELT", "SET_VECTOR_ELT", "VECTOR_PTR", "LENGTH", "Rf_length", "Rf_xlength", "XLENGTH", "TRUELENGTH", "SET_TRUELENGTH", "SETLENGTH", "XTRUELENGTH", "IS_GROWABLE", "SET_GROWABLE_BIT", "Rf_lengthgets", "Rf_xlengthgets", "INTEGER_ELT", "REAL_ELT", "LOGICAL_ELT", "RAW_ELT", "COMPLEX_ELT", "SET_INTEGER_ELT", "SET_REAL_ELT", "SET_LOGICAL_ELT", "INTEGER_GET_REGION", "REAL_GET_REGION", "INTEGER_IS_SORTED", "REAL_IS_SORTED", "INTEGER_NO_NA", "REAL_NO_NA", "STRING_NO_NA", "SORTED_", "KNOWN_", "UNKNOWN_SORTEDNESS")),
    ("allocation and coercion", ("Rf_allocVector", "Rf_allocMatrix", "Rf_alloc3DArray", "Rf_allocArray", "Rf_allocS4Object", "Rf_Scalar", "Rf_coerceVector", "Rf_as", "Rf_duplicate", "Rf_shallow_duplicate", "Rf_lazy_duplicate", "Rf_copyVector", "Rf_copyMatrix", "Rf_copyMostAttrib", "MAYBE_SHARED", "MAYBE_REFERENCED", "NO_REFERENCES", "MARK_NOT_MUTABLE", "NAMED", "SET_NAMED", "Rf_PairToVectorList", "Rf_VectorToPairList")),
    ("attributes, classes, S4", ("Rf_getAttrib", "Rf_setAttrib", "ATTRIB", "SET_ATTRIB", "R_NamesSymbol", "R_ClassSymbol", "R_DimSymbol", "R_DimNamesSymbol", "R_RowNamesSymbol", "R_LevelsSymbol", "Rf_inherits", "Rf_classgets", "Rf_namesgets", "Rf_dimgets", "Rf_dimnamesgets", "OBJECT", "SET_OBJECT", "IS_S4_OBJECT", "SET_S4_OBJECT", "UNSET_S4_OBJECT", "Rf_isS4", "R_do_slot", "R_do_new_object", "R_do_MAKE_CLASS", "R_has_slot", "GET_SLOT", "SET_SLOT", "R_getClassDef", "Rf_GetRowNames", "Rf_GetColNames", "Rf_GetArrayDimnames", "Rf_GetMatrixDimnames", "Rf_isFrame", "Rf_isFactor", "LEVELS", "Rf_nlevels", "R_compute_identical", "Rf_copyMostAttribNoTs")),
    ("math, RNG, BLAS/LAPACK", ("Rf_d", "Rf_p", "Rf_q", "Rf_r", "dnorm4", "pnorm5", "qnorm5", "unif_rand", "norm_rand", "exp_rand", "GetRNGstate", "PutRNGstate", "R_unif_index", "F77_", "FCONE", "FCLEN", "BLAS", "La_", "R_finite", "R_IsNA", "R_IsNaN", "ISNAN", "ISNA", "R_FINITE", "R_PosInf", "R_NegInf", "R_NaN", "R_NaReal", "R_NaInt", "R_pow", "R_rsort", "R_qsort", "rsort_with_index", "revsort", "Rf_fmax2", "Rf_fmin2", "Rf_imax2", "Rf_imin2", "Rf_fsign", "Rf_ftrunc", "Rf_fprec", "Rf_fround", "Rf_choose", "Rf_lchoose", "Rf_gammafn", "Rf_lgammafn", "Rf_beta", "Rf_lbeta", "Rf_digamma", "Rf_trigamma", "Rf_bessel", "R_orderVector", "M_", "R_atof", "R_strtod", "Rf_sign", "Rf_log1pmx", "Rf_lgamma1p", "Rf_logspace", "R_zeroin", "optif9", "vmmin", "nmmin", "lbfgsb", "cgmin", "samin", "Rdqags", "Rdqagi", "R_max_col", "set_seed", "user_unif")),
]


def group_by_subsystem(symbols):
    groups = collections.OrderedDict((name, []) for name, _ in SUBSYSTEMS)
    groups["types, constants, and everything else"] = []
    for row in symbols:
        for name, prefixes in SUBSYSTEMS:
            if row["symbol"].startswith(prefixes):
                groups[name].append(row)
                break
        else:
            groups["types, constants, and everything else"].append(row)
    return groups


if __name__ == "__main__":
    main()
