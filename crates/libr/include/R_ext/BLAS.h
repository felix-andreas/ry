/*
 * ry's declaration of the BLAS routines R's API exposes, source-compatible with R 4.6.
 *
 * libR does not implement BLAS: a package links the system's BLAS through $(BLAS_LIBS). The
 * declarations follow the reference BLAS's Fortran calling convention, so every argument is
 * passed by pointer, and each CHARACTER argument has a hidden length that follows all the
 * other arguments. A declaration spells that length FCLEN and a call passes it as FCONE; both
 * are empty when FC_LEN_T is not defined (see Rconfig.h). Parameter names follow the
 * reference BLAS sources.
 */
#ifndef R_BLAS_H
#define R_BLAS_H

#include <Rconfig.h>
#include <R_ext/RS.h>

#ifdef FC_LEN_T
#include <stddef.h>
#define FCLEN , FC_LEN_T
#define FCONE , (FC_LEN_T) 1
#else
#define FCLEN
#define FCONE
#endif

/* A build against a BLAS with 64-bit integers defines ILP64 and its own BLAS_INT. */
#ifndef ILP64
#define BLAS_INT int
#endif

#ifndef BLAS_complex
#include <R_ext/Complex.h>
#define BLAS_complex Rcomplex
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* A package may define BLAS_extern first, for example to add a visibility attribute. */
#ifndef BLAS_extern
#define BLAS_extern extern
#endif

/* Double precision real routines. */

BLAS_extern double F77_NAME(dasum)(const BLAS_INT *n, const double *dx, const BLAS_INT *incx);
BLAS_extern void F77_NAME(daxpy)(const BLAS_INT *n, const double *da, const double *dx,
    const BLAS_INT *incx, double *dy, const BLAS_INT *incy);
BLAS_extern void F77_NAME(dcopy)(const BLAS_INT *n, const double *dx, const BLAS_INT *incx,
    double *dy, const BLAS_INT *incy);
BLAS_extern double F77_NAME(ddot)(const BLAS_INT *n, const double *dx, const BLAS_INT *incx,
    const double *dy, const BLAS_INT *incy);
BLAS_extern void F77_NAME(dgbmv)(const char *trans, const BLAS_INT *m, const BLAS_INT *n,
    const BLAS_INT *kl, const BLAS_INT *ku, const double *alpha, const double *a,
    const BLAS_INT *lda, const double *x, const BLAS_INT *incx, const double *beta, double *y,
    const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(dgemm)(const char *transa, const char *transb, const BLAS_INT *m,
    const BLAS_INT *n, const BLAS_INT *k, const double *alpha, const double *a, const BLAS_INT *lda,
    const double *b, const BLAS_INT *ldb, const double *beta, double *c,
    const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(dgemv)(const char *trans, const BLAS_INT *m, const BLAS_INT *n,
    const double *alpha, const double *a, const BLAS_INT *lda, const double *x,
    const BLAS_INT *incx, const double *beta, double *y, const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(dger)(const BLAS_INT *m, const BLAS_INT *n, const double *alpha,
    const double *x, const BLAS_INT *incx, const double *y, const BLAS_INT *incy, double *a,
    const BLAS_INT *lda);
BLAS_extern double F77_NAME(dnrm2)(const BLAS_INT *n, const double *x, const BLAS_INT *incx);
BLAS_extern void F77_NAME(drot)(const BLAS_INT *n, double *dx, const BLAS_INT *incx, double *dy,
    const BLAS_INT *incy, const double *c, const double *s);
BLAS_extern void F77_NAME(drotg)(const double *a, const double *b, double *c, double *s);
BLAS_extern void F77_NAME(drotm)(const BLAS_INT *n, double *dx, const BLAS_INT *incx, double *dy,
    const BLAS_INT *incy, const double *dparam);
BLAS_extern void F77_NAME(drotmg)(const double *dd1, const double *dd2, const double *dx1,
    const double *dy1, double *dparam);
BLAS_extern void F77_NAME(dsbmv)(const char *uplo, const BLAS_INT *n, const BLAS_INT *k,
    const double *alpha, const double *a, const BLAS_INT *lda, const double *x,
    const BLAS_INT *incx, const double *beta, double *y, const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(dscal)(const BLAS_INT *n, const double *da, double *dx,
    const BLAS_INT *incx);
BLAS_extern void F77_NAME(dspmv)(const char *uplo, const BLAS_INT *n, const double *alpha,
    const double *ap, const double *x, const BLAS_INT *incx, const double *beta, double *y,
    const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(dspr)(const char *uplo, const BLAS_INT *n, const double *alpha,
    const double *x, const BLAS_INT *incx, double *ap FCLEN);
BLAS_extern void F77_NAME(dspr2)(const char *uplo, const BLAS_INT *n, const double *alpha,
    const double *x, const BLAS_INT *incx, const double *y, const BLAS_INT *incy, double *ap FCLEN);
BLAS_extern void F77_NAME(dswap)(const BLAS_INT *n, double *dx, const BLAS_INT *incx, double *dy,
    const BLAS_INT *incy);
BLAS_extern void F77_NAME(dsymm)(const char *side, const char *uplo, const BLAS_INT *m,
    const BLAS_INT *n, const double *alpha, const double *a, const BLAS_INT *lda, const double *b,
    const BLAS_INT *ldb, const double *beta, double *c, const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(dsymv)(const char *uplo, const BLAS_INT *n, const double *alpha,
    const double *a, const BLAS_INT *lda, const double *x, const BLAS_INT *incx, const double *beta,
    double *y, const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(dsyr)(const char *uplo, const BLAS_INT *n, const double *alpha,
    const double *x, const BLAS_INT *incx, double *a, const BLAS_INT *lda FCLEN);
BLAS_extern void F77_NAME(dsyr2)(const char *uplo, const BLAS_INT *n, const double *alpha,
    const double *x, const BLAS_INT *incx, const double *y, const BLAS_INT *incy, double *a,
    const BLAS_INT *lda FCLEN);
BLAS_extern void F77_NAME(dsyr2k)(const char *uplo, const char *trans, const BLAS_INT *n,
    const BLAS_INT *k, const double *alpha, const double *a, const BLAS_INT *lda, const double *b,
    const BLAS_INT *ldb, const double *beta, double *c, const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(dsyrk)(const char *uplo, const char *trans, const BLAS_INT *n,
    const BLAS_INT *k, const double *alpha, const double *a, const BLAS_INT *lda,
    const double *beta, double *c, const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(dtbmv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const BLAS_INT *k, const double *a, const BLAS_INT *lda, double *x,
    const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(dtbsv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const BLAS_INT *k, const double *a, const BLAS_INT *lda, double *x,
    const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(dtpmv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const double *ap, double *x, const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(dtpsv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const double *ap, double *x, const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(dtrmm)(const char *side, const char *uplo, const char *transa,
    const char *diag, const BLAS_INT *m, const BLAS_INT *n, const double *alpha, const double *a,
    const BLAS_INT *lda, double *b, const BLAS_INT *ldb FCLEN FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(dtrmv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const double *a, const BLAS_INT *lda, double *x,
    const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(dtrsm)(const char *side, const char *uplo, const char *transa,
    const char *diag, const BLAS_INT *m, const BLAS_INT *n, const double *alpha, const double *a,
    const BLAS_INT *lda, double *b, const BLAS_INT *ldb FCLEN FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(dtrsv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const double *a, const BLAS_INT *lda, double *x,
    const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern BLAS_INT F77_NAME(idamax)(const BLAS_INT *n, const double *dx, const BLAS_INT *incx);

/* Double precision complex routines. */

BLAS_extern double F77_NAME(dcabs1)(const BLAS_complex *z);
BLAS_extern double F77_NAME(dzasum)(const BLAS_INT *n, const BLAS_complex *zx,
    const BLAS_INT *incx);
BLAS_extern double F77_NAME(dznrm2)(const BLAS_INT *n, const BLAS_complex *x, const BLAS_INT *incx);
BLAS_extern BLAS_INT F77_NAME(izamax)(const BLAS_INT *n, const BLAS_complex *zx,
    const BLAS_INT *incx);
BLAS_extern void F77_NAME(zaxpy)(const BLAS_INT *n, const BLAS_complex *za, const BLAS_complex *zx,
    const BLAS_INT *incx, const BLAS_complex *zy, const BLAS_INT *incy);
BLAS_extern void F77_NAME(zcopy)(const BLAS_INT *n, const BLAS_complex *zx, const BLAS_INT *incx,
    const BLAS_complex *zy, const BLAS_INT *incy);
BLAS_extern BLAS_complex F77_NAME(zdotc)(const BLAS_INT *n, const BLAS_complex *zx,
    const BLAS_INT *incx, const BLAS_complex *zy, const BLAS_INT *incy);
BLAS_extern BLAS_complex F77_NAME(zdotu)(const BLAS_INT *n, const BLAS_complex *zx,
    const BLAS_INT *incx, const BLAS_complex *zy, const BLAS_INT *incy);
BLAS_extern void F77_NAME(zdrot)(const BLAS_INT *n, const BLAS_complex *zx, const BLAS_INT *incx,
    BLAS_complex *zy, const BLAS_INT *incy, const double *c, const double *s);
BLAS_extern void F77_NAME(zdscal)(const BLAS_INT *n, const double *da, BLAS_complex *zx,
    const BLAS_INT *incx);
BLAS_extern void F77_NAME(zgbmv)(const char *trans, BLAS_INT *m, BLAS_INT *n, BLAS_INT *kl,
    BLAS_INT *ku, BLAS_complex *alpha, BLAS_complex *a, BLAS_INT *lda, BLAS_complex *x,
    BLAS_INT *incx, BLAS_complex *beta, BLAS_complex *y, BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(zgemm)(const char *transa, const char *transb, const BLAS_INT *m,
    const BLAS_INT *n, const BLAS_INT *k, const BLAS_complex *alpha, const BLAS_complex *a,
    const BLAS_INT *lda, const BLAS_complex *b, const BLAS_INT *ldb, const BLAS_complex *beta,
    BLAS_complex *c, const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(zgemv)(const char *trans, const BLAS_INT *m, const BLAS_INT *n,
    const BLAS_complex *alpha, const BLAS_complex *a, const BLAS_INT *lda, const BLAS_complex *x,
    const BLAS_INT *incx, const BLAS_complex *beta, BLAS_complex *y, const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(zgerc)(const BLAS_INT *m, const BLAS_INT *n, const BLAS_complex *alpha,
    const BLAS_complex *x, const BLAS_INT *incx, const BLAS_complex *y, const BLAS_INT *incy,
    BLAS_complex *a, const BLAS_INT *lda);
BLAS_extern void F77_NAME(zgeru)(const BLAS_INT *m, const BLAS_INT *n, const BLAS_complex *alpha,
    const BLAS_complex *x, const BLAS_INT *incx, const BLAS_complex *y, const BLAS_INT *incy,
    BLAS_complex *a, const BLAS_INT *lda);
BLAS_extern void F77_NAME(zhbmv)(const char *uplo, const BLAS_INT *n, const BLAS_INT *k,
    const BLAS_complex *alpha, const BLAS_complex *a, const BLAS_INT *lda, const BLAS_complex *x,
    const BLAS_INT *incx, const BLAS_complex *beta, BLAS_complex *y, const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(zhemm)(const char *side, const char *uplo, const BLAS_INT *m,
    const BLAS_INT *n, const BLAS_complex *alpha, const BLAS_complex *a, const BLAS_INT *lda,
    const BLAS_complex *b, const BLAS_INT *ldb, const BLAS_complex *beta, BLAS_complex *c,
    const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(zhemv)(const char *uplo, const BLAS_INT *n, const BLAS_complex *alpha,
    const BLAS_complex *a, const BLAS_INT *lda, const BLAS_complex *x, const BLAS_INT *incx,
    const BLAS_complex *beta, BLAS_complex *y, const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(zher)(const char *uplo, const BLAS_INT *n, const double *alpha,
    const BLAS_complex *x, const BLAS_INT *incx, BLAS_complex *a, const BLAS_INT *lda FCLEN);
BLAS_extern void F77_NAME(zher2)(const char *uplo, const BLAS_INT *n, const BLAS_complex *alpha,
    const BLAS_complex *x, const BLAS_INT *incx, const BLAS_complex *y, const BLAS_INT *incy,
    BLAS_complex *a, const BLAS_INT *lda FCLEN);
BLAS_extern void F77_NAME(zher2k)(const char *uplo, const char *trans, const BLAS_INT *n,
    const BLAS_INT *k, const BLAS_complex *alpha, const BLAS_complex *a, const BLAS_INT *lda,
    const BLAS_complex *b, const BLAS_INT *ldb, const double *beta, BLAS_complex *c,
    const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(zherk)(const char *uplo, const char *trans, const BLAS_INT *n,
    const BLAS_INT *k, const double *alpha, const BLAS_complex *a, const BLAS_INT *lda,
    const double *beta, BLAS_complex *c, const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(zhpmv)(const char *uplo, const BLAS_INT *n, const BLAS_complex *alpha,
    const BLAS_complex *ap, const BLAS_complex *x, const BLAS_INT *incx, const BLAS_complex *beta,
    BLAS_complex *y, const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(zhpr)(const char *uplo, const BLAS_INT *n, const double *alpha,
    const BLAS_complex *x, const BLAS_INT *incx, BLAS_complex *ap FCLEN);
BLAS_extern void F77_NAME(zhpr2)(const char *uplo, const BLAS_INT *n, const BLAS_complex *alpha,
    const BLAS_complex *x, const BLAS_INT *incx, const BLAS_complex *y, const BLAS_INT *incy,
    BLAS_complex *ap FCLEN);
BLAS_extern void F77_NAME(zrotg)(const BLAS_complex *a, const BLAS_complex *b, double *c,
    BLAS_complex *s);
BLAS_extern void F77_NAME(zscal)(const BLAS_INT *n, const BLAS_complex *za, BLAS_complex *zx,
    const BLAS_INT *incx);
BLAS_extern void F77_NAME(zspmv)(const char *uplo, const BLAS_INT *n, const BLAS_complex *alpha,
    const BLAS_complex *ap, const BLAS_complex *x, const BLAS_INT *incx, const BLAS_complex *beta,
    BLAS_complex *y, const BLAS_INT *incy FCLEN);
BLAS_extern void F77_NAME(zswap)(const BLAS_INT *n, BLAS_complex *zx, const BLAS_INT *incx,
    BLAS_complex *zy, const BLAS_INT *incy);
BLAS_extern void F77_NAME(zsymm)(const char *side, const char *uplo, const BLAS_INT *m,
    const BLAS_INT *n, const BLAS_complex *alpha, const BLAS_complex *a, const BLAS_INT *lda,
    const BLAS_complex *b, const BLAS_INT *ldb, const BLAS_complex *beta, BLAS_complex *c,
    const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(zsyr2k)(const char *uplo, const char *trans, BLAS_INT *n, BLAS_INT *k,
    BLAS_complex *alpha, BLAS_complex *a, BLAS_INT *lda, BLAS_complex *b, BLAS_INT *ldb,
    BLAS_complex *beta, BLAS_complex *c, BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(zsyrk)(const char *uplo, const char *trans, const BLAS_INT *n,
    const BLAS_INT *k, const BLAS_complex *alpha, const BLAS_complex *a, const BLAS_INT *lda,
    const BLAS_complex *beta, BLAS_complex *c, const BLAS_INT *ldc FCLEN FCLEN);
BLAS_extern void F77_NAME(ztbmv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const BLAS_INT *k, const BLAS_complex *a, const BLAS_INT *lda,
    BLAS_complex *x, const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(ztbsv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const BLAS_INT *k, const BLAS_complex *a, const BLAS_INT *lda,
    BLAS_complex *x, const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(ztpmv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const BLAS_complex *ap, BLAS_complex *x,
    const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(ztpsv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const BLAS_complex *ap, BLAS_complex *x,
    const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(ztrmm)(const char *side, const char *uplo, const char *transa,
    const char *diag, const BLAS_INT *m, const BLAS_INT *n, const BLAS_complex *alpha,
    const BLAS_complex *a, const BLAS_INT *lda, BLAS_complex *b,
    const BLAS_INT *ldb FCLEN FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(ztrmv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const BLAS_complex *a, const BLAS_INT *lda, BLAS_complex *x,
    const BLAS_INT *incx FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(ztrsm)(const char *side, const char *uplo, const char *transa,
    const char *diag, BLAS_INT *m, BLAS_INT *n, BLAS_complex *alpha, BLAS_complex *a, BLAS_INT *lda,
    BLAS_complex *b, BLAS_INT *ldb FCLEN FCLEN FCLEN FCLEN);
BLAS_extern void F77_NAME(ztrsv)(const char *uplo, const char *trans, const char *diag,
    const BLAS_INT *n, const BLAS_complex *a, const BLAS_INT *lda, BLAS_complex *x,
    const BLAS_INT *incx FCLEN FCLEN FCLEN);
#ifdef __cplusplus
}
#endif

#endif
