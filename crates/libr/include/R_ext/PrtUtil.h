/* R_ext/PrtUtil.h: GNU R's number formatting and vector printing, as native code reaches it.
 * The format functions compute the common field of a vector the way print() lays it out; the
 * Encode functions render one element in that field and return a buffer the next call to the
 * same function overwrites. */
#ifndef R_EXT_PRTUTIL_H_
#define R_EXT_PRTUTIL_H_

#include <Rinternals.h>
#include <R_ext/Complex.h>
#include <R_ext/Print.h>

#define formatLogical Rf_formatLogical
#define formatInteger Rf_formatInteger
#define formatReal Rf_formatReal
#define formatComplex Rf_formatComplex
#define EncodeLogical Rf_EncodeLogical
#define EncodeInteger Rf_EncodeInteger
#define EncodeReal Rf_EncodeReal
#define EncodeReal0 Rf_EncodeReal0
#define EncodeComplex Rf_EncodeComplex
#define IndexWidth Rf_IndexWidth
#define VectorIndex Rf_VectorIndex
#define printIntegerVector Rf_printIntegerVector
#define printRealVector Rf_printRealVector
#define printComplexVector Rf_printComplexVector

#ifdef __cplusplus
extern "C" {
#endif

void formatLogical(const int *x, R_xlen_t n, int *fieldwidth);
void formatInteger(const int *x, R_xlen_t n, int *fieldwidth);
void formatReal(const double *x, R_xlen_t n, int *w, int *d, int *e, int nsmall);
void formatComplex(const Rcomplex *x, R_xlen_t n, int *wr, int *dr, int *er, int *wi, int *di,
                   int *ei, int nsmall);
void formatLogicalS(SEXP x, R_xlen_t n, int *fieldwidth);
void formatIntegerS(SEXP x, R_xlen_t n, int *fieldwidth);
void formatRealS(SEXP x, R_xlen_t n, int *w, int *d, int *e, int nsmall);
void formatComplexS(SEXP x, R_xlen_t n, int *wr, int *dr, int *er, int *wi, int *di, int *ei,
                    int nsmall);
const char *EncodeLogical(int x, int w);
const char *EncodeInteger(int x, int w);
const char *EncodeReal0(double x, int w, int d, int e, const char *dec);
const char *EncodeComplex(Rcomplex x, int wr, int dr, int er, int wi, int di, int ei,
                          const char *dec);
const char *EncodeReal(double x, int w, int d, int e, char cdec);
int IndexWidth(R_xlen_t n);
void VectorIndex(R_xlen_t i, int w);
void printIntegerVector(const int *x, R_xlen_t n, int indx);
void printRealVector(const double *x, R_xlen_t n, int indx);
void printComplexVector(const Rcomplex *x, R_xlen_t n, int indx);
void printIntegerVectorS(SEXP x, R_xlen_t n, int indx);
void printRealVectorS(SEXP x, R_xlen_t n, int indx);
void printComplexVectorS(SEXP x, R_xlen_t n, int indx);

#ifdef __cplusplus
}
#endif

#endif
