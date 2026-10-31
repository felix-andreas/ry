#ifndef R_APPLIC_H_
#define R_APPLIC_H_

/*
 * Numerical routines from R's own library: adaptive quadrature, the optimizers behind optim()
 * and nlm(), interval search, and R's QR decomposition with limited column pivoting.
 */

#include <R_ext/Boolean.h>
#include <R_ext/RS.h>
#include <R_ext/BLAS.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * The integrand receives n abscissae in x and must overwrite each with the integrand's value
 * there; ex is passed through untouched.
 */
typedef void integr_fn(double *x, int n, void *ex);

/* QUADPACK dqags: a finite interval [a, b]. iwork has limit ints, work has lenw >= 4 * limit. */
void Rdqags(integr_fn f, void *ex, double *a, double *b,
            double *epsabs, double *epsrel,
            double *result, double *abserr, int *neval, int *ier,
            int *limit, int *lenw, int *last, int *iwork, double *work);

/* QUADPACK dqagi: (bound, Inf) for inf = 1, (-Inf, bound) for inf = -1, (-Inf, Inf) for 2. */
void Rdqagi(integr_fn f, void *ex, double *bound, int *inf,
            double *epsabs, double *epsrel,
            double *result, double *abserr, int *neval, int *ier,
            int *limit, int *lenw, int *last,
            int *iwork, double *work);

/* The objective and its gradient for the optim() family. */
typedef double optimfn(int, double *, void *);
typedef void optimgr(int, double *, double *, void *);

/* Variable metric (BFGS); mask[i] == 0 holds parameter i fixed. */
void vmmin(int n, double *b, double *Fmin,
           optimfn fn, optimgr gr, int maxit, int trace,
           int *mask, double abstol, double reltol, int nREPORT,
           void *ex, int *fncount, int *grcount, int *fail);
/* Nelder-Mead simplex search from Bvec; the minimizer is stored in X. */
void nmmin(int n, double *Bvec, double *X, double *Fmin, optimfn fn,
           int *fail, double abstol, double intol, void *ex,
           double alpha, double bet, double gamm, int trace,
           int *fncount, int maxit);
/* Conjugate gradients: type 1 Fletcher-Reeves, 2 Polak-Ribiere, 3 Beale-Sorenson. */
void cgmin(int n, double *Bvec, double *X, double *Fmin,
           optimfn fn, optimgr gr,
           int *fail, double abstol, double intol, void *ex,
           int type, int trace, int *fncount, int *grcount, int maxit);
/*
 * Limited-memory BFGS with bounds. nbd[i] is 0 (unbounded), 1 (lower only), 2 (both) or 3
 * (upper only); msg receives the termination message and needs room for 60 characters.
 */
void lbfgsb(int n, int m, double *x, double *l, double *u, int *nbd,
            double *Fmin, optimfn fn, optimgr gr, int *fail, void *ex,
            double factr, double pgtol, int *fncount, int *grcount,
            int maxit, char *msg, int trace, int nREPORT);
/* Simulated annealing with Gaussian candidate steps, drawing from R's generator. */
void samin(int n, double *pb, double *yb, optimfn fn, int maxit,
           int tmax, double ti, int trace, void *ex);

int findInterval(double *xt, int n, double x,
                 Rboolean rightmost_closed, Rboolean all_inside, int ilo,
                 int *mflag);

/* Applications of a QR decomposition from dqrdc2 to the ny columns of y. */
void F77_NAME(dqrqty)(double *x, int *n, int *k, double *qraux,
                      double *y, int *ny, double *qty);
void F77_NAME(dqrqy)(double *x, int *n, int *k, double *qraux,
                     double *y, int *ny, double *qy);
void F77_NAME(dqrcf)(double *x, int *n, int *k, double *qraux,
                     double *y, int *ny, double *b, int *info);
void F77_NAME(dqrrsd)(double *x, int *n, int *k, double *qraux,
                      double *y, int *ny, double *rsd);
void F77_NAME(dqrxb)(double *x, int *n, int *k, double *qraux,
                     double *y, int *ny, double *xb);

/* The tick computation behind pretty(). */
double R_pretty(double *lo, double *up, int *ndiv, int min_n,
                double shrink_sml, const double high_u_fact[],
                int eps_correction, int return_bounds);

/* Dennis and Schnabel's UNCMIN, the engine of nlm(). */
typedef void (*fcn_p)(int, double *, double *, void *);
typedef void (*d2fcn_p)(int, int, double *, double *, void *);

void fdhess(int n, double *x, double fval, fcn_p fun, void *state,
            double *h, int nfd, double *step, double *f, int ndigit,
            double *typx);

void optif9(int nr, int n, double *x,
            fcn_p fcn, fcn_p d1fcn, d2fcn_p d2fcn,
            void *state, double *typsiz, double fscale, int method,
            int iexp, int *msg, int ndigit, int itnlim, int iagflg,
            int iahflg, double dlt, double gradtl, double stepmx,
            double steptl, double *xpls, double *fpls, double *gpls,
            int *itrmcd, double *a, double *wrk, int *itncnt);

/* LINPACK QR with R's limited pivoting: negligible columns move to the right. */
void F77_NAME(dqrdc2)(double *x, int *ldx, int *n, int *p,
                      double *tol, int *rank,
                      double *qraux, int *pivot, double *work);
/* Least squares fit of the ny columns of y on x, the core of lm.fit(). */
void F77_NAME(dqrls)(double *x, int *n, int *p, double *y, int *ny,
                     double *tol, double *b, double *rsd,
                     double *qty, int *k,
                     int *jpvt, double *qraux, double *work);

#ifdef __cplusplus
}
#endif

#endif
