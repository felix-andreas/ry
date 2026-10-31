/*
 * Package-local forwarders to the stats package's registered C routines. Include this header
 * in exactly one file of a package that calls the S_* routines of R_ext/stats_package.h.
 */
#include <R_ext/Rdynload.h>
#include <R_ext/stats_package.h>

typedef void (*ry_stats_divset_t)(int, int *, int, int, double *);
typedef void (*ry_stats_nlminb_t)(double *, double *, double, double *, double *, int *, int,
                                  int, int, double *, double *);
typedef void (*ry_stats_nlsb_t)(double *, double *, double *, int *, int, int, int, int, int,
                                double *, double *, double *, double *);
typedef void (*ry_stats_rcont2_t)(int, int, const int *, const int *, int, const double *,
                                  int *, int *);

attribute_hidden void S_Rf_divset(int alg, int iv[], int liv, int lv, double v[])
{
    static ry_stats_divset_t routine = NULL;
    if (routine == NULL)
        routine = (ry_stats_divset_t) R_GetCCallable("stats", "Rf_divset");
    routine(alg, iv, liv, lv, v);
}

attribute_hidden void S_nlminb_iterate(double b[], double d[], double fx, double g[],
                                       double h[], int iv[], int liv, int lv, int n, double v[],
                                       double x[])
{
    static ry_stats_nlminb_t routine = NULL;
    if (routine == NULL)
        routine = (ry_stats_nlminb_t) R_GetCCallable("stats", "nlminb_iterate");
    routine(b, d, fx, g, h, iv, liv, lv, n, v, x);
}

attribute_hidden void S_nlsb_iterate(double b[], double d[], double dr[], int iv[], int liv,
                                     int lv, int n, int nd, int p, double r[], double rd[],
                                     double v[], double x[])
{
    static ry_stats_nlsb_t routine = NULL;
    if (routine == NULL)
        routine = (ry_stats_nlsb_t) R_GetCCallable("stats", "nlsb_iterate");
    routine(b, d, dr, iv, liv, lv, n, nd, p, r, rd, v, x);
}

attribute_hidden void S_rcont2(int nrow, int ncol, const int nrowt[], const int ncolt[],
                               int ntotal, const double fact[], int jwork[], int matrix[])
{
    static ry_stats_rcont2_t routine = NULL;
    if (routine == NULL)
        routine = (ry_stats_rcont2_t) R_GetCCallable("stats", "rcont2");
    routine(nrow, ncol, nrowt, ncolt, ntotal, fact, jwork, matrix);
}
