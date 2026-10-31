/* Evaluates Rmath.h functions by name, so one table of cases can be run against GNU R's
   libR (to record expectations) and against ry's (to check them). Built against either
   header set unchanged, it also checks that ry's Rmath.h is source compatible. */

#include <R.h>
#include <Rinternals.h>
#include <Rmath.h>
#include <R_ext/Rdynload.h>
#include <string.h>

enum kind {
    F1,  /* f(x) */
    F2,  /* f(x, a) */
    F3,  /* f(x, a, b) */
    D2,  /* d(x, a, log) */
    D3,  /* d(x, a, b, log) */
    D4,  /* d(x, a, b, c, log) */
    P2,  /* p(x, a, lower, log) */
    P3,  /* p(x, a, b, lower, log) */
    P4,  /* p(x, a, b, c, lower, log) */
    POWDI,
    R0,  /* r() */
    R1,  /* r(a) */
    R2,  /* r(a, b) */
    R3   /* r(a, b, c) */
};

typedef double (*fn1)(double);
typedef double (*fn2)(double, double);
typedef double (*fn3)(double, double, double);
typedef double (*fn4)(double, double, double, double);
typedef double (*fn5)(double, double, double, double, double);
typedef double (*fn6)(double, double, double, double, double, double);
typedef double (*fn0)(void);
typedef double (*fnd2)(double, double, int);
typedef double (*fnd3)(double, double, double, int);
typedef double (*fnd4)(double, double, double, double, int);
typedef double (*fnp2)(double, double, int, int);
typedef double (*fnp3)(double, double, double, int, int);
typedef double (*fnp4)(double, double, double, double, int, int);

struct entry {
    const char *name;
    enum kind kind;
    void (*function)(void);
};

/* The name is the one written in the table, before Rmath.h's remapping. */
#define E(name, kind) { #name, kind, (void (*)(void)) name }

static const struct entry entries[] = {
    E(gammafn, F1), E(lgammafn, F1), E(lgamma1p, F1), E(digamma, F1), E(trigamma, F1),
    E(tetragamma, F1), E(pentagamma, F1), E(log1pmx, F1), E(log1pexp, F1),
    E(log1mexp, F1), E(sign, F1), E(ftrunc, F1), E(cospi, F1), E(sinpi, F1),
    E(Rtanpi, F1),
    E(beta, F2), E(lbeta, F2), E(choose, F2), E(lchoose, F2), E(psigamma, F2),
    E(fmax2, F2), E(fmin2, F2), E(fprec, F2), E(fround, F2), E(fsign, F2),
    E(logspace_add, F2), E(logspace_sub, F2), E(R_pow, F2), E(bessel_j, F2),
    E(bessel_y, F2),
    E(bessel_i, F3), E(bessel_k, F3),
    E(dchisq, D2), E(dexp, D2), E(dgeom, D2), E(dpois, D2), E(dpois_raw, D2), E(dt, D2),
    E(dsignrank, D2),
    E(dnorm, D3), E(dunif, D3), E(dgamma, D3), E(dbeta, D3), E(dlnorm, D3), E(df, D3),
    E(dbinom, D3), E(dcauchy, D3), E(dlogis, D3), E(dweibull, D3), E(dnbinom, D3),
    E(dnbinom_mu, D3), E(dnchisq, D3), E(dnt, D3), E(dwilcox, D3),
    E(dhyper, D4), E(dnbeta, D4), E(dnf, D4), E(dbinom_raw, D4),
    E(pchisq, P2), E(qchisq, P2), E(pexp, P2), E(qexp, P2), E(pgeom, P2), E(qgeom, P2),
    E(ppois, P2), E(qpois, P2), E(pt, P2), E(qt, P2), E(psignrank, P2), E(qsignrank, P2),
    E(pnorm, P3), E(qnorm, P3), E(punif, P3), E(qunif, P3), E(pgamma, P3), E(qgamma, P3),
    E(pbeta, P3), E(qbeta, P3), E(plnorm, P3), E(qlnorm, P3), E(pf, P3), E(qf, P3),
    E(pbinom, P3), E(qbinom, P3), E(pcauchy, P3), E(qcauchy, P3), E(plogis, P3),
    E(qlogis, P3), E(pweibull, P3), E(qweibull, P3), E(pnbinom, P3), E(qnbinom, P3),
    E(pnbinom_mu, P3), E(qnbinom_mu, P3), E(pnchisq, P3), E(qnchisq, P3), E(pnt, P3),
    E(qnt, P3), E(pwilcox, P3), E(qwilcox, P3),
    E(phyper, P4), E(qhyper, P4), E(pnbeta, P4), E(qnbeta, P4), E(pnf, P4), E(qnf, P4),
    E(ptukey, P4), E(qtukey, P4),
    { "R_pow_di", POWDI, (void (*)(void)) R_pow_di },
    E(norm_rand, R0), E(exp_rand, R0), E(unif_rand, R0),
    E(rchisq, R1), E(rexp, R1), E(rgeom, R1), E(rpois, R1), E(rt, R1), E(rsignrank, R1),
    E(rnorm, R2), E(runif, R2), E(rgamma, R2), E(rbeta, R2), E(rlnorm, R2), E(rnchisq, R2),
    E(rf, R2), E(rbinom, R2), E(rcauchy, R2), E(rnbinom, R2), E(rnbinom_mu, R2),
    E(rweibull, R2), E(rlogis, R2), E(rwilcox, R2),
    E(rhyper, R3),
};

static const struct entry *find(const char *name)
{
    for (size_t i = 0; i < sizeof entries / sizeof entries[0]; i++)
        if (strcmp(entries[i].name, name) == 0)
            return &entries[i];
    return NULL;
}

/* Functions whose results are not one double get names of their own. */
static int special(const char *name, const double *a, double *result)
{
    if (strcmp(name, "pnorm_both.cum") == 0 || strcmp(name, "pnorm_both.ccum") == 0) {
        double cum, ccum;
        pnorm_both(a[0], &cum, &ccum, (int) a[1], (int) a[2]);
        *result = strcmp(name, "pnorm_both.cum") == 0 ? cum : ccum;
        return 1;
    }
    if (strcmp(name, "lgammafn_sign.sign") == 0) {
        int sign = 0;
        lgammafn_sign(a[0], &sign);
        *result = sign;
        return 1;
    }
    if (strcmp(name, "dpsifn") == 0 || strcmp(name, "dpsifn.ierr") == 0) {
        double ans[1] = {0};
        int nz = 0, ierr = 0;
        dpsifn(a[0], (int) a[1], (int) a[2], 1, ans, &nz, &ierr);
        *result = strcmp(name, "dpsifn") == 0 ? ans[0] : ierr;
        return 1;
    }
    if (strcmp(name, "logspace_sum") == 0) {
        *result = logspace_sum(a + 1, (int) a[0]);
        return 1;
    }
    if (strcmp(name, "imax2") == 0) {
        *result = imax2((int) a[0], (int) a[1]);
        return 1;
    }
    if (strcmp(name, "imin2") == 0) {
        *result = imin2((int) a[0], (int) a[1]);
        return 1;
    }
    return 0;
}

static double evaluate(const struct entry *e, const double *a)
{
    switch (e->kind) {
    case F1: return ((fn1) e->function)(a[0]);
    case F2: return ((fn2) e->function)(a[0], a[1]);
    case F3: return ((fn3) e->function)(a[0], a[1], a[2]);
    case D2: return ((fnd2) e->function)(a[0], a[1], (int) a[2]);
    case D3: return ((fnd3) e->function)(a[0], a[1], a[2], (int) a[3]);
    case D4: return ((fnd4) e->function)(a[0], a[1], a[2], a[3], (int) a[4]);
    case P2: return ((fnp2) e->function)(a[0], a[1], (int) a[2], (int) a[3]);
    case P3: return ((fnp3) e->function)(a[0], a[1], a[2], (int) a[3], (int) a[4]);
    case P4: return ((fnp4) e->function)(a[0], a[1], a[2], a[3], (int) a[4], (int) a[5]);
    case POWDI: return R_pow_di(a[0], (int) a[1]);
    case R0: return ((fn0) e->function)();
    case R1: return ((fn1) e->function)(a[0]);
    case R2: return ((fn2) e->function)(a[0], a[1]);
    case R3: return ((fn3) e->function)(a[0], a[1], a[2]);
    }
    return NA_REAL;
}

#define ARITY 6

/* names: n function names; args: 6 n doubles, the arguments of case i at 6 i. */
SEXP rmath_eval(SEXP names, SEXP args)
{
    R_xlen_t n = XLENGTH(names);
    if (XLENGTH(args) != ARITY * n)
        error("rmath_eval needs %d arguments per case", ARITY);
    SEXP result = PROTECT(allocVector(REALSXP, n));
    const double *a = REAL(args);
    double *out = REAL(result);
    for (R_xlen_t i = 0; i < n; i++) {
        const char *name = CHAR(STRING_ELT(names, i));
        double value;
        if (special(name, a + ARITY * i, &value)) {
            out[i] = value;
            continue;
        }
        const struct entry *e = find(name);
        if (e == NULL)
            error("rmath_eval: unknown function '%s'", name);
        out[i] = evaluate(e, a + ARITY * i);
    }
    UNPROTECT(1);
    return result;
}

/* count draws of a random variate generator, between GetRNGstate and PutRNGstate. For
   rmultinom, args holds the size followed by the probabilities, and each draw contributes
   one count per category. */
SEXP rmath_random(SEXP name_, SEXP args, SEXP count_)
{
    const char *name = CHAR(STRING_ELT(name_, 0));
    int count = asInteger(count_);
    const double *a = REAL(args);
    SEXP result;
    GetRNGstate();
    if (strcmp(name, "rmultinom") == 0) {
        int k = (int) XLENGTH(args) - 1;
        double *probabilities = (double *) R_alloc(k, sizeof(double));
        int *drawn = (int *) R_alloc(k, sizeof(int));
        for (int j = 0; j < k; j++)
            probabilities[j] = a[j + 1];
        result = PROTECT(allocVector(REALSXP, (R_xlen_t) count * k));
        for (int i = 0; i < count; i++) {
            rmultinom((int) a[0], probabilities, k, drawn);
            for (int j = 0; j < k; j++)
                REAL(result)[(R_xlen_t) i * k + j] = drawn[j];
        }
    } else {
        const struct entry *e = find(name);
        if (e == NULL || e->kind < R0)
            error("rmath_random: unknown generator '%s'", name);
        double padded[ARITY] = {0};
        for (R_xlen_t j = 0; j < XLENGTH(args) && j < ARITY; j++)
            padded[j] = a[j];
        result = PROTECT(allocVector(REALSXP, count));
        for (int i = 0; i < count; i++)
            REAL(result)[i] = evaluate(e, padded);
    }
    PutRNGstate();
    UNPROTECT(1);
    return result;
}

/* Constants and remapped names: the header must spell them as GNU R's does. */
SEXP rmath_constants(void)
{
    SEXP result = PROTECT(allocVector(REALSXP, 8));
    REAL(result)[0] = M_LN2;
    REAL(result)[1] = M_1_SQRT_2PI;
    REAL(result)[2] = M_LN_SQRT_2PI;
    REAL(result)[3] = M_LOG10_2;
    REAL(result)[4] = M_PI_2;
    REAL(result)[5] = M_SQRT_32;
    REAL(result)[6] = M_LN_SQRT_PId2;
    REAL(result)[7] = M_2PI;
    UNPROTECT(1);
    return result;
}

static const R_CallMethodDef call_entries[] = {
    {"rmath_eval", (DL_FUNC) &rmath_eval, 2},
    {"rmath_random", (DL_FUNC) &rmath_random, 3},
    {"rmath_constants", (DL_FUNC) &rmath_constants, 0},
    {NULL, NULL, 0}
};

void R_init_rmathtest(DllInfo *dll)
{
    R_registerRoutines(dll, NULL, call_entries, NULL, NULL);
    R_useDynamicSymbols(dll, FALSE);
}
