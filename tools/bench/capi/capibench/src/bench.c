#include <string.h>
#include <R.h>
#include <Rinternals.h>
#include <R_ext/Rdynload.h>

/* Each loop calls the accessor on every iteration, as macro-heavy package code does. */
SEXP cb_integer(SEXP x, SEXP reps) {
    double total = 0;
    for (int r = 0; r < asInteger(reps); r++)
        for (R_xlen_t i = 0; i < XLENGTH(x); i++) total += INTEGER(x)[i];
    return ScalarReal(total);
}

SEXP cb_real(SEXP x, SEXP reps) {
    double total = 0;
    for (int r = 0; r < asInteger(reps); r++)
        for (R_xlen_t i = 0; i < XLENGTH(x); i++) total += REAL(x)[i];
    return ScalarReal(total);
}

SEXP cb_string(SEXP x, SEXP reps) {
    double total = 0;
    for (int r = 0; r < asInteger(reps); r++)
        for (R_xlen_t i = 0; i < XLENGTH(x); i++) total += strlen(CHAR(STRING_ELT(x, i)));
    return ScalarReal(total);
}

SEXP cb_list(SEXP x, SEXP reps) {
    double total = 0;
    for (int r = 0; r < asInteger(reps); r++)
        for (R_xlen_t i = 0; i < XLENGTH(x); i++) {
            SEXP element = VECTOR_ELT(x, i);
            total += TYPEOF(element) + XLENGTH(element);
        }
    return ScalarReal(total);
}

SEXP cb_pairlist(SEXP x, SEXP reps) {
    double total = 0;
    for (int r = 0; r < asInteger(reps); r++)
        for (SEXP cell = x; cell != R_NilValue; cell = CDR(cell))
            total += TYPEOF(CAR(cell)) + (TAG(cell) != R_NilValue);
    return ScalarReal(total);
}

/* Unprotected temporaries dropped at once: GNU R collects them as the loop runs. */
SEXP cb_temporaries(SEXP n) {
    double total = 0;
    for (int i = 0; i < asInteger(n); i++) {
        SEXP scratch = allocVector(REALSXP, 64);
        REAL(scratch)[0] = i;
        total += REAL(scratch)[0];
    }
    return ScalarReal(total);
}

SEXP cb_strings(SEXP n) {
    int count = asInteger(n);
    SEXP out = PROTECT(allocVector(STRSXP, count));
    char buffer[32];
    for (int i = 0; i < count; i++) {
        snprintf(buffer, sizeof buffer, "s%d", i % 1000);
        SET_STRING_ELT(out, i, mkChar(buffer));
    }
    UNPROTECT(1);
    return out;
}

static const R_CallMethodDef calls[] = {
    {"cb_integer", (DL_FUNC) &cb_integer, 2},
    {"cb_real", (DL_FUNC) &cb_real, 2},
    {"cb_string", (DL_FUNC) &cb_string, 2},
    {"cb_list", (DL_FUNC) &cb_list, 2},
    {"cb_pairlist", (DL_FUNC) &cb_pairlist, 2},
    {"cb_temporaries", (DL_FUNC) &cb_temporaries, 1},
    {"cb_strings", (DL_FUNC) &cb_strings, 1},
    {NULL, NULL, 0}
};

void R_init_capibench(DllInfo *info) {
    R_registerRoutines(info, NULL, calls, NULL, NULL);
    R_useDynamicSymbols(info, FALSE);
}
