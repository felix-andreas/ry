#ifndef R_EXT_UTILS_H_
#define R_EXT_UTILS_H_

#include <R_ext/Boolean.h>
#include <R_ext/Complex.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void R_CheckUserInterrupt(void);
void R_CheckStack(void);
void R_CheckStack2(size_t);

void R_isort(int *, int);
void R_rsort(double *, int);
void rsort_with_index(double *, int *, int);
void revsort(double *, int *, int);
void Rf_iPsort(int *, int, int);
void Rf_rPsort(double *, int, int);

double R_atof(const char *);
double R_strtod(const char *, char **);

#ifndef R_NO_REMAP
#define iPsort Rf_iPsort
#define rPsort Rf_rPsort
#endif

#ifdef __cplusplus
}
#endif

#endif
