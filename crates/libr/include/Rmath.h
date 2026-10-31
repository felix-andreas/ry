/*
 * R's mathematical library: densities, distribution and quantile functions and random
 * variates of R's distributions, and the special functions behind them.
 *
 * Written for ry to be source compatible with R 4.6's Rmath.h. Unless R_NO_REMAP_RMATH is
 * defined, the short names (dnorm, pgamma, ...) are macros for the Rf_ names the library
 * exports. MATHLIB_STANDALONE selects the declarations of R's standalone libRmath instead.
 *
 * Conventions: `give_log` returns a density on the log scale; `lower_tail` selects P[X <= x]
 * over P[X > x]; `log_p` makes probabilities log probabilities, both as results of the p
 * functions and as arguments of the q functions.
 */

#ifndef RMATH_H
#define RMATH_H

/* cospi(), sinpi() and tanpi() are ISO/IEC TS 18661-4 functions that some C libraries
   declare only on request. */
#ifndef __STDC_WANT_IEC_60559_FUNCS_EXT__
#define __STDC_WANT_IEC_60559_FUNCS_EXT__ 1
#endif

#if defined(__cplusplus) && !defined(DO_NOT_USE_CXX_HEADERS)
#include <cmath>
#else
#include <math.h>
#endif

#define R_VERSION_STRING "4.6.2"

#ifndef HAVE_EXPM1
#define HAVE_EXPM1 1
#endif
#ifndef HAVE_HYPOT
#define HAVE_HYPOT 1
#endif
#ifndef HAVE_LOG1P
#define HAVE_LOG1P 1
#endif
#ifndef HAVE_WORKING_LOG1P
#define HAVE_WORKING_LOG1P 1
#endif

#if !defined(HAVE_WORKING_LOG1P)
#ifdef __cplusplus
extern "C"
#endif
double Rlog1p(double);
#define log1p Rlog1p
#endif

/* Mathematical constants, to more digits than a double holds; each is defined only when the
   C library has not already done so. */
#ifndef M_E
#define M_E 2.718281828459045235360287471353
#endif
#ifndef M_LOG2E
#define M_LOG2E 1.442695040888963407359924681002
#endif
#ifndef M_LOG10E
#define M_LOG10E 0.434294481903251827651128918917
#endif
#ifndef M_LN2
#define M_LN2 0.693147180559945309417232121458
#endif
#ifndef M_LN10
#define M_LN10 2.302585092994045684017991454684
#endif
#ifndef M_PI
#define M_PI 3.141592653589793238462643383280
#endif
#ifndef M_2PI
#define M_2PI 6.283185307179586476925286766559
#endif
#ifndef M_PI_2
#define M_PI_2 1.570796326794896619231321691640
#endif
#ifndef M_PI_4
#define M_PI_4 0.785398163397448309615660845820
#endif
#ifndef M_1_PI
#define M_1_PI 0.318309886183790671537767526745
#endif
#ifndef M_2_PI
#define M_2_PI 0.636619772367581343075535053490
#endif
#ifndef M_2_SQRTPI
#define M_2_SQRTPI 1.128379167095512573896158903122
#endif
#ifndef M_SQRT2
#define M_SQRT2 1.414213562373095048801688724210
#endif
#ifndef M_SQRT1_2
#define M_SQRT1_2 0.707106781186547524400844362105
#endif
/* The remaining constants are R's own. */
#ifndef M_SQRT_3
#define M_SQRT_3 1.732050807568877293527446341506 /* sqrt(3) */
#endif
#ifndef M_SQRT_32
#define M_SQRT_32 5.656854249492380195206754896838 /* sqrt(32) */
#endif
#ifndef M_LOG10_2
#define M_LOG10_2 0.301029995663981195213738894724 /* log10(2) */
#endif
#ifndef M_SQRT_PI
#define M_SQRT_PI 1.772453850905516027298167483341 /* sqrt(pi) */
#endif
#ifndef M_1_SQRT_2PI
#define M_1_SQRT_2PI 0.398942280401432677939946059934 /* 1/sqrt(2 pi) */
#endif
#ifndef M_SQRT_2dPI
#define M_SQRT_2dPI 0.797884560802865355879892119869 /* sqrt(2/pi) */
#endif
#ifndef M_LN_2PI
#define M_LN_2PI 1.837877066409345483560659472811 /* log(2 pi) */
#endif
#ifndef M_LN_SQRT_PI
#define M_LN_SQRT_PI 0.572364942924700087071713675677 /* log(sqrt(pi)) */
#endif
#ifndef M_LN_SQRT_2PI
#define M_LN_SQRT_2PI 0.918938533204672741780329736406 /* log(sqrt(2 pi)) */
#endif
#ifndef M_LN_SQRT_PId2
#define M_LN_SQRT_PId2 0.225791352644727432363097614947 /* log(sqrt(pi/2)) */
#endif

#ifdef MATHLIB_STANDALONE
#ifndef R_EXT_BOOLEAN_H_
#define R_EXT_BOOLEAN_H_
#undef FALSE
#undef TRUE
typedef enum { FALSE = 0, TRUE } Rboolean;
#if !defined(__cplusplus) && !(defined(__STDC_VERSION__) && __STDC_VERSION__ > 202000L)
#include <stdbool.h>
#endif
#endif
#else
#include <R_ext/Boolean.h>
#endif

#if !defined(MATHLIB_STANDALONE)
#define bessel_i Rf_bessel_i
#define bessel_j Rf_bessel_j
#define bessel_k Rf_bessel_k
#define bessel_y Rf_bessel_y
#define bessel_i_ex Rf_bessel_i_ex
#define bessel_j_ex Rf_bessel_j_ex
#define bessel_k_ex Rf_bessel_k_ex
#define bessel_y_ex Rf_bessel_y_ex
#define beta Rf_beta
#define choose Rf_choose
#define dbeta Rf_dbeta
#define dbinom Rf_dbinom
#define dbinom_raw Rf_dbinom_raw
#define dcauchy Rf_dcauchy
#define dchisq Rf_dchisq
#define dexp Rf_dexp
#define df Rf_df
#define dgamma Rf_dgamma
#define dgeom Rf_dgeom
#define dhyper Rf_dhyper
#define digamma Rf_digamma
#define dlnorm Rf_dlnorm
#define dlogis Rf_dlogis
#define dnbeta Rf_dnbeta
#define dnbinom Rf_dnbinom
#define dnbinom_mu Rf_dnbinom_mu
#define dnchisq Rf_dnchisq
#define dnf Rf_dnf
#define dnorm4 Rf_dnorm4
#define dnt Rf_dnt
#define dpois_raw Rf_dpois_raw
#define dpois Rf_dpois
#define dpsifn Rf_dpsifn
#define dsignrank Rf_dsignrank
#define dt Rf_dt
#define dunif Rf_dunif
#define dweibull Rf_dweibull
#define dwilcox Rf_dwilcox
#define fmax2 Rf_fmax2
#define fmin2 Rf_fmin2
#define fprec Rf_fprec
#define fround Rf_fround
#define ftrunc Rf_ftrunc
#define fsign Rf_fsign
#define gammafn Rf_gammafn
#define imax2 Rf_imax2
#define imin2 Rf_imin2
#define lbeta Rf_lbeta
#define lchoose Rf_lchoose
#define lgammafn Rf_lgammafn
#define lgammafn_sign Rf_lgammafn_sign
#define lgamma1p Rf_lgamma1p
#define pow1p Rf_pow1p
#define log1mexp Rf_log1mexp
#define log1pexp Rf_log1pexp
#define log1pmx Rf_log1pmx
#define logspace_add Rf_logspace_add
#define logspace_sub Rf_logspace_sub
#define logspace_sum Rf_logspace_sum
#define pbeta Rf_pbeta
#define pbeta_raw Rf_pbeta_raw
#define pbinom Rf_pbinom
#define pcauchy Rf_pcauchy
#define pchisq Rf_pchisq
#define pentagamma Rf_pentagamma
#define pexp Rf_pexp
#define pf Rf_pf
#define pgamma Rf_pgamma
#define pgeom Rf_pgeom
#define phyper Rf_phyper
#define plnorm Rf_plnorm
#define plogis Rf_plogis
#define pnbeta Rf_pnbeta
#define pnbinom Rf_pnbinom
#define pnbinom_mu Rf_pnbinom_mu
#define pnchisq Rf_pnchisq
#define pnf Rf_pnf
#define pnorm5 Rf_pnorm5
#define pnorm_both Rf_pnorm_both
#define pnt Rf_pnt
#define ppois Rf_ppois
#define psignrank Rf_psignrank
#define psigamma Rf_psigamma
#define pt Rf_pt
#define ptukey Rf_ptukey
#define punif Rf_punif
#define pweibull Rf_pweibull
#define pwilcox Rf_pwilcox
#define pythag Rf_pythag
#define qbeta Rf_qbeta
#define qbinom Rf_qbinom
#define qcauchy Rf_qcauchy
#define qchisq Rf_qchisq
#define qchisq_appr Rf_qchisq_appr
#define qexp Rf_qexp
#define qf Rf_qf
#define qgamma Rf_qgamma
#define qgeom Rf_qgeom
#define qhyper Rf_qhyper
#define qlnorm Rf_qlnorm
#define qlogis Rf_qlogis
#define qnbeta Rf_qnbeta
#define qnbinom Rf_qnbinom
#define qnbinom_mu Rf_qnbinom_mu
#define qnchisq Rf_qnchisq
#define qnf Rf_qnf
#define qnorm5 Rf_qnorm5
#define qnt Rf_qnt
#define qpois Rf_qpois
#define qsignrank Rf_qsignrank
#define qt Rf_qt
#define qtukey Rf_qtukey
#define qunif Rf_qunif
#define qweibull Rf_qweibull
#define qwilcox Rf_qwilcox
#define rbeta Rf_rbeta
#define rbinom Rf_rbinom
#define rcauchy Rf_rcauchy
#define rchisq Rf_rchisq
#define rexp Rf_rexp
#define rf Rf_rf
#define rgamma Rf_rgamma
#define rgeom Rf_rgeom
#define rhyper Rf_rhyper
#define rlnorm Rf_rlnorm
#define rlogis Rf_rlogis
#define rmultinom Rf_rmultinom
#define rnbinom Rf_rnbinom
#define rnbinom_mu Rf_rnbinom_mu
#define rnchisq Rf_rnchisq
#define rnorm Rf_rnorm
#define rpois Rf_rpois
#define rsignrank Rf_rsignrank
#define rt Rf_rt
#define runif Rf_runif
#define rweibull Rf_rweibull
#define rwilcox Rf_rwilcox
#define sign Rf_sign
#define tetragamma Rf_tetragamma
#define trigamma Rf_trigamma
#endif

/* The normal functions carry their historical argument counts in the exported names. */
#define dnorm dnorm4
#define pnorm pnorm5
#define qnorm qnorm5

#ifdef __cplusplus
extern "C" {
#endif

/* Powers with R's conventions for infinite and NaN arguments. */
double R_pow(double x, double y);
double R_pow_di(double, int);

/* Uniform, normal and exponential deviates from R's generator (never remapped). */
double norm_rand(void);
double unif_rand(void);
double R_unif_index(double);
double exp_rand(void);
#ifdef MATHLIB_STANDALONE
void set_seed(unsigned int, unsigned int);
void get_seed(unsigned int *, unsigned int *);
#endif

/* Normal; pnorm_both fills both tails: i_tail 0 lower, 1 upper, 2 both. */
double dnorm(double, double, double, int);
double pnorm(double, double, double, int, int);
double qnorm(double, double, double, int, int);
double rnorm(double, double);
void pnorm_both(double, double *, double *, int, int);

/* Uniform */
double dunif(double, double, double, int);
double punif(double, double, double, int, int);
double qunif(double, double, double, int, int);
double runif(double, double);

/* Gamma (shape, scale) */
double dgamma(double, double, double, int);
double pgamma(double, double, double, int, int);
double qgamma(double, double, double, int, int);
double rgamma(double, double);

/* Helpers accurate where the obvious formula cancels. */
double log1pmx(double);  /* log(1 + x) - x */
double log1pexp(double); /* log(1 + exp(x)) */
double log1mexp(double); /* log(1 - exp(-x)) */
double lgamma1p(double); /* log(gamma(1 + a)) for small a */
double pow1p(double, double); /* (1 + x)^y */

/* log(exp(logx) + exp(logy)), log(exp(logx) - exp(logy)), log(sum(exp(logx[i]))) */
double logspace_add(double logx, double logy);
double logspace_sub(double logx, double logy);
double logspace_sum(const double *, int);

/* Beta */
double dbeta(double, double, double, int);
double pbeta(double, double, double, int, int);
double qbeta(double, double, double, int, int);
double rbeta(double, double);

/* Log-normal */
double dlnorm(double, double, double, int);
double plnorm(double, double, double, int, int);
double qlnorm(double, double, double, int, int);
double rlnorm(double, double);

/* Chi-squared */
double dchisq(double, double, int);
double pchisq(double, double, int, int);
double qchisq(double, double, int, int);
double rchisq(double);

/* Non-central chi-squared */
double dnchisq(double, double, double, int);
double pnchisq(double, double, double, int, int);
double qnchisq(double, double, double, int, int);
double rnchisq(double, double);

/* F */
double df(double, double, double, int);
double pf(double, double, double, int, int);
double qf(double, double, double, int, int);
double rf(double, double);

/* Student's t */
double dt(double, double, int);
double pt(double, double, int, int);
double qt(double, double, int, int);
double rt(double);

/* Binomial; dbinom_raw takes q = 1 - p separately for accuracy. */
double dbinom_raw(double x, double n, double p, double q, int give_log);
double dbinom(double, double, double, int);
double pbinom(double, double, double, int, int);
double qbinom(double, double, double, int, int);
double rbinom(double, double);

/* Multinomial: rmultinom(n, prob, K, rN) draws one vector of K counts. */
void rmultinom(int, double *, int, int *);

/* Cauchy */
double dcauchy(double, double, double, int);
double pcauchy(double, double, double, int, int);
double qcauchy(double, double, double, int, int);
double rcauchy(double, double);

/* Exponential (scale = 1/rate) */
double dexp(double, double, int);
double pexp(double, double, int, int);
double qexp(double, double, int, int);
double rexp(double);

/* Geometric */
double dgeom(double, double, int);
double pgeom(double, double, int, int);
double qgeom(double, double, int, int);
double rgeom(double);

/* Hypergeometric */
double dhyper(double, double, double, double, int);
double phyper(double, double, double, double, int, int);
double qhyper(double, double, double, double, int, int);
double rhyper(double, double, double);

/* Negative binomial, by probability and by mean */
double dnbinom(double, double, double, int);
double pnbinom(double, double, double, int, int);
double qnbinom(double, double, double, int, int);
double rnbinom(double, double);
double dnbinom_mu(double, double, double, int);
double pnbinom_mu(double, double, double, int, int);
double qnbinom_mu(double, double, double, int, int);
double rnbinom_mu(double, double);

/* Poisson */
double dpois_raw(double, double, int);
double dpois(double, double, int);
double ppois(double, double, int, int);
double qpois(double, double, int, int);
double rpois(double);

/* Weibull */
double dweibull(double, double, double, int);
double pweibull(double, double, double, int, int);
double qweibull(double, double, double, int, int);
double rweibull(double, double);

/* Logistic */
double dlogis(double, double, double, int);
double plogis(double, double, double, int, int);
double qlogis(double, double, double, int, int);
double rlogis(double, double);

/* Non-central beta, F and t */
double dnbeta(double, double, double, double, int);
double pnbeta(double, double, double, double, int, int);
double qnbeta(double, double, double, double, int, int);
double dnf(double, double, double, double, int);
double pnf(double, double, double, double, int, int);
double qnf(double, double, double, double, int, int);
double dnt(double, double, double, int);
double pnt(double, double, double, int, int);
double qnt(double, double, double, int, int);

/* Studentized range */
double ptukey(double, double, double, double, int, int);
double qtukey(double, double, double, double, int, int);

/* Wilcoxon rank sum and signed rank statistics; the _free functions release cached tables
   and are never remapped. */
double dwilcox(double, double, double, int);
double pwilcox(double, double, double, int, int);
double qwilcox(double, double, double, int, int);
double rwilcox(double, double);
void wilcox_free(void);
double dsignrank(double, double, int);
double psignrank(double, double, int, int);
double qsignrank(double, double, int, int);
double rsignrank(double);
void signrank_free(void);

/* Gamma and polygamma functions */
double gammafn(double);
double lgammafn(double);
double lgammafn_sign(double, int *);
void dpsifn(double, int, int, int, double *, int *, int *);
double psigamma(double, double);
double digamma(double);
double trigamma(double);
double tetragamma(double);
double pentagamma(double);

double beta(double, double);
double lbeta(double, double);

double choose(double, double);
double lchoose(double, double);

/* Bessel functions; the _ex forms take a work array of floor(nu) + 1 doubles. */
double bessel_i(double, double, double);
double bessel_j(double, double);
double bessel_k(double, double, double);
double bessel_y(double, double);
double bessel_i_ex(double, double, double, double *);
double bessel_j_ex(double, double, double *);
double bessel_k_ex(double, double, double, double *);
double bessel_y_ex(double, double, double *);
/* The largest orders the Bessel functions accept. */
#define M_bessel_j_max_alpha 1e7
#define M_bessel_y_max_alpha 1e7
#define M_bessel_ik_max_alpha 1e9

/* Support functions */
int imax2(int, int);
int imin2(int, int);
double fmax2(double, double);
double fmin2(double, double);
double sign(double);
double fprec(double, double);  /* signif() */
double fround(double, double); /* round() */
double fsign(double, double);
double ftrunc(double);
double pythag(double, double);

/* cos(pi x), sin(pi x) and tan(pi x), exact at the multiples of 1/2 (1/4 for tan). A C
   library that implements TS 18661-4 declares the first three itself. */
#if !(defined(__STDC_IEC_60559_FUNCS__) && __STDC_IEC_60559_FUNCS__ >= 201506L)
double cospi(double);
double sinpi(double);
double tanpi(double);
#endif
double Rtanpi(double);

#if !defined(MATHLIB_STANDALONE) && defined(R_NO_REMAP_RMATH)
#undef bessel_i
#undef bessel_j
#undef bessel_k
#undef bessel_y
#undef bessel_i_ex
#undef bessel_j_ex
#undef bessel_k_ex
#undef bessel_y_ex
#undef beta
#undef choose
#undef dbeta
#undef dbinom
#undef dbinom_raw
#undef dcauchy
#undef dchisq
#undef dexp
#undef df
#undef dgamma
#undef dgeom
#undef dhyper
#undef digamma
#undef dlnorm
#undef dlogis
#undef dnbeta
#undef dnbinom
#undef dnbinom_mu
#undef dnchisq
#undef dnf
#undef dnorm4
#undef dnt
#undef dpois_raw
#undef dpois
#undef dpsifn
#undef dsignrank
#undef dt
#undef dunif
#undef dweibull
#undef dwilcox
#undef fmax2
#undef fmin2
#undef fprec
#undef fround
#undef ftrunc
#undef fsign
#undef gammafn
#undef imax2
#undef imin2
#undef lbeta
#undef lchoose
#undef lgammafn
#undef lgammafn_sign
#undef lgamma1p
#undef pow1p
#undef log1mexp
#undef log1pexp
#undef log1pmx
#undef logspace_add
#undef logspace_sub
#undef logspace_sum
#undef pbeta
#undef pbeta_raw
#undef pbinom
#undef pcauchy
#undef pchisq
#undef pentagamma
#undef pexp
#undef pf
#undef pgamma
#undef pgeom
#undef phyper
#undef plnorm
#undef plogis
#undef pnbeta
#undef pnbinom
#undef pnbinom_mu
#undef pnchisq
#undef pnf
#undef pnorm5
#undef pnorm_both
#undef pnt
#undef ppois
#undef psignrank
#undef psigamma
#undef pt
#undef ptukey
#undef punif
#undef pweibull
#undef pwilcox
#undef pythag
#undef qbeta
#undef qbinom
#undef qcauchy
#undef qchisq
#undef qchisq_appr
#undef qexp
#undef qf
#undef qgamma
#undef qgeom
#undef qhyper
#undef qlnorm
#undef qlogis
#undef qnbeta
#undef qnbinom
#undef qnbinom_mu
#undef qnchisq
#undef qnf
#undef qnorm5
#undef qnt
#undef qpois
#undef qsignrank
#undef qt
#undef qtukey
#undef qunif
#undef qweibull
#undef qwilcox
#undef rbeta
#undef rbinom
#undef rcauchy
#undef rchisq
#undef rexp
#undef rf
#undef rgamma
#undef rgeom
#undef rhyper
#undef rlnorm
#undef rlogis
#undef rmultinom
#undef rnbinom
#undef rnbinom_mu
#undef rnchisq
#undef rnorm
#undef rpois
#undef rsignrank
#undef rt
#undef runif
#undef rweibull
#undef rwilcox
#undef sign
#undef tetragamma
#undef trigamma
#endif

#ifdef __cplusplus
}
#endif

#endif /* RMATH_H */
