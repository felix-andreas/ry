#ifndef R_LINPACK_H_
#define R_LINPACK_H_

/*
 * The double precision LINPACK subroutines R's library provides, callable from C and Fortran.
 * Every argument is passed by reference, as Fortran passes it.
 */

#include <R_ext/RS.h>
#include <R_ext/BLAS.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Cholesky factorization with optional diagonal pivoting. */
extern void F77_NAME(dchdc)(double *, int *, int *, double *, int *, int *, int *);
/* Banded positive definite: factor and solve. */
extern void F77_NAME(dpbfa)(double *, int *, int *, int *, int *);
extern void F77_NAME(dpbsl)(double *, int *, int *, int *, double *);
/* Positive definite: factor with condition estimate, determinant and inverse, factor, solve. */
extern void F77_NAME(dpoco)(double *, int *, int *, double *, double *, int *);
extern void F77_NAME(dpodi)(double *, int *, int *, double *, int *);
extern void F77_NAME(dpofa)(double *, int *, int *, int *);
extern void F77_NAME(dposl)(double *, int *, int *, double *);
/* QR decomposition with optional column pivoting, and its applications. */
extern void F77_NAME(dqrdc)(double *, int *, int *, int *, double *, int *, double *, int *);
extern void F77_NAME(dqrsl)(double *, int *, int *, int *, double *, double *, double *,
                            double *, double *, double *, double *, int *, int *);
/* Singular value decomposition. */
extern void F77_NAME(dsvdc)(double *, int *, int *, int *, double *, double *, double *, int *,
                            double *, int *, double *, int *, int *);
/* Triangular: condition estimate and solve. */
extern void F77_NAME(dtrco)(double *, int *, int *, double *, double *, int *);
extern void F77_NAME(dtrsl)(double *, int *, int *, double *, int *, int *);

#ifdef __cplusplus
}
#endif

#endif
