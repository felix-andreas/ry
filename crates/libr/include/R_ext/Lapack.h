/*
 * ry's declaration of the LAPACK routines R's API exposes, source-compatible with R 4.6.
 *
 * libR does not implement LAPACK: a package links the system's LAPACK through
 * $(LAPACK_LIBS) $(BLAS_LIBS). The calling convention is the one R_ext/BLAS.h describes,
 * including FCLEN and FCONE for the hidden lengths of CHARACTER arguments. Parameter names
 * follow the reference LAPACK sources.
 *
 * Where R 4.6's header gets a routine's interface wrong, these declarations follow the
 * reference routine instead, because a call written against the wrong interface corrupts the
 * routine's arguments: dgges (R omits sdim), the stale argument lists R gives dlaed2, dlar1v,
 * dlarrb, dlarre, dlarrf, dlarrv and dlasq2 to dlasq5, the pointer-to-pointer arguments R
 * gives several INTEGER arrays, and the pointer results R gives izmax1 and lsame. The number
 * of hidden lengths always follows R 4.6, so calls written with FCONE compile unchanged.
 */
#ifndef R_LAPACK_H
#define R_LAPACK_H

#include <R_ext/RS.h>
#include <R_ext/BLAS.h>

/* Keep in step with BLAS_INT in R_ext/BLAS.h. LOGICAL arguments are Fortran default
   LOGICALs, which every supported compiler passes as int. */
#ifndef ILP64
#define La_INT int
#define La_LGL int
#endif

#ifndef La_complex
#include <R_ext/Complex.h>
#define La_complex Rcomplex
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* A package may define La_extern first, for example to add a visibility attribute. */
#ifndef La_extern
#define La_extern extern
#endif

/* The eigenvalue selector dggesx takes. */
typedef La_INT *(*L_fp)(double *, double *, double *);

/*
 * R 4.6 declares dspgv with three hidden lengths and dtptrs with two, the reverse of the
 * reference routines (two CHARACTER arguments and three). Calls written against those
 * declarations keep compiling; a package that defines usePR18534fix gets the corrected ones.
 * Rconfig.h defines PR18534fixed so a package can tell which headers it has.
 */

La_extern void F77_NAME(dbdsdc)(const char *uplo, const char *compq, La_INT *n, double *d,
    double *e, double *u, La_INT *ldu, double *vt, La_INT *ldvt, double *q, La_INT *iq,
    double *work, La_INT *iwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dbdsqr)(const char *uplo, const La_INT *n, const La_INT *ncvt,
    const La_INT *nru, const La_INT *ncc, double *d, double *e, double *vt, const La_INT *ldvt,
    double *u, const La_INT *ldu, double *c, const La_INT *ldc, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(ddisna)(const char *job, const La_INT *m, const La_INT *n, double *d,
    double *sep, La_INT *info FCLEN);
La_extern void F77_NAME(dgbbrd)(const char *vect, const La_INT *m, const La_INT *n,
    const La_INT *ncc, const La_INT *kl, const La_INT *ku, double *ab, const La_INT *ldab,
    double *d, double *e, double *q, const La_INT *ldq, double *pt, const La_INT *ldpt, double *c,
    const La_INT *ldc, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dgbcon)(const char *norm, const La_INT *n, const La_INT *kl,
    const La_INT *ku, double *ab, const La_INT *ldab, La_INT *ipiv, const double *anorm,
    double *rcond, double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dgbequ)(const La_INT *m, const La_INT *n, const La_INT *kl,
    const La_INT *ku, double *ab, const La_INT *ldab, double *r, double *c, double *rowcnd,
    double *colcnd, double *amax, La_INT *info);
La_extern void F77_NAME(dgbrfs)(const char *trans, const La_INT *n, const La_INT *kl,
    const La_INT *ku, const La_INT *nrhs, double *ab, const La_INT *ldab, double *afb,
    const La_INT *ldafb, La_INT *ipiv, double *b, const La_INT *ldb, double *x, const La_INT *ldx,
    double *ferr, double *berr, double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dgbsv)(const La_INT *n, const La_INT *kl, const La_INT *ku,
    const La_INT *nrhs, double *ab, const La_INT *ldab, La_INT *ipiv, double *b, const La_INT *ldb,
    La_INT *info);
La_extern void F77_NAME(dgbsvx)(const La_INT *fact, const char *trans, const La_INT *n,
    const La_INT *kl, const La_INT *ku, const La_INT *nrhs, double *ab, const La_INT *ldab,
    double *afb, const La_INT *ldafb, La_INT *ipiv, const char *equed, double *r, double *c,
    double *b, const La_INT *ldb, double *x, const La_INT *ldx, double *rcond, double *ferr,
    double *berr, double *work, La_INT *iwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dgbtf2)(const La_INT *m, const La_INT *n, const La_INT *kl,
    const La_INT *ku, double *ab, const La_INT *ldab, La_INT *ipiv, La_INT *info);
La_extern void F77_NAME(dgbtrf)(const La_INT *m, const La_INT *n, const La_INT *kl,
    const La_INT *ku, double *ab, const La_INT *ldab, La_INT *ipiv, La_INT *info);
La_extern void F77_NAME(dgbtrs)(const char *trans, const La_INT *n, const La_INT *kl,
    const La_INT *ku, const La_INT *nrhs, const double *ab, const La_INT *ldab, const La_INT *ipiv,
    double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dgebak)(const char *job, const char *side, const La_INT *n,
    const La_INT *ilo, const La_INT *ihi, double *scale, const La_INT *m, double *v,
    const La_INT *ldv, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dgebal)(const char *job, const La_INT *n, double *a, const La_INT *lda,
    La_INT *ilo, La_INT *ihi, double *scale, La_INT *info FCLEN);
La_extern void F77_NAME(dgebd2)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *d, double *e, double *tauq, double *taup, double *work, La_INT *info);
La_extern void F77_NAME(dgebrd)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *d, double *e, double *tauq, double *taup, double *work, const La_INT *lwork,
    La_INT *info);
La_extern void F77_NAME(dgecon)(const char *norm, const La_INT *n, const double *a,
    const La_INT *lda, const double *anorm, double *rcond, double *work, La_INT *iwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dgeequ)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *r, double *c, double *rowcnd, double *colcnd, double *amax, La_INT *info);
La_extern void F77_NAME(dgees)(const char *jobvs, const char *sort,
    La_LGL (*select)(const double*, const double*), const La_INT *n, double *a, const La_INT *lda,
    La_INT *sdim, double *wr, double *wi, double *vs, const La_INT *ldvs, double *work,
    const La_INT *lwork, La_LGL *bwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dgeesx)(const char *jobvs, const char *sort,
    La_LGL (*select)(const double*, const double*), const char *sense, const La_INT *n, double *a,
    const La_INT *lda, La_INT *sdim, double *wr, double *wi, double *vs, const La_INT *ldvs,
    double *rconde, double *rcondv, double *work, const La_INT *lwork, La_INT *iwork,
    const La_INT *liwork, La_LGL *bwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dgeev)(const char *jobvl, const char *jobvr, const La_INT *n, double *a,
    const La_INT *lda, double *wr, double *wi, double *vl, const La_INT *ldvl, double *vr,
    const La_INT *ldvr, double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dgeevx)(const char *balanc, const char *jobvl, const char *jobvr,
    const char *sense, const La_INT *n, double *a, const La_INT *lda, double *wr, double *wi,
    double *vl, const La_INT *ldvl, double *vr, const La_INT *ldvr, La_INT *ilo, La_INT *ihi,
    double *scale, double *abnrm, double *rconde, double *rcondv, double *work, const La_INT *lwork,
    La_INT *iwork, La_INT *info FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dgegs)(const char *jobvsl, const char *jobvsr, La_INT *n, double *a,
    La_INT *lda, double *b, La_INT *ldb, double *alphar, double *alphai, double *beta, double *vsl,
    La_INT *ldvsl, double *vsr, La_INT *ldvsr, double *work, La_INT *lwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dgegv)(const char *jobvl, const char *jobvr, const La_INT *n, double *a,
    const La_INT *lda, double *b, const La_INT *ldb, double *alphar, double *alphai,
    const double *beta, double *vl, const La_INT *ldvl, double *vr, const La_INT *ldvr,
    double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dgehd2)(const La_INT *n, const La_INT *ilo, const La_INT *ihi, double *a,
    const La_INT *lda, double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dgehrd)(const La_INT *n, const La_INT *ilo, const La_INT *ihi, double *a,
    const La_INT *lda, double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgelq2)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dgelqf)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgels)(const char *trans, const La_INT *m, const La_INT *n,
    const La_INT *nrhs, double *a, const La_INT *lda, double *b, const La_INT *ldb, double *work,
    const La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(dgelsd)(La_INT *m, La_INT *n, La_INT *nrhs, double *a, La_INT *lda,
    double *b, La_INT *ldb, double *s, double *rcond, La_INT *rank, double *work, La_INT *lwork,
    La_INT *iwork, La_INT *info);
La_extern void F77_NAME(dgelss)(const La_INT *m, const La_INT *n, const La_INT *nrhs, double *a,
    const La_INT *lda, double *b, const La_INT *ldb, double *s, double *rcond, La_INT *rank,
    double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgelsx)(La_INT *m, La_INT *n, La_INT *nrhs, double *a, La_INT *lda,
    double *b, La_INT *ldb, La_INT *jpvt, double *rcond, La_INT *rank, double *work, La_INT *info);
La_extern void F77_NAME(dgelsy)(const La_INT *m, const La_INT *n, const La_INT *nrhs, double *a,
    const La_INT *lda, double *b, const La_INT *ldb, La_INT *jpvt, const double *rcond,
    La_INT *rank, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgeql2)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dgeqlf)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgeqp3)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    La_INT *jpvt, double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgeqpf)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    La_INT *jpvt, double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dgeqr2)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dgeqrf)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgerfs)(const char *trans, const La_INT *n, const La_INT *nrhs, double *a,
    const La_INT *lda, double *af, const La_INT *ldaf, La_INT *ipiv, double *b, const La_INT *ldb,
    double *x, const La_INT *ldx, double *ferr, double *berr, double *work, La_INT *iwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dgerq2)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dgerqf)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgesc2)(La_INT *n, double *a, La_INT *lda, double *rhs, La_INT *ipiv,
    La_INT *jpiv, double *scale);
La_extern void F77_NAME(dgesdd)(const char *jobz, const La_INT *m, const La_INT *n, double *a,
    const La_INT *lda, double *s, double *u, const La_INT *ldu, double *vt, const La_INT *ldvt,
    double *work, const La_INT *lwork, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dgesv)(const La_INT *n, const La_INT *nrhs, double *a, const La_INT *lda,
    La_INT *ipiv, double *b, const La_INT *ldb, La_INT *info);
La_extern void F77_NAME(dgesvd)(const char *jobu, const char *jobvt, const La_INT *m,
    const La_INT *n, double *a, const La_INT *lda, double *s, double *u, const La_INT *ldu,
    double *vt, const La_INT *ldvt, double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dgesvx)(const char *fact, const char *trans, const La_INT *n,
    const La_INT *nrhs, double *a, const La_INT *lda, double *af, const La_INT *ldaf, La_INT *ipiv,
    char *equed, double *r, double *c, double *b, const La_INT *ldb, double *x, const La_INT *ldx,
    double *rcond, double *ferr, double *berr, double *work, La_INT *iwork,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dgetc2)(La_INT *n, double *a, La_INT *lda, La_INT *ipiv, La_INT *jpiv,
    La_INT *info);
La_extern void F77_NAME(dgetf2)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    La_INT *ipiv, La_INT *info);
La_extern void F77_NAME(dgetrf)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    La_INT *ipiv, La_INT *info);
La_extern void F77_NAME(dgetri)(const La_INT *n, double *a, const La_INT *lda, La_INT *ipiv,
    double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgetrs)(const char *trans, const La_INT *n, const La_INT *nrhs,
    const double *a, const La_INT *lda, const La_INT *ipiv, double *b, const La_INT *ldb,
    La_INT *info FCLEN);
La_extern void F77_NAME(dggbak)(const char *job, const char *side, const La_INT *n,
    const La_INT *ilo, const La_INT *ihi, double *lscale, double *rscale, const La_INT *m,
    double *v, const La_INT *ldv, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dggbal)(const char *job, const La_INT *n, double *a, const La_INT *lda,
    double *b, const La_INT *ldb, La_INT *ilo, La_INT *ihi, double *lscale, double *rscale,
    double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dgges)(const char *jobvsl, const char *jobvsr, const char *sort,
    La_LGL (*selctg)(double*, double*, double*), const La_INT *n, double *a, const La_INT *lda,
    double *b, const La_INT *ldb, La_INT *sdim, double *alphar, double *alphai, double *beta,
    double *vsl, const La_INT *ldvsl, double *vsr, const La_INT *ldvsr, double *work,
    const La_INT *lwork, La_LGL *bwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dggesx)(const char *jobvsl, const char *jobvsr, const char *sort,
    L_fp selctg, const char *sense, La_INT *n, double *a, La_INT *lda, double *b, La_INT *ldb,
    La_INT *sdim, double *alphar, double *alphai, double *beta, double *vsl, La_INT *ldvsl,
    double *vsr, La_INT *ldvsr, double *rconde, double *rcondv, double *work, La_INT *lwork,
    La_INT *iwork, La_INT *liwork, La_LGL *bwork, La_INT *info FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dggev)(const char *jobvl, const char *jobvr, La_INT *n, double *a,
    La_INT *lda, double *b, La_INT *ldb, double *alphar, double *alphai, double *beta, double *vl,
    La_INT *ldvl, double *vr, La_INT *ldvr, double *work, La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dggevx)(const char *balanc, const char *jobvl, const char *jobvr,
    const char *sense, La_INT *n, double *a, La_INT *lda, double *b, La_INT *ldb, double *alphar,
    double *alphai, double *beta, double *vl, La_INT *ldvl, double *vr, La_INT *ldvr, La_INT *ilo,
    La_INT *ihi, double *lscale, double *rscale, double *abnrm, double *bbnrm, double *rconde,
    double *rcondv, double *work, La_INT *lwork, La_INT *iwork, La_LGL *bwork,
    La_INT *info FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dggglm)(const La_INT *n, const La_INT *m, const La_INT *p, double *a,
    const La_INT *lda, double *b, const La_INT *ldb, double *d, double *x, double *y, double *work,
    const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dgghrd)(const char *compq, const char *compz, const La_INT *n,
    const La_INT *ilo, const La_INT *ihi, double *a, const La_INT *lda, double *b,
    const La_INT *ldb, double *q, const La_INT *ldq, double *z, const La_INT *ldz,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dgglse)(const La_INT *m, const La_INT *n, const La_INT *p, double *a,
    const La_INT *lda, double *b, const La_INT *ldb, double *c, double *d, double *x, double *work,
    const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dggqrf)(const La_INT *n, const La_INT *m, const La_INT *p, double *a,
    const La_INT *lda, double *taua, double *b, const La_INT *ldb, double *taub, double *work,
    const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dggrqf)(const La_INT *m, const La_INT *p, const La_INT *n, double *a,
    const La_INT *lda, double *taua, double *b, const La_INT *ldb, double *taub, double *work,
    const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dggsvd)(const char *jobu, const char *jobv, const char *jobq,
    const La_INT *m, const La_INT *n, const La_INT *p, const La_INT *k, const La_INT *l, double *a,
    const La_INT *lda, double *b, const La_INT *ldb, const double *alpha, const double *beta,
    double *u, const La_INT *ldu, double *v, const La_INT *ldv, double *q, const La_INT *ldq,
    double *work, La_INT *iwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dggsvp)(const char *jobu, const char *jobv, const char *jobq, La_INT *m,
    La_INT *p, La_INT *n, double *a, La_INT *lda, double *b, La_INT *ldb, double *tola,
    double *tolb, La_INT *k, La_INT *l, double *u, La_INT *ldu, double *v, La_INT *ldv, double *q,
    La_INT *ldq, La_INT *iwork, double *tau, double *work, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dgtcon)(const char *norm, const La_INT *n, double *dl, double *d,
    double *du, double *du2, La_INT *ipiv, const double *anorm, double *rcond, double *work,
    La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dgtrfs)(const char *trans, const La_INT *n, const La_INT *nrhs, double *dl,
    double *d, double *du, double *dlf, double *df, double *duf, double *du2, La_INT *ipiv,
    double *b, const La_INT *ldb, double *x, const La_INT *ldx, double *ferr, double *berr,
    double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dgtsv)(const La_INT *n, const La_INT *nrhs, double *dl, double *d,
    double *du, double *b, const La_INT *ldb, La_INT *info);
La_extern void F77_NAME(dgtsvx)(const La_INT *fact, const char *trans, const La_INT *n,
    const La_INT *nrhs, double *dl, double *d, double *du, double *dlf, double *df, double *duf,
    double *du2, La_INT *ipiv, double *b, const La_INT *ldb, double *x, const La_INT *ldx,
    double *rcond, double *ferr, double *berr, double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dgttrf)(const La_INT *n, double *dl, double *d, double *du, double *du2,
    La_INT *ipiv, La_INT *info);
La_extern void F77_NAME(dgttrs)(const char *trans, const La_INT *n, const La_INT *nrhs, double *dl,
    double *d, double *du, double *du2, La_INT *ipiv, double *b, const La_INT *ldb,
    La_INT *info FCLEN);
La_extern void F77_NAME(dgtts2)(La_INT *itrans, La_INT *n, La_INT *nrhs, double *dl, double *d,
    double *du, double *du2, La_INT *ipiv, double *b, La_INT *ldb);
La_extern void F77_NAME(dhgeqz)(const char *job, const char *compq, const char *compz,
    const La_INT *n, const La_INT *ilo, const La_INT *ihi, double *h, const La_INT *ldh, double *t,
    const La_INT *ldt, double *alphar, double *alphai, const double *beta, double *q,
    const La_INT *ldq, double *z, const La_INT *ldz, double *work, const La_INT *lwork,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dhsein)(const char *side, const char *eigsrc, const char *initv,
    La_LGL *select, const La_INT *n, double *h, const La_INT *ldh, double *wr, double *wi,
    double *vl, const La_INT *ldvl, double *vr, const La_INT *ldvr, const La_INT *mm, La_INT *m,
    double *work, La_INT *ifaill, La_INT *ifailr, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dhseqr)(const char *job, const char *compz, const La_INT *n,
    const La_INT *ilo, const La_INT *ihi, double *h, const La_INT *ldh, double *wr, double *wi,
    double *z, const La_INT *ldz, double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern La_LGL F77_NAME(disnan)(const La_INT *din);
La_extern void F77_NAME(dlabad)(double *small, double *large);
La_extern void F77_NAME(dlabrd)(const La_INT *m, const La_INT *n, const La_INT *nb, double *a,
    const La_INT *lda, double *d, double *e, double *tauq, double *taup, double *x,
    const La_INT *ldx, double *y, const La_INT *ldy);
La_extern void F77_NAME(dlacon)(const La_INT *n, double *v, double *x, La_INT *isgn, double *est,
    La_INT *kase);
La_extern void F77_NAME(dlacpy)(const char *uplo, const La_INT *m, const La_INT *n, const double *a,
    const La_INT *lda, double *b, const La_INT *ldb FCLEN);
La_extern void F77_NAME(dladiv)(const double *a, const double *b, const double *c, const double *d,
    double *p, double *q);
La_extern void F77_NAME(dlae2)(const double *a, const double *b, const double *c, double *rt1,
    double *rt2);
La_extern void F77_NAME(dlaebz)(const La_INT *ijob, const La_INT *nitmax, const La_INT *n,
    const La_INT *mmax, const La_INT *minp, const La_INT *nbmin, const double *abstol,
    const double *reltol, const double *pivmin, double *d, double *e, double *e2, La_INT *nval,
    double *ab, double *c, La_INT *mout, La_INT *nab, double *work, La_INT *iwork, La_INT *info);
La_extern void F77_NAME(dlaed0)(const La_INT *icompq, const La_INT *qsiz, const La_INT *n,
    double *d, double *e, double *q, const La_INT *ldq, double *qstore, const La_INT *ldqs,
    double *work, La_INT *iwork, La_INT *info);
La_extern void F77_NAME(dlaed1)(const La_INT *n, double *d, double *q, const La_INT *ldq,
    La_INT *indxq, const double *rho, const La_INT *cutpnt, double *work, La_INT *iwork,
    La_INT *info);
La_extern void F77_NAME(dlaed2)(La_INT *k, const La_INT *n, const La_INT *n1, double *d, double *q,
    const La_INT *ldq, La_INT *indxq, double *rho, const double *z, double *dlambda, double *w,
    double *q2, La_INT *indx, La_INT *indxc, La_INT *indxp, La_INT *coltyp, La_INT *info);
La_extern void F77_NAME(dlaed3)(const La_INT *k, const La_INT *n, const La_INT *n1, double *d,
    double *q, const La_INT *ldq, const double *rho, double *dlambda, double *q2, La_INT *indx,
    La_INT *ctot, double *w, double *s, La_INT *info);
La_extern void F77_NAME(dlaed4)(const La_INT *n, const La_INT *i, const double *d, const double *z,
    const double *delta, const double *rho, double *dlam, La_INT *info);
La_extern void F77_NAME(dlaed5)(const La_INT *i, const double *d, const double *z, double *delta,
    const double *rho, double *dlam);
La_extern void F77_NAME(dlaed6)(const La_INT *kniter, const La_INT *orgati, const double *rho,
    const double *d, const double *z, const double *finit, double *tau, La_INT *info);
La_extern void F77_NAME(dlaed7)(const La_INT *icompq, const La_INT *n, const La_INT *qsiz,
    const La_INT *tlvls, const La_INT *curlvl, const La_INT *curpbm, double *d, double *q,
    const La_INT *ldq, La_INT *indxq, const double *rho, const La_INT *cutpnt, double *qstore,
    double *qptr, const La_INT *prmptr, const La_INT *perm, const La_INT *givptr,
    const La_INT *givcol, const double *givnum, double *work, La_INT *iwork, La_INT *info);
La_extern void F77_NAME(dlaed8)(const La_INT *icompq, const La_INT *k, const La_INT *n,
    const La_INT *qsiz, double *d, double *q, const La_INT *ldq, const La_INT *indxq, double *rho,
    const La_INT *cutpnt, const double *z, double *dlambda, double *q2, const La_INT *ldq2,
    double *w, La_INT *perm, La_INT *givptr, La_INT *givcol, double *givnum, La_INT *indxp,
    La_INT *indx, La_INT *info);
La_extern void F77_NAME(dlaed9)(const La_INT *k, const La_INT *kstart, const La_INT *kstop,
    const La_INT *n, double *d, double *q, const La_INT *ldq, const double *rho,
    const double *dlambda, const double *w, double *s, const La_INT *lds, La_INT *info);
La_extern void F77_NAME(dlaeda)(const La_INT *n, const La_INT *tlvls, const La_INT *curlvl,
    const La_INT *curpbm, const La_INT *prmptr, const La_INT *perm, const La_INT *givptr,
    const La_INT *givcol, const double *givnum, const double *q, const La_INT *qptr, double *z,
    double *ztemp, La_INT *info);
La_extern void F77_NAME(dlaein)(const La_INT *rightv, const La_INT *noinit, const La_INT *n,
    const double *h, const La_INT *ldh, const double *wr, const double *wi, double *vr, double *vi,
    double *b, const La_INT *ldb, double *work, const double *eps3, const double *smlnum,
    const double *bignum, La_INT *info);
La_extern void F77_NAME(dlaev2)(const double *a, const double *b, const double *c, double *rt1,
    double *rt2, double *cs1, double *sn1);
La_extern void F77_NAME(dlaexc)(const La_INT *wantq, const La_INT *n, double *t, const La_INT *ldt,
    double *q, const La_INT *ldq, const La_INT *j1, const La_INT *n1, const La_INT *n2,
    double *work, La_INT *info);
La_extern void F77_NAME(dlag2)(const double *a, const La_INT *lda, const double *b,
    const La_INT *ldb, const double *safmin, double *scale1, double *scale2, double *wr1,
    double *wr2, double *wi);
La_extern void F77_NAME(dlags2)(const La_INT *upper, const double *a1, const double *a2,
    const double *a3, const double *b1, const double *b2, const double *b3, double *csu,
    double *snu, double *csv, double *snv, double *csq, double *snq);
La_extern void F77_NAME(dlagtf)(const La_INT *n, double *a, const double *lambda, double *b,
    double *c, const double *tol, double *d, La_INT *in, La_INT *info);
La_extern void F77_NAME(dlagtm)(const char *trans, const La_INT *n, const La_INT *nrhs,
    const double *alpha, const double *dl, const double *d, const double *du, const double *x,
    const La_INT *ldx, const double *beta, double *b, const La_INT *ldb FCLEN);
La_extern void F77_NAME(dlagts)(const La_INT *job, const La_INT *n, const double *a,
    const double *b, const double *c, const double *d, const La_INT *in, double *y, double *tol,
    La_INT *info);
La_extern void F77_NAME(dlagv2)(double *a, La_INT *lda, double *b, La_INT *ldb, double *alphar,
    double *alphai, double *beta, double *csl, double *snl, double *csr, double *snr);
La_extern void F77_NAME(dlahqr)(const La_INT *wantt, const La_INT *wantz, const La_INT *n,
    const La_INT *ilo, const La_INT *ihi, double *h, const La_INT *ldh, double *wr, double *wi,
    const La_INT *iloz, const La_INT *ihiz, double *z, const La_INT *ldz, La_INT *info);
La_extern void F77_NAME(dlahrd)(const La_INT *n, const La_INT *k, const La_INT *nb, double *a,
    const La_INT *lda, double *tau, double *t, const La_INT *ldt, double *y, const La_INT *ldy);
La_extern void F77_NAME(dlaic1)(const La_INT *job, const La_INT *j, const double *x,
    const double *sest, const double *w, const double *gamma, double *sestpr, double *s, double *c);
La_extern void F77_NAME(dlaln2)(const La_INT *ltrans, const La_INT *na, const La_INT *nw,
    const double *smin, const double *ca, const double *a, const La_INT *lda, const double *d1,
    const double *d2, const double *b, const La_INT *ldb, const double *wr, const double *wi,
    double *x, const La_INT *ldx, double *scale, double *xnorm, La_INT *info);
La_extern void F77_NAME(dlals0)(La_INT *icompq, La_INT *nl, La_INT *nr, La_INT *sqre, La_INT *nrhs,
    double *b, La_INT *ldb, double *bx, La_INT *ldbx, La_INT *perm, La_INT *givptr, La_INT *givcol,
    La_INT *ldgcol, double *givnum, La_INT *ldgnum, double *poles, double *difl, double *difr,
    double *z, La_INT *k, double *c, double *s, double *work, La_INT *info);
La_extern void F77_NAME(dlalsa)(La_INT *icompq, La_INT *smlsiz, La_INT *n, La_INT *nrhs, double *b,
    La_INT *ldb, double *bx, La_INT *ldbx, double *u, La_INT *ldu, double *vt, La_INT *k,
    double *difl, double *difr, double *z, double *poles, La_INT *givptr, La_INT *givcol,
    La_INT *ldgcol, La_INT *perm, double *givnum, double *c, double *s, double *work, La_INT *iwork,
    La_INT *info);
La_extern void F77_NAME(dlalsd)(const char *uplo, La_INT *smlsiz, La_INT *n, La_INT *nrhs,
    double *d, double *e, double *b, La_INT *ldb, double *rcond, La_INT *rank, double *work,
    La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dlamc1)(La_INT *beta, La_INT *t, La_INT *rnd, La_INT *ieee1);
La_extern void F77_NAME(dlamc2)(La_INT *beta, La_INT *t, La_INT *rnd, double *eps, La_INT *emin,
    double *rmin, La_INT *emax, double *rmax);
La_extern double F77_NAME(dlamc3)(double *a, double *b);
La_extern void F77_NAME(dlamc4)(La_INT *emin, double *start, La_INT *base);
La_extern void F77_NAME(dlamc5)(La_INT *beta, La_INT *p, La_INT *emin, La_INT *ieee, La_INT *emax,
    double *rmax);
La_extern double F77_NAME(dlamch)(const char *cmach FCLEN);
La_extern void F77_NAME(dlamrg)(const La_INT *n1, const La_INT *n2, const double *a,
    const La_INT *dtrd1, const La_INT *dtrd2, La_INT *index);
La_extern double F77_NAME(dlangb)(const char *norm, const La_INT *n, const La_INT *kl,
    const La_INT *ku, const double *ab, const La_INT *ldab, double *work FCLEN);
La_extern double F77_NAME(dlange)(const char *norm, const La_INT *m, const La_INT *n,
    const double *a, const La_INT *lda, double *work FCLEN);
La_extern double F77_NAME(dlangt)(const char *norm, const La_INT *n, const double *dl,
    const double *d, const double *du FCLEN);
La_extern double F77_NAME(dlanhs)(const char *norm, const La_INT *n, const double *a,
    const La_INT *lda, double *work FCLEN);
La_extern double F77_NAME(dlansb)(const char *norm, const char *uplo, const La_INT *n,
    const La_INT *k, const double *ab, const La_INT *ldab, double *work FCLEN FCLEN);
La_extern double F77_NAME(dlansp)(const char *norm, const char *uplo, const La_INT *n,
    const double *ap, double *work FCLEN FCLEN);
La_extern double F77_NAME(dlanst)(const char *norm, const La_INT *n, const double *d,
    const double *e FCLEN);
La_extern double F77_NAME(dlansy)(const char *norm, const char *uplo, const La_INT *n,
    const double *a, const La_INT *lda, double *work FCLEN FCLEN);
La_extern double F77_NAME(dlantb)(const char *norm, const char *uplo, const char *diag,
    const La_INT *n, const La_INT *k, const double *ab, const La_INT *ldab,
    double *work FCLEN FCLEN FCLEN);
La_extern double F77_NAME(dlantp)(const char *norm, const char *uplo, const char *diag,
    const La_INT *n, const double *ap, double *work FCLEN FCLEN FCLEN);
La_extern double F77_NAME(dlantr)(const char *norm, const char *uplo, const char *diag,
    const La_INT *m, const La_INT *n, const double *a, const La_INT *lda,
    double *work FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dlanv2)(double *a, double *b, double *c, double *d, double *rt1r,
    double *rt1i, double *rt2r, double *rt2i, double *cs, double *sn);
La_extern void F77_NAME(dlapll)(const La_INT *n, double *x, const La_INT *incx, double *y,
    const La_INT *incy, double *ssmin);
La_extern void F77_NAME(dlapmt)(const La_LGL *forwrd, const La_INT *m, const La_INT *n, double *x,
    const La_INT *ldx, const La_INT *k);
La_extern double F77_NAME(dlapy2)(const double *x, const double *y);
La_extern double F77_NAME(dlapy3)(const double *x, const double *y, const double *z);
La_extern void F77_NAME(dlaqgb)(const La_INT *m, const La_INT *n, const La_INT *kl,
    const La_INT *ku, double *ab, const La_INT *ldab, double *r, double *c, double *rowcnd,
    double *colcnd, const double *amax, const char *equed FCLEN);
La_extern void F77_NAME(dlaqge)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *r, double *c, double *rowcnd, double *colcnd, const double *amax,
    const char *equed FCLEN);
La_extern void F77_NAME(dlaqp2)(La_INT *m, La_INT *n, La_INT *offset, double *a, La_INT *lda,
    La_INT *jpvt, double *tau, double *vn1, double *vn2, double *work);
La_extern void F77_NAME(dlaqps)(La_INT *m, La_INT *n, La_INT *offset, La_INT *nb, La_INT *kb,
    double *a, La_INT *lda, La_INT *jpvt, double *tau, double *vn1, double *vn2, double *auxv,
    double *f, La_INT *ldf);
La_extern void F77_NAME(dlaqsb)(const char *uplo, const La_INT *n, const La_INT *kd, double *ab,
    const La_INT *ldab, const double *s, const double *scond, const double *amax,
    const char *equed FCLEN FCLEN);
La_extern void F77_NAME(dlaqsp)(const char *uplo, const La_INT *n, double *ap, const double *s,
    const double *scond, const double *amax, La_INT *equed FCLEN);
La_extern void F77_NAME(dlaqsy)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    const double *s, const double *scond, const double *amax, La_INT *equed FCLEN);
La_extern void F77_NAME(dlaqtr)(const La_INT *ltran, const La_INT *lreal, const La_INT *n,
    const double *t, const La_INT *ldt, const double *b, const double *w, double *scale, double *x,
    double *work, La_INT *info);
La_extern void F77_NAME(dlar1v)(const La_INT *n, const La_INT *b1, const La_INT *bn,
    const double *lambda, const double *d, const double *l, const double *ld, const double *lld,
    const double *pivmin, const double *gaptol, double *z, const La_LGL *wantnc, La_INT *negcnt,
    double *ztz, double *mingma, La_INT *r, La_INT *isuppz, double *nrminv, double *resid,
    double *rqcorr, double *work);
La_extern void F77_NAME(dlar2v)(const La_INT *n, double *x, double *y, double *z,
    const La_INT *incx, const double *c, const double *s, const La_INT *incc);
La_extern void F77_NAME(dlarf)(const char *side, const La_INT *m, const La_INT *n, const double *v,
    const La_INT *incv, const double *tau, double *c, const La_INT *ldc, double *work FCLEN);
La_extern void F77_NAME(dlarfb)(const char *side, const char *trans, const char *direct,
    const char *storev, const La_INT *m, const La_INT *n, const La_INT *k, const double *v,
    const La_INT *ldv, const double *t, const La_INT *ldt, double *c, const La_INT *ldc,
    double *work, const La_INT *ldwork FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dlarfg)(const La_INT *n, const double *alpha, double *x, const La_INT *incx,
    double *tau);
La_extern void F77_NAME(dlarft)(const char *direct, const char *storev, const La_INT *n,
    const La_INT *k, double *v, const La_INT *ldv, const double *tau, double *t,
    const La_INT *ldt FCLEN FCLEN);
La_extern void F77_NAME(dlarfx)(const char *side, const La_INT *m, const La_INT *n, const double *v,
    const double *tau, double *c, const La_INT *ldc, double *work FCLEN);
La_extern void F77_NAME(dlargv)(const La_INT *n, double *x, const La_INT *incx, double *y,
    const La_INT *incy, double *c, const La_INT *incc);
La_extern void F77_NAME(dlarnv)(const La_INT *idist, La_INT *iseed, const La_INT *n, double *x);
La_extern void F77_NAME(dlarrb)(const La_INT *n, const double *d, const double *lld,
    const La_INT *ifirst, const La_INT *ilast, const double *rtol1, const double *rtol2,
    const La_INT *offset, double *w, double *wgap, double *werr, double *work, La_INT *iwork,
    const double *pivmin, const double *spdiam, const La_INT *twist, La_INT *info);
La_extern void F77_NAME(dlarre)(const char *range, const La_INT *n, double *vl, double *vu,
    const La_INT *il, const La_INT *iu, double *d, double *e, double *e2, const double *rtol1,
    const double *rtol2, const double *spltol, La_INT *nsplit, La_INT *isplit, La_INT *m, double *w,
    double *werr, double *wgap, La_INT *iblock, La_INT *indexw, double *gers, double *pivmin,
    double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dlarrf)(const La_INT *n, const double *d, const double *l, const double *ld,
    const La_INT *clstrt, const La_INT *clend, const double *w, double *wgap, const double *werr,
    const double *spdiam, const double *clgapl, const double *clgapr, const double *pivmin,
    double *sigma, double *dplus, double *lplus, double *work, La_INT *info);
La_extern void F77_NAME(dlarrv)(const La_INT *n, const double *vl, const double *vu, double *d,
    double *l, const double *pivmin, const La_INT *isplit, const La_INT *m, const La_INT *dol,
    const La_INT *dou, const double *minrgp, const double *rtol1, const double *rtol2, double *w,
    double *werr, double *wgap, const La_INT *iblock, const La_INT *indexw, const double *gers,
    double *z, const La_INT *ldz, La_INT *isuppz, double *work, La_INT *iwork, La_INT *info);
La_extern void F77_NAME(dlartg)(const double *f, const double *g, double *c, double *s, double *r);
La_extern void F77_NAME(dlartv)(const La_INT *n, double *x, const La_INT *incx, double *y,
    const La_INT *incy, const double *c, const double *s, const La_INT *incc);
La_extern void F77_NAME(dlaruv)(La_INT *iseed, const La_INT *n, double *x);
La_extern void F77_NAME(dlarz)(const char *side, La_INT *m, La_INT *n, La_INT *l, double *v,
    La_INT *incv, double *tau, double *c, La_INT *ldc, double *work FCLEN);
La_extern void F77_NAME(dlarzb)(const char *side, const char *trans, const char *direct,
    const char *storev, La_INT *m, La_INT *n, La_INT *k, La_INT *l, double *v, La_INT *ldv,
    double *t, La_INT *ldt, double *c, La_INT *ldc, double *work,
    La_INT *ldwork FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dlarzt)(const char *direct, const char *storev, La_INT *n, La_INT *k,
    double *v, La_INT *ldv, double *tau, double *t, La_INT *ldt FCLEN FCLEN);
La_extern void F77_NAME(dlas2)(const double *f, const double *g, const double *h, double *ssmin,
    double *ssmax);
La_extern void F77_NAME(dlascl)(const char *type, const La_INT *kl, const La_INT *ku, double *cfrom,
    double *cto, const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(dlasd0)(La_INT *n, La_INT *sqre, double *d, double *e, double *u,
    La_INT *ldu, double *vt, La_INT *ldvt, La_INT *smlsiz, La_INT *iwork, double *work,
    La_INT *info);
La_extern void F77_NAME(dlasd1)(La_INT *nl, La_INT *nr, La_INT *sqre, double *d, double *alpha,
    double *beta, double *u, La_INT *ldu, double *vt, La_INT *ldvt, La_INT *idxq, La_INT *iwork,
    double *work, La_INT *info);
La_extern void F77_NAME(dlasd2)(La_INT *nl, La_INT *nr, La_INT *sqre, La_INT *k, double *d,
    double *z, double *alpha, double *beta, double *u, La_INT *ldu, double *vt, La_INT *ldvt,
    double *dsigma, double *u2, La_INT *ldu2, double *vt2, La_INT *ldvt2, La_INT *idxp, La_INT *idx,
    La_INT *idxc, La_INT *idxq, La_INT *coltyp, La_INT *info);
La_extern void F77_NAME(dlasd3)(La_INT *nl, La_INT *nr, La_INT *sqre, La_INT *k, double *d,
    double *q, La_INT *ldq, double *dsigma, double *u, La_INT *ldu, double *u2, La_INT *ldu2,
    double *vt, La_INT *ldvt, double *vt2, La_INT *ldvt2, La_INT *idxc, La_INT *ctot, double *z,
    La_INT *info);
La_extern void F77_NAME(dlasd4)(La_INT *n, La_INT *i, double *d, double *z, double *delta,
    double *rho, double *sigma, double *work, La_INT *info);
La_extern void F77_NAME(dlasd5)(La_INT *i, double *d, double *z, double *delta, double *rho,
    double *dsigma, double *work);
La_extern void F77_NAME(dlasd6)(La_INT *icompq, La_INT *nl, La_INT *nr, La_INT *sqre, double *d,
    double *vf, double *vl, double *alpha, double *beta, La_INT *idxq, La_INT *perm, La_INT *givptr,
    La_INT *givcol, La_INT *ldgcol, double *givnum, La_INT *ldgnum, double *poles, double *difl,
    double *difr, double *z, La_INT *k, double *c, double *s, double *work, La_INT *iwork,
    La_INT *info);
La_extern void F77_NAME(dlasd7)(La_INT *icompq, La_INT *nl, La_INT *nr, La_INT *sqre, La_INT *k,
    double *d, double *z, double *zw, double *vf, double *vfw, double *vl, double *vlw,
    double *alpha, double *beta, double *dsigma, La_INT *idx, La_INT *idxp, La_INT *idxq,
    La_INT *perm, La_INT *givptr, La_INT *givcol, La_INT *ldgcol, double *givnum, La_INT *ldgnum,
    double *c, double *s, La_INT *info);
La_extern void F77_NAME(dlasd8)(La_INT *icompq, La_INT *k, double *d, double *z, double *vf,
    double *vl, double *difl, double *difr, La_INT *lddifr, double *dsigma, double *work,
    La_INT *info);
La_extern void F77_NAME(dlasd9)(La_INT *icompq, La_INT *ldu, La_INT *k, double *d, double *z,
    double *vf, double *vl, double *difl, double *difr, double *dsigma, double *work, La_INT *info);
La_extern void F77_NAME(dlasda)(La_INT *icompq, La_INT *smlsiz, La_INT *n, La_INT *sqre, double *d,
    double *e, double *u, La_INT *ldu, double *vt, La_INT *k, double *difl, double *difr, double *z,
    double *poles, La_INT *givptr, La_INT *givcol, La_INT *ldgcol, La_INT *perm, double *givnum,
    double *c, double *s, double *work, La_INT *iwork, La_INT *info);
La_extern void F77_NAME(dlasdq)(const char *uplo, La_INT *sqre, La_INT *n, La_INT *ncvt,
    La_INT *nru, La_INT *ncc, double *d, double *e, double *vt, La_INT *ldvt, double *u,
    La_INT *ldu, double *c, La_INT *ldc, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dlasdt)(La_INT *n, La_INT *lvl, La_INT *nd, La_INT *inode, La_INT *ndiml,
    La_INT *ndimr, La_INT *msub);
La_extern void F77_NAME(dlaset)(const char *uplo, const La_INT *m, const La_INT *n,
    const double *alpha, const double *beta, double *a, const La_INT *lda FCLEN);
La_extern void F77_NAME(dlasq1)(const La_INT *n, double *d, double *e, double *work, La_INT *info);
La_extern void F77_NAME(dlasq2)(const La_INT *n, double *z, La_INT *info);
La_extern void F77_NAME(dlasq3)(const La_INT *i0, La_INT *n0, double *z, La_INT *pp, double *dmin,
    double *sigma, double *desig, const double *qmax, La_INT *nfail, La_INT *iter, La_INT *ndiv,
    const La_LGL *ieee, La_INT *ttype, double *dmin1, double *dmin2, double *dn, double *dn1,
    double *dn2, double *g, double *tau);
La_extern void F77_NAME(dlasq4)(const La_INT *i0, const La_INT *n0, const double *z,
    const La_INT *pp, const La_INT *n0in, const double *dmin, const double *dmin1,
    const double *dmin2, const double *dn, const double *dn1, const double *dn2, double *tau,
    La_INT *ttype, double *g);
La_extern void F77_NAME(dlasq5)(const La_INT *i0, const La_INT *n0, const double *z,
    const La_INT *pp, const double *tau, const double *sigma, double *dmin, double *dmin1,
    double *dmin2, double *dn, double *dnm1, double *dnm2, const La_LGL *ieee, const double *eps);
La_extern void F77_NAME(dlasq6)(La_INT *i0, La_INT *n0, double *z, La_INT *pp, double *dmin,
    double *dmin1, double *dmin2, double *dn, double *dnm1, double *dnm2);
La_extern void F77_NAME(dlasr)(const char *side, const char *pivot, const char *direct,
    const La_INT *m, const La_INT *n, const double *c, const double *s, double *a,
    const La_INT *lda FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dlasrt)(const char *id, const La_INT *n, double *d, La_INT *info FCLEN);
La_extern void F77_NAME(dlassq)(const La_INT *n, const double *x, const La_INT *incx, double *scale,
    double *sumsq);
La_extern void F77_NAME(dlasv2)(const double *f, const double *g, const double *h, double *ssmin,
    double *ssmax, double *snr, double *csr, double *snl, double *csl);
La_extern void F77_NAME(dlaswp)(const La_INT *n, double *a, const La_INT *lda, const La_INT *k1,
    const La_INT *k2, const La_INT *ipiv, const La_INT *incx);
La_extern void F77_NAME(dlasy2)(const La_INT *ltranl, const La_INT *ltranr, const La_INT *isgn,
    const La_INT *n1, const La_INT *n2, const double *tl, const La_INT *ldtl, const double *tr,
    const La_INT *ldtr, const double *b, const La_INT *ldb, double *scale, double *x,
    const La_INT *ldx, double *xnorm, La_INT *info);
La_extern void F77_NAME(dlasyf)(const char *uplo, const La_INT *n, const La_INT *nb,
    const La_INT *kb, double *a, const La_INT *lda, La_INT *ipiv, double *w, const La_INT *ldw,
    La_INT *info FCLEN);
La_extern void F77_NAME(dlatbs)(const char *uplo, const char *trans, const char *diag,
    const char *normin, const La_INT *n, const La_INT *kd, const double *ab, const La_INT *ldab,
    double *x, double *scale, double *cnorm, La_INT *info FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dlatdf)(La_INT *ijob, La_INT *n, double *z, La_INT *ldz, double *rhs,
    double *rdsum, double *rdscal, La_INT *ipiv, La_INT *jpiv);
La_extern void F77_NAME(dlatps)(const char *uplo, const char *trans, const char *diag,
    const char *normin, const La_INT *n, const double *ap, double *x, double *scale, double *cnorm,
    La_INT *info FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dlatrd)(const char *uplo, const La_INT *n, const La_INT *nb, double *a,
    const La_INT *lda, double *e, double *tau, double *w, const La_INT *ldw FCLEN);
La_extern void F77_NAME(dlatrs)(const char *uplo, const char *trans, const char *diag,
    const char *normin, const La_INT *n, const double *a, const La_INT *lda, double *x,
    double *scale, double *cnorm, La_INT *info FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dlatrz)(La_INT *m, La_INT *n, La_INT *l, double *a, La_INT *lda,
    double *tau, double *work);
La_extern void F77_NAME(dlatzm)(const char *side, const La_INT *m, const La_INT *n, const double *v,
    const La_INT *incv, const double *tau, double *c1, double *c2, const La_INT *ldc,
    double *work FCLEN);
La_extern void F77_NAME(dlauu2)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(dlauum)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(dopgtr)(const char *uplo, const La_INT *n, const double *ap,
    const double *tau, double *q, const La_INT *ldq, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dopmtr)(const char *side, const char *uplo, const char *trans,
    const La_INT *m, const La_INT *n, const double *ap, const double *tau, double *c,
    const La_INT *ldc, double *work, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dorg2l)(const La_INT *m, const La_INT *n, const La_INT *k, double *a,
    const La_INT *lda, const double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dorg2r)(const La_INT *m, const La_INT *n, const La_INT *k, double *a,
    const La_INT *lda, const double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dorgbr)(const char *vect, const La_INT *m, const La_INT *n, const La_INT *k,
    double *a, const La_INT *lda, const double *tau, double *work, const La_INT *lwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dorghr)(const La_INT *n, const La_INT *ilo, const La_INT *ihi, double *a,
    const La_INT *lda, const double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dorgl2)(const La_INT *m, const La_INT *n, const La_INT *k, double *a,
    const La_INT *lda, const double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dorglq)(const La_INT *m, const La_INT *n, const La_INT *k, double *a,
    const La_INT *lda, const double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dorgql)(const La_INT *m, const La_INT *n, const La_INT *k, double *a,
    const La_INT *lda, const double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dorgqr)(const La_INT *m, const La_INT *n, const La_INT *k, double *a,
    const La_INT *lda, const double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dorgr2)(const La_INT *m, const La_INT *n, const La_INT *k, double *a,
    const La_INT *lda, const double *tau, double *work, La_INT *info);
La_extern void F77_NAME(dorgrq)(const La_INT *m, const La_INT *n, const La_INT *k, double *a,
    const La_INT *lda, const double *tau, double *work, const La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dorgtr)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    const double *tau, double *work, const La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(dorm2l)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dorm2r)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormbr)(const char *vect, const char *side, const char *trans,
    const La_INT *m, const La_INT *n, const La_INT *k, const double *a, const La_INT *lda,
    const double *tau, double *c, const La_INT *ldc, double *work, const La_INT *lwork,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dormhr)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *ilo, const La_INT *ihi, const double *a, const La_INT *lda,
    const double *tau, double *c, const La_INT *ldc, double *work, const La_INT *lwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dorml2)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormlq)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormql)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormqr)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormr2)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormr3)(const char *side, const char *trans, La_INT *m, La_INT *n,
    La_INT *k, La_INT *l, double *a, La_INT *lda, double *tau, double *c, La_INT *ldc, double *work,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormrq)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormrz)(const char *side, const char *trans, La_INT *m, La_INT *n,
    La_INT *k, La_INT *l, double *a, La_INT *lda, double *tau, double *c, La_INT *ldc, double *work,
    La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dormtr)(const char *side, const char *uplo, const char *trans,
    const La_INT *m, const La_INT *n, const double *a, const La_INT *lda, const double *tau,
    double *c, const La_INT *ldc, double *work, const La_INT *lwork,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dpbcon)(const char *uplo, const La_INT *n, const La_INT *kd,
    const double *ab, const La_INT *ldab, const double *anorm, double *rcond, double *work,
    La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dpbequ)(const char *uplo, const La_INT *n, const La_INT *kd,
    const double *ab, const La_INT *ldab, double *s, double *scond, double *amax,
    La_INT *info FCLEN);
La_extern void F77_NAME(dpbrfs)(const char *uplo, const La_INT *n, const La_INT *kd,
    const La_INT *nrhs, const double *ab, const La_INT *ldab, const double *afb,
    const La_INT *ldafb, const double *b, const La_INT *ldb, double *x, const La_INT *ldx,
    double *ferr, double *berr, double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dpbstf)(const char *uplo, const La_INT *n, const La_INT *kd, double *ab,
    const La_INT *ldab, La_INT *info FCLEN);
La_extern void F77_NAME(dpbsv)(const char *uplo, const La_INT *n, const La_INT *kd,
    const La_INT *nrhs, double *ab, const La_INT *ldab, double *b, const La_INT *ldb,
    La_INT *info FCLEN);
La_extern void F77_NAME(dpbsvx)(const La_INT *fact, const char *uplo, const La_INT *n,
    const La_INT *kd, const La_INT *nrhs, double *ab, const La_INT *ldab, double *afb,
    const La_INT *ldafb, char *equed, double *s, double *b, const La_INT *ldb, double *x,
    const La_INT *ldx, double *rcond, double *ferr, double *berr, double *work, La_INT *iwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dpbtf2)(const char *uplo, const La_INT *n, const La_INT *kd, double *ab,
    const La_INT *ldab, La_INT *info FCLEN);
La_extern void F77_NAME(dpbtrf)(const char *uplo, const La_INT *n, const La_INT *kd, double *ab,
    const La_INT *ldab, La_INT *info FCLEN);
La_extern void F77_NAME(dpbtrs)(const char *uplo, const La_INT *n, const La_INT *kd,
    const La_INT *nrhs, const double *ab, const La_INT *ldab, double *b, const La_INT *ldb,
    La_INT *info FCLEN);
La_extern void F77_NAME(dpftrf)(const char *transr, const char *uplo, const La_INT *n, double *a,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dpocon)(const char *uplo, const La_INT *n, const double *a,
    const La_INT *lda, const double *anorm, double *rcond, double *work, La_INT *iwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dpoequ)(const La_INT *n, const double *a, const La_INT *lda, double *s,
    double *scond, double *amax, La_INT *info);
La_extern void F77_NAME(dporfs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *a, const La_INT *lda, const double *af, const La_INT *ldaf, const double *b,
    const La_INT *ldb, double *x, const La_INT *ldx, double *ferr, double *berr, double *work,
    La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dposv)(const char *uplo, const La_INT *n, const La_INT *nrhs, double *a,
    const La_INT *lda, double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dposvx)(const La_INT *fact, const char *uplo, const La_INT *n,
    const La_INT *nrhs, double *a, const La_INT *lda, double *af, const La_INT *ldaf,
    const char *equed, double *s, double *b, const La_INT *ldb, double *x, const La_INT *ldx,
    double *rcond, double *ferr, double *berr, double *work, La_INT *iwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dpotf2)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(dpotrf)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(dpotrf2)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(dpotri)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(dpotrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *a, const La_INT *lda, double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dppcon)(const char *uplo, const La_INT *n, const double *ap,
    const double *anorm, double *rcond, double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dppequ)(const char *uplo, const La_INT *n, const double *ap, double *s,
    double *scond, double *amax, La_INT *info FCLEN);
La_extern void F77_NAME(dpprfs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *ap, const double *afp, const double *b, const La_INT *ldb, double *x,
    const La_INT *ldx, double *ferr, double *berr, double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dppsv)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *ap, double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dppsvx)(const char *fact, const char *uplo, const La_INT *n,
    const La_INT *nrhs, double *ap, double *afp, const char *equed, double *s, double *b,
    const La_INT *ldb, double *x, const La_INT *ldx, double *rcond, double *ferr, double *berr,
    double *work, La_INT *iwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dpptrf)(const char *uplo, const La_INT *n, double *ap, La_INT *info FCLEN);
La_extern void F77_NAME(dpptri)(const char *uplo, const La_INT *n, double *ap, La_INT *info FCLEN);
La_extern void F77_NAME(dpptrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *ap, double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dpstrf)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *piv, La_INT *rank, double *tol, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dptcon)(const La_INT *n, const double *d, const double *e,
    const double *anorm, double *rcond, double *work, La_INT *info);
La_extern void F77_NAME(dpteqr)(const char *compz, const La_INT *n, double *d, double *e, double *z,
    const La_INT *ldz, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dptrfs)(const La_INT *n, const La_INT *nrhs, const double *d,
    const double *e, const double *df, const double *ef, const double *b, const La_INT *ldb,
    double *x, const La_INT *ldx, double *ferr, double *berr, double *work, La_INT *info);
La_extern void F77_NAME(dptsv)(const La_INT *n, const La_INT *nrhs, double *d, double *e, double *b,
    const La_INT *ldb, La_INT *info);
La_extern void F77_NAME(dptsvx)(const La_INT *fact, const La_INT *n, const La_INT *nrhs,
    const double *d, const double *e, double *df, double *ef, const double *b, const La_INT *ldb,
    double *x, const La_INT *ldx, double *rcond, double *ferr, double *berr, double *work,
    La_INT *info);
La_extern void F77_NAME(dpttrf)(const La_INT *n, double *d, double *e, La_INT *info);
La_extern void F77_NAME(dpttrs)(const La_INT *n, const La_INT *nrhs, const double *d,
    const double *e, double *b, const La_INT *ldb, La_INT *info);
La_extern void F77_NAME(dptts2)(La_INT *n, La_INT *nrhs, double *d, double *e, double *b,
    La_INT *ldb);
La_extern void F77_NAME(drscl)(const La_INT *n, const double *sa, double *sx, const La_INT *incx);
La_extern void F77_NAME(dsbev)(const char *jobz, const char *uplo, const La_INT *n,
    const La_INT *kd, double *ab, const La_INT *ldab, double *w, double *z, const La_INT *ldz,
    double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsbevd)(const char *jobz, const char *uplo, const La_INT *n,
    const La_INT *kd, double *ab, const La_INT *ldab, double *w, double *z, const La_INT *ldz,
    double *work, const La_INT *lwork, La_INT *iwork, const La_INT *liwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsbevx)(const char *jobz, const char *range, const char *uplo,
    const La_INT *n, const La_INT *kd, double *ab, const La_INT *ldab, double *q, const La_INT *ldq,
    const double *vl, const double *vu, const La_INT *il, const La_INT *iu, const double *abstol,
    La_INT *m, double *w, double *z, const La_INT *ldz, double *work, La_INT *iwork, La_INT *ifail,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dsbgst)(const char *vect, const char *uplo, const La_INT *n,
    const La_INT *ka, const La_INT *kb, double *ab, const La_INT *ldab, double *bb,
    const La_INT *ldbb, double *x, const La_INT *ldx, double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsbgv)(const char *jobz, const char *uplo, const La_INT *n,
    const La_INT *ka, const La_INT *kb, double *ab, const La_INT *ldab, double *bb,
    const La_INT *ldbb, double *w, double *z, const La_INT *ldz, double *work,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsbgvd)(const char *jobz, const char *uplo, La_INT *n, La_INT *ka,
    La_INT *kb, double *ab, La_INT *ldab, double *bb, La_INT *ldbb, double *w, double *z,
    La_INT *ldz, double *work, La_INT *lwork, La_INT *iwork, La_INT *liwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsbgvx)(const char *jobz, const char *range, const char *uplo, La_INT *n,
    La_INT *ka, La_INT *kb, double *ab, La_INT *ldab, double *bb, La_INT *ldbb, double *q,
    La_INT *ldq, double *vl, double *vu, La_INT *il, La_INT *iu, double *abstol, La_INT *m,
    double *w, double *z, La_INT *ldz, double *work, La_INT *iwork, La_INT *ifail,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dsbtrd)(const char *vect, const char *uplo, const La_INT *n,
    const La_INT *kd, double *ab, const La_INT *ldab, double *d, double *e, double *q,
    const La_INT *ldq, double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dspcon)(const char *uplo, const La_INT *n, const double *ap,
    const La_INT *ipiv, const double *anorm, double *rcond, double *work, La_INT *iwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dspev)(const char *jobz, const char *uplo, const La_INT *n, double *ap,
    double *w, double *z, const La_INT *ldz, double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dspevd)(const char *jobz, const char *uplo, const La_INT *n, double *ap,
    double *w, double *z, const La_INT *ldz, double *work, const La_INT *lwork, La_INT *iwork,
    const La_INT *liwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dspevx)(const char *jobz, const char *range, const char *uplo,
    const La_INT *n, double *ap, const double *vl, const double *vu, const La_INT *il,
    const La_INT *iu, const double *abstol, La_INT *m, double *w, double *z, const La_INT *ldz,
    double *work, La_INT *iwork, La_INT *ifail, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dspgst)(const La_INT *itype, const char *uplo, const La_INT *n, double *ap,
    double *bp, La_INT *info FCLEN);
#ifdef usePR18534fix
La_extern void F77_NAME(dspgv)(const La_INT *itype, const char *jobz, const char *uplo,
    const La_INT *n, double *ap, double *bp, double *w, double *z, const La_INT *ldz, double *work,
    La_INT *info FCLEN FCLEN);
#else
La_extern void F77_NAME(dspgv)(const La_INT *itype, const char *jobz, const char *uplo,
    const La_INT *n, double *ap, double *bp, double *w, double *z, const La_INT *ldz, double *work,
    La_INT *info FCLEN FCLEN FCLEN);
#endif
La_extern void F77_NAME(dspgvd)(La_INT *itype, const char *jobz, const char *uplo, La_INT *n,
    double *ap, double *bp, double *w, double *z, La_INT *ldz, double *work, La_INT *lwork,
    La_INT *iwork, La_INT *liwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dspgvx)(La_INT *itype, const char *jobz, const char *range,
    const char *uplo, La_INT *n, double *ap, double *bp, double *vl, double *vu, La_INT *il,
    La_INT *iu, double *abstol, La_INT *m, double *w, double *z, La_INT *ldz, double *work,
    La_INT *iwork, La_INT *ifail, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dsprfs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *ap, const double *afp, const La_INT *ipiv, const double *b, const La_INT *ldb,
    double *x, const La_INT *ldx, double *ferr, double *berr, double *work, La_INT *iwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dspsv)(const char *uplo, const La_INT *n, const La_INT *nrhs, double *ap,
    La_INT *ipiv, double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dspsvx)(const La_INT *fact, const char *uplo, const La_INT *n,
    const La_INT *nrhs, const double *ap, double *afp, La_INT *ipiv, const double *b,
    const La_INT *ldb, double *x, const La_INT *ldx, double *rcond, double *ferr, double *berr,
    double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dsptrd)(const char *uplo, const La_INT *n, double *ap, double *d, double *e,
    double *tau, La_INT *info FCLEN);
La_extern void F77_NAME(dsptrf)(const char *uplo, const La_INT *n, double *ap, La_INT *ipiv,
    La_INT *info FCLEN);
La_extern void F77_NAME(dsptri)(const char *uplo, const La_INT *n, double *ap, const La_INT *ipiv,
    double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dsptrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *ap, const La_INT *ipiv, double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dstebz)(const char *range, const char *order, const La_INT *n,
    const double *vl, const double *vu, const La_INT *il, const La_INT *iu, const double *abstol,
    const double *d, const double *e, La_INT *m, La_INT *nsplit, double *w, La_INT *iblock,
    La_INT *isplit, double *work, La_INT *iwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dstedc)(const char *compz, const La_INT *n, double *d, double *e, double *z,
    const La_INT *ldz, double *work, const La_INT *lwork, La_INT *iwork, const La_INT *liwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dstegr)(const char *jobz, const char *range, La_INT *n, double *d,
    double *e, double *vl, double *vu, La_INT *il, La_INT *iu, double *abstol, La_INT *m, double *w,
    double *z, La_INT *ldz, La_INT *isuppz, double *work, La_INT *lwork, La_INT *iwork,
    La_INT *liwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dstein)(const La_INT *n, const double *d, const double *e, const La_INT *m,
    const double *w, const La_INT *iblock, const La_INT *isplit, double *z, const La_INT *ldz,
    double *work, La_INT *iwork, La_INT *ifail, La_INT *info);
La_extern void F77_NAME(dsteqr)(const char *compz, const La_INT *n, double *d, double *e, double *z,
    const La_INT *ldz, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dsterf)(const La_INT *n, double *d, double *e, La_INT *info);
La_extern void F77_NAME(dstev)(const char *jobz, const La_INT *n, double *d, double *e, double *z,
    const La_INT *ldz, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dstevd)(const char *jobz, const La_INT *n, double *d, double *e, double *z,
    const La_INT *ldz, double *work, const La_INT *lwork, La_INT *iwork, const La_INT *liwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dstevr)(const char *jobz, const char *range, La_INT *n, double *d,
    double *e, double *vl, double *vu, La_INT *il, La_INT *iu, double *abstol, La_INT *m, double *w,
    double *z, La_INT *ldz, La_INT *isuppz, double *work, La_INT *lwork, La_INT *iwork,
    La_INT *liwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dstevx)(const char *jobz, const char *range, const La_INT *n, double *d,
    double *e, const double *vl, const double *vu, const La_INT *il, const La_INT *iu,
    const double *abstol, La_INT *m, double *w, double *z, const La_INT *ldz, double *work,
    La_INT *iwork, La_INT *ifail, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsycon)(const char *uplo, const La_INT *n, const double *a,
    const La_INT *lda, const La_INT *ipiv, const double *anorm, double *rcond, double *work,
    La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dsyequb)(const char *uplo, const La_INT *n, const double *a,
    const La_INT *lda, double *s, double *scond, double *amax, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dsyev)(const char *jobz, const char *uplo, const La_INT *n, double *a,
    const La_INT *lda, double *w, double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsyevd)(const char *jobz, const char *uplo, const La_INT *n, double *a,
    const La_INT *lda, double *w, double *work, const La_INT *lwork, La_INT *iwork,
    const La_INT *liwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsyevr)(const char *jobz, const char *range, const char *uplo,
    const La_INT *n, double *a, const La_INT *lda, const double *vl, const double *vu,
    const La_INT *il, const La_INT *iu, const double *abstol, La_INT *m, double *w, double *z,
    const La_INT *ldz, La_INT *isuppz, double *work, const La_INT *lwork, La_INT *iwork,
    const La_INT *liwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dsyevx)(const char *jobz, const char *range, const char *uplo,
    const La_INT *n, double *a, const La_INT *lda, const double *vl, const double *vu,
    const La_INT *il, const La_INT *iu, const double *abstol, La_INT *m, double *w, double *z,
    const La_INT *ldz, double *work, const La_INT *lwork, La_INT *iwork, La_INT *ifail,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dsygs2)(const La_INT *itype, const char *uplo, const La_INT *n, double *a,
    const La_INT *lda, const double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dsygst)(const La_INT *itype, const char *uplo, const La_INT *n, double *a,
    const La_INT *lda, const double *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(dsygv)(const La_INT *itype, const char *jobz, const char *uplo,
    const La_INT *n, double *a, const La_INT *lda, double *b, const La_INT *ldb, double *w,
    double *work, const La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsygvd)(La_INT *itype, const char *jobz, const char *uplo, La_INT *n,
    double *a, La_INT *lda, double *b, La_INT *ldb, double *w, double *work, La_INT *lwork,
    La_INT *iwork, La_INT *liwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dsygvx)(La_INT *itype, const char *jobz, const char *range,
    const char *uplo, La_INT *n, double *a, La_INT *lda, double *b, La_INT *ldb, double *vl,
    double *vu, La_INT *il, La_INT *iu, double *abstol, La_INT *m, double *w, double *z,
    La_INT *ldz, double *work, La_INT *lwork, La_INT *iwork, La_INT *ifail,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dsyrfs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *a, const La_INT *lda, const double *af, const La_INT *ldaf, const La_INT *ipiv,
    const double *b, const La_INT *ldb, double *x, const La_INT *ldx, double *ferr, double *berr,
    double *work, La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dsysv)(const char *uplo, const La_INT *n, const La_INT *nrhs, double *a,
    const La_INT *lda, La_INT *ipiv, double *b, const La_INT *ldb, double *work,
    const La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(dsysvx)(const La_INT *fact, const char *uplo, const La_INT *n,
    const La_INT *nrhs, const double *a, const La_INT *lda, double *af, const La_INT *ldaf,
    La_INT *ipiv, const double *b, const La_INT *ldb, double *x, const La_INT *ldx, double *rcond,
    double *ferr, double *berr, double *work, const La_INT *lwork, La_INT *iwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(dsytd2)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    double *d, double *e, double *tau, La_INT *info FCLEN);
La_extern void F77_NAME(dsytf2)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *ipiv, La_INT *info FCLEN);
La_extern void F77_NAME(dsytrd)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    double *d, double *e, double *tau, double *work, const La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(dsytrf)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    La_INT *ipiv, double *work, const La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(dsytri)(const char *uplo, const La_INT *n, double *a, const La_INT *lda,
    const La_INT *ipiv, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dsytrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const double *a, const La_INT *lda, const La_INT *ipiv, double *b, const La_INT *ldb,
    La_INT *info FCLEN);
La_extern void F77_NAME(dtbcon)(const char *norm, const char *uplo, const char *diag,
    const La_INT *n, const La_INT *kd, const double *ab, const La_INT *ldab, double *rcond,
    double *work, La_INT *iwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtbrfs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *kd, const La_INT *nrhs, const double *ab, const La_INT *ldab,
    const double *b, const La_INT *ldb, double *x, const La_INT *ldx, double *ferr, double *berr,
    double *work, La_INT *iwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtbtrs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *kd, const La_INT *nrhs, const double *ab, const La_INT *ldab,
    double *b, const La_INT *ldb, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtfsm)(const char *transr, const char *side, const char *uplo,
    const char *trans, const char *diag, const La_INT *m, const La_INT *n, const double *alpha,
    const double *a, double *b, const La_INT *ldb FCLEN FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtgevc)(const char *side, const char *howmny, const La_LGL *select,
    const La_INT *n, const double *s, const La_INT *lds, const double *p, const La_INT *ldp,
    double *vl, const La_INT *ldvl, double *vr, const La_INT *ldvr, const La_INT *mm, La_INT *m,
    double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtgex2)(const La_LGL *wantq, const La_LGL *wantz, La_INT *n, double *a,
    La_INT *lda, double *b, La_INT *ldb, double *q, La_INT *ldq, double *z, La_INT *ldz, La_INT *j1,
    La_INT *n1, La_INT *n2, double *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dtgexc)(La_LGL *wantq, La_LGL *wantz, La_INT *n, double *a, La_INT *lda,
    double *b, La_INT *ldb, double *q, La_INT *ldq, double *z, La_INT *ldz, La_INT *ifst,
    La_INT *ilst, double *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(dtgsen)(La_INT *ijob, const La_LGL *wantq, const La_LGL *wantz,
    const La_LGL *select, La_INT *n, double *a, La_INT *lda, double *b, La_INT *ldb, double *alphar,
    double *alphai, double *beta, double *q, La_INT *ldq, double *z, La_INT *ldz, La_INT *m,
    double *pl, double *pr, double *dif, double *work, La_INT *lwork, La_INT *iwork, La_INT *liwork,
    La_INT *info);
La_extern void F77_NAME(dtgsja)(const char *jobu, const char *jobv, const char *jobq,
    const La_INT *m, const La_INT *p, const La_INT *n, const La_INT *k, const La_INT *l, double *a,
    const La_INT *lda, double *b, const La_INT *ldb, const double *tola, const double *tolb,
    double *alpha, double *beta, double *u, const La_INT *ldu, double *v, const La_INT *ldv,
    double *q, const La_INT *ldq, double *work, La_INT *ncycle, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtgsna)(const char *job, const char *howmny, La_LGL *select, La_INT *n,
    double *a, La_INT *lda, double *b, La_INT *ldb, double *vl, La_INT *ldvl, double *vr,
    La_INT *ldvr, double *s, double *dif, La_INT *mm, La_INT *m, double *work, La_INT *lwork,
    La_INT *iwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtgsy2)(const char *trans, La_INT *ijob, La_INT *m, La_INT *n, double *a,
    La_INT *lda, double *b, La_INT *ldb, double *c, La_INT *ldc, double *d, La_INT *ldd, double *e,
    La_INT *lde, double *f, La_INT *ldf, double *scale, double *rdsum, double *rdscal,
    La_INT *iwork, La_INT *pq, La_INT *info FCLEN);
La_extern void F77_NAME(dtgsyl)(const char *trans, La_INT *ijob, La_INT *m, La_INT *n, double *a,
    La_INT *lda, double *b, La_INT *ldb, double *c, La_INT *ldc, double *d, La_INT *ldd, double *e,
    La_INT *lde, double *f, La_INT *ldf, double *scale, double *dif, double *work, La_INT *lwork,
    La_INT *iwork, La_INT *info FCLEN);
La_extern void F77_NAME(dtpcon)(const char *norm, const char *uplo, const char *diag,
    const La_INT *n, const double *ap, double *rcond, double *work, La_INT *iwork,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtprfs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *nrhs, const double *ap, const double *b, const La_INT *ldb,
    double *x, const La_INT *ldx, double *ferr, double *berr, double *work, La_INT *iwork,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtptri)(const char *uplo, const char *diag, const La_INT *n, double *ap,
    La_INT *info FCLEN FCLEN);
#ifdef usePR18534fix
La_extern void F77_NAME(dtptrs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *nrhs, const double *ap, double *b, const La_INT *ldb,
    La_INT *info FCLEN FCLEN FCLEN);
#else
La_extern void F77_NAME(dtptrs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *nrhs, const double *ap, double *b, const La_INT *ldb,
    La_INT *info FCLEN FCLEN);
#endif
La_extern void F77_NAME(dtpttr)(const char *uplo, const La_INT *n, const double *ap, double *a,
    const La_INT *lda, const La_INT *info FCLEN);
La_extern void F77_NAME(dtrcon)(const char *norm, const char *uplo, const char *diag,
    const La_INT *n, const double *a, const La_INT *lda, double *rcond, double *work, La_INT *iwork,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtrevc)(const char *side, const char *howmny, const La_LGL *select,
    const La_INT *n, const double *t, const La_INT *ldt, double *vl, const La_INT *ldvl, double *vr,
    const La_INT *ldvr, const La_INT *mm, La_INT *m, double *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtrexc)(const char *compq, const La_INT *n, double *t, const La_INT *ldt,
    double *q, const La_INT *ldq, La_INT *ifst, La_INT *ilst, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(dtrrfs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *nrhs, const double *a, const La_INT *lda, const double *b,
    const La_INT *ldb, double *x, const La_INT *ldx, double *ferr, double *berr, double *work,
    La_INT *iwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtrsen)(const char *job, const char *compq, const La_LGL *select,
    const La_INT *n, double *t, const La_INT *ldt, double *q, const La_INT *ldq, double *wr,
    double *wi, La_INT *m, double *s, double *sep, double *work, const La_INT *lwork, La_INT *iwork,
    const La_INT *liwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtrsna)(const char *job, const char *howmny, const La_LGL *select,
    const La_INT *n, const double *t, const La_INT *ldt, const double *vl, const La_INT *ldvl,
    const double *vr, const La_INT *ldvr, double *s, double *sep, const La_INT *mm, La_INT *m,
    double *work, const La_INT *ldwork, La_INT *iwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtrsyl)(const char *trana, const char *tranb, const La_INT *isgn,
    const La_INT *m, const La_INT *n, const double *a, const La_INT *lda, const double *b,
    const La_INT *ldb, double *c, const La_INT *ldc, double *scale, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtrti2)(const char *uplo, const char *diag, const La_INT *n, double *a,
    const La_INT *lda, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtrtri)(const char *uplo, const char *diag, const La_INT *n, double *a,
    const La_INT *lda, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtrtrs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *nrhs, const double *a, const La_INT *lda, double *b,
    const La_INT *ldb, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(dtrttf)(const char *transr, const char *uplo, const La_INT *n,
    const double *a, const La_INT *lda, double *arf, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(dtrttp)(const char *uplo, const La_INT *n, const double *a,
    const La_INT *lda, double *ap, const La_INT *info FCLEN);
La_extern void F77_NAME(dtzrqf)(const La_INT *m, const La_INT *n, double *a, const La_INT *lda,
    double *tau, La_INT *info);
La_extern void F77_NAME(dtzrzf)(La_INT *m, La_INT *n, double *a, La_INT *lda, double *tau,
    double *work, La_INT *lwork, La_INT *info);
La_extern double F77_NAME(dzsum1)(const La_INT *n, La_complex *cx, const La_INT *incx);
La_extern int F77_NAME(ilaenv)(const La_INT *ispec, const char *name, const char *opts,
    const La_INT *n1, const La_INT *n2, const La_INT *n3, const La_INT *n4 FCLEN FCLEN);
La_extern void F77_NAME(ilaver)(La_INT *vers_major, La_INT *vers_minor, La_INT *vers_patch);
La_extern La_INT F77_NAME(izmax1)(const La_INT *n, La_complex *zx, const La_INT *incx);
La_extern La_LGL F77_NAME(lsame)(const char *ca, const char *cb FCLEN FCLEN);
La_extern void F77_NAME(zbdsqr)(const char *uplo, La_INT *n, La_INT *ncvt, La_INT *nru, La_INT *ncc,
    double *d, double *e, La_complex *vt, La_INT *ldvt, La_complex *u, La_INT *ldu, La_complex *c,
    La_INT *ldc, double *rwork, La_INT *info FCLEN);
La_extern void F77_NAME(zgebak)(const char *job, const char *side, La_INT *n, La_INT *ilo,
    La_INT *ihi, double *scale, La_INT *m, La_complex *v, La_INT *ldv, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zgebal)(const char *job, const La_INT *n, La_complex *a, const La_INT *lda,
    La_INT *ilo, La_INT *ihi, double *scale, La_INT *info FCLEN);
La_extern void F77_NAME(zgebd2)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, double *d,
    double *e, La_complex *tauq, La_complex *taup, La_complex *work, La_INT *info);
La_extern void F77_NAME(zgebrd)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, double *d,
    double *e, La_complex *tauq, La_complex *taup, La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zgecon)(const char *norm, const La_INT *n, const La_complex *a,
    const La_INT *lda, const double *anorm, double *rcond, La_complex *work, double *rwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(zgees)(const char *jobvs, const char *sort,
    La_LGL (*select)(const La_complex *), const La_INT *n, La_complex *a, const La_INT *lda,
    La_INT *sdim, La_complex *w, La_complex *vs, const La_INT *ldvs, La_complex *work,
    const La_INT *lwork, double *rwork, La_LGL *bwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zgeev)(const char *jobvl, const char *jobvr, const La_INT *n, La_complex *a,
    const La_INT *lda, La_complex *w, La_complex *vl, const La_INT *ldvl, La_complex *vr,
    const La_INT *ldvr, La_complex *work, const La_INT *lwork, double *rwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zgehd2)(La_INT *n, La_INT *ilo, La_INT *ihi, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *info);
La_extern void F77_NAME(zgehrd)(La_INT *n, La_INT *ilo, La_INT *ihi, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zgelq2)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, La_complex *tau,
    La_complex *work, La_INT *info);
La_extern void F77_NAME(zgelqf)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, La_complex *tau,
    La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zgels)(const char *trans, const La_INT *m, const La_INT *n,
    const La_INT *nrhs, La_complex *a, const La_INT *lda, La_complex *b, const La_INT *ldb,
    La_complex *work, const La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(zgelsd)(La_INT *m, La_INT *n, La_INT *nrhs, La_complex *a, La_INT *lda,
    La_complex *b, La_INT *ldb, double *s, double *rcond, La_INT *rank, La_complex *work,
    La_INT *lwork, double *rwork, La_INT *iwork, La_INT *info);
La_extern void F77_NAME(zgeqp3)(const La_INT *m, const La_INT *n, La_complex *a, const La_INT *lda,
    La_INT *jpvt, La_complex *tau, La_complex *work, const La_INT *lwork, double *rwork,
    La_INT *info);
La_extern void F77_NAME(zgeqr2)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, La_complex *tau,
    La_complex *work, La_INT *info);
La_extern void F77_NAME(zgeqrf)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, La_complex *tau,
    La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zgesdd)(const char *jobz, const La_INT *m, const La_INT *n, La_complex *a,
    const La_INT *lda, double *s, La_complex *u, const La_INT *ldu, La_complex *vt,
    const La_INT *ldvt, La_complex *work, const La_INT *lwork, double *rwork, La_INT *iwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(zgesv)(const La_INT *n, const La_INT *nrhs, La_complex *a,
    const La_INT *lda, La_INT *ipiv, La_complex *b, const La_INT *ldb, La_INT *info);
La_extern void F77_NAME(zgesvd)(const char *jobu, const char *jobvt, const La_INT *m,
    const La_INT *n, La_complex *a, const La_INT *lda, double *s, La_complex *u, const La_INT *ldu,
    La_complex *vt, const La_INT *ldvt, La_complex *work, const La_INT *lwork, double *rwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zgesvx)(const char *fact, const char *trans, const La_INT *n,
    const La_INT *nrhs, La_complex *a, const La_INT *lda, La_complex *af, const La_INT *ldaf,
    La_INT *ipiv, char *equed, double *r, double *c, La_complex *b, const La_INT *ldb,
    La_complex *x, const La_INT *ldx, double *rcond, double *ferr, double *berr, La_complex *work,
    double *rwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(zgetf2)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, La_INT *ipiv,
    La_INT *info);
La_extern void F77_NAME(zgetrf)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, La_INT *ipiv,
    La_INT *info);
La_extern void F77_NAME(zgetrf2)(La_INT *m, La_INT *n, La_complex *a, La_INT *lda, La_INT *ipiv,
    La_INT *info);
La_extern void F77_NAME(zgetri)(La_INT *n, La_complex *a, La_INT *lda, La_INT *ipiv,
    La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zgetrs)(const char *trans, La_INT *n, La_INT *nrhs, La_complex *a,
    La_INT *lda, La_INT *ipiv, La_complex *b, La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(zgges)(const char *jobvsl, const char *jobvsr, const char *sort,
    void *selctg, const La_INT *n, La_complex *a, const La_INT *lda, La_complex *b,
    const La_INT *ldb, La_INT *sdim, La_complex *alpha, La_complex *beta, La_complex *vsl,
    const La_INT *ldvsl, La_complex *vsr, const La_INT *ldvsr, La_complex *work,
    const La_INT *lwork, double *rwork, La_LGL *bwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(zggev)(const char *jobvl, const char *jobvr, const La_INT *n, La_complex *a,
    const La_INT *lda, La_complex *b, const La_INT *ldb, La_complex *alpha, La_complex *beta,
    La_complex *vl, const La_INT *ldvl, La_complex *vr, const La_INT *ldvr, La_complex *work,
    const La_INT *lwork, double *rwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zhecon)(const char *uplo, const La_INT *n, const La_complex *a,
    const La_INT *lda, const La_INT *ipiv, const double *anorm, double *rcond, La_complex *work,
    La_INT *info FCLEN);
La_extern void F77_NAME(zheev)(const char *jobz, const char *uplo, const La_INT *n, La_complex *a,
    const La_INT *lda, double *w, La_complex *work, const La_INT *lwork, double *rwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zheevd)(const char *jobz, const char *uplo, const La_INT *n, La_complex *a,
    La_INT *lda, double *w, La_complex *work, const La_INT *lwork, double *rwork,
    const La_INT *lrwork, La_INT *iwork, const La_INT *liwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zhetd2)(const char *uplo, La_INT *n, La_complex *a, La_INT *lda, double *d,
    double *e, La_complex *tau, La_INT *info FCLEN);
La_extern void F77_NAME(zhetrd)(const char *uplo, La_INT *n, La_complex *a, La_INT *lda, double *d,
    double *e, La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(zhetrf)(const char *uplo, const La_INT *n, La_complex *a, const La_INT *lda,
    La_INT *ipiv, La_complex *work, const La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(zhetri)(const char *uplo, const La_INT *n, La_complex *a, const La_INT *lda,
    const La_INT *ipiv, La_complex *work, La_INT *info FCLEN);
La_extern void F77_NAME(zhetrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const La_complex *a, const La_INT *lda, const La_INT *ipiv, La_complex *b, const La_INT *ldb,
    La_INT *info FCLEN);
La_extern void F77_NAME(zhpcon)(const char *uplo, const La_INT *n, const La_complex *ap,
    const La_INT *ipiv, const double *anorm, double *rcond, La_complex *work, La_INT *info FCLEN);
La_extern void F77_NAME(zhpev)(const char *jobz, const char *uplo, const La_INT *n, La_complex *ap,
    double *w, La_complex *z, const La_INT *ldz, La_complex *work, double *rwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zhptrf)(const char *uplo, const La_INT *n, La_complex *ap, La_INT *ipiv,
    La_INT *info FCLEN);
La_extern void F77_NAME(zhptri)(const char *uplo, const La_INT *n, La_complex *ap,
    const La_INT *ipiv, La_complex *work, La_INT *info FCLEN);
La_extern void F77_NAME(zhptrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const La_complex *ap, const La_INT *ipiv, La_complex *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(zhseqr)(const char *job, const char *compz, La_INT *n, La_INT *ilo,
    La_INT *ihi, La_complex *h, La_INT *ldh, La_complex *w, La_complex *z, La_INT *ldz,
    La_complex *work, La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zlabrd)(La_INT *m, La_INT *n, La_INT *nb, La_complex *a, La_INT *lda,
    double *d, double *e, La_complex *tauq, La_complex *taup, La_complex *x, La_INT *ldx,
    La_complex *y, La_INT *ldy);
La_extern void F77_NAME(zlacgv)(La_INT *n, La_complex *x, La_INT *incx);
La_extern void F77_NAME(zlacn2)(const La_INT *n, La_complex *v, La_complex *x, double *est,
    La_INT *kase, La_INT *isave);
La_extern void F77_NAME(zlacpy)(const char *uplo, La_INT *m, La_INT *n, La_complex *a, La_INT *lda,
    La_complex *b, La_INT *ldb FCLEN);
La_extern void F77_NAME(zlahqr)(La_INT *wantt, La_INT *wantz, La_INT *n, La_INT *ilo, La_INT *ihi,
    La_complex *h, La_INT *ldh, La_complex *w, La_INT *iloz, La_INT *ihiz, La_complex *z,
    La_INT *ldz, La_INT *info);
La_extern void F77_NAME(zlahrd)(La_INT *n, La_INT *k, La_INT *nb, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *t, La_INT *ldt, La_complex *y, La_INT *ldy);
La_extern double F77_NAME(zlange)(const char *norm, La_INT *m, La_INT *n, La_complex *a,
    La_INT *lda, double *work FCLEN);
La_extern double F77_NAME(zlanhe)(const char *norm, const char *uplo, La_INT *n, La_complex *a,
    La_INT *lda, double *work FCLEN FCLEN);
La_extern double F77_NAME(zlanhp)(const char *norm, const char *uplo, const La_INT *n,
    const La_complex *ap, double *work FCLEN FCLEN);
La_extern double F77_NAME(zlanhs)(const char *norm, La_INT *n, La_complex *a, La_INT *lda,
    double *work FCLEN);
La_extern double F77_NAME(zlansp)(const char *norm, const char *uplo, const La_INT *n,
    La_complex *ap, double *work FCLEN FCLEN);
La_extern double F77_NAME(zlansy)(const char *norm, const char *uplo, const La_INT *n,
    La_complex *a, const La_INT *lda, double *work FCLEN FCLEN);
La_extern double F77_NAME(zlantp)(const char *norm, const char *uplo, const char *diag, La_INT *n,
    La_complex *ap, double *work FCLEN FCLEN FCLEN);
La_extern double F77_NAME(zlantr)(const char *norm, const char *uplo, const char *diag,
    const La_INT *m, const La_INT *n, La_complex *a, const La_INT *lda,
    double *work FCLEN FCLEN FCLEN);
La_extern void F77_NAME(zlaqp2)(La_INT *m, La_INT *n, La_INT *offset, La_complex *a, La_INT *lda,
    La_INT *jpvt, La_complex *tau, double *vn1, double *vn2, La_complex *work);
La_extern void F77_NAME(zlaqps)(La_INT *m, La_INT *n, La_INT *offset, La_INT *nb, La_INT *kb,
    La_complex *a, La_INT *lda, La_INT *jpvt, La_complex *tau, double *vn1, double *vn2,
    La_complex *auxv, La_complex *f, La_INT *ldf);
La_extern void F77_NAME(zlarf)(const char *side, La_INT *m, La_INT *n, La_complex *v, La_INT *incv,
    La_complex *tau, La_complex *c, La_INT *ldc, La_complex *work FCLEN);
La_extern void F77_NAME(zlarfb)(const char *side, const char *trans, const char *direct,
    const char *storev, La_INT *m, La_INT *n, La_INT *k, La_complex *v, La_INT *ldv, La_complex *t,
    La_INT *ldt, La_complex *c, La_INT *ldc, La_complex *work,
    La_INT *ldwork FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(zlarfg)(La_INT *n, La_complex *alpha, La_complex *x, La_INT *incx,
    La_complex *tau);
La_extern void F77_NAME(zlarft)(const char *direct, const char *storev, La_INT *n, La_INT *k,
    La_complex *v, La_INT *ldv, La_complex *tau, La_complex *t, La_INT *ldt FCLEN FCLEN);
La_extern void F77_NAME(zlarfx)(const char *side, La_INT *m, La_INT *n, La_complex *v,
    La_complex *tau, La_complex *c, La_INT *ldc, La_complex *work FCLEN);
La_extern void F77_NAME(zlartg)(const La_complex *f, const La_complex *g, double *c, La_complex *s,
    La_complex *r);
La_extern void F77_NAME(zlascl)(const char *type, La_INT *kl, La_INT *ku, double *cfrom,
    double *cto, La_INT *m, La_INT *n, La_complex *a, La_INT *lda, La_INT *info FCLEN);
La_extern void F77_NAME(zlaset)(const char *uplo, La_INT *m, La_INT *n, La_complex *alpha,
    La_complex *beta, La_complex *a, La_INT *lda FCLEN);
La_extern void F77_NAME(zlasr)(const char *side, const char *pivot, const char *direct, La_INT *m,
    La_INT *n, double *c, double *s, La_complex *a, La_INT *lda FCLEN FCLEN FCLEN);
La_extern void F77_NAME(zlassq)(La_INT *n, La_complex *x, La_INT *incx, double *scale,
    double *sumsq);
La_extern void F77_NAME(zlaswp)(La_INT *n, La_complex *a, La_INT *lda, La_INT *k1, La_INT *k2,
    La_INT *ipiv, La_INT *incx);
La_extern void F77_NAME(zlatrd)(const char *uplo, La_INT *n, La_INT *nb, La_complex *a, La_INT *lda,
    double *e, La_complex *tau, La_complex *w, La_INT *ldw FCLEN);
La_extern void F77_NAME(zlatrs)(const char *uplo, const char *trans, const char *diag,
    const char *normin, La_INT *n, La_complex *a, La_INT *lda, La_complex *x, double *scale,
    double *cnorm, La_INT *info FCLEN FCLEN FCLEN FCLEN);
La_extern void F77_NAME(zpocon)(const char *uplo, const La_INT *n, const La_complex *a,
    const La_INT *lda, const double *anorm, double *rcond, La_complex *work, double *rwork,
    La_INT *info FCLEN);
La_extern void F77_NAME(zpotrf)(const char *uplo, const La_INT *n, La_complex *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(zpotrf2)(const char *uplo, const La_INT *n, La_complex *a,
    const La_INT *lda, La_INT *info FCLEN);
La_extern void F77_NAME(zpotri)(const char *uplo, const La_INT *n, La_complex *a, const La_INT *lda,
    La_INT *info FCLEN);
La_extern void F77_NAME(zpotrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const La_complex *a, const La_INT *lda, La_complex *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(zppcon)(const char *uplo, const La_INT *n, const La_complex *ap,
    const double *anorm, double *rcond, La_complex *work, double *rwork, La_INT *info FCLEN);
La_extern void F77_NAME(zpptrf)(const char *uplo, const La_INT *n, La_complex *ap,
    La_INT *info FCLEN);
La_extern void F77_NAME(zpptri)(const char *uplo, const La_INT *n, La_complex *ap,
    La_INT *info FCLEN);
La_extern void F77_NAME(zpptrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const La_complex *ap, La_complex *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(zpstrf)(const char *uplo, const La_INT *n, La_complex *a, const La_INT *lda,
    La_INT *piv, La_INT *rank, const double *tol, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(zrot)(const La_INT *n, La_complex *cx, const La_INT *incx, La_complex *cy,
    const La_INT *incy, double *c, La_complex *s);
La_extern void F77_NAME(zspcon)(const char *uplo, const La_INT *n, const La_complex *ap,
    const La_INT *ipiv, const double *anorm, double *rcond, La_complex *work, La_INT *info FCLEN);
La_extern void F77_NAME(zsptrf)(const char *uplo, const La_INT *n, La_complex *ap, La_INT *ipiv,
    La_INT *info FCLEN);
La_extern void F77_NAME(zsptri)(const char *uplo, const La_INT *n, La_complex *ap, La_INT *ipiv,
    La_complex *work, La_INT *info FCLEN);
La_extern void F77_NAME(zsptrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const La_complex *ap, const La_INT *ipiv, La_complex *b, const La_INT *ldb, La_INT *info FCLEN);
La_extern void F77_NAME(zsteqr)(const char *compz, La_INT *n, double *d, double *e, La_complex *z,
    La_INT *ldz, double *work, La_INT *info FCLEN);
La_extern void F77_NAME(zsycon)(const char *uplo, const La_INT *n, const La_complex *a,
    const La_INT *lda, La_INT *ipiv, const double *anorm, double *rcond, La_complex *work,
    La_INT *info FCLEN);
La_extern void F77_NAME(zsymv)(const char *uplo, const La_INT *n, const La_complex *alpha,
    const La_complex *a, const La_INT *lda, const La_complex *x, const La_INT *incx,
    const La_complex *beta, La_complex *y, const La_INT *incy FCLEN);
La_extern void F77_NAME(zsytrf)(const char *uplo, const La_INT *n, La_complex *a, const La_INT *lda,
    La_INT *ipiv, La_complex *work, const La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(zsytri)(const char *uplo, const La_INT *n, La_complex *a, const La_INT *lda,
    const La_INT *ipiv, La_complex *work, La_INT *info FCLEN);
La_extern void F77_NAME(zsytrs)(const char *uplo, const La_INT *n, const La_INT *nrhs,
    const La_complex *a, const La_INT *lda, const La_INT *ipiv, La_complex *b, const La_INT *ldb,
    La_INT *info FCLEN);
La_extern void F77_NAME(ztgsen)(const La_INT *ijob, const La_LGL *wantq, const La_LGL *wantz,
    const La_LGL *select, const La_INT *n, La_complex *a, const La_INT *lda, La_complex *b,
    const La_INT *ldb, La_complex *alpha, La_complex *beta, La_complex *q, const La_INT *ldq,
    La_complex *z, const La_INT *ldz, La_INT *m, double *pl, double *pr, double *dif,
    La_complex *work, const La_INT *lwork, La_INT *iwork, const La_INT *liwork, La_INT *info);
La_extern void F77_NAME(ztpcon)(const char *norm, const char *uplo, const char *diag,
    const La_INT *n, const La_complex *ap, double *rcond, La_complex *work, double *rwork,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(ztptri)(const char *uplo, const char *diag, const La_INT *n, La_complex *ap,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(ztptrs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *nrhs, const La_complex *ap, La_complex *b, const La_INT *ldb,
    La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(ztrcon)(const char *norm, const char *uplo, const char *diag,
    const La_INT *n, const La_complex *a, const La_INT *lda, double *rcond, La_complex *work,
    double *rwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(ztrevc)(const char *side, const char *howmny, La_LGL *select, La_INT *n,
    La_complex *t, La_INT *ldt, La_complex *vl, La_INT *ldvl, La_complex *vr, La_INT *ldvr,
    La_INT *mm, La_INT *m, La_complex *work, double *rwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(ztrsen)(const char *job, const char *compq, const La_LGL *select,
    const La_INT *n, La_complex *t, const La_INT *ldt, La_complex *q, const La_INT *ldq,
    La_complex *w, La_INT *m, double *s, double *sep, La_complex *work, const La_INT *lwork,
    La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(ztrsyl)(const char *trana, const char *tranb, const La_INT *isgn,
    const La_INT *m, const La_INT *n, const La_complex *a, const La_INT *lda, const La_complex *b,
    const La_INT *ldb, La_complex *c, const La_INT *ldc, double *scale, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(ztrtri)(const char *uplo, const char *diag, const La_INT *n, La_complex *a,
    const La_INT *lda, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(ztrtrs)(const char *uplo, const char *trans, const char *diag,
    const La_INT *n, const La_INT *nrhs, La_complex *a, const La_INT *lda, La_complex *b,
    const La_INT *ldb, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(zung2l)(La_INT *m, La_INT *n, La_INT *k, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *info);
La_extern void F77_NAME(zung2r)(La_INT *m, La_INT *n, La_INT *k, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *info);
La_extern void F77_NAME(zungbr)(const char *vect, La_INT *m, La_INT *n, La_INT *k, La_complex *a,
    La_INT *lda, La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(zunghr)(La_INT *n, La_INT *ilo, La_INT *ihi, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zungl2)(La_INT *m, La_INT *n, La_INT *k, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *info);
La_extern void F77_NAME(zunglq)(La_INT *m, La_INT *n, La_INT *k, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zungql)(La_INT *m, La_INT *n, La_INT *k, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zungqr)(La_INT *m, La_INT *n, La_INT *k, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zungr2)(La_INT *m, La_INT *n, La_INT *k, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *info);
La_extern void F77_NAME(zungrq)(La_INT *m, La_INT *n, La_INT *k, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info);
La_extern void F77_NAME(zungtr)(const char *uplo, La_INT *n, La_complex *a, La_INT *lda,
    La_complex *tau, La_complex *work, La_INT *lwork, La_INT *info FCLEN);
La_extern void F77_NAME(zunm2r)(const char *side, const char *trans, La_INT *m, La_INT *n,
    La_INT *k, La_complex *a, La_INT *lda, La_complex *tau, La_complex *c, La_INT *ldc,
    La_complex *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zunmbr)(const char *vect, const char *side, const char *trans, La_INT *m,
    La_INT *n, La_INT *k, La_complex *a, La_INT *lda, La_complex *tau, La_complex *c, La_INT *ldc,
    La_complex *work, La_INT *lwork, La_INT *info FCLEN FCLEN FCLEN);
La_extern void F77_NAME(zunml2)(const char *side, const char *trans, La_INT *m, La_INT *n,
    La_INT *k, La_complex *a, La_INT *lda, La_complex *tau, La_complex *c, La_INT *ldc,
    La_complex *work, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zunmlq)(const char *side, const char *trans, La_INT *m, La_INT *n,
    La_INT *k, La_complex *a, La_INT *lda, La_complex *tau, La_complex *c, La_INT *ldc,
    La_complex *work, La_INT *lwork, La_INT *info FCLEN FCLEN);
La_extern void F77_NAME(zunmqr)(const char *side, const char *trans, const La_INT *m,
    const La_INT *n, const La_INT *k, La_complex *a, const La_INT *lda, La_complex *tau,
    La_complex *c, const La_INT *ldc, La_complex *work, const La_INT *lwork,
    La_INT *info FCLEN FCLEN);

#ifdef __cplusplus
}
#endif

#endif
