#ifndef R_ERROR_H_
#define R_ERROR_H_

#ifdef __cplusplus
extern "C" {
#endif

#if defined(__cplusplus) && __cplusplus >= 201103L
#define NORET [[noreturn]]
#else
#define NORET __attribute__((noreturn))
#endif

#define R_PRINTF_FORMAT(M, N) __attribute__((format(printf, M, N)))

NORET void Rf_error(const char *, ...) R_PRINTF_FORMAT(1, 2);
void Rf_warning(const char *, ...) R_PRINTF_FORMAT(1, 2);
void R_ShowMessage(const char *);

#ifndef R_NO_REMAP
#define error Rf_error
#define warning Rf_warning
#endif

#ifdef __cplusplus
}
#endif

#endif
