/* Prints the layout of the serialization stream structs and the format codes, so a test can
 * compare ry's headers with GNU R's. */
#include <stddef.h>
#include <stdio.h>
#include <Rinternals.h>

#define FIELD(type, field)                                                                     \
    printf(#type "." #field " %zu %zu\n", offsetof(struct type, field),                       \
           sizeof(((struct type *) 0)->field))

int main(void)
{
    FIELD(R_outpstream_st, data);
    FIELD(R_outpstream_st, type);
    FIELD(R_outpstream_st, version);
    FIELD(R_outpstream_st, OutChar);
    FIELD(R_outpstream_st, OutBytes);
    FIELD(R_outpstream_st, OutPersistHookFunc);
    FIELD(R_outpstream_st, OutPersistHookData);
    FIELD(R_inpstream_st, data);
    FIELD(R_inpstream_st, type);
    FIELD(R_inpstream_st, InChar);
    FIELD(R_inpstream_st, InBytes);
    FIELD(R_inpstream_st, InPersistHookFunc);
    FIELD(R_inpstream_st, InPersistHookData);
    FIELD(R_inpstream_st, native_encoding);
    FIELD(R_inpstream_st, nat2nat_obj);
    FIELD(R_inpstream_st, nat2utf8_obj);
    printf("R_outpstream_st %zu\n", sizeof(struct R_outpstream_st));
    printf("R_inpstream_st %zu\n", sizeof(struct R_inpstream_st));
    printf("R_CODESET_MAX %d\n", R_CODESET_MAX);
    printf("formats %d %d %d %d %d\n", R_pstream_any_format, R_pstream_ascii_format,
           R_pstream_binary_format, R_pstream_xdr_format, R_pstream_asciihex_format);
    return 0;
}
