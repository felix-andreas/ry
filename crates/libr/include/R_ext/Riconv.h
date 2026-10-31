#ifndef R_EXT_RICONV_H_
#define R_EXT_RICONV_H_

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void *Riconv_open(const char *tocode, const char *fromcode);
size_t Riconv(void *cd, const char **inbuf, size_t *inbytesleft, char **outbuf, size_t *outbytesleft);
int Riconv_close(void *cd);

#ifdef __cplusplus
}
#endif

#endif
