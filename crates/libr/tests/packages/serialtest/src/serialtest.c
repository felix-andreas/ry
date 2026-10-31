/* Drives R's serialization stream API the way packages do (qs, fst, RApiSerialize): through
 * R_InitOutPStream and R_InitInPStream with callbacks over a memory buffer or a FILE, with and
 * without persistence hooks. */

#include <R.h>
#include <Rinternals.h>
#include <R_ext/Rdynload.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    unsigned char *bytes;
    size_t length;
    size_t capacity;
    size_t position;
} Buffer;

/* The buffer is a malloc'd block, because R_alloc memory cannot grow in place; buffer_to_raw
 * frees it. */
static void out_bytes(R_outpstream_t stream, void *bytes, int length)
{
    Buffer *buffer = (Buffer *) stream->data;
    if (buffer->length + (size_t) length > buffer->capacity) {
        size_t capacity = buffer->capacity ? buffer->capacity : 256;
        while (capacity < buffer->length + (size_t) length)
            capacity *= 2;
        unsigned char *grown = (unsigned char *) realloc(buffer->bytes, capacity);
        if (grown == NULL)
            error("out of memory");
        buffer->bytes = grown;
        buffer->capacity = capacity;
    }
    memcpy(buffer->bytes + buffer->length, bytes, (size_t) length);
    buffer->length += (size_t) length;
}

static void out_char(R_outpstream_t stream, int c)
{
    unsigned char byte = (unsigned char) c;
    out_bytes(stream, &byte, 1);
}

static void in_bytes(R_inpstream_t stream, void *bytes, int length)
{
    Buffer *buffer = (Buffer *) stream->data;
    if (buffer->position + (size_t) length > buffer->length)
        error("serialtest: read past the end of the buffer");
    memcpy(bytes, buffer->bytes + buffer->position, (size_t) length);
    buffer->position += (size_t) length;
}

static int in_char(R_inpstream_t stream)
{
    Buffer *buffer = (Buffer *) stream->data;
    if (buffer->position >= buffer->length)
        return -1;
    return buffer->bytes[buffer->position++];
}

static SEXP buffer_to_raw(Buffer *buffer)
{
    SEXP raw = allocVector(RAWSXP, (R_xlen_t) buffer->length);
    if (buffer->length)
        memcpy(RAW(raw), buffer->bytes, buffer->length);
    free(buffer->bytes);
    buffer->bytes = NULL;
    return raw;
}

SEXP serialtest_serialize(SEXP object, SEXP format, SEXP version)
{
    Buffer buffer = {0};
    struct R_outpstream_st stream;
    R_InitOutPStream(&stream, &buffer, (R_pstream_format_t) asInteger(format),
                     asInteger(version), out_char, out_bytes, NULL, R_NilValue);
    R_Serialize(object, &stream);
    return buffer_to_raw(&buffer);
}

SEXP serialtest_unserialize(SEXP raw, SEXP format)
{
    Buffer buffer = {RAW(raw), (size_t) XLENGTH(raw), 0, 0};
    struct R_inpstream_st stream;
    R_InitInPStream(&stream, &buffer, (R_pstream_format_t) asInteger(format), in_char,
                    in_bytes, NULL, R_NilValue);
    return R_Unserialize(&stream);
}

/* The format and native encoding an input stream reports after reading. */
SEXP serialtest_stream_info(SEXP raw)
{
    Buffer buffer = {RAW(raw), (size_t) XLENGTH(raw), 0, 0};
    struct R_inpstream_st stream;
    R_InitInPStream(&stream, &buffer, R_pstream_any_format, in_char, in_bytes, NULL,
                    R_NilValue);
    R_Unserialize(&stream);
    SEXP info = PROTECT(allocVector(STRSXP, 2));
    char type[16];
    snprintf(type, sizeof type, "%d", (int) stream.type);
    SET_STRING_ELT(info, 0, mkChar(type));
    SET_STRING_ELT(info, 1, mkChar(stream.native_encoding));
    UNPROTECT(1);
    return info;
}

/* Writing, an external pointer is replaced by the name in its tag; reading, a name becomes a
 * fresh external pointer tagged with it. */
static SEXP name_pointer(SEXP object, SEXP data)
{
    if (TYPEOF(object) != EXTPTRSXP)
        return R_NilValue;
    INTEGER(data)[0]++;
    return ScalarString(PRINTNAME(R_ExternalPtrTag(object)));
}

static SEXP restore_pointer(SEXP name, SEXP data)
{
    INTEGER(data)[0]++;
    return R_MakeExternalPtr(NULL, installChar(STRING_ELT(name, 0)), R_NilValue);
}

/* Serializes list(p, p, q) for two pointers and reads it back through the hooks. Returns the
 * restored list, with the number of hook calls on each side as an attribute. */
SEXP serialtest_hooks(void)
{
    SEXP p = PROTECT(R_MakeExternalPtr(&p, install("first"), R_NilValue));
    SEXP q = PROTECT(R_MakeExternalPtr(&q, install("second"), R_NilValue));
    SEXP list = PROTECT(allocVector(VECSXP, 3));
    SET_VECTOR_ELT(list, 0, p);
    SET_VECTOR_ELT(list, 1, p);
    SET_VECTOR_ELT(list, 2, q);
    SEXP counts = PROTECT(allocVector(INTSXP, 2));
    INTEGER(counts)[0] = INTEGER(counts)[1] = 0;
    SEXP written = PROTECT(ScalarInteger(0));
    SEXP read = PROTECT(ScalarInteger(0));

    Buffer buffer = {0};
    struct R_outpstream_st out;
    R_InitOutPStream(&out, &buffer, R_pstream_xdr_format, 3, NULL, out_bytes, name_pointer,
                     written);
    R_Serialize(list, &out);
    SEXP raw = PROTECT(buffer_to_raw(&buffer));

    Buffer input = {RAW(raw), (size_t) XLENGTH(raw), 0, 0};
    struct R_inpstream_st in;
    R_InitInPStream(&in, &input, R_pstream_xdr_format, NULL, in_bytes, restore_pointer, read);
    SEXP restored = PROTECT(R_Unserialize(&in));
    INTEGER(counts)[0] = INTEGER(written)[0];
    INTEGER(counts)[1] = INTEGER(read)[0];
    setAttrib(restored, install("counts"), counts);
    UNPROTECT(8);
    return restored;
}

static SEXP failing_hook(SEXP object, SEXP data)
{
    error("the hook refused %s", type2char(TYPEOF(object)));
    return R_NilValue;
}

SEXP serialtest_failing_hook(SEXP object)
{
    Buffer buffer = {0};
    struct R_outpstream_st stream;
    R_InitOutPStream(&stream, &buffer, R_pstream_xdr_format, 2, NULL, out_bytes, failing_hook,
                     R_NilValue);
    R_Serialize(object, &stream);
    return buffer_to_raw(&buffer);
}

/* Writes the object to a file and reads it back through the FILE streams. */
SEXP serialtest_file(SEXP object, SEXP path, SEXP format)
{
    const char *name = CHAR(STRING_ELT(path, 0));
    FILE *file = fopen(name, "wb");
    if (file == NULL)
        error("cannot open %s", name);
    struct R_outpstream_st out;
    R_InitFileOutPStream(&out, file, (R_pstream_format_t) asInteger(format), 0, NULL,
                         R_NilValue);
    R_Serialize(object, &out);
    fclose(file);
    file = fopen(name, "rb");
    if (file == NULL)
        error("cannot open %s", name);
    struct R_inpstream_st in;
    R_InitFileInPStream(&in, file, R_pstream_any_format, NULL, R_NilValue);
    SEXP result = R_Unserialize(&in);
    fclose(file);
    return result;
}

static const R_CallMethodDef call_entries[] = {
    {"serialtest_serialize", (DL_FUNC) &serialtest_serialize, 3},
    {"serialtest_unserialize", (DL_FUNC) &serialtest_unserialize, 2},
    {"serialtest_stream_info", (DL_FUNC) &serialtest_stream_info, 1},
    {"serialtest_hooks", (DL_FUNC) &serialtest_hooks, 0},
    {"serialtest_failing_hook", (DL_FUNC) &serialtest_failing_hook, 1},
    {"serialtest_file", (DL_FUNC) &serialtest_file, 3},
    {NULL, NULL, 0}
};

void R_init_serialtest(DllInfo *dll)
{
    R_registerRoutines(dll, NULL, call_entries, NULL, NULL);
    R_useDynamicSymbols(dll, FALSE);
}
