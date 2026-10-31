/*
 * Conformance cases for R's numerical C API (R_ext/Applic.h, R_ext/Linpack.h, R_ext/Utils.h).
 * The same source is built against GNU R and against ry; every case returns its results as
 * strings ("%.17g" for numbers), and the expectation file records GNU R's answers.
 */

#include <math.h>
#include <stdio.h>
#include <string.h>

/* RS.h first, so Utils.h declares its Fortran entry points. */
#include <R_ext/RS.h>
#include <R.h>
#include <Rinternals.h>
#include <R_ext/Applic.h>
#include <R_ext/Linpack.h>
#include <R_ext/Utils.h>

/* No header declares R's Fortran printing and signalling helpers; packages declare them. */
void F77_NAME(dblepr)(const char *label, int *nchar, double *data, int *ndata);
void F77_NAME(intpr)(const char *label, int *nchar, int *data, int *ndata);
void F77_NAME(rexitc)(const char *message, int *nchar);
void F77_NAME(rwarnc)(const char *message, int *nchar);
/* Utils.h declares these only from R 4.4. */
double F77_NAME(d1mach)(int *i);
int F77_NAME(i1mach)(int *i);

#define MAX_TOKENS 4096

static char tokens[MAX_TOKENS][80];
static int token_count;

static void put_text(const char *text)
{
    if (token_count < MAX_TOKENS)
        snprintf(tokens[token_count++], sizeof tokens[0], "%s", text);
}

static void put(double value)
{
    char buffer[80];
    if (ISNA(value))
        snprintf(buffer, sizeof buffer, "NA");
    else if (ISNAN(value))
        snprintf(buffer, sizeof buffer, "NaN");
    else
        snprintf(buffer, sizeof buffer, "%.17g", value);
    put_text(buffer);
}

static void put_int(int value)
{
    put(value == NA_INTEGER ? NA_REAL : (double) value);
}

static void put_all(const double *values, int count)
{
    for (int i = 0; i < count; i++)
        put(values[i]);
}

static void put_ints(const int *values, int count)
{
    for (int i = 0; i < count; i++)
        put_int(values[i]);
}

/* A fixed pseudo-random stream, so inputs are the same in every runtime. */
static unsigned int lcg_state;

static double lcg(void)
{
    lcg_state = lcg_state * 1103515245u + 12345u;
    return (double) ((lcg_state >> 8) & 0xffff) / 65536.0;
}

static void fill(double *values, int count, unsigned int seed, int levels)
{
    lcg_state = seed;
    for (int i = 0; i < count; i++)
        values[i] = levels > 0 ? floor(lcg() * levels) : lcg() * 10 - 5;
}

/* ---- sorting ---- */

static void case_sorts(void)
{
    double v[60], w[60];
    int index[60], iv[60];
    fill(v, 60, 7, 12);
    memcpy(w, v, sizeof v);
    for (int i = 0; i < 60; i++)
        index[i] = i + 1;
    R_qsort_I(w, index, 1, 60);
    put_all(w, 60);
    put_ints(index, 60);
    memcpy(w, v, sizeof v);
    R_qsort(w, 5, 50);
    put_all(w, 60);
    for (int i = 0; i < 60; i++) {
        iv[i] = (int) v[i];
        index[i] = i + 1;
    }
    R_qsort_int_I(iv, index, 1, 60);
    put_ints(iv, 60);
    put_ints(index, 60);
    for (int i = 0; i < 60; i++)
        iv[i] = (int) v[59 - i];
    R_qsort_int(iv, 1, 60);
    put_ints(iv, 60);
    int first = 3, last = 40;
    memcpy(w, v, sizeof v);
    for (int i = 0; i < 60; i++)
        index[i] = i + 1;
    F77_CALL(qsort4)(w, index, &first, &last);
    put_all(w, 60);
    put_ints(index, 60);
}

static void case_shell_sorts(void)
{
    double v[40];
    int index[40], iv[40];
    fill(v, 40, 11, 6);
    v[3] = NA_REAL;
    v[17] = R_NaN;
    v[30] = R_NegInf;
    v[31] = R_PosInf;
    double w[40];
    memcpy(w, v, sizeof v);
    R_rsort(w, 40);
    put_all(w, 40);
    memcpy(w, v, sizeof v);
    for (int i = 0; i < 40; i++)
        index[i] = i + 1;
    rsort_with_index(w, index, 40);
    put_all(w, 40);
    put_ints(index, 40);
    for (int i = 0; i < 40; i++)
        iv[i] = i % 7 == 3 ? NA_INTEGER : 3 * i % 11 - 4;
    R_isort(iv, 40);
    put_ints(iv, 40);
    fill(w, 40, 23, 9);
    for (int i = 0; i < 40; i++)
        index[i] = i + 1;
    revsort(w, index, 40);
    put_all(w, 40);
    put_ints(index, 40);
    Rcomplex z[20];
    lcg_state = 5;
    for (int i = 0; i < 20; i++) {
        z[i].r = floor(lcg() * 4);
        z[i].i = floor(lcg() * 4);
    }
    z[4].r = NA_REAL;
    R_csort(z, 20);
    for (int i = 0; i < 20; i++) {
        put(z[i].r);
        put(z[i].i);
    }
}

static void case_partial_sorts(void)
{
    double v[30];
    int iv[30];
    for (int k = 0; k < 30; k += 7) {
        fill(v, 30, 31 + k, 8);
        v[2] = NA_REAL;
        rPsort(v, 30, k);
        put_all(v, 30);
        for (int i = 0; i < 30; i++)
            iv[i] = (int) v[(i * 7) % 30 == 2 ? 0 : (i * 7) % 30];
        iv[5] = NA_INTEGER;
        iPsort(iv, 30, k);
        put_ints(iv, 30);
    }
    Rcomplex z[12];
    lcg_state = 9;
    for (int i = 0; i < 12; i++) {
        z[i].r = floor(lcg() * 3);
        z[i].i = floor(lcg() * 5);
    }
    cPsort(z, 12, 6);
    for (int i = 0; i < 12; i++) {
        put(z[i].r);
        put(z[i].i);
    }
}

/* ---- intervals, max.col ---- */

static void case_intervals(void)
{
    double knots[] = {1, 2, 2, 3, 5, 5, 8};
    double probes[] = {-1, 1, 1.5, 2, 2.5, 3, 4.9, 5, 6, 8, 9};
    for (int left_open = 0; left_open < 2; left_open++)
        for (int closed = 0; closed < 2; closed++)
            for (int inside = 0; inside < 2; inside++)
                for (int p = 0; p < 11; p++) {
                    int flag = 99;
                    put_int(findInterval2(knots, 7, probes[p], closed, inside, left_open, 1, &flag));
                    put_int(flag);
                }
    int n = 7, ilo = 1, closed = 1, inside = 0, flag;
    for (int p = 0; p < 11; p++) {
        /* GNU R before 4.5 declares the flags as Rboolean *, later ones as int *. */
        put_int(F77_CALL(interv)(knots, &n, &probes[p], (void *) &closed, (void *) &inside, &ilo,
                                 &flag));
        put_int(findInterval(knots, 7, probes[p], FALSE, TRUE, 1, &flag));
    }
}

static void case_max_col(void)
{
    double m[] = {1, 5, 3, 7, 5, 2, 7, 5, 3, NA_REAL, 0, 0};
    int nr = 4, nc = 3, maxes[4];
    for (int method = 2; method <= 3; method++) {
        R_max_col(m, &nr, &nc, maxes, &method);
        put_ints(maxes, 4);
    }
}

static void case_max_col_random(void)
{
    double m[60];
    fill(m, 60, 77, 3);
    int nr = 12, nc = 5, maxes[12], method = 1;
    GetRNGstate();
    R_max_col(m, &nr, &nc, maxes, &method);
    PutRNGstate();
    put_ints(maxes, 12);
}

/* ---- LINPACK ---- */

static void design(double *x, int n, int p, unsigned int seed)
{
    fill(x, n * p, seed, 0);
}

static void case_dqrdc2(void)
{
    double x[8 * 5], qraux[5], work[10], tol = 1e-7;
    int n = 8, p = 5, rank, pivot[5];
    design(x, n, p, 3);
    for (int i = 0; i < n; i++)
        x[i + 2 * n] = x[i] + 2 * x[i + n]; /* column 3 is collinear */
    for (int j = 0; j < p; j++)
        pivot[j] = j + 1;
    F77_CALL(dqrdc2)(x, &n, &n, &p, &tol, &rank, qraux, pivot, work);
    put_int(rank);
    put_ints(pivot, p);
    /* The columns past the rank hold rounding noise, which no two implementations share. */
    put_all(qraux, rank);
    put_all(x, n * rank);
}

static void case_dqrls(void)
{
    double x[10 * 3], y[10 * 2], b[3 * 2], rsd[10 * 2], qty[10 * 2], qraux[3], work[6];
    double tol = 1e-7;
    int n = 10, p = 3, ny = 2, k, jpvt[3] = {1, 2, 3};
    design(x, n, p, 17);
    fill(y, n * ny, 19, 0);
    F77_CALL(dqrls)(x, &n, &p, y, &ny, &tol, b, rsd, qty, &k, jpvt, qraux, work);
    put_int(k);
    put_ints(jpvt, p);
    put_all(b, p * ny);
    put_all(rsd, n * ny);
    put_all(qty, n * ny);
    put_all(qraux, p);
    /* The dqrutl helpers on the same decomposition. */
    double out[10 * 2], yy[10 * 2], coef[3 * 2];
    int info;
    fill(yy, n * ny, 19, 0);
    F77_CALL(dqrqty)(x, &n, &k, qraux, yy, &ny, out);
    put_all(out, n * ny);
    F77_CALL(dqrqy)(x, &n, &k, qraux, yy, &ny, out);
    put_all(out, n * ny);
    F77_CALL(dqrcf)(x, &n, &k, qraux, yy, &ny, coef, &info);
    put_all(coef, k * ny);
    put_int(info);
    fill(yy, n * ny, 19, 0);
    F77_CALL(dqrrsd)(x, &n, &k, qraux, yy, &ny, out);
    put_all(out, n * ny);
    fill(yy, n * ny, 19, 0);
    F77_CALL(dqrxb)(x, &n, &k, qraux, yy, &ny, out);
    put_all(out, n * ny);
}

static void case_dqrdc_dqrsl(void)
{
    double x[7 * 4], qraux[4], work[4];
    int n = 7, p = 4, jpvt[4] = {0, 1, 0, -1}, job = 1;
    design(x, n, p, 41);
    F77_CALL(dqrdc)(x, &n, &n, &p, qraux, jpvt, work, &job);
    put_ints(jpvt, p);
    put_all(qraux, p);
    put_all(x, n * p);
    double y[7], qy[7], qty[7], b[4], rsd[7], xb[7];
    int k = 4, info, all = 11111;
    fill(y, n, 43, 0);
    F77_CALL(dqrsl)(x, &n, &n, &k, qraux, y, qy, qty, b, rsd, xb, &all, &info);
    put_all(qy, n);
    put_all(qty, n);
    put_all(b, k);
    put_all(rsd, n);
    put_all(xb, n);
    put_int(info);
}

static void case_triangular(void)
{
    double t[4 * 4], b[4], z[4], rcond;
    int n = 4, info;
    for (int job = 0; job <= 11; job += (job % 10 == 0 ? 1 : 9)) {
        design(t, n, n, 51 + job);
        for (int i = 0; i < n; i++)
            t[i + i * n] += 3;
        fill(b, n, 53, 0);
        F77_CALL(dtrsl)(t, &n, &n, b, &job, &info);
        put_all(b, n);
        put_int(info);
        int lower = job % 10 == 0 ? 0 : 1;
        F77_CALL(dtrco)(t, &n, &n, &rcond, z, &lower);
        put(rcond);
        put_all(z, n);
    }
    t[5] = 0;
    fill(b, n, 53, 0);
    int job = 1;
    F77_CALL(dtrsl)(t, &n, &n, b, &job, &info);
    put_int(info);
}

static void case_positive_definite(void)
{
    double g[5 * 5], a[5 * 5], b[5], z[5], det[2], rcond;
    int n = 5, info, job = 11;
    design(g, n, n, 61);
    for (int i = 0; i < n; i++)
        for (int j = 0; j < n; j++) {
            double s = i == j ? 1 : 0;
            for (int k = 0; k < n; k++)
                s += g[k + i * n] * g[k + j * n];
            a[i + j * n] = s;
        }
    double saved[25];
    memcpy(saved, a, sizeof a);
    F77_CALL(dpoco)(a, &n, &n, &rcond, z, &info);
    put(rcond);
    put_int(info);
    put_all(z, n);
    fill(b, n, 63, 0);
    F77_CALL(dposl)(a, &n, &n, b);
    put_all(b, n);
    F77_CALL(dpodi)(a, &n, &n, det, &job);
    put_all(det, 2);
    put_all(a, n * n);
    memcpy(a, saved, sizeof a);
    F77_CALL(dpofa)(a, &n, &n, &info);
    put_int(info);
    put_all(a, n * n);
    /* Not positive definite. */
    a[0] = -1;
    F77_CALL(dpofa)(a, &n, &n, &info);
    put_int(info);
    /* Banded: tridiagonal with 4 on the diagonal and 1 off it, in LINPACK's layout. */
    double band[2 * 6], rhs[6];
    int m = 1, size = 6, lda = 2;
    for (int j = 0; j < size; j++) {
        band[0 + 2 * j] = j == 0 ? 0 : 1;
        band[1 + 2 * j] = 4;
        rhs[j] = j + 1;
    }
    F77_CALL(dpbfa)(band, &lda, &size, &m, &info);
    put_int(info);
    F77_CALL(dpbsl)(band, &lda, &size, &m, rhs);
    put_all(rhs, size);
}

static void case_svd(void)
{
    for (int shape = 0; shape < 3; shape++) {
        int n = shape == 1 ? 3 : 6, p = shape == 1 ? 5 : 4;
        double x[30], s[6], e[6], u[36], v[25], work[6];
        int job = shape == 2 ? 21 : 11, info;
        design(x, n, p, 71 + shape);
        if (shape == 2)
            for (int i = 0; i < n; i++)
                x[i + 3 * n] = x[i] - x[i + n];
        F77_CALL(dsvdc)(x, &n, &n, &p, s, e, u, &n, v, &p, work, &job, &info);
        int count = n < p ? n : p;
        put_int(info);
        put_all(s, count);
        /* Singular vectors are determined up to sign; report products that are not. */
        int ncu = job / 10 == 1 ? n : count;
        for (int k = 0; k < count; k++) {
            /* A left singular vector for a zero singular value is not unique. */
            if (s[k] < 1e-10)
                continue;
            double sign = 1;
            for (int i = 0; i < n; i++)
                if (fabs(u[i + k * n]) > 1e-8) {
                    sign = u[i + k * n] < 0 ? -1 : 1;
                    break;
                }
            for (int i = 0; i < n; i++)
                put(sign * u[i + k * n]);
            for (int i = 0; i < p; i++)
                put(sign * v[i + k * p]);
        }
        put_int(ncu);
    }
}

static void case_machine(void)
{
    for (int i = 1; i <= 5; i++)
        put(F77_CALL(d1mach)(&i));
    for (int i = 1; i <= 16; i++)
        put_int(F77_CALL(i1mach)(&i));
}

/* ---- optimizers ---- */

static int evaluations;

static double rosenbrock(int n, double *p, void *ex)
{
    evaluations++;
    double a = p[1] - p[0] * p[0], b = 1 - p[0];
    return 100 * a * a + b * b;
}

static void rosenbrock_gradient(int n, double *p, double *g, void *ex)
{
    double a = p[1] - p[0] * p[0];
    g[0] = -400 * p[0] * a - 2 * (1 - p[0]);
    g[1] = 200 * a;
}

/* A smooth function of four variables whose minimum is at (1, 2, 3, 4). */
static double quartic(int n, double *p, void *ex)
{
    double s = 0;
    for (int i = 0; i < n; i++) {
        double d = p[i] - (i + 1);
        s += d * d + 0.1 * d * d * d * d + 0.5 * d * (i > 0 ? p[i - 1] - i : 0);
    }
    return s;
}

static void quartic_gradient(int n, double *p, double *g, void *ex)
{
    for (int i = 0; i < n; i++)
        g[i] = 0;
    for (int i = 0; i < n; i++) {
        double d = p[i] - (i + 1);
        double previous = i > 0 ? p[i - 1] - i : 0;
        g[i] += 2 * d + 0.4 * d * d * d + 0.5 * previous;
        if (i > 0)
            g[i - 1] += 0.5 * d;
    }
}

static void case_vmmin(void)
{
    double b[2] = {-1.2, 1}, fmin;
    int mask[2] = {1, 1}, fncount, grcount, fail;
    vmmin(2, b, &fmin, rosenbrock, rosenbrock_gradient, 100, 0, mask, R_NegInf, 1.490116e-08, 10,
          NULL, &fncount, &grcount, &fail);
    put_all(b, 2);
    put(fmin);
    put_int(fncount);
    put_int(grcount);
    put_int(fail);
    double q[4] = {0, 0, 0, 0};
    int qmask[4] = {1, 0, 1, 1};
    vmmin(4, q, &fmin, quartic, quartic_gradient, 100, 0, qmask, R_NegInf, 1.490116e-08, 10,
          NULL, &fncount, &grcount, &fail);
    put_all(q, 4);
    put(fmin);
    put_int(fncount);
    put_int(grcount);
    put_int(fail);
    double c[2] = {-1.2, 1};
    vmmin(2, c, &fmin, rosenbrock, rosenbrock_gradient, 5, 0, mask, R_NegInf, 1.490116e-08, 10,
          NULL, &fncount, &grcount, &fail);
    put_all(c, 2);
    put(fmin);
    put_int(fncount);
    put_int(fail);
}

static void case_nmmin(void)
{
    double b[2] = {-1.2, 1}, x[2], fmin;
    int fail, fncount;
    nmmin(2, b, x, &fmin, rosenbrock, &fail, R_NegInf, 1.490116e-08, NULL, 1.0, 0.5, 2.0, 0,
          &fncount, 500);
    put_all(x, 2);
    put(fmin);
    put_int(fail);
    put_int(fncount);
    double q[4] = {0, 0, 0, 0}, y[4];
    nmmin(4, q, y, &fmin, quartic, &fail, R_NegInf, 1.490116e-08, NULL, 1.0, 0.5, 2.0, 0,
          &fncount, 60);
    put_all(y, 4);
    put(fmin);
    put_int(fail);
    put_int(fncount);
}

static void case_cgmin(void)
{
    for (int type = 1; type <= 3; type++) {
        double b[4] = {0, 0, 0, 0}, x[4], fmin;
        int fail, fncount, grcount;
        cgmin(4, b, x, &fmin, quartic, quartic_gradient, &fail, R_NegInf, 1.490116e-08, NULL,
              type, 0, &fncount, &grcount, 200);
        put_all(x, 4);
        put(fmin);
        put_int(fail);
        put_int(fncount);
        put_int(grcount);
    }
    double b[2] = {-1.2, 1}, x[2], fmin;
    int fail, fncount, grcount;
    cgmin(2, b, x, &fmin, rosenbrock, rosenbrock_gradient, &fail, R_NegInf, 1.490116e-08, NULL,
          2, 0, &fncount, &grcount, 100);
    put_all(x, 2);
    put(fmin);
    put_int(fail);
    put_int(fncount);
    put_int(grcount);
}

static void case_lbfgsb(void)
{
    char msg[60];
    for (int bounded = 0; bounded < 3; bounded++) {
        double x[4] = {0, 0, 0, 0}, l[4] = {0, 0, 0, 0}, u[4] = {1.5, 1.5, 10, 3.5}, fmin;
        int nbd[4], fail, fncount, grcount;
        for (int i = 0; i < 4; i++)
            nbd[i] = bounded == 0 ? 0 : (bounded == 1 ? 2 : (i % 2 ? 1 : 3));
        lbfgsb(4, 5, x, l, u, nbd, &fmin, quartic, quartic_gradient, &fail, NULL, 1e7, 0,
               &fncount, &grcount, 100, msg, 0, 10);
        put_all(x, 4);
        put(fmin);
        put_int(fail);
        put_int(fncount);
        put_text(msg);
    }
    double r[2] = {-1.2, 1}, l[2], u[2], fmin;
    int nbd[2] = {0, 0}, fail, fncount, grcount;
    lbfgsb(2, 5, r, l, u, nbd, &fmin, rosenbrock, rosenbrock_gradient, &fail, NULL, 1e7, 0,
           &fncount, &grcount, 100, msg, 0, 10);
    put_all(r, 2);
    put(fmin);
    put_int(fail);
    put_int(fncount);
    put_text(msg);
}

static void case_samin(void)
{
    double b[2] = {-1.2, 1}, fmin;
    /*
     * GNU R's samin reads ex as optim()'s internal state to look for a custom candidate
     * generator in its second field, so a direct caller must pass something shaped like it
     * with R_NilValue there.
     */
    SEXP fake_state[16];
    for (int i = 0; i < 16; i++)
        fake_state[i] = R_NilValue;
    samin(2, b, &fmin, rosenbrock, 300, 10, 10, 0, fake_state);
    put_all(b, 2);
    put(fmin);
}

/* Every optimizer stopped at a range of iteration limits, so the exact path is compared, not
 * only the converged answer. CG and Nelder-Mead report both the point they leave in their
 * input vector and the one they return. */
static void case_optim_limits(void)
{
    double fmin;
    int fail, fncount, grcount;
    for (int maxit = 0; maxit <= 120; maxit += 8) {
        for (int type = 1; type <= 3; type++) {
            double b[2] = {-1.2, 1}, x[2] = {0, 0};
            cgmin(2, b, x, &fmin, rosenbrock, rosenbrock_gradient, &fail, R_NegInf,
                  1.490116e-08, NULL, type, 0, &fncount, &grcount, maxit);
            put_all(b, 2);
            put_all(x, 2);
            put(fmin);
            put_int(fail);
            put_int(fncount);
            put_int(grcount);
            double q[5] = {0, 0, 0, 0, 0}, y[5] = {0, 0, 0, 0, 0};
            cgmin(5, q, y, &fmin, quartic, quartic_gradient, &fail, 1e-3, 1e-6, NULL, type, 0,
                  &fncount, &grcount, maxit);
            put_all(q, 5);
            put_all(y, 5);
            put(fmin);
            put_int(fail);
            put_int(fncount);
            put_int(grcount);
        }
        double v[2] = {-1.2, 1};
        int mask[2] = {1, 1};
        vmmin(2, v, &fmin, rosenbrock, rosenbrock_gradient, maxit, 0, mask, R_NegInf,
              1.490116e-08, 10, NULL, &fncount, &grcount, &fail);
        put_all(v, 2);
        put(fmin);
        put_int(fncount);
        put_int(grcount);
        put_int(fail);
        double w[5] = {0, 0, 0, 0, 0};
        int partial[5] = {1, 0, 1, 1, 1};
        vmmin(5, w, &fmin, quartic, quartic_gradient, maxit, 0, partial, 1e-2, 1e-6, 10, NULL,
              &fncount, &grcount, &fail);
        put_all(w, 5);
        put(fmin);
        put_int(fncount);
        put_int(grcount);
        put_int(fail);
        double nb[2] = {-1.2, 1}, nx[2] = {0, 0};
        nmmin(2, nb, nx, &fmin, rosenbrock, &fail, R_NegInf, 1.490116e-08, NULL, 1.0, 0.5, 2.0,
              0, &fncount, maxit * 3);
        put_all(nb, 2);
        put_all(nx, 2);
        put(fmin);
        put_int(fail);
        put_int(fncount);
        double nq[5] = {0, 0, 0, 0, 0}, ny[5] = {0, 0, 0, 0, 0};
        nmmin(5, nq, ny, &fmin, quartic, &fail, 1e-3, 1e-6, NULL, 1.3, 0.4, 2.5, 0, &fncount,
              maxit * 4);
        put_all(nq, 5);
        put_all(ny, 5);
        put(fmin);
        put_int(fail);
        put_int(fncount);
    }
    /* A start at the minimum, where Nelder-Mead must invent its own step size. */
    double z[3] = {1, 2, 3}, zx[3];
    nmmin(3, z, zx, &fmin, quartic, &fail, R_NegInf, 1.490116e-08, NULL, 1.0, 0.5, 2.0, 0,
          &fncount, 200);
    put_all(zx, 3);
    put(fmin);
    put_int(fail);
    put_int(fncount);
}

/* The progress text every optimizer prints with tracing on. */
static void case_optim_traces(void)
{
    double fmin;
    int fail, fncount, grcount;
    double nb[2] = {-1.2, 1}, nx[2];
    nmmin(2, nb, nx, &fmin, rosenbrock, &fail, R_NegInf, 1.490116e-08, NULL, 1.0, 0.5, 2.0, 1,
          &fncount, 60);
    put(fmin);
    double z[2] = {0, 0}, zx[2];
    nmmin(2, z, zx, &fmin, quartic, &fail, R_NegInf, 1.490116e-08, NULL, 1.0, 0.5, 2.0, 1,
          &fncount, 0);
    put(fmin);
    for (int type = 1; type <= 3; type++) {
        double b[3] = {0, 0, 0}, x[3];
        cgmin(3, b, x, &fmin, quartic, quartic_gradient, &fail, R_NegInf, 1.490116e-08, NULL,
              type, 1, &fncount, &grcount, 12);
        put(fmin);
    }
    double c[2] = {-1.2, 1}, cx[2];
    cgmin(2, c, cx, &fmin, rosenbrock, rosenbrock_gradient, &fail, R_NegInf, 1.490116e-08, NULL,
          1, 1, &fncount, &grcount, 400);
    put(fmin);
    int mask[2] = {1, 1};
    for (int report = 1; report <= 7; report += 6) {
        double v[2] = {-1.2, 1};
        vmmin(2, v, &fmin, rosenbrock, rosenbrock_gradient, 40, 1, mask, R_NegInf,
              1.490116e-08, report, NULL, &fncount, &grcount, &fail);
        put(fmin);
    }
    double v[2] = {1, 1};
    vmmin(2, v, &fmin, rosenbrock, rosenbrock_gradient, 40, 1, mask, R_NegInf, 1.490116e-08, 1,
          NULL, &fncount, &grcount, &fail);
    put(fmin);
}

static void case_samin_traced(void)
{
    SEXP fake_state[16];
    for (int i = 0; i < 16; i++)
        fake_state[i] = R_NilValue;
    double b[2] = {-1.2, 1}, fmin;
    samin(2, b, &fmin, rosenbrock, 250, 7, 25, 5, fake_state);
    put_all(b, 2);
    put(fmin);
    double q[4] = {0, 0, 0, 0};
    samin(4, q, &fmin, quartic, 400, 3, 0.5, 40, fake_state);
    put_all(q, 4);
    put(fmin);
    double r[1] = {3};
    samin(1, r, &fmin, quartic, 0, 10, 10, 1, fake_state);
    put_all(r, 1);
    put(fmin);
}

/* Ties, infinities and missing values, under every tie method. */
static void case_max_col_ties(void)
{
    double m[] = {
        /* column 1 */ 1, R_PosInf, NA_REAL, 4, -1, 0, 1e-7, 5, R_NegInf, R_NaN,
        /* column 2 */ 3, R_PosInf, 2, 4, -1, 0, 0, 5, R_NegInf, 1,
        /* column 3 */ 3, 1, 2, 4.00001, -1, 0, 0, 5 + 1e-6, R_NegInf, 1,
        /* column 4 */ 2, R_NegInf, 2, 4, -1, -0.0, 1e-7, 5, R_NegInf, 1,
    };
    int nr = 10, nc = 4, maxes[10];
    for (int method = 1; method <= 3; method++) {
        GetRNGstate();
        R_max_col(m, &nr, &nc, maxes, &method);
        PutRNGstate();
        put_ints(maxes, 10);
    }
    /* Random ties over a larger matrix, drawn twice to show the stream advances. */
    double big[400];
    fill(big, 400, 5, 4);
    int bnr = 40, bnc = 10, bmaxes[40], random = 1;
    for (int pass = 0; pass < 2; pass++) {
        GetRNGstate();
        R_max_col(big, &bnr, &bnc, bmaxes, &random);
        PutRNGstate();
        put_ints(bmaxes, 40);
    }
    /* One column: every method answers 1 without drawing. */
    int one = 1;
    for (int method = 1; method <= 3; method++) {
        R_max_col(m, &nr, &one, maxes, &method);
        put_ints(maxes, 10);
    }
}

static void uncmin_value(int n, double *x, double *f, void *state)
{
    *f = quartic(n, x, state);
}

static void uncmin_gradient(int n, double *x, double *g, void *state)
{
    quartic_gradient(n, x, g, state);
}

static void uncmin_hessian(int nr, int n, double *x, double *h, void *state)
{
    for (int i = 0; i < n; i++) {
        double d = x[i] - (i + 1);
        h[i + i * nr] = 2 + 1.2 * d * d;
        if (i > 0)
            h[i + (i - 1) * nr] = 0.5;
    }
}

static void case_optif9(void)
{
    /* method, iexp, iagflg, iahflg */
    int settings[][4] = {{1, 0, 0, 0}, {1, 1, 0, 0}, {1, 0, 1, 0}, {1, 0, 1, 1}, {2, 1, 1, 0},
                         {2, 0, 0, 0}, {3, 1, 0, 0}, {3, 0, 1, 1}};
    for (int c = 0; c < 8; c++) {
        /* Allow n == 1, print nothing, and check the analytic derivatives supplied. */
        int n = 4, msg = 1 + 8, itrmcd = 0, itncnt = 0;
        double x[4] = {0, 0, 0, 0}, typsiz[4] = {1, 1, 1, 1}, xpls[4] = {0}, fpls = 0;
        double gpls[4] = {0}, a[16] = {0}, wrk[32];
        optif9(n, n, x, uncmin_value, uncmin_gradient, uncmin_hessian, NULL, typsiz, 1.0,
               settings[c][0], settings[c][1], &msg, 12, 100, settings[c][2], settings[c][3],
               1.0, 1e-6, 1000, 1e-6, xpls, &fpls, gpls, &itrmcd, a, wrk, &itncnt);
        put_all(xpls, 4);
        put(fpls);
        put_all(gpls, 4);
        put_int(itrmcd);
        put_int(itncnt);
        put_int(msg);
    }
    double x[4] = {1.5, 2.5, 2, 4}, typx[4] = {1, 1, 1, 1}, h[16], step[4], f[4];
    double fval;
    uncmin_value(4, x, &fval, NULL);
    fdhess(4, x, fval, uncmin_value, NULL, h, 4, step, f, 12, typx);
    for (int j = 0; j < 4; j++)
        for (int i = 0; i <= j; i++)
            put(h[i + j * 4]);
    put_all(step, 4);
}

/* ---- quadrature ---- */

static void gaussian(double *x, int n, void *ex)
{
    for (int i = 0; i < n; i++)
        x[i] = exp(-x[i] * x[i]);
}

static void inverse_sqrt(double *x, int n, void *ex)
{
    for (int i = 0; i < n; i++)
        x[i] = 1 / sqrt(x[i]);
}

static void log_singular(double *x, int n, void *ex)
{
    for (int i = 0; i < n; i++)
        x[i] = log(x[i]) * sqrt(x[i]);
}

static void oscillating(double *x, int n, void *ex)
{
    for (int i = 0; i < n; i++)
        x[i] = sin(30 * x[i]) * exp(-x[i]);
}

static void cauchy_density(double *x, int n, void *ex)
{
    for (int i = 0; i < n; i++)
        x[i] = 1 / (M_PI * (1 + x[i] * x[i]));
}

static void slow_tail(double *x, int n, void *ex)
{
    for (int i = 0; i < n; i++)
        x[i] = 1 / ((1 + x[i]) * sqrt(x[i] + 1e-300));
}

static void put_integration(double result, double abserr, int neval, int ier, int last)
{
    put(result);
    put(abserr);
    put_int(neval);
    put_int(ier);
    put_int(last);
}

static void case_quadrature(void)
{
    integr_fn *finite[] = {gaussian, inverse_sqrt, log_singular, oscillating};
    double bounds[][2] = {{0, 1}, {0, 1}, {0, 1}, {0, 10}};
    for (int c = 0; c < 4; c++) {
        for (int limit_case = 0; limit_case < 2; limit_case++) {
            double a = bounds[c][0], b = bounds[c][1], epsabs = 1.220703e-4, epsrel = 1.220703e-4;
            double result, abserr, work[400];
            int neval, ier, limit = limit_case ? 3 : 100, lenw = 4 * limit, last, iwork[100];
            Rdqags(finite[c], NULL, &a, &b, &epsabs, &epsrel, &result, &abserr, &neval, &ier,
                   &limit, &lenw, &last, iwork, work);
            put_integration(result, abserr, neval, ier, last);
        }
    }
    integr_fn *infinite[] = {gaussian, cauchy_density, slow_tail};
    int kinds[] = {2, 1, 1};
    double starts[] = {0, -1, 0};
    for (int c = 0; c < 3; c++) {
        double bound = starts[c], epsabs = 1.220703e-4, epsrel = 1.220703e-4;
        double result, abserr, work[400];
        int inf = kinds[c], neval, ier, limit = 100, lenw = 400, last, iwork[100];
        Rdqagi(infinite[c], NULL, &bound, &inf, &epsabs, &epsrel, &result, &abserr, &neval, &ier,
               &limit, &lenw, &last, iwork, work);
        put_integration(result, abserr, neval, ier, last);
    }
}

/* ---- strings, files, printing ---- */

static void case_strings(void)
{
    const char *words[] = {"T", "True", "TRUE", "true", "yes", "F", "False", "FALSE", "false", "0",
                           "", "  \t", " x "};
    for (int i = 0; i < 13; i++) {
        put_int(StringTrue(words[i]));
        put_int(StringFalse(words[i]));
        put_int(isBlankString(words[i]));
    }
    put_text(R_ExpandFileName("plain/name"));
    put_text(R_ExpandFileName("/absolute/~name"));
}

static void case_print(void)
{
    double reals[] = {1, 2.5, -3.25, 1e-10, 123456789, NA_REAL, R_PosInf};
    int ints[] = {1, -20, 300, NA_INTEGER};
    int nchar = 5, count = 7, icount = 4, one = 1;
    F77_CALL(dblepr)("reals here", &nchar, reals, &count);
    nchar = 4;
    F77_CALL(intpr)("ints", &nchar, ints, &icount);
    /* A negative length means Fortran's hidden length argument, which C callers lack. */
    nchar = 11;
    F77_CALL(intpr)("whole label", &nchar, ints, &one);
    double small[] = {0.1, 0.25};
    count = 2;
    nchar = 0;
    F77_CALL(dblepr)("", &nchar, small, &count);
    double many[30];
    for (int i = 0; i < 30; i++)
        many[i] = i * 1.5;
    count = 30;
    nchar = 4;
    F77_CALL(dblepr)("many", &nchar, many, &count);
    put_text("printed");
}

typedef void (*case_fn)(void);

static const struct {
    const char *name;
    case_fn run;
} cases[] = {
    {"sorts", case_sorts},
    {"shell_sorts", case_shell_sorts},
    {"partial_sorts", case_partial_sorts},
    {"intervals", case_intervals},
    {"max_col", case_max_col},
    {"max_col_random", case_max_col_random},
    {"dqrdc2", case_dqrdc2},
    {"dqrls", case_dqrls},
    {"dqrdc_dqrsl", case_dqrdc_dqrsl},
    {"triangular", case_triangular},
    {"positive_definite", case_positive_definite},
    {"svd", case_svd},
    {"machine", case_machine},
    {"vmmin", case_vmmin},
    {"nmmin", case_nmmin},
    {"cgmin", case_cgmin},
    {"lbfgsb", case_lbfgsb},
    {"samin", case_samin},
    {"samin_traced", case_samin_traced},
    {"optim_limits", case_optim_limits},
    {"optim_traces", case_optim_traces},
    {"max_col_ties", case_max_col_ties},
    {"optif9", case_optif9},
    {"quadrature", case_quadrature},
    {"strings", case_strings},
    {"print", case_print},
};

SEXP num_cases(void)
{
    int count = (int) (sizeof cases / sizeof cases[0]);
    SEXP names = PROTECT(allocVector(STRSXP, count));
    for (int i = 0; i < count; i++)
        SET_STRING_ELT(names, i, mkChar(cases[i].name));
    UNPROTECT(1);
    return names;
}

SEXP num_case(SEXP name)
{
    const char *wanted = CHAR(STRING_ELT(name, 0));
    token_count = 0;
    for (size_t i = 0; i < sizeof cases / sizeof cases[0]; i++)
        if (strcmp(cases[i].name, wanted) == 0) {
            cases[i].run();
            SEXP result = PROTECT(allocVector(STRSXP, token_count));
            for (int t = 0; t < token_count; t++)
                SET_STRING_ELT(result, t, mkChar(tokens[t]));
            UNPROTECT(1);
            return result;
        }
    error("no case named '%s'", wanted);
    return R_NilValue;
}

/* ---- errors raised inside callbacks ---- */

static double failing_objective(int n, double *p, void *ex)
{
    if (p[0] > 0.5)
        error("objective failed at %g", p[0]);
    return (p[0] - 1) * (p[0] - 1) + p[1] * p[1];
}

static void failing_gradient(int n, double *p, double *g, void *ex)
{
    g[0] = 2 * (p[0] - 1);
    g[1] = 2 * p[1];
}

static void failing_integrand(double *x, int n, void *ex)
{
    error("integrand failed with %d points", n);
}

SEXP num_failing(SEXP which)
{
    int kind = asInteger(which);
    double b[2] = {0, 0}, fmin, result, abserr, work[400], lo = 0, hi = 1, eps = 1e-6;
    int mask[2] = {1, 1}, fncount, grcount, fail, neval, ier, limit = 100, lenw = 400, last;
    int iwork[100], length = 12;
    PROTECT(allocVector(REALSXP, 3));
    switch (kind) {
    case 0:
        vmmin(2, b, &fmin, failing_objective, failing_gradient, 100, 0, mask, R_NegInf, 1e-8, 10,
              NULL, &fncount, &grcount, &fail);
        break;
    case 1:
        Rdqags(failing_integrand, NULL, &lo, &hi, &eps, &eps, &result, &abserr, &neval, &ier,
               &limit, &lenw, &last, iwork, work);
        break;
    case 2:
        F77_CALL(rexitc)("fortran says no", &length);
        break;
    default:
        F77_CALL(rwarnc)("fortran warns", &length);
        break;
    }
    UNPROTECT(1);
    return ScalarInteger(kind);
}
