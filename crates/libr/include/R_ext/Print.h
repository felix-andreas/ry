#ifndef R_EXT_PRINT_H_
#define R_EXT_PRINT_H_

#include <stdarg.h>

#ifdef __cplusplus
extern "C" {
#endif

#ifndef R_PRINTF_FORMAT
#define R_PRINTF_FORMAT(M, N) __attribute__((format(printf, M, N)))
#endif

void Rprintf(const char *, ...) R_PRINTF_FORMAT(1, 2);
void REprintf(const char *, ...) R_PRINTF_FORMAT(1, 2);
void Rvprintf(const char *, va_list) R_PRINTF_FORMAT(1, 0);
void REvprintf(const char *, va_list) R_PRINTF_FORMAT(1, 0);

#ifdef __cplusplus
}
#endif

#endif
