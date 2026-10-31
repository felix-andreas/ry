/*
 * Calls a handful of BLAS and LAPACK routines through ry's R_ext/BLAS.h and R_ext/Lapack.h,
 * against the system's libblas and liblapack. The same file compiles as C and as C++, and with
 * or without hidden character lengths (DONT_USE_FC_LEN_T), so every combination a package can
 * build in is exercised. Besides fixed cases, a bounded randomized pass multiplies and solves
 * matrices of random shapes with padded leading dimensions and compares against direct loops,
 * which catches a swapped or misplaced argument in a declaration. It prints one line per failed
 * check and exits with the failure count.
 */
#include <R.h>
#include <R_ext/BLAS.h>
#include <R_ext/Lapack.h>

static int failures = 0;

static void check(const char *what, double actual, double expected) {
    if (fabs(actual - expected) > 1e-10) {
        printf("%s: got %.17g, expected %.17g\n", what, actual, expected);
        failures++;
    }
}

static void check_dgemm(void) {
    /* Column-major 2x3 times 3x2. */
    double a[6] = {1, 4, 2, 5, 3, 6};
    double b[6] = {7, 9, 11, 8, 10, 12};
    double c[4] = {0, 0, 0, 0};
    double one = 1, zero = 0;
    int m = 2, n = 2, k = 3;
    F77_CALL(dgemm)("N", "N", &m, &n, &k, &one, a, &m, b, &k, &zero, c, &m FCONE FCONE);
    check("dgemm c[0,0]", c[0], 58);
    check("dgemm c[1,0]", c[1], 139);
    check("dgemm c[0,1]", c[2], 64);
    check("dgemm c[1,1]", c[3], 154);
}

static void check_dgesv(void) {
    double a[9] = {2, 1, 1, 1, 3, 2, 1, 0, 0};
    double b[3] = {4, 5, 6};
    int n = 3, nrhs = 1, ipiv[3], info = -1;
    F77_CALL(dgesv)(&n, &nrhs, a, &n, ipiv, b, &n, &info);
    check("dgesv info", info, 0);
    /* The system 2x + y + z = 4, x + 3y = 5, x + 2y = 6 has x = 8, y = -1, z = -11. */
    check("dgesv x", b[0], 8);
    check("dgesv y", b[1], -1);
    check("dgesv z", b[2], -11);
}

static void check_dsyev(void) {
    double a[4] = {2, 1, 1, 2};
    double w[2], work[64];
    int n = 2, lwork = 64, info = -1;
    F77_CALL(dsyev)("V", "U", &n, a, &n, w, work, &lwork, &info FCONE FCONE);
    check("dsyev info", info, 0);
    check("dsyev w[0]", w[0], 1);
    check("dsyev w[1]", w[1], 3);
    check("dsyev |v[0]|", fabs(a[0]), sqrt(0.5));
}

static void check_zgemm(void) {
    /* (1+i)(2-i) + 0 = 3+i, and so on, for a 1x2 times 2x1 product. */
    Rcomplex a[2], b[2], c[1], one, zero;
    int m = 1, n = 1, k = 2;
    a[0].r = 1;
    a[0].i = 1;
    a[1].r = 0;
    a[1].i = 2;
    b[0].r = 2;
    b[0].i = -1;
    b[1].r = 3;
    b[1].i = 0;
    c[0].r = 99;
    c[0].i = 99;
    one.r = 1;
    one.i = 0;
    zero.r = 0;
    zero.i = 0;
    F77_CALL(zgemm)("N", "N", &m, &n, &k, &one, a, &m, b, &k, &zero, c, &m FCONE FCONE);
    /* (1+i)(2-i) = 3+i and (2i)(3) = 6i, so the sum is 3+7i. */
    check("zgemm real", c[0].r, 3);
    check("zgemm imaginary", c[0].i, 7);
}

static void check_dpotrf(void) {
    double a[4] = {4, 2, 2, 3};
    int n = 2, info = -1;
    F77_CALL(dpotrf)("L", &n, a, &n, &info FCONE);
    check("dpotrf info", info, 0);
    check("dpotrf l[0,0]", a[0], 2);
    check("dpotrf l[1,0]", a[1], 1);
    check("dpotrf l[1,1]", a[3], sqrt(2.0));
}

static void check_dgesvd(void) {
    double a[4] = {3, 0, 0, -4};
    double s[2], u[4], vt[4], query, work[512];
    int n = 2, lwork = -1, info = -1;
    F77_CALL(dgesvd)("A", "A", &n, &n, a, &n, s, u, &n, vt, &n, &query, &lwork, &info
                     FCONE FCONE);
    check("dgesvd query info", info, 0);
    lwork = (int) query;
    if (lwork > 512 || lwork < 1) {
        printf("dgesvd: workspace query asked for %d doubles\n", lwork);
        failures++;
        return;
    }
    F77_CALL(dgesvd)("A", "A", &n, &n, a, &n, s, u, &n, vt, &n, work, &lwork, &info
                     FCONE FCONE);
    check("dgesvd info", info, 0);
    check("dgesvd s[0]", s[0], 4);
    check("dgesvd s[1]", s[1], 3);
}

static void check_auxiliaries(void) {
    double epsilon = F77_CALL(dlamch)("E" FCONE);
    if (!(epsilon > 0 && epsilon < 1e-15)) {
        printf("dlamch: epsilon %g\n", epsilon);
        failures++;
    }
    int ispec = 1, size = 100, unused = -1;
    int block = F77_CALL(ilaenv)(&ispec, "DGETRF", " ", &size, &size, &unused, &unused
                                 FCONE FCONE);
    if (block < 1) {
        printf("ilaenv: block size %d\n", block);
        failures++;
    }
    int major = 0, minor = 0, patch = 0;
    F77_CALL(ilaver)(&major, &minor, &patch);
    if (major < 3) {
        printf("ilaver: version %d.%d.%d\n", major, minor, patch);
        failures++;
    }
}

/* A fixed-seed generator, so a failure reproduces. */
static unsigned long long state = 0x9E3779B97F4A7C15ULL;

static double uniform(void) {
    state = state * 6364136223846793005ULL + 1442695040888963407ULL;
    return (double) (state >> 11) / 9007199254740992.0 * 2 - 1;
}

static int below(int bound) {
    int value = (int) ((uniform() + 1) / 2 * bound);
    return value < bound ? value : bound - 1;
}

enum { MAX_DIM = 7, PAD = 3, CELLS = (MAX_DIM + PAD) * MAX_DIM };

static void random_dgemm(int round) {
    double a[CELLS], b[CELLS], c[CELLS], expected[CELLS];
    int m = 1 + below(MAX_DIM), n = 1 + below(MAX_DIM), k = 1 + below(MAX_DIM);
    int transpose_a = below(2), transpose_b = below(2);
    /* op(A) is m x k and op(B) is k x n; each stored matrix gets extra rows of padding. */
    int rows_a = transpose_a ? k : m, rows_b = transpose_b ? n : k;
    int lda = rows_a + below(PAD + 1), ldb = rows_b + below(PAD + 1), ldc = m + below(PAD + 1);
    double alpha = uniform(), beta = uniform();
    for (int index = 0; index < CELLS; index++) {
        a[index] = uniform();
        b[index] = uniform();
        c[index] = uniform();
        expected[index] = c[index];
    }
    for (int i = 0; i < m; i++) {
        for (int j = 0; j < n; j++) {
            double sum = 0;
            for (int l = 0; l < k; l++) {
                double left = transpose_a ? a[l + i * lda] : a[i + l * lda];
                double right = transpose_b ? b[j + l * ldb] : b[l + j * ldb];
                sum += left * right;
            }
            expected[i + j * ldc] = alpha * sum + beta * c[i + j * ldc];
        }
    }
    F77_CALL(dgemm)(transpose_a ? "T" : "N", transpose_b ? "T" : "N", &m, &n, &k, &alpha, a,
                    &lda, b, &ldb, &beta, c, &ldc FCONE FCONE);
    for (int index = 0; index < CELLS; index++) {
        if (fabs(c[index] - expected[index]) > 1e-12) {
            printf("random dgemm round %d (m=%d n=%d k=%d): cell %d is %.17g, expected %.17g\n",
                   round, m, n, k, index, c[index], expected[index]);
            failures++;
            return;
        }
    }
}

static void random_dgesv(int round) {
    double a[CELLS], original[CELLS], b[CELLS], x[CELLS];
    int n = 1 + below(MAX_DIM), nrhs = 1 + below(3), ipiv[MAX_DIM], info = -1;
    int lda = n + below(PAD + 1), ldb = n + below(PAD + 1);
    for (int index = 0; index < CELLS; index++) {
        a[index] = uniform();
        b[index] = uniform();
    }
    /* Diagonal dominance keeps the system well conditioned. */
    for (int i = 0; i < n; i++)
        a[i + i * lda] += n + 1;
    for (int index = 0; index < CELLS; index++) {
        original[index] = a[index];
        x[index] = b[index];
    }
    F77_CALL(dgesv)(&n, &nrhs, a, &lda, ipiv, x, &ldb, &info);
    if (info != 0) {
        printf("random dgesv round %d: info %d\n", round, info);
        failures++;
        return;
    }
    for (int column = 0; column < nrhs; column++) {
        for (int i = 0; i < n; i++) {
            double sum = 0;
            for (int j = 0; j < n; j++)
                sum += original[i + j * lda] * x[j + column * ldb];
            if (fabs(sum - b[i + column * ldb]) > 1e-10) {
                printf("random dgesv round %d (n=%d): residual %.3g\n", round, n,
                       sum - b[i + column * ldb]);
                failures++;
                return;
            }
        }
    }
}

static void random_zgemm(int round) {
    Rcomplex a[CELLS], b[CELLS], c[CELLS], alpha, beta;
    double expected_r[CELLS], expected_i[CELLS];
    int m = 1 + below(MAX_DIM), n = 1 + below(MAX_DIM), k = 1 + below(MAX_DIM);
    int lda = m + below(PAD + 1), ldb = k + below(PAD + 1), ldc = m + below(PAD + 1);
    alpha.r = uniform();
    alpha.i = uniform();
    beta.r = uniform();
    beta.i = uniform();
    for (int index = 0; index < CELLS; index++) {
        a[index].r = uniform();
        a[index].i = uniform();
        b[index].r = uniform();
        b[index].i = uniform();
        c[index].r = uniform();
        c[index].i = uniform();
        expected_r[index] = c[index].r;
        expected_i[index] = c[index].i;
    }
    for (int i = 0; i < m; i++) {
        for (int j = 0; j < n; j++) {
            double sum_r = 0, sum_i = 0;
            for (int l = 0; l < k; l++) {
                Rcomplex left = a[i + l * lda], right = b[l + j * ldb];
                sum_r += left.r * right.r - left.i * right.i;
                sum_i += left.r * right.i + left.i * right.r;
            }
            Rcomplex old = c[i + j * ldc];
            expected_r[i + j * ldc] = alpha.r * sum_r - alpha.i * sum_i + beta.r * old.r - beta.i * old.i;
            expected_i[i + j * ldc] = alpha.r * sum_i + alpha.i * sum_r + beta.r * old.i + beta.i * old.r;
        }
    }
    F77_CALL(zgemm)("N", "N", &m, &n, &k, &alpha, a, &lda, b, &ldb, &beta, c, &ldc FCONE FCONE);
    for (int index = 0; index < CELLS; index++) {
        if (fabs(c[index].r - expected_r[index]) > 1e-12 ||
            fabs(c[index].i - expected_i[index]) > 1e-12) {
            printf("random zgemm round %d (m=%d n=%d k=%d): cell %d differs\n", round, m, n, k,
                   index);
            failures++;
            return;
        }
    }
}

int main(void) {
    check_dgemm();
    check_dgesv();
    check_dsyev();
    check_zgemm();
    check_dpotrf();
    check_dgesvd();
    check_auxiliaries();
    for (int round = 0; round < 300 && failures == 0; round++) {
        random_dgemm(round);
        random_dgesv(round);
        random_zgemm(round);
    }
    return failures;
}
