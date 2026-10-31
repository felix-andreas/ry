/* A test package for the connections API. It builds against ry's headers and against GNU R's,
   so the same routines record GNU R's behavior and check ry's:

   - a custom connection backed by memory, created the way curl and processx create theirs,
     with read and write methods that move at most `chunk` bytes per call like a pipe would;
   - the readers and writers that vroom, readr, xml2 and brio-style code use on any connection:
     R_GetConnection, then R_ReadConnection or R_WriteConnection in a loop;
   - direct calls through the struct's method pointers, which packages also make. */

#include <R.h>
#include <Rinternals.h>
#include <R_ext/Connections.h>
#include <R_ext/Rdynload.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

#if R_CONNECTIONS_VERSION != 1
#error "this package knows version 1 of the connections interface"
#endif

typedef struct {
    unsigned char *data;
    size_t length, capacity, position;
    size_t chunk;
    /* 0 works; 1 fails in read; 2 fails in open; 3 fails in write. */
    int failure;
} Memory;

static int destroyed = 0;

static Rboolean memory_open(Rconnection con)
{
    Memory *memory = con->private;
    if (memory->failure == 2)
        Rf_error("custom open failed");
    if (con->mode[0] == 'w')
        memory->length = 0;
    memory->position = con->mode[0] == 'a' ? memory->length : 0;
    con->isopen = TRUE;
    con->canread = con->mode[0] == 'r' || strchr(con->mode, '+') != NULL;
    con->canwrite = con->mode[0] != 'r' || strchr(con->mode, '+') != NULL;
    con->text = strchr(con->mode, 'b') == NULL;
    return TRUE;
}

static void memory_close(Rconnection con)
{
    con->isopen = FALSE;
}

static void memory_destroy(Rconnection con)
{
    Memory *memory = con->private;
    free(memory->data);
    free(memory);
    destroyed++;
}

static size_t memory_read(void *buffer, size_t size, size_t n, Rconnection con)
{
    Memory *memory = con->private;
    if (memory->failure == 1)
        Rf_error("custom read failed");
    size_t wanted = size * n;
    size_t available = memory->length - memory->position;
    if (wanted > available)
        wanted = available;
    if (memory->chunk > 0 && wanted > memory->chunk)
        wanted = memory->chunk;
    memcpy(buffer, memory->data + memory->position, wanted);
    memory->position += wanted;
    return size == 1 ? wanted : wanted / size;
}

/* Only fgetc_internal is set, as curl does: the default fgetc reaches it. */
static int memory_fgetc(Rconnection con)
{
    unsigned char byte;
    return memory_read(&byte, 1, 1, con) == 1 ? byte : -1;
}

static size_t memory_write(const void *buffer, size_t size, size_t n, Rconnection con)
{
    Memory *memory = con->private;
    if (memory->failure == 3)
        Rf_error("custom write failed");
    size_t bytes = size * n;
    if (memory->length + bytes > memory->capacity) {
        size_t capacity = (memory->length + bytes) * 2 + 16;
        unsigned char *data = realloc(memory->data, capacity);
        if (data == NULL)
            Rf_error("out of memory");
        memory->data = data;
        memory->capacity = capacity;
    }
    memcpy(memory->data + memory->length, buffer, bytes);
    memory->length += bytes;
    return n;
}

/* ryconn_new(description, mode, contents, chunk, failure): a memory connection holding the raw
   vector `contents`, not yet open. */
SEXP ryconn_new(SEXP description, SEXP mode, SEXP contents, SEXP chunk, SEXP failure)
{
    Rconnection con;
    SEXP object = PROTECT(R_new_custom_connection(CHAR(STRING_ELT(description, 0)),
                                                  CHAR(STRING_ELT(mode, 0)), "ryconn", &con));
    Memory *memory = calloc(1, sizeof(Memory));
    if (memory == NULL)
        Rf_error("out of memory");
    memory->length = memory->capacity = XLENGTH(contents);
    memory->data = malloc(memory->capacity + 1);
    if (memory->length > 0)
        memcpy(memory->data, RAW(contents), memory->length);
    memory->chunk = (size_t) asInteger(chunk);
    memory->failure = asInteger(failure);
    con->private = memory;
    con->open = memory_open;
    con->close = memory_close;
    con->destroy = memory_destroy;
    con->read = memory_read;
    con->write = memory_write;
    con->fgetc_internal = memory_fgetc;
    con->canseek = FALSE;
    UNPROTECT(1);
    return object;
}

/* A custom connection whose methods are all the defaults. */
SEXP ryconn_bare(SEXP mode)
{
    Rconnection con;
    return R_new_custom_connection("bare", CHAR(STRING_ELT(mode, 0)), "ryconn", &con);
}

/* Marks a bare custom connection open without calling a method, as some packages do. */
SEXP ryconn_mark_open(SEXP object)
{
    Rconnection con = R_GetConnection(object);
    con->isopen = TRUE;
    return R_NilValue;
}

SEXP ryconn_destroyed(void)
{
    return ScalarInteger(destroyed);
}

SEXP ryconn_contents(SEXP object)
{
    Memory *memory = R_GetConnection(object)->private;
    SEXP result = allocVector(RAWSXP, memory->length);
    if (memory->length > 0)
        memcpy(RAW(result), memory->data, memory->length);
    return result;
}

/* One R_ReadConnection call of up to n bytes. */
SEXP ryconn_read(SEXP object, SEXP n)
{
    Rconnection con = R_GetConnection(object);
    size_t wanted = (size_t) asInteger(n);
    unsigned char *buffer = (unsigned char *) R_alloc(wanted + 1, 1);
    size_t got = R_ReadConnection(con, buffer, wanted);
    SEXP result = allocVector(RAWSXP, got);
    if (got > 0)
        memcpy(RAW(result), buffer, got);
    return result;
}

/* Reads everything in chunks until a read returns 0, as vroom and readr do. */
SEXP ryconn_read_all(SEXP object, SEXP chunk)
{
    Rconnection con = R_GetConnection(object);
    size_t size = (size_t) asInteger(chunk);
    unsigned char *buffer = (unsigned char *) R_alloc(size, 1);
    size_t total = 0, capacity = 64;
    unsigned char *all = malloc(capacity);
    for (;;) {
        size_t got = R_ReadConnection(con, buffer, size);
        if (got == 0)
            break;
        if (total + got > capacity) {
            capacity = (total + got) * 2;
            all = realloc(all, capacity);
        }
        memcpy(all + total, buffer, got);
        total += got;
    }
    SEXP result = allocVector(RAWSXP, total);
    if (total > 0)
        memcpy(RAW(result), all, total);
    free(all);
    return result;
}

/* Writes a raw vector in chunks of `chunk` bytes, as xml2's output callback does. */
SEXP ryconn_write(SEXP object, SEXP bytes, SEXP chunk)
{
    Rconnection con = R_GetConnection(object);
    size_t size = (size_t) asInteger(chunk), total = 0, length = XLENGTH(bytes);
    while (total < length) {
        size_t piece = length - total < size ? length - total : size;
        total += R_WriteConnection(con, RAW(bytes) + total, piece);
    }
    return ScalarInteger((int) total);
}

static int call_vfprintf(Rconnection con, const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int result = con->vfprintf(con, format, arguments);
    va_end(arguments);
    return result;
}

/* Prints "<text>=<number>\n" through the connection's vfprintf method. */
SEXP ryconn_printf(SEXP object, SEXP text, SEXP number)
{
    Rconnection con = R_GetConnection(object);
    return ScalarInteger(call_vfprintf(con, "%s=%d\n", CHAR(STRING_ELT(text, 0)), asInteger(number)));
}

/* Calls the fgetc method n times. */
SEXP ryconn_fgetc(SEXP object, SEXP n)
{
    Rconnection con = R_GetConnection(object);
    int count = asInteger(n);
    SEXP result = PROTECT(allocVector(INTSXP, count));
    for (int i = 0; i < count; i++)
        INTEGER(result)[i] = con->fgetc(con);
    UNPROTECT(1);
    return result;
}

/* Calls the seek method: where < 0 queries. */
SEXP ryconn_seek(SEXP object, SEXP where)
{
    Rconnection con = R_GetConnection(object);
    double position = asReal(where);
    return ScalarReal(con->seek(con, position < 0 ? NA_REAL : position, 1, 0));
}

/* The struct's plain fields, as a named list. */
SEXP ryconn_fields(SEXP object)
{
    Rconnection con = R_GetConnection(object);
    const char *names[] = {"class", "description", "mode", "text", "isopen", "canread",
                           "canwrite", "canseek", "blocking", "incomplete", "has_vfprintf"};
    int count = sizeof names / sizeof names[0];
    SEXP result = PROTECT(allocVector(VECSXP, count));
    SEXP result_names = PROTECT(allocVector(STRSXP, count));
    for (int i = 0; i < count; i++)
        SET_STRING_ELT(result_names, i, mkChar(names[i]));
    SET_VECTOR_ELT(result, 0, mkString(con->class));
    SET_VECTOR_ELT(result, 1, mkString(con->description));
    SET_VECTOR_ELT(result, 2, mkString(con->mode));
    SET_VECTOR_ELT(result, 3, ScalarLogical(con->text));
    SET_VECTOR_ELT(result, 4, ScalarLogical(con->isopen));
    SET_VECTOR_ELT(result, 5, ScalarLogical(con->canread));
    SET_VECTOR_ELT(result, 6, ScalarLogical(con->canwrite));
    SET_VECTOR_ELT(result, 7, ScalarLogical(con->canseek));
    SET_VECTOR_ELT(result, 8, ScalarLogical(con->blocking));
    SET_VECTOR_ELT(result, 9, ScalarLogical(con->incomplete));
    SET_VECTOR_ELT(result, 10, ScalarLogical(con->vfprintf != NULL));
    setAttrib(result, R_NamesSymbol, result_names);
    UNPROTECT(2);
    return result;
}

/* Calls one default method of a bare custom connection by name, to observe its behavior. */
SEXP ryconn_call_method(SEXP object, SEXP method)
{
    Rconnection con = R_GetConnection(object);
    const char *name = CHAR(STRING_ELT(method, 0));
    char buffer[8];
    if (strcmp(name, "open") == 0)
        return ScalarLogical(con->open(con));
    if (strcmp(name, "close") == 0) {
        con->close(con);
        return ScalarLogical(con->isopen);
    }
    if (strcmp(name, "fgetc") == 0)
        return ScalarInteger(con->fgetc(con));
    if (strcmp(name, "fgetc_internal") == 0)
        return ScalarInteger(con->fgetc_internal(con));
    if (strcmp(name, "seek") == 0)
        return ScalarReal(con->seek(con, NA_REAL, 1, 0));
    if (strcmp(name, "truncate") == 0) {
        con->truncate(con);
        return R_NilValue;
    }
    if (strcmp(name, "fflush") == 0)
        return ScalarInteger(con->fflush(con));
    if (strcmp(name, "read") == 0)
        return ScalarInteger((int) con->read(buffer, 1, sizeof buffer, con));
    if (strcmp(name, "write") == 0)
        return ScalarInteger((int) con->write("abc", 1, 3, con));
    Rf_error("no method %s", name);
    return R_NilValue;
}

static const R_CallMethodDef call_entries[] = {
    {"ryconn_new", (DL_FUNC) &ryconn_new, 5},
    {"ryconn_bare", (DL_FUNC) &ryconn_bare, 1},
    {"ryconn_mark_open", (DL_FUNC) &ryconn_mark_open, 1},
    {"ryconn_destroyed", (DL_FUNC) &ryconn_destroyed, 0},
    {"ryconn_contents", (DL_FUNC) &ryconn_contents, 1},
    {"ryconn_read", (DL_FUNC) &ryconn_read, 2},
    {"ryconn_read_all", (DL_FUNC) &ryconn_read_all, 2},
    {"ryconn_write", (DL_FUNC) &ryconn_write, 3},
    {"ryconn_printf", (DL_FUNC) &ryconn_printf, 3},
    {"ryconn_fgetc", (DL_FUNC) &ryconn_fgetc, 2},
    {"ryconn_seek", (DL_FUNC) &ryconn_seek, 2},
    {"ryconn_fields", (DL_FUNC) &ryconn_fields, 1},
    {"ryconn_call_method", (DL_FUNC) &ryconn_call_method, 2},
    {NULL, NULL, 0}};

void R_init_ryconn(DllInfo *dll)
{
    R_registerRoutines(dll, NULL, call_entries, NULL, NULL);
    R_useDynamicSymbols(dll, FALSE);
}
