#ifndef R_EXT_UTILS_H_
#define R_EXT_UTILS_H_

/* Sorting, interval search, file-name and string helpers, and the interrupt and stack checks. */

#include <R_ext/Boolean.h>
#include <R_ext/Complex.h>

#if defined(__cplusplus) && !defined(DO_NOT_USE_CXX_HEADERS)
# include <cstddef>
# define R_SIZE_T std::size_t
#else
# include <stddef.h>
# define R_SIZE_T size_t
#endif

/* The library exports these under their Rf_ names. */
#define revsort       Rf_revsort
#define iPsort        Rf_iPsort
#define rPsort        Rf_rPsort
#define cPsort        Rf_cPsort
#define StringFalse   Rf_StringFalse
#define StringTrue    Rf_StringTrue
#define isBlankString Rf_isBlankString

#ifdef __cplusplus
extern "C" {
#endif

/* Ascending sorts with NA and NaN last. */
void R_isort(int *, int);
void R_rsort(double *, int);
void R_csort(Rcomplex *, int);
void rsort_with_index(double *, int *, int);
/* Descending sort of the values, permuting the index vector alongside. */
void revsort(double *, int *, int);
/* Partial sorts: element k (0-based) ends up where a full sort would put it. */
void iPsort(int *, int, int);
void rPsort(double *, int, int);
void cPsort(Rcomplex *, int, int);

/* Quicksort of the 1-based inclusive range i..j, optionally permuting I alongside. */
void R_qsort(double *v, R_SIZE_T i, R_SIZE_T j);
void R_qsort_I(double *v, int *I, int i, int j);
void R_qsort_int(int *iv, R_SIZE_T i, R_SIZE_T j);
void R_qsort_int_I(int *iv, int *I, int i, int j);
#ifdef R_RS_H
void F77_NAME(qsort4)(double *v, int *indx, int *ii, int *jj);
void F77_NAME(qsort3)(double *v, int *ii, int *jj);
#endif

#ifdef R_RS_H
/* Machine constants for Fortran code, in the classic PORT numbering. */
int F77_SUB(i1mach)(int *i);
double F77_SUB(d1mach)(int *i);
#endif

/* Expands a leading ~ or ~user; the result lives in a static buffer. */
const char *R_ExpandFileName(const char *);

Rboolean StringFalse(const char *);
Rboolean StringTrue(const char *);
Rboolean isBlankString(const char *);

/* Locale-independent: '.' is the decimal point, and "NA", "Inf", "NaN" and hex are read. */
double R_atof(const char *str);
double R_strtod(const char *c, char **end);

/* A fresh name for a file in tempdir; free it with R_free_tmpnam. */
char *R_tmpnam(const char *prefix, const char *tempdir);
char *R_tmpnam2(const char *prefix, const char *tempdir, const char *fileext);
void R_free_tmpnam(char *name);

void R_CheckUserInterrupt(void);
void R_CheckStack(void);
void R_CheckStack2(R_SIZE_T);

/* The interval of the sorted xt[0..n-1] that holds x, as findInterval() in R computes it. */
int findInterval(double *xt, int n, double x,
                 Rboolean rightmost_closed, Rboolean all_inside, int ilo,
                 int *mflag);
int findInterval2(double *xt, int n, double x,
                  Rboolean rightmost_closed, Rboolean all_inside, Rboolean left_open,
                  int ilo, int *mflag);
#ifdef R_RS_H
int F77_SUB(interv)(double *xt, int *n, double *x,
                    int *rightmost_closed, int *all_inside,
                    int *ilo, int *mflag);
#endif

/* The column of each row's maximum; ties_meth is 1 (random), 2 (first) or 3 (last). */
void R_max_col(double *matrix, int *nr, int *nc, int *maxes, int *ties_meth);

#ifdef __cplusplus
}
#endif

#endif
