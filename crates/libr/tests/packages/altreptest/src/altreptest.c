/*
 * ALTREP classes of every kind, and routines that drive every accessor on them.
 *
 * `altreptest_report` exercises the classes and ry's or GNU R's built-in ones through the C
 * API only and returns one line per observation, so the same report built against GNU R's
 * headers and run by GNU R is the conformance expectation for ry (see expected.R).
 */

#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#include <R.h>
#include <Rinternals.h>
#include <R_ext/Altrep.h>
#include <R_ext/Rdynload.h>
#include <Rversion.h>

/* libR exports these dispatch functions without declaring them in a public header. */
SEXP ALTINTEGER_SUM(SEXP x, Rboolean narm);
SEXP ALTINTEGER_MIN(SEXP x, Rboolean narm);
SEXP ALTINTEGER_MAX(SEXP x, Rboolean narm);
SEXP ALTREAL_SUM(SEXP x, Rboolean narm);
SEXP ALTREAL_MIN(SEXP x, Rboolean narm);
SEXP ALTREAL_MAX(SEXP x, Rboolean narm);
SEXP ALTLOGICAL_SUM(SEXP x, Rboolean narm);

static R_altrep_class_t elt_only_class, pointer_class, string_class, list_class, logical_class,
    raw_class, complex_class, failing_class, failing_string_class, allocating_class;

static int elt_calls = 0;
static int region_calls = 0;
static int duplicate_calls = 0;

/* elt_only: an integer class with only Length and Elt, whose element i is 2 i. data1 holds the
   length as a double. */

static R_xlen_t elt_only_length(SEXP x)
{
    return (R_xlen_t) REAL(R_altrep_data1(x))[0];
}

static int elt_only_elt(SEXP x, R_xlen_t i)
{
    elt_calls++;
    return (int) (2 * i);
}

static int elt_only_elt_shifted(SEXP x, R_xlen_t i)
{
    return (int) (1000 + i);
}

/* pointer: a double class whose Dataptr materializes i + 0.5 into data2; it has no Elt, so
   elements come through the data pointer. */

static R_xlen_t pointer_length(SEXP x)
{
    return (R_xlen_t) REAL(R_altrep_data1(x))[0];
}

static void *pointer_dataptr(SEXP x, Rboolean writable)
{
    SEXP data = R_altrep_data2(x);
    if (data == R_NilValue) {
        R_xlen_t n = pointer_length(x);
        data = PROTECT(allocVector(REALSXP, n));
        for (R_xlen_t i = 0; i < n; i++)
            REAL(data)[i] = i + 0.5;
        R_set_altrep_data2(x, data);
        UNPROTECT(1);
    }
    return REAL(data);
}

static const void *pointer_dataptr_or_null(SEXP x)
{
    SEXP data = R_altrep_data2(x);
    return data == R_NilValue ? NULL : REAL_RO(data);
}

static int pointer_is_sorted(SEXP x)
{
    return SORTED_INCR;
}

static int pointer_no_na(SEXP x)
{
    return 1;
}

static SEXP pointer_sum(SEXP x, Rboolean narm)
{
    double n = (double) pointer_length(x);
    return ScalarReal(n * n / 2);
}

static SEXP pointer_min(SEXP x, Rboolean narm)
{
    return ScalarReal(0.5);
}

static SEXP pointer_max(SEXP x, Rboolean narm)
{
    return ScalarReal(pointer_length(x) - 0.5);
}

/* string: a character class over a character vector in data1, with Elt and Set_elt. */

static R_xlen_t string_length(SEXP x)
{
    return XLENGTH(R_altrep_data1(x));
}

static SEXP string_elt(SEXP x, R_xlen_t i)
{
    return STRING_ELT(R_altrep_data1(x), i);
}

static void string_set_elt(SEXP x, R_xlen_t i, SEXP v)
{
    SET_STRING_ELT(R_altrep_data1(x), i, v);
}

static int string_no_na(SEXP x)
{
    SEXP data = R_altrep_data1(x);
    for (R_xlen_t i = 0; i < XLENGTH(data); i++)
        if (STRING_ELT(data, i) == NA_STRING)
            return 0;
    return 1;
}

static int string_is_sorted(SEXP x)
{
    return KNOWN_UNSORTED;
}

/* list: a list class over a list in data1, whose Dataptr is the list's own storage. */

static R_xlen_t list_length(SEXP x)
{
    return XLENGTH(R_altrep_data1(x));
}

static SEXP list_elt(SEXP x, R_xlen_t i)
{
    return VECTOR_ELT(R_altrep_data1(x), i);
}

static void list_set_elt(SEXP x, R_xlen_t i, SEXP v)
{
    SET_VECTOR_ELT(R_altrep_data1(x), i, v);
}

static void *list_dataptr(SEXP x, Rboolean writable)
{
    return writable ? DATAPTR(R_altrep_data1(x)) : (void *) DATAPTR_RO(R_altrep_data1(x));
}

/* logical: TRUE, FALSE, TRUE, ... with its own region, sortedness, missing values and sum. */

static R_xlen_t logical_length(SEXP x)
{
    return (R_xlen_t) REAL(R_altrep_data1(x))[0];
}

static int logical_elt(SEXP x, R_xlen_t i)
{
    return i % 2 == 0;
}

static R_xlen_t logical_region(SEXP x, R_xlen_t start, R_xlen_t n, int *buffer)
{
    region_calls++;
    R_xlen_t size = logical_length(x);
    R_xlen_t count = size - start < n ? size - start : n;
    for (R_xlen_t k = 0; k < count; k++)
        buffer[k] = (start + k) % 2 == 0;
    return count;
}

static int logical_is_sorted(SEXP x)
{
    return logical_length(x) <= 1 ? SORTED_INCR : KNOWN_UNSORTED;
}

static int logical_no_na(SEXP x)
{
    return 1;
}

static SEXP logical_sum(SEXP x, Rboolean narm)
{
    return ScalarInteger((int) ((logical_length(x) + 1) / 2));
}

/* raw: bytes 7 i mod 256, with a region method. */

static R_xlen_t raw_length(SEXP x)
{
    return (R_xlen_t) REAL(R_altrep_data1(x))[0];
}

static Rbyte raw_elt(SEXP x, R_xlen_t i)
{
    return (Rbyte) ((7 * i) & 255);
}

static R_xlen_t raw_region(SEXP x, R_xlen_t start, R_xlen_t n, Rbyte *buffer)
{
    R_xlen_t size = raw_length(x);
    R_xlen_t count = size - start < n ? size - start : n;
    for (R_xlen_t k = 0; k < count; k++)
        buffer[k] = raw_elt(x, start + k);
    return count;
}

/* complex: i - i i, with only Elt. */

static R_xlen_t complex_length(SEXP x)
{
    return (R_xlen_t) REAL(R_altrep_data1(x))[0];
}

static Rcomplex complex_elt(SEXP x, R_xlen_t i)
{
    Rcomplex value;
    value.r = (double) i;
    value.i = -(double) i;
    return value;
}

/* failing: an integer class whose methods raise R errors. */

static R_xlen_t failing_length(SEXP x)
{
    return 6;
}

static int failing_elt(SEXP x, R_xlen_t i)
{
    if (i == 3)
        Rf_error("element %d is unavailable", (int) i);
    return (int) i;
}

static void *failing_dataptr(SEXP x, Rboolean writable)
{
    Rf_error("no data pointer for a failing vector");
    return NULL;
}

static SEXP failing_sum(SEXP x, Rboolean narm)
{
    Rf_error("cannot sum a failing vector");
    return R_NilValue;
}

/* failing_string: three strings whose second element cannot be read, so R code that asks for
   another element must not touch it. */
static R_xlen_t failing_string_length(SEXP x)
{
    return 3;
}

static SEXP failing_string_elt(SEXP x, R_xlen_t i)
{
    if (i == 1)
        Rf_error("string %d is unavailable", (int) i + 1);
    return Rf_mkChar("ok");
}

/* allocating: an integer class over data1 whose methods allocate, for collection stress; it
   has its own Duplicate and Coerce. */

static R_xlen_t allocating_length(SEXP x)
{
    SEXP garbage = PROTECT(allocVector(VECSXP, 2));
    SET_VECTOR_ELT(garbage, 0, mkChar("garbage"));
    UNPROTECT(1);
    return XLENGTH(R_altrep_data1(x));
}

static int allocating_elt(SEXP x, R_xlen_t i)
{
    SEXP garbage = PROTECT(allocVector(INTSXP, 3));
    INTEGER(garbage)[0] = INTEGER(R_altrep_data1(x))[i];
    UNPROTECT(1);
    return INTEGER(garbage)[0];
}

static void *allocating_dataptr(SEXP x, Rboolean writable)
{
    SEXP garbage = PROTECT(allocVector(STRSXP, 4));
    SET_STRING_ELT(garbage, 1, mkChar("more garbage"));
    UNPROTECT(1);
    return INTEGER(R_altrep_data1(x));
}

static SEXP allocating_duplicate(SEXP x, Rboolean deep)
{
    duplicate_calls++;
    SEXP data = PROTECT(duplicate(R_altrep_data1(x)));
    SEXP copy = R_new_altrep(allocating_class, data, R_NilValue);
    UNPROTECT(1);
    return copy;
}

static SEXP allocating_coerce(SEXP x, int type)
{
    if (type != REALSXP)
        return NULL;
    SEXP data = R_altrep_data1(x);
    SEXP result = PROTECT(allocVector(REALSXP, XLENGTH(data)));
    for (R_xlen_t i = 0; i < XLENGTH(data); i++)
        REAL(result)[i] = INTEGER(data)[i] + 0.25;
    UNPROTECT(1);
    return result;
}

/* Constructors. */

static SEXP sized(R_altrep_class_t class, R_xlen_t n)
{
    SEXP state = PROTECT(ScalarReal((double) n));
    SEXP x = R_new_altrep(class, state, R_NilValue);
    UNPROTECT(1);
    return x;
}

SEXP altreptest_make(SEXP kind, SEXP length)
{
    const char *name = CHAR(STRING_ELT(kind, 0));
    R_xlen_t n = (R_xlen_t) asReal(length);
    if (strcmp(name, "elt_only") == 0)
        return sized(elt_only_class, n);
    if (strcmp(name, "pointer") == 0)
        return sized(pointer_class, n);
    if (strcmp(name, "logical") == 0)
        return sized(logical_class, n);
    if (strcmp(name, "raw") == 0)
        return sized(raw_class, n);
    if (strcmp(name, "complex") == 0)
        return sized(complex_class, n);
    if (strcmp(name, "failing") == 0)
        return sized(failing_class, n);
    if (strcmp(name, "failing_string") == 0)
        return sized(failing_string_class, n);
    Rf_error("unknown class '%s'", name);
    return R_NilValue;
}

/* An ALTREP view of a copy of an ordinary vector: integer (allocating class), character or
   list. */
SEXP altreptest_view(SEXP x)
{
    SEXP data = PROTECT(duplicate(x));
    SEXP view;
    switch (TYPEOF(x)) {
    case INTSXP: view = R_new_altrep(allocating_class, data, R_NilValue); break;
    case STRSXP: view = R_new_altrep(string_class, data, R_NilValue); break;
    case VECSXP: view = R_new_altrep(list_class, data, R_NilValue); break;
    default: Rf_error("no view for this type");
    }
    UNPROTECT(1);
    return view;
}

/* Accessors, each returning what native code sees. */

SEXP altreptest_integer_elts(SEXP x)
{
    R_xlen_t n = XLENGTH(x);
    SEXP result = PROTECT(allocVector(INTSXP, n));
    for (R_xlen_t i = 0; i < n; i++)
        INTEGER(result)[i] = INTEGER_ELT(x, i);
    UNPROTECT(1);
    return result;
}

SEXP altreptest_integer_pointer(SEXP x)
{
    R_xlen_t n = XLENGTH(x);
    const int *data = INTEGER_RO(x);
    SEXP result = PROTECT(allocVector(INTSXP, n));
    memcpy(INTEGER(result), data, n * sizeof(int));
    UNPROTECT(1);
    return result;
}

SEXP altreptest_integer_region(SEXP x, SEXP start, SEXP count)
{
    R_xlen_t n = (R_xlen_t) asReal(count);
    int *buffer = (int *) R_alloc(n > 0 ? n : 1, sizeof(int));
    R_xlen_t copied = INTEGER_GET_REGION(x, (R_xlen_t) asReal(start), n, buffer);
    SEXP result = PROTECT(allocVector(INTSXP, copied));
    memcpy(INTEGER(result), buffer, copied * sizeof(int));
    UNPROTECT(1);
    return result;
}

SEXP altreptest_set_integer(SEXP x, SEXP index, SEXP value)
{
    SET_INTEGER_ELT(x, (R_xlen_t) asReal(index), asInteger(value));
    return x;
}

SEXP altreptest_string_elts(SEXP x)
{
    R_xlen_t n = XLENGTH(x);
    SEXP result = PROTECT(allocVector(STRSXP, n));
    for (R_xlen_t i = 0; i < n; i++)
        SET_STRING_ELT(result, i, STRING_ELT(x, i));
    UNPROTECT(1);
    return result;
}

SEXP altreptest_set_string(SEXP x, SEXP index, SEXP value)
{
    SET_STRING_ELT(x, (R_xlen_t) asReal(index), STRING_ELT(value, 0));
    return x;
}

SEXP altreptest_list_elts(SEXP x)
{
    R_xlen_t n = XLENGTH(x);
    SEXP result = PROTECT(allocVector(VECSXP, n));
    for (R_xlen_t i = 0; i < n; i++)
        SET_VECTOR_ELT(result, i, VECTOR_ELT(x, i));
    UNPROTECT(1);
    return result;
}

SEXP altreptest_set_list(SEXP x, SEXP index, SEXP value)
{
    SET_VECTOR_ELT(x, (R_xlen_t) asReal(index), value);
    return x;
}

SEXP altreptest_duplicate(SEXP x)
{
    return duplicate(x);
}

SEXP altreptest_shallow_duplicate(SEXP x)
{
    return shallow_duplicate(x);
}

SEXP altreptest_coerce(SEXP x, SEXP type)
{
    return coerceVector(x, (SEXPTYPE) asInteger(type));
}

SEXP altreptest_sorted(SEXP x)
{
    SEXP result = PROTECT(allocVector(INTSXP, 2));
    switch (TYPEOF(x)) {
    case INTSXP:
        INTEGER(result)[0] = INTEGER_IS_SORTED(x);
        INTEGER(result)[1] = INTEGER_NO_NA(x);
        break;
    case REALSXP:
        INTEGER(result)[0] = REAL_IS_SORTED(x);
        INTEGER(result)[1] = REAL_NO_NA(x);
        break;
    case LGLSXP:
        INTEGER(result)[0] = LOGICAL_IS_SORTED(x);
        INTEGER(result)[1] = LOGICAL_NO_NA(x);
        break;
    case STRSXP:
        INTEGER(result)[0] = STRING_IS_SORTED(x);
        INTEGER(result)[1] = STRING_NO_NA(x);
        break;
    default:
        Rf_error("no sortedness for this type");
    }
    UNPROTECT(1);
    return result;
}

SEXP altreptest_class(SEXP x)
{
    if (!ALTREP(x))
        return R_NilValue;
    SEXP info = ATTRIB(ALTREP_CLASS(x));
    SEXP result = PROTECT(allocVector(STRSXP, 3));
    SET_STRING_ELT(result, 0, PRINTNAME(CAR(info)));
    SET_STRING_ELT(result, 1, PRINTNAME(CADR(info)));
    SET_STRING_ELT(result, 2, mkChar(type2char(INTEGER(CADDR(info))[0])));
    UNPROTECT(1);
    return result;
}

SEXP altreptest_class_name(SEXP x)
{
    SEXP result = PROTECT(allocVector(VECSXP, 2));
#if R_VERSION >= R_Version(4, 6, 0)
    SET_VECTOR_ELT(result, 0, R_altrep_class_name(x));
    SET_VECTOR_ELT(result, 1, R_altrep_class_package(x));
#endif
    UNPROTECT(1);
    return result;
}

SEXP altreptest_data(SEXP x)
{
    SEXP result = PROTECT(allocVector(VECSXP, 2));
    SET_VECTOR_ELT(result, 0, R_altrep_data1(x));
    SET_VECTOR_ELT(result, 1, R_altrep_data2(x));
    UNPROTECT(1);
    return result;
}

SEXP altreptest_counters(void)
{
    SEXP result = PROTECT(allocVector(INTSXP, 3));
    INTEGER(result)[0] = elt_calls;
    INTEGER(result)[1] = region_calls;
    INTEGER(result)[2] = duplicate_calls;
    elt_calls = region_calls = duplicate_calls = 0;
    UNPROTECT(1);
    return result;
}

/* Re-registering a class under its name gives existing instances the new methods. */
SEXP altreptest_reregister(SEXP shifted)
{
    elt_only_class = R_make_altinteger_class("elt_only", "altreptest", NULL);
    R_set_altrep_Length_method(elt_only_class, elt_only_length);
    R_set_altinteger_Elt_method(elt_only_class,
                                asLogical(shifted) ? elt_only_elt_shifted : elt_only_elt);
    return R_NilValue;
}

/* Stores a fresh object through a writable list data pointer, which bypasses the write
   barrier, then collects: the object must survive. */
SEXP altreptest_store_through_pointer(SEXP x)
{
    SEXP *slots = (SEXP *) DATAPTR(x);
    slots[0] = mkString("stored through the pointer");
    R_gc();
    SEXP more = PROTECT(allocVector(VECSXP, 100));
    UNPROTECT(1);
    (void) more;
    R_gc();
    return VECTOR_ELT(x, 0);
}

/* A C NULL from a class without the method reads as R NULL. */
static SEXP or_nil(SEXP x)
{
    return x == NULL ? R_NilValue : x;
}

SEXP altreptest_sums(SEXP x)
{
    SEXP result = PROTECT(allocVector(VECSXP, 3));
    switch (TYPEOF(x)) {
    case INTSXP:
        SET_VECTOR_ELT(result, 0, or_nil(ALTINTEGER_SUM(x, TRUE)));
        SET_VECTOR_ELT(result, 1, or_nil(ALTINTEGER_MIN(x, TRUE)));
        SET_VECTOR_ELT(result, 2, or_nil(ALTINTEGER_MAX(x, TRUE)));
        break;
    case REALSXP:
        SET_VECTOR_ELT(result, 0, or_nil(ALTREAL_SUM(x, TRUE)));
        SET_VECTOR_ELT(result, 1, or_nil(ALTREAL_MIN(x, TRUE)));
        SET_VECTOR_ELT(result, 2, or_nil(ALTREAL_MAX(x, TRUE)));
        break;
    case LGLSXP:
        SET_VECTOR_ELT(result, 0, or_nil(ALTLOGICAL_SUM(x, TRUE)));
        break;
    }
    UNPROTECT(1);
    return result;
}

/* The conformance report. */

#define REPORT_LINES 400

static char report[REPORT_LINES][256];
static int report_count;

static void note(const char *format, ...)
{
    if (report_count >= REPORT_LINES)
        return;
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(report[report_count++], sizeof(report[0]), format, arguments);
    va_end(arguments);
}

typedef void (*Probe)(SEXP);

typedef struct {
    Probe probe;
    SEXP x;
} Attempt;

static SEXP run_attempt(void *data)
{
    Attempt *attempt = (Attempt *) data;
    attempt->probe(attempt->x);
    return R_NilValue;
}

static SEXP report_error(SEXP condition, void *data)
{
    note("  %s: error: %s", (const char *) data, CHAR(STRING_ELT(VECTOR_ELT(condition, 0), 0)));
    return R_NilValue;
}

/* Runs `probe(x)`, noting the error message instead if it raises one. */
static void attempt(const char *label, Probe probe, SEXP x)
{
    Attempt data = {probe, x};
    R_tryCatchError(run_attempt, &data, report_error, (void *) label);
}

static void note_integers(const char *label, SEXP x)
{
    char line[200] = "";
    R_xlen_t n = XLENGTH(x);
    for (R_xlen_t i = 0; i < n && i < 12; i++) {
        char item[24];
        int value = INTEGER_ELT(x, i);
        if (value == NA_INTEGER)
            snprintf(item, sizeof(item), "%sNA", i ? "," : "");
        else
            snprintf(item, sizeof(item), "%s%d", i ? "," : "", value);
        strncat(line, item, sizeof(line) - strlen(line) - 1);
    }
    note("  %s: [%s]", label, line);
}

static void note_reals(const char *label, SEXP x)
{
    char line[200] = "";
    R_xlen_t n = XLENGTH(x);
    for (R_xlen_t i = 0; i < n && i < 12; i++) {
        char item[32];
        snprintf(item, sizeof(item), "%s%g", i ? "," : "", REAL_ELT(x, i));
        strncat(line, item, sizeof(line) - strlen(line) - 1);
    }
    note("  %s: [%s]", label, line);
}

static void note_strings(const char *label, SEXP x)
{
    char line[200] = "";
    R_xlen_t n = XLENGTH(x);
    for (R_xlen_t i = 0; i < n && i < 12; i++) {
        SEXP element = STRING_ELT(x, i);
        char item[48];
        snprintf(item, sizeof(item), "%s%s", i ? "," : "",
                 element == NA_STRING ? "NA" : CHAR(element));
        strncat(line, item, sizeof(line) - strlen(line) - 1);
    }
    note("  %s: [%s]", label, line);
}

static void note_class(SEXP x)
{
    if (!ALTREP(x)) {
        note("  altrep: no");
        return;
    }
    SEXP info = ATTRIB(ALTREP_CLASS(x));
    note("  altrep: %s/%s %s, class handle %s", CHAR(PRINTNAME(CAR(info))),
         CHAR(PRINTNAME(CADR(info))), type2char(INTEGER(CADDR(info))[0]),
         type2char(TYPEOF(ALTREP_CLASS(x))));
}

static void note_sorted(SEXP x)
{
    SEXP sorted = PROTECT(altreptest_sorted(x));
    if (INTEGER(sorted)[0] == UNKNOWN_SORTEDNESS)
        note("  sorted: unknown, no_na: %d", INTEGER(sorted)[1]);
    else
        note("  sorted: %d, no_na: %d", INTEGER(sorted)[0], INTEGER(sorted)[1]);
    UNPROTECT(1);
}

static void probe_integer_pointer(SEXP x)
{
    note("  INTEGER[1]: %d", INTEGER(x)[1]);
}

static void probe_set_integer(SEXP x)
{
    SET_INTEGER_ELT(x, 1, 77);
    note("  after SET_INTEGER_ELT: %d", INTEGER_ELT(x, 1));
}

static void probe_duplicate(SEXP x)
{
    SEXP copy = PROTECT(duplicate(x));
    note_class(copy);
    switch (TYPEOF(copy)) {
    case INTSXP: note_integers("copy", copy); break;
    case REALSXP: note_reals("copy", copy); break;
    case STRSXP: note_strings("copy", copy); break;
    default: note("  copy length: %d", (int) XLENGTH(copy));
    }
    SEXP tag = getAttrib(copy, install("tag"));
    if (tag != R_NilValue)
        note("  copy keeps attribute tag = %s", CHAR(STRING_ELT(tag, 0)));
    UNPROTECT(1);
}

static void probe_region(SEXP x)
{
    int buffer[8];
    R_xlen_t copied = INTEGER_GET_REGION(x, 2, 8, buffer);
    char line[120] = "";
    for (R_xlen_t i = 0; i < copied; i++) {
        char item[16];
        snprintf(item, sizeof(item), "%s%d", i ? "," : "", buffer[i]);
        strncat(line, item, sizeof(line) - strlen(line) - 1);
    }
    note("  INTEGER_GET_REGION(2, 8): %d [%s]", (int) copied, line);
}

static void probe_elt_3(SEXP x)
{
    note("  INTEGER_ELT[3]: %d", INTEGER_ELT(x, 3));
}

static void probe_sum(SEXP x)
{
    SEXP sum = ALTINTEGER_SUM(x, TRUE);
    note("  ALTINTEGER_SUM: %s", sum == NULL ? "NULL" : "a value");
}

static void probe_string_pointer(SEXP x)
{
    note("  STRING_PTR_RO[1]: %s", CHAR(STRING_PTR_RO(x)[1]));
}

static void probe_coerce_string(SEXP x)
{
    SEXP converted = PROTECT(coerceVector(x, STRSXP));
    note_class(converted);
    note_strings("as character", converted);
    UNPROTECT(1);
}

static void probe_coerce_real(SEXP x)
{
    SEXP converted = PROTECT(coerceVector(x, REALSXP));
    note_class(converted);
    note_reals("as double", converted);
    UNPROTECT(1);
}

static void probe_coerce_integer(SEXP x)
{
    SEXP converted = PROTECT(coerceVector(x, INTSXP));
    note_class(converted);
    note_integers("as integer", converted);
    UNPROTECT(1);
}

static void probe_raw_pointer(SEXP x)
{
    note("  RAW[2]: %d", RAW(x)[2]);
}

SEXP altreptest_report(void)
{
    report_count = 0;
    altreptest_reregister(ScalarLogical(FALSE));

    note("elt_only, length 6");
    SEXP x = PROTECT(sized(elt_only_class, 6));
    note_class(x);
    note("  LENGTH: %d, XLENGTH: %d, length: %d", LENGTH(x), (int) XLENGTH(x), length(x));
    note_integers("INTEGER_ELT", x);
    note("  DATAPTR_OR_NULL: %s, INTEGER_OR_NULL: %s", DATAPTR_OR_NULL(x) ? "pointer" : "NULL",
         INTEGER_OR_NULL(x) ? "pointer" : "NULL");
    attempt("region", probe_region, x);
    note_sorted(x);
    note("  R_altrep_inherits: %d", R_altrep_inherits(x, elt_only_class));
    attempt("INTEGER", probe_integer_pointer, x);
    attempt("SET_INTEGER_ELT", probe_set_integer, x);
    attempt("sum", probe_sum, x);
    attempt("as double", probe_coerce_real, x);
    attempt("as character", probe_coerce_string, x);
    UNPROTECT(1);

    note("pointer, length 5");
    x = PROTECT(sized(pointer_class, 5));
    note_class(x);
    note("  DATAPTR_OR_NULL before: %s", DATAPTR_OR_NULL(x) ? "pointer" : "NULL");
    note_reals("REAL_ELT", x);
    note("  DATAPTR_OR_NULL after: %s, REAL_OR_NULL: %s", DATAPTR_OR_NULL(x) ? "pointer" : "NULL",
         REAL_OR_NULL(x) ? "pointer" : "NULL");
    note("  data2 materialized: %s", R_altrep_data2(x) == R_NilValue ? "no" : "yes");
    note_sorted(x);
    note("  sum %g, min %g, max %g", REAL(ALTREAL_SUM(x, TRUE))[0], REAL(ALTREAL_MIN(x, TRUE))[0],
         REAL(ALTREAL_MAX(x, TRUE))[0]);
    SET_REAL_ELT(x, 1, 9.0);
    note("  after SET_REAL_ELT: %g", REAL_ELT(x, 1));
    {
        double buffer[3];
        R_xlen_t copied = REAL_GET_REGION(x, 3, 3, buffer);
        note("  REAL_GET_REGION(3, 3): %d [%g,%g]", (int) copied, buffer[0], buffer[1]);
    }
    attempt("duplicate", probe_duplicate, x);
    attempt("as integer", probe_coerce_integer, x);
    attempt("as character", probe_coerce_string, x);
    UNPROTECT(1);

    note("string");
    SEXP source = PROTECT(allocVector(STRSXP, 3));
    SET_STRING_ELT(source, 0, mkChar("b"));
    SET_STRING_ELT(source, 1, mkChar("a"));
    SET_STRING_ELT(source, 2, NA_STRING);
    x = PROTECT(altreptest_view(source));
    note_class(x);
    note_strings("STRING_ELT", x);
    note_sorted(x);
    SET_STRING_ELT(x, 2, mkChar("c"));
    note_strings("after SET_STRING_ELT", x);
    note_sorted(x);
    note("  DATAPTR_OR_NULL: %s", DATAPTR_OR_NULL(x) ? "pointer" : "NULL");
    attempt("STRING_PTR_RO", probe_string_pointer, x);
    /* GNU R copies an atomic or string class through its data pointer, so a string class
       without Dataptr cannot be duplicated there; ry copies it through Elt instead. */
    UNPROTECT(2);

    note("list");
    source = PROTECT(allocVector(VECSXP, 2));
    SET_VECTOR_ELT(source, 0, ScalarInteger(1));
    SET_VECTOR_ELT(source, 1, mkString("two"));
    x = PROTECT(altreptest_view(source));
    note_class(x);
    note("  VECTOR_ELT[1]: %s", CHAR(STRING_ELT(VECTOR_ELT(x, 1), 0)));
    SET_VECTOR_ELT(x, 0, mkString("one"));
    note("  after SET_VECTOR_ELT: %s", CHAR(STRING_ELT(VECTOR_ELT(x, 0), 0)));
    note("  DATAPTR_RO[1]: %s", CHAR(STRING_ELT(((const SEXP *) DATAPTR_RO(x))[1], 0)));
    attempt("duplicate", probe_duplicate, x);
    UNPROTECT(2);

    note("logical, length 5");
    x = PROTECT(sized(logical_class, 5));
    note_class(x);
    note_integers("LOGICAL_ELT", x);
    {
        int buffer[4];
        R_xlen_t copied = LOGICAL_GET_REGION(x, 2, 4, buffer);
        note("  LOGICAL_GET_REGION(2, 4): %d [%d,%d,%d]", (int) copied, buffer[0], buffer[1],
             buffer[2]);
    }
    note_sorted(x);
    note("  sum: %d", INTEGER(ALTLOGICAL_SUM(x, TRUE))[0]);
    attempt("as integer", probe_coerce_integer, x);
    UNPROTECT(1);

    note("raw, length 4");
    x = PROTECT(sized(raw_class, 4));
    note_class(x);
    note("  RAW_ELT: %d %d %d %d", RAW_ELT(x, 0), RAW_ELT(x, 1), RAW_ELT(x, 2), RAW_ELT(x, 3));
    {
        Rbyte buffer[4];
        R_xlen_t copied = RAW_GET_REGION(x, 1, 4, buffer);
        note("  RAW_GET_REGION(1, 4): %d [%d,%d,%d]", (int) copied, buffer[0], buffer[1],
             buffer[2]);
    }
    note("  RAW_OR_NULL: %s", RAW_OR_NULL(x) ? "pointer" : "NULL");
    attempt("RAW", probe_raw_pointer, x);
    attempt("as integer", probe_coerce_integer, x);
    UNPROTECT(1);

    note("complex, length 3");
    x = PROTECT(sized(complex_class, 3));
    note_class(x);
    note("  COMPLEX_ELT[2]: %g%+gi", COMPLEX_ELT(x, 2).r, COMPLEX_ELT(x, 2).i);
    {
        Rcomplex buffer[3];
        R_xlen_t copied = COMPLEX_GET_REGION(x, 1, 5, buffer);
        note("  COMPLEX_GET_REGION(1, 5): %d [%g%+gi,%g%+gi]", (int) copied, buffer[0].r,
             buffer[0].i, buffer[1].r, buffer[1].i);
    }
    note("  COMPLEX_OR_NULL: %s", COMPLEX_OR_NULL(x) ? "pointer" : "NULL");
    attempt("as character", probe_coerce_string, x);
    UNPROTECT(1);

    note("failing");
    x = PROTECT(sized(failing_class, 6));
    note("  LENGTH: %d", LENGTH(x));
    attempt("INTEGER_ELT", probe_elt_3, x);
    attempt("region", probe_region, x);
    attempt("INTEGER", probe_integer_pointer, x);
    attempt("sum", probe_sum, x);
    UNPROTECT(1);

    note("allocating");
    source = PROTECT(allocVector(INTSXP, 4));
    for (int i = 0; i < 4; i++)
        INTEGER(source)[i] = 10 * (i + 1);
    x = PROTECT(altreptest_view(source));
    setAttrib(x, install("tag"), mkString("kept"));
    note_class(x);
    note_integers("INTEGER_ELT", x);
    attempt("duplicate", probe_duplicate, x);
    attempt("as double", probe_coerce_real, x);
    attempt("as character", probe_coerce_string, x);
    UNPROTECT(2);

    note("wrapper");
    source = PROTECT(allocVector(INTSXP, 3));
    INTEGER(source)[0] = 3;
    INTEGER(source)[1] = 1;
    INTEGER(source)[2] = 2;
    x = PROTECT(R_tryWrap(source));
    note_class(x);
    note_integers("INTEGER_ELT", x);
    note_sorted(x);
    {
        SEXP metadata = R_altrep_data2(x);
        note("  metadata: %s %d", type2char(TYPEOF(metadata)), (int) XLENGTH(metadata));
        note("  wraps the source: %s", R_altrep_data1(x) == source ? "yes" : "no");
    }
    note("  DATAPTR_OR_NULL: %s", DATAPTR_OR_NULL(x) ? "pointer" : "NULL");
    attempt("duplicate", probe_duplicate, x);
    attempt("SET_INTEGER_ELT", probe_set_integer, x);
    note_integers("source after the write", source);
    SEXP list = PROTECT(allocVector(VECSXP, 1));
    note("  R_tryWrap(list) is the list: %s", R_tryWrap(list) == list ? "yes" : "no");
    SEXP strings = PROTECT(mkString("s"));
    SEXP wrapped = PROTECT(R_tryWrap(strings));
    note_class(wrapped);
    UNPROTECT(5);

    note("deferred string");
    source = PROTECT(allocVector(INTSXP, 3));
    INTEGER(source)[0] = 10;
    INTEGER(source)[1] = NA_INTEGER;
    INTEGER(source)[2] = -4;
    x = PROTECT(coerceVector(source, STRSXP));
    note_class(x);
    note("  LENGTH: %d", LENGTH(x));
    note("  STRING_ELT[0]: %s", CHAR(STRING_ELT(x, 0)));
    note("  DATAPTR_OR_NULL: %s", DATAPTR_OR_NULL(x) ? "pointer" : "NULL");
    note_sorted(x);
    note_strings("STRING_ELT", x);
    note("  STRING_PTR_RO[2]: %s", CHAR(STRING_PTR_RO(x)[2]));
    note("  DATAPTR_OR_NULL after STRING_PTR_RO: %s", DATAPTR_OR_NULL(x) ? "pointer" : "NULL");
    SET_STRING_ELT(x, 0, mkChar("ten"));
    note_strings("after SET_STRING_ELT", x);
    UNPROTECT(2);
    source = PROTECT(allocVector(REALSXP, 4));
    REAL(source)[0] = 1.5;
    REAL(source)[1] = 100000;
    REAL(source)[2] = 1e-20;
    REAL(source)[3] = 123456789012.0;
    x = PROTECT(coerceVector(source, STRSXP));
    note_class(x);
    note_strings("doubles", x);
    note_sorted(x);
    UNPROTECT(2);
    source = PROTECT(allocVector(INTSXP, 2));
    setAttrib(source, R_NamesSymbol, mkString("a"));
    x = PROTECT(coerceVector(source, STRSXP));
    note("  with names:");
    note_class(x);
    UNPROTECT(2);
    source = PROTECT(allocVector(LGLSXP, 2));
    x = PROTECT(coerceVector(source, STRSXP));
    note("  from logical:");
    note_class(x);
    UNPROTECT(2);

    SEXP result = PROTECT(allocVector(STRSXP, report_count));
    for (int i = 0; i < report_count; i++)
        SET_STRING_ELT(result, i, mkChar(report[i]));
    UNPROTECT(1);
    return result;
}

#define CALL(name, n) {#name, (DL_FUNC) &name, n}

static const R_CallMethodDef call_entries[] = {
    CALL(altreptest_make, 2),
    CALL(altreptest_view, 1),
    CALL(altreptest_integer_elts, 1),
    CALL(altreptest_integer_pointer, 1),
    CALL(altreptest_integer_region, 3),
    CALL(altreptest_set_integer, 3),
    CALL(altreptest_string_elts, 1),
    CALL(altreptest_set_string, 3),
    CALL(altreptest_list_elts, 1),
    CALL(altreptest_set_list, 3),
    CALL(altreptest_duplicate, 1),
    CALL(altreptest_shallow_duplicate, 1),
    CALL(altreptest_coerce, 2),
    CALL(altreptest_sorted, 1),
    CALL(altreptest_class, 1),
    CALL(altreptest_class_name, 1),
    CALL(altreptest_data, 1),
    CALL(altreptest_counters, 0),
    CALL(altreptest_reregister, 1),
    CALL(altreptest_store_through_pointer, 1),
    CALL(altreptest_sums, 1),
    CALL(altreptest_report, 0),
    {NULL, NULL, 0}
};

void R_init_altreptest(DllInfo *dll)
{
    R_registerRoutines(dll, NULL, call_entries, NULL, NULL);
    R_useDynamicSymbols(dll, FALSE);

    elt_only_class = R_make_altinteger_class("elt_only", "altreptest", dll);
    R_set_altrep_Length_method(elt_only_class, elt_only_length);
    R_set_altinteger_Elt_method(elt_only_class, elt_only_elt);

    pointer_class = R_make_altreal_class("pointer", "altreptest", dll);
    R_set_altrep_Length_method(pointer_class, pointer_length);
    R_set_altvec_Dataptr_method(pointer_class, pointer_dataptr);
    R_set_altvec_Dataptr_or_null_method(pointer_class, pointer_dataptr_or_null);
    R_set_altreal_Is_sorted_method(pointer_class, pointer_is_sorted);
    R_set_altreal_No_NA_method(pointer_class, pointer_no_na);
    R_set_altreal_Sum_method(pointer_class, pointer_sum);
    R_set_altreal_Min_method(pointer_class, pointer_min);
    R_set_altreal_Max_method(pointer_class, pointer_max);

    string_class = R_make_altstring_class("string", "altreptest", dll);
    R_set_altrep_Length_method(string_class, string_length);
    R_set_altstring_Elt_method(string_class, string_elt);
    R_set_altstring_Set_elt_method(string_class, string_set_elt);
    R_set_altstring_No_NA_method(string_class, string_no_na);
    R_set_altstring_Is_sorted_method(string_class, string_is_sorted);

    list_class = R_make_altlist_class("list", "altreptest", dll);
    R_set_altrep_Length_method(list_class, list_length);
    R_set_altlist_Elt_method(list_class, list_elt);
    R_set_altlist_Set_elt_method(list_class, list_set_elt);
    R_set_altvec_Dataptr_method(list_class, list_dataptr);

    logical_class = R_make_altlogical_class("logical", "altreptest", dll);
    R_set_altrep_Length_method(logical_class, logical_length);
    R_set_altlogical_Elt_method(logical_class, logical_elt);
    R_set_altlogical_Get_region_method(logical_class, logical_region);
    R_set_altlogical_Is_sorted_method(logical_class, logical_is_sorted);
    R_set_altlogical_No_NA_method(logical_class, logical_no_na);
    R_set_altlogical_Sum_method(logical_class, logical_sum);

    raw_class = R_make_altraw_class("raw", "altreptest", dll);
    R_set_altrep_Length_method(raw_class, raw_length);
    R_set_altraw_Elt_method(raw_class, raw_elt);
    R_set_altraw_Get_region_method(raw_class, raw_region);

    complex_class = R_make_altcomplex_class("complex", "altreptest", dll);
    R_set_altrep_Length_method(complex_class, complex_length);
    R_set_altcomplex_Elt_method(complex_class, complex_elt);

    failing_class = R_make_altinteger_class("failing", "altreptest", dll);
    R_set_altrep_Length_method(failing_class, failing_length);
    R_set_altinteger_Elt_method(failing_class, failing_elt);
    R_set_altvec_Dataptr_method(failing_class, failing_dataptr);
    R_set_altinteger_Sum_method(failing_class, failing_sum);

    failing_string_class = R_make_altstring_class("failing_string", "altreptest", dll);
    R_set_altrep_Length_method(failing_string_class, failing_string_length);
    R_set_altstring_Elt_method(failing_string_class, failing_string_elt);

    allocating_class = R_make_altinteger_class("allocating", "altreptest", dll);
    R_set_altrep_Length_method(allocating_class, allocating_length);
    R_set_altinteger_Elt_method(allocating_class, allocating_elt);
    R_set_altvec_Dataptr_method(allocating_class, allocating_dataptr);
    R_set_altrep_Duplicate_method(allocating_class, allocating_duplicate);
    R_set_altrep_Coerce_method(allocating_class, allocating_coerce);
}
