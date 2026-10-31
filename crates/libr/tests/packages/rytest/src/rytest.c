#include <R.h>
#include <Rinternals.h>
#include <R_ext/Altrep.h>
#include <R_ext/Rdynload.h>
#include <stdint.h>
#include <string.h>

/* Values and vectors. */

SEXP rytest_sum(SEXP x)
{
    double total = 0;
    R_xlen_t n = XLENGTH(x);
    if (TYPEOF(x) == REALSXP) {
        const double *values = REAL_RO(x);
        for (R_xlen_t i = 0; i < n; i++)
            total += values[i];
    } else if (TYPEOF(x) == INTSXP) {
        const int *values = INTEGER_RO(x);
        for (R_xlen_t i = 0; i < n; i++) {
            if (values[i] == NA_INTEGER)
                return ScalarReal(NA_REAL);
            total += values[i];
        }
    } else {
        error("rytest_sum needs a numeric vector, not a %s", type2char(TYPEOF(x)));
    }
    return ScalarReal(total);
}

/* A named list of n strings, "item_1" to "item_n". */
SEXP rytest_make_list(SEXP count)
{
    int n = asInteger(count);
    SEXP list = PROTECT(allocVector(VECSXP, n));
    SEXP names = PROTECT(allocVector(STRSXP, n));
    char buffer[64];
    for (int i = 0; i < n; i++) {
        snprintf(buffer, sizeof buffer, "item_%d", i + 1);
        SET_VECTOR_ELT(list, i, mkString(buffer));
        snprintf(buffer, sizeof buffer, "n%d", i + 1);
        SET_STRING_ELT(names, i, mkChar(buffer));
    }
    setAttrib(list, R_NamesSymbol, names);
    UNPROTECT(2);
    return list;
}

/* Builds a long pairlist and a vector of vectors under allocation pressure, collecting in
   between, then checks every value survived. Returns the checksum. */
SEXP rytest_gc_churn(SEXP count)
{
    int n = asInteger(count);
    SEXP keep = PROTECT(allocVector(VECSXP, n));
    SEXP chain = R_NilValue;
    PROTECT_INDEX chain_index;
    PROTECT_WITH_INDEX(chain, &chain_index);
    for (int i = 0; i < n; i++) {
        SEXP element = allocVector(INTSXP, 3);
        INTEGER(element)[0] = i;
        INTEGER(element)[1] = i * 2;
        INTEGER(element)[2] = i * 3;
        SET_VECTOR_ELT(keep, i, element);
        allocVector(REALSXP, 100); /* garbage */
        chain = cons(ScalarInteger(i), chain);
        REPROTECT(chain, chain_index);
        if (i % 50 == 0)
            R_gc();
    }
    double checksum = 0;
    for (int i = 0; i < n; i++) {
        SEXP element = VECTOR_ELT(keep, i);
        if (INTEGER(element)[0] != i || INTEGER(element)[1] != 2 * i || INTEGER(element)[2] != 3 * i)
            error("element %d was corrupted", i);
        checksum += INTEGER(element)[2];
    }
    int expected = n - 1;
    for (SEXP cell = chain; cell != R_NilValue; cell = CDR(cell), expected--) {
        if (asInteger(CAR(cell)) != expected)
            error("pairlist cell %d was corrupted", expected);
    }
    UNPROTECT(2);
    return ScalarReal(checksum);
}

/* Keeps temporaries in C locals without PROTECT. GNU R may collect them; ry's handle scope
   keeps everything native code receives alive until the call returns. */
SEXP rytest_unprotected_temporaries(SEXP count)
{
    int n = asInteger(count);
    SEXP first = allocVector(INTSXP, 1);
    INTEGER(first)[0] = 41;
    SEXP second = mkString("kept");
    for (int i = 0; i < n; i++)
        allocVector(REALSXP, 64);
    R_gc();
    SEXP result = allocVector(VECSXP, 2);
    SET_VECTOR_ELT(result, 0, ScalarInteger(INTEGER(first)[0] + 1));
    SET_VECTOR_ELT(result, 1, second);
    return result;
}

/* PROTECT discipline. */

SEXP rytest_protect_leak(void)
{
    PROTECT(allocVector(INTSXP, 1));
    PROTECT(allocVector(INTSXP, 1));
    return ScalarLogical(TRUE);
}

SEXP rytest_unprotect_too_many(void)
{
    PROTECT(ScalarInteger(1));
    UNPROTECT(2);
    return R_NilValue;
}

SEXP rytest_protect_overflow(void)
{
    for (;;)
        PROTECT(R_NilValue);
    return R_NilValue;
}

SEXP rytest_protect_depth_error(void)
{
    PROTECT(ScalarInteger(1));
    PROTECT(ScalarInteger(2));
    PROTECT(ScalarInteger(3));
    error("failing with three objects protected");
    return R_NilValue;
}

/* Errors and non-local exits. */

SEXP rytest_error(SEXP message)
{
    error("custom: %s", CHAR(STRING_ELT(message, 0)));
    return R_NilValue;
}

SEXP rytest_errorcall(void)
{
    SEXP call = PROTECT(lang2(install("f"), ScalarInteger(1)));
    errorcall(call, "boom");
    return R_NilValue;
}

SEXP rytest_warning(void)
{
    warning("careful: %d", 42);
    return ScalarInteger(1);
}

SEXP rytest_eval(SEXP call, SEXP env)
{
    return eval(call, env);
}

SEXP rytest_try_eval(SEXP call)
{
    int failed = 0;
    SEXP value = R_tryEvalSilent(call, R_GlobalEnv, &failed);
    if (failed)
        return mkString(R_curErrorBuf());
    return value;
}

static int cleanup_count = 0;
static int cleanup_jumped = -1;

static SEXP eval_body(void *data)
{
    return eval((SEXP) data, R_GlobalEnv);
}

static void count_cleanup(void *data, Rboolean jump)
{
    cleanup_count++;
    cleanup_jumped = jump;
}

SEXP rytest_unwind_protect(SEXP call)
{
    SEXP cont = PROTECT(R_MakeUnwindCont());
    SEXP result = R_UnwindProtect(eval_body, call, count_cleanup, NULL, cont);
    UNPROTECT(1);
    return result;
}

SEXP rytest_cleanup_state(void)
{
    SEXP state = allocVector(INTSXP, 2);
    INTEGER(state)[0] = cleanup_count;
    INTEGER(state)[1] = cleanup_jumped;
    return state;
}

static void toplevel_body(void *data)
{
    eval((SEXP) data, R_GlobalEnv);
}

SEXP rytest_toplevel(SEXP call)
{
    return ScalarLogical(R_ToplevelExec(toplevel_body, call));
}

static SEXP catch_handler(SEXP condition, void *data)
{
    SEXP message = VECTOR_ELT(condition, 0);
    char buffer[512];
    snprintf(buffer, sizeof buffer, "caught: %s", CHAR(STRING_ELT(message, 0)));
    return mkString(buffer);
}

SEXP rytest_try_catch(SEXP call)
{
    return R_tryCatchError(eval_body, call, catch_handler, NULL);
}

static void plain_cleanup(void *data)
{
    cleanup_count++;
}

SEXP rytest_exec_with_cleanup(SEXP call)
{
    return R_ExecWithCleanup(eval_body, call, plain_cleanup, NULL);
}

/* R_alloc. */

SEXP rytest_r_alloc(SEXP count)
{
    int n = asInteger(count);
    void *mark = vmaxget();
    int *block = (int *) R_alloc(n, sizeof(int));
    for (int i = 0; i < n; i++)
        block[i] = i;
    char *scratch = S_alloc(100, 1);
    for (int i = 0; i < 100; i++)
        if (scratch[i] != 0)
            error("S_alloc memory is not zeroed");
    double sum = 0;
    for (int i = 0; i < n; i++)
        sum += block[i];
    vmaxset(mark);
    char *kept = R_alloc(10, 1);
    strcpy(kept, "watermark");
    return ScalarReal(sum);
}

SEXP rytest_r_alloc_error(SEXP count)
{
    R_alloc(asInteger(count), 1);
    error("failing while holding R_alloc memory");
    return R_NilValue;
}

/* External pointers and finalizers. */

static int finalized = 0;

static void finalize_counter(SEXP pointer)
{
    int *value = (int *) R_ExternalPtrAddr(pointer);
    if (value != NULL) {
        finalized += *value;
        free(value);
        R_ClearExternalPtr(pointer);
    }
}

SEXP rytest_external_ptr(SEXP weight)
{
    int *value = malloc(sizeof(int));
    *value = asInteger(weight);
    SEXP pointer = PROTECT(R_MakeExternalPtr(value, install("rytest_counter"), R_NilValue));
    R_RegisterCFinalizerEx(pointer, finalize_counter, FALSE);
    UNPROTECT(1);
    return pointer;
}

SEXP rytest_external_value(SEXP pointer)
{
    int *value = (int *) R_ExternalPtrAddr(pointer);
    if (TYPEOF(R_ExternalPtrTag(pointer)) != SYMSXP)
        error("the tag was lost");
    return ScalarInteger(value == NULL ? NA_INTEGER : *value);
}

SEXP rytest_finalized(void)
{
    return ScalarInteger(finalized);
}

/* ALTREP: a compact integer sequence start, start + 1, ... that materializes on demand. */

static R_altrep_class_t sequence_class;

static R_xlen_t sequence_length(SEXP x)
{
    return (R_xlen_t) REAL(R_altrep_data1(x))[1];
}

static int sequence_elt(SEXP x, R_xlen_t i)
{
    return (int) REAL(R_altrep_data1(x))[0] + (int) i;
}

static void *sequence_dataptr(SEXP x, Rboolean writable)
{
    SEXP data = R_altrep_data2(x);
    if (data == R_NilValue) {
        R_xlen_t n = sequence_length(x);
        data = PROTECT(allocVector(INTSXP, n));
        for (R_xlen_t i = 0; i < n; i++)
            INTEGER(data)[i] = sequence_elt(x, i);
        R_set_altrep_data2(x, data);
        UNPROTECT(1);
    }
    return DATAPTR(data);
}

static const void *sequence_dataptr_or_null(SEXP x)
{
    SEXP data = R_altrep_data2(x);
    return data == R_NilValue ? NULL : DATAPTR_RO(data);
}

static int sequence_is_sorted(SEXP x)
{
    return SORTED_INCR;
}

SEXP rytest_sequence(SEXP start, SEXP length)
{
    SEXP state = PROTECT(allocVector(REALSXP, 2));
    REAL(state)[0] = asReal(start);
    REAL(state)[1] = asReal(length);
    SEXP x = R_new_altrep(sequence_class, state, R_NilValue);
    UNPROTECT(1);
    return x;
}

SEXP rytest_sequence_info(SEXP x)
{
    SEXP info = PROTECT(allocVector(INTSXP, 6));
    INTEGER(info)[0] = ALTREP(x);
    INTEGER(info)[1] = DATAPTR_OR_NULL(x) != NULL;
    INTEGER(info)[2] = INTEGER_ELT(x, 2);
    INTEGER(info)[3] = INTEGER_IS_SORTED(x);
    INTEGER(info)[4] = INTEGER(x)[LENGTH(x) - 1];
    INTEGER(info)[5] = DATAPTR_OR_NULL(x) != NULL;
    UNPROTECT(1);
    return info;
}

/* Cross-package C callables. */

static SEXP twice(SEXP x)
{
    return ScalarReal(2 * asReal(x));
}

SEXP rytest_call_callable(SEXP x)
{
    SEXP (*function)(SEXP) = (SEXP (*)(SEXP)) R_GetCCallable("rytest", "twice");
    return function(x);
}

SEXP rytest_missing_callable(void)
{
    R_GetCCallable("nopkg", "nothing");
    return R_NilValue;
}

/* Strings and encodings. */

SEXP rytest_strings(void)
{
    SEXP result = PROTECT(allocVector(STRSXP, 6));
    const char latin1[] = { 'c', 'a', 'f', (char) 0xe9, 0 };
    SEXP from_latin1 = mkCharCE(latin1, CE_LATIN1);
    SET_STRING_ELT(result, 0, mkCharCE(translateCharUTF8(from_latin1), CE_UTF8));
    SET_STRING_ELT(result, 1, mkChar(getCharCE(from_latin1) == CE_LATIN1 ? "latin1" : "other"));
    SET_STRING_ELT(result, 2, mkChar(IS_ASCII(mkChar("plain")) ? "ascii" : "not ascii"));
    SET_STRING_ELT(result, 3, mkChar(mkChar("same") == mkCharCE("same", CE_UTF8) ? "interned" : "copied"));
    SET_STRING_ELT(result, 4, NA_STRING);
    SET_STRING_ELT(result, 5, mkCharLen("truncated", 5));
    UNPROTECT(1);
    return result;
}

SEXP rytest_embedded_nul(void)
{
    return mkCharLen("a\0b", 3);
}

/* Coercion, attributes, environments. */

SEXP rytest_coerce(SEXP x, SEXP type)
{
    return coerceVector(x, (SEXPTYPE) asInteger(type));
}

SEXP rytest_matrix(SEXP rows, SEXP columns)
{
    int r = asInteger(rows), c = asInteger(columns);
    SEXP m = PROTECT(allocMatrix(REALSXP, r, c));
    for (int i = 0; i < r * c; i++)
        REAL(m)[i] = i;
    SEXP klass = PROTECT(mkString("rytest_matrix"));
    classgets(m, klass);
    SEXP out = PROTECT(allocVector(INTSXP, 4));
    INTEGER(out)[0] = nrows(m);
    INTEGER(out)[1] = ncols(m);
    INTEGER(out)[2] = isMatrix(m);
    INTEGER(out)[3] = inherits(m, "rytest_matrix") && OBJECT(m);
    UNPROTECT(3);
    return out;
}

SEXP rytest_environment(void)
{
    SEXP env = PROTECT(R_NewEnv(R_GlobalEnv, TRUE, 0));
    SEXP x = install("x");
    defineVar(x, ScalarInteger(7), env);
    SEXP out = PROTECT(allocVector(INTSXP, 5));
    INTEGER(out)[0] = asInteger(findVarInFrame(env, x));
    INTEGER(out)[1] = R_existsVarInFrame(env, install("y"));
    defineVar(install("y"), ScalarInteger(8), R_GlobalEnv);
    INTEGER(out)[2] = asInteger(findVar(install("y"), env));
    R_removeVarFromFrame(x, env);
    INTEGER(out)[3] = findVarInFrame(env, x) == R_UnboundValue;
    INTEGER(out)[4] = ENCLOS(env) == R_GlobalEnv;
    UNPROTECT(2);
    return out;
}

SEXP rytest_duplicate(SEXP x)
{
    SEXP copy = PROTECT(duplicate(x));
    if (TYPEOF(copy) == REALSXP && XLENGTH(copy) > 0)
        REAL(copy)[0] = -1;
    SEXP out = PROTECT(allocVector(VECSXP, 2));
    SET_VECTOR_ELT(out, 0, x);
    SET_VECTOR_ELT(out, 1, copy);
    UNPROTECT(2);
    return out;
}

SEXP rytest_identical(SEXP x, SEXP y)
{
    return ScalarLogical(R_compute_identical(x, y, 16));
}

/* The random number generator, through .Random.seed as packages use it. */

SEXP rytest_unif(SEXP count)
{
    int n = asInteger(count);
    SEXP out = PROTECT(allocVector(REALSXP, n));
    GetRNGstate();
    for (int i = 0; i < n; i++)
        REAL(out)[i] = unif_rand();
    PutRNGstate();
    UNPROTECT(1);
    return out;
}

SEXP rytest_sample(SEXP range, SEXP count)
{
    int n = asInteger(count);
    SEXP out = PROTECT(allocVector(INTSXP, n));
    GetRNGstate();
    for (int i = 0; i < n; i++)
        INTEGER(out)[i] = (int) R_unif_index(asReal(range)) + 1;
    PutRNGstate();
    UNPROTECT(1);
    return out;
}

SEXP rytest_cpp_unwind(SEXP call);
SEXP rytest_destructor_count(void);

static const R_CallMethodDef call_entries[] = {
    {"rytest_sum", (DL_FUNC) &rytest_sum, 1},
    {"rytest_make_list", (DL_FUNC) &rytest_make_list, 1},
    {"rytest_gc_churn", (DL_FUNC) &rytest_gc_churn, 1},
    {"rytest_unprotected_temporaries", (DL_FUNC) &rytest_unprotected_temporaries, 1},
    {"rytest_protect_leak", (DL_FUNC) &rytest_protect_leak, 0},
    {"rytest_unprotect_too_many", (DL_FUNC) &rytest_unprotect_too_many, 0},
    {"rytest_protect_overflow", (DL_FUNC) &rytest_protect_overflow, 0},
    {"rytest_protect_depth_error", (DL_FUNC) &rytest_protect_depth_error, 0},
    {"rytest_error", (DL_FUNC) &rytest_error, 1},
    {"rytest_errorcall", (DL_FUNC) &rytest_errorcall, 0},
    {"rytest_warning", (DL_FUNC) &rytest_warning, 0},
    {"rytest_eval", (DL_FUNC) &rytest_eval, 2},
    {"rytest_try_eval", (DL_FUNC) &rytest_try_eval, 1},
    {"rytest_unwind_protect", (DL_FUNC) &rytest_unwind_protect, 1},
    {"rytest_cleanup_state", (DL_FUNC) &rytest_cleanup_state, 0},
    {"rytest_toplevel", (DL_FUNC) &rytest_toplevel, 1},
    {"rytest_try_catch", (DL_FUNC) &rytest_try_catch, 1},
    {"rytest_exec_with_cleanup", (DL_FUNC) &rytest_exec_with_cleanup, 1},
    {"rytest_r_alloc", (DL_FUNC) &rytest_r_alloc, 1},
    {"rytest_r_alloc_error", (DL_FUNC) &rytest_r_alloc_error, 1},
    {"rytest_external_ptr", (DL_FUNC) &rytest_external_ptr, 1},
    {"rytest_external_value", (DL_FUNC) &rytest_external_value, 1},
    {"rytest_finalized", (DL_FUNC) &rytest_finalized, 0},
    {"rytest_sequence", (DL_FUNC) &rytest_sequence, 2},
    {"rytest_sequence_info", (DL_FUNC) &rytest_sequence_info, 1},
    {"rytest_call_callable", (DL_FUNC) &rytest_call_callable, 1},
    {"rytest_missing_callable", (DL_FUNC) &rytest_missing_callable, 0},
    {"rytest_strings", (DL_FUNC) &rytest_strings, 0},
    {"rytest_embedded_nul", (DL_FUNC) &rytest_embedded_nul, 0},
    {"rytest_coerce", (DL_FUNC) &rytest_coerce, 2},
    {"rytest_matrix", (DL_FUNC) &rytest_matrix, 2},
    {"rytest_environment", (DL_FUNC) &rytest_environment, 0},
    {"rytest_duplicate", (DL_FUNC) &rytest_duplicate, 1},
    {"rytest_identical", (DL_FUNC) &rytest_identical, 2},
    {"rytest_unif", (DL_FUNC) &rytest_unif, 1},
    {"rytest_sample", (DL_FUNC) &rytest_sample, 2},
    {"rytest_cpp_unwind", (DL_FUNC) &rytest_cpp_unwind, 1},
    {"rytest_destructor_count", (DL_FUNC) &rytest_destructor_count, 0},
    {NULL, NULL, 0}
};

void R_init_rytest(DllInfo *dll)
{
    R_registerRoutines(dll, NULL, call_entries, NULL, NULL);
    R_useDynamicSymbols(dll, FALSE);
    R_RegisterCCallable("rytest", "twice", (DL_FUNC) twice);

    sequence_class = R_make_altinteger_class("sequence", "rytest", dll);
    R_set_altrep_Length_method(sequence_class, sequence_length);
    R_set_altinteger_Elt_method(sequence_class, sequence_elt);
    R_set_altvec_Dataptr_method(sequence_class, sequence_dataptr);
    R_set_altvec_Dataptr_or_null_method(sequence_class, sequence_dataptr_or_null);
    R_set_altinteger_Is_sorted_method(sequence_class, sequence_is_sorted);
}
