/*
 * ry's declaration of the C entry points the stats package registers for other packages
 * (R_GetCCallable("stats", ...)). They live in stats, not in libR: a package includes
 * R_ext/stats_stubs.h once to define package-local forwarders to them.
 */
#ifndef R_STATS_PACKAGE_H
#define R_STATS_PACKAGE_H

#include <Rinternals.h>
#include <R_ext/Visibility.h>

enum AlgType { NREG = 1, OPT = 2 };

/* Zero-based positions in the PORT routines' v work vector. */
enum VPos { F = 9, F0 = 12, FDIF = 10, G = 27, HC = 70 };

/* Zero-based positions in the PORT routines' iv work vector. */
enum IVPos {
    AI = 90, AM = 94, ALGSAV = 50, COVMAT = 25, COVPRT = 13, COVREQ = 14, DRADPR = 100,
    DTYPE = 15, IERR = 74, INITH = 24, INITS = 24, IPIVOT = 75, IVNEED = 2, LASTIV = 42,
    LASTV = 44, LMAT = 41, MXFCAL = 16, MXITER = 17, NEXTV = 46, NFCALL = 5, NFCOV = 51,
    NFGCAL = 6, NGCOV = 52, NITER = 30, NVDFLT = 49, NVSAVE = 8, OUTLEV = 18, PARPRT = 19,
    PARSAV = 48, PERM = 57, PRUNIT = 20, QRTYP = 79, RDREQ = 56, RMAT = 77, SOLPRT = 21,
    STATPR = 22, TOOBIG = 1, VNEED = 3, VSAVE = 59, X0PRT = 23
};

#ifdef __cplusplus
extern "C" {
#endif

attribute_hidden void S_Rf_divset(int alg, int iv[], int liv, int lv, double v[]);
attribute_hidden void S_nlminb_iterate(double b[], double d[], double fx, double g[], double h[], int iv[],
                      int liv, int lv, int n, double v[], double x[]);
attribute_hidden void S_nlsb_iterate(double b[], double d[], double dr[], int iv[], int liv, int lv, int n,
                    int nd, int p, double r[], double rd[], double v[], double x[]);
attribute_hidden void S_rcont2(int nrow, int ncol, const int nrowt[], const int ncolt[], int ntotal,
              const double fact[], int *jwork, int *matrix);

#ifdef __cplusplus
}
#endif

#endif
