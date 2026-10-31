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

    print("R_UnwindProtect users:", next((row["used_by"] for row in symbols if row["symbol"] == "R_UnwindProtect"), ""))
    print("names used by at least k packages:", {k: sum(1 for row in symbols if int(row["packages"]) >= k) for k in (2, 5, 10, 25, 50)})
    api_rows = [row for row in symbols if row["class"] != "blas-lapack"]
    print("names excluding BLAS/LAPACK:", len(api_rows), "occurrences:", sum(int(row["occurrences"]) for row in api_rows))

    ranked = [row["symbol"] for row in symbols]
    half = next(
        size for size in range(1, len(ranked) + 1)
        if 2 * sum(1 for needed in needs.values() if needed <= set(ranked[:size])) >= len(needs)
    )
    print("names needed to fully cover half the compiled packages:", half)
    print("coverage curve (top N symbols -> packages fully covered, out of", len(needs), "):")
    for size in (25, 50, 100, 150, 200, 300, 400, 500, len(ranked)):
        chosen = set(ranked[:size])
        covered = sum(1 for needed in needs.values() if needed <= chosen)
        print(f"  {size:4d}: {covered}")

    print("packages needing the most symbols (own + LinkingTo):")
    for package, needed in sorted(needs.items(), key=lambda item: -len(item[1]))[:15]:
        print(f"  {package}: {len(needed)}")

    print("packages using the most symbols in their own code:")
    for package, used in sorted(own.items(), key=lambda item: -len(item[1]))[:15]:
        print(f"  {package}: {len(used)}")

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


# Subsystems are assigned by declaring header first, then, for the names in
# Rinternals.h and friends, by exact name or by a prefix ending in "*".
HEADER_SUBSYSTEMS = {
    "R_ext/Altrep.h": "ALTREP",
    "R_ext/GraphicsEngine.h": "graphics engine and devices",
    "R_ext/GraphicsDevice.h": "graphics engine and devices",
    "R_ext/Connections.h": "connections",
    "R_ext/BLAS.h": "BLAS, LAPACK and LINPACK",
    "R_ext/Lapack.h": "BLAS, LAPACK and LINPACK",
    "R_ext/Linpack.h": "BLAS, LAPACK and LINPACK",
    "Rmath.h0.in": "math and RNG",
    "R_ext/Random.h": "math and RNG",
    "R_ext/Arith.h": "math and RNG",
    "R_ext/Applic.h": "math and RNG",
    "R_ext/Rdynload.h": "registration and cross-package C calls",
    "R_ext/Memory.h": "protection and GC",
    "R_ext/Error.h": "non-local exits and errors",
}

# GraphicsDevice.h also declares the interrupt-suspension macros that
# non-graphics packages use, so those are classified by name first.
NAME_SUBSYSTEMS = [
    ("non-local exits and errors", "BEGIN_SUSPEND_INTERRUPTS END_SUSPEND_INTERRUPTS R_interrupts_pending R_interrupts_suspended Rf_error Rf_errorcall Rf_warning Rf_warningcall R_UnwindProtect R_ContinueUnwind R_MakeUnwindCont R_ToplevelExec R_tryEval R_tryEvalSilent R_tryCatch R_tryCatchError R_CheckUserInterrupt Rf_onintr R_ExecWithCleanup R_withCallingErrorHandler"),
    ("ALTREP", "ALTREP* R_altrep* R_new_altrep R_make_alt* R_set_alt* ALTVEC* ALTINTEGER* ALTREAL* ALTSTRING* ALTLOGICAL* ALTRAW* ALTCOMPLEX* ALTLIST*"),
    ("protection and GC", "Rf_protect Rf_unprotect PROTECT_INDEX R_ProtectWithIndex R_Reprotect R_PreserveObject R_ReleaseObject R_PreserveInMSet R_ReleaseFromMSet R_NewPreciousMSet R_gc R_gc_running R_alloc vmaxget vmaxset R_CheckStack R_CheckStack2 Rf_unprotect_ptr"),
    ("external pointers, weak references, finalizers", "R_MakeExternalPtr* R_ExternalPtr* R_SetExternalPtr* R_ClearExternalPtr R_RegisterCFinalizer* R_RegisterFinalizer* R_MakeWeakRef* R_WeakRef* EXTPTR_* R_RunPendingFinalizers EXTPTRSXP WEAKREFSXP"),
    ("environments, closures, promises", "R_GlobalEnv R_BaseEnv R_EmptyEnv R_BaseNamespace R_NamespaceRegistry Rf_findVar Rf_findVarInFrame Rf_findVarInFrame3 Rf_findFun Rf_defineVar Rf_setVar R_getVar R_getVarEx R_existsVarInFrame R_removeVarFromFrame R_lsInternal* R_ParentEnv R_NewEnv ENCLOS SET_ENCLOS FRAME SET_FRAME HASHTAB SET_HASHTAB CLOENV SET_CLOENV FORMALS SET_FORMALS BODY SET_BODY R_Closure* R_mkClosure PRENV PRVALUE PRCODE SET_PR* R_PromiseExpr R_IsPackageEnv R_IsNamespaceEnv R_FindNamespace Rf_isEnvironment R_MakeActiveBinding R_BindingIsActive R_LockBinding R_unLockBinding R_LockEnvironment R_EnvironmentIsLocked R_BindingIsLocked R_GetBindingType R_BindingType* R_ActiveBinding* R_UnboundValue ENVSXP CLOSXP PROMSXP"),
    ("evaluation, calls and pairlists", "Rf_eval Rf_lang* Rf_lcons Rf_allocLang R_forceAndCall Rf_applyClosure R_ParseVector R_ParseEvalString R_ParseString Rf_install Rf_installChar Rf_installTrChar PRINTNAME Rf_list* Rf_cons CAR CDR CAD* CDD* SETCAR SETCDR SETCAD* TAG SET_TAG R_MissingArg R_DotsSymbol Rf_elt Rf_lastElt Rf_nthcdr Rf_allocList Rf_allocSExp LANGSXP LISTSXP DOTSXP SYMSXP ParseStatus PARSE_* Rf_isLanguage Rf_isSymbol"),
    ("strings and encodings", "R_CHAR Rf_mkChar* Rf_mkString STRING_ELT SET_STRING_ELT STRING_PTR STRING_PTR_RO R_NaString R_BlankString CE_* Rf_translateChar* Rf_getCharCE Rf_reEnc IS_UTF8 IS_ASCII Rf_ScalarString Rf_ucstoutf8 Rf_utf8Toutf8NoPUA cetype_t R_nchar STRSXP CHARSXP Rf_isString Rf_StringBlank Rf_charIsASCII Rf_charIsUTF8 Rf_charIsLatin1"),
    ("vector data access", "INTEGER INTEGER_RO REAL REAL_RO LOGICAL LOGICAL_RO COMPLEX COMPLEX_RO RAW RAW_RO DATAPTR DATAPTR_RO DATAPTR_OR_NULL VECTOR_ELT SET_VECTOR_ELT VECTOR_PTR VECTOR_PTR_RO LENGTH Rf_length Rf_xlength XLENGTH TRUELENGTH SET_TRUELENGTH SETLENGTH XTRUELENGTH IS_GROWABLE SET_GROWABLE_BIT Rf_lengthgets Rf_xlengthgets *_ELT *_GET_REGION *_IS_SORTED *_NO_NA SORTED_* KNOWN_* UNKNOWN_SORTEDNESS R_allocResizableVector R_resizeVector R_duplicateAsResizable R_isResizable R_maxLength"),
    ("allocation, coercion, duplication", "Rf_allocVector Rf_allocVector3 Rf_allocMatrix Rf_alloc3DArray Rf_allocArray Rf_allocS4Object Rf_Scalar* Rf_coerceVector Rf_as* Rf_duplicate Rf_shallow_duplicate Rf_lazy_duplicate Rf_copyVector Rf_copyMatrix Rf_copyMostAttrib MAYBE_SHARED MAYBE_REFERENCED NO_REFERENCES MARK_NOT_MUTABLE NAMED SET_NAMED Rf_PairToVectorList Rf_VectorToPairList"),
    ("attributes, classes, S4", "Rf_getAttrib Rf_setAttrib ATTRIB SET_ATTRIB R_mapAttrib R_hasAttrib R_getAttribNames R_*Symbol Rf_inherits Rf_classgets Rf_namesgets Rf_dimgets Rf_dimnamesgets OBJECT SET_OBJECT IS_S4_OBJECT SET_S4_OBJECT UNSET_S4_OBJECT Rf_isS4 R_do_* R_has_slot GET_SLOT SET_SLOT R_getClassDef Rf_GetRowNames Rf_GetColNames Rf_GetArrayDimnames Rf_GetMatrixDimnames Rf_isFrame Rf_isFactor LEVELS Rf_nlevels R_compute_identical Rf_isObject"),
]


def subsystem_of(symbol, declared_in):
    for name, patterns in NAME_SUBSYSTEMS:
        for pattern in patterns.split():
            if pattern.endswith("*") and symbol.startswith(pattern[:-1]):
                return name
            if pattern.startswith("*") and symbol.endswith(pattern[1:]):
                return name
            if pattern == symbol:
                return name
    return HEADER_SUBSYSTEMS.get(declared_in, "types, constants, and everything else")


def group_by_subsystem(symbols):
    groups = collections.defaultdict(list)
    for row in symbols:
        groups[subsystem_of(row["symbol"], row["declared_in"])].append(row)
    return dict(sorted(groups.items(), key=lambda item: -len({p for r in item[1] for p in r["used_by"].split()})))


if __name__ == "__main__":
    main()
