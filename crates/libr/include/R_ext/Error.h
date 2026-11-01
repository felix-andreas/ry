#ifndef R_ERROR_H_
#define R_ERROR_H_

#ifdef __cplusplus
extern "C" {
#endif

/* C++ code places NORET where only an attribute of the GNU form is accepted (Rcpp writes
 * `inline void NORET stop(...)`), so C++ gets that form, and only C23 gets `[[noreturn]]`.
 * A package that defines NORET itself keeps its definition. */
#if defined(NORET)
#elif defined(__STDC_VERSION__) && __STDC_VERSION__ >= 202301L
#define NORET [[noreturn]]
#elif defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#define NORET _Noreturn
#elif defined(__GNUC__)
#define NORET __attribute__((noreturn))
#else
#define NORET
#endif

#define R_PRINTF_FORMAT(M, N) __attribute__((format(printf, M, N)))

#ifdef __cplusplus
[[noreturn]] void Rf_error(const char *, ...) R_PRINTF_FORMAT(1, 2);
[[noreturn]] void UNIMPLEMENTED(const char *);
[[noreturn]] void WrongArgCount(const char *);
#else
NORET void Rf_error(const char *, ...) R_PRINTF_FORMAT(1, 2);
NORET void UNIMPLEMENTED(const char *);
NORET void WrongArgCount(const char *);
#endif
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
